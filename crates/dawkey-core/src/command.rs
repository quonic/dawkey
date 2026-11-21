// SPDX-License-Identifier: AGPL-3.0-or-later
//! User-level commands. Each command is its own argument struct, so its JSON
//! Schema can be published as an MCP tool, and its doc comment becomes the
//! tool description.

use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, generate::SchemaSettings};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::edit::{ClipProp, Edit, TrackProp};
use crate::error::CoreError;
use crate::model::{
    ArchivedTrack, AutomationLane, AutomationPoint, AutomationTarget, CC_MAX, Clip, ClipContent,
    ClipId, ControlPoint, Controller, DEVICE_LATENCY_MS_MAX, DeletedTrackArchive,
    DetachedTrackRoutes, DrumStep, GAIN_DB_MAX, GAIN_DB_MIN, MASTER, MIDI_MAX, MidiOutput,
    MixdownArchive, Note, NoteId, PluginId, PluginSlot, Project, SavedDrumPattern, SavedRackPreset,
    SavedRackUnit, Send, Source, SourceId, SynthParams, TEMPO_BPM_MAX, TEMPO_BPM_MIN,
    TimeSignature, Track, TrackId, TrackKind, Waveform,
};
use crate::model::{InputChannels, Region, TimeRange};
use crate::session::Outcome;
use crate::tempo::{MeterChange, MeterMap, TempoChange, TempoMap};

const MAX_NAME_LEN: usize = 256;
/// Latest timeline position accepted, in seconds (24 hours).
const MAX_POSITION_S: f64 = 86_400.0;
/// Most notes one MIDI clip may hold.
const MAX_NOTES: usize = 100_000;
/// Most tempo changes a project may have.
const MAX_TEMPO_CHANGES: usize = 10_000;

/// How a command affects the project, for MCP tool annotations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Creates something new. Repeating it creates more.
    Additive,
    /// Sets a value. Repeating it with the same arguments changes nothing.
    Setter,
    /// Deletes something (still undoable).
    Destructive,
}

/// Static description of a command, used to publish it over MCP.
#[derive(Debug)]
pub struct CommandSpec {
    /// Stable snake_case name; also the MCP tool name.
    pub name: &'static str,
    pub effect: Effect,
    /// JSON Schema of the command's arguments (an object schema).
    pub schema: fn() -> Schema,
    /// Builds the command from JSON arguments.
    pub parse: fn(serde_json::Value) -> Result<Command, serde_json::Error>,
}

impl CommandSpec {
    pub fn find(name: &str) -> Option<&'static CommandSpec> {
        COMMAND_SPECS.iter().find(|spec| spec.name == name)
    }
}

/// JSON Schema for a command or tool argument type, with every subschema
/// inlined (some MCP clients don't resolve `$ref`).
pub fn args_schema<T: JsonSchema>() -> Schema {
    SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<T>()
}

fn parse_as<T: DeserializeOwned + Into<Command>>(
    args: serde_json::Value,
) -> Result<Command, serde_json::Error> {
    serde_json::from_value::<T>(args).map(Into::into)
}

macro_rules! commands {
    ($($variant:ident => $name:literal, $effect:ident;)*) => {
        /// A change to the project. Every frontend goes through this type.
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(tag = "command", content = "args", rename_all = "snake_case")]
        pub enum Command {
            $($variant($variant),)*
        }

        impl Command {
            pub fn name(&self) -> &'static str {
                match self {
                    $(Command::$variant(_) => $name,)*
                }
            }

            /// Validates the command against `project` and lowers it to edits.
            /// May allocate IDs, which are not returned to the pool if the
            /// edits are later rolled back.
            pub(crate) fn into_edits(
                self,
                project: &mut Project,
            ) -> Result<(Vec<Edit>, Outcome), CoreError> {
                match self {
                    $(Command::$variant(c) => c.lower(project),)*
                }
            }
        }

        $(
            impl From<$variant> for Command {
                fn from(c: $variant) -> Self {
                    Command::$variant(c)
                }
            }
        )*

        /// Every command, in a stable order.
        pub static COMMAND_SPECS: &[CommandSpec] = &[
            $(CommandSpec {
                name: $name,
                effect: Effect::$effect,
                schema: args_schema::<$variant>,
                parse: parse_as::<$variant>,
            },)*
        ];
    };
}

commands! {
    AddTrack => "add_track", Additive;
    DuplicateTrack => "duplicate_track", Additive;
    AddGuitarTrack => "add_guitar_track", Additive;
    RemoveTrack => "remove_track", Destructive;
    DeleteTracks => "delete_tracks", Destructive;
    RestoreDeletedTracks => "restore_deleted_tracks", Additive;
    ArchiveTracksForMixdown => "archive_tracks_for_mixdown", Destructive;
    RestoreMixdownTracks => "restore_mixdown_tracks", Additive;
    RenameTrack => "rename_track", Setter;
    SetTrackColor => "set_track_color", Setter;
    SetTrackGain => "set_track_gain", Setter;
    SetTrackPan => "set_track_pan", Setter;
    SetTrackMute => "set_track_mute", Setter;
    SetTrackSolo => "set_track_solo", Setter;
    SetTrackOutput => "set_track_output", Setter;
    SetTrackSend => "set_track_send", Setter;
    RemoveTrackSend => "remove_track_send", Destructive;
    SetTempo => "set_tempo", Setter;
    RemoveTempoChange => "remove_tempo_change", Destructive;
    SetTempoMap => "set_tempo_map", Setter;
    SetTimeSignature => "set_time_signature", Setter;
    RemoveTimeSignatureChange => "remove_time_signature_change", Destructive;
    SetTimeSignatureMap => "set_time_signature_map", Setter;
    RenameProject => "rename_project", Setter;
    AddClip => "add_clip", Additive;
    MoveClip => "move_clip", Setter;
    RemoveClip => "remove_clip", Destructive;
    SetClipGain => "set_clip_gain", Setter;
    SetClipMute => "set_clip_mute", Setter;
    RenameClip => "rename_clip", Setter;
    SetTrackArm => "set_track_arm", Setter;
    SetTrackInput => "set_track_input", Setter;
    SetTrackMonitor => "set_track_monitor", Setter;
    SetTrackMidiInput => "set_track_midi_input", Setter;
    SetTrackMidiOutput => "set_track_midi_output", Setter;
    SetLoop => "set_loop", Setter;
    SetPunch => "set_punch", Setter;
    SetClipLength => "set_clip_length", Setter;
    TrimClip => "trim_clip", Setter;
    SplitClip => "split_clip", Additive;
    DuplicateClip => "duplicate_clip", Additive;
    RepeatClip => "repeat_clip", Additive;
    CompTake => "comp_take", Setter;
    AddMidiClip => "add_midi_clip", Additive;
    AddMidiNotes => "add_midi_notes", Additive;
    EditMidiNotes => "edit_midi_notes", Setter;
    RemoveMidiNotes => "remove_midi_notes", Destructive;
    QuantizeMidiNotes => "quantize_midi_notes", Setter;
    SetMidiControls => "set_midi_controls", Setter;
    SetSynth => "set_synth", Setter;
    AddPlugin => "add_plugin", Additive;
    RemovePlugin => "remove_plugin", Destructive;
    MovePlugin => "move_plugin", Setter;
    SetPluginParam => "set_plugin_param", Setter;
    SetPluginBypass => "set_plugin_bypass", Setter;
    SetPluginSidechain => "set_plugin_sidechain", Setter;
    SetPluginOutput => "set_plugin_output", Setter;
    LoadRackPreset => "load_rack_preset", Setter;
    SaveRackPreset => "save_rack_preset", Setter;
    RemoveSavedRackPreset => "remove_saved_rack_preset", Destructive;
    LoadSavedRackPreset => "load_saved_rack_preset", Setter;
    AddDrumTrack => "add_drum_track", Additive;
    ApplyDrumKit => "apply_drum_kit", Setter;
    ApplyDrumPattern => "apply_drum_pattern", Setter;
    SaveDrumPattern => "save_drum_pattern", Setter;
    RemoveSavedDrumPattern => "remove_saved_drum_pattern", Destructive;
    LoadSavedDrumPattern => "load_saved_drum_pattern", Setter;
    SetAutomation => "set_automation", Setter;
    AddAutomationPoint => "add_automation_point", Setter;
    RemoveAutomationPoints => "remove_automation_points", Destructive;
    WriteAutomation => "write_automation", Setter;
}

/// Most plugins on one track.
const MAX_PLUGINS: usize = 64;
/// Most points in one automation lane.
const MAX_POINTS: usize = 100_000;

/// Add a new track. Returns the new track's ID. MIDI tracks play their
/// clips through a built-in synth (see set_synth). A bus has no clips: it
/// mixes the tracks sent to it (set_track_output) through its own plugins
/// and fader, on the way to the master.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddTrack {
    /// Track name. Defaults to "Audio N", "MIDI N" or "Bus N".
    #[serde(default)]
    pub name: Option<String>,
    /// Kind of track: audio, midi, or bus (no clips; it mixes the tracks
    /// sent to it). Defaults to audio.
    #[serde(default)]
    pub kind: TrackKind,
    /// Position in the track list (0 = top). Defaults to the end.
    #[serde(default)]
    pub index: Option<usize>,
}

impl AddTrack {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let name = match self.name {
            Some(name) => validate_name(&name)?,
            None => default_track_name(project, self.kind),
        };
        let id = project.alloc_track_id();
        let index = self.index.unwrap_or(project.tracks.len());
        let track = Track::new(id, name, self.kind);
        Ok((vec![Edit::InsertTrack { index, track }], Outcome::track(id)))
    }
}

/// Copy a track — its clips, plugins and automation, fader/pan/mute/solo,
/// routing, and synth or MIDI device settings — to a new track right after
/// it (or at `index`). Not armed, even if the source is. The master can't
/// be duplicated. Returns the new track's ID; its plugin slots come back in
/// `plugins`, in the same order as the source track's own, so a plugin's
/// saved state (set_plugin_property and the like, which this command
/// doesn't copy) can be carried over to the matching new slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DuplicateTrack {
    pub track: TrackId,
    /// Position in the track list (0 = top). Defaults to right after the
    /// source track.
    #[serde(default)]
    pub index: Option<usize>,
}

impl DuplicateTrack {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_not_master(project, self.track, "duplicated")?;
        let source = project
            .track(self.track)
            .ok_or(CoreError::TrackNotFound(self.track))?
            .clone();
        let index = match self.index {
            Some(index) => index,
            None => project.track_index(self.track).expect("checked above") + 1,
        };
        let id = project.alloc_track_id();
        let name = duplicate_track_name(project, &source.name);

        let clips = source
            .clips
            .iter()
            .map(|clip| {
                let mut copy = Clip {
                    id: project.alloc_clip_id(),
                    ..clip.clone()
                };
                if let ClipContent::Midi { notes, .. } = &mut copy.content {
                    for note in notes {
                        note.id = project.alloc_note_id();
                    }
                }
                copy
            })
            .collect();

        let plugins: Vec<PluginSlot> = source
            .plugins
            .iter()
            .map(|slot| PluginSlot {
                id: project.alloc_plugin_id(),
                ..slot.clone()
            })
            .collect();
        let plugin_ids: Vec<PluginId> = plugins.iter().map(|p| p.id).collect();
        let old_to_new: BTreeMap<PluginId, PluginId> = source
            .plugins
            .iter()
            .map(|slot| slot.id)
            .zip(plugin_ids.iter().copied())
            .collect();
        let automation = source
            .automation
            .iter()
            .map(|lane| {
                let target = match &lane.target {
                    AutomationTarget::Param { plugin, param } => AutomationTarget::Param {
                        plugin: old_to_new.get(plugin).copied().unwrap_or(*plugin),
                        param: param.clone(),
                    },
                    other => other.clone(),
                };
                AutomationLane {
                    target,
                    points: lane.points.clone(),
                }
            })
            .collect();

        let track = Track {
            id,
            name,
            armed: false,
            clips,
            plugins,
            automation,
            ..source
        };
        Ok((
            vec![Edit::InsertTrack { index, track }],
            Outcome {
                track: Some(id),
                plugins: plugin_ids,
                ..Outcome::default()
            },
        ))
    }
}

fn duplicate_track_name(project: &Project, base: &str) -> String {
    let once = format!("{base} copy");
    if project.tracks.iter().all(|t| t.name != once) {
        return once;
    }
    (2..)
        .map(|n| format!("{base} copy {n}"))
        .find(|name| project.tracks.iter().all(|t| &t.name != name))
        .expect("unbounded range always yields a free name")
}

/// Adds an audio track already set up to monitor a single instrument live:
/// armed, monitoring, and listening to one channel (the left input by
/// default) rather than both. That's the usual wiring for a guitar, bass
/// or other mono source plugged into one input, so you hear it the moment
/// you plug in, without a silent "other half" from an unconnected second
/// channel the way a plain stereo-input track would have. Returns the new
/// track's ID. Use add_track for a normal (stereo) audio track.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddGuitarTrack {
    /// Track name. Defaults to "Guitar N".
    #[serde(default)]
    pub name: Option<String>,
    /// Which input channel to listen to. Defaults to the left (first)
    /// input.
    #[serde(default)]
    pub input: Option<InputChannels>,
}

impl AddGuitarTrack {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let name = match self.name {
            Some(name) => validate_name(&name)?,
            None => (1..)
                .map(|n| format!("Guitar {n}"))
                .find(|name| project.tracks.iter().all(|t| &t.name != name))
                .expect("unbounded range always yields a free name"),
        };
        let input = self.input.unwrap_or(InputChannels::Left);
        let id = project.alloc_track_id();
        let track = Track::new(id, name, TrackKind::Audio);
        Ok((
            vec![
                Edit::InsertTrack {
                    index: project.tracks.len(),
                    track,
                },
                Edit::SetTrack {
                    id,
                    prop: TrackProp::Armed(true),
                },
                Edit::SetTrack {
                    id,
                    prop: TrackProp::Input(input),
                },
                Edit::SetTrack {
                    id,
                    prop: TrackProp::Monitor(true),
                },
            ],
            Outcome::track(id),
        ))
    }
}

/// Remove a track (or bus) and everything on it. Tracks sent to a removed
/// bus go to the master, and their sends to it (with the sends' automation)
/// go.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveTrack {
    pub track: TrackId,
}

impl RemoveTrack {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_not_master(project, self.track, "removed")?;
        let gone = self.track;
        let mut edits: Vec<Edit> = project
            .tracks
            .iter()
            .filter(|t| t.output == Some(gone))
            .map(|t| Edit::SetTrack {
                id: t.id,
                prop: TrackProp::Output(None),
            })
            .collect();
        // Sidechains from it, plugin outputs to it, and sends to it (and
        // their automation) stop.
        for track in project.all_tracks().filter(|t| t.id != gone) {
            if track.sends.iter().any(|s| s.bus == gone) {
                edits.push(Edit::SetTrack {
                    id: track.id,
                    prop: TrackProp::Sends(
                        track
                            .sends
                            .iter()
                            .copied()
                            .filter(|s| s.bus != gone)
                            .collect(),
                    ),
                });
            }
            let send_lane = |l: &AutomationLane| l.target == AutomationTarget::Send { bus: gone };
            if track.automation.iter().any(send_lane) {
                edits.push(Edit::SetTrack {
                    id: track.id,
                    prop: TrackProp::Automation(
                        track
                            .automation
                            .iter()
                            .filter(|l| !send_lane(l))
                            .cloned()
                            .collect(),
                    ),
                });
            }
            let routed = |p: &PluginSlot| {
                p.sidechain == Some(gone) || p.outputs.values().any(|&b| b == gone)
            };
            if track.plugins.iter().any(routed) {
                let mut plugins = track.plugins.clone();
                for p in &mut plugins {
                    p.sidechain = p.sidechain.filter(|&s| s != gone);
                    p.outputs.retain(|_, b| *b != gone);
                }
                edits.push(Edit::SetTrack {
                    id: track.id,
                    prop: TrackProp::Plugins(plugins),
                });
            }
        }
        edits.push(Edit::RemoveTrack { id: gone });
        if project.mixdown_archives.iter().any(|a| a.mix_track == gone) {
            let mut archives = project.mixdown_archives.clone();
            archives.retain(|a| a.mix_track != gone);
            edits.push(Edit::SetMixdownArchives(archives));
        }
        Ok((edits, Outcome::none()))
    }
}

fn default_true() -> bool {
    true
}

fn archive_tracks_edits(
    project: &Project,
    unique_ids: &[TrackId],
) -> (Vec<ArchivedTrack>, Vec<DetachedTrackRoutes>, Vec<Edit>) {
    let mut archived: Vec<ArchivedTrack> = unique_ids
        .iter()
        .filter_map(|&id| {
            project.track_index(id).map(|index| ArchivedTrack {
                index,
                track: project.tracks[index].clone(),
            })
        })
        .collect();
    archived.sort_by_key(|a| a.index);

    let mut edits = Vec::new();
    let mut detached_routes = Vec::new();
    for track in project.all_tracks().filter(|t| !unique_ids.contains(&t.id)) {
        let detached_output = track.output.filter(|o| unique_ids.contains(o));
        if detached_output.is_some() {
            edits.push(Edit::SetTrack {
                id: track.id,
                prop: TrackProp::Output(None),
            });
        }
        let detached_sends: Vec<Send> = track
            .sends
            .iter()
            .copied()
            .filter(|s| unique_ids.contains(&s.bus))
            .collect();
        if !detached_sends.is_empty() {
            edits.push(Edit::SetTrack {
                id: track.id,
                prop: TrackProp::Sends(
                    track
                        .sends
                        .iter()
                        .copied()
                        .filter(|s| !unique_ids.contains(&s.bus))
                        .collect(),
                ),
            });
        }
        let is_detached_send_lane = |l: &AutomationLane| matches!(l.target, AutomationTarget::Send { bus } if unique_ids.contains(&bus));
        let detached_send_automation: Vec<AutomationLane> = track
            .automation
            .iter()
            .filter(|l| is_detached_send_lane(l))
            .cloned()
            .collect();
        if !detached_send_automation.is_empty() {
            edits.push(Edit::SetTrack {
                id: track.id,
                prop: TrackProp::Automation(
                    track
                        .automation
                        .iter()
                        .filter(|l| !is_detached_send_lane(l))
                        .cloned()
                        .collect(),
                ),
            });
        }
        let mut detached_plugin_routes = Vec::new();
        let mut updated_plugins = track.plugins.clone();
        let mut plugins_changed = false;
        for p in &mut updated_plugins {
            let sc = p.sidechain.filter(|s| unique_ids.contains(s));
            let outs: BTreeMap<u32, TrackId> = p
                .outputs
                .iter()
                .filter(|&(_, b)| unique_ids.contains(b))
                .map(|(&out, &bus)| (out, bus))
                .collect();
            if sc.is_some() || !outs.is_empty() {
                plugins_changed = true;
                detached_plugin_routes.push((p.id, sc, outs));
                p.sidechain = p.sidechain.filter(|s| !unique_ids.contains(s));
                p.outputs.retain(|_, b| !unique_ids.contains(b));
            }
        }
        if plugins_changed {
            edits.push(Edit::SetTrack {
                id: track.id,
                prop: TrackProp::Plugins(updated_plugins),
            });
        }
        if detached_output.is_some()
            || !detached_sends.is_empty()
            || !detached_send_automation.is_empty()
            || !detached_plugin_routes.is_empty()
        {
            detached_routes.push(DetachedTrackRoutes {
                track: track.id,
                output: detached_output,
                sends: detached_sends,
                send_automation: detached_send_automation,
                plugin_routes: detached_plugin_routes,
            });
        }
    }

    for a in &archived {
        edits.push(Edit::RemoveTrack { id: a.track.id });
    }
    (archived, detached_routes, edits)
}

fn restore_archived_tracks_edits(
    project: &Project,
    mut archived: Vec<ArchivedTrack>,
    detached_routes: Vec<DetachedTrackRoutes>,
    skip_track: Option<TrackId>,
) -> (Vec<Edit>, Option<TrackId>, Project) {
    archived.sort_by_key(|a| a.index);

    let existing_unrestored: Vec<TrackId> = project
        .tracks
        .iter()
        .filter(|t| Some(t.id) != skip_track)
        .map(|t| t.id)
        .collect();
    let existing_buses: Vec<TrackId> = project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Bus && Some(t.id) != skip_track)
        .map(|t| t.id)
        .collect();

    let mut valid_ids = existing_unrestored;
    let mut valid_buses = existing_buses;
    for a in &archived {
        if !valid_ids.contains(&a.track.id) {
            valid_ids.push(a.track.id);
        }
        if a.track.kind == TrackKind::Bus && !valid_buses.contains(&a.track.id) {
            valid_buses.push(a.track.id);
        }
    }

    let mut edits = Vec::new();
    let mut first_restored = None;
    let mut sim = project.clone();
    for ArchivedTrack { index, mut track } in archived {
        if sim.tracks.iter().any(|t| t.id == track.id) {
            continue;
        }
        if first_restored.is_none() {
            first_restored = Some(track.id);
        }
        track.output = track.output.filter(|b| valid_buses.contains(b));
        track
            .sends
            .retain(|s| valid_buses.contains(&s.bus) && s.bus != track.id);
        let sends = &track.sends;
        let plugins = &track.plugins;
        track.automation.retain(|l| match &l.target {
            AutomationTarget::Send { bus } => sends.iter().any(|s| s.bus == *bus),
            AutomationTarget::Param { plugin, .. } => plugins.iter().any(|p| p.id == *plugin),
            _ => true,
        });
        for p in &mut track.plugins {
            p.sidechain = p
                .sidechain
                .filter(|s| valid_ids.contains(s) && *s != track.id);
            p.outputs.retain(|_, b| valid_buses.contains(b));
        }
        let at = index.min(sim.tracks.len());
        sim.tracks.insert(at, track.clone());
        edits.push(Edit::InsertTrack { index: at, track });
    }

    for r in detached_routes {
        if Some(r.track) == skip_track {
            continue;
        }
        let Some(cur) = sim.track(r.track).cloned() else {
            continue;
        };
        if cur.id != MASTER
            && cur.output.is_none()
            && let Some(bus) = r.output
            && valid_buses.contains(&bus)
            && !sim.depends_on(cur.id, bus)
        {
            sim.track_mut(cur.id).expect("checked").output = Some(bus);
            edits.push(Edit::SetTrack {
                id: cur.id,
                prop: TrackProp::Output(Some(bus)),
            });
        }
        if cur.id != MASTER && !r.sends.is_empty() {
            let mut sends = cur.sends.clone();
            let mut sends_changed = false;
            for send in r.sends {
                if valid_buses.contains(&send.bus)
                    && send.bus != cur.id
                    && !sends.iter().any(|s| s.bus == send.bus)
                    && !sim.depends_on(cur.id, send.bus)
                {
                    sends.push(send);
                    sends_changed = true;
                }
            }
            if sends_changed {
                sim.track_mut(cur.id).expect("checked").sends = sends.clone();
                edits.push(Edit::SetTrack {
                    id: cur.id,
                    prop: TrackProp::Sends(sends),
                });
                if !r.send_automation.is_empty() {
                    let cur_ref = sim.track(cur.id).expect("checked");
                    let mut auto = cur_ref.automation.clone();
                    let mut auto_changed = false;
                    for lane in r.send_automation {
                        if let AutomationTarget::Send { bus } = lane.target
                            && cur_ref.sends.iter().any(|s| s.bus == bus)
                            && !auto.iter().any(|l| l.target == lane.target)
                        {
                            auto.push(lane);
                            auto_changed = true;
                        }
                    }
                    if auto_changed {
                        sim.track_mut(cur.id).expect("checked").automation = auto.clone();
                        edits.push(Edit::SetTrack {
                            id: cur.id,
                            prop: TrackProp::Automation(auto),
                        });
                    }
                }
            }
        }
        if !r.plugin_routes.is_empty() {
            let mut plugins = sim.track(cur.id).expect("checked").plugins.clone();
            let mut changed = false;
            for (pid, sc, outputs) in r.plugin_routes {
                let Some(slot) = plugins.iter_mut().find(|p| p.id == pid) else {
                    continue;
                };
                if slot.sidechain.is_none()
                    && let Some(src) = sc
                    && valid_ids.contains(&src)
                    && src != cur.id
                    && !sim.depends_on(src, cur.id)
                {
                    slot.sidechain = Some(src);
                    changed = true;
                }
                for (out_idx, bus) in outputs {
                    if !slot.outputs.contains_key(&out_idx)
                        && valid_buses.contains(&bus)
                        && !sim.depends_on(cur.id, bus)
                    {
                        slot.outputs.insert(out_idx, bus);
                        changed = true;
                    }
                }
            }
            if changed {
                sim.track_mut(cur.id).expect("checked").plugins = plugins.clone();
                edits.push(Edit::SetTrack {
                    id: cur.id,
                    prop: TrackProp::Plugins(plugins),
                });
            }
        }
    }

    (edits, first_restored, sim)
}

