// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recordings of a project that was never saved go to a cache folder,
//! `$XDG_CACHE_HOME/dawkey/recordings/<pid>`, until saving copies them into
//! the project. After that nothing refers to them, so they're deleted once
//! the project is saved, replaced or closed. The project's autosave goes
//! there too. Folders left by a Dawkey that didn't exit cleanly are offered
//! for recovery (a Dawkey that recovers one adopts it: its `owner` file
//! names the process), and swept at a start once they're a week old.

/// The file naming the process that adopted a folder of another's.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The file naming the process that adopted a folder of another's.
pub(crate) const OWNER: &str = "owner";

/// How long a crashed instance's recordings are kept.
pub const STALE_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// `$XDG_CACHE_HOME/dawkey/recordings` (`~/.cache` if unset).
pub fn recordings_root() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("dawkey")
        .join("recordings")
}

/// This process's folder under [`recordings_root`].
pub(crate) fn own_dir() -> PathBuf {
    recordings_root().join(std::process::id().to_string())
}

/// Deletes `files`, then the folders they were in if that left them empty.
pub(crate) fn remove(files: Vec<PathBuf>) {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for file in files {
        match std::fs::remove_file(&file) {
            Ok(()) => tracing::debug!("removed cached recording {}", file.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("removing {}: {e}", file.display()),
        }
        if let Some(dir) = file.parent()
            && !dirs.iter().any(|d| d == dir)
        {
            dirs.push(dir.to_owned());
        }
    }
    for dir in dirs {
        // Fails (and stays) if anything else is in it.
        let _ = std::fs::remove_dir(&dir);
    }
}

/// Deletes the recordings folders under `root` whose process isn't running
/// and that nothing was written to for `max_age`. Returns what it deleted.
pub fn sweep_stale(root: &Path, max_age: Duration) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let now = SystemTime::now();
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !is_orphan(&dir) {
            continue;
        }
        let age = last_written(&dir).and_then(|t| now.duration_since(t).ok());
        if age.is_none_or(|age| age < max_age) {
            continue;
        }
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {
                tracing::info!("removed old recordings {}", dir.display());
                removed.push(dir);
            }
            Err(e) => tracing::warn!("removing {}: {e}", dir.display()),
        }
    }
    removed
}

/// Whether `dir` is a pid-named folder that no running process uses: its
/// own has gone, and so has the one that adopted it, if any.
fn is_orphan(dir: &Path) -> bool {
    let running = |pid: u32| Path::new("/proc").join(pid.to_string()).exists();
    let Some(pid) = dir
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.parse::<u32>().ok())
    else {
        return false;
    };
    let owner = std::fs::read_to_string(dir.join(OWNER))
        .ok()
        .and_then(|o| o.trim().parse::<u32>().ok());
    dir.is_dir() && !running(pid) && !owner.is_some_and(running)
}

/// Folders under `root` that Dawkeys which stopped without exiting cleanly
/// left an autosave in, with when it was written, newest first.
pub fn orphaned_autosaves(root: &Path) -> Vec<(PathBuf, SystemTime)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<(PathBuf, SystemTime)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|dir| is_orphan(dir))
        .filter_map(|dir| {
            let file = dir.join(dawkey_core::project_file::AUTOSAVE_NAME);
            let modified = std::fs::metadata(&file).and_then(|m| m.modified()).ok()?;
            Some((dir, modified))
        })
        .collect();
    found.sort_by_key(|f| std::cmp::Reverse(f.1));
    found
}

/// Takes over `dir`, another process's folder: while this one runs, it
/// isn't swept or offered for recovery.
pub(crate) fn adopt(dir: &Path) -> std::io::Result<()> {
    std::fs::write(dir.join(OWNER), std::process::id().to_string())
}

/// When `dir` or anything directly in it was last modified.
fn last_written(dir: &Path) -> Option<SystemTime> {
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let files = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| modified(&e.path()));
    files.chain(modified(dir)).max()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweeps_only_old_folders_of_dead_processes() {
        let root = tempfile::tempdir().unwrap();
        let make = |name: &str| {
            let dir = root.path().join(name);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("Vox take 1.wav"), b"RIFF").unwrap();
            dir
        };
        // Pids are below 2^22 on Linux, so this one isn't running.
        let dead = make("4294967295");
        let alive = make(&std::process::id().to_string());
        let other = make("not-a-pid");

        // Too recent.
        assert!(sweep_stale(root.path(), STALE_AFTER).is_empty());
        assert!(dead.exists());

        assert_eq!(
            sweep_stale(root.path(), Duration::ZERO),
            std::slice::from_ref(&dead)
        );
        assert!(!dead.exists() && alive.exists() && other.exists());
        // A missing root is nothing to sweep.
        assert!(sweep_stale(&root.path().join("none"), Duration::ZERO).is_empty());
    }

    #[test]
    fn removing_files_removes_their_emptied_folder() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("1");
        std::fs::create_dir(&dir).unwrap();
        let files: Vec<_> = (1..=2)
            .map(|n| {
                let f = dir.join(format!("take {n}.wav"));
                std::fs::write(&f, b"RIFF").unwrap();
                f
            })
            .collect();
        remove(vec![files[0].clone()]);
        assert!(!files[0].exists() && dir.exists());
        remove(vec![files[1].clone(), dir.join("gone.wav")]);
        assert!(!dir.exists());
    }
}
