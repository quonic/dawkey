// SPDX-License-Identifier: AGPL-3.0-or-later
//! A log of what changed the project, so a client can ask what happened
//! since a revision it knows (`get_changes` over MCP).

use std::collections::VecDeque;

use serde::Serialize;

use crate::edit::{ClipProp, Edit, TrackProp};
use crate::model::{ClipId, NoteId, PluginId, Project, TrackId};
use crate::session::Outcome;

/// How many changes are remembered.
pub(crate) const CHANGE_LOG_LIMIT: usize = 1000;

/// What kind of step a change was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// A command (or a batch of them) as one undo step.
    Command,
    Undo,
    Redo,
    /// A source added to the pool (not an undo step).
    Source,
}

/// One change to the project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    /// The revision the change made.
    pub revision: u64,
    pub kind: ChangeKind,
    /// The undo step's name (the command's tool name).
    pub label: String,
    /// The tracks it touched (added, removed or changed, or whose clips
    /// changed).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<TrackId>,
    /// The clips it touched.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<ClipId>,
    /// The notes it added, removed or changed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<NoteId>,
    /// The plugin slots it created.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<PluginId>,
    /// What else it changed: `name`, `tempo`, `meter`, `loop`, `punch`,
    /// `plugins` (a track's chain), `automation`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<&'static str>,
}

impl Change {
    /// A change at `revision` whose effects are reverted by `inverses` (the
    /// edits an undo step keeps), on `project` as it is after the change.
    pub(crate) fn new(
        revision: u64,
        kind: ChangeKind,
        label: impl Into<String>,
        project: &Project,
        inverses: &[Edit],
        outcomes: &[Outcome],
    ) -> Self {
        let mut change = Self {
            revision,
            kind,
            label: label.into(),
            tracks: Vec::new(),
            clips: Vec::new(),
            notes: Vec::new(),
            plugins: Vec::new(),
            also: Vec::new(),
        };
        for edit in inverses {
            change.note_edit(project, edit);
        }
        for outcome in outcomes {
            change.note_outcome(project, outcome);
        }
        change.tidy();
        change
    }

    /// Folds another change into this one (a merged gesture).
    pub(crate) fn merge(&mut self, other: Change) {
        self.revision = other.revision;
        self.tracks.extend(other.tracks);
        self.clips.extend(other.clips);
        self.notes.extend(other.notes);
        self.plugins.extend(other.plugins);
        self.also.extend(other.also);
        self.tidy();
    }

    fn tidy(&mut self) {
        self.tracks.sort_unstable();
        self.tracks.dedup();
        self.clips.sort_unstable();
        self.clips.dedup();
        self.notes.sort_unstable();
        self.notes.dedup();
        self.plugins.sort_unstable();
        self.plugins.dedup();
        self.also.sort_unstable();
        self.also.dedup();
    }

    fn note_edit(&mut self, project: &Project, edit: &Edit) {
        match edit {
            Edit::InsertTrack { track, .. } => self.tracks.push(track.id),
            Edit::RemoveTrack { id } => self.tracks.push(*id),
            Edit::SetTrack { id, prop } => {
                self.tracks.push(*id);
                match prop {
                    TrackProp::Plugins(_) => self.also.push("plugins"),
                    TrackProp::Automation(_) => self.also.push("automation"),
                    _ => {}
                }
            }
            Edit::InsertClip { track, clip, .. } => {
                self.tracks.push(*track);
                self.clips.push(clip.id);
                self.notes.extend(clip.notes().iter().map(|n| n.id));
            }
            Edit::RemoveClip { id } => {
                self.note_clip(project, *id);
                if let Some((_, clip)) = project.clip(*id) {
                    self.notes.extend(clip.notes().iter().map(|n| n.id));
                }
            }
            Edit::SetClip { id, prop } => {
                self.note_clip(project, *id);
                if let ClipProp::Notes(before) = prop
                    && let Some((_, clip)) = project.clip(*id)
                {
                    // The notes that differ between before and after.
                    let after = clip.notes();
                    for note in before.iter().filter(|n| !after.contains(n)) {
                        self.notes.push(note.id);
                    }
                    for note in after.iter().filter(|n| !before.contains(n)) {
                        self.notes.push(note.id);
                    }
                }
            }
            Edit::SetProjectName(_) => self.also.push("name"),
            Edit::SetTempo { .. } => self.also.push("tempo"),
            Edit::SetMeter { .. } => self.also.push("meter"),
            Edit::SetLoop(_) => self.also.push("loop"),
            Edit::SetPunch(_) => self.also.push("punch"),
        }
    }

    fn note_clip(&mut self, project: &Project, id: ClipId) {
        self.clips.push(id);
        if let Some((track, _)) = project.clip(id) {
            self.tracks.push(track.id);
        }
    }

    fn note_outcome(&mut self, project: &Project, outcome: &Outcome) {
        if let Some(track) = outcome.track {
            self.tracks.push(track);
        }
        for clip in outcome.clip.iter().chain(&outcome.clips) {
            self.note_clip(project, *clip);
        }
        self.notes.extend(outcome.notes.iter().copied());
        if let Some(plugin) = outcome.plugin {
            self.plugins.push(plugin);
        }
    }
}

/// The changes since a revision, as `get_changes` reports them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangesSince {
    /// The revision asked about.
    pub since_revision: u64,
    /// The current revision.
    pub revision: u64,
    /// The changes after `since_revision`, oldest first.
    pub changes: Vec<Change>,
    /// Whether `changes` has every change since `since_revision`. False if
    /// the log doesn't go back that far, or `since_revision` is past the
    /// current revision (another project was opened, or Dawkey restarted:
    /// get_state gives the project as it is).
    pub complete: bool,
}

/// The most recent changes, oldest first.
#[derive(Debug, Default)]
pub(crate) struct ChangeLog {
    changes: VecDeque<Change>,
}

impl ChangeLog {
    pub(crate) fn push(&mut self, change: Change) {
        if self.changes.len() == CHANGE_LOG_LIMIT {
            self.changes.pop_front();
        }
        self.changes.push_back(change);
    }

    /// The latest change, to merge a gesture's next step into.
    pub(crate) fn last_mut(&mut self) -> Option<&mut Change> {
        self.changes.back_mut()
    }

    pub(crate) fn since(&self, since_revision: u64, revision: u64) -> ChangesSince {
        let changes: Vec<Change> = self
            .changes
            .iter()
            .filter(|c| c.revision > since_revision)
            .cloned()
            .collect();
        let complete = since_revision <= revision
            && (since_revision == revision
                || self
                    .changes
                    .front()
                    .is_some_and(|first| first.revision <= since_revision + 1));
        ChangesSince {
            since_revision,
            revision,
            changes,
            complete,
        }
    }
}