fn track_audio_sources(track: &Track) -> impl Iterator<Item = SourceId> + '_ {
    track.clips.iter().filter_map(|c| match c.content {
        ClipContent::Audio { source, .. } => Some(source),
        ClipContent::Midi { .. } => None,
    })
}

/// Delete one or more tracks (or buses). By default (`permanent: false`),
/// the tracks, their clips, plugins, and routing are archived non-destructively
/// in the project so they can be restored later with restore_deleted_tracks.
/// When `permanent` is true, the tracks, any mixdown archives attached to them,
/// and any audio sources used exclusively by them are permanently removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteTracks {
    pub tracks: Vec<TrackId>,
    /// Permanently delete the tracks and their exclusive audio sources and
    /// settings instead of archiving them for later restoration. Defaults to false.
    #[serde(default)]
    pub permanent: bool,
}

impl DeleteTracks {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let mut unique_ids = Vec::with_capacity(self.tracks.len());
        for id in self.tracks {
            require_not_master(project, id, "deleted")?;
            if project.track(id).is_none() {
                return Err(CoreError::TrackNotFound(id));
            }
            if !unique_ids.contains(&id) {
                unique_ids.push(id);
            }
        }
        if unique_ids.is_empty() {
            return Err(CoreError::invalid("select at least one track to delete"));
        }

        let (archived, detached_routes, mut edits) = archive_tracks_edits(project, &unique_ids);
        if !self.permanent {
            let mut archives = project.deleted_track_archives.clone();
            archives.push(DeletedTrackArchive {
                tracks: archived,
                detached_routes,
            });
            edits.push(Edit::SetDeletedTrackArchives(archives));
            return Ok((edits, Outcome::none()));
        }

        let deleted_sources: Vec<SourceId> = unique_ids
            .iter()
            .filter_map(|&id| project.track(id))
            .flat_map(track_audio_sources)
            .chain(
                project
                    .mixdown_archives
                    .iter()
                    .filter(|a| unique_ids.contains(&a.mix_track))
                    .flat_map(|a| &a.tracks)
                    .flat_map(|at| track_audio_sources(&at.track)),
            )
            .collect();

        if project
            .mixdown_archives
            .iter()
            .any(|a| unique_ids.contains(&a.mix_track))
        {
            let mut mixdown_archives = project.mixdown_archives.clone();
            mixdown_archives.retain(|a| !unique_ids.contains(&a.mix_track));
            edits.push(Edit::SetMixdownArchives(mixdown_archives));
        }

        let retained_sources: Vec<SourceId> = project
            .tracks
            .iter()
            .filter(|t| !unique_ids.contains(&t.id))
            .flat_map(track_audio_sources)
            .chain(
                project
                    .mixdown_archives
                    .iter()
                    .filter(|a| !unique_ids.contains(&a.mix_track))
                    .flat_map(|a| &a.tracks)
                    .flat_map(|at| track_audio_sources(&at.track)),
            )
            .chain(
                project
                    .deleted_track_archives
                    .iter()
                    .flat_map(|a| &a.tracks)
                    .flat_map(|at| track_audio_sources(&at.track)),
            )
            .collect();

        let remaining_sources: Vec<Source> = project
            .sources
            .iter()
            .filter(|s| !(deleted_sources.contains(&s.id) && !retained_sources.contains(&s.id)))
            .cloned()
            .collect();
        if remaining_sources.len() != project.sources.len() {
            edits.push(Edit::SetSources(remaining_sources));
        }

        Ok((edits, Outcome::none()))
    }
}

/// Restore the most recently non-destructively deleted track(s) (or the
/// deleted-track archive at `index`), including their clips, plugins, bus
/// routing, sends, automation, and detached incoming routes, as one undo step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreDeletedTracks {
    /// Optional index into `deleted_track_archives`. Defaults to the most
    /// recently deleted batch (`None`).
    #[serde(default)]
    pub index: Option<usize>,
}

impl RestoreDeletedTracks {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        if project.deleted_track_archives.is_empty() {
            return Err(CoreError::invalid("there are no deleted tracks to restore"));
        }
        let target_index = match self.index {
            Some(i) if i < project.deleted_track_archives.len() => i,
            Some(i) => {
                return Err(CoreError::invalid(format!(
                    "deleted track archive index {i} out of range"
                )));
            }
            None => project.deleted_track_archives.len() - 1,
        };
        let mut archives = project.deleted_track_archives.clone();
        let archive = archives.remove(target_index);
        let (mut edits, first_restored, _) =
            restore_archived_tracks_edits(project, archive.tracks, archive.detached_routes, None);
        edits.push(Edit::SetDeletedTrackArchives(archives));
        Ok((
            edits,
            Outcome {
                track: first_restored,
                ..Outcome::default()
            },
        ))
    }
}

/// Remove source tracks that were mixed down into `mix_track`, stashing their
/// full state (clips, plugins, bus routing, sends, automation) and any
/// incoming routes from remaining tracks inside the project's non-destructive
/// mixdown archive so they can be restored later with restore_mixdown_tracks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchiveTracksForMixdown {
    pub mix_track: TrackId,
    pub tracks: Vec<TrackId>,
}

impl ArchiveTracksForMixdown {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_not_master(project, self.mix_track, "used as a mixdown target")?;
        let mut unique_ids = Vec::with_capacity(self.tracks.len());
        for id in self.tracks {
            require_not_master(project, id, "archived")?;
            if id == self.mix_track {
                return Err(CoreError::invalid("a mixdown track cannot archive itself"));
            }
            if !unique_ids.contains(&id) {
                unique_ids.push(id);
            }
        }
        if unique_ids.is_empty() {
            return Err(CoreError::invalid("select at least one track to archive"));
        }

        let (archived, detached_routes, mut edits) = archive_tracks_edits(project, &unique_ids);
        let mut archives = project.mixdown_archives.clone();
        archives.retain(|a| a.mix_track != self.mix_track);
        archives.push(MixdownArchive {
            mix_track: self.mix_track,
            tracks: archived,
            detached_routes,
        });
        edits.push(Edit::SetMixdownArchives(archives));
        Ok((edits, Outcome::none()))
    }
}

/// Restore the original tracks archived when `mix_track` was created
/// (including their clips, plugins, bus routing, sends, automation, and
/// detached incoming routes), optionally removing `mix_track`, as one undo step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreMixdownTracks {
    pub mix_track: TrackId,
    /// Remove the combined mixdown track when restoring its source tracks.
    /// Defaults to true.
    #[serde(default = "default_true")]
    pub remove_mix_track: bool,
}

impl RestoreMixdownTracks {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let archive = project
            .mixdown_archive(self.mix_track)
            .cloned()
            .ok_or_else(|| {
                CoreError::invalid(format!(
                    "track {} has no archived source tracks to restore",
                    self.mix_track
                ))
            })?;
        let will_remove_mix = self.remove_mix_track
            && self.mix_track != MASTER
            && project.track(self.mix_track).is_some();

        let (mut edits, _, mut sim) = restore_archived_tracks_edits(
            project,
            archive.tracks,
            archive.detached_routes,
            will_remove_mix.then_some(self.mix_track),
        );

        let mut archives = project.mixdown_archives.clone();
        archives.retain(|a| a.mix_track != self.mix_track);
        if will_remove_mix {
            let (mut remove_edits, _) = (RemoveTrack {
                track: self.mix_track,
            })
            .lower(&mut sim)?;
            // Avoid duplicate SetMixdownArchives if RemoveTrack emitted one.
            remove_edits.retain(|e| !matches!(e, Edit::SetMixdownArchives(_)));
            edits.extend(remove_edits);
        }
        edits.push(Edit::SetMixdownArchives(archives));
        Ok((edits, Outcome::none()))
    }
}

/// Send a track's (or bus's) signal to a bus instead of the master, or
/// back to the master (bus left out or 0). add_track with kind "bus" makes
/// a bus. A bus can't feed itself, even through other buses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTrackOutput {
    pub track: TrackId,
    /// The bus's track ID. Left out or 0: the master.
    #[serde(default)]
    pub bus: Option<TrackId>,
}

impl SetTrackOutput {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_not_master(project, self.track, "sent elsewhere")?;
        let bus = self.bus.filter(|&b| b != MASTER);
        if let Some(bus) = bus {
            let target = project.track(bus).ok_or(CoreError::TrackNotFound(bus))?;
            if target.kind != TrackKind::Bus {
                return Err(CoreError::invalid(format!(
                    "track {bus} is not a bus (add_track with kind \"bus\" makes one)"
                )));
            }
            if project.depends_on(self.track, bus) {
                return Err(CoreError::invalid(format!(
                    "track {} already takes bus {bus}'s signal: that would make a loop",
                    self.track
                )));
            }
        }
        Ok(set_track(self.track, TrackProp::Output(bus)))
    }
}

/// Send a copy of a track's (or bus's) signal to a bus at its own level,
/// besides where its output goes: an aux send, as for sharing one reverb
/// among tracks with a different amount each. Adds the send, or changes
/// the one to that bus. The level is after the fader and pan unless
/// `pre_fader`. Automate it with target `{ "kind": "send", "bus": <bus>
/// }`. A send can't make a loop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTrackSend {
    pub track: TrackId,
    /// The bus's track ID.
    pub bus: TrackId,
    /// The send's level in dB (-144 to +12). Defaults to 0 for a new send,
    /// or the level it has.
    #[serde(default)]
    pub gain_db: Option<f32>,
    /// Take the signal before the fader and pan. Defaults to false for a
    /// new send, or what it is.
    #[serde(default)]
    pub pre_fader: Option<bool>,
}

impl SetTrackSend {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_not_master(project, self.track, "sent elsewhere")?;
        let bus = self.bus;
        if bus == MASTER {
            return Err(CoreError::invalid(
                "a send can't go to the master, which every track reaches already; \
                 send to a bus (add_track with kind \"bus\" makes one)",
            ));
        }
        let target = project.track(bus).ok_or(CoreError::TrackNotFound(bus))?;
        if target.kind != TrackKind::Bus {
            return Err(CoreError::invalid(format!(
                "track {bus} is not a bus (add_track with kind \"bus\" makes one)"
            )));
        }
        if project.depends_on(self.track, bus) {
            return Err(CoreError::invalid(format!(
                "track {} already takes bus {bus}'s signal: that would make a loop",
                self.track
            )));
        }
        let track = project
            .track(self.track)
            .ok_or(CoreError::TrackNotFound(self.track))?;
        let mut sends = track.sends.clone();
        let existing = sends.iter().find(|s| s.bus == bus).copied();
        let gain_db = self
            .gain_db
            .unwrap_or_else(|| existing.map_or(0.0, |s| s.gain_db));
        validate_gain(gain_db)?;
        let send = Send {
            bus,
            gain_db,
            pre_fader: self
                .pre_fader
                .unwrap_or_else(|| existing.is_some_and(|s| s.pre_fader)),
        };
        match sends.iter_mut().find(|s| s.bus == bus) {
            Some(slot) => *slot = send,
            None => sends.push(send),
        }
        Ok(set_track(self.track, TrackProp::Sends(sends)))
    }
}

/// Remove a track's send to a bus (its automation lane for the send goes
/// too).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveTrackSend {
    pub track: TrackId,
    pub bus: TrackId,
}

impl RemoveTrackSend {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let track = project
            .track(self.track)
            .ok_or(CoreError::TrackNotFound(self.track))?;
        if !track.sends.iter().any(|s| s.bus == self.bus) {
            return Err(CoreError::invalid(format!(
                "track {} has no send to bus {}",
                self.track, self.bus
            )));
        }
        let sends = track
            .sends
            .iter()
            .copied()
            .filter(|s| s.bus != self.bus)
            .collect();
        let (mut edits, outcome) = set_track(self.track, TrackProp::Sends(sends));
        let lanes: Vec<AutomationLane> = track
            .automation
            .iter()
            .filter(|l| l.target != AutomationTarget::Send { bus: self.bus })
            .cloned()
            .collect();
        if lanes.len() != track.automation.len() {
            edits.push(Edit::SetTrack {
                id: self.track,
                prop: TrackProp::Automation(lanes),
            });
        }
        Ok((edits, outcome))
    }
}

/// Rename a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameTrack {
    pub track: TrackId,
    pub name: String,
}

impl RenameTrack {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_not_master(project, self.track, "renamed")?;
        let name = validate_name(&self.name)?;
        Ok(set_track(self.track, TrackProp::Name(name)))
    }
}

/// Set a track's own color override in the arrange and mixer views, as
/// packed 0xRRGGBB (e.g. 0x4c8dff); omit to go back to the automatic color
/// cycled by the track's position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTrackColor {
    pub track: TrackId,
    #[serde(default)]
    pub color: Option<u32>,
}

impl SetTrackColor {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_track(project, self.track)?;
        Ok(set_track(self.track, TrackProp::Color(self.color)))
    }
}

/// Set a track's fader gain in dB (-144 to +12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTrackGain {
    pub track: TrackId,
    pub gain_db: f32,
}

impl SetTrackGain {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_track(project, self.track)?;
        validate_gain(self.gain_db)?;
        Ok(set_track(self.track, TrackProp::GainDb(self.gain_db)))
    }
}

/// Set a track's stereo pan, from -1.0 (left) to 1.0 (right).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTrackPan {
    pub track: TrackId,
    pub pan: f32,
}

impl SetTrackPan {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_track(project, self.track)?;
        if !(-1.0..=1.0).contains(&self.pan) {
            return Err(CoreError::invalid(format!(
                "pan must be between -1.0 and 1.0, got {}",
                self.pan
            )));
        }
        Ok(set_track(self.track, TrackProp::Pan(self.pan)))
    }
}

/// Mute or unmute a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTrackMute {
    pub track: TrackId,
    pub mute: bool,
}

impl SetTrackMute {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_track(project, self.track)?;
        Ok(set_track(self.track, TrackProp::Mute(self.mute)))
    }
}

/// Solo or unsolo a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTrackSolo {
    pub track: TrackId,
    pub solo: bool,
}

impl SetTrackSolo {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_not_master(project, self.track, "soloed")?;
        Ok(set_track(self.track, TrackProp::Solo(self.solo)))
    }
}

/// Set the tempo in beats per minute (20 to 999). Without at_beats this is
/// the tempo at the start of the project; with at_beats it adds a tempo
/// change at that beat (or changes the one there). With ramp, the tempo
/// glides to the new one from the previous change (or the start) instead of
/// jumping at at_beats. MIDI clips and notes keep their bar and beat
/// positions, so they move in time; audio clips, automation and the loop
/// and punch ranges keep their times.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTempo {
    pub bpm: f64,
    /// Where the tempo changes, in beats (quarter notes) from the start of
    /// the project. Left out or 0: the start.
    #[serde(default)]
    pub at_beats: Option<f64>,
    /// Glide to this tempo (only with at_beats). Defaults to false.
    #[serde(default)]
    pub ramp: bool,
}

impl SetTempo {
    /// Sets the tempo at the start of the project.
    pub fn new(bpm: f64) -> Self {
        Self {
            bpm,
            at_beats: None,
            ramp: false,
        }
    }

    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        validate_bpm(self.bpm)?;
        let (mut bpm, mut changes) = (project.tempo_bpm, project.tempo_changes.clone());
        match self.at_beats {
            None | Some(0.0) if self.ramp => {
                return Err(CoreError::invalid(
                    "a ramp needs at_beats: the tempo glides from the previous change to it",
                ));
            }
            None | Some(0.0) => bpm = self.bpm,
            Some(at) => {
                let beat = validate_tempo_beat(at)?;
                match changes.iter_mut().find(|c| same_beat(c.beat, beat)) {
                    Some(change) => {
                        change.bpm = self.bpm;
                        change.ramp = self.ramp;
                    }
                    None => changes.push(TempoChange {
                        beat,
                        bpm: self.bpm,
                        ramp: self.ramp,
                    }),
                }
            }
        }
        Ok((retime(project, bpm, changes)?, Outcome::none()))
    }
}

/// Remove the tempo change at a beat (see set_tempo). MIDI clips and notes
/// keep their bar and beat positions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveTempoChange {
    /// The change's position, in beats (quarter notes) from the start of
    /// the project.
    pub at_beats: f64,
}

impl RemoveTempoChange {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let mut changes = project.tempo_changes.clone();
        let before = changes.len();
        changes.retain(|c| !same_beat(c.beat, self.at_beats));
        if changes.len() == before {
            return Err(CoreError::invalid(format!(
                "there is no tempo change at beat {}",
                self.at_beats
            )));
        }
        Ok((
            retime(project, project.tempo_bpm, changes)?,
            Outcome::none(),
        ))
    }
}

/// Replace the whole tempo map: the tempo at the start and every later
/// change (an empty list keeps one tempo throughout). MIDI clips and notes
/// keep their bar and beat positions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTempoMap {
    /// Tempo at the start, in BPM (20 to 999).
    pub bpm: f64,
    /// Later changes: {"beat": quarter notes from the start (more than 0),
    /// "bpm": tempo, "ramp": true to glide there from the previous change
    /// (optional)}.
    #[serde(default)]
    pub changes: Vec<TempoChange>,
}

impl SetTempoMap {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        validate_bpm(self.bpm)?;
        let mut changes = Vec::with_capacity(self.changes.len());
        for change in &self.changes {
            validate_bpm(change.bpm)?;
            let beat = validate_tempo_beat(change.beat)?;
            // A later entry for the same beat wins.
            changes.retain(|c: &TempoChange| !same_beat(c.beat, beat));
            changes.push(TempoChange {
                beat,
                bpm: change.bpm,
                ramp: change.ramp,
            });
        }
        Ok((retime(project, self.bpm, changes)?, Outcome::none()))
    }
}

fn validate_bpm(bpm: f64) -> Result<(), CoreError> {
    if !(TEMPO_BPM_MIN..=TEMPO_BPM_MAX).contains(&bpm) {
        return Err(CoreError::invalid(format!(
            "bpm must be between {TEMPO_BPM_MIN} and {TEMPO_BPM_MAX}, got {bpm}"
        )));
    }
    Ok(())
}

fn validate_tempo_beat(beat: f64) -> Result<f64, CoreError> {
    let max = MAX_POSITION_S * TEMPO_BPM_MAX / 60.0;
    if !(beat > 0.0 && beat <= max) {
        return Err(CoreError::invalid(format!(
            "a tempo change must be after beat 0 and at most beat {max}, got {beat}"
        )));
    }
    Ok(beat)
}

/// Tempo changes this close together are at the same beat.
fn same_beat(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

/// Edits that set the tempo map to `bpm` and `changes` (sorted here), and
/// move every MIDI clip and note so it keeps its beat position.
fn retime(
    project: &Project,
    bpm: f64,
    mut changes: Vec<TempoChange>,
) -> Result<Vec<Edit>, CoreError> {
    if changes.len() > MAX_TEMPO_CHANGES {
        return Err(CoreError::invalid(format!(
            "a project can have at most {MAX_TEMPO_CHANGES} tempo changes"
        )));
    }
    changes.sort_by(|a, b| a.beat.total_cmp(&b.beat));
    let old = project.tempo_map();
    let new = TempoMap::new(project.sample_rate, bpm, &changes);
    let mut edits = vec![Edit::SetTempo { bpm, changes }];
    // Old timeline frame → new timeline frame, keeping the beat.
    let map = |frame: u64| new.frame_at(old.beat_at(frame as f64)).round().max(0.0) as u64;
    for clip in project.tracks.iter().flat_map(|t| &t.clips) {
        let ClipContent::Midi { notes, controls } = &clip.content else {
            continue;
        };
        let start = map(clip.start);
        let length = map(clip.end()).saturating_sub(start).max(1);
        let moved: Vec<Note> = notes
            .iter()
            .map(|n| {
                let from = map(clip.start + n.start).saturating_sub(start);
                let to = map(clip.start + n.end()).saturating_sub(start);
                Note {
                    start: from,
                    length: to.saturating_sub(from).max(1),
                    ..*n
                }
            })
            .collect();
        if start != clip.start {
            edits.push(Edit::SetClip {
                id: clip.id,
                prop: ClipProp::Start(start),
            });
        }
        if length != clip.length {
            edits.push(Edit::SetClip {
                id: clip.id,
                prop: ClipProp::Length(length),
            });
        }
        if moved != *notes {
            edits.push(Edit::SetClip {
                id: clip.id,
                prop: ClipProp::Notes(moved),
            });
        }
        let moved: Vec<ControlPoint> = controls
            .iter()
            .map(|c| ControlPoint {
                frame: map(clip.start + c.frame).saturating_sub(start),
                ..*c
            })
            .collect();
        if moved != *controls {
            edits.push(Edit::SetClip {
                id: clip.id,
                prop: ClipProp::Controls(moved),
            });
        }
    }
    Ok(edits)
}

/// Set the time signature, e.g. 7/8. The denominator must be a power of
/// two up to 32. Without at_bar this is the time signature at the start of
/// the project; with at_bar it adds a change from that bar on (or changes
/// the one there). Bars are numbered as the ruler shows them, from 1.
/// Notes, clips and tempo changes keep their times and beats; only where
/// bars start moves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTimeSignature {
    pub numerator: u8,
    pub denominator: u8,
    /// The bar the change starts at, 2 or more. Left out or 1: the start.
    #[serde(default)]
    pub at_bar: Option<u32>,
}

