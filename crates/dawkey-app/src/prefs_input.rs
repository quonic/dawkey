// SPDX-License-Identifier: AGPL-3.0-or-later
//! The preferences panel: each change applies at once (the audio backend,
//! auto-connect and plugin processes from the next start) and is saved to the preferences
//! file, if the app has one.

use std::path::{Path, PathBuf};

use crate::App;
use crate::prefs::{
    AUTOSAVE_MINUTES, AudioChoice, PluginKind, Preferences, ThemeChoice, UI_SCALES,
};
use crate::state::{PlatformRequest, TextField};

impl App {
    /// Uses `prefs`, saving changes to `path` (`None`: nowhere).
    pub fn set_preferences(&mut self, prefs: Preferences, path: Option<PathBuf>) {
        let before = self.prefs.clone();
        self.prefs = prefs;
        self.prefs_path = path;
        self.apply_preferences(&before, true);
    }

    pub fn preferences(&self) -> &Preferences {
        &self.prefs
    }

    /// Applies what changed from `before` (everything, with `all`).
    fn apply_preferences(&mut self, before: &Preferences, all: bool) {
        let prefs = self.prefs.clone();
        self.autosave_every = prefs.autosave_every();
        if all || prefs.ui_scale != before.ui_scale {
            self.set_ui_scale(prefs.ui_scale);
        }
        if (all || prefs.record_offset_ms != before.record_offset_ms)
            && let Err(e) = self.studio.set_record_offset_ms(prefs.record_offset_ms)
        {
            self.message = Some(e.to_string());
        }
        // Only the window looks for plugins where the preferences say
        // (headless instances, and tests, keep to the standard folders).
        let folders = (&prefs.clap_folders, &prefs.lv2_folders, &prefs.vst3_folders);
        if !all
            && self.platform == crate::Platform::Wayland
            && folders
                != (
                    &before.clap_folders,
                    &before.lv2_folders,
                    &before.vst3_folders,
                )
        {
            self.studio.change_plugin_search(prefs.plugin_search());
        }
        if all || prefs.theme != before.theme {
            self.apply_theme();
        }
        if all || (prefs.mcp_http, prefs.mcp_http_port) != (before.mcp_http, before.mcp_http_port) {
            self.apply_mcp_http();
        }
    }

    /// Changes the preferences with `change`, applies and saves them.
    fn change_preferences(&mut self, change: impl FnOnce(&mut Preferences)) {
        let before = self.prefs.clone();
        change(&mut self.prefs);
        if self.prefs == before {
            return;
        }
        self.apply_preferences(&before, false);
        if let Some(path) = &self.prefs_path
            && let Err(e) = self.prefs.save(path)
        {
            self.message = Some(format!("Saving the preferences: {e}"));
        }
    }

    pub(crate) fn open_preferences(&mut self) {
        self.close_overlays();
        self.view.prefs_open = true;
    }

