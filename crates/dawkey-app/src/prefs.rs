// SPDX-License-Identifier: AGPL-3.0-or-later
//! Preferences: settings that belong to the user rather than a project,
//! kept in `$XDG_CONFIG_HOME/dawkey/preferences.json` and changed in the
//! preferences panel (`status/preferences`, Ctrl+,). Only the window uses
//! them; headless instances (and tests) run with the defaults, so they stay
//! deterministic.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Autosave intervals the panel offers, in minutes.
pub(crate) const AUTOSAVE_MINUTES: [u32; 4] = [1, 2, 5, 10];
/// UI scales the panel offers.
pub(crate) const UI_SCALES: [f32; 8] = [0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// Where the window plays (from the next start).
    pub audio: AudioChoice,
    /// Link the outputs to the default sink (from the next start).
    pub autoconnect: bool,
    /// Recordings are moved this much earlier than the latency estimate
    /// says (see `set_record_offset`).
    pub record_offset_ms: f64,
    /// Keep unsaved changes in an autosave, every `autosave_minutes`.
    pub autosave: bool,
    pub autosave_minutes: u32,
    pub theme: ThemeChoice,
    /// Scales the whole window on top of the desktop's scale.
    pub ui_scale: f32,
    /// Folders to look for plugins in besides the standard ones.
    pub clap_folders: Vec<PathBuf>,
    pub lv2_folders: Vec<PathBuf>,
    pub vst3_folders: Vec<PathBuf>,
    /// Run plugins in processes of their own (from the next start)...
    pub bridge_plugins: bool,
    /// ...the instances of one plugin file sharing one (else each its own),
    /// in a sandbox (writing only to their own folders, no network).
    pub share_plugin_processes: bool,
    pub sandbox_plugins: bool,
    /// Serve MCP over HTTP on `127.0.0.1:mcp_http_port` (with the token in
    /// `mcp-token` beside this file).
    pub mcp_http: bool,
    pub mcp_http_port: u16,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            audio: AudioChoice::Pipewire,
            autoconnect: true,
            record_offset_ms: 0.0,
            autosave: true,
            autosave_minutes: 2,
            theme: ThemeChoice::System,
            ui_scale: 1.0,
            clap_folders: Vec::new(),
            lv2_folders: Vec::new(),
            vst3_folders: Vec::new(),
            bridge_plugins: true,
            share_plugin_processes: true,
            sandbox_plugins: true,
            mcp_http: false,
            mcp_http_port: crate::mcp_http::DEFAULT_MCP_PORT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioChoice {
    Pipewire,
    /// No device: nothing plays (time only moves for MCP's advance_clock).
    Virtual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    /// Light or dark, as the desktop is.
    System,
    Dark,
    Light,
}

/// A kind of plugin folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    Clap,
    Lv2,
    Vst3,
}

impl Preferences {
    /// `$XDG_CONFIG_HOME/dawkey/preferences.json` (`~/.config` if unset).
    pub fn default_path() -> Option<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
        Some(config.join("dawkey").join("preferences.json"))
    }

    /// The preferences at `path`; the defaults if there are none, or (with
    /// a warning) if they can't be read. A value that isn't of its type
    /// (a file edited by hand) takes its default, with a warning, and the
    /// rest are kept.
    pub fn load(path: &Path) -> Self {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!("reading {}: {e}", path.display());
                return Self::default();
            }
        };
        let file = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(serde_json::Value::Object(file)) => file,
            Ok(_) => {
                tracing::warn!("{}: not a JSON object; using the defaults", path.display());
                return Self::default();
            }
            Err(e) => {
                tracing::warn!("{}: {e}; using the defaults", path.display());
                return Self::default();
            }
        };
        // Take the file's values one at a time (missing ones are the
        // defaults), leaving out those that don't fit.
        let mut prefs = serde_json::Map::new();
        for (key, value) in file {
            prefs.insert(key.clone(), value);
            if let Err(e) = serde_json::from_value::<Self>(prefs.clone().into()) {
                tracing::warn!("{}: {key}: {e}; using its default", path.display());
                prefs.remove(&key);
            }
        }
        serde_json::from_value::<Self>(prefs.into())
            .unwrap_or_default()
            .sanitized()
    }

    /// Writes them to `path` (replacing what was there at once).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut json = serde_json::to_vec_pretty(self).expect("preferences serialize");
        json.push(b'\n');
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(tmp, path)
    }

    /// Values in range (a file edited by hand may hold anything).
    fn sanitized(mut self) -> Self {
        if !(-1000.0..=1000.0).contains(&self.record_offset_ms) {
            self.record_offset_ms = 0.0;
        }
        self.autosave_minutes = self.autosave_minutes.clamp(1, 60);
        if !(0.5..=3.0).contains(&self.ui_scale) {
            self.ui_scale = 1.0;
        }
        if self.mcp_http_port == 0 {
            self.mcp_http_port = crate::mcp_http::DEFAULT_MCP_PORT;
        }
        self
    }

    /// How often to autosave, if at all.
    pub fn autosave_every(&self) -> Option<Duration> {
        self.autosave
            .then(|| Duration::from_secs(u64::from(self.autosave_minutes) * 60))
    }

    /// Where to look for plugins: the standard folders and the user's.
    pub fn plugin_search(&self) -> dawkey_studio::SearchPath {
        dawkey_studio::SearchPath::with_extra(
            &self.clap_folders,
            &self.lv2_folders,
            &self.vst3_folders,
        )
    }

    pub(crate) fn folders_mut(&mut self, kind: PluginKind) -> &mut Vec<PathBuf> {
        match kind {
            PluginKind::Clap => &mut self.clap_folders,
            PluginKind::Lv2 => &mut self.lv2_folders,
            PluginKind::Vst3 => &mut self.vst3_folders,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_falls_back_to_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dawkey/preferences.json");
        assert_eq!(Preferences::load(&path), Preferences::default());
        let prefs = Preferences {
            audio: AudioChoice::Virtual,
            ui_scale: 1.25,
            lv2_folders: vec![PathBuf::from("/opt/lv2")],
            ..Preferences::default()
        };
        prefs.save(&path).unwrap();
        assert_eq!(Preferences::load(&path), prefs);
        // Missing fields take their defaults; values out of range are reset.
        std::fs::write(&path, br#"{ "ui_scale": 40, "autosave": false }"#).unwrap();
        let loaded = Preferences::load(&path);
        assert_eq!((loaded.ui_scale, loaded.autosave), (1.0, false));
        assert_eq!(loaded.autosave_every(), None);
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(Preferences::load(&path), Preferences::default());
    }

    /// A value of the wrong type (an interval of 5.5 minutes, a port
    /// above 65535) takes its default, and the rest of the file is kept.
    #[test]
    fn a_mistyped_value_takes_its_default_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        std::fs::write(
            &path,
            br#"{ "lv2_folders": ["/opt/lv2"], "ui_scale": 1.5, "autosave_minutes": 5.5,
                  "mcp_http_port": 70000, "theme": "pink", "autosave": false }"#,
        )
        .unwrap();
        let expected = Preferences {
            lv2_folders: vec![PathBuf::from("/opt/lv2")],
            ui_scale: 1.5,
            autosave: false,
            ..Preferences::default()
        };
        assert_eq!(Preferences::load(&path), expected);
    }
}