impl SetTimeSignature {
    /// Sets the time signature at the start of the project.
    pub fn new(numerator: u8, denominator: u8) -> Self {
        Self {
            numerator,
            denominator,
            at_bar: None,
        }
    }

    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let sig = validate_meter(self.numerator, self.denominator)?;
        let (mut start, mut changes) = (project.time_signature, project.meter_changes.clone());
        match self.at_bar {
            None | Some(1) => start = sig,
            Some(bar) => {
                validate_bar(bar)?;
                changes.retain(|c| c.bar != bar);
                if changes.len() >= MAX_METER_CHANGES {
                    return Err(CoreError::invalid(format!(
                        "a project can have at most {MAX_METER_CHANGES} time signature changes"
                    )));
                }
                changes.push(MeterChange {
                    bar,
                    numerator: sig.numerator,
                    denominator: sig.denominator,
                });
                changes.sort_by_key(|c| c.bar);
            }
        }
        let mut edits = vec![Edit::SetMeter {
            start,
            changes: changes.clone(),
        }];
        edits.extend(update_drum_clips_for_meter(project, start, &changes));
        Ok((edits, Outcome::none()))
    }
}

/// Remove the time signature change at a bar (see set_time_signature).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveTimeSignatureChange {
    /// The bar the change starts at, numbered as the ruler shows it.
    pub at_bar: u32,
}

impl RemoveTimeSignatureChange {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let mut changes = project.meter_changes.clone();
        let before = changes.len();
        changes.retain(|c| c.bar != self.at_bar);
        if changes.len() == before {
            return Err(CoreError::invalid(format!(
                "there is no time signature change at bar {}",
                self.at_bar
            )));
        }
        let mut edits = vec![Edit::SetMeter {
            start: project.time_signature,
            changes: changes.clone(),
        }];
        edits.extend(update_drum_clips_for_meter(
            project,
            project.time_signature,
            &changes,
        ));
        Ok((edits, Outcome::none()))
    }
}

/// Replace the whole time signature map: the time signature at the start
/// and every later change (an empty list keeps one time signature
/// throughout). Bars are numbered as the ruler shows them, from 1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTimeSignatureMap {
    pub numerator: u8,
    pub denominator: u8,
    /// Later changes: {"bar": 2 or more, "numerator", "denominator"}.
    #[serde(default)]
    pub changes: Vec<MeterChange>,
}

impl SetTimeSignatureMap {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let start = validate_meter(self.numerator, self.denominator)?;
        if self.changes.len() > MAX_METER_CHANGES {
            return Err(CoreError::invalid(format!(
                "a project can have at most {MAX_METER_CHANGES} time signature changes"
            )));
        }
        let mut changes: Vec<MeterChange> = Vec::with_capacity(self.changes.len());
        for change in self.changes {
            validate_meter(change.numerator, change.denominator)?;
            validate_bar(change.bar)?;
            // A later entry for the same bar wins.
            changes.retain(|c| c.bar != change.bar);
            changes.push(change);
        }
        changes.sort_by_key(|c| c.bar);
        let mut edits = vec![Edit::SetMeter {
            start,
            changes: changes.clone(),
        }];
        edits.extend(update_drum_clips_for_meter(project, start, &changes));
        Ok((edits, Outcome::none()))
    }
}

/// Adjusts clips on drum tracks (`builtin:drums`) when the time
/// signature map changes, so each clip's length and notes match the
/// new bar's step grid at its start.
fn update_drum_clips_for_meter(
    project: &Project,
    new_start: TimeSignature,
    new_changes: &[MeterChange],
) -> Vec<Edit> {
    let tempo = project.tempo_map();
    let old_meter = project.meter_map();
    let new_meter = MeterMap::new(new_start, new_changes);
    let mut edits = Vec::new();
    for track in &project.tracks {
        if !track
            .plugins
            .first()
            .is_some_and(|p| p.plugin == DRUMS_PLUGIN_ID)
        {
            continue;
        }
        for clip in &track.clips {
            let ClipContent::Midi { notes, .. } = &clip.content else {
                continue;
            };
            let start_beat = tempo.beat_at(clip.start as f64);
            let old_sig = old_meter.meter_at(start_beat + 1e-9);
            let new_sig = new_meter.meter_at(start_beat + 1e-9);
            if old_sig == new_sig {
                continue;
            }
            let new_len = tempo
                .clip_frames(clip.start, new_sig.beats_per_bar())
                .max(1);
            if clip.length != new_len {
                edits.push(Edit::SetClip {
                    id: clip.id,
                    prop: ClipProp::Length(new_len),
                });
            }
            let old_step_beats = old_sig.step_beats();
            let new_step_beats = new_sig.step_beats();
            let max_steps = new_sig.steps_per_bar();
            let mut updated = Vec::with_capacity(notes.len());
            for n in notes {
                let beat = tempo.clip_beats(clip.start, n.start);
                let step_f = (beat / old_step_beats).round();
                if step_f < 0.0 || step_f >= f64::from(max_steps) {
                    continue;
                }
                let (start, length) = if (old_step_beats - new_step_beats).abs() > 1e-12 {
                    let s = tempo.clip_frames(clip.start, step_f * new_step_beats);
                    let e = tempo.clip_frames(clip.start, (step_f + 1.0) * new_step_beats);
                    (s, e.saturating_sub(s).max(1))
                } else {
                    (n.start, n.length)
                };
                if start < new_len {
                    updated.push(Note {
                        start,
                        length,
                        ..*n
                    });
                }
            }
            if updated != *notes {
                edits.push(Edit::SetClip {
                    id: clip.id,
                    prop: ClipProp::Notes(updated),
                });
            }
        }
    }
    edits
}

fn validate_meter(numerator: u8, denominator: u8) -> Result<TimeSignature, CoreError> {
    if !(1..=32).contains(&numerator) {
        return Err(CoreError::invalid(format!(
            "numerator must be between 1 and 32, got {numerator}"
        )));
    }
    if !(denominator.is_power_of_two() && denominator <= 32) {
        return Err(CoreError::invalid(format!(
            "denominator must be 1, 2, 4, 8, 16 or 32, got {denominator}"
        )));
    }
    Ok(TimeSignature {
        numerator,
        denominator,
    })
}

/// Highest bar a time signature change may start at.
const MAX_BAR: u32 = 1_000_000;
/// Most time signature changes a project may have.
const MAX_METER_CHANGES: usize = 10_000;

fn validate_bar(bar: u32) -> Result<(), CoreError> {
    if !(2..=MAX_BAR).contains(&bar) {
        return Err(CoreError::invalid(format!(
            "a time signature change must be at bar 2 to {MAX_BAR}, got {bar}"
        )));
    }
    Ok(())
}

/// Rename the project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameProject {
    pub name: String,
}

impl RenameProject {
    fn lower(self, _project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let name = validate_name(&self.name)?;
        Ok((vec![Edit::SetProjectName(name)], Outcome::none()))
    }
}

/// Place an audio source from the pool (see import_audio) on the timeline.
/// Returns the new clip's ID, and the track's ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddClip {
    pub source: SourceId,
    /// Audio track to place the clip on. Defaults to a new track named after
    /// the source.
    #[serde(default)]
    pub track: Option<TrackId>,
    /// Timeline position of the clip's start, in seconds. Defaults to 0.
    #[serde(default)]
    pub position_s: f64,
    /// Seconds into the source where the clip starts. Defaults to 0.
    #[serde(default)]
    pub offset_s: f64,
    /// Clip length in seconds. Defaults to the rest of the source.
    #[serde(default)]
    pub length_s: Option<f64>,
    /// Place the clip muted.
    #[serde(default)]
    pub muted: bool,
    /// Place the clip as a new take of what it overlaps on the track: in a
    /// take lane above those clips, which (unless this clip is muted) go
    /// silent where it covers them, split at its edges. Recording places
    /// takes this way.
    #[serde(default)]
    pub take: bool,
}

impl AddClip {
    /// The whole source at `position_s`.
    pub fn new(source: SourceId, track: Option<TrackId>, position_s: f64) -> Self {
        Self {
            source,
            track,
            position_s,
            offset_s: 0.0,
            length_s: None,
            muted: false,
            take: false,
        }
    }

    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let source = project
            .source(self.source)
            .ok_or(CoreError::SourceNotFound(self.source))?;
        let (source_name, source_frames) = (source.name.clone(), source.frames);
        if source_frames == 0 {
            return Err(CoreError::invalid(format!(
                "source {} is empty",
                self.source
            )));
        }
        let start = position_frames(project, self.position_s)?;
        let offset = position_frames(project, self.offset_s)?;
        if offset >= source_frames {
            return Err(CoreError::invalid(format!(
                "offset_s is past the end of source {}",
                self.source
            )));
        }
        let available = source_frames - offset;
        let length = match self.length_s {
            Some(l) => position_frames(project, l)?.min(available),
            None => available,
        };
        if length == 0 {
            return Err(CoreError::invalid("length_s must be more than 0"));
        }

        let mut edits = Vec::new();
        let track = target_track(
            project,
            self.track,
            TrackKind::Audio,
            &source_name,
            &mut edits,
        )?;
        let clip = Clip {
            id: project.alloc_clip_id(),
            start,
            length,
            gain_db: 0.0,
            muted: self.muted,
            lane: 0,
            name: None,
            content: ClipContent::Audio {
                source: self.source,
                offset,
            },
        };
        let outcome = Outcome {
            track: Some(track),
            clip: Some(clip.id),
            ..Outcome::default()
        };
        insert_clip(project, track, clip, self.take, &mut edits);
        Ok((edits, outcome))
    }
}

/// Move a clip to a new position and/or onto another track of the same kind
/// (audio clips onto audio tracks, MIDI clips onto MIDI tracks).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveClip {
    pub clip: ClipId,
    /// New timeline position of the clip's start, in seconds. Defaults to
    /// the current position.
    #[serde(default)]
    pub position_s: Option<f64>,
    /// Track to move the clip to. Defaults to its current track.
    #[serde(default)]
    pub track: Option<TrackId>,
}

impl MoveClip {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (current_track, clip) = project
            .clip(self.clip)
            .ok_or(CoreError::ClipNotFound(self.clip))?;
        let (current_track, mut clip) = (current_track.id, clip.clone());
        if self.position_s.is_none() && self.track.is_none() {
            return Err(CoreError::invalid("give position_s, track, or both"));
        }
        if let Some(position_s) = self.position_s {
            clip.start = position_frames(project, position_s)?;
        }
        let edits = match self.track {
            Some(track) if track != current_track => {
                require_kind(project, track, clip.kind())?;
                vec![
                    Edit::RemoveClip { id: clip.id },
                    Edit::InsertClip {
                        track,
                        index: usize::MAX,
                        clip,
                    },
                ]
            }
            _ => vec![Edit::SetClip {
                id: clip.id,
                prop: ClipProp::Start(clip.start),
            }],
        };
        Ok((edits, Outcome::none()))
    }
}

/// Remove a clip from the timeline. Its source stays in the pool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveClip {
    pub clip: ClipId,
}

impl RemoveClip {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        project
            .clip(self.clip)
            .ok_or(CoreError::ClipNotFound(self.clip))?;
        Ok((vec![Edit::RemoveClip { id: self.clip }], Outcome::none()))
    }
}

/// Set a clip's gain in dB (-144 to +12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetClipGain {
    pub clip: ClipId,
    pub gain_db: f32,
}

impl SetClipGain {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        project
            .clip(self.clip)
            .ok_or(CoreError::ClipNotFound(self.clip))?;
        validate_gain(self.gain_db)?;
        let edit = Edit::SetClip {
            id: self.clip,
            prop: ClipProp::GainDb(self.gain_db),
        };
        Ok((vec![edit], Outcome::none()))
    }
}

/// Mute or unmute a clip. Muted clips stay on the timeline but are silent
/// (earlier takes of a loop recording are muted).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetClipMute {
    pub clip: ClipId,
    pub muted: bool,
}

impl SetClipMute {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        project
            .clip(self.clip)
            .ok_or(CoreError::ClipNotFound(self.clip))?;
        let edit = Edit::SetClip {
            id: self.clip,
            prop: ClipProp::Muted(self.muted),
        };
        Ok((vec![edit], Outcome::none()))
    }
}

/// Name a clip. An empty name goes back to the default label (its audio
/// file's name, or its track's name for MIDI clips).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameClip {
    pub clip: ClipId,
    pub name: String,
}

impl RenameClip {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        project
            .clip(self.clip)
            .ok_or(CoreError::ClipNotFound(self.clip))?;
        let name = match self.name.trim() {
            "" => None,
            _ => Some(validate_name(&self.name)?),
        };
        let edit = Edit::SetClip {
            id: self.clip,
            prop: ClipProp::Name(name),
        };
        Ok((vec![edit], Outcome::none()))
    }
}

/// Arm or disarm a track for recording. While an audio track is armed,
/// Dawkey connects its audio inputs to the default audio source. While a
/// MIDI track is armed, Dawkey connects MIDI inputs (keyboards) and plays
/// what arrives through that track's synth, even when stopped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTrackArm {
    pub track: TrackId,
    pub armed: bool,
}

impl SetTrackArm {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let track = project
            .track(self.track)
            .ok_or(CoreError::TrackNotFound(self.track))?;
        if track.kind == TrackKind::Bus {
            return Err(CoreError::invalid(format!(
                "track {} is a bus; only audio and MIDI tracks record",
                self.track
            )));
        }
        Ok(set_track(self.track, TrackProp::Armed(self.armed)))
    }
}

/// Choose which input channels a track records: "stereo" (both), "left"
/// or "right" (mono).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTrackInput {
    pub track: TrackId,
    pub input: InputChannels,
}

impl SetTrackInput {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_kind(project, self.track, TrackKind::Audio)?;
        Ok(set_track(self.track, TrackProp::Input(self.input)))
    }
}

/// Turn input monitoring on or off for an audio track. While the track is
/// armed and monitoring, it plays its input (the channels set_track_input
/// picks) through its plugins, fader and pan, so you hear what you record,
/// along with its clips. Use headphones: a microphone near speakers feeds
/// back. Armed MIDI tracks always play their input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTrackMonitor {
    pub track: TrackId,
    pub monitor: bool,
}

impl SetTrackMonitor {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_kind(project, self.track, TrackKind::Audio)?;
        Ok(set_track(self.track, TrackProp::Monitor(self.monitor)))
    }
}

/// Choose which MIDI device and channel play into a MIDI track while it is
/// armed (and are recorded on it). get_engine_status lists the devices
/// under `midi_inputs`. Leave device out (or null) for every device, and
/// channel out for every channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTrackMidiInput {
    pub track: TrackId,
    #[serde(default)]
    pub device: Option<String>,
    /// Only messages on this MIDI channel, 1 to 16.
    #[serde(default)]
    pub channel: Option<u8>,
}

impl SetTrackMidiInput {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_kind(project, self.track, TrackKind::Midi)?;
        let device = self.device.as_deref().map(validate_device).transpose()?;
        if let Some(channel) = self.channel {
            validate_channel(channel)?;
        }
        Ok(set_track(
            self.track,
            TrackProp::MidiInput(device, self.channel),
        ))
    }
}

fn validate_channel(channel: u8) -> Result<(), CoreError> {
    if !(1..=16).contains(&channel) {
        return Err(CoreError::invalid(format!(
            "channel must be between 1 and 16, got {channel}"
        )));
    }
    Ok(())
}

/// Send a MIDI track's notes and controllers to an external MIDI device (or
/// another app's MIDI input) instead of playing them on its instrument.
/// MIDI effect plugins at the start of the track's chain still shape the
/// notes on the way. get_engine_status lists the devices under
/// `midi_outputs`. Leave device out (or null) to play the track's own
/// instrument again. latency_ms is how long the device takes to sound
/// after it gets MIDI: delay compensation then sends its MIDI that much
/// ahead of the rest of the mix (delaying the mix if needed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTrackMidiOutput {
    pub track: TrackId,
    #[serde(default)]
    pub device: Option<String>,
    /// MIDI channel, 1 to 16. Defaults to 1.
    #[serde(default)]
    pub channel: Option<u8>,
    /// The device's latency in milliseconds, 0 to 1000. Left out: kept if
    /// the device stays the same, otherwise 0.
    #[serde(default)]
    pub latency_ms: Option<f64>,
}

impl SetTrackMidiOutput {
    /// Sends `track` to `device` (or back to its instrument with `None`) on
    /// channel 1.
    pub fn new(track: TrackId, device: Option<String>) -> Self {
        Self {
            track,
            device,
            channel: None,
            latency_ms: None,
        }
    }

    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_kind(project, self.track, TrackKind::Midi)?;
        let current = project
            .track(self.track)
            .and_then(|t| t.midi_output.clone());
        let channel = self.channel.unwrap_or(1);
        validate_channel(channel)?;
        if let Some(ms) = self.latency_ms
            && !(0.0..=DEVICE_LATENCY_MS_MAX).contains(&ms)
        {
            return Err(CoreError::invalid(format!(
                "latency_ms must be between 0 and {DEVICE_LATENCY_MS_MAX}, got {ms}"
            )));
        }
        let output = match self.device {
            Some(device) => {
                let device = validate_device(&device)?;
                let kept = current
                    .filter(|c| c.device == device)
                    .map_or(0.0, |c| c.latency_ms);
                Some(MidiOutput {
                    device,
                    channel,
                    latency_ms: self.latency_ms.unwrap_or(kept),
                })
            }
            None if self.channel.is_some() || self.latency_ms.is_some() => {
                return Err(CoreError::invalid(
                    "give a device with the channel and latency",
                ));
            }
            None => None,
        };
        Ok(set_track(self.track, TrackProp::MidiOutput(output)))
    }
}

/// Checks a MIDI device name.
fn validate_device(name: &str) -> Result<String, CoreError> {
    validate_name(name).map_err(|e| CoreError::invalid(format!("device: {e}")))
}

/// Set the loop range and/or switch looping on or off. While enabled,
/// playback (and recording) repeats the range; each recorded pass becomes a
/// take, with all but the last muted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetLoop {
    /// Loop start in seconds. Give with `end_s` to change the range.
    #[serde(default)]
    pub start_s: Option<f64>,
    /// Loop end in seconds.
    #[serde(default)]
    pub end_s: Option<f64>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

impl SetLoop {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let region = update_region(
            project,
            project.loop_region,
            self.start_s,
            self.end_s,
            self.enabled,
        )?;
        Ok((vec![Edit::SetLoop(region)], Outcome::none()))
    }
}

/// Set the punch range and/or switch punch recording on or off. While
/// enabled, recording only captures inside the range.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPunch {
    /// Punch-in time in seconds. Give with `end_s` to change the range.
    #[serde(default)]
    pub start_s: Option<f64>,
    /// Punch-out time in seconds.
    #[serde(default)]
    pub end_s: Option<f64>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

impl SetPunch {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let region = update_region(
            project,
            project.punch,
            self.start_s,
            self.end_s,
            self.enabled,
        )?;
        Ok((vec![Edit::SetPunch(region)], Outcome::none()))
    }
}

/// Set a clip's length in seconds. An audio clip can't be longer than the
/// rest of its source; a MIDI clip's notes past the new end are cut off (but
/// kept).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetClipLength {
    pub clip: ClipId,
    pub length_s: f64,
}

impl SetClipLength {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (_, clip) = project
            .clip(self.clip)
            .ok_or(CoreError::ClipNotFound(self.clip))?;
        let length = position_frames(project, self.length_s)?;
        if length == 0 {
            return Err(CoreError::invalid("length_s must be more than 0"));
        }
        if let ClipContent::Audio { source, offset } = clip.content {
            let available = project
                .source(source)
                .ok_or(CoreError::SourceNotFound(source))?
                .frames
                .saturating_sub(offset);
            if length > available {
                return Err(CoreError::invalid(format!(
                    "clip {} can be at most {:.3} s long (the rest of its source)",
                    self.clip,
                    project.frames_to_seconds(available)
                )));
            }
        }
        let edit = Edit::SetClip {
            id: self.clip,
            prop: ClipProp::Length(length),
        };
        Ok((vec![edit], Outcome::none()))
    }
}

/// Move a clip's start and/or end edge on the timeline while what it plays
/// stays where it is (trimming). Moving an audio clip's start later skips
/// into its source, earlier shows more of it (back to the source's start),
/// and its end can't go past the source's end. Moving a MIDI clip's start
/// shifts its notes and controller changes so they keep their timeline
/// positions: notes that start before the new start are removed, and the
/// clip starts with each controller's value there. A MIDI clip's notes past
/// its end are cut off (but kept).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrimClip {
    pub clip: ClipId,
    /// New timeline position of the clip's start, in seconds. Defaults to
    /// the current start.
    #[serde(default)]
    pub start_s: Option<f64>,
    /// New timeline position of the clip's end, in seconds. Defaults to the
    /// current end.
    #[serde(default)]
    pub end_s: Option<f64>,
}

impl TrimClip {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (_, clip) = project
            .clip(self.clip)
            .ok_or(CoreError::ClipNotFound(self.clip))?;
        if self.start_s.is_none() && self.end_s.is_none() {
            return Err(CoreError::invalid("give start_s, end_s, or both"));
        }
        let start = match self.start_s {
            Some(s) => position_frames(project, s)?,
            None => clip.start,
        };
        let end = match self.end_s {
            Some(s) => position_frames(project, s)?,
            None => clip.end(),
        };
        if end <= start {
            return Err(CoreError::invalid(format!(
                "the clip's end ({:.3} s) must be after its start ({:.3} s)",
                project.frames_to_seconds(end),
                project.frames_to_seconds(start)
            )));
        }
        let id = self.clip;
        let mut edits = vec![
            Edit::SetClip {
                id,
                prop: ClipProp::Start(start),
            },
            Edit::SetClip {
                id,
                prop: ClipProp::Length(end - start),
            },
        ];
        match &clip.content {
            ClipContent::Audio { source, offset } => {
                let source_frames = project
                    .source(*source)
                    .ok_or(CoreError::SourceNotFound(*source))?
                    .frames;
                // The source frame that plays at the new start.
                let new_offset = (*offset + start).checked_sub(clip.start).ok_or_else(|| {
                    CoreError::invalid(format!(
                        "clip {id} can start at most {:.3} s earlier (at the start of its source)",
                        project.frames_to_seconds(*offset)
                    ))
                })?;
                if new_offset + (end - start) > source_frames {
                    return Err(CoreError::invalid(format!(
                        "clip {id} can end at most at {:.3} s (the end of its source)",
                        project.frames_to_seconds(start + source_frames.saturating_sub(new_offset))
                    )));
                }
                edits.push(Edit::SetClip {
                    id,
                    prop: ClipProp::Offset(new_offset),
                });
            }
            ClipContent::Midi { notes, controls } => {
                let (notes, controls) = if start >= clip.start {
                    let cut = start - clip.start;
                    (split_notes(notes, cut).1, split_controls(controls, cut).1)
                } else {
                    let shift = clip.start - start;
                    (
                        notes
                            .iter()
                            .map(|n| Note {
                                start: n.start + shift,
                                ..*n
                            })
                            .collect(),
                        controls
                            .iter()
                            .map(|p| ControlPoint {
                                frame: p.frame + shift,
                                ..*p
                            })
                            .collect(),
                    )
                };
                edits.push(Edit::SetClip {
                    id,
                    prop: ClipProp::Notes(notes),
                });
                edits.push(Edit::SetClip {
                    id,
                    prop: ClipProp::Controls(controls),
                });
            }
        }
        Ok((edits, Outcome::none()))
    }
}

