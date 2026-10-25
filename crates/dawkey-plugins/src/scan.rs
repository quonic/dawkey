// SPDX-License-Identifier: AGPL-3.0-or-later
//! Finding plugins and describing them.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{PluginError, PluginInfo, clap, lv2, vst3};

/// What a scan found.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    /// Sorted by name.
    pub plugins: Vec<PluginInfo>,
    /// Files or plugins that couldn't be read, and why.
    pub failures: Vec<ScanFailure>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanFailure {
    pub path: PathBuf,
    pub error: String,
}

impl Catalog {
    pub fn get(&self, id: &str) -> Option<&PluginInfo> {
        self.plugins.iter().find(|p| p.id == id)
    }
}

/// Where plugins are looked for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchPath {
    /// Directories searched (recursively) for `.clap` files.
    pub clap: Vec<PathBuf>,
    /// Directories holding LV2 bundles; `None` for lilv's standard ones.
    pub lv2: Option<Vec<PathBuf>>,
    /// Directories searched (recursively) for `.vst3` bundles.
    #[serde(default)]
    pub vst3: Vec<PathBuf>,
}

impl SearchPath {
    /// `$CLAP_PATH`, `$LV2_PATH` and `$VST3_PATH` (colon-separated) if
    /// set; otherwise the standard locations: `~/.clap`, `/usr/lib/clap`,
    /// `/usr/lib64/clap`, `/usr/local/lib/clap`, lilv's LV2 defaults, and
    /// `~/.vst3`, `/usr/lib/vst3`, `/usr/lib64/vst3`, `/usr/local/lib/vst3`.
    pub fn from_env() -> Self {
        let split = |var| std::env::var_os(var).map(|p| std::env::split_paths(&p).collect());
        let standard = |name: &str| {
            let mut dirs = Vec::new();
            if let Some(home) = std::env::var_os("HOME") {
                dirs.push(Path::new(&home).join(format!(".{name}")));
            }
            dirs.extend(
                ["/usr/lib", "/usr/lib64", "/usr/local/lib"].map(|d| Path::new(d).join(name)),
            );
            dirs
        };
        Self {
            clap: split("CLAP_PATH").unwrap_or_else(|| standard("clap")),
            lv2: split("LV2_PATH"),
            vst3: split("VST3_PATH").unwrap_or_else(|| standard("vst3")),
        }
    }

    /// [`from_env`](Self::from_env) and `clap`, `lv2` and `vst3` as well
    /// (folders the user added).
    pub fn with_extra(clap: &[PathBuf], lv2: &[PathBuf], vst3: &[PathBuf]) -> Self {
        let mut search = Self::from_env();
        search.clap.extend(clap.iter().cloned());
        search.vst3.extend(vst3.iter().cloned());
        if !lv2.is_empty() {
            // lilv's standard folders, which an explicit path replaces.
            let mut dirs = search.lv2.take().unwrap_or_else(|| {
                let mut dirs = Vec::new();
                if let Some(home) = std::env::var_os("HOME") {
                    dirs.push(Path::new(&home).join(".lv2"));
                }
                dirs.extend(
                    [
                        "/usr/local/lib64/lv2",
                        "/usr/local/lib/lv2",
                        "/usr/lib64/lv2",
                        "/usr/lib/lv2",
                    ]
                    .map(PathBuf::from),
                );
                dirs
            });
            dirs.extend(lv2.iter().cloned());
            search.lv2 = Some(dirs);
        }
        search
    }
}

/// Every `.clap` file on the search path (following symlinks), each once.
pub fn clap_files(search: &SearchPath) -> Vec<PathBuf> {
    find(&search.clap, "clap", |path| path.is_file())
}

/// Every `.vst3` bundle on the search path that has a binary for this
/// machine (following symlinks), each once.
pub fn vst3_bundles(search: &SearchPath) -> Vec<PathBuf> {
    find(&search.vst3, "vst3", |path| vst3_binary(path).is_file())
}

/// A `.vst3` bundle's binary for this machine:
/// `Contents/<arch>-linux/<name>.so`.
pub fn vst3_binary(bundle: &Path) -> PathBuf {
    vst3::bundle_binary(bundle)
}

/// Files or bundles with extension `extension` under `dirs` that pass
/// `wanted`, each once. Bundles aren't looked into.
fn find(dirs: &[PathBuf], extension: &str, wanted: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for dir in dirs {
        walk(dir, 4, extension, &mut |path| {
            if !wanted(path) {
                return;
            }
            let canonical = path.canonicalize().unwrap_or_else(|_| path.to_owned());
            if seen.insert(canonical) {
                found.push(path.to_owned());
            }
        });
    }
    found
}