    /// Clicks on the preferences panel and the button that opens it.
    /// `None` if `parts` isn't one of theirs.
    pub(crate) fn on_prefs_click(&mut self, parts: &[&str]) -> Option<Result<(), String>> {
        let next = |list: &[f32], value: f32| {
            let i = list.iter().position(|&v| (v - value).abs() < 1e-3);
            list[i.map_or(0, |i| (i + 1) % list.len())]
        };
        Some(match parts {
            ["status", "preferences"] => {
                self.open_preferences();
                Ok(())
            }
            ["prefs"] => Ok(()),
            ["prefs", "close" | "backdrop"] => {
                self.view.prefs_open = false;
                Ok(())
            }
            ["prefs", "audio"] => {
                self.change_preferences(|p| {
                    p.audio = match p.audio {
                        AudioChoice::Pipewire => AudioChoice::Virtual,
                        AudioChoice::Virtual => AudioChoice::Pipewire,
                    }
                });
                Ok(())
            }
            ["prefs", "autoconnect"] => {
                self.change_preferences(|p| p.autoconnect = !p.autoconnect);
                Ok(())
            }
            ["prefs", "record_offset"] => self.start_text_edit(TextField::RecordOffset),
            ["prefs", "autosave"] => {
                self.change_preferences(|p| p.autosave = !p.autosave);
                Ok(())
            }
            ["prefs", "autosave_every"] => {
                self.change_preferences(|p| {
                    let i = AUTOSAVE_MINUTES
                        .iter()
                        .position(|&m| m == p.autosave_minutes);
                    p.autosave_minutes =
                        AUTOSAVE_MINUTES[i.map_or(0, |i| (i + 1) % AUTOSAVE_MINUTES.len())];
                });
                Ok(())
            }
            ["prefs", "theme"] => {
                self.change_preferences(|p| {
                    p.theme = match p.theme {
                        ThemeChoice::System => ThemeChoice::Dark,
                        ThemeChoice::Dark => ThemeChoice::Light,
                        ThemeChoice::Light => ThemeChoice::System,
                    }
                });
                Ok(())
            }
            ["prefs", "scale"] => {
                self.change_preferences(|p| p.ui_scale = next(&UI_SCALES, p.ui_scale));
                Ok(())
            }
            ["prefs", "plugins", "add_clap"] => {
                self.platform_requests.push(PlatformRequest::ChooseFolder {
                    kind: PluginKind::Clap,
                });
                Ok(())
            }
            ["prefs", "plugins", "add_lv2"] => {
                self.platform_requests.push(PlatformRequest::ChooseFolder {
                    kind: PluginKind::Lv2,
                });
                Ok(())
            }
            ["prefs", "plugins", "add_vst3"] => {
                self.platform_requests.push(PlatformRequest::ChooseFolder {
                    kind: PluginKind::Vst3,
                });
                Ok(())
            }
            ["prefs", "plugins", "remove", kind, n] => {
                let kind = match *kind {
                    "clap" => PluginKind::Clap,
                    "lv2" => PluginKind::Lv2,
                    "vst3" => PluginKind::Vst3,
                    _ => return None,
                };
                let n: usize = n.parse().ok()?;
                self.change_preferences(|p| {
                    let folders = p.folders_mut(kind);
                    if n < folders.len() {
                        folders.remove(n);
                    }
                });
                Ok(())
            }
            ["prefs", "plugins", "bridge"] => {
                self.change_preferences(|p| p.bridge_plugins = !p.bridge_plugins);
                Ok(())
            }
            ["prefs", "plugins", "sandbox"] => {
                self.change_preferences(|p| p.sandbox_plugins = !p.sandbox_plugins);
                Ok(())
            }
            ["prefs", "plugins", "share"] => {
                self.change_preferences(|p| {
                    p.share_plugin_processes = !p.share_plugin_processes;
                });
                Ok(())
            }
            ["prefs", "mcp_http"] => {
                self.change_preferences(|p| p.mcp_http = !p.mcp_http);
                Ok(())
            }
            ["prefs", "mcp_port"] => self.start_text_edit(TextField::McpPort),
            ["prefs", "mcp_token"] => self.renew_mcp_token(),
            ["prefs", "plugins", "rescan"] => {
                self.studio.rescan_plugins_in_background();
                self.message = Some("Scanning for plugins…".to_owned());
                Ok(())
            }
            _ => return None,
        })
    }

    /// Keys while the panel is open: Escape closes it, and nothing else
    /// gets through to the window behind it. `None` if it's closed.
    pub(crate) fn on_prefs_key(&mut self, key: &str) -> Option<Result<(), String>> {
        if !self.view.prefs_open {
            return None;
        }
        if key == "escape" {
            self.view.prefs_open = false;
        }
        Some(Ok(()))
    }

    /// Looks for plugins of `kind` in `folder` too (from the folder dialog).
    pub fn add_plugin_folder(&mut self, kind: PluginKind, folder: &Path) {
        self.change_preferences(|p| {
            let folders = p.folders_mut(kind);
            if !folders.iter().any(|f| f == folder) {
                folders.push(folder.to_owned());
            }
        });
    }

    /// The MCP port typed in the panel.
    pub(crate) fn set_mcp_port_pref(&mut self, port: u16) {
        self.change_preferences(|p| p.mcp_http_port = port);
    }

    /// The record offset typed in the panel.
    pub(crate) fn set_record_offset_pref(&mut self, ms: f64) {
        self.change_preferences(|p| p.record_offset_ms = ms);
    }
}