/// Split a clip in two at a timeline position inside it. The first part
/// keeps the clip's ID; the second part gets a new ID, returned as `clip`.
/// Both keep the clip's gain and mute. A MIDI clip's notes go to the part
/// they start in (a note that crosses the split is cut off at the end of
/// the first part), and the second part starts with each controller's value
/// at the split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SplitClip {
    pub clip: ClipId,
    /// Timeline position of the split, in seconds; must be inside the clip.
    pub at_s: f64,
}

impl SplitClip {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (track, clip) = project
            .clip(self.clip)
            .ok_or(CoreError::ClipNotFound(self.clip))?;
        let at = position_frames(project, self.at_s)?;
        if at <= clip.start || at >= clip.end() {
            return Err(CoreError::invalid(format!(
                "at_s must be inside clip {} ({:.3} s to {:.3} s), got {}",
                self.clip,
                project.frames_to_seconds(clip.start),
                project.frames_to_seconds(clip.end()),
                self.at_s
            )));
        }
        let track_id = track.id;
        let index = track
            .clips
            .iter()
            .position(|c| c.id == self.clip)
            .expect("the clip is on its track");
        let clip = clip.clone();
        let (first, second) = cut_clip(&clip, at, project.alloc_clip_id());
        let mut edits = clip_edits(&clip, first);
        let outcome = Outcome {
            track: Some(track_id),
            clip: Some(second.id),
            ..Outcome::default()
        };
        edits.push(Edit::InsertClip {
            track: track_id,
            index: index + 1,
            clip: second,
        });
        Ok((edits, outcome))
    }
}

/// Most copies one `repeat_clip` makes.
const MAX_REPEATS: u64 = 1000;

/// Copy a clip (with its gain, mute, and audio region or MIDI notes and
/// controllers) to a position on its own track or another of the same
/// kind. The copy is placed exactly there, even over other clips (which
/// then play together). Returns the copy as `clip` (and its notes' IDs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DuplicateClip {
    pub clip: ClipId,
    /// Timeline position of the copy's start, in seconds. Defaults to the
    /// clip's end, so the copy follows it.
    #[serde(default)]
    pub position_s: Option<f64>,
    /// Track to put the copy on. Defaults to the clip's track.
    #[serde(default)]
    pub track: Option<TrackId>,
}

impl DuplicateClip {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (track, clip) = project
            .clip(self.clip)
            .ok_or(CoreError::ClipNotFound(self.clip))?;
        let (mut track, clip) = (track.id, clip.clone());
        if let Some(target) = self.track {
            require_kind(project, target, clip.kind())?;
            track = target;
        }
        let start = match self.position_s {
            Some(position_s) => position_frames(project, position_s)?,
            None => clip.end(),
        };
        let copy = copy_clip(project, &clip, start);
        let outcome = Outcome {
            track: Some(track),
            clip: Some(copy.id),
            notes: copy.notes().iter().map(|n| n.id).collect(),
            ..Outcome::default()
        };
        let edit = Edit::InsertClip {
            track,
            index: usize::MAX,
            clip: copy,
        };
        Ok((vec![edit], outcome))
    }
}

/// Repeat a clip: copies of it laid end to end after it on its track, up
/// to a timeline position; the last copy is cut short there. Copies are
/// placed even over other clips. Returns the copies as `clips` (the first
/// also as `clip`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepeatClip {
    pub clip: ClipId,
    /// Where the repeats end (timeline position in seconds); must be after
    /// the clip's end.
    pub until_s: f64,
}

impl RepeatClip {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (track, clip) = project
            .clip(self.clip)
            .ok_or(CoreError::ClipNotFound(self.clip))?;
        let (track, clip) = (track.id, clip.clone());
        let until = position_frames(project, self.until_s)?;
        if until <= clip.end() || clip.length == 0 {
            return Err(CoreError::invalid(format!(
                "until_s must be after clip {}'s end ({:.3} s), got {}",
                clip.id,
                project.frames_to_seconds(clip.end()),
                self.until_s
            )));
        }
        if (until - clip.end()).div_ceil(clip.length) > MAX_REPEATS {
            return Err(CoreError::invalid(format!(
                "that would make more than {MAX_REPEATS} copies; repeat up to an earlier until_s"
            )));
        }
        let mut edits = Vec::new();
        let mut clips = Vec::new();
        let mut at = clip.end();
        while at < until {
            // Lengths are whole frames, so a clip made a bar long can fall
            // short of the bar by up to a frame (a bar at 122 BPM is
            // 94 426.23 frames), and so can each copy. A remainder no longer
            // than that shortfall is where the copies were meant to end,
            // not a sliver of one more.
            if !clips.is_empty() && until - at <= clips.len() as u64 + 1 {
                break;
            }
            let mut copy = copy_clip(project, &clip, at);
            if copy.end() > until {
                copy = cut_clip(&copy, until, copy.id).0;
            }
            at = copy.end();
            clips.push(copy.id);
            edits.push(Edit::InsertClip {
                track,
                index: usize::MAX,
                clip: copy,
            });
        }
        let outcome = Outcome {
            track: Some(track),
            clip: clips.first().copied(),
            clips,
            ..Outcome::default()
        };
        Ok((edits, outcome))
    }
}

/// A copy of `clip` at timeline frame `start`, with a new ID (and new note
/// IDs), in the main lane.
fn copy_clip(project: &mut Project, clip: &Clip, start: u64) -> Clip {
    let mut copy = Clip {
        id: project.alloc_clip_id(),
        start,
        lane: 0,
        ..clip.clone()
    };
    if let ClipContent::Midi { notes, .. } = &mut copy.content {
        for note in notes {
            note.id = project.alloc_note_id();
        }
    }
    copy
}

/// Make a take play over a time range (comping). Within the range, the
/// clip and the other clips in its take lane play (unmuted), and every
/// other clip on the track goes silent (muted) where they play. Clips are
/// split at the range's edges where needed, so nothing changes outside it.
/// The range defaults to the clip, and may reach past it over more of the
/// lane, but must overlap the clip. Returns the part of the clip that now
/// plays as `clip` (a new ID if it was split).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompTake {
    pub clip: ClipId,
    /// Timeline position where the range starts, in seconds. Defaults to the
    /// clip's start.
    #[serde(default)]
    pub start_s: Option<f64>,
    /// Timeline position where the range ends, in seconds. Defaults to the
    /// clip's end.
    #[serde(default)]
    pub end_s: Option<f64>,
}

impl CompTake {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (track, clip) = project
            .clip(self.clip)
            .ok_or(CoreError::ClipNotFound(self.clip))?;
        let from = match self.start_s {
            Some(s) => position_frames(project, s)?,
            None => clip.start,
        };
        let to = match self.end_s {
            Some(s) => position_frames(project, s)?,
            None => clip.end(),
        };
        if from.max(clip.start) >= to.min(clip.end()) {
            return Err(CoreError::invalid(format!(
                "the range must overlap clip {} ({:.3} s to {:.3} s)",
                self.clip,
                project.frames_to_seconds(clip.start),
                project.frames_to_seconds(clip.end()),
            )));
        }
        let lane: Vec<Clip> = track
            .clips
            .iter()
            .filter(|c| c.lane == clip.lane && c.start < to && from < c.end())
            .cloned()
            .collect();
        let track = track.id;
        let mut edits = Vec::new();
        let mut playing = self.clip;
        let mut spans = Vec::new();
        for c in &lane {
            let span = (from.max(c.start), to.min(c.end()));
            spans.push(span);
            if !c.muted {
                continue;
            }
            let pieces = cut_at(project, c, [span.0, span.1])
                .into_iter()
                .map(|mut piece| {
                    if within(&piece, &[span]) {
                        piece.muted = false;
                        if c.id == self.clip {
                            playing = piece.id;
                        }
                    }
                    piece
                })
                .collect();
            replace_clip(c, track, pieces, &mut edits);
        }
        let keep: Vec<ClipId> = lane.iter().map(|c| c.id).collect();
        silence_others(project, track, &keep, &spans, &mut edits);
        let outcome = Outcome {
            track: Some(track),
            clip: Some(playing),
            ..Outcome::default()
        };
        Ok((edits, outcome))
    }
}

/// `clip` cut in two at timeline frame `at` (inside it). The first part
/// keeps the clip's ID; the second gets `id`. A MIDI clip's notes go to the
/// part they start in, and the second part starts with each controller's
/// value at the cut.
fn cut_clip(clip: &Clip, at: u64, id: ClipId) -> (Clip, Clip) {
    let cut = at - clip.start;
    let mut first = Clip {
        length: cut,
        ..clip.clone()
    };
    let mut second = Clip {
        id,
        start: at,
        length: clip.length - cut,
        ..clip.clone()
    };
    match (&mut first.content, &mut second.content) {
        (ClipContent::Audio { .. }, ClipContent::Audio { offset, .. }) => *offset += cut,
        (
            ClipContent::Midi {
                notes: first_notes,
                controls: first_controls,
            },
            ClipContent::Midi {
                notes: second_notes,
                controls: second_controls,
            },
        ) => {
            let notes = split_notes(first_notes, cut);
            let controls = split_controls(first_controls, cut);
            (*first_notes, *second_notes) = notes;
            (*first_controls, *second_controls) = controls;
        }
        _ => unreachable!("both parts are copies of one clip"),
    }
    (first, second)
}

/// `clip` cut at each of `points` (timeline frames) that falls inside it,
/// in order. The first part keeps the clip's ID.
fn cut_at(project: &mut Project, clip: &Clip, points: impl IntoIterator<Item = u64>) -> Vec<Clip> {
    let mut points: Vec<u64> = points.into_iter().collect();
    points.sort_unstable();
    points.dedup();
    let mut pieces = Vec::new();
    let mut rest = clip.clone();
    for at in points {
        if at > rest.start && at < rest.end() {
            let (before, after) = cut_clip(&rest, at, project.alloc_clip_id());
            pieces.push(before);
            rest = after;
        }
    }
    pieces.push(rest);
    pieces
}

/// Whether `piece` (cut at the spans' edges) lies inside one of `spans`.
fn within(piece: &Clip, spans: &[(u64, u64)]) -> bool {
    spans
        .iter()
        .any(|&(from, to)| piece.start >= from && piece.start < to)
}

/// The edits that turn clip `old` into `new` (the same clip, changed).
fn clip_edits(old: &Clip, new: Clip) -> Vec<Edit> {
    let id = old.id;
    let mut props = Vec::new();
    if new.start != old.start {
        props.push(ClipProp::Start(new.start));
    }
    if new.length != old.length {
        props.push(ClipProp::Length(new.length));
    }
    if new.gain_db != old.gain_db {
        props.push(ClipProp::GainDb(new.gain_db));
    }
    if new.muted != old.muted {
        props.push(ClipProp::Muted(new.muted));
    }
    if new.name != old.name {
        props.push(ClipProp::Name(new.name));
    }
    match (&old.content, new.content) {
        (ClipContent::Audio { offset: a, .. }, ClipContent::Audio { offset: b, .. }) => {
            if *a != b {
                props.push(ClipProp::Offset(b));
            }
        }
        (
            ClipContent::Midi {
                notes: old_notes,
                controls: old_controls,
            },
            ClipContent::Midi { notes, controls },
        ) => {
            if *old_notes != notes {
                props.push(ClipProp::Notes(notes));
            }
            if *old_controls != controls {
                props.push(ClipProp::Controls(controls));
            }
        }
        _ => unreachable!("a clip keeps its kind"),
    }
    props
        .into_iter()
        .map(|prop| Edit::SetClip { id, prop })
        .collect()
}

/// Edits that replace `clip` on `track` with `pieces`, the first of which
/// keeps its ID.
fn replace_clip(clip: &Clip, track: TrackId, pieces: Vec<Clip>, edits: &mut Vec<Edit>) {
    let mut pieces = pieces.into_iter();
    if let Some(first) = pieces.next() {
        edits.extend(clip_edits(clip, first));
    }
    edits.extend(pieces.map(|piece| Edit::InsertClip {
        track,
        index: usize::MAX,
        clip: piece,
    }));
}

/// Edits that silence `spans` (timeline ranges) of `track` for every clip
/// but those in `keep`: each unmuted clip that plays there is muted there,
/// split at the spans' edges.
fn silence_others(
    project: &mut Project,
    track: TrackId,
    keep: &[ClipId],
    spans: &[(u64, u64)],
    edits: &mut Vec<Edit>,
) {
    let others: Vec<Clip> = project
        .track(track)
        .map(|t| {
            t.clips
                .iter()
                .filter(|c| !keep.contains(&c.id) && !c.muted)
                .filter(|c| {
                    spans
                        .iter()
                        .any(|&(from, to)| c.start < to && from < c.end())
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let edges = || spans.iter().flat_map(|&(from, to)| [from, to]);
    for clip in others {
        let pieces = cut_at(project, &clip, edges())
            .into_iter()
            .map(|mut piece| {
                piece.muted |= within(&piece, spans);
                piece
            })
            .collect();
        replace_clip(&clip, track, pieces, edits);
    }
}

/// Pushes the edits that put a new `clip` on `track`. As a `take`, it goes
/// in the take lane above the clips it overlaps, and (unless muted)
/// silences them where it plays.
fn insert_clip(
    project: &mut Project,
    track: TrackId,
    mut clip: Clip,
    take: bool,
    edits: &mut Vec<Edit>,
) {
    let (id, span, muted) = (clip.id, (clip.start, clip.end()), clip.muted);
    if take {
        clip.lane = project
            .track(track)
            .and_then(|t| {
                t.clips
                    .iter()
                    .filter(|c| c.start < span.1 && span.0 < c.end())
                    .map(|c| c.lane + 1)
                    .max()
            })
            .unwrap_or(0);
    }
    edits.push(Edit::InsertClip {
        track,
        index: usize::MAX,
        clip,
    });
    if take && !muted {
        silence_others(project, track, &[id], &[span], edits);
    }
}

/// A MIDI clip's notes split at `at` frames from its start: those that
/// start before, and those that start at or after, moved to start from `at`.
fn split_notes(notes: &[Note], at: u64) -> (Vec<Note>, Vec<Note>) {
    let (before, after): (Vec<Note>, Vec<Note>) = notes.iter().partition(|n| n.start < at);
    let after = after
        .into_iter()
        .map(|n| Note {
            start: n.start - at,
            ..n
        })
        .collect();
    (before, after)
}

/// A MIDI clip's controller changes split at `at` frames from its start:
/// those before, and those from `at` on, moved to start from `at` and led
/// by each controller's value at `at` (so the second part sounds the same).
fn split_controls(controls: &[ControlPoint], at: u64) -> (Vec<ControlPoint>, Vec<ControlPoint>) {
    let (before, rest): (Vec<ControlPoint>, Vec<ControlPoint>) =
        controls.iter().partition(|p| p.frame < at);
    let mut latest = std::collections::BTreeMap::new();
    for p in &before {
        latest.insert(p.controller, p.value);
    }
    for p in rest.iter().filter(|p| p.frame == at) {
        latest.remove(&p.controller);
    }
    let after = latest
        .into_iter()
        .map(|(controller, value)| ControlPoint {
            controller,
            frame: 0,
            value,
        })
        .chain(rest.into_iter().map(|p| ControlPoint {
            frame: p.frame - at,
            ..p
        }))
        .collect();
    (before, after)
}

fn default_velocity() -> u8 {
    100
}

/// A note to add to a MIDI clip.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoteSpec {
    /// MIDI pitch, 0 to 127 (60 is middle C, 69 is A 440 Hz).
    pub pitch: u8,
    /// Start in beats (quarter notes) from the clip's start.
    pub start_beats: f64,
    /// Length in beats (quarter notes).
    pub length_beats: f64,
    /// Velocity, 1 to 127. Defaults to 100.
    #[serde(default = "default_velocity")]
    pub velocity: u8,
}

impl NoteSpec {
    /// The note in a clip that starts at timeline frame `clip_start`.
    fn to_note(
        self,
        project: &mut Project,
        map: &TempoMap,
        clip_start: u64,
    ) -> Result<Note, CoreError> {
        validate_pitch(self.pitch)?;
        validate_velocity(self.velocity)?;
        check_beats("start_beats", self.start_beats)?;
        check_beats("length_beats", self.length_beats)?;
        let (start, length) = note_frames(map, clip_start, self.start_beats, self.length_beats)?;
        if length == 0 || self.length_beats <= 0.0 {
            return Err(CoreError::invalid("length_beats must be more than 0"));
        }
        Ok(Note {
            id: project.alloc_note_id(),
            pitch: self.pitch,
            velocity: self.velocity,
            start,
            length,
        })
    }
}

/// A note's start and length in frames from its clip's start, from beats.
fn note_frames(
    map: &TempoMap,
    clip_start: u64,
    start_beats: f64,
    length_beats: f64,
) -> Result<(u64, u64), CoreError> {
    let start = clip_offset(map, clip_start, start_beats)?;
    let end = clip_offset(map, clip_start, start_beats + length_beats)?;
    Ok((start, end.saturating_sub(start)))
}

/// Frames from a clip's start to `beats` into it, checking that it stays
/// on the timeline (24 hours).
fn clip_offset(map: &TempoMap, clip_start: u64, beats: f64) -> Result<u64, CoreError> {
    let frames = map.clip_frames(clip_start, beats);
    if (clip_start + frames) as f64 > MAX_POSITION_S * map.sample_rate() {
        return Err(CoreError::invalid(format!(
            "{beats} beats into the clip is past the end of the timeline (24 hours)"
        )));
    }
    Ok(frames)
}

/// Create a MIDI clip, optionally with notes. Note times are in beats
/// (quarter notes) from the clip's start, at the project tempo. Returns the
/// clip's ID, the track's ID and the new notes' IDs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddMidiClip {
    /// MIDI track to place the clip on. Defaults to a new MIDI track.
    #[serde(default)]
    pub track: Option<TrackId>,
    /// Name of the new track, when `track` is left out. Defaults to
    /// "MIDI N".
    #[serde(default)]
    pub track_name: Option<String>,
    /// Timeline position of the clip's start, in seconds. Defaults to 0.
    #[serde(default)]
    pub position_s: f64,
    /// Clip length in seconds. Defaults to whole bars covering the notes
    /// (at least one bar).
    #[serde(default)]
    pub length_s: Option<f64>,
    #[serde(default)]
    pub notes: Vec<NoteSpec>,
    /// Controller changes (pitch bend, CCs, channel pressure); see
    /// set_midi_controls.
    #[serde(default)]
    pub controls: Vec<ControlSpec>,
    /// Place the clip muted.
    #[serde(default)]
    pub muted: bool,
    /// Place the clip as a new take of what it overlaps on the track: in a
    /// take lane above those clips, which (unless this clip is muted) go
    /// silent where it covers them, split at its edges. Recording places
    /// takes this way.
    #[serde(default)]
    pub take: bool,
}

impl AddMidiClip {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let start = position_frames(project, self.position_s)?;
        let explicit = match self.length_s {
            Some(l) => {
                let length = position_frames(project, l)?;
                if length == 0 {
                    return Err(CoreError::invalid("length_s must be more than 0"));
                }
                Some(length)
            }
            None => None,
        };
        let notes = new_notes(project, &self.notes, start, 0)?;
        let map = project.tempo_map();
        let mut controls = self
            .controls
            .iter()
            .map(|c| c.to_point(&map, start))
            .collect::<Result<Vec<_>, _>>()?;
        if controls.len() > MAX_POINTS {
            return Err(CoreError::invalid(format!(
                "a clip can hold at most {MAX_POINTS} controller changes"
            )));
        }
        controls.sort_by_key(|c| c.frame);
        let length = match explicit {
            Some(length) => {
                if let Some(late) = notes.iter().find(|n| n.start >= length) {
                    return Err(CoreError::invalid(format!(
                        "a note at {:.3} beats starts after the clip ends",
                        project.tempo_map().clip_beats(start, late.start)
                    )));
                }
                length
            }
            None => {
                // Past its last note and controller change.
                let end = notes_end(&notes).max(controls.last().map_or(0, |c| c.frame + 1));
                fit_to_bars(project, start, 0, end)
            }
        };
        let mut edits = Vec::new();
        let name = match (&self.track_name, self.track) {
            (Some(name), None) => validate_name(name)?,
            _ => String::new(),
        };
        let track = target_track(project, self.track, TrackKind::Midi, &name, &mut edits)?;
        let clip = Clip {
            id: project.alloc_clip_id(),
            start,
            length,
            gain_db: 0.0,
            muted: self.muted,
            lane: 0,
            name: None,
            content: ClipContent::Midi {
                notes: sorted(notes.clone()),
                controls,
            },
        };
        let outcome = Outcome {
            track: Some(track),
            clip: Some(clip.id),
            notes: notes.iter().map(|n| n.id).collect(),
            ..Outcome::default()
        };
        insert_clip(project, track, clip, self.take, &mut edits);
        Ok((edits, outcome))
    }
}

/// Add notes to a MIDI clip. Note times are in beats (quarter notes) from
/// the clip's start. The clip is lengthened to whole bars if a note ends
/// past it. Returns the new notes' IDs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddMidiNotes {
    pub clip: ClipId,
    pub notes: Vec<NoteSpec>,
}

impl AddMidiNotes {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (current, start, length) = midi_notes(project, self.clip)?;
        if self.notes.is_empty() {
            return Err(CoreError::invalid("give at least one note"));
        }
        let added = new_notes(project, &self.notes, start, current.len())?;
        let ids = added.iter().map(|n| n.id).collect();
        let mut notes = current;
        notes.extend(added);
        let edits = set_notes(project, self.clip, start, length, notes);
        Ok((
            edits,
            Outcome {
                notes: ids,
                ..Outcome::default()
            },
        ))
    }
}

/// A change to one note. Fields left out keep their value.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoteChange {
    pub id: NoteId,
    #[serde(default)]
    pub pitch: Option<u8>,
    /// New start in beats (quarter notes) from the clip's start.
    #[serde(default)]
    pub start_beats: Option<f64>,
    #[serde(default)]
    pub length_beats: Option<f64>,
    #[serde(default)]
    pub velocity: Option<u8>,
}

/// Change notes in a MIDI clip: move them (start_beats), transpose them
/// (pitch), resize them (length_beats) or change their velocity. Times are
/// in beats (quarter notes) from the clip's start. The clip is lengthened to
/// whole bars if a note ends past it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditMidiNotes {
    pub clip: ClipId,
    pub notes: Vec<NoteChange>,
}

impl EditMidiNotes {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (mut notes, start, length) = midi_notes(project, self.clip)?;
        if self.notes.is_empty() {
            return Err(CoreError::invalid("give at least one note"));
        }
        let map = project.tempo_map();
        for change in &self.notes {
            let note = notes
                .iter_mut()
                .find(|n| n.id == change.id)
                .ok_or_else(|| {
                    CoreError::invalid(format!("clip {} has no note {}", self.clip, change.id))
                })?;
            if let Some(pitch) = change.pitch {
                validate_pitch(pitch)?;
                note.pitch = pitch;
            }
            if let Some(velocity) = change.velocity {
                validate_velocity(velocity)?;
                note.velocity = velocity;
            }
            if change.start_beats.is_none() && change.length_beats.is_none() {
                continue;
            }
            // Moved notes keep their length in beats.
            let from = map.clip_beats(start, note.start);
            let beats = map.clip_beats(start, note.end()) - from;
            let from = match change.start_beats {
                Some(b) => check_beats("start_beats", b)?,
                None => from,
            };
            let beats = match change.length_beats {
                Some(b) => check_beats("length_beats", b)?,
                None => beats,
            };
            let (at, frames) = note_frames(&map, start, from, beats)?;
            if frames == 0 || beats <= 0.0 {
                return Err(CoreError::invalid("length_beats must be more than 0"));
            }
            note.start = at;
            note.length = frames;
        }
        Ok((
            set_notes(project, self.clip, start, length, notes),
            Outcome::none(),
        ))
    }
}

