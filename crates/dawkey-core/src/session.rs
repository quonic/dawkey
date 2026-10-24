// SPDX-License-Identifier: AGPL-3.0-or-later
//! An open project plus its undo history and file location.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::changes::{Change, ChangeKind, ChangeLog, ChangesSince};
use crate::command::Command;
use crate::edit::{self, Edit};
use crate::error::CoreError;
use crate::model::{ClipId, NoteId, PluginId, PluginState, Project, Source, SourceId, TrackId};
use crate::project_file;

/// Maximum number of undo groups kept.
const UNDO_LIMIT: usize = 1000;

/// What a command produced, beyond changing the project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Outcome {
    /// The track the command created or used, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<TrackId>,
    /// The clip the command created, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip: Option<ClipId>,
    /// Every clip the command created, when it can make several
    /// (repeat_clip).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<ClipId>,
    /// The notes the command created, if any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<NoteId>,
    /// The plugin slot the command created, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginId>,
}

impl Outcome {
    pub(crate) fn none() -> Self {
        Self::default()
    }

    pub(crate) fn track(id: TrackId) -> Self {
        Self {
            track: Some(id),
            ..Self::default()
        }
    }
}

/// One undoable step: the inverse edits of everything it changed, in the
/// order that reverts them.
#[derive(Debug)]
struct UndoGroup {
    label: String,
    inverses: Vec<Edit>,
    /// Set while later commands with the same key may still join this step
    /// (see [`Session::execute_merging`]).
    merge_key: Option<String>,
}

/// Session status plus the full project, as reported to clients.
#[derive(Debug, Clone, Serialize)]
pub struct SessionState {
    /// The project, without the plugin states (see `plugin_state_bytes`).
    pub project: Project,
    /// Slots whose plugin saved state of its own, and its size in bytes.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub plugin_state_bytes: std::collections::BTreeMap<PluginId, usize>,
    pub path: Option<PathBuf>,
    /// Incremented on every change, including undo and redo.
    pub revision: u64,
    /// True if there are changes since the last save or load.
    pub dirty: bool,
    pub undo: Option<String>,
    pub redo: Option<String>,
}

#[derive(Debug, Default)]
pub struct Session {
    project: Project,
    path: Option<PathBuf>,
    undo: Vec<UndoGroup>,
    redo: Vec<UndoGroup>,
    revision: u64,
    saved_revision: u64,
    /// A plugin state changed since the last save (plugin states aren't
    /// undo steps, so they don't move the revision).
    states_changed: bool,
    /// What changed, by revision (see [`changes_since`](Self::changes_since)).
    changes: ChangeLog,
}

impl Session {
    pub fn new(project: Project) -> Self {
        Self {
            project,
            ..Self::default()
        }
    }

    /// Opens a project bundle from disk.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let (project, path) = project_file::load(path.as_ref())?;
        Ok(Self {
            project,
            path: Some(path),
            ..Self::default()
        })
    }

    /// A session from an autosave (see
    /// [`write_autosave`](Self::write_autosave)), of the bundle at `bundle`
    /// (`None` if it was never saved). It has unsaved changes: the ones the
    /// autosave kept.
    pub fn recover(autosave: &Path, bundle: Option<PathBuf>) -> Result<Self, CoreError> {
        let project = project_file::read_document(autosave)?;
        Ok(Self {
            project,
            path: bundle,
            revision: 1,
            ..Self::default()
        })
    }

    /// Writes the project as it is, unsaved changes and all, to `file`,
    /// with every source's absolute path: an autosave, to recover from if
    /// Dawkey stops without saving. Doesn't save the project.
    pub fn write_autosave(&self, file: &Path) -> Result<(), CoreError> {
        let mut project = self.project.clone();
        let present = self.plugin_ids();
        project.plugin_states.retain(|id, _| present.contains(id));
        for source in &mut project.sources {
            source.path = resolve_source_path(self.path.as_deref(), &source.path);
        }
        project_file::write_document(&project, file)
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision || self.states_changed
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|g| g.label.as_str())
    }

    /// What changed after revision `since`: every undo step, undo, redo and
    /// source added, with what it touched. The log keeps the last thousand.
    pub fn changes_since(&self, since: u64) -> ChangesSince {
        self.changes.since(since, self.revision)
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|g| g.label.as_str())
    }

    pub fn state(&self) -> SessionState {
        let mut project = self.project.clone();
        let states = std::mem::take(&mut project.plugin_states);
        let present: Vec<PluginId> = self.plugin_ids();
        SessionState {
            project,
            plugin_state_bytes: states
                .iter()
                .filter(|(id, _)| present.contains(id))
                .map(|(&id, state)| (id, state.bytes().len()))
                .collect(),
            path: self.path.clone(),
            revision: self.revision,
            dirty: self.is_dirty(),
            undo: self.undo_label().map(str::to_owned),
            redo: self.redo_label().map(str::to_owned),
        }
    }

    /// Adds an audio file to the project's pool. This is not an undo step:
    /// sources stay in the pool, and undo removes the clips that use them.
    /// `frames` is the length at the project sample rate.
    pub fn add_source(
        &mut self,
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        channels: u16,
        frames: u64,
    ) -> SourceId {
        let id = self.project.alloc_source_id();
        self.project.sources.push(Source {
            id,
            name: name.into(),
            path: path.into(),
            channels,
            frames,
        });
        self.revision += 1;
        self.changes.push(Change::new(
            self.revision,
            ChangeKind::Source,
            "add_source",
            &self.project,
            &[],
            &[],
        ));
        id
    }

    /// Keeps what a plugin saved of itself (beyond its parameters) for slot
    /// `id`, or forgets it. This is not an undo step: the plugin changed
    /// itself, and undoing a slot's removal should bring it back as it was.
    /// The project counts as changed until it is saved.
    pub fn set_plugin_state(&mut self, id: PluginId, state: Option<PluginState>) {
        let changed = match state {
            Some(state) => self.project.plugin_states.insert(id, state.clone()) != Some(state),
            None => self.project.plugin_states.remove(&id).is_some(),
        };
        self.states_changed |= changed;
    }

    /// Every plugin slot in the project.
    fn plugin_ids(&self) -> Vec<PluginId> {
        self.project
            .all_tracks()
            .flat_map(|t| &t.plugins)
            .map(|p| p.id)
            .collect()
    }

    /// Absolute path of a source's file. Relative source paths are relative
    /// to the project bundle.
    pub fn source_path(&self, id: SourceId) -> Option<PathBuf> {
        let source = self.project.source(id)?;
        Some(resolve_source_path(self.path.as_deref(), &source.path))
    }

    /// Runs one command as one undo step.
    pub fn execute(&mut self, command: Command) -> Result<Outcome, CoreError> {
        let label = command.name().to_owned();
        let mut outcomes = self.execute_batch(label, vec![command])?;
        Ok(outcomes.pop().unwrap_or_default())
    }

    /// Runs a command that joins the previous undo step if that step came
    /// from a command with the same `merge_key` and nothing else happened in
    /// between. A fader drag sends many gain changes under one key and is
    /// undone in one step. Call [`end_merge`](Self::end_merge) when the
    /// gesture ends.
    pub fn execute_merging(
        &mut self,
        command: Command,
        merge_key: &str,
    ) -> Result<Outcome, CoreError> {
        let label = command.name().to_owned();
        let mut command = Some(command);
        let (mut outcomes, inverses) =
            self.apply_commands(1, |_, _| Ok(command.take().expect("one command")))?;
        if !inverses.is_empty() {
            self.revision += 1;
            let change = Change::new(
                self.revision,
                ChangeKind::Command,
                &label,
                &self.project,
                &inverses,
                &outcomes,
            );
            match self.undo.last_mut() {
                Some(group) if group.merge_key.as_deref() == Some(merge_key) => {
                    // The new inverses revert first.
                    let mut merged = inverses;
                    merged.append(&mut group.inverses);
                    group.inverses = merged;
                    match self.changes.last_mut() {
                        Some(last) if last.label == label => last.merge(change),
                        _ => self.changes.push(change),
                    }
                }
                _ => {
                    self.end_merge();
                    self.push_undo(UndoGroup {
                        label,
                        inverses,
                        merge_key: Some(merge_key.to_owned()),
                    });
                    self.changes.push(change);
                }
            }
            self.redo.clear();
        }
        Ok(outcomes.pop().unwrap_or_default())
    }

    /// Stops later commands from merging into the last undo step.
    pub fn end_merge(&mut self) {
        if let Some(group) = self.undo.last_mut() {
            group.merge_key = None;
        }
    }

    /// Runs several commands as a single undo step. All-or-nothing: if any
    /// command fails, the project is left as it was before the batch.
    pub fn execute_batch(
        &mut self,
        label: impl Into<String>,
        commands: Vec<Command>,
    ) -> Result<Vec<Outcome>, CoreError> {
        let mut commands = commands.into_iter();
        let count = commands.len();
        self.execute_batch_with(label, count, |_, _| {
            Ok(commands.next().expect("one command per step"))
        })
    }

    /// Runs `count` commands as a single undo step, each made by `next`
    /// from the project as the earlier ones left it and their outcomes (so
    /// a command can use IDs an earlier one created). All-or-nothing: if
    /// `next` or a command fails, the project is left as it was before the
    /// batch.
    pub fn execute_batch_with<F>(
        &mut self,
        label: impl Into<String>,
        count: usize,
        next: F,
    ) -> Result<Vec<Outcome>, CoreError>
    where
        F: FnMut(&Project, &[Outcome]) -> Result<Command, CoreError>,
    {
        let (outcomes, inverses) = self.apply_commands(count, next)?;
        if !inverses.is_empty() {
            let label = label.into();
            self.end_merge();
            self.revision += 1;
            self.changes.push(Change::new(
                self.revision,
                ChangeKind::Command,
                &label,
                &self.project,
                &inverses,
                &outcomes,
            ));
            self.push_undo(UndoGroup {
                label,
                inverses,
                merge_key: None,
            });
            self.redo.clear();
        }
        Ok(outcomes)
    }

    /// Applies `count` commands from `next` all-or-nothing. Returns their
    /// outcomes and the inverse edits, in the order that reverts them all.
    fn apply_commands<F>(
        &mut self,
        count: usize,
        mut next: F,
    ) -> Result<(Vec<Outcome>, Vec<Edit>), CoreError>
    where
        F: FnMut(&Project, &[Outcome]) -> Result<Command, CoreError>,
    {
        let mut outcomes = Vec::with_capacity(count);
        let mut inverses: Vec<Edit> = Vec::new();
        // In a batch, an error says which step failed.
        let step = |index: usize, name: Option<&str>, err: CoreError| {
            if count < 2 {
                return err;
            }
            match name {
                Some(name) => err.in_context(&format!("operation {index} ({name})")),
                None => err.in_context(&format!("operation {index}")),
            }
        };
        for index in 0..count {
            let command = match next(&self.project, &outcomes) {
                Ok(command) => command,
                Err(err) => {
                    edit::rollback(&mut self.project, inverses);
                    return Err(step(index, None, err));
                }
            };
            let name = command.name();
            let result = command
                .into_edits(&mut self.project)
                .and_then(|(edits, outcome)| {
                    edit::apply_all(&mut self.project, edits).map(|inv| (inv, outcome))
                });
            match result {
                Ok((mut inv, outcome)) => {
                    // `inv` reverts this command; it must run before the
                    // inverses of earlier commands.
                    inv.append(&mut inverses);
                    inverses = inv;
                    outcomes.push(outcome);
                }
                Err(err) => {
                    edit::rollback(&mut self.project, inverses);
                    return Err(step(index, Some(name), err));
                }
            }
        }
        Ok((outcomes, inverses))
    }

    /// Reverts the last undo step. Returns its label, or `None` if there was
    /// nothing to undo.
    pub fn undo(&mut self) -> Result<Option<String>, CoreError> {
        let Some(group) = self.undo.pop() else {
            return Ok(None);
        };
        let label = group.label.clone();
        let redo = self.revert(group, ChangeKind::Undo)?;
        self.redo.push(redo);
        self.end_merge();
        Ok(Some(label))
    }

    /// Re-applies the last undone step. Returns its label, or `None` if there
    /// was nothing to redo.
    pub fn redo(&mut self) -> Result<Option<String>, CoreError> {
        let Some(group) = self.redo.pop() else {
            return Ok(None);
        };
        let label = group.label.clone();
        let undo = self.revert(group, ChangeKind::Redo)?;
        self.push_undo(undo);
        Ok(Some(label))
    }

    /// Applies a group's inverses and returns the group that reverts that.
    fn revert(&mut self, group: UndoGroup, kind: ChangeKind) -> Result<UndoGroup, CoreError> {
        let inverses = edit::apply_all(&mut self.project, group.inverses)?;
        self.revision += 1;
        self.changes.push(Change::new(
            self.revision,
            kind,
            &group.label,
            &self.project,
            &inverses,
            &[],
        ));
        Ok(UndoGroup {
            label: group.label,
            inverses,
            merge_key: None,
        })
    }

    fn push_undo(&mut self, group: UndoGroup) {
        if self.undo.len() == UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(group);
    }

    /// Saves to `path`, or to the current path if `None`. A `.dawkey`
    /// extension is added if missing. Returns the bundle path written.
    pub fn save(&mut self, path: Option<&Path>) -> Result<PathBuf, CoreError> {
        let target = match (path, &self.path) {
            (Some(p), _) => project_file::bundle_path(p),
            (None, Some(p)) => p.clone(),
            (None, None) => {
                return Err(CoreError::invalid(
                    "the project has never been saved; a path is required",
                ));
            }
        };
        // Copy sources that live outside the target bundle into it, and store
        // their paths relative to it.
        let mut project = self.project.clone();
        // States of slots that are gone (kept for undo) aren't saved.
        let present = self.plugin_ids();
        project.plugin_states.retain(|id, _| present.contains(id));
        for source in &mut project.sources {
            let current = resolve_source_path(self.path.as_deref(), &source.path);
            source.path = project_file::store_in_bundle(&current, &target, source.id)?;
        }
        project_file::save(&project, &target)?;
        self.project.sources = project.sources;
        self.path = Some(target.clone());
        self.saved_revision = self.revision;
        self.states_changed = false;
        Ok(target)
    }
}

