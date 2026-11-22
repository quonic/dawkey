// SPDX-License-Identifier: AGPL-3.0-or-later
//! Starting a new project, opening another one, and closing the window ask
//! first when there are unsaved changes: save them, discard them, or cancel.

use dawkey_core::Project;
use dawkey_studio::LoadedProject;

use crate::App;
use crate::state::{PendingAction, PlatformRequest};

impl App {
    /// Does `action`, or first opens the "save changes?" dialog when there
    /// are unsaved changes.
    pub(crate) fn request_action(&mut self, action: PendingAction) {
        if self.has_unsaved_changes() {
            self.view.shared.confirm = Some(action);
        } else {
            self.proceed(action);
        }
    }

    /// Asked by the platform when the compositor wants the window closed:
    /// true if it may close now, false if the "save changes?" dialog opened
    /// instead (its buttons close the window later, or don't).
    pub fn request_close(&mut self) -> bool {
        if !self.has_unsaved_changes() {
            return true;
        }
        self.view.shared.confirm = Some(PendingAction::Close);
        self.rebuild();
        false
    }

    /// Whether the project has unsaved changes. A recording in progress is
    /// stopped first, so its takes are in the project to save or discard
    /// (not lost with it).
    fn has_unsaved_changes(&mut self) -> bool {
        if self.studio.is_recording()
            && let Err(e) = self.studio.stop()
        {
            tracing::warn!("finishing the recording: {e}");
        }
        self.studio.session().is_dirty()
    }

    /// Carries out `action`, whatever happens to unsaved changes: once
    /// they're saved (the platform calls this after a save dialog asked for
    /// by the "save changes?" dialog), or when they may be discarded.
    pub fn proceed(&mut self, action: PendingAction) {
        self.view.shared.confirm = None;
        match action {
            PendingAction::NewProject => {
                let project = Project::new("Untitled");
                if let Err(e) = self.studio.replace(LoadedProject::new(project)) {
                    self.message = Some(e.to_string());
                }
            }
            PendingAction::OpenProject => self
                .platform_requests
                .push(PlatformRequest::ChooseProjectToOpen),
            PendingAction::Close => self.close_requested = true,
        }
    }

    /// Clicks in the dialog. `None` if `parts` isn't one of its widgets.
    pub(crate) fn on_confirm_click(&mut self, parts: &[&str]) -> Option<Result<(), String>> {
        let action = self.view.shared.confirm?;
        Some(match parts {
            ["confirm", "save"] => self.save_then(action),
            ["confirm", "discard"] => {
                self.proceed(action);
                Ok(())
            }
            ["confirm", "cancel"] => {
                self.view.shared.confirm = None;
                Ok(())
            }
            // The backdrop (and anything else) waits for an answer.
            _ => Ok(()),
        })
    }

    /// Keys while the dialog is open: Return saves, Escape cancels, and
    /// nothing else gets through.
    pub(crate) fn on_confirm_key(&mut self, key: &str) -> Option<Result<(), String>> {
        let action = self.view.shared.confirm?;
        Some(match key {
            "return" => self.save_then(action),
            "escape" => {
                self.view.shared.confirm = None;
                Ok(())
            }
            _ => Ok(()),
        })
    }

    /// Saves, then does `action`. A project without a file asks where to
    /// save it first, and the platform carries on from there.
    fn save_then(&mut self, action: PendingAction) -> Result<(), String> {
        self.view.shared.confirm = None;
        if self.studio.session().path().is_none() {
            self.platform_requests
                .push(PlatformRequest::ChooseSaveLocation { then: Some(action) });
            return Ok(());
        }
        self.studio.save(None).map_err(|e| e.to_string())?;
        self.proceed(action);
        Ok(())
    }
}