/// Remove notes from a MIDI clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveMidiNotes {
    pub clip: ClipId,
    pub notes: Vec<NoteId>,
}

impl RemoveMidiNotes {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (mut notes, start, length) = midi_notes(project, self.clip)?;
        if let Some(missing) = self
            .notes
            .iter()
            .find(|id| notes.iter().all(|n| n.id != **id))
        {
            return Err(CoreError::invalid(format!(
                "clip {} has no note {missing}",
                self.clip
            )));
        }
        notes.retain(|n| !self.notes.contains(&n.id));
        Ok((
            set_notes(project, self.clip, start, length, notes),
            Outcome::none(),
        ))
    }
}

/// Move note starts in a MIDI clip to the nearest grid line, keeping their
/// lengths. The grid is in beats (quarter notes) from the clip's start:
/// 0.25 is sixteenth notes, 0.5 eighths, 1 quarters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuantizeMidiNotes {
    pub clip: ClipId,
    pub grid_beats: f64,
    /// Notes to quantize. Defaults to all of the clip's notes.
    #[serde(default)]
    pub notes: Option<Vec<NoteId>>,
}

impl QuantizeMidiNotes {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (mut notes, start, length) = midi_notes(project, self.clip)?;
        if !(self.grid_beats.is_finite() && (1.0 / 64.0..=16.0).contains(&self.grid_beats)) {
            return Err(CoreError::invalid(format!(
                "grid_beats must be between 1/64 and 16, got {}",
                self.grid_beats
            )));
        }
        let map = project.tempo_map();
        let grid = self.grid_beats;
        for note in &mut notes {
            if self.notes.as_ref().is_none_or(|ids| ids.contains(&note.id)) {
                let beat = map.clip_beats(start, note.start);
                note.start = map.clip_frames(start, (beat / grid).round() * grid);
            }
        }
        Ok((
            set_notes(project, self.clip, start, length, notes),
            Outcome::none(),
        ))
    }
}

/// A controller change to put in a MIDI clip.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlSpec {
    pub controller: Controller,
    /// When, in beats (quarter notes) from the clip's start.
    pub time_beats: f64,
    /// Pitch bend 0 to 16383 (8192 is no bend); CCs, pressure and programs
    /// 0 to 127; MIDI 2.0 per-note controllers 0 to 65535.
    pub value: u16,
}

impl ControlSpec {
    fn to_point(self, map: &TempoMap, clip_start: u64) -> Result<ControlPoint, CoreError> {
        validate_controller(self.controller, self.value)?;
        check_beats("time_beats", self.time_beats)?;
        Ok(ControlPoint {
            controller: self.controller,
            frame: clip_offset(map, clip_start, self.time_beats)?,
            value: self.value,
        })
    }
}

/// A controller value at a time, for set_midi_controls.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlValue {
    /// When, in beats (quarter notes) from the clip's start.
    pub time_beats: f64,
    pub value: u16,
}

/// Replace one controller's changes in a MIDI clip (an empty list removes
/// them). Controllers: {"kind": "pitch_bend"} (0 to 16383, 8192 is no
/// bend), {"kind": "cc", "number": 0-119} (0 to 127; 1 is the mod wheel, 7
/// volume, 11 expression, 64 the sustain pedal), {"kind":
/// "channel_pressure"} (0 to 127), {"kind": "poly_pressure", "pitch":
/// 0-127} (one key's pressure, 0 to 127) and {"kind": "program"} (program
/// change, 0 to 127). MIDI 2.0 per-note controllers act on one key and take
/// 0 to 65535: {"kind": "note_pitch_bend", "pitch": 0-127} (32768 is no
/// bend, the ends ±48 semitones), {"kind": "note_controller", "pitch",
/// "number": 0-255} (registered: 7 the note's volume and 11 its
/// expression, 65535 full; 10 pan and 74 brightness, 32768 center) and
/// {"kind": "note_assignable", "pitch", "number": 0-255}. The built-in synth
/// plays per-note bend, volume, expression and brightness; CLAP and VST3
/// plugins get them as note expressions, MIDI devices as MIDI 2.0 messages. Each value
/// holds until the next change. Times are in beats (quarter notes) from the
/// clip's start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetMidiControls {
    pub clip: ClipId,
    pub controller: Controller,
    pub points: Vec<ControlValue>,
}

impl SetMidiControls {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (_, start, _) = midi_notes(project, self.clip)?;
        // The controller itself, even with no points.
        validate_controller(self.controller, 0)?;
        let (_, clip) = project.clip(self.clip).expect("checked above");
        if self.points.len() > MAX_POINTS {
            return Err(CoreError::invalid(format!(
                "a clip can hold at most {MAX_POINTS} controller changes"
            )));
        }
        let map = project.tempo_map();
        let mut new = self
            .points
            .iter()
            .map(|p| {
                ControlSpec {
                    controller: self.controller,
                    time_beats: p.time_beats,
                    value: p.value,
                }
                .to_point(&map, start)
            })
            .collect::<Result<Vec<_>, _>>()?;
        // Sort, keeping the last of several changes at one frame.
        new.reverse();
        new.sort_by_key(|c| c.frame);
        new.dedup_by_key(|c| c.frame);
        let mut controls: Vec<ControlPoint> = clip
            .controls()
            .iter()
            .filter(|c| c.controller != self.controller)
            .copied()
            .collect();
        if controls.len() + new.len() > MAX_POINTS {
            return Err(CoreError::invalid(format!(
                "a clip can hold at most {MAX_POINTS} controller changes"
            )));
        }
        controls.extend(new);
        controls.sort_by_key(|c| c.frame);
        let edit = Edit::SetClip {
            id: self.clip,
            prop: ClipProp::Controls(controls),
        };
        Ok((vec![edit], Outcome::none()))
    }
}

fn validate_controller(controller: Controller, value: u16) -> Result<(), CoreError> {
    if let Controller::Cc { number } = controller
        && number > CC_MAX
    {
        return Err(CoreError::invalid(format!(
            "CC number must be between 0 and {CC_MAX}, got {number}"
        )));
    }
    if let Some(pitch) = controller.pitch()
        && pitch > MIDI_MAX
    {
        return Err(CoreError::invalid(format!(
            "the controller's pitch must be between 0 and {MIDI_MAX}, got {pitch}"
        )));
    }
    if value > controller.max() {
        return Err(CoreError::invalid(format!(
            "value must be between 0 and {}, got {value}",
            controller.max()
        )));
    }
    Ok(())
}

/// Change the built-in synth of a MIDI track. Fields left out keep their
/// value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetSynth {
    pub track: TrackId,
    /// Oscillator shape: "saw", "square", "triangle" or "sine".
    #[serde(default)]
    pub waveform: Option<Waveform>,
    /// Envelope attack, 0 to 10000 ms.
    #[serde(default)]
    pub attack_ms: Option<f32>,
    /// Envelope decay, 0 to 20000 ms.
    #[serde(default)]
    pub decay_ms: Option<f32>,
    /// Envelope sustain level, 0 to 1.
    #[serde(default)]
    pub sustain: Option<f32>,
    /// Envelope release, 0 to 20000 ms.
    #[serde(default)]
    pub release_ms: Option<f32>,
    /// Low-pass filter cutoff, 20 to 20000 Hz.
    #[serde(default)]
    pub cutoff_hz: Option<f32>,
    /// Filter resonance, 0 to 1.
    #[serde(default)]
    pub resonance: Option<f32>,
    /// How far the envelope opens the filter, 0 to 8 octaves.
    #[serde(default)]
    pub filter_env_octaves: Option<f32>,
}

impl SetSynth {
    /// Changes nothing yet; set the fields to change.
    pub fn new(track: TrackId) -> Self {
        Self {
            track,
            waveform: None,
            attack_ms: None,
            decay_ms: None,
            sustain: None,
            release_ms: None,
            cutoff_hz: None,
            resonance: None,
            filter_env_octaves: None,
        }
    }

    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        require_kind(project, self.track, TrackKind::Midi)?;
        let mut params: SynthParams = project
            .track(self.track)
            .and_then(|t| t.synth)
            .unwrap_or_default();
        let range = |name: &str, v: f32, min: f32, max: f32| {
            if (min..=max).contains(&v) {
                Ok(v)
            } else {
                Err(CoreError::invalid(format!(
                    "{name} must be between {min} and {max}, got {v}"
                )))
            }
        };
        let mut changed = false;
        if let Some(w) = self.waveform {
            params.waveform = w;
            changed = true;
        }
        let fields = [
            (
                "attack_ms",
                self.attack_ms,
                0.0,
                10_000.0,
                &mut params.attack_ms,
            ),
            (
                "decay_ms",
                self.decay_ms,
                0.0,
                20_000.0,
                &mut params.decay_ms,
            ),
            ("sustain", self.sustain, 0.0, 1.0, &mut params.sustain),
            (
                "release_ms",
                self.release_ms,
                0.0,
                20_000.0,
                &mut params.release_ms,
            ),
            (
                "cutoff_hz",
                self.cutoff_hz,
                20.0,
                20_000.0,
                &mut params.cutoff_hz,
            ),
            ("resonance", self.resonance, 0.0, 1.0, &mut params.resonance),
            (
                "filter_env_octaves",
                self.filter_env_octaves,
                0.0,
                8.0,
                &mut params.filter_env_octaves,
            ),
        ];
        for (name, value, min, max, field) in fields {
            if let Some(v) = value {
                *field = range(name, v, min, max)?;
                changed = true;
            }
        }
        if !changed {
            return Err(CoreError::invalid("give at least one setting to change"));
        }
        Ok(set_track(self.track, TrackProp::Synth(params)))
    }
}

/// Add a plugin to a track's insert chain (list_plugins shows the installed
/// ones). On a MIDI track, an instrument first in the chain plays the notes
/// instead of the built-in synth. Returns the new plugin slot's ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddPlugin {
    pub track: TrackId,
    /// The plugin's ID from list_plugins, e.g. "builtin:amp",
    /// "clap:org.example.reverb", "lv2:http://example.org/eq" or
    /// "vst3:5A1C7E8F3B2D4C6E9F0A1B2C3D4E5F60".
    pub plugin: String,
    /// Position in the chain (0 = first). Defaults to the end.
    #[serde(default)]
    pub index: Option<usize>,
}

impl AddPlugin {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let track = project
            .track(self.track)
            .ok_or(CoreError::TrackNotFound(self.track))?;
        if !["builtin:", "clap:", "lv2:", "vst3:"]
            .iter()
            .any(|prefix| self.plugin.starts_with(prefix))
        {
            return Err(CoreError::invalid(format!(
                "unknown plugin {:?}; plugin IDs start with builtin:, clap:, lv2: or vst3: (see list_plugins)",
                self.plugin
            )));
        }
        if track.plugins.len() >= MAX_PLUGINS {
            return Err(CoreError::invalid(format!(
                "a track can hold at most {MAX_PLUGINS} plugins"
            )));
        }
        let mut plugins = track.plugins.clone();
        let index = self.index.unwrap_or(plugins.len()).min(plugins.len());
        let id = project.alloc_plugin_id();
        plugins.insert(
            index,
            PluginSlot {
                id,
                plugin: self.plugin,
                params: Default::default(),
                bypass: false,
                sidechain: None,
                outputs: Default::default(),
            },
        );
        Ok((
            set_track(self.track, TrackProp::Plugins(plugins)).0,
            Outcome {
                track: Some(self.track),
                plugin: Some(id),
                ..Outcome::default()
            },
        ))
    }
}

/// Remove a plugin from its track, with its automation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemovePlugin {
    pub plugin: PluginId,
}

impl RemovePlugin {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (track, _) = plugin_slot(project, self.plugin)?;
        let mut plugins = track.plugins.clone();
        plugins.retain(|p| p.id != self.plugin);
        let mut edits = vec![Edit::SetTrack {
            id: track.id,
            prop: TrackProp::Plugins(plugins),
        }];
        let lanes: Vec<AutomationLane> = track
            .automation
            .iter()
            .filter(|l| !targets_plugin(&l.target, self.plugin))
            .cloned()
            .collect();
        if lanes.len() != track.automation.len() {
            edits.push(Edit::SetTrack {
                id: track.id,
                prop: TrackProp::Automation(lanes),
            });
        }
        Ok((edits, Outcome::none()))
    }
}

/// Curated 19-inch studio guitar rack signal chains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RackPreset {
    Clean,
    Crunch,
    Lead,
    Ambient,
    Blues,
    Metal,
    Jazz,
    Acoustic,
    Bass,
    BassDrive,
    Funk,
    Reggae,
    FunkDrive,
    Psychedelic,
}

/// Mount a curated studio guitar effects rack preset onto a track (replaces
/// existing rack/insert units unless `replace` is false), as one undo step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoadRackPreset {
    pub track: TrackId,
    pub preset: RackPreset,
    /// Replace the track's current chain (default true), or append to it.
    #[serde(default)]
    pub replace: Option<bool>,
}

impl LoadRackPreset {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let chain = preset_chain(self.preset)
            .into_iter()
            .map(|(plugin, params)| {
                (
                    plugin.to_owned(),
                    params.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
                )
            })
            .collect();
        mount_chain(project, self.track, chain, self.replace.unwrap_or(true))
    }
}

/// Save the builtin rack units mounted on a track (in order, with their
/// current parameters; bypass state, sidechains and any hosted external
/// plugin are not saved) as a named preset of your own, alongside the
/// curated ones load_rack_preset mounts. Saving under a name that's
/// already used replaces it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveRackPreset {
    pub track: TrackId,
    pub name: String,
}

impl SaveRackPreset {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let name = validate_name(&self.name)?;
        let track = project
            .track(self.track)
            .ok_or(CoreError::TrackNotFound(self.track))?;
        let chain: Vec<SavedRackUnit> = track
            .plugins
            .iter()
            .filter(|p| p.plugin.starts_with("builtin:"))
            .map(|p| SavedRackUnit {
                plugin: p.plugin.clone(),
                params: p.params.clone(),
            })
            .collect();
        if chain.is_empty() {
            return Err(CoreError::invalid(
                "the track has no built-in rack units to save",
            ));
        }
        let mut presets = project.rack_presets.clone();
        match presets.iter_mut().find(|p| p.name == name) {
            Some(existing) => existing.chain = chain,
            None => presets.push(SavedRackPreset { name, chain }),
        }
        Ok((vec![Edit::SetRackPresets(presets)], Outcome::none()))
    }
}

/// Remove one of your saved rack presets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveSavedRackPreset {
    pub name: String,
}

impl RemoveSavedRackPreset {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let mut presets = project.rack_presets.clone();
        let before = presets.len();
        presets.retain(|p| p.name != self.name);
        if presets.len() == before {
            return Err(CoreError::invalid(format!(
                "no saved rack preset named {:?}",
                self.name
            )));
        }
        Ok((vec![Edit::SetRackPresets(presets)], Outcome::none()))
    }
}

/// Mount one of your own saved rack presets (save_rack_preset) onto a
/// track, as one undo step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoadSavedRackPreset {
    pub track: TrackId,
    pub name: String,
    /// Replace the track's current chain (default true), or append to it.
    #[serde(default)]
    pub replace: Option<bool>,
}

impl LoadSavedRackPreset {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let saved = project
            .rack_presets
            .iter()
            .find(|p| p.name == self.name)
            .ok_or_else(|| {
                CoreError::invalid(format!("no saved rack preset named {:?}", self.name))
            })?;
        let chain = saved
            .chain
            .iter()
            .map(|u| (u.plugin.clone(), u.params.clone()))
            .collect();
        mount_chain(project, self.track, chain, self.replace.unwrap_or(true))
    }
}

/// Mounts `chain` (plugin ID, params) onto `track`, replacing its current
/// chain or appending to it, as [`LoadRackPreset`] and
/// [`LoadSavedRackPreset`] both do.
fn mount_chain(
    project: &mut Project,
    track_id: TrackId,
    chain: Vec<(String, BTreeMap<String, f64>)>,
    replace: bool,
) -> Result<(Vec<Edit>, Outcome), CoreError> {
    let track = project
        .track(track_id)
        .ok_or(CoreError::TrackNotFound(track_id))?;
    let existing = if replace { 0 } else { track.plugins.len() };
    if existing + chain.len() > MAX_PLUGINS {
        return Err(CoreError::invalid(format!(
            "a track can hold at most {MAX_PLUGINS} plugins"
        )));
    }
    let mut plugins = if replace {
        Vec::with_capacity(chain.len())
    } else {
        track.plugins.clone()
    };
    let removed: Vec<PluginId> = if replace {
        track.plugins.iter().map(|p| p.id).collect()
    } else {
        Vec::new()
    };
    let old_lanes = track.automation.clone();
    for (plugin, params) in chain {
        let id = project.alloc_plugin_id();
        plugins.push(PluginSlot {
            id,
            plugin,
            params,
            bypass: false,
            sidechain: None,
            outputs: Default::default(),
        });
    }
    let mut edits = vec![Edit::SetTrack {
        id: track_id,
        prop: TrackProp::Plugins(plugins),
    }];
    if !removed.is_empty() {
        let lanes: Vec<AutomationLane> = old_lanes
            .iter()
            .filter(|l| !removed.iter().any(|id| targets_plugin(&l.target, *id)))
            .cloned()
            .collect();
        if lanes.len() != old_lanes.len() {
            edits.push(Edit::SetTrack {
                id: track_id,
                prop: TrackProp::Automation(lanes),
            });
        }
    }
    Ok((edits, Outcome::track(track_id)))
}