fn resolve_source_path(bundle: Option<&Path>, path: &Path) -> PathBuf {
    match bundle {
        Some(bundle) if path.is_relative() => bundle.join(path),
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{
        AddClip, AddTrack, MoveClip, RemoveClip, RemoveTrack, RenameClip, RenameTrack, SetClipGain,
        SetTempo, SetTrackGain,
    };
    use crate::model::TrackKind;

    fn add(session: &mut Session, name: &str) -> TrackId {
        session
            .execute(
                AddTrack {
                    name: Some(name.into()),
                    ..Default::default()
                }
                .into(),
            )
            .unwrap()
            .track
            .unwrap()
    }

    #[test]
    fn add_undo_redo_keeps_track_id() {
        let mut s = Session::default();
        let id = add(&mut s, "Drums");
        assert_eq!(s.project().tracks.len(), 1);

        assert_eq!(s.undo().unwrap().as_deref(), Some("add_track"));
        assert!(s.project().tracks.is_empty());

        assert_eq!(s.redo().unwrap().as_deref(), Some("add_track"));
        assert_eq!(s.project().tracks[0].id, id);
        assert_eq!(s.project().tracks[0].name, "Drums");
    }

    #[test]
    fn remove_undo_restores_position_and_fields() {
        let mut s = Session::default();
        let a = add(&mut s, "A");
        let b = add(&mut s, "B");
        let c = add(&mut s, "C");
        s.execute(
            SetTrackGain {
                track: b,
                gain_db: -6.0,
            }
            .into(),
        )
        .unwrap();
        s.execute(RemoveTrack { track: b }.into()).unwrap();
        let ids: Vec<_> = s.project().tracks.iter().map(|t| t.id).collect();
        assert_eq!(ids, [a, c]);

        s.undo().unwrap();
        let restored = &s.project().tracks[1];
        assert_eq!((restored.id, restored.gain_db), (b, -6.0));
    }

    #[test]
    fn ids_are_not_reused_after_undo() {
        let mut s = Session::default();
        let first = add(&mut s, "A");
        s.undo().unwrap();
        let second = add(&mut s, "B");
        assert_ne!(first, second);
    }

    #[test]
    fn new_command_clears_redo() {
        let mut s = Session::default();
        add(&mut s, "A");
        s.undo().unwrap();
        add(&mut s, "B");
        assert_eq!(s.redo().unwrap(), None);
    }

    #[test]
    fn failed_batch_leaves_project_unchanged() {
        let mut s = Session::default();
        let id = add(&mut s, "A");
        let before = s.project().clone();
        let revision = s.revision();

        let err = s
            .execute_batch(
                "batch",
                vec![
                    RenameTrack {
                        track: id,
                        name: "Renamed".into(),
                    }
                    .into(),
                    AddTrack::default().into(),
                    SetTempo::new(5000.0).into(),
                ],
            )
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidArgument(_)));
        assert_eq!(s.project().tracks, before.tracks);
        assert_eq!(s.project().name, before.name);
        assert_eq!(s.revision(), revision);
        assert_eq!(s.undo_label(), Some("add_track"));
    }

    #[test]
    fn batch_undoes_as_one_step() {
        let mut s = Session::default();
        s.execute_batch(
            "setup",
            vec![
                AddTrack::default().into(),
                AddTrack {
                    kind: TrackKind::Midi,
                    ..Default::default()
                }
                .into(),
                SetTempo::new(90.0).into(),
            ],
        )
        .unwrap();
        let names: Vec<_> = s.project().tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Audio 1", "MIDI 1"]);

        assert_eq!(s.undo().unwrap().as_deref(), Some("setup"));
        assert!(s.project().tracks.is_empty());
        assert_eq!(s.project().tempo_bpm, 120.0);
        assert_eq!(s.undo().unwrap(), None);
    }

    #[test]
    fn invalid_arguments_are_rejected() {
        let mut s = Session::default();
        let id = add(&mut s, "A");
        for cmd in [
            SetTrackGain {
                track: id,
                gain_db: 30.0,
            }
            .into(),
            SetTrackGain {
                track: TrackId(99),
                gain_db: 0.0,
            }
            .into(),
            RenameTrack {
                track: id,
                name: "   ".into(),
            }
            .into(),
            Command::from(SetTempo::new(0.0)),
        ] {
            assert!(s.execute(cmd).is_err());
        }
        assert_eq!(s.revision(), 1);
    }

    #[test]
    fn dirty_tracks_save_point() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::default();
        assert!(!s.is_dirty());
        add(&mut s, "A");
        assert!(s.is_dirty());
        assert!(s.save(None).is_err());
        let path = s.save(Some(&dir.path().join("song"))).unwrap();
        assert_eq!(path.extension().unwrap(), "dawkey");
        assert!(!s.is_dirty());
        s.undo().unwrap();
        assert!(s.is_dirty());
    }

    fn place(s: &mut Session, source: SourceId, track: Option<TrackId>, at: f64) -> Outcome {
        s.execute(AddClip::new(source, track, at).into()).unwrap()
    }

    #[test]
    fn add_clip_on_new_track_and_undo() {
        let mut s = Session::default();
        let src = s.add_source("Kick", "/tmp/kick.wav", 1, 48_000);
        let placed = place(&mut s, src, None, 1.5);
        let (track, clip) = (placed.track.unwrap(), placed.clip.unwrap());

        let t = s.project().track(track).unwrap();
        assert_eq!(t.name, "Kick");
        let c = &t.clips[0];
        assert_eq!((c.id, c.start, c.length), (clip, 72_000, 48_000));

        // One undo step removes both the clip and the track it created.
        s.undo().unwrap();
        assert!(s.project().tracks.is_empty());
        assert_eq!(s.project().sources.len(), 1, "sources stay in the pool");
        s.redo().unwrap();
        assert_eq!(s.project().clip(clip).unwrap().0.id, track);
    }

    #[test]
    fn move_remove_and_gain_clips() {
        let mut s = Session::default();
        let src = s.add_source("Loop", "/tmp/loop.wav", 2, 1000);
        let a = add(&mut s, "A");
        let b = add(&mut s, "B");
        let clip = place(&mut s, src, Some(a), 0.0).clip.unwrap();

        s.execute(
            MoveClip {
                clip,
                position_s: Some(2.0),
                track: Some(b),
            }
            .into(),
        )
        .unwrap();
        let (track, c) = s.project().clip(clip).unwrap();
        assert_eq!((track.id, c.start), (b, 96_000));
        s.undo().unwrap();
        let (track, c) = s.project().clip(clip).unwrap();
        assert_eq!((track.id, c.start), (a, 0));

        s.execute(
            SetClipGain {
                clip,
                gain_db: -3.0,
            }
            .into(),
        )
        .unwrap();
        assert_eq!(s.project().clip(clip).unwrap().1.gain_db, -3.0);

        s.execute(RemoveClip { clip }.into()).unwrap();
        assert!(s.project().clip(clip).is_none());
        s.undo().unwrap();
        assert_eq!(s.project().clip(clip).unwrap().1.gain_db, -3.0);
    }

    #[test]
    fn rename_clips() {
        let mut s = Session::default();
        let src = s.add_source("Loop", "/tmp/loop.wav", 2, 1000);
        let clip = place(&mut s, src, None, 0.0).clip.unwrap();
        let name = |s: &Session| s.project().clip(clip).unwrap().1.name.clone();
        let rename = |s: &mut Session, name: &str| {
            s.execute(
                RenameClip {
                    clip,
                    name: name.into(),
                }
                .into(),
            )
        };
        assert_eq!(name(&s), None);
        rename(&mut s, "  Verse 1 ").unwrap();
        assert_eq!(name(&s).as_deref(), Some("Verse 1"));
        // An empty name goes back to the default.
        rename(&mut s, " ").unwrap();
        assert_eq!(name(&s), None);
        s.undo().unwrap();
        assert_eq!(name(&s).as_deref(), Some("Verse 1"));
        s.undo().unwrap();
        assert_eq!(name(&s), None);
        s.redo().unwrap();
        assert_eq!(name(&s).as_deref(), Some("Verse 1"));

        for bad in ["a\nb", &"x".repeat(257)] {
            let err = rename(&mut s, bad).unwrap_err();
            assert!(matches!(err, CoreError::InvalidArgument(_)), "{bad:?}");
        }
        let err = s
            .execute(
                RenameClip {
                    clip: ClipId(999),
                    name: "x".into(),
                }
                .into(),
            )
            .unwrap_err();
        assert!(matches!(err, CoreError::ClipNotFound(_)));
        assert_eq!(name(&s).as_deref(), Some("Verse 1"));
    }

    #[test]
    fn clip_validation() {
        let mut s = Session::default();
        let src = s.add_source("x", "/tmp/x.wav", 1, 10);
        let midi = s
            .execute(
                AddTrack {
                    kind: TrackKind::Midi,
                    ..Default::default()
                }
                .into(),
            )
            .unwrap()
            .track;
        let revision = s.revision();
        for cmd in [
            AddClip::new(src, midi, 0.0).into(),
            AddClip::new(SourceId(99), None, 0.0).into(),
            AddClip::new(src, None, -1.0).into(),
            Command::from(MoveClip {
                clip: ClipId(1),
                position_s: Some(1.0),
                track: None,
            }),
        ] {
            assert!(s.execute(cmd).is_err());
        }
        assert_eq!(s.revision(), revision);
    }

    #[test]
    fn save_copies_sources_into_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("take.wav");
        std::fs::write(&original, b"not really audio").unwrap();

        let mut s = Session::default();
        let src = s.add_source("take", &original, 1, 10);
        let bundle = s.save(Some(&dir.path().join("Song"))).unwrap();

        let stored = &s.project().source(src).unwrap().path;
        assert!(stored.is_relative(), "{stored:?}");
        let copied = s.source_path(src).unwrap();
        assert!(copied.starts_with(&bundle));
        assert_eq!(std::fs::read(&copied).unwrap(), b"not really audio");

        // Saving elsewhere copies again; the reopened project resolves paths
        // against its own bundle.
        let other = s.save(Some(&dir.path().join("Copy"))).unwrap();
        let reopened = Session::open(&other).unwrap();
        let path = reopened.source_path(src).unwrap();
        assert!(path.starts_with(&other) && path.exists(), "{path:?}");
    }

    #[test]
    fn merging_commands_undo_in_one_step() {
        let mut s = Session::default();
        let id = add(&mut s, "A");
        for gain in [-1.0, -2.0, -3.0] {
            s.execute_merging(
                SetTrackGain {
                    track: id,
                    gain_db: gain,
                }
                .into(),
                "drag 1",
            )
            .unwrap();
        }
        assert_eq!(s.project().tracks[0].gain_db, -3.0);
        s.end_merge();
        // A new gesture with the same key starts a new step.
        s.execute_merging(
            SetTrackGain {
                track: id,
                gain_db: -9.0,
            }
            .into(),
            "drag 1",
        )
        .unwrap();

        s.undo().unwrap();
        assert_eq!(s.project().tracks[0].gain_db, -3.0);
        s.undo().unwrap();
        assert_eq!(s.project().tracks[0].gain_db, 0.0);
        assert_eq!(s.undo_label(), Some("add_track"));
        s.redo().unwrap();
        assert_eq!(s.project().tracks[0].gain_db, -3.0);

        // An ordinary command in between ends the merge.
        s.execute_merging(
            SetTrackGain {
                track: id,
                gain_db: -4.0,
            }
            .into(),
            "k",
        )
        .unwrap();
        s.execute(SetTempo::new(100.0).into()).unwrap();
        s.execute_merging(
            SetTrackGain {
                track: id,
                gain_db: -5.0,
            }
            .into(),
            "k",
        )
        .unwrap();
        s.undo().unwrap();
        assert_eq!(s.project().tracks[0].gain_db, -4.0);
    }

    #[test]
    fn loop_punch_arm_and_clip_mute() {
        use crate::command::{SetClipMute, SetLoop, SetPunch, SetTrackArm, SetTrackInput};
        use crate::model::{InputChannels, TimeRange};

        let mut s = Session::default();
        let src = s.add_source("take", "/tmp/take.wav", 1, 48_000);
        let track = add(&mut s, "Vox");
        let clip = place(&mut s, src, Some(track), 0.0).clip.unwrap();

        s.execute(
            SetLoop {
                start_s: Some(1.0),
                end_s: Some(3.0),
                enabled: Some(true),
            }
            .into(),
        )
        .unwrap();
        let region = s.project().loop_region;
        assert_eq!(
            region.active(),
            Some(TimeRange {
                start: 48_000,
                end: 144_000
            })
        );
        s.execute(
            SetLoop {
                enabled: Some(false),
                ..Default::default()
            }
            .into(),
        )
        .unwrap();
        assert_eq!(s.project().loop_region.active(), None);
        assert!(s.project().loop_region.range.is_some(), "range is kept");
        for bad in [
            SetLoop {
                start_s: Some(2.0),
                end_s: Some(1.0),
                enabled: None,
            },
            SetLoop {
                start_s: Some(2.0),
                end_s: None,
                enabled: None,
            },
            SetLoop::default(),
        ] {
            assert!(s.execute(bad.into()).is_err());
        }
        assert!(
            s.execute(
                SetPunch {
                    enabled: Some(true),
                    ..Default::default()
                }
                .into()
            )
            .is_err()
        );

        s.execute(SetTrackArm { track, armed: true }.into())
            .unwrap();
        s.execute(
            SetTrackInput {
                track,
                input: InputChannels::Right,
            }
            .into(),
        )
        .unwrap();
        let t = s.project().track(track).unwrap();
        assert!(t.armed);
        assert_eq!(t.input, InputChannels::Right);

        s.execute(SetClipMute { clip, muted: true }.into()).unwrap();
        assert!(s.project().clip(clip).unwrap().1.muted);
        s.undo().unwrap();
        assert!(!s.project().clip(clip).unwrap().1.muted);
    }

    fn note(pitch: u8, start_beats: f64, length_beats: f64) -> crate::command::NoteSpec {
        crate::command::NoteSpec {
            pitch,
            start_beats,
            length_beats,
            velocity: 100,
        }
    }

    #[test]
    fn midi_clips_and_notes() {
        use crate::command::{
            AddMidiClip, AddMidiNotes, EditMidiNotes, NoteChange, QuantizeMidiNotes,
            RemoveMidiNotes, SetClipLength,
        };

        let mut s = Session::default();
        // 120 BPM at 48 kHz: a beat is 24 000 frames, a 4/4 bar 96 000.
        let added = s
            .execute(
                AddMidiClip {
                    position_s: 2.0,
                    notes: vec![note(60, 0.0, 1.0), note(64, 1.0, 0.5)],
                    ..Default::default()
                }
                .into(),
            )
            .unwrap();
        let (track, clip) = (added.track.unwrap(), added.clip.unwrap());
        assert_eq!(added.notes.len(), 2);
        let t = s.project().track(track).unwrap();
        assert_eq!((t.kind, t.name.as_str()), (TrackKind::Midi, "MIDI 1"));
        assert!(t.synth.is_some());
        let c = &t.clips[0];
        assert_eq!((c.start, c.length), (96_000, 96_000), "one bar by default");
        let starts: Vec<_> = c
            .notes()
            .iter()
            .map(|n| (n.pitch, n.start, n.length))
            .collect();
        assert_eq!(starts, [(60, 0, 24_000), (64, 24_000, 12_000)]);

        // Adding a note past the end grows the clip to whole bars.
        let more = s
            .execute(
                AddMidiNotes {
                    clip,
                    notes: vec![note(67, 4.5, 1.0)],
                }
                .into(),
            )
            .unwrap();
        assert_eq!(s.project().clip(clip).unwrap().1.length, 192_000);
        let id = more.notes[0];
        s.undo().unwrap();
        assert_eq!(s.project().clip(clip).unwrap().1.length, 96_000);
        assert_eq!(s.project().clip(clip).unwrap().1.notes().len(), 2);
        s.redo().unwrap();

        s.execute(
            EditMidiNotes {
                clip,
                notes: vec![NoteChange {
                    id,
                    pitch: Some(72),
                    start_beats: Some(0.1),
                    length_beats: None,
                    velocity: Some(64),
                }],
            }
            .into(),
        )
        .unwrap();
        let notes = s.project().clip(clip).unwrap().1.notes().to_vec();
        assert_eq!(
            (notes[0].pitch, notes[0].start, notes[0].velocity),
            (60, 0, 100)
        );
        assert_eq!(
            (notes[1].id, notes[1].pitch, notes[1].start),
            (id, 72, 2_400)
        );

        s.execute(
            QuantizeMidiNotes {
                clip,
                grid_beats: 0.25,
                notes: None,
            }
            .into(),
        )
        .unwrap();
        let note = *s
            .project()
            .clip(clip)
            .unwrap()
            .1
            .notes()
            .iter()
            .find(|n| n.id == id)
            .unwrap();
        assert_eq!(note.start, 0);

        s.execute(
            RemoveMidiNotes {
                clip,
                notes: vec![id],
            }
            .into(),
        )
        .unwrap();
        assert_eq!(s.project().clip(clip).unwrap().1.notes().len(), 2);

        s.execute(
            SetClipLength {
                clip,
                length_s: 1.0,
            }
            .into(),
        )
        .unwrap();
        assert_eq!(s.project().clip(clip).unwrap().1.length, 48_000);
    }

    #[test]
    fn trim_and_split_audio_clips() {
        use crate::command::{SplitClip, TrimClip};

        let mut s = Session::default();
        // Four seconds of source at 48 kHz, placed at 1 s.
        let src = s.add_source("Loop", "/tmp/loop.wav", 2, 192_000);
        let placed = place(&mut s, src, None, 1.0);
        let (track, clip) = (placed.track.unwrap(), placed.clip.unwrap());
        let edges = |s: &Session, id| {
            let c = s.project().clip(id).unwrap().1.clone();
            let crate::model::ClipContent::Audio { offset, .. } = c.content else {
                panic!("audio clip")
            };
            (c.start, c.length, offset)
        };

        // Trimming the start later skips into the source; the end stays.
        s.execute(
            TrimClip {
                clip,
                start_s: Some(1.5),
                end_s: Some(4.0),
            }
            .into(),
        )
        .unwrap();
        assert_eq!(edges(&s, clip), (72_000, 120_000, 24_000));
        assert_eq!(s.undo().unwrap().as_deref(), Some("trim_clip"));
        assert_eq!(edges(&s, clip), (48_000, 192_000, 0));
        s.redo().unwrap();

        // Back to the source's start, but no earlier, nor past its end.
        let trim = |start_s, end_s| -> Command {
            TrimClip {
                clip,
                start_s,
                end_s,
            }
            .into()
        };
        let revision = s.revision();
        for bad in [
            trim(Some(0.5), None),
            trim(None, Some(5.5)),
            trim(Some(3.0), Some(2.0)),
            trim(None, None),
        ] {
            assert!(s.execute(bad).is_err());
        }
        assert_eq!(s.revision(), revision);
        s.execute(trim(Some(1.0), Some(5.0))).unwrap();
        assert_eq!(edges(&s, clip), (48_000, 192_000, 0));

        // Splitting: the second part plays on from where the first stops.
        s.execute(
            SetClipGain {
                clip,
                gain_db: -3.0,
            }
            .into(),
        )
        .unwrap();
        let second = s
            .execute(SplitClip { clip, at_s: 2.0 }.into())
            .unwrap()
            .clip
            .unwrap();
        assert_ne!(second, clip);
        assert_eq!(edges(&s, clip), (48_000, 48_000, 0));
        assert_eq!(edges(&s, second), (96_000, 144_000, 48_000));
        let t = s.project().track(track).unwrap();
        let order: Vec<_> = t.clips.iter().map(|c| c.id).collect();
        assert_eq!(order, [clip, second]);
        assert_eq!(t.clips[1].gain_db, -3.0);

        // One undo step joins them again.
        assert_eq!(s.undo().unwrap().as_deref(), Some("split_clip"));
        assert!(s.project().clip(second).is_none());
        assert_eq!(edges(&s, clip), (48_000, 192_000, 0));

        // Only inside the clip.
        for at_s in [1.0, 5.0, 0.5, 9.0] {
            assert!(s.execute(SplitClip { clip, at_s }.into()).is_err());
        }
    }

    #[test]
    fn takes_stack_in_lanes_and_comp() {
        use crate::command::CompTake;

        let mut s = Session::default();
        let src = s.add_source("Vox", "/tmp/vox.wav", 1, 96_000);
        let first = place(&mut s, src, None, 0.0);
        let (track, a) = (first.track.unwrap(), first.clip.unwrap());
        // A second take over 0.5-1.5 s: a lane up, and the first take goes
        // silent under it.
        let take = |s: &mut Session, at: f64, muted: bool| {
            s.execute(
                AddClip {
                    length_s: Some(1.0),
                    muted,
                    take: true,
                    ..AddClip::new(src, Some(track), at)
                }
                .into(),
            )
            .unwrap()
            .clip
            .unwrap()
        };
        let b = take(&mut s, 0.5, false);
        // (start, length, muted, lane, offset), by start then lane.
        let clips = |s: &Session| {
            let mut v: Vec<_> = s
                .project()
                .track(track)
                .unwrap()
                .clips
                .iter()
                .map(|c| {
                    let crate::model::ClipContent::Audio { offset, .. } = c.content else {
                        panic!("audio clip")
                    };
                    (c.start, c.length, c.muted, c.lane, offset)
                })
                .collect();
            v.sort_by_key(|c| (c.0, c.3));
            v
        };
        assert_eq!(
            clips(&s),
            [
                (0, 24_000, false, 0, 0),
                (24_000, 48_000, true, 0, 24_000),
                (24_000, 48_000, false, 1, 0),
                (72_000, 24_000, false, 0, 72_000),
            ]
        );
        assert_eq!(s.undo_label(), Some("add_clip"));

        // A muted take goes another lane up and changes nothing else.
        let c = take(&mut s, 0.5, true);
        assert_eq!(s.project().clip(c).unwrap().1.lane, 2);
        assert_eq!(clips(&s).iter().filter(|c| !c.2).count(), 3);

        // Comping 1.0-1.25 s of the muted take: it plays there and the
        // second take doesn't. One undo step.
        let playing = s
            .execute(
                CompTake {
                    clip: c,
                    start_s: Some(1.0),
                    end_s: Some(1.25),
                }
                .into(),
            )
            .unwrap()
            .clip
            .unwrap();
        assert_ne!(playing, c);
        let audible: Vec<_> = clips(&s).into_iter().filter(|c| !c.2).collect();
        assert_eq!(
            audible,
            [
                (0, 24_000, false, 0, 0),
                (24_000, 24_000, false, 1, 0),
                (48_000, 12_000, false, 2, 24_000),
                (60_000, 12_000, false, 1, 36_000),
                (72_000, 24_000, false, 0, 72_000),
            ]
        );
        s.undo().unwrap();
        assert_eq!(clips(&s).iter().filter(|c| !c.2).count(), 3);
        assert_eq!(s.project().track(track).unwrap().clips.len(), 5);

        // The first take's middle (`a` is its part before the split), whole:
        // everything else goes silent where it plays.
        let middle = s
            .project()
            .track(track)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.start == 24_000 && c.lane == 0)
            .unwrap()
            .id;
        assert_ne!(middle, a);
        s.execute(
            CompTake {
                clip: middle,
                start_s: None,
                end_s: None,
            }
            .into(),
        )
        .unwrap();
        let audible: Vec<_> = clips(&s).into_iter().filter(|c| !c.2).collect();
        assert_eq!(
            audible,
            [
                (0, 24_000, false, 0, 0),
                (24_000, 48_000, false, 0, 24_000),
                (72_000, 24_000, false, 0, 72_000),
            ]
        );
        assert!(s.project().clip(b).unwrap().1.muted);

        // A range outside the clip is an error.
        let err = s
            .execute(
                CompTake {
                    clip: b,
                    start_s: Some(1.5),
                    end_s: Some(2.0),
                }
                .into(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("must overlap"), "{err}");
    }

    #[test]
    fn comping_reaches_across_a_lane() {
        use crate::command::CompTake;

        let mut s = Session::default();
        let src = s.add_source("Vox", "/tmp/vox.wav", 1, 96_000);
        let first = place(&mut s, src, None, 0.0);
        let (track, a) = (first.track.unwrap(), first.clip.unwrap());
        s.execute(
            AddClip {
                take: true,
                ..AddClip::new(src, Some(track), 0.0)
            }
            .into(),
        )
        .unwrap();
        let comp = |s: &mut Session, clip, start_s, end_s| {
            s.execute(
                CompTake {
                    clip,
                    start_s: Some(start_s),
                    end_s: Some(end_s),
                }
                .into(),
            )
            .unwrap()
        };
        comp(&mut s, a, 0.5, 1.0);
        // Lane 0 is now three parts, the middle one playing. Swiping from
        // the first part to 1.5 s takes in the whole first part and the
        // start of the third.
        comp(&mut s, a, 0.0, 1.5);
        let mut audible: Vec<_> = s
            .project()
            .track(track)
            .unwrap()
            .clips
            .iter()
            .filter(|c| !c.muted)
            .map(|c| (c.lane, c.start, c.end()))
            .collect();
        audible.sort();
        assert_eq!(
            audible,
            [
                (0, 0, 24_000),
                (0, 24_000, 48_000),
                (0, 48_000, 72_000),
                (1, 72_000, 96_000)
            ]
        );
    }

    #[test]
    fn duplicate_and_repeat_clips() {
        use crate::command::{
            AddMidiClip, AddTrack, DuplicateClip, RepeatClip, SetClipGain, SetClipMute, TrimClip,
        };
        use crate::model::TrackKind;

        let mut s = Session::default();
        // A one-second audio clip at 1 s, two seconds into its source, at
        // -6 dB and muted.
        let src = s.add_source("Loop", "/tmp/loop.wav", 2, 192_000);
        let placed = place(&mut s, src, None, 1.0);
        let (track, clip) = (placed.track.unwrap(), placed.clip.unwrap());
        s.execute(
            TrimClip {
                clip,
                start_s: Some(3.0),
                end_s: None,
            }
            .into(),
        )
        .unwrap();
        s.execute(
            TrimClip {
                clip,
                start_s: None,
                end_s: Some(4.0),
            }
            .into(),
        )
        .unwrap();
        let (clip_start, clip_len) = (144_000, 48_000);
        s.execute(
            SetClipGain {
                clip,
                gain_db: -6.0,
            }
            .into(),
        )
        .unwrap();
        s.execute(SetClipMute { clip, muted: true }.into()).unwrap();
        let other = s
            .execute(
                AddTrack {
                    name: Some("Other".to_owned()),
                    kind: TrackKind::Audio,
                    index: None,
                }
                .into(),
            )
            .unwrap()
            .track
            .unwrap();

        // By default the copy follows the clip, keeping everything but its ID.
        let dup = |position_s, track| -> Command {
            DuplicateClip {
                clip,
                position_s,
                track,
            }
            .into()
        };
        let copy = s.execute(dup(None, None)).unwrap();
        assert_eq!(copy.track, Some(track));
        let copy = copy.clip.unwrap();
        assert_ne!(copy, clip);
        let original = s.project().clip(clip).unwrap().1.clone();
        let (on, c) = s.project().clip(copy).unwrap();
        assert_eq!(on.id, track);
        assert_eq!(
            c,
            &crate::model::Clip {
                id: copy,
                start: clip_start + clip_len,
                ..original.clone()
            }
        );
        assert_eq!(s.undo().unwrap().as_deref(), Some("duplicate_clip"));
        assert!(s.project().clip(copy).is_none());

        // Onto another audio track, anywhere; not onto a MIDI track.
        let copy = s
            .execute(dup(Some(10.0), Some(other)))
            .unwrap()
            .clip
            .unwrap();
        let (on, c) = s.project().clip(copy).unwrap();
        assert_eq!((on.id, c.start), (other, 480_000));
        let midi = s
            .execute(
                AddTrack {
                    name: None,
                    kind: TrackKind::Midi,
                    index: None,
                }
                .into(),
            )
            .unwrap()
            .track
            .unwrap();
        let err = s.execute(dup(None, Some(midi))).unwrap_err();
        assert!(err.to_string().contains("MIDI track"), "{err}");

        // A MIDI copy's notes get new IDs.
        let midi_clip = s
            .execute(
                AddMidiClip {
                    track: Some(midi),
                    position_s: 0.0,
                    length_s: Some(1.0),
                    notes: vec![note(60, 0.0, 0.5), note(64, 1.0, 0.5)],
                    ..Default::default()
                }
                .into(),
            )
            .unwrap();
        let copy = s
            .execute(
                DuplicateClip {
                    clip: midi_clip.clip.unwrap(),
                    position_s: None,
                    track: None,
                }
                .into(),
            )
            .unwrap();
        assert_eq!(copy.notes.len(), 2);
        assert!(copy.notes.iter().all(|n| !midi_clip.notes.contains(n)));
        let notes = |id| {
            s.project()
                .clip(id)
                .unwrap()
                .1
                .notes()
                .iter()
                .map(|n| (n.pitch, n.start, n.length))
                .collect::<Vec<_>>()
        };
        assert_eq!(notes(midi_clip.clip.unwrap()), notes(copy.clip.unwrap()));

        // Repeating to 6.5 s makes two whole copies and a half one, in one
        // undo step.
        let repeated = s.execute(RepeatClip { clip, until_s: 6.5 }.into()).unwrap();
        assert_eq!(repeated.clips.len(), 3);
        assert_eq!(repeated.clip, Some(repeated.clips[0]));
        let spans: Vec<_> = repeated
            .clips
            .iter()
            .map(|id| {
                let c = s.project().clip(*id).unwrap().1;
                let crate::model::ClipContent::Audio { offset, .. } = c.content else {
                    panic!("audio clip")
                };
                (c.start, c.length, offset, c.muted)
            })
            .collect();
        assert_eq!(
            spans,
            vec![
                (192_000, 48_000, 96_000, true),
                (240_000, 48_000, 96_000, true),
                (288_000, 24_000, 96_000, true),
            ]
        );
        assert_eq!(s.undo().unwrap().as_deref(), Some("repeat_clip"));
        assert!(
            repeated
                .clips
                .iter()
                .all(|id| s.project().clip(*id).is_none())
        );

        // Repeats must go past the clip, and not too far.
        let repeat = |until_s| -> Command { RepeatClip { clip, until_s }.into() };
        let revision = s.revision();
        for bad in [repeat(4.0), repeat(1.0), repeat(2000.0)] {
            assert!(s.execute(bad).is_err());
        }
        assert_eq!(s.revision(), revision);
    }

    #[test]
    fn trim_and_split_midi_clips() {
        use crate::command::{AddMidiClip, ControlSpec, SplitClip, TrimClip};
        use crate::model::Controller;

        let mut s = Session::default();
        // 120 BPM: a beat is 24 000 frames. A clip at 1 s with a note on
        // each of the first four beats (the first one crossing beat 1) and
        // the mod wheel moving at beats 0.5 and 2.5.
        let cc = |time_beats, value| ControlSpec {
            controller: Controller::Cc { number: 1 },
            time_beats,
            value,
        };
        let clip = s
            .execute(
                AddMidiClip {
                    position_s: 1.0,
                    notes: vec![
                        note(60, 0.0, 1.5),
                        note(62, 1.0, 1.0),
                        note(64, 2.0, 1.0),
                        note(65, 3.0, 1.0),
                    ],
                    controls: vec![cc(0.5, 40), cc(2.5, 90)],
                    ..Default::default()
                }
                .into(),
            )
            .unwrap()
            .clip
            .unwrap();
        let get = |s: &Session, id| {
            let c = s.project().clip(id).unwrap().1.clone();
            let notes: Vec<_> = c.notes().iter().map(|n| (n.pitch, n.start)).collect();
            let controls: Vec<_> = c.controls().iter().map(|p| (p.frame, p.value)).collect();
            (c.start, c.length, notes, controls)
        };

        // Split at beat 1.25: the crossing notes stay in the first part, and
        // the second starts with the mod wheel where it was.
        let second = s
            .execute(SplitClip { clip, at_s: 1.625 }.into())
            .unwrap()
            .clip
            .unwrap();
        assert_eq!(
            get(&s, clip),
            (
                48_000,
                30_000,
                vec![(60, 0), (62, 24_000)],
                vec![(12_000, 40)]
            )
        );
        assert_eq!(
            get(&s, second),
            (
                78_000,
                66_000,
                vec![(64, 18_000), (65, 42_000)],
                vec![(0, 40), (30_000, 90)]
            )
        );
        s.undo().unwrap();

        // Trimming the start to beat 2 drops the notes before it; the
        // others keep their place on the timeline.
        s.execute(
            TrimClip {
                clip,
                start_s: Some(2.0),
                end_s: None,
            }
            .into(),
        )
        .unwrap();
        assert_eq!(
            get(&s, clip),
            (
                96_000,
                48_000,
                vec![(64, 0), (65, 24_000)],
                vec![(0, 40), (12_000, 90)]
            )
        );
        // Moving it earlier shifts them the other way.
        s.execute(
            TrimClip {
                clip,
                start_s: Some(0.5),
                end_s: None,
            }
            .into(),
        )
        .unwrap();
        assert_eq!(
            get(&s, clip),
            (
                24_000,
                120_000,
                vec![(64, 72_000), (65, 96_000)],
                vec![(72_000, 40), (84_000, 90)]
            )
        );
        s.undo().unwrap();
        s.undo().unwrap();
        assert_eq!(get(&s, clip).2.len(), 4);
    }

    #[test]
    fn tempo_changes_move_midi_but_not_audio() {
        use crate::command::{AddMidiClip, AddMidiNotes, RemoveTempoChange, SetTempoMap};
        use crate::tempo::TempoChange;

        let mut s = Session::default();
        let audio = add(&mut s, "A");
        let src = s.add_source("x", "/tmp/x.wav", 1, 480_000);
        let audio_clip = place(&mut s, src, Some(audio), 3.0).clip.unwrap();
        // At 120 BPM: beat 4 (2 s, frame 96 000), notes on beats 4.5 and 5.
        let clip = s
            .execute(
                AddMidiClip {
                    position_s: 2.0,
                    notes: vec![note(60, 0.5, 0.5), note(62, 1.0, 1.0)],
                    ..Default::default()
                }
                .into(),
            )
            .unwrap()
            .clip
            .unwrap();
        let frames = |s: &Session| {
            let c = s.project().clip(clip).unwrap().1;
            let notes: Vec<_> = c.notes().iter().map(|n| (n.start, n.length)).collect();
            (c.start, c.length, notes)
        };
        let before = frames(&s);
        assert_eq!(
            before,
            (96_000, 96_000, vec![(12_000, 12_000), (24_000, 24_000)])
        );

        // Half speed from beat 2 on: beat 4 is now at 1 s + 2 s = 144 000.
        s.execute(
            SetTempo {
                bpm: 60.0,
                at_beats: Some(2.0),
                ramp: false,
            }
            .into(),
        )
        .unwrap();
        assert_eq!(
            frames(&s),
            (144_000, 192_000, vec![(24_000, 24_000), (48_000, 48_000)])
        );
        // Audio stays where it was.
        assert_eq!(s.project().clip(audio_clip).unwrap().1.start, 144_000);
        assert_eq!(s.project().tempo_changes, [TempoChange::new(2.0, 60.0)]);

        // Notes added in beats land on the new grid.
        s.execute(
            AddMidiNotes {
                clip,
                notes: vec![note(64, 2.0, 1.0)],
            }
            .into(),
        )
        .unwrap();
        let added = *s.project().clip(clip).unwrap().1.notes().last().unwrap();
        assert_eq!((added.start, added.length), (96_000, 48_000));

        s.undo().unwrap();
        s.undo().unwrap();
        assert_eq!(frames(&s), before);
        assert!(s.project().tempo_changes.is_empty());
        s.redo().unwrap();

        // Removing the change, or replacing the map, re-times again; a
        // round trip ends where it began.
        s.execute(RemoveTempoChange { at_beats: 2.0 }.into())
            .unwrap();
        assert_eq!(frames(&s), before);
        s.execute(
            SetTempoMap {
                bpm: 90.0,
                changes: vec![TempoChange::new(8.0, 150.0)],
            }
            .into(),
        )
        .unwrap();
        assert_eq!(frames(&s).0, 128_000);
        s.execute(SetTempo::new(120.0).into()).unwrap();
        s.execute(RemoveTempoChange { at_beats: 8.0 }.into())
            .unwrap();
        assert_eq!(frames(&s), before);

        let revision = s.revision();
        for bad in [
            Command::from(RemoveTempoChange { at_beats: 3.0 }),
            SetTempo {
                bpm: 100.0,
                at_beats: Some(-1.0),
                ramp: false,
            }
            .into(),
            SetTempo {
                bpm: 100.0,
                at_beats: None,
                ramp: true,
            }
            .into(),
            SetTempoMap {
                bpm: 100.0,
                changes: vec![TempoChange::new(4.0, 5.0)],
            }
            .into(),
        ] {
            assert!(s.execute(bad).is_err());
        }
        assert_eq!(s.revision(), revision);
    }

    #[test]
    fn midi_controls() {
        use crate::command::{AddMidiClip, ControlSpec, ControlValue, SetMidiControls};
        use crate::model::Controller;

        let mut s = Session::default();
        let bend = Controller::PitchBend;
        let modwheel = Controller::Cc { number: 1 };
        let clip = s
            .execute(
                AddMidiClip {
                    position_s: 1.0,
                    controls: vec![ControlSpec {
                        controller: modwheel,
                        time_beats: 1.0,
                        value: 64,
                    }],
                    ..Default::default()
                }
                .into(),
            )
            .unwrap()
            .clip
            .unwrap();
        let points = |s: &Session| {
            let c = s.project().clip(clip).unwrap().1;
            c.controls()
                .iter()
                .map(|p| (p.controller, p.frame, p.value))
                .collect::<Vec<_>>()
        };
        assert_eq!(points(&s), [(modwheel, 24_000, 64)]);
        let set = |points: Vec<(f64, u16)>| SetMidiControls {
            clip,
            controller: bend,
            points: points
                .into_iter()
                .map(|(time_beats, value)| ControlValue { time_beats, value })
                .collect(),
        };
        // The last of two changes at one time wins.
        s.execute(set(vec![(2.0, 0), (0.5, 16_383), (2.0, 8192)]).into())
            .unwrap();
        assert_eq!(
            points(&s),
            [
                (bend, 12_000, 16_383),
                (modwheel, 24_000, 64),
                (bend, 48_000, 8192)
            ]
        );
        // Tempo changes move them with the notes.
        s.execute(SetTempo::new(60.0).into()).unwrap();
        assert_eq!(points(&s)[1], (modwheel, 48_000, 64));
        s.execute(set(vec![]).into()).unwrap();
        assert_eq!(points(&s), [(modwheel, 48_000, 64)]);
        // 60 BPM: a day of beats is 86 400, so 100 000 is past the end.
        for bad in [
            set(vec![(0.0, 16_384)]),
            set(vec![(-1.0, 0)]),
            set(vec![(100_000.0, 0)]),
        ] {
            assert!(s.execute(bad.into()).is_err());
        }
        assert!(
            s.execute(
                SetMidiControls {
                    clip,
                    controller: Controller::Cc { number: 120 },
                    points: vec![ControlValue {
                        time_beats: 0.0,
                        value: 0
                    }],
                }
                .into()
            )
            .is_err()
        );
        assert!(
            s.execute(
                SetMidiControls {
                    clip,
                    controller: Controller::PolyPressure { pitch: 128 },
                    points: vec![ControlValue {
                        time_beats: 0.0,
                        value: 0
                    }],
                }
                .into()
            )
            .is_err()
        );
        for (controller, value) in [
            (bend, 10_000),
            (Controller::PolyPressure { pitch: 61 }, 99),
            (Controller::Program, 42),
            (Controller::ChannelPressure, 7),
        ] {
            assert_eq!(
                Controller::from_message(controller.message(3, value)),
                Some((controller, value))
            );
        }
        assert_eq!(Controller::Program.message(1, 42), [0xc1, 42, 0]);
        assert!(!Controller::Program.resets());
    }

    #[test]
    fn midi_devices_per_track() {
        use crate::command::{AddMidiClip, SetTrackMidiInput, SetTrackMidiOutput};
        use crate::model::MidiOutput;

        let mut s = Session::default();
        let audio = add(&mut s, "A");
        let midi = s
            .execute(AddMidiClip::default().into())
            .unwrap()
            .track
            .unwrap();
        s.execute(
            SetTrackMidiInput {
                track: midi,
                device: Some("Keys:Keys MIDI 1".into()),
                channel: Some(2),
            }
            .into(),
        )
        .unwrap();
        s.execute(
            SetTrackMidiOutput {
                track: midi,
                device: Some("Synth:Synth MIDI 1".into()),
                channel: Some(10),
                latency_ms: None,
            }
            .into(),
        )
        .unwrap();
        let t = s.project().track(midi).unwrap();
        assert_eq!(t.midi_input.as_deref(), Some("Keys:Keys MIDI 1"));
        assert_eq!(t.midi_input_channel, Some(2));
        assert_eq!(
            t.midi_output,
            Some(MidiOutput {
                device: "Synth:Synth MIDI 1".into(),
                channel: 10,
                latency_ms: 0.0,
            })
        );
        // A device's latency stays while the track keeps the device.
        let output = |s: &Session| s.project().track(midi).unwrap().midi_output.clone();
        let mut set = SetTrackMidiOutput::new(midi, Some("Synth:Synth MIDI 1".into()));
        set.latency_ms = Some(12.5);
        s.execute(set.into()).unwrap();
        assert_eq!(output(&s).unwrap().latency_ms, 12.5);
        let same = SetTrackMidiOutput {
            channel: Some(3),
            ..SetTrackMidiOutput::new(midi, Some("Synth:Synth MIDI 1".into()))
        };
        s.execute(same.into()).unwrap();
        assert_eq!(output(&s).unwrap().latency_ms, 12.5);
        let other = SetTrackMidiOutput::new(midi, Some("Other".into()));
        s.execute(other.into()).unwrap();
        assert_eq!(output(&s).unwrap().latency_ms, 0.0);
        for _ in 0..4 {
            s.undo().unwrap();
        }
        assert_eq!(s.project().track(midi).unwrap().midi_output, None);
        let revision = s.revision();
        let mut too_slow = SetTrackMidiOutput::new(midi, Some("x".into()));
        too_slow.latency_ms = Some(1000.5);
        let mut no_device = SetTrackMidiOutput::new(midi, None);
        no_device.latency_ms = Some(5.0);
        let bad: [Command; 7] = [
            too_slow.into(),
            no_device.into(),
            SetTrackMidiInput {
                track: audio,
                device: None,
                channel: None,
            }
            .into(),
            SetTrackMidiInput {
                track: midi,
                device: Some(" ".into()),
                channel: None,
            }
            .into(),
            SetTrackMidiInput {
                track: midi,
                device: None,
                channel: Some(0),
            }
            .into(),
            SetTrackMidiOutput {
                track: midi,
                device: Some("x".into()),
                channel: Some(17),
                latency_ms: None,
            }
            .into(),
            SetTrackMidiOutput {
                track: midi,
                device: None,
                channel: Some(2),
                latency_ms: None,
            }
            .into(),
        ];
        for command in bad {
            assert!(s.execute(command).is_err());
        }
        assert_eq!(s.revision(), revision);
    }

    #[test]
    fn midi_validation_and_kinds() {
        use crate::command::{AddMidiClip, AddMidiNotes, SetClipLength, SetSynth, SetTrackArm};

        let mut s = Session::default();
        let audio = add(&mut s, "A");
        let src = s.add_source("x", "/tmp/x.wav", 1, 1000);
        let audio_clip = place(&mut s, src, Some(audio), 0.0).clip.unwrap();
        let midi = s.execute(AddMidiClip::default().into()).unwrap();
        let (midi_track, midi_clip) = (midi.track.unwrap(), midi.clip.unwrap());
        let revision = s.revision();
        let bad: Vec<Command> = vec![
            AddMidiClip {
                track: Some(audio),
                ..Default::default()
            }
            .into(),
            AddMidiClip {
                notes: vec![note(128, 0.0, 1.0)],
                ..Default::default()
            }
            .into(),
            AddMidiClip {
                notes: vec![note(60, 0.0, 0.0)],
                ..Default::default()
            }
            .into(),
            AddMidiClip {
                length_s: Some(0.5),
                notes: vec![note(60, 1.0, 1.0)],
                ..Default::default()
            }
            .into(),
            AddMidiNotes {
                clip: audio_clip,
                notes: vec![note(60, 0.0, 1.0)],
            }
            .into(),
            AddMidiNotes {
                clip: midi_clip,
                notes: vec![crate::command::NoteSpec {
                    velocity: 0,
                    ..note(60, 0.0, 1.0)
                }],
            }
            .into(),
            AddClip::new(src, Some(midi_track), 0.0).into(),
            MoveClip {
                clip: audio_clip,
                position_s: None,
                track: Some(midi_track),
            }
            .into(),
            SetClipLength {
                clip: audio_clip,
                length_s: 1.0,
            }
            .into(),
            SetSynth {
                track: audio,
                ..SetSynth::new(midi_track)
            }
            .into(),
            SetSynth::new(midi_track).into(),
            SetSynth {
                cutoff_hz: Some(5.0),
                ..SetSynth::new(midi_track)
            }
            .into(),
        ];
        for cmd in bad {
            let name = cmd.name();
            assert!(s.execute(cmd).is_err(), "{name} should fail");
        }
        assert_eq!(s.revision(), revision);

        s.execute(
            SetSynth {
                waveform: Some(crate::model::Waveform::Square),
                release_ms: Some(50.0),
                ..SetSynth::new(midi_track)
            }
            .into(),
        )
        .unwrap();
        let synth = s.project().track(midi_track).unwrap().synth.unwrap();
        assert_eq!(synth.waveform, crate::model::Waveform::Square);
        assert_eq!(synth.release_ms, 50.0);
        s.execute(
            SetTrackArm {
                track: midi_track,
                armed: true,
            }
            .into(),
        )
        .unwrap();
        assert!(s.project().track(midi_track).unwrap().armed);
    }

    #[test]
    fn plugins_and_automation() {
        use crate::command::{
            AddAutomationPoint, AddPlugin, MovePlugin, PointSpec, RemoveAutomationPoints,
            RemovePlugin, SetAutomation, SetPluginBypass, SetPluginParam,
        };
        use crate::model::AutomationTarget;

        let mut s = Session::default();
        let track = add(&mut s, "Gtr");
        let add_plugin = |s: &mut Session, plugin: &str, index| {
            s.execute(
                AddPlugin {
                    track,
                    plugin: plugin.into(),
                    index,
                }
                .into(),
            )
            .map(|o| o.plugin.unwrap())
        };
        let eq = add_plugin(&mut s, "lv2:urn:eq", None).unwrap();
        let comp = add_plugin(&mut s, "clap:comp", Some(0)).unwrap();
        assert!(add_plugin(&mut s, "vst:nope", None).is_err());
        let order = |s: &Session| -> Vec<_> {
            s.project().tracks[0].plugins.iter().map(|p| p.id).collect()
        };
        assert_eq!(order(&s), [comp, eq]);
        s.execute(
            MovePlugin {
                plugin: comp,
                index: 9,
            }
            .into(),
        )
        .unwrap();
        assert_eq!(order(&s), [eq, comp]);

        s.execute(SetPluginParam::new(eq, "gain", -3.0).into())
            .unwrap();
        s.execute(
            SetPluginBypass {
                plugin: comp,
                bypass: true,
            }
            .into(),
        )
        .unwrap();
        let t = &s.project().tracks[0];
        assert_eq!(t.plugin(eq).unwrap().params["gain"], -3.0);
        assert!(t.plugin(comp).unwrap().bypass);

        let param = AutomationTarget::Param {
            plugin: eq,
            param: "gain".into(),
        };
        s.execute(
            SetAutomation {
                track,
                target: param.clone(),
                points: vec![
                    PointSpec {
                        time_s: 1.0,
                        value: 0.0,
                    },
                    PointSpec {
                        time_s: 0.0,
                        value: -12.0,
                    },
                    PointSpec {
                        time_s: 1.0,
                        value: 6.0,
                    },
                ],
            }
            .into(),
        )
        .unwrap();
        let lane = s.project().tracks[0].lane(&param).unwrap().clone();
        let points: Vec<_> = lane.points.iter().map(|p| (p.frame, p.value)).collect();
        assert_eq!(
            points,
            [(0, -12.0), (48_000, 6.0)],
            "sorted; the later duplicate wins"
        );
        assert_eq!(lane.value_at(24_000), Some(-3.0));
        assert_eq!(lane.value_at(96_000), Some(6.0));

        s.execute(
            AddAutomationPoint {
                track,
                target: AutomationTarget::Gain,
                time_s: 2.0,
                value: -6.0,
            }
            .into(),
        )
        .unwrap();
        for bad in [
            Command::from(AddAutomationPoint {
                track,
                target: AutomationTarget::Pan,
                time_s: 0.0,
                value: 2.0,
            }),
            AddAutomationPoint {
                track,
                target: AutomationTarget::Param {
                    plugin: PluginId(99),
                    param: "x".into(),
                },
                time_s: 0.0,
                value: 0.0,
            }
            .into(),
        ] {
            assert!(s.execute(bad).is_err());
        }
        s.execute(
            RemoveAutomationPoints {
                track,
                target: AutomationTarget::Gain,
                from_s: 0.0,
                to_s: None,
            }
            .into(),
        )
        .unwrap();
        assert!(
            s.project().tracks[0]
                .lane(&AutomationTarget::Gain)
                .is_none()
        );

        // Removing a plugin removes its lanes; undo brings both back.
        s.execute(RemovePlugin { plugin: eq }.into()).unwrap();
        assert_eq!(order(&s), [comp]);
        assert!(s.project().tracks[0].automation.is_empty());
        s.undo().unwrap();
        assert_eq!(order(&s), [eq, comp]);
        assert!(s.project().tracks[0].lane(&param).is_some());
    }

    #[test]
    fn time_signature_changes() {
        use crate::command::NoteSpec;
        use crate::command::{
            AddMidiClip, AddMidiNotes, RemoveTimeSignatureChange, SetTimeSignature,
            SetTimeSignatureMap,
        };
        use crate::tempo::MeterChange;

        let mut s = Session::default();
        let seven = SetTimeSignature {
            at_bar: Some(3),
            ..SetTimeSignature::new(7, 8)
        };
        s.execute(seven.into()).unwrap();
        s.execute(SetTimeSignature::new(3, 4).into()).unwrap();
        let p = s.project();
        assert_eq!(p.time_signature.numerator, 3);
        assert_eq!(
            p.meter_changes,
            [MeterChange {
                bar: 3,
                numerator: 7,
                denominator: 8
            }]
        );
        // 3/4 for bars 1 and 2, then 7/8.
        let meter = p.meter_map();
        assert_eq!(meter.bar_start(2), 6.0);
        assert_eq!(meter.bar_start(3), 9.5);

        // A note past a clip that starts on a bar line stretches it to the
        // next bar line of the map: a clip at bar 2 (beat 3) with a note
        // ending at beat 5 ends with bar 3, at beat 9.5.
        let per_beat = 48_000.0 / 2.0;
        let outcome = s
            .execute(
                AddMidiClip {
                    position_s: 3.0 / 2.0,
                    length_s: Some(0.5),
                    ..Default::default()
                }
                .into(),
            )
            .unwrap();
        let clip = outcome.clip.unwrap();
        s.execute(
            AddMidiNotes {
                clip,
                notes: vec![NoteSpec {
                    pitch: 60,
                    start_beats: 0.0,
                    length_beats: 5.0,
                    velocity: 100,
                }],
            }
            .into(),
        )
        .unwrap();
        let (_, c) = s.project().clip(clip).unwrap();
        assert_eq!(c.length as f64, (9.5 - 3.0) * per_beat);

        s.execute(RemoveTimeSignatureChange { at_bar: 3 }.into())
            .unwrap();
        assert!(s.project().meter_changes.is_empty());
        assert!(
            s.execute(RemoveTimeSignatureChange { at_bar: 3 }.into())
                .is_err()
        );
        s.undo().unwrap();
        assert_eq!(s.project().meter_changes.len(), 1);

        s.execute(
            SetTimeSignatureMap {
                numerator: 4,
                denominator: 4,
                changes: vec![
                    MeterChange {
                        bar: 9,
                        numerator: 5,
                        denominator: 4,
                    },
                    MeterChange {
                        bar: 5,
                        numerator: 6,
                        denominator: 8,
                    },
                ],
            }
            .into(),
        )
        .unwrap();
        let bars: Vec<u32> = s.project().meter_changes.iter().map(|c| c.bar).collect();
        assert_eq!(bars, [5, 9]);
        for bad in [
            SetTimeSignature {
                at_bar: Some(0),
                ..SetTimeSignature::new(4, 4)
            },
            SetTimeSignature {
                at_bar: Some(4),
                ..SetTimeSignature::new(4, 3)
            },
        ] {
            assert!(s.execute(bad.into()).is_err());
        }
    }

    #[test]
    fn tempo_ramps_move_midi() {
        use crate::command::AddMidiClip;
        use crate::tempo::TempoChange;

        let mut s = Session::default();
        // A clip at beat 8 (4 s at 120 BPM).
        let clip = s
            .execute(
                AddMidiClip {
                    position_s: 4.0,
                    ..Default::default()
                }
                .into(),
            )
            .unwrap()
            .clip
            .unwrap();
        s.execute(SetTempo::new(60.0).into()).unwrap();
        // At 60 BPM beat 8 is at 8 s.
        assert_eq!(s.project().clip(clip).unwrap().1.start, 384_000);
        // Gliding from 60 to 180 BPM over those 8 beats takes 4 s.
        s.execute(
            SetTempo {
                bpm: 180.0,
                at_beats: Some(8.0),
                ramp: true,
            }
            .into(),
        )
        .unwrap();
        assert_eq!(s.project().clip(clip).unwrap().1.start, 192_000);
        assert_eq!(
            s.project().tempo_changes,
            [TempoChange {
                beat: 8.0,
                bpm: 180.0,
                ramp: true
            }]
        );
        s.undo().unwrap();
        assert_eq!(s.project().clip(clip).unwrap().1.start, 384_000);
    }

    #[test]
    fn buses_and_the_master() {
        use crate::command::{AddPlugin, SetTrackArm, SetTrackOutput, SetTrackSolo};
        use crate::model::MASTER;
        let mut s = Session::default();
        let track = add(&mut s, "Kick");
        let bus = |s: &mut Session| {
            s.execute(
                AddTrack {
                    kind: TrackKind::Bus,
                    ..Default::default()
                }
                .into(),
            )
            .unwrap()
            .track
            .unwrap()
        };
        let (drums, all) = (bus(&mut s), bus(&mut s));
        assert_eq!(s.project().track(drums).unwrap().name, "Bus 1");
        let send = |s: &mut Session, track, bus| s.execute(SetTrackOutput { track, bus }.into());
        send(&mut s, track, Some(drums)).unwrap();
        send(&mut s, drums, Some(all)).unwrap();
        assert!(s.project().feeds(track, all));
        assert_eq!(s.project().destination(all), Some(MASTER));
        // No loops, and only buses take tracks.
        let err = send(&mut s, all, Some(drums)).unwrap_err().to_string();
        assert!(err.contains("loop"), "{err}");
        assert!(send(&mut s, drums, Some(drums)).is_err());
        assert!(send(&mut s, drums, Some(track)).is_err());
        // Back to the master: no bus, or 0.
        send(&mut s, drums, Some(MASTER)).unwrap();
        assert_eq!(s.project().track(drums).unwrap().output, None);
        s.undo().unwrap();

        // Buses have no clips and don't record; the master stays.
        assert!(
            s.execute(
                SetTrackArm {
                    track: drums,
                    armed: true
                }
                .into()
            )
            .is_err()
        );
        for command in [
            RemoveTrack { track: MASTER }.into(),
            RenameTrack {
                track: MASTER,
                name: "M".into(),
            }
            .into(),
            SetTrackSolo {
                track: MASTER,
                solo: true,
            }
            .into(),
            SetTrackOutput {
                track: MASTER,
                bus: Some(drums),
            }
            .into(),
        ] {
            let err = s.execute(command).unwrap_err().to_string();
            assert!(err.contains("master"), "{err}");
        }
        // The master has plugins and a fader like any bus.
        s.execute(
            AddPlugin {
                track: MASTER,
                plugin: "clap:x".into(),
                index: None,
            }
            .into(),
        )
        .unwrap();
        s.execute(
            SetTrackGain {
                track: MASTER,
                gain_db: -3.0,
            }
            .into(),
        )
        .unwrap();
        assert_eq!(s.project().master.plugins.len(), 1);
        assert_eq!(s.project().master.gain_db, -3.0);

        // Removing a bus sends what went there to the master; undo puts
        // it back.
        s.execute(RemoveTrack { track: drums }.into()).unwrap();
        assert_eq!(s.project().track(track).unwrap().output, None);
        s.undo().unwrap();
        assert_eq!(s.project().track(track).unwrap().output, Some(drums));

        // Saved and loaded with the rest.
        let dir = tempfile::tempdir().unwrap();
        let path = s.save(Some(&dir.path().join("Bus"))).unwrap();
        let loaded = Session::open(&path).unwrap();
        assert_eq!(loaded.project().master.gain_db, -3.0);
        assert_eq!(loaded.project().track(drums).unwrap().output, Some(all));
    }

    #[test]
    fn writing_automation_replaces_a_stretch() {
        use crate::command::{PointSpec, SetAutomation, WriteAutomation};
        use crate::model::AutomationTarget;
        let mut s = Session::default();
        let track = add(&mut s, "Vox");
        let point = |time_s, value| PointSpec { time_s, value };
        s.execute(
            SetAutomation {
                track,
                target: AutomationTarget::Gain,
                points: vec![
                    point(0.0, 0.0),
                    point(1.0, -6.0),
                    point(2.0, 0.0),
                    point(3.0, -3.0),
                ],
            }
            .into(),
        )
        .unwrap();
        let write = |s: &mut Session, from_s, to_s, points| {
            s.execute(
                WriteAutomation {
                    track,
                    target: AutomationTarget::Gain,
                    from_s,
                    to_s,
                    points,
                }
                .into(),
            )
        };
        write(&mut s, 0.5, 2.0, vec![point(0.5, -12.0), point(1.5, -10.0)]).unwrap();
        let values: Vec<(f64, f64)> = s.project().tracks[0].automation[0]
            .points
            .iter()
            .map(|p| (s.project().frames_to_seconds(p.frame), p.value))
            .collect();
        assert_eq!(
            values,
            [(0.0, 0.0), (0.5, -12.0), (1.5, -10.0), (3.0, -3.0)]
        );
        // Points outside the stretch are refused.
        let err = write(&mut s, 0.5, 1.0, vec![point(2.0, 0.0)]).unwrap_err();
        assert!(err.to_string().contains("between"), "{err}");
        // A new lane if there's none.
        s.execute(
            WriteAutomation {
                track,
                target: AutomationTarget::Pan,
                from_s: 1.0,
                to_s: 1.0,
                points: vec![point(1.0, 0.5)],
            }
            .into(),
        )
        .unwrap();
        assert_eq!(s.project().tracks[0].automation.len(), 2);
    }

    #[test]
    fn autosaves_recover_unsaved_changes() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Song.dawkey");
        let mut s = Session::default();
        s.execute(AddTrack::default().into()).unwrap();
        s.save(Some(&bundle)).unwrap();
        s.execute(
            RenameTrack {
                track: crate::model::TrackId(1),
                name: "Lead".into(),
            }
            .into(),
        )
        .unwrap();
        let autosave = bundle.join("backups/autosave.json");
        s.write_autosave(&autosave).unwrap();
        // Writing it saved nothing.
        assert!(s.is_dirty());
        assert_eq!(
            Session::open(&bundle).unwrap().project().tracks[0].name,
            "Audio 1"
        );

        let recovered = Session::recover(&autosave, Some(bundle.clone())).unwrap();
        assert_eq!(recovered.project(), s.project());
        assert_eq!(recovered.path(), Some(bundle.as_path()));
        assert!(recovered.is_dirty());
    }
}
