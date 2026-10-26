// SPDX-License-Identifier: AGPL-3.0-or-later
//! User-level commands. Each command is its own argument struct, so its JSON
//! Schema can be published as an MCP tool, and its doc comment becomes the
//! tool description.

use schemars::{JsonSchema, Schema, generate::SchemaSettings};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::edit::{ClipProp, Edit, TrackProp};
use crate::error::CoreError;
use crate::model::{
    AutomationLane, AutomationPoint, AutomationTarget, CC_MAX, Clip, ClipContent, ClipId,
    ControlPoint, Controller, DEVICE_LATENCY_MS_MAX, GAIN_DB_MAX, GAIN_DB_MIN, MASTER, MIDI_MAX,
    MidiOutput, Note, NoteId, PluginId, PluginSlot, Project, Send, SourceId, SynthParams,
    TEMPO_BPM_MAX, TEMPO_BPM_MIN, TimeSignature, Track, TrackId, TrackKind, Waveform,
};
use crate::model::{InputChannels, Region, TimeRange};
use crate::session::Outcome;
use crate::tempo::{MeterChange, TempoChange, TempoMap};

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
    RemoveTrack => "remove_track", Destructive;
    RenameTrack => "rename_track", Setter;
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
        Ok((vec![Edit::SetMeter { start, changes }], Outcome::none()))
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
        let edit = Edit::SetMeter {
            start: project.time_signature,
            changes,
        };
        Ok((vec![edit], Outcome::none()))
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
    fn lower(self, _project: &mut Project) -> Result<(Vec<Edit>, Outcome), CoreError> {
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
        Ok((vec![Edit::SetMeter { start, changes }], Outcome::none()))
    }
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
    /// The plugin's ID from list_plugins, e.g. "clap:org.example.reverb",
    /// "lv2:http://example.org/eq" or
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
        if !["clap:", "lv2:", "vst3:"]
            .iter()
            .any(|prefix| self.plugin.starts_with(prefix))
        {
            return Err(CoreError::invalid(format!(
                "unknown plugin {:?}; plugin IDs start with clap:, lv2: or vst3: (see list_plugins)",
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
/// (`sidechain`). The source is heard as usual too.
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
    if end <= length && length > 0 {
        return length;
    }
    let map = project.tempo_map();
    let meter = project.meter_map();
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