fn preset_chain(preset: RackPreset) -> Vec<(&'static str, Vec<(&'static str, f64)>)> {
    match preset {
        RackPreset::Clean => vec![
            ("builtin:gate", vec![("threshold", -52.0)]),
            (
                "builtin:comp",
                vec![("threshold", -20.0), ("ratio", 3.5), ("makeup", 2.5)],
            ),
            (
                "builtin:amp",
                vec![
                    ("channel", 0.0),
                    ("gain", 3.5),
                    ("treble", 6.5),
                    ("presence", 5.5),
                ],
            ),
            (
                "builtin:cab",
                vec![("cabinet", 1.0), ("mic", 2.0), ("room", 20.0)],
            ),
            (
                "builtin:chorus",
                vec![("rate", 0.7), ("depth", 40.0), ("mix", 30.0)],
            ),
            (
                "builtin:reverb",
                vec![("mode", 1.0), ("decay", 2.0), ("mix", 20.0)],
            ),
        ],
        RackPreset::Crunch => vec![
            ("builtin:gate", vec![("threshold", -46.0)]),
            (
                "builtin:drive",
                vec![("drive", 4.0), ("tone", 5.8), ("tight", 1.0)],
            ),
            (
                "builtin:amp",
                vec![
                    ("channel", 1.0),
                    ("gain", 6.0),
                    ("mid", 6.5),
                    ("treble", 6.2),
                ],
            ),
            (
                "builtin:eq",
                vec![("low_cut", 80.0), ("high_mid_gain", 2.0)],
            ),
            ("builtin:cab", vec![("cabinet", 2.0), ("mic", 0.0)]),
            (
                "builtin:reverb",
                vec![("mode", 0.0), ("decay", 1.6), ("mix", 18.0)],
            ),
        ],
        RackPreset::Lead => vec![
            ("builtin:gate", vec![("threshold", -40.0)]),
            (
                "builtin:drive",
                vec![
                    ("drive", 6.5),
                    ("tone", 6.2),
                    ("level", 3.0),
                    ("tight", 1.0),
                ],
            ),
            (
                "builtin:amp",
                vec![
                    ("channel", 2.0),
                    ("gain", 7.8),
                    ("mid", 6.5),
                    ("treble", 7.0),
                    ("presence", 6.5),
                ],
            ),
            (
                "builtin:eq",
                vec![("low_cut", 95.0), ("mid_gain", 2.5), ("high_gain", 1.5)],
            ),
            ("builtin:cab", vec![("cabinet", 3.0), ("mic", 0.0)]),
            (
                "builtin:delay",
                vec![
                    ("sync", 3.0),
                    ("feedback", 38.0),
                    ("duck", 35.0),
                    ("mix", 28.0),
                ],
            ),
            (
                "builtin:reverb",
                vec![("mode", 2.0), ("decay", 2.8), ("mix", 22.0)],
            ),
        ],
        RackPreset::Ambient => vec![
            (
                "builtin:comp",
                vec![("threshold", -22.0), ("ratio", 5.0), ("mix", 85.0)],
            ),
            (
                "builtin:amp",
                vec![("channel", 0.0), ("gain", 3.0), ("treble", 6.0)],
            ),
            (
                "builtin:cab",
                vec![("cabinet", 0.0), ("mic", 1.0), ("room", 35.0)],
            ),
            (
                "builtin:chorus",
                vec![("depth", 65.0), ("width", 100.0), ("mix", 45.0)],
            ),
            (
                "builtin:delay",
                vec![
                    ("time", 520.0),
                    ("feedback", 55.0),
                    ("ping_pong", 85.0),
                    ("mod_depth", 30.0),
                    ("mix", 40.0),
                ],
            ),
            (
                "builtin:reverb",
                vec![("mode", 2.0), ("decay", 6.5), ("mix", 45.0)],
            ),
        ],
        RackPreset::Blues => vec![
            ("builtin:gate", vec![("threshold", -50.0)]),
            (
                "builtin:drive",
                vec![("drive", 2.5), ("tone", 5.0), ("tight", 0.0), ("mix", 70.0)],
            ),
            (
                "builtin:amp",
                vec![
                    ("channel", 1.0),
                    ("gain", 4.0),
                    ("bass", 5.5),
                    ("mid", 6.0),
                    ("treble", 5.5),
                    ("sag", 5.0),
                ],
            ),
            ("builtin:cab", vec![("cabinet", 0.0), ("mic", 1.0)]),
            (
                "builtin:reverb",
                vec![("mode", 0.0), ("decay", 2.2), ("mix", 24.0)],
            ),
        ],
        RackPreset::Metal => vec![
            ("builtin:gate", vec![("threshold", -36.0), ("hold", 15.0)]),
            (
                "builtin:drive",
                vec![
                    ("drive", 8.0),
                    ("tone", 5.0),
                    ("tight", 1.0),
                    ("mix", 100.0),
                ],
            ),
            (
                "builtin:amp",
                vec![
                    ("channel", 3.0),
                    ("gain", 9.2),
                    ("bass", 4.5),
                    ("mid", 3.5),
                    ("treble", 7.0),
                    ("presence", 6.5),
                ],
            ),
            (
                "builtin:eq",
                vec![
                    ("low_cut", 100.0),
                    ("mid_gain", -3.0),
                    ("high_mid_gain", 2.5),
                ],
            ),
            ("builtin:cab", vec![("cabinet", 2.0), ("mic", 2.0)]),
            (
                "builtin:reverb",
                vec![("mode", 0.0), ("decay", 1.0), ("mix", 10.0)],
            ),
        ],
        RackPreset::Jazz => vec![
            ("builtin:gate", vec![("threshold", -54.0)]),
            (
                "builtin:comp",
                vec![("threshold", -24.0), ("ratio", 2.5), ("makeup", 2.0)],
            ),
            (
                "builtin:amp",
                vec![
                    ("channel", 0.0),
                    ("gain", 2.5),
                    ("bass", 6.0),
                    ("mid", 5.0),
                    ("treble", 4.5),
                ],
            ),
            ("builtin:cab", vec![("cabinet", 1.0), ("mic", 1.0)]),
            (
                "builtin:chorus",
                vec![("rate", 0.4), ("depth", 25.0), ("mix", 18.0)],
            ),
            (
                "builtin:reverb",
                vec![("mode", 1.0), ("decay", 1.8), ("mix", 16.0)],
            ),
        ],
        RackPreset::Acoustic => vec![
            ("builtin:gate", vec![("threshold", -48.0)]),
            (
                "builtin:comp",
                vec![("threshold", -22.0), ("ratio", 3.0), ("makeup", 3.0)],
            ),
            (
                "builtin:eq",
                vec![
                    ("low_cut", 90.0),
                    ("mid_gain", -1.5),
                    ("high_mid_gain", 2.0),
                ],
            ),
            (
                "builtin:chorus",
                vec![
                    ("rate", 0.5),
                    ("depth", 20.0),
                    ("width", 60.0),
                    ("mix", 15.0),
                ],
            ),
            (
                "builtin:reverb",
                vec![("mode", 1.0), ("decay", 2.4), ("mix", 28.0)],
            ),
        ],
        RackPreset::Bass => vec![
            ("builtin:gate", vec![("threshold", -52.0)]),
            (
                "builtin:comp",
                vec![
                    ("threshold", -24.0),
                    ("ratio", 4.0),
                    ("attack", 8.0),
                    ("release", 90.0),
                    ("makeup", 3.0),
                ],
            ),
            (
                "builtin:amp",
                vec![
                    ("channel", 0.0),
                    ("gain", 3.0),
                    ("bass", 7.0),
                    ("mid", 5.5),
                    ("treble", 4.0),
                    ("presence", 3.5),
                ],
            ),
            (
                "builtin:eq",
                vec![("low_cut", 30.0), ("low_gain", 2.0), ("mid_gain", 1.0)],
            ),
            (
                "builtin:cab",
                vec![
                    ("cabinet", 4.0),
                    ("mic", 1.0),
                    ("low_cut", 35.0),
                    ("high_cut", 4500.0),
                ],
            ),
        ],
        RackPreset::BassDrive => vec![
            ("builtin:gate", vec![("threshold", -46.0)]),
            (
                "builtin:comp",
                vec![("threshold", -22.0), ("ratio", 5.0), ("makeup", 2.5)],
            ),
            (
                "builtin:drive",
                vec![("drive", 3.0), ("tone", 4.5), ("tight", 1.0), ("mix", 65.0)],
            ),
            (
                "builtin:amp",
                vec![
                    ("channel", 1.0),
                    ("gain", 5.0),
                    ("bass", 7.0),
                    ("mid", 6.0),
                    ("treble", 4.5),
                    ("presence", 4.0),
                ],
            ),
            (
                "builtin:eq",
                vec![("low_cut", 30.0), ("low_gain", 1.5), ("mid_gain", 1.5)],
            ),
            (
                "builtin:cab",
                vec![
                    ("cabinet", 5.0),
                    ("mic", 1.0),
                    ("low_cut", 35.0),
                    ("high_cut", 5000.0),
                ],
            ),
        ],
        // Clean touch-wah: no drive or amp dirt, just the envelope filter
        // quacking over a clean tone.
        RackPreset::Funk => vec![
            (
                "builtin:comp",
                vec![("threshold", -22.0), ("ratio", 4.0), ("makeup", 2.0)],
            ),
            (
                "builtin:autowah",
                vec![
                    ("sensitivity", 70.0),
                    ("freq_low", 280.0),
                    ("freq_high", 2600.0),
                    ("q", 3.5),
                    ("attack", 2.0),
                    ("release", 90.0),
                    ("direction", 0.0),
                ],
            ),
            (
                "builtin:amp",
                vec![
                    ("channel", 0.0),
                    ("gain", 3.0),
                    ("treble", 6.0),
                    ("presence", 5.5),
                ],
            ),
            ("builtin:cab", vec![("cabinet", 1.0), ("mic", 0.0)]),
        ],
        // A mellower, slower clean wah with chorus and a touch of reverb.
        RackPreset::Reggae => vec![
            (
                "builtin:comp",
                vec![("threshold", -24.0), ("ratio", 3.0), ("makeup", 2.0)],
            ),
            (
                "builtin:autowah",
                vec![
                    ("sensitivity", 55.0),
                    ("freq_low", 220.0),
                    ("freq_high", 1600.0),
                    ("q", 2.5),
                    ("attack", 6.0),
                    ("release", 220.0),
                    ("direction", 0.0),
                    ("mix", 85.0),
                ],
            ),
            (
                "builtin:amp",
                vec![
                    ("channel", 0.0),
                    ("gain", 2.5),
                    ("bass", 6.0),
                    ("treble", 5.0),
                ],
            ),
            (
                "builtin:chorus",
                vec![("rate", 0.5), ("depth", 35.0), ("mix", 25.0)],
            ),
            (
                "builtin:reverb",
                vec![("mode", 1.0), ("decay", 1.8), ("mix", 18.0)],
            ),
        ],
        // Wah into overdrive: the envelope filter shapes the signal before
        // it hits the dirt, like a wah pedal in front of a driven amp.
        RackPreset::FunkDrive => vec![
            ("builtin:gate", vec![("threshold", -44.0)]),
            (
                "builtin:autowah",
                vec![
                    ("sensitivity", 65.0),
                    ("freq_low", 300.0),
                    ("freq_high", 2400.0),
                    ("q", 3.0),
                    ("attack", 2.5),
                    ("release", 100.0),
                    ("direction", 0.0),
                ],
            ),
            (
                "builtin:drive",
                vec![("drive", 3.5), ("tone", 5.5), ("tight", 1.0), ("mix", 80.0)],
            ),
            (
                "builtin:amp",
                vec![
                    ("channel", 1.0),
                    ("gain", 5.5),
                    ("mid", 6.5),
                    ("treble", 6.0),
                ],
            ),
            ("builtin:cab", vec![("cabinet", 2.0), ("mic", 0.0)]),
        ],
        // Hendrix-style fuzz-wah: the wah comes after the dirt for a
        // resonant, vocal sweep, with a wide high-Q range and the filter
        // opening on quiet passages (Down) for a more unpredictable swirl.
        RackPreset::Psychedelic => vec![
            ("builtin:gate", vec![("threshold", -40.0)]),
            (
                "builtin:drive",
                vec![
                    ("drive", 7.5),
                    ("tone", 6.0),
                    ("tight", 0.0),
                    ("mix", 100.0),
                ],
            ),
            (
                "builtin:autowah",
                vec![
                    ("sensitivity", 75.0),
                    ("freq_low", 350.0),
                    ("freq_high", 3200.0),
                    ("q", 6.0),
                    ("attack", 3.0),
                    ("release", 140.0),
                    ("direction", 1.0),
                ],
            ),
            (
                "builtin:amp",
                vec![
                    ("channel", 2.0),
                    ("gain", 7.0),
                    ("mid", 6.0),
                    ("treble", 6.5),
                    ("presence", 6.0),
                ],
            ),
            (
                "builtin:delay",
                vec![("sync", 2.0), ("feedback", 30.0), ("mix", 20.0)],
            ),
            (
                "builtin:reverb",
                vec![("mode", 2.0), ("decay", 3.5), ("mix", 25.0)],
            ),
        ],
    }
}

/// `builtin:drums`' plugin ID (see `dawkey-plugins`' `builtin::DRUMS_ID`,
/// which must stay in sync with this literal).
const DRUMS_PLUGIN_ID: &str = "builtin:drums";

/// General MIDI drum-map note numbers builtin:drums' eight pads answer to,
/// and the parameter-ID slug each pad's tone/decay/level params use (see
/// `dawkey-plugins`' `builtin::drum_plugin_info`, which must stay in sync):
/// Kick, Snare, Closed Hat, Open Hat, Tom, Clap, Crash, Ride.
const DRUM_PAD_NOTES: [u8; 8] = [36, 38, 42, 46, 45, 39, 49, 51];
const DRUM_PAD_SLUGS: [&str; 8] = [
    "kick",
    "snare",
    "closed_hat",
    "open_hat",
    "tom",
    "clap",
    "crash",
    "ride",
];

fn drum_kit_params(pads: [(f64, f64, f64); 8]) -> BTreeMap<String, f64> {
    let mut params = BTreeMap::new();
    for (i, slug) in DRUM_PAD_SLUGS.iter().enumerate() {
        let (tone, decay_ms, level_db) = pads[i];
        params.insert(format!("{slug}_tone"), tone);
        params.insert(format!("{slug}_decay"), decay_ms);
        params.insert(format!("{slug}_level"), level_db);
    }
    params
}

/// Curated drum kits: a synthesized character per pad (tone, decay_ms,
/// level_db), plus a short processing chain mounted after the instrument
/// (a glue compressor, an EQ carve, and either a touch of room reverb or,
/// for LoFi, a touch of saturation instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DrumKit {
    Kit808,
    Kit909,
    /// A clean, natural-sounding synthesized character (not to be
    /// confused with the `acoustic_vcsl` *sample* kit, a real recording).
    Natural,
    /// A darkened, saturated synthesized character (not to be confused
    /// with the `lofi_virtuosity` *sample* kit, a real recording).
    Tape,
    Trap,
    Metal,
}

#[allow(clippy::type_complexity)]
fn drum_kit_spec(
    kit: DrumKit,
) -> (
    [(f64, f64, f64); 8],
    Vec<(&'static str, Vec<(&'static str, f64)>)>,
) {
    match kit {
        DrumKit::Kit808 => (
            [
                (0.6, 420.0, 1.0),
                (0.4, 150.0, 0.0),
                (0.3, 55.0, -4.0),
                (0.3, 300.0, -6.0),
                (0.5, 260.0, -2.0),
                (0.45, 120.0, -2.0),
                (0.4, 1300.0, -7.0),
                (0.4, 850.0, -8.0),
            ],
            vec![
                (
                    "builtin:comp",
                    vec![("threshold", -18.0), ("ratio", 3.0), ("makeup", 2.0)],
                ),
                ("builtin:eq", vec![("low_cut", 40.0), ("low_gain", 2.0)]),
                (
                    "builtin:reverb",
                    vec![("mode", 0.0), ("decay", 1.0), ("mix", 8.0)],
                ),
            ],
        ),
        DrumKit::Kit909 => (
            [
                (0.7, 300.0, 1.0),
                (0.65, 170.0, 1.0),
                (0.6, 60.0, -3.0),
                (0.6, 280.0, -5.0),
                (0.55, 230.0, -2.0),
                (0.6, 110.0, -1.0),
                (0.55, 1200.0, -6.0),
                (0.5, 800.0, -7.0),
            ],
            vec![
                (
                    "builtin:comp",
                    vec![("threshold", -16.0), ("ratio", 4.0), ("makeup", 2.5)],
                ),
                (
                    "builtin:eq",
                    vec![("low_cut", 50.0), ("high_mid_gain", 2.0)],
                ),
                (
                    "builtin:reverb",
                    vec![("mode", 1.0), ("decay", 1.4), ("mix", 12.0)],
                ),
            ],
        ),
        DrumKit::Natural => (
            [
                (0.4, 280.0, 0.0),
                (0.3, 220.0, 0.0),
                (0.45, 80.0, -3.0),
                (0.45, 340.0, -5.0),
                (0.6, 320.0, -1.0),
                (0.5, 150.0, -2.0),
                (0.45, 1600.0, -5.0),
                (0.4, 1000.0, -6.0),
            ],
            vec![
                (
                    "builtin:comp",
                    vec![("threshold", -20.0), ("ratio", 2.5), ("makeup", 1.5)],
                ),
                (
                    "builtin:eq",
                    vec![("low_mid_gain", 1.0), ("high_gain", 1.0)],
                ),
                (
                    "builtin:reverb",
                    vec![("mode", 2.0), ("decay", 2.8), ("mix", 30.0)],
                ),
            ],
        ),
        DrumKit::Tape => (
            [
                (0.3, 300.0, -1.0),
                (0.2, 160.0, -2.0),
                (0.15, 65.0, -6.0),
                (0.15, 260.0, -8.0),
                (0.25, 240.0, -3.0),
                (0.2, 110.0, -4.0),
                (0.2, 1100.0, -9.0),
                (0.2, 750.0, -9.0),
            ],
            vec![
                (
                    "builtin:comp",
                    vec![("threshold", -18.0), ("ratio", 3.0), ("makeup", 1.5)],
                ),
                ("builtin:eq", vec![("low_cut", 30.0), ("high_gain", -4.0)]),
                (
                    "builtin:drive",
                    vec![
                        ("drive", 2.0),
                        ("tone", 3.0),
                        ("level", 0.0),
                        ("tight", 0.0),
                        ("mix", 25.0),
                    ],
                ),
            ],
        ),
        DrumKit::Trap => (
            [
                (0.55, 260.0, 1.0),
                (0.5, 150.0, 0.0),
                (0.5, 140.0, -3.0),
                (0.5, 380.0, -5.0),
                (0.45, 220.0, -2.0),
                (0.55, 130.0, -1.0),
                (0.45, 1200.0, -6.0),
                (0.4, 800.0, -7.0),
            ],
            vec![
                (
                    "builtin:comp",
                    vec![("threshold", -14.0), ("ratio", 5.0), ("makeup", 3.0)],
                ),
                ("builtin:eq", vec![("low_cut", 35.0), ("low_gain", 2.0)]),
                (
                    "builtin:reverb",
                    vec![("mode", 0.0), ("decay", 0.8), ("mix", 6.0)],
                ),
            ],
        ),
        DrumKit::Metal => (
            [
                (0.65, 200.0, 2.0),
                (0.6, 140.0, 1.0),
                (0.5, 50.0, -2.0),
                (0.45, 220.0, -4.0),
                (0.55, 200.0, -1.0),
                (0.5, 100.0, -1.0),
                (0.45, 1100.0, -5.0),
                (0.4, 750.0, -6.0),
            ],
            vec![
                (
                    "builtin:gate",
                    vec![
                        ("threshold", -35.0),
                        ("floor", -70.0),
                        ("hysteresis", 3.0),
                        ("attack", 0.5),
                        ("hold", 20.0),
                        ("release", 60.0),
                    ],
                ),
                (
                    "builtin:comp",
                    vec![
                        ("threshold", -22.0),
                        ("ratio", 6.0),
                        ("attack", 2.0),
                        ("makeup", 3.0),
                    ],
                ),
                (
                    "builtin:eq",
                    vec![
                        ("low_cut", 50.0),
                        ("low_gain", 3.0),
                        ("mid_gain", -4.0),
                        ("high_mid_gain", 3.0),
                        ("high_gain", 2.0),
                    ],
                ),
                (
                    "builtin:drive",
                    vec![
                        ("drive", 4.0),
                        ("tone", 6.0),
                        ("level", 0.0),
                        ("tight", 1.0),
                        ("mix", 35.0),
                    ],
                ),
            ],
        ),
    }
}

/// Builds `builtin:drums` plus `kit`'s processing chain as a `mount_chain`
/// input (plugin ID, params).
fn drum_kit_chain(kit: DrumKit) -> Vec<(String, BTreeMap<String, f64>)> {
    let (pads, fx) = drum_kit_spec(kit);
    let mut chain = vec![(DRUMS_PLUGIN_ID.to_owned(), drum_kit_params(pads))];
    chain.extend(fx.into_iter().map(|(id, params)| {
        (
            id.to_owned(),
            params.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        )
    }));
    chain
}

/// Add a MIDI track already set up as a drum machine: builtin:drums (the
/// synthesized drum kit instrument) loaded with a kit preset's sound and
/// processing chain, and one empty one-bar MIDI clip ready for step
/// programming (see apply_drum_pattern, add_midi_notes). Returns the new
/// track's ID and the clip's ID.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddDrumTrack {
    /// Track name. Defaults to "Drums N".
    #[serde(default)]
    pub name: Option<String>,
    /// Starting kit. Defaults to Kit808.
    #[serde(default)]
    pub kit: Option<DrumKit>,
}

impl AddDrumTrack {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let name = match self.name {
            Some(name) => validate_name(&name)?,
            None => (1..)
                .map(|n| format!("Drums {n}"))
                .find(|name| project.tracks.iter().all(|t| &t.name != name))
                .expect("unbounded range always yields a free name"),
        };
        let id = project.alloc_track_id();
        let track = Track::new(id, name, TrackKind::Midi);
        let mut edits = vec![Edit::InsertTrack {
            index: project.tracks.len(),
            track,
        }];
        let mut plugins = Vec::new();
        for (plugin, params) in drum_kit_chain(self.kit.unwrap_or(DrumKit::Kit808)) {
            plugins.push(PluginSlot {
                id: project.alloc_plugin_id(),
                plugin,
                params,
                bypass: false,
                sidechain: None,
                outputs: Default::default(),
            });
        }
        edits.push(Edit::SetTrack {
            id,
            prop: TrackProp::Plugins(plugins),
        });
        let length = fit_to_bars(project, 0, 0, 0);
        let clip = Clip {
            id: project.alloc_clip_id(),
            start: 0,
            length,
            gain_db: 0.0,
            muted: false,
            lane: 0,
            name: None,
            content: ClipContent::Midi {
                notes: Vec::new(),
                controls: Vec::new(),
            },
        };
        let clip_id = clip.id;
        insert_clip(project, id, clip, false, &mut edits);
        Ok((
            edits,
            Outcome {
                track: Some(id),
                clip: Some(clip_id),
                ..Outcome::default()
            },
        ))
    }
}

/// Mount a drum kit preset's sound and processing chain (see
/// add_drum_track) onto a MIDI track, replacing its current plugin chain,
/// as one undo step.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplyDrumKit {
    pub track: TrackId,
    pub kit: DrumKit,
}

impl ApplyDrumKit {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let track = project
            .track(self.track)
            .ok_or(CoreError::TrackNotFound(self.track))?;
        if track.kind != TrackKind::Midi {
            return Err(CoreError::invalid(format!(
                "track {} is not a MIDI track (add_drum_track makes one)",
                self.track
            )));
        }
        let chain = drum_kit_chain(self.kit);
        if chain.len() > MAX_PLUGINS {
            return Err(CoreError::invalid(format!(
                "a track can hold at most {MAX_PLUGINS} plugins"
            )));
        }
        // Keep the instrument's existing slot ID and per-pad bus routing,
        // so a sample kit chosen for it (plugin_states, outside this Edit)
        // and any pads patched to buses survive a kit change; only the
        // processing chain after it gets fresh IDs.
        let existing_drums = track
            .plugins
            .first()
            .filter(|p| p.plugin == DRUMS_PLUGIN_ID)
            .map(|p| (p.id, p.outputs.clone()));
        let existing_drums_id = existing_drums.as_ref().map(|(id, _)| *id);
        let removed: Vec<PluginId> = track
            .plugins
            .iter()
            .filter(|p| Some(p.id) != existing_drums_id)
            .map(|p| p.id)
            .collect();
        let old_lanes = track.automation.clone();

        let mut plugins = Vec::with_capacity(chain.len());
        for (i, (plugin, params)) in chain.into_iter().enumerate() {
            let (id, outputs) = if i == 0 {
                match existing_drums.clone() {
                    Some((id, outputs)) => (id, outputs),
                    None => (project.alloc_plugin_id(), Default::default()),
                }
            } else {
                (project.alloc_plugin_id(), Default::default())
            };
            plugins.push(PluginSlot {
                id,
                plugin,
                params,
                bypass: false,
                sidechain: None,
                outputs,
            });
        }

        let mut edits = vec![Edit::SetTrack {
            id: self.track,
            prop: TrackProp::Plugins(plugins),
        }];
        if !removed.is_empty() {
            let lanes: Vec<AutomationLane> = old_lanes
                .iter()
                .filter(|l| !removed.iter().any(|id| targets_plugin(&l.target, *id)))
                .cloned()
                .collect();
            if lanes.len() != old_lanes.len() {
                edits.push(Edit::SetTrack {
                    id: self.track,
                    prop: TrackProp::Automation(lanes),
                });
            }
        }
        Ok((edits, Outcome::track(self.track)))
    }
}

/// Curated drum step patterns (4/4 grooves, fills, and non-4/4 meter grooves).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DrumPattern {
    FourOnTheFloor,
    BoomBap,
    Trap,
    HouseShuffle,
    DrumAndBass,
    Rock,
    Metal,
    Funk,
    JazzSwing,
    BossaNova,
    ReggaeOneDrop,
    Disco,
    Techno,
    HalfTime,
    Afrobeat,
    Waltz,
    SixEightBlues,
    TakeFive,
    ProgSevenEight,
    NineEightCompound,
    TwelveEightShuffle,
    FillSnareRoll,
    FillTomCascade,
    FillTrapRoll,
    FillBreakbeat,
    FillDoubleKick,
    FillJazzTurnaround,
}

/// Time signature required by a meter-specific curated pattern, if any.
fn pattern_time_signature(pattern: DrumPattern) -> Option<TimeSignature> {
    let (numerator, denominator) = match pattern {
        DrumPattern::Waltz => (3, 4),
        DrumPattern::SixEightBlues => (6, 8),
        DrumPattern::TakeFive => (5, 4),
        DrumPattern::ProgSevenEight => (7, 8),
        DrumPattern::NineEightCompound => (9, 8),
        DrumPattern::TwelveEightShuffle => (12, 8),
        _ => return None,
    };
    Some(TimeSignature {
        numerator,
        denominator,
    })
}

fn is_fill_pattern(pattern: DrumPattern) -> bool {
    matches!(
        pattern,
        DrumPattern::FillSnareRoll
            | DrumPattern::FillTomCascade
            | DrumPattern::FillTrapRoll
            | DrumPattern::FillBreakbeat
            | DrumPattern::FillDoubleKick
            | DrumPattern::FillJazzTurnaround
    )
}

fn step(pad_index: usize, step: u8, velocity: u8) -> DrumStep {
    DrumStep {
        pad: DRUM_PAD_NOTES[pad_index],
        step,
        velocity,
    }
}

