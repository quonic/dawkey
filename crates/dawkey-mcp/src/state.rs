// SPDX-License-Identifier: AGPL-3.0-or-later
//! Smaller project states: get_state can leave out notes, controls,
//! automation points and tracks, and open_project and new_project return a
//! summary.

use serde_json::{Value, json};

/// What get_state leaves out of a project state.
#[derive(Debug, Clone, Default)]
pub(crate) struct Trim {
    /// Replace each MIDI clip's notes with `note_count` and `pitch_range`.
    pub(crate) notes: bool,
    /// Replace each MIDI clip's controls with `control_count`.
    pub(crate) controls: bool,
    /// Replace each automation lane's points with `point_count`.
    pub(crate) automation: bool,
    /// Keep only these tracks (the master always stays).
    pub(crate) tracks: Option<Vec<u64>>,
}

impl Trim {
    /// A summary: counts instead of notes, controls and automation points.
    pub(crate) fn summary() -> Self {
        Self {
            notes: true,
            controls: true,
            automation: true,
            tracks: None,
        }
    }

    /// Whether anything is left out.
    pub(crate) fn any(&self) -> bool {
        self.notes || self.controls || self.automation || self.tracks.is_some()
    }

    /// Leaves out what this says from `state`, noting under `summary` what
    /// was left out.
    pub(crate) fn apply(&self, state: &mut Value) {
        if !self.any() {
            return;
        }
        let project = &mut state["project"];
        if let Some(keep) = &self.tracks
            && let Some(tracks) = project["tracks"].as_array_mut()
        {
            tracks.retain(|t| t["id"].as_u64().is_some_and(|id| keep.contains(&id)));
        }
        let tracks = project["tracks"]
            .as_array_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        let mut tracks: Vec<Value> = tracks;
        let mut master = std::mem::take(&mut project["master"]);
        for track in tracks.iter_mut().chain(std::iter::once(&mut master)) {
            self.trim_track(track);
        }
        project["tracks"] = Value::Array(tracks);
        project["master"] = master;
        let mut left_out = Vec::new();
        for (yes, what) in [
            (self.notes, "notes"),
            (self.controls, "controls"),
            (self.automation, "automation_points"),
        ] {
            if yes {
                left_out.push(what);
            }
        }
        if self.tracks.is_some() {
            left_out.push("other_tracks");
        }
        state["summary"] = json!({ "left_out": left_out });
    }

    fn trim_track(&self, track: &mut Value) {
        if let Some(clips) = track["clips"].as_array_mut() {
            for clip in clips {
                let Some(clip) = clip.as_object_mut() else {
                    continue;
                };
                if self.notes
                    && let Some(Value::Array(notes)) = clip.remove("notes")
                {
                    clip.insert("note_count".into(), json!(notes.len()));
                    let pitches = notes.iter().filter_map(|n| n["pitch"].as_u64());
                    if let (Some(lo), Some(hi)) = (pitches.clone().min(), pitches.max()) {
                        clip.insert("pitch_range".into(), json!([lo, hi]));
                    }
                }
                if self.controls
                    && let Some(Value::Array(controls)) = clip.remove("controls")
                {
                    clip.insert("control_count".into(), json!(controls.len()));
                }
            }
        }
        if self.automation
            && let Some(lanes) = track["automation"].as_array_mut()
        {
            for lane in lanes {
                let Some(lane) = lane.as_object_mut() else {
                    continue;
                };
                if let Some(Value::Array(points)) = lane.remove("points") {
                    lane.insert("point_count".into(), json!(points.len()));
                }
            }
        }
    }
}