fn walk(dir: &Path, depth: u32, extension: &str, found: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        let named = path.extension().is_some_and(|e| e == extension);
        if named {
            found(&path);
        }
        // A `.vst3` bundle isn't looked into; a folder merely named
        // `.clap` is.
        let bundle = named && extension == "vst3";
        if !bundle && path.is_dir() && depth > 0 {
            walk(&path, depth - 1, extension, found);
        }
    }
}

/// Describes the plugins in one `.clap` file, in this process. This runs
/// the plugin's code: a crash here takes the process down, which is why
/// [`scan`] calls it through a subprocess.
pub fn scan_clap_file(path: &Path) -> Result<Vec<PluginInfo>, PluginError> {
    clap::scan_file(path)
}

/// Describes the plugins in one `.vst3` bundle, in this process. Like
/// [`scan_clap_file`], this runs the plugin's code.
pub fn scan_vst3_bundle(path: &Path) -> Result<Vec<PluginInfo>, PluginError> {
    vst3::scan_bundle(path)
}

/// Describes the plugins in a `.clap` file or a `.vst3` bundle, by its
/// extension (what `dawkey scan-plugin` does).
pub fn scan_plugin_file(path: &Path) -> Result<Vec<PluginInfo>, PluginError> {
    if path.extension().is_some_and(|e| e == "vst3") {
        scan_vst3_bundle(path)
    } else {
        scan_clap_file(path)
    }
}

/// Describes every installed LV2 plugin, without running plugin code.
pub fn scan_lv2(search: &SearchPath) -> (Vec<PluginInfo>, Vec<ScanFailure>) {
    let (plugins, failures) = lv2::scan(search.lv2.as_deref());
    let failures = failures
        .into_iter()
        .map(|(path, error)| ScanFailure { path, error })
        .collect();
    (plugins, failures)
}

/// Scans every plugin. `scanner` is a program that describes one `.clap`
/// file or `.vst3` bundle given as its last argument, printing a JSON list
/// of [`PluginInfo`] (`dawkey scan-plugin`); each gets its own process,
/// killed after `timeout`, so a plugin that crashes or hangs only loses
/// itself. With no scanner, they're read in-process.
pub fn scan(search: &SearchPath, scanner: Option<(&Path, &[&str])>, timeout: Duration) -> Catalog {
    let mut catalog = Catalog::default();
    for path in clap_files(search).into_iter().chain(vst3_bundles(search)) {
        let result = match scanner {
            Some((program, args)) => scan_in_subprocess(program, args, &path, timeout),
            None => scan_plugin_file(&path).map_err(|e| e.0),
        };
        match result {
            Ok(plugins) => catalog.plugins.extend(plugins),
            Err(error) => {
                tracing::warn!("{}: {error}", path.display());
                catalog.failures.push(ScanFailure { path, error });
            }
        }
    }
    let (lv2, failures) = scan_lv2(search);
    catalog.plugins.extend(lv2);
    catalog.failures.extend(failures);
    catalog.plugins.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    catalog.plugins.dedup_by(|a, b| a.id == b.id);
    catalog
}

/// How long the scanner's output is read for after it exits. All of it is
/// in the pipe by then; only a process the plugin started (wine's
/// wineserver for yabridge's plugins, a licence daemon) that holds the
/// pipe open keeps it from ending.
const OUTPUT_GRACE: Duration = Duration::from_millis(500);

fn scan_in_subprocess(
    program: &Path,
    args: &[&str],
    path: &Path,
    timeout: Duration,
) -> Result<Vec<PluginInfo>, String> {
    let mut child = Command::new(program)
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("starting the scanner: {e}"))?;
    // Read on a thread, so a chatty plugin can't fill the pipe and stall;
    // what it reads comes over a channel, so waiting for it has a limit.
    let mut stdout = child.stdout.take().expect("piped");
    let (chunks, read) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0; 8192];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) if chunks.send(buf[..n].to_vec()).is_ok() => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                _ => break,
            }
        }
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {} s", timeout.as_secs()));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    // Until the pipe ends, or for a moment: a process the plugin left
    // running may hold it open (and the reader, until it goes).
    let until = Instant::now() + OUTPUT_GRACE;
    let mut out = Vec::new();
    while let Ok(chunk) = read.recv_timeout(until.saturating_duration_since(Instant::now())) {
        out.extend(chunk);
    }
    if !status.success() {
        return Err(match status.code() {
            Some(code) => format!("the scanner failed (exit status {code})"),
            None => "the plugin crashed the scanner".to_owned(),
        });
    }
    serde_json::from_slice(&out).map_err(|e| format!("bad scanner output: {e}"))
}