/// Pad indices: 0 Kick, 1 Snare, 2 Closed Hat, 3 Open Hat, 4 Tom, 5 Clap, 6
/// Crash, 7 Ride.
fn curated_pattern_steps(pattern: DrumPattern) -> Vec<DrumStep> {
    match pattern {
        DrumPattern::FourOnTheFloor => vec![
            step(0, 0, 115),
            step(0, 4, 115),
            step(0, 8, 115),
            step(0, 12, 115),
            step(5, 4, 100),
            step(5, 12, 100),
            step(2, 2, 80),
            step(2, 6, 80),
            step(2, 10, 80),
            step(2, 14, 80),
        ],
        DrumPattern::BoomBap => vec![
            step(0, 0, 115),
            step(0, 10, 110),
            step(1, 4, 110),
            step(1, 12, 110),
            step(2, 0, 70),
            step(2, 2, 65),
            step(2, 4, 70),
            step(2, 6, 65),
            step(2, 8, 70),
            step(2, 10, 65),
            step(2, 12, 70),
            step(2, 14, 65),
        ],
        DrumPattern::Trap => vec![
            step(0, 0, 118),
            step(0, 7, 105),
            step(0, 10, 110),
            step(5, 4, 105),
            step(5, 12, 105),
            step(2, 0, 70),
            step(2, 1, 60),
            step(2, 2, 95),
            step(2, 3, 60),
            step(2, 4, 70),
            step(2, 5, 60),
            step(2, 6, 95),
            step(2, 7, 60),
            step(2, 8, 70),
            step(2, 9, 60),
            step(2, 10, 95),
            step(2, 11, 60),
            step(2, 12, 70),
            step(2, 13, 60),
            step(2, 14, 95),
            step(2, 15, 60),
        ],
        DrumPattern::HouseShuffle => vec![
            step(0, 0, 115),
            step(0, 4, 115),
            step(0, 8, 115),
            step(0, 12, 115),
            step(5, 4, 95),
            step(5, 12, 95),
            step(3, 2, 90),
            step(3, 6, 90),
            step(3, 10, 90),
            step(3, 14, 90),
        ],
        DrumPattern::DrumAndBass => vec![
            step(0, 0, 118),
            step(0, 10, 112),
            step(1, 4, 115),
            step(1, 12, 115),
            step(1, 7, 60),
            step(2, 0, 70),
            step(2, 2, 70),
            step(2, 4, 70),
            step(2, 6, 70),
            step(2, 8, 70),
            step(2, 10, 70),
            step(2, 12, 70),
            step(2, 14, 70),
        ],
        DrumPattern::Rock => vec![
            step(6, 0, 110),
            step(0, 0, 118),
            step(0, 6, 100),
            step(0, 8, 115),
            step(1, 4, 115),
            step(1, 12, 118),
            step(1, 15, 65),
            step(2, 2, 80),
            step(2, 4, 85),
            step(2, 6, 80),
            step(2, 8, 85),
            step(2, 10, 80),
            step(2, 12, 85),
            step(3, 14, 95),
        ],
        DrumPattern::Metal => vec![
            step(6, 0, 118),
            step(0, 0, 120),
            step(0, 1, 105),
            step(0, 2, 112),
            step(0, 3, 105),
            step(0, 6, 112),
            step(0, 7, 105),
            step(0, 8, 120),
            step(0, 9, 105),
            step(0, 10, 112),
            step(0, 11, 105),
            step(0, 14, 112),
            step(0, 15, 108),
            step(1, 4, 122),
            step(1, 12, 122),
            step(7, 0, 95),
            step(7, 4, 95),
            step(7, 8, 95),
            step(7, 12, 95),
        ],
        DrumPattern::Funk => vec![
            step(0, 0, 118),
            step(0, 2, 95),
            step(0, 10, 110),
            step(0, 13, 92),
            step(1, 4, 115),
            step(1, 7, 52),
            step(1, 9, 55),
            step(1, 12, 118),
            step(1, 15, 58),
            step(2, 0, 85),
            step(2, 2, 70),
            step(2, 4, 85),
            step(2, 6, 75),
            step(2, 8, 85),
            step(2, 10, 75),
            step(2, 12, 85),
            step(3, 14, 90),
        ],
        DrumPattern::JazzSwing => vec![
            step(7, 0, 95),
            step(7, 3, 72),
            step(7, 4, 90),
            step(7, 8, 95),
            step(7, 11, 72),
            step(7, 12, 90),
            step(2, 4, 80),
            step(2, 12, 80),
            step(0, 0, 62),
            step(0, 8, 58),
            step(1, 7, 65),
            step(1, 14, 72),
        ],
        DrumPattern::BossaNova => vec![
            step(0, 0, 105),
            step(0, 3, 82),
            step(0, 4, 100),
            step(0, 7, 82),
            step(0, 8, 105),
            step(0, 11, 82),
            step(0, 12, 100),
            step(0, 15, 82),
            step(5, 0, 85),
            step(5, 3, 85),
            step(5, 6, 85),
            step(5, 10, 85),
            step(5, 12, 85),
            step(2, 0, 68),
            step(2, 2, 62),
            step(2, 4, 68),
            step(2, 6, 62),
            step(2, 8, 68),
            step(2, 10, 62),
            step(2, 12, 68),
            step(2, 14, 62),
        ],
        DrumPattern::ReggaeOneDrop => vec![
            step(0, 8, 118),
            step(1, 8, 108),
            step(5, 8, 95),
            step(2, 0, 75),
            step(2, 2, 88),
            step(2, 4, 75),
            step(2, 6, 88),
            step(2, 8, 75),
            step(2, 10, 88),
            step(2, 12, 75),
            step(3, 14, 92),
            step(4, 15, 78),
        ],
        DrumPattern::Disco => vec![
            step(0, 0, 116),
            step(0, 4, 116),
            step(0, 8, 116),
            step(0, 12, 116),
            step(1, 4, 110),
            step(1, 12, 110),
            step(5, 4, 95),
            step(5, 12, 95),
            step(2, 0, 80),
            step(2, 1, 65),
            step(3, 2, 95),
            step(2, 4, 80),
            step(2, 5, 65),
            step(3, 6, 95),
            step(2, 8, 80),
            step(2, 9, 65),
            step(3, 10, 95),
            step(2, 12, 80),
            step(2, 13, 65),
            step(3, 14, 95),
        ],
        DrumPattern::Techno => vec![
            step(0, 0, 122),
            step(0, 4, 122),
            step(0, 8, 122),
            step(0, 12, 122),
            step(0, 15, 78),
            step(5, 4, 105),
            step(5, 12, 105),
            step(2, 0, 65),
            step(2, 1, 60),
            step(3, 2, 95),
            step(2, 3, 65),
            step(2, 4, 65),
            step(2, 5, 60),
            step(3, 6, 95),
            step(2, 7, 65),
            step(2, 8, 65),
            step(2, 9, 60),
            step(3, 10, 95),
            step(2, 11, 65),
            step(2, 12, 65),
            step(2, 13, 60),
            step(3, 14, 95),
            step(2, 15, 70),
            step(7, 2, 75),
            step(7, 6, 75),
            step(7, 10, 75),
            step(7, 14, 82),
        ],
        DrumPattern::HalfTime => vec![
            step(6, 0, 100),
            step(0, 0, 120),
            step(0, 11, 105),
            step(1, 8, 120),
            step(5, 8, 105),
            step(1, 15, 62),
            step(2, 0, 80),
            step(2, 2, 72),
            step(2, 4, 85),
            step(2, 6, 72),
            step(2, 8, 80),
            step(2, 10, 72),
            step(2, 12, 85),
            step(3, 14, 90),
        ],
        DrumPattern::Afrobeat => vec![
            step(0, 0, 115),
            step(0, 4, 110),
            step(0, 8, 115),
            step(0, 12, 110),
            step(1, 3, 98),
            step(1, 6, 105),
            step(1, 11, 98),
            step(1, 14, 108),
            step(4, 7, 85),
            step(4, 15, 90),
            step(2, 0, 75),
            step(2, 2, 70),
            step(2, 4, 75),
            step(2, 6, 70),
            step(2, 8, 75),
            step(3, 10, 88),
            step(2, 12, 75),
            step(2, 14, 70),
        ],
        DrumPattern::FillSnareRoll => vec![
            step(0, 0, 118),
            step(0, 4, 110),
            step(0, 8, 112),
            step(0, 12, 116),
            step(2, 0, 80),
            step(2, 2, 75),
            step(1, 4, 55),
            step(1, 5, 60),
            step(1, 6, 68),
            step(1, 7, 74),
            step(1, 8, 82),
            step(1, 9, 88),
            step(1, 10, 95),
            step(1, 11, 102),
            step(1, 12, 110),
            step(1, 13, 115),
            step(1, 14, 122),
            step(1, 15, 127),
            step(5, 12, 100),
            step(5, 14, 112),
            step(6, 15, 120),
        ],
        DrumPattern::FillTomCascade => vec![
            step(0, 0, 118),
            step(6, 0, 105),
            step(1, 2, 100),
            step(1, 3, 108),
            step(1, 4, 112),
            step(1, 5, 95),
            step(4, 6, 108),
            step(4, 7, 100),
            step(1, 8, 115),
            step(4, 9, 105),
            step(4, 10, 112),
            step(4, 11, 118),
            step(0, 12, 118),
            step(4, 13, 115),
            step(4, 14, 122),
            step(1, 15, 124),
            step(6, 15, 118),
        ],
        DrumPattern::FillTrapRoll => vec![
            step(0, 0, 120),
            step(0, 2, 105),
            step(0, 3, 112),
            step(5, 4, 110),
            step(0, 7, 108),
            step(0, 8, 118),
            step(1, 9, 80),
            step(1, 10, 92),
            step(1, 11, 104),
            step(5, 12, 115),
            step(1, 12, 112),
            step(1, 13, 118),
            step(4, 14, 115),
            step(1, 15, 124),
            step(2, 0, 80),
            step(2, 1, 75),
            step(2, 4, 85),
            step(2, 5, 75),
            step(2, 6, 95),
            step(3, 8, 100),
            step(6, 14, 115),
        ],
        DrumPattern::FillBreakbeat => vec![
            step(6, 0, 110),
            step(0, 0, 118),
            step(1, 2, 72),
            step(1, 4, 116),
            step(0, 5, 105),
            step(4, 6, 98),
            step(1, 7, 68),
            step(4, 8, 108),
            step(1, 9, 85),
            step(0, 10, 115),
            step(1, 11, 92),
            step(6, 12, 112),
            step(1, 12, 118),
            step(1, 13, 88),
            step(1, 14, 112),
            step(1, 15, 122),
        ],
        DrumPattern::FillDoubleKick => vec![
            step(6, 0, 118),
            step(0, 0, 122),
            step(0, 1, 115),
            step(1, 2, 118),
            step(1, 3, 110),
            step(0, 4, 122),
            step(0, 5, 115),
            step(4, 6, 118),
            step(4, 7, 112),
            step(6, 8, 115),
            step(0, 8, 122),
            step(0, 9, 115),
            step(1, 10, 120),
            step(4, 11, 115),
            step(0, 12, 124),
            step(0, 13, 118),
            step(4, 14, 122),
            step(1, 15, 126),
            step(6, 14, 120),
        ],
        DrumPattern::FillJazzTurnaround => vec![
            step(7, 0, 95),
            step(1, 0, 92),
            step(0, 2, 85),
            step(1, 3, 102),
            step(4, 4, 98),
            step(1, 6, 105),
            step(0, 7, 88),
            step(4, 8, 108),
            step(0, 10, 95),
            step(4, 11, 112),
            step(1, 12, 115),
            step(0, 13, 100),
            step(0, 14, 120),
            step(1, 14, 120),
            step(6, 14, 115),
            step(7, 15, 85),
        ],
        // 3/4 Waltz (12 16th steps): oom-pah-pah on Kick (beat 1) + Snare/Hat (beats 2, 3).
        DrumPattern::Waltz => vec![
            step(0, 0, 115),
            step(7, 0, 95),
            step(1, 4, 85),
            step(2, 4, 75),
            step(7, 4, 80),
            step(7, 6, 70),
            step(1, 8, 80),
            step(2, 8, 75),
            step(7, 8, 80),
            step(7, 10, 70),
            step(1, 11, 55),
        ],
        // 6/8 Blues (12 16th steps): 2 dotted-quarter beats with snare backbeat on step 6.
        DrumPattern::SixEightBlues => vec![
            step(0, 0, 118),
            step(2, 0, 70),
            step(7, 0, 95),
            step(2, 2, 65),
            step(7, 2, 75),
            step(0, 4, 85),
            step(2, 4, 70),
            step(7, 4, 80),
            step(1, 6, 115),
            step(7, 6, 95),
            step(2, 8, 65),
            step(7, 8, 75),
            step(0, 10, 90),
            step(3, 10, 78),
            step(7, 10, 80),
            step(1, 11, 55),
        ],
        // 5/4 Take Five (20 16th steps): 3+2 jazz swing ride with kick/snare comping.
        DrumPattern::TakeFive => vec![
            step(0, 0, 108),
            step(7, 0, 100),
            step(1, 4, 95),
            step(2, 4, 82),
            step(7, 4, 90),
            step(0, 6, 85),
            step(7, 7, 80),
            step(1, 8, 78),
            step(7, 8, 95),
            step(1, 11, 55),
            step(0, 12, 100),
            step(1, 12, 100),
            step(2, 12, 82),
            step(7, 12, 100),
            step(7, 15, 80),
            step(1, 16, 88),
            step(7, 16, 92),
            step(0, 18, 82),
            step(7, 19, 75),
        ],
        // 7/8 Prog (14 16th steps): 2+2+3 odd-meter rock groove.
        DrumPattern::ProgSevenEight => vec![
            step(0, 0, 118),
            step(2, 0, 85),
            step(1, 2, 102),
            step(2, 2, 75),
            step(0, 4, 110),
            step(2, 4, 85),
            step(1, 6, 108),
            step(2, 6, 75),
            step(0, 8, 115),
            step(2, 8, 88),
            step(0, 10, 95),
            step(2, 10, 78),
            step(1, 11, 65),
            step(1, 12, 115),
            step(3, 12, 90),
            step(4, 13, 92),
        ],
        // 9/8 Compound (18 16th steps): 3+3+3 dotted-quarter flow.
        DrumPattern::NineEightCompound => vec![
            step(0, 0, 115),
            step(2, 0, 72),
            step(7, 0, 95),
            step(7, 2, 75),
            step(0, 4, 85),
            step(7, 4, 80),
            step(0, 6, 105),
            step(1, 6, 100),
            step(2, 6, 75),
            step(7, 6, 92),
            step(7, 8, 75),
            step(1, 10, 60),
            step(7, 10, 80),
            step(0, 12, 108),
            step(1, 12, 108),
            step(2, 12, 75),
            step(7, 12, 92),
            step(7, 14, 75),
            step(0, 16, 88),
            step(7, 16, 80),
            step(1, 17, 65),
        ],
        // 12/8 Shuffle (24 16th steps): slow 4-beat compound soul/blues shuffle.
        DrumPattern::TwelveEightShuffle => vec![
            step(0, 0, 118),
            step(2, 0, 72),
            step(7, 0, 92),
            step(7, 2, 72),
            step(0, 4, 85),
            step(2, 4, 68),
            step(7, 4, 80),
            step(1, 6, 116),
            step(2, 6, 76),
            step(7, 6, 90),
            step(7, 8, 72),
            step(0, 10, 92),
            step(2, 10, 68),
            step(7, 10, 80),
            step(1, 11, 58),
            step(0, 12, 110),
            step(2, 12, 72),
            step(7, 12, 92),
            step(7, 14, 72),
            step(2, 16, 68),
            step(7, 16, 80),
            step(1, 18, 118),
            step(2, 18, 76),
            step(7, 18, 90),
            step(7, 20, 72),
            step(0, 22, 85),
            step(3, 22, 82),
            step(7, 22, 80),
            step(1, 23, 60),
        ],
    }
}

const CURATED_FILL_STEPS: u16 = 16;

#[derive(Clone, Copy)]
enum StepFit {
    /// Place steps at their exact indices, dropping any past the bar.
    Exact,
    /// Repeat a `period`-step groove to fill longer bars (and truncate shorter ones).
    Tile(u16),
    /// Align a 16-step fill so its final steps land at the end of the bar.
    EndAlignedFill,
}

fn push_step_if_in_range(out: &mut Vec<DrumStep>, step: u16, s: &DrumStep, limit: u16) {
    if step < limit && step <= u16::from(u8::MAX) {
        out.push(DrumStep {
            step: step as u8,
            ..*s
        });
    }
}

fn fit_drum_steps(steps: &[DrumStep], bar_steps: u16, fit: StepFit) -> Vec<DrumStep> {
    let mut adapted = Vec::new();
    match fit {
        StepFit::Tile(period) if period > 0 => {
            for offset in (0..bar_steps).step_by(usize::from(period)) {
                for s in steps {
                    push_step_if_in_range(&mut adapted, offset + u16::from(s.step), s, bar_steps);
                }
            }
        }
        StepFit::EndAlignedFill if bar_steps < CURATED_FILL_STEPS => {
            let drop = CURATED_FILL_STEPS - bar_steps;
            for s in steps {
                let raw = u16::from(s.step);
                if raw >= drop && raw < CURATED_FILL_STEPS {
                    push_step_if_in_range(&mut adapted, raw - drop, s, bar_steps);
                }
            }
        }
        StepFit::EndAlignedFill => {
            let extra = bar_steps - CURATED_FILL_STEPS;
            if extra > 0 {
                for offset in (0..extra).step_by(usize::from(CURATED_FILL_STEPS)) {
                    for s in steps {
                        push_step_if_in_range(&mut adapted, offset + u16::from(s.step), s, extra);
                    }
                }
            }
            for s in steps {
                push_step_if_in_range(&mut adapted, u16::from(s.step) + extra, s, bar_steps);
            }
        }
        _ => {
            for s in steps {
                push_step_if_in_range(&mut adapted, u16::from(s.step), s, bar_steps);
            }
        }
    }
    adapted
}

/// Steps on a bar's step grid as note specs.
fn steps_to_note_specs(steps: &[DrumStep], step_beats: f64) -> Vec<NoteSpec> {
    steps
        .iter()
        .map(|s| NoteSpec {
            pitch: s.pad,
            start_beats: f64::from(s.step) * step_beats,
            length_beats: step_beats,
            velocity: s.velocity,
        })
        .collect()
}

/// Replaces clip `id`'s notes with `steps`' fitted to one bar of the
/// clip's meter (or `meter_override` when a pattern also switches meter), as
/// one undo step.
fn apply_drum_steps(
    project: &mut Project,
    id: ClipId,
    steps: &[DrumStep],
    meter_override: Option<&MeterMap>,
    fit: StepFit,
) -> Result<(Vec<Edit>, Outcome), CoreError> {
    let (_, start, length) = midi_notes(project, id)?;
    let map = project.tempo_map();
    let project_meter = project.meter_map();
    let meter = meter_override.unwrap_or(&project_meter);
    let sig = meter.meter_at(map.beat_at(start as f64) + 1e-9);
    let adapted = fit_drum_steps(steps, sig.steps_per_bar(), fit);
    let specs = steps_to_note_specs(&adapted, sig.step_beats());
    let notes = new_notes(project, &specs, start, 0)?;
    let one_bar = fit_to_bars_with(&map, meter, start, 0, 0);
    let fitted = fit_to_bars_with(&map, meter, start, one_bar, notes_end(&notes));
    let mut edits = vec![Edit::SetClip {
        id,
        prop: ClipProp::Notes(sorted(notes)),
    }];
    if fitted != length {
        edits.push(Edit::SetClip {
            id,
            prop: ClipProp::Length(fitted),
        });
    }
    Ok((edits, Outcome::none()))
}

/// Replace a MIDI clip's notes with a curated drum pattern's (see
/// add_drum_track, which gives a track the right instrument to play them),
/// as one undo step. Meter-specific patterns (waltz, take_five, etc.) also
/// set the clip's bar to that pattern's time signature.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplyDrumPattern {
    pub clip: ClipId,
    pub pattern: DrumPattern,
}

impl ApplyDrumPattern {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (_, start, _) = midi_notes(project, self.clip)?;
        let tempo = project.tempo_map();
        let current_meter = project.meter_map();
        let start_beat = tempo.beat_at(start as f64) + 1e-9;
        let current_sig = current_meter.meter_at(start_beat);

        let mut edits = Vec::new();
        let mut meter_override = None;
        if let Some(target_sig) = pattern_time_signature(self.pattern)
            && target_sig != current_sig
        {
            let bar = current_meter.bar_at(start_beat).saturating_add(1);
            let (mut meter_start, mut changes) =
                (project.time_signature, project.meter_changes.clone());
            if bar <= 1 {
                meter_start = target_sig;
            } else {
                changes.retain(|c| c.bar != bar);
                if changes.len() >= MAX_METER_CHANGES {
                    return Err(CoreError::invalid(format!(
                        "a project can have at most {MAX_METER_CHANGES} time signature changes"
                    )));
                }
                changes.push(MeterChange {
                    bar,
                    numerator: target_sig.numerator,
                    denominator: target_sig.denominator,
                });
                changes.sort_by_key(|c| c.bar);
            }
            edits.push(Edit::SetMeter {
                start: meter_start,
                changes: changes.clone(),
            });
            edits.extend(
                update_drum_clips_for_meter(project, meter_start, &changes)
                    .into_iter()
                    .filter(|e| !matches!(e, Edit::SetClip { id, .. } if *id == self.clip)),
            );
            meter_override = Some(MeterMap::new(meter_start, &changes));
        }

        let fit = if pattern_time_signature(self.pattern).is_some() {
            StepFit::Exact
        } else if is_fill_pattern(self.pattern) {
            StepFit::EndAlignedFill
        } else {
            StepFit::Tile(16)
        };
        let (pattern_edits, outcome) = apply_drum_steps(
            project,
            self.clip,
            &curated_pattern_steps(self.pattern),
            meter_override.as_ref(),
            fit,
        )?;
        edits.extend(pattern_edits);
        Ok((edits, outcome))
    }
}

/// Save a MIDI clip's notes as a named drum step pattern of your own (a
/// 16-step bar), alongside the curated ones apply_drum_pattern mounts.
/// Every note must be on one of builtin:drums' pad notes (see list_plugins).
/// Saving under a name that's already used replaces it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveDrumPattern {
    pub clip: ClipId,
    pub name: String,
}

impl SaveDrumPattern {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let name = validate_name(&self.name)?;
        let (notes, start, _) = midi_notes(project, self.clip)?;
        if notes.is_empty() {
            return Err(CoreError::invalid("the clip has no notes to save"));
        }
        let map = project.tempo_map();
        let sig = project
            .meter_map()
            .meter_at(map.beat_at(start as f64) + 1e-9);
        let step_beats = sig.step_beats();
        let max_step = sig
            .steps_per_bar()
            .saturating_sub(1)
            .min(u16::from(u8::MAX));
        let steps = notes
            .iter()
            .map(|n| {
                if !DRUM_PAD_NOTES.contains(&n.pitch) {
                    return Err(CoreError::invalid(format!(
                        "note at pitch {} is not one of builtin:drums' pad notes",
                        n.pitch
                    )));
                }
                let step = (map.clip_beats(start, n.start) / step_beats)
                    .round()
                    .clamp(0.0, f64::from(max_step)) as u8;
                Ok(DrumStep {
                    pad: n.pitch,
                    step,
                    velocity: n.velocity,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut patterns = project.drum_patterns.clone();
        match patterns.iter_mut().find(|p| p.name == name) {
            Some(existing) => existing.steps = steps,
            None => patterns.push(SavedDrumPattern { name, steps }),
        }
        Ok((vec![Edit::SetDrumPatterns(patterns)], Outcome::none()))
    }
}

/// Remove one of your saved drum patterns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveSavedDrumPattern {
    pub name: String,
}

impl RemoveSavedDrumPattern {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let mut patterns = project.drum_patterns.clone();
        let before = patterns.len();
        patterns.retain(|p| p.name != self.name);
        if patterns.len() == before {
            return Err(CoreError::invalid(format!(
                "no saved drum pattern named {:?}",
                self.name
            )));
        }
        Ok((vec![Edit::SetDrumPatterns(patterns)], Outcome::none()))
    }
}

/// Replace a MIDI clip's notes with one of your own saved drum patterns
/// (save_drum_pattern), as one undo step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoadSavedDrumPattern {
    pub clip: ClipId,
    pub name: String,
}

