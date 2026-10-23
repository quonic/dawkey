// SPDX-License-Identifier: AGPL-3.0-or-later
//! Autosave and recovery: every so often, unsaved changes are written to an
//! autosave (in the project's bundle, or with a never-saved project's
//! recordings in the cache), which saving, replacing the project or exiting
//! cleanly deletes. If Dawkey stops without exiting cleanly, the next one
//! offers the changes back: when it opens that project, or when it starts
//! without one and finds a never-saved project's autosave.

use std::time::{Duration, Instant};

use dawkey_studio::Studio;

use crate::App;

/// How often unsaved changes are autosaved, unless the preferences say.
pub(crate) const AUTOSAVE_EVERY: Duration = Duration::from_secs(120);

impl App {
    /// Every tick: autosaves when it's time and there are changes to keep.
    pub(crate) fn autosave_now_and_then(&mut self) {
        let Some(every) = self.autosave_every else {
            return;
        };
        if self.last_autosave.elapsed() < every {
            return;
        }
        self.last_autosave = Instant::now();
        if let Err(e) = self.studio.autosave() {
            tracing::warn!("autosave: {e}");
            self.message = Some(format!("Autosave failed: {e}"));
        }
    }

    /// At startup in a window: offers back changes a Dawkey that stopped
    /// without exiting cleanly kept, of the project it opened, or (with
    /// none) of a never-saved project.
    pub fn offer_recovery(&mut self) {
        self.view.recovery = match self.studio.session().path() {
            Some(_) => self.studio.bundle_recovery(),
            None => self.studio.orphan_recovery(),
        };
    }

    /// Clicks on the recovery dialog. `None` if `parts` isn't one of its.
    pub(crate) fn on_recovery_click(&mut self, parts: &[&str]) -> Option<Result<(), String>> {
        Some(match parts {
            ["recover"] | ["recover", "backdrop"] => Ok(()),
            ["recover", "recover"] => self.recover(),
            ["recover", "discard"] => {
                if let Some(recovery) = self.view.recovery.take() {
                    Studio::discard_recovery(&recovery);
                }
                Ok(())
            }
            _ => return None,
        })
    }

    /// While the dialog is open it takes every key: Return recovers.
    pub(crate) fn on_recovery_key(&mut self, key: &str) -> Option<Result<(), String>> {
        self.view.recovery.as_ref()?;
        Some(match key {
            "return" => self.recover(),
            _ => Ok(()),
        })
    }

    fn recover(&mut self) -> Result<(), String> {
        let Some(recovery) = self.view.recovery.take() else {
            return Ok(());
        };
        self.studio.recover(&recovery).map_err(|e| e.to_string())?;
        self.view = crate::state::ViewState {
            page: self.view.page,
            export: self.view.export,
            ..Default::default()
        };
        self.message = Some("Recovered the unsaved changes".into());
        Ok(())
    }
}

/// "3 minutes ago", for the dialog.
pub(crate) fn ago(age: Duration) -> String {
    let s = age.as_secs();
    match s {
        0..60 => "less than a minute ago".into(),
        60..120 => "a minute ago".into(),
        120..3600 => format!("{} minutes ago", s / 60),
        3600..7200 => "an hour ago".into(),
        7200..86_400 => format!("{} hours ago", s / 3600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", s / 86_400),
    }
}