impl LoadSavedDrumPattern {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let steps = project
            .drum_patterns
            .iter()
            .find(|p| p.name == self.name)
            .ok_or_else(|| {
                CoreError::invalid(format!("no saved drum pattern named {:?}", self.name))
            })?
            .steps
            .clone();
        apply_drum_steps(project, self.clip, &steps, None, StepFit::Exact)
    }
}

fn targets_plugin(target: &AutomationTarget, id: PluginId) -> bool {
    matches!(target, AutomationTarget::Param { plugin, .. } if *plugin == id)
}

/// Move a plugin to another position in its track's chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MovePlugin {
    pub plugin: PluginId,
    /// New position (0 = first).
    pub index: usize,
}

impl MovePlugin {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (track, _) = plugin_slot(project, self.plugin)?;
        let mut plugins = track.plugins.clone();
        let from = plugins
            .iter()
            .position(|p| p.id == self.plugin)
            .expect("found above");
        let slot = plugins.remove(from);
        plugins.insert(self.index.min(plugins.len()), slot);
        Ok(set_track(track.id, TrackProp::Plugins(plugins)))
    }
}

/// Set a plugin parameter (get_plugin lists a plugin's parameters, with
/// their IDs, ranges, and values as the plugin shows them). Give the new
/// `value`, or `text` for the plugin to read the way it shows values
/// (e.g. "-6 dB", "50 %", or one of the names in the parameter's
/// `choices`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPluginParam {
    pub plugin: PluginId,
    /// The parameter's ID.
    pub param: String,
    /// The new value, within the parameter's range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// The new value as text, instead of `value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl SetPluginParam {
    /// Sets `param` of `plugin` to `value`.
    pub fn new(plugin: PluginId, param: impl Into<String>, value: f64) -> Self {
        Self {
            plugin,
            param: param.into(),
            value: Some(value),
            text: None,
        }
    }

    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (track, _) = plugin_slot(project, self.plugin)?;
        let value = match (self.value, &self.text) {
            (Some(value), None) => value,
            (Some(_), Some(_)) => return Err(CoreError::invalid("give value or text, not both")),
            (None, Some(text)) => {
                return Err(CoreError::invalid(format!(
                    "{text:?} can only be read by the running plugin; give a value instead"
                )));
            }
            (None, None) => return Err(CoreError::invalid("give the new value (or text)")),
        };
        if !value.is_finite() {
            return Err(CoreError::invalid("value must be a finite number"));
        }
        if self.param.is_empty() || self.param.len() > MAX_NAME_LEN {
            return Err(CoreError::invalid("param must be a parameter ID"));
        }
        let mut plugins = track.plugins.clone();
        let slot = plugins
            .iter_mut()
            .find(|p| p.id == self.plugin)
            .expect("found above");
        slot.params.insert(self.param, value);
        Ok(set_track(track.id, TrackProp::Plugins(plugins)))
    }
}

/// Bypass a plugin (the signal passes through unchanged), or stop
/// bypassing it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPluginBypass {
    pub plugin: PluginId,
    pub bypass: bool,
}

impl SetPluginBypass {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (track, _) = plugin_slot(project, self.plugin)?;
        let mut plugins = track.plugins.clone();
        for slot in plugins.iter_mut().filter(|p| p.id == self.plugin) {
            slot.bypass = self.bypass;
        }
        Ok(set_track(track.id, TrackProp::Plugins(plugins)))
    }
}

/// Feed a track's (or bus's) signal, after its fader, into a plugin's
/// sidechain input, e.g. a kick into a compressor on the bass; or stop
/// (source left out). list_plugins says which plugins have one
/// (`sidechain`). The source is heard as usual too, and it feeds the
/// sidechain even while it's muted or left out by solo (so a muted kick
/// still ducks the bass, and a render of the bass alone includes the
/// ducking); leave the source out or bypass the plugin to hear it without.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPluginSidechain {
    pub plugin: PluginId,
    /// The track or bus to take the signal from.
    #[serde(default)]
    pub source: Option<TrackId>,
}

impl SetPluginSidechain {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (track, _) = plugin_slot(project, self.plugin)?;
        let track_id = track.id;
        if let Some(source) = self.source {
            require_not_master(project, source, "a sidechain")?;
            if source == track_id || project.depends_on(source, track_id) {
                return Err(CoreError::invalid(format!(
                    "track {source} takes plugin {}'s signal already: that would make a loop",
                    self.plugin
                )));
            }
        }
        let mut plugins = track.plugins.clone();
        for slot in plugins.iter_mut().filter(|p| p.id == self.plugin) {
            slot.sidechain = self.source;
        }
        Ok(set_track(track_id, TrackProp::Plugins(plugins)))
    }
}

/// Send one of a plugin's other outputs (a drum instrument's separate
/// outputs, say; list_plugins lists a plugin's `outputs`, numbered from
/// 0) to a bus, where it's mixed with what else goes there; or stop (bus
/// left out). Outputs that go nowhere are silent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPluginOutput {
    pub plugin: PluginId,
    /// The output's number, from 0.
    pub output: u32,
    /// The bus's track ID (add_track with kind "bus" makes one).
    #[serde(default)]
    pub bus: Option<TrackId>,
}

impl SetPluginOutput {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let (track, _) = plugin_slot(project, self.plugin)?;
        let track_id = track.id;
        if let Some(bus) = self.bus {
            let target = project.track(bus).ok_or(CoreError::TrackNotFound(bus))?;
            if target.kind != TrackKind::Bus || bus == MASTER {
                return Err(CoreError::invalid(format!(
                    "track {bus} is not a bus (add_track with kind \"bus\" makes one)"
                )));
            }
            if project.depends_on(track_id, bus) {
                return Err(CoreError::invalid(format!(
                    "track {track_id} already takes bus {bus}'s signal: that would make a loop"
                )));
            }
        }
        if self.output > 255 {
            return Err(CoreError::invalid(
                "output must be a plugin output's number",
            ));
        }
        let mut plugins = track.plugins.clone();
        for slot in plugins.iter_mut().filter(|p| p.id == self.plugin) {
            match self.bus {
                Some(bus) => slot.outputs.insert(self.output, bus),
                None => slot.outputs.remove(&self.output),
            };
        }
        Ok(set_track(track_id, TrackProp::Plugins(plugins)))
    }
}

/// A point to put in an automation lane.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PointSpec {
    /// Timeline position in seconds.
    pub time_s: f64,
    pub value: f64,
}

/// Replace a track's automation lane for a target with these points (an
/// empty list removes the lane). Targets: {"kind": "gain"} (dB),
/// {"kind": "pan"} (-1 to 1), or {"kind": "param", "plugin": ID, "param":
/// "ID"} (in the parameter's range). Between points the value moves in a
/// straight line; before the first and after the last it holds. A lane
/// overrides the fader, pan or parameter setting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetAutomation {
    pub track: TrackId,
    pub target: AutomationTarget,
    pub points: Vec<PointSpec>,
}

impl SetAutomation {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let track = automation_track(project, self.track, &self.target)?;
        if self.points.len() > MAX_POINTS {
            return Err(CoreError::invalid(format!(
                "a lane holds at most {MAX_POINTS} points"
            )));
        }
        let mut points = self
            .points
            .iter()
            .map(|p| point(project, &self.target, p.time_s, p.value))
            .collect::<Result<Vec<_>, _>>()?;
        // Sort, keeping the last of several points at one frame.
        points.reverse();
        points.sort_by_key(|p| p.frame);
        points.dedup_by_key(|p| p.frame);
        let lanes = replace_lane(track, self.target, points);
        Ok(set_track(self.track, TrackProp::Automation(lanes)))
    }
}

/// Add a point to an automation lane (making the lane if needed), replacing
/// any point at the same time. See set_automation for the targets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddAutomationPoint {
    pub track: TrackId,
    pub target: AutomationTarget,
    pub time_s: f64,
    pub value: f64,
}

impl AddAutomationPoint {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let track = automation_track(project, self.track, &self.target)?;
        let new = point(project, &self.target, self.time_s, self.value)?;
        let mut points = track
            .lane(&self.target)
            .map(|l| l.points.clone())
            .unwrap_or_default();
        let at = points.partition_point(|p| p.frame < new.frame);
        if points.get(at).is_some_and(|p| p.frame == new.frame) {
            points[at] = new;
        } else if points.len() >= MAX_POINTS {
            return Err(CoreError::invalid(format!(
                "a lane holds at most {MAX_POINTS} points"
            )));
        } else {
            points.insert(at, new);
        }
        let lanes = replace_lane(track, self.target, points);
        Ok(set_track(self.track, TrackProp::Automation(lanes)))
    }
}

/// Remove an automation lane's points from from_s up to (not including)
/// to_s. A lane left without points is removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveAutomationPoints {
    pub track: TrackId,
    pub target: AutomationTarget,
    /// Defaults to 0.
    #[serde(default)]
    pub from_s: f64,
    /// Defaults to the end of the lane.
    #[serde(default)]
    pub to_s: Option<f64>,
}

impl RemoveAutomationPoints {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let track = automation_track(project, self.track, &self.target)?;
        let lane = track.lane(&self.target).ok_or_else(|| {
            CoreError::invalid(format!("track {} has no such automation lane", self.track))
        })?;
        let from = position_frames(project, self.from_s)?;
        let to = match self.to_s {
            Some(t) => position_frames(project, t)?,
            None => u64::MAX,
        };
        let points: Vec<AutomationPoint> = lane
            .points
            .iter()
            .filter(|p| !(from..to).contains(&p.frame))
            .copied()
            .collect();
        let lanes = replace_lane(track, self.target, points);
        Ok(set_track(self.track, TrackProp::Automation(lanes)))
    }
}

/// Write a stretch of automation, the way recording a control's moves
/// does: the lane's points from from_s to to_s (both included) are replaced
/// by these points, which must lie in that range; points outside it stay.
/// Makes the lane if needed. See set_automation for the targets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteAutomation {
    pub track: TrackId,
    pub target: AutomationTarget,
    pub from_s: f64,
    pub to_s: f64,
    pub points: Vec<PointSpec>,
}

impl WriteAutomation {
    fn lower(self, project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
        let track = automation_track(project, self.track, &self.target)?;
        let (from, to) = (
            position_frames(project, self.from_s)?,
            position_frames(project, self.to_s)?,
        );
        if to < from {
            return Err(CoreError::invalid("to_s must not be before from_s"));
        }
        let mut new = self
            .points
            .iter()
            .map(|p| point(project, &self.target, p.time_s, p.value))
            .collect::<Result<Vec<_>, _>>()?;
        if new.iter().any(|p| !(from..=to).contains(&p.frame)) {
            return Err(CoreError::invalid(
                "points must lie between from_s and to_s",
            ));
        }
        let mut points: Vec<AutomationPoint> = track
            .lane(&self.target)
            .map(|l| l.points.clone())
            .unwrap_or_default();
        points.retain(|p| !(from..=to).contains(&p.frame));
        // Keep the last of several points at one frame.
        new.reverse();
        points.extend(new);
        points.sort_by_key(|p| p.frame);
        points.dedup_by_key(|p| p.frame);
        if points.len() > MAX_POINTS {
            return Err(CoreError::invalid(format!(
                "a lane holds at most {MAX_POINTS} points"
            )));
        }
        let lanes = replace_lane(track, self.target, points);
        Ok(set_track(self.track, TrackProp::Automation(lanes)))
    }
}

/// The plugin slot `id` and its track.
fn plugin_slot(project: &Project, id: PluginId) -> Result<(&Track, &PluginSlot), CoreError> {
    project
        .plugin(id)
        .ok_or_else(|| CoreError::invalid(format!("plugin {id} does not exist")))
}

/// The track for an automation command, checking that the target exists on
/// it.
fn automation_track<'a>(
    project: &'a Project,
    id: TrackId,
    target: &AutomationTarget,
) -> Result<&'a Track, CoreError> {
    let track = project.track(id).ok_or(CoreError::TrackNotFound(id))?;
    if let AutomationTarget::Param { plugin, .. } = target
        && track.plugin(*plugin).is_none()
    {
        return Err(CoreError::invalid(format!(
            "plugin {plugin} is not on track {id}"
        )));
    }
    if let AutomationTarget::Send { bus } = target
        && !track.sends.iter().any(|s| s.bus == *bus)
    {
        return Err(CoreError::invalid(format!(
            "track {id} has no send to bus {bus} (set_track_send adds one)"
        )));
    }
    Ok(track)
}

/// Validates an automation point for `target`.
fn point(
    project: &Project,
    target: &AutomationTarget,
    time_s: f64,
    value: f64,
) -> Result<AutomationPoint, CoreError> {
    let frame = position_frames(project, time_s)?;
    match target {
        AutomationTarget::Gain | AutomationTarget::Send { .. }
            if !(f64::from(GAIN_DB_MIN)..=f64::from(GAIN_DB_MAX)).contains(&value) =>
        {
            Err(CoreError::invalid(format!(
                "gain automation is in dB, {GAIN_DB_MIN} to {GAIN_DB_MAX}; got {value}"
            )))
        }
        AutomationTarget::Pan if !(-1.0..=1.0).contains(&value) => Err(CoreError::invalid(
            format!("pan automation is -1 to 1; got {value}"),
        )),
        _ if !value.is_finite() => Err(CoreError::invalid("value must be a finite number")),
        _ => Ok(AutomationPoint { frame, value }),
    }
}

/// The track's lanes with `target`'s lane set to `points` (removed if
/// empty).
fn replace_lane(
    track: &Track,
    target: AutomationTarget,
    points: Vec<AutomationPoint>,
) -> Vec<AutomationLane> {
    let mut lanes = track.automation.clone();
    match lanes.iter().position(|l| l.target == target) {
        Some(i) if points.is_empty() => {
            lanes.remove(i);
        }
        Some(i) => lanes[i].points = points,
        None if points.is_empty() => {}
        None => lanes.push(AutomationLane { target, points }),
    }
    lanes
}

/// The notes, start and length of MIDI clip `id`.
fn midi_notes(project: &Project, id: ClipId) -> Result<(Vec<Note>, u64, u64), CoreError> {
    let (_, clip) = project.clip(id).ok_or(CoreError::ClipNotFound(id))?;
    match &clip.content {
        ClipContent::Midi { notes, .. } => Ok((notes.clone(), clip.start, clip.length)),
        ClipContent::Audio { .. } => Err(CoreError::invalid(format!(
            "clip {id} is an audio clip; only MIDI clips have notes"
        ))),
    }
}

/// Builds notes from specs for a clip starting at timeline frame
/// `clip_start`, given that the clip already has `existing`.
fn new_notes(
    project: &mut Project,
    specs: &[NoteSpec],
    clip_start: u64,
    existing: usize,
) -> Result<Vec<Note>, CoreError> {
    if existing + specs.len() > MAX_NOTES {
        return Err(CoreError::invalid(format!(
            "a clip can hold at most {MAX_NOTES} notes"
        )));
    }
    let map = project.tempo_map();
    specs
        .iter()
        .map(|spec| spec.to_note(project, &map, clip_start))
        .collect()
}

/// Edits that give clip `id` (starting at timeline frame `start`) these
/// notes, lengthening it to whole bars if a note ends past `length`.
fn set_notes(
    project: &Project,
    id: ClipId,
    start: u64,
    length: u64,
    notes: Vec<Note>,
) -> Vec<Edit> {
    let fitted = fit_to_bars(project, start, length, notes_end(&notes));
    let mut edits = vec![Edit::SetClip {
        id,
        prop: ClipProp::Notes(sorted(notes)),
    }];
    if fitted > length {
        edits.push(Edit::SetClip {
            id,
            prop: ClipProp::Length(fitted),
        });
    }
    edits
}

/// Where the last of `notes` ends, in frames from the clip's start.
fn notes_end(notes: &[Note]) -> u64 {
    notes.iter().map(Note::end).max().unwrap_or(0)
}

/// `length`, or the whole number of bars (at least one, counted from the
/// clip's start at timeline frame `start`) that covers `end` (frames from
/// the clip's start), whichever is longer. A clip that starts on a bar line
/// ends on one, with the bars of the meter map; otherwise bars are as long
/// as the one the clip starts in.
fn fit_to_bars(project: &Project, start: u64, length: u64, end: u64) -> u64 {
    fit_to_bars_with(
        &project.tempo_map(),
        &project.meter_map(),
        start,
        length,
        end,
    )
}

fn fit_to_bars_with(map: &TempoMap, meter: &MeterMap, start: u64, length: u64, end: u64) -> u64 {
    if end <= length && length > 0 {
        return length;
    }
    let start_beat = map.beat_at(start as f64);
    let first = meter.bar_at(start_beat + 1e-9);
    let first_bar = meter.bar_start(first);
    let beats = if (first_bar - start_beat).abs() < 1e-6 {
        let end_beat = map.beat_at((start + end) as f64);
        let last_bar = meter.bar_at_or_after(end_beat - 1e-9);
        last_bar.max(meter.bar_start(first + 1)) - start_beat
    } else {
        let bar = meter.meter_at(start_beat).beats_per_bar();
        (map.clip_beats(start, end) / bar - 1e-9).ceil().max(1.0) * bar
    };
    map.clip_frames(start, beats).max(length)
}

fn sorted(mut notes: Vec<Note>) -> Vec<Note> {
    notes.sort_by_key(|n| (n.start, n.pitch, n.id));
    notes
}

/// Checks a note time in beats.
fn check_beats(name: &str, beats: f64) -> Result<f64, CoreError> {
    let max = MAX_POSITION_S * TEMPO_BPM_MAX / 60.0;
    if !(0.0..=max).contains(&beats) {
        return Err(CoreError::invalid(format!(
            "{name} must be between 0 and {max}, got {beats}"
        )));
    }
    Ok(beats)
}

fn validate_pitch(pitch: u8) -> Result<(), CoreError> {
    if pitch > MIDI_MAX {
        return Err(CoreError::invalid(format!(
            "pitch must be between 0 and {MIDI_MAX}, got {pitch}"
        )));
    }
    Ok(())
}

fn validate_velocity(velocity: u8) -> Result<(), CoreError> {
    if !(1..=MIDI_MAX).contains(&velocity) {
        return Err(CoreError::invalid(format!(
            "velocity must be between 1 and {MIDI_MAX}, got {velocity}"
        )));
    }
    Ok(())
}

fn update_region(
    project: &Project,
    mut region: Region,
    start_s: Option<f64>,
    end_s: Option<f64>,
    enabled: Option<bool>,
) -> Result<Region, CoreError> {
    match (start_s, end_s) {
        (Some(start), Some(end)) => {
            let (start, end) = (
                position_frames(project, start)?,
                position_frames(project, end)?,
            );
            if end <= start {
                return Err(CoreError::invalid("end_s must be after start_s"));
            }
            region.range = Some(TimeRange { start, end });
        }
        (None, None) => {}
        _ => return Err(CoreError::invalid("give start_s and end_s together")),
    }
    if let Some(enabled) = enabled {
        if enabled && region.range.is_none() {
            return Err(CoreError::invalid(
                "set a range (start_s, end_s) before enabling",
            ));
        }
        region.enabled = enabled;
    }
    if start_s.is_none() && enabled.is_none() {
        return Err(CoreError::invalid("give a range, enabled, or both"));
    }
    Ok(region)
}

/// Checks that track `id` exists and is of `kind`.
fn require_kind(project: &Project, id: TrackId, kind: TrackKind) -> Result<(), CoreError> {
    let track = project.track(id).ok_or(CoreError::TrackNotFound(id))?;
    match (track.kind, kind) {
        (TrackKind::Audio, TrackKind::Audio) | (TrackKind::Midi, TrackKind::Midi) => Ok(()),
        (TrackKind::Midi, TrackKind::Audio) => Err(CoreError::invalid(format!(
            "track {id} is a MIDI track; audio needs an audio track"
        ))),
        (TrackKind::Audio, TrackKind::Midi) => Err(CoreError::invalid(format!(
            "track {id} is an audio track; MIDI needs a MIDI track"
        ))),
        (TrackKind::Bus, _) => Err(CoreError::invalid(format!(
            "track {id} is a bus; buses have no clips (they mix the tracks sent to them)"
        ))),
        (_, TrackKind::Bus) => Err(CoreError::invalid(format!("track {id} is not a bus"))),
    }
}

/// Checks that track `id` exists and isn't the master, which can't be
/// `what` ("removed", "renamed", ...).
fn require_not_master(project: &Project, id: TrackId, what: &str) -> Result<(), CoreError> {
    if id == MASTER {
        return Err(CoreError::invalid(format!(
            "track 0 is the master bus, which can't be {what}"
        )));
    }
    require_track(project, id)
}

/// The track to put a new clip of `kind` on: `track` if given (checked),
/// otherwise a new track named `name`, whose insertion is pushed to `edits`.
fn target_track(
    project: &mut Project,
    track: Option<TrackId>,
    kind: TrackKind,
    name: &str,
    edits: &mut Vec<Edit>,
) -> Result<TrackId, CoreError> {
    if let Some(id) = track {
        require_kind(project, id, kind)?;
        return Ok(id);
    }
    let id = project.alloc_track_id();
    let name = validate_name(name).unwrap_or_else(|_| default_track_name(project, kind));
    edits.push(Edit::InsertTrack {
        index: project.tracks.len(),
        track: Track::new(id, name, kind),
    });
    Ok(id)
}

fn position_frames(project: &Project, seconds: f64) -> Result<u64, CoreError> {
    if !(0.0..=MAX_POSITION_S).contains(&seconds) {
        return Err(CoreError::invalid(format!(
            "position_s must be between 0 and {MAX_POSITION_S}, got {seconds}"
        )));
    }
    Ok(project.seconds_to_frames(seconds))
}

fn validate_gain(gain_db: f32) -> Result<(), CoreError> {
    if !(GAIN_DB_MIN..=GAIN_DB_MAX).contains(&gain_db) {
        return Err(CoreError::invalid(format!(
            "gain_db must be between {GAIN_DB_MIN} and {GAIN_DB_MAX}, got {gain_db}"
        )));
    }
    Ok(())
}

fn require_track(project: &Project, id: TrackId) -> Result<(), CoreError> {
    project
        .track(id)
        .map(|_| ())
        .ok_or(CoreError::TrackNotFound(id))
}

fn set_track(id: TrackId, prop: TrackProp) -> (Vec<Edit>, Outcome) {
    (vec![Edit::SetTrack { id, prop }], Outcome::none())
}

fn validate_name(name: &str) -> Result<String, CoreError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CoreError::invalid("name must not be empty"));
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(CoreError::invalid(format!(
            "name must be at most {MAX_NAME_LEN} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(CoreError::invalid(
            "name must not contain control characters",
        ));
    }
    Ok(name.to_owned())
}

fn default_track_name(project: &Project, kind: TrackKind) -> String {
    let prefix = match kind {
        TrackKind::Audio => "Audio",
        TrackKind::Midi => "MIDI",
        TrackKind::Bus => "Bus",
    };
    (1..)
        .map(|n| format!("{prefix} {n}"))
        .find(|name| project.tracks.iter().all(|t| &t.name != name))
        .expect("unbounded range always yields a free name")
}
