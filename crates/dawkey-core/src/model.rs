// SPDX-License-Identifier: AGPL-3.0-or-later
//! The project document: everything that is saved to disk.
//!
//! Timeline positions and lengths are in frames at the project sample rate.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::tempo::{MeterChange, MeterMap, TempoChange, TempoMap};

pub const GAIN_DB_MIN: f32 = -144.0;
pub const GAIN_DB_MAX: f32 = 12.0;
pub const TEMPO_BPM_MIN: f64 = 20.0;
pub const TEMPO_BPM_MAX: f64 = 999.0;
/// Highest MIDI pitch and velocity.
pub const MIDI_MAX: u8 = 127;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
            JsonSchema,
        )]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_type!(
    /// Stable identifier of a track. IDs are never reused within a project,
    /// so an ID held by a client stays valid across undo and redo.
    TrackId
);
id_type!(
    /// Stable identifier of an audio source in the project's pool.
    SourceId
);
id_type!(
    /// Stable identifier of a clip.
    ClipId
);
id_type!(
    /// Stable identifier of a MIDI note. Unique across the whole project.
    NoteId
);
id_type!(
    /// Stable identifier of a plugin on a track (an insert slot).
    PluginId
);

/// What a track holds. A bus has no clips of its own: it mixes the tracks
/// sent to it (see set_track_output) through its plugins and fader. The
/// master is one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    #[default]
    Audio,
    Midi,
    Bus,
}

/// The master bus's track ID. Every track and bus without an output goes
/// there, and it goes to the speakers.
pub const MASTER: TrackId = TrackId(0);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    /// Fader gain in dB.
    pub gain_db: f32,
    /// Stereo pan, -1.0 (left) to 1.0 (right).
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    /// Armed for recording.
    #[serde(default)]
    pub armed: bool,
    /// Which input channels a recording on this track captures.
    #[serde(default)]
    pub input: InputChannels,
    /// Audio tracks: while armed, the track plays its input (through its
    /// plugins and fader), so you hear what you record.
    #[serde(default, skip_serializing_if = "is_false")]
    pub monitor: bool,
    /// The built-in synth that plays a MIDI track. `None` on audio tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synth: Option<SynthParams>,
    /// MIDI tracks: the MIDI device that plays into the track while it is
    /// armed. `None`: every device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi_input: Option<String>,
    /// MIDI tracks: the only MIDI channel (1 to 16) the track listens to.
    /// `None`: every channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi_input_channel: Option<u8>,
    /// MIDI tracks: an external MIDI device that plays the track's notes
    /// instead of its instrument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi_output: Option<MidiOutput>,
    /// Clips (audio clips on audio tracks, MIDI clips on MIDI tracks), in no
    /// particular order. Overlapping clips are summed, except muted ones.
    #[serde(default)]
    pub clips: Vec<Clip>,
    /// Insert effects, in signal order. On a MIDI track, an instrument
    /// plugin first in the chain plays the notes instead of the built-in
    /// synth.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<PluginSlot>,
    /// Automation lanes, one per target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automation: Vec<AutomationLane>,
    /// The bus the track's signal goes to; `None`: the master.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<TrackId>,
    /// Aux sends: copies of the signal to buses at their own levels,
    /// besides `output`. At most one per bus.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sends: Vec<Send>,
}

/// An aux send: a copy of a track's signal to a bus, at its own level,
/// taken after the fader and pan (or before them with `pre_fader`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Send {
    /// The bus it goes to.
    pub bus: TrackId,
    /// Its level in dB.
    pub gain_db: f32,
    /// Taken before the fader and pan, so the fader doesn't change it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pre_fader: bool,
}

impl Track {
    /// A new track with default settings: the synth for MIDI tracks.
    pub fn new(id: TrackId, name: String, kind: TrackKind) -> Self {
        Self {
            id,
            name,
            kind,
            gain_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            armed: false,
            input: InputChannels::default(),
            monitor: false,
            synth: (kind == TrackKind::Midi).then(SynthParams::default),
            midi_input: None,
            midi_input_channel: None,
            midi_output: None,
            sends: Vec::new(),
            clips: Vec::new(),
            plugins: Vec::new(),
            automation: Vec::new(),
            output: None,
        }
    }

    /// The master bus.
    pub fn master() -> Self {
        Self::new(MASTER, "Master".to_owned(), TrackKind::Bus)
    }

    /// Whether this is the master bus.
    pub fn is_master(&self) -> bool {
        self.id == MASTER
    }

    pub fn plugin(&self, id: PluginId) -> Option<&PluginSlot> {
        self.plugins.iter().find(|p| p.id == id)
    }

    pub fn lane(&self, target: &AutomationTarget) -> Option<&AutomationLane> {
        self.automation.iter().find(|l| &l.target == target)
    }

    /// The start nearest `start` where `length` frames fit without
    /// overlapping any of the track's clips (in any take lane, muted or not)
    /// except `ignore`. A clip placed over another lands flush against it,
    /// in the nearest gap it fits in; the earlier one on a tie.
    pub fn free_start(&self, start: u64, length: u64, ignore: Option<ClipId>) -> u64 {
        let mut spans: Vec<(u64, u64)> = self
            .clips
            .iter()
            .filter(|c| Some(c.id) != ignore && c.length > 0)
            .map(|c| (c.start, c.end()))
            .collect();
        spans.sort_unstable();
        let mut best: Option<u64> = None;
        let mut consider = |candidate: u64| {
            if best.is_none_or(|b| candidate.abs_diff(start) < b.abs_diff(start)) {
                best = Some(candidate);
            }
        };
        let mut gap_start = 0u64;
        for (from, to) in spans {
            if from >= gap_start.saturating_add(length) {
                consider(start.clamp(gap_start, from - length));
            }
            gap_start = gap_start.max(to);
        }
        consider(start.max(gap_start));
        best.expect("the gap after the last clip fits anything")
    }
}

/// Where a MIDI track sends its notes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MidiOutput {
    /// The device's name, as get_engine_status lists it.
    pub device: String,
    /// MIDI channel, 1 to 16.
    pub channel: u8,
    /// How long the device takes to sound after it gets MIDI, in
    /// milliseconds. Delay compensation sends its MIDI that much earlier
    /// than the rest of the mix plays.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub latency_ms: f64,
}

/// Longest MIDI device latency, in milliseconds.
pub const DEVICE_LATENCY_MS_MAX: f64 = 1000.0;

fn is_false(v: &bool) -> bool {
    !*v
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// A plugin on a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PluginSlot {
    pub id: PluginId,
    /// The plugin: `clap:<id>`, `lv2:<URI>` or `vst3:<class ID>`, as
    /// list_plugins shows it.
    pub plugin: String,
    /// Parameter values set on this slot, by parameter ID. Others keep the
    /// plugin's default.
    #[serde(default)]
    pub params: BTreeMap<String, f64>,
    /// Passed over: the signal goes through unchanged.
    #[serde(default)]
    pub bypass: bool,
    /// The track (or bus) whose signal, after its fader, feeds the
    /// plugin's sidechain input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidechain: Option<TrackId>,
    /// The plugin's other outputs (by number, from 0) that go to a bus.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<u32, TrackId>,
}

/// What a plugin saved of itself beyond its parameters (presets, loaded
/// files, settings it has no parameters for), as the plugin wrote it.
/// Saved in project files as base64.
#[derive(Clone, PartialEq, Eq)]
pub struct PluginState(Arc<[u8]>);

impl PluginState {
    pub fn new(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self(bytes.into())
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for PluginState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PluginState({} bytes)", self.0.len())
    }
}

impl Serialize for PluginState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use base64::Engine;
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for PluginState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use base64::Engine;
        let text = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(text.as_bytes())
            .map(|bytes| Self(bytes.into()))
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for PluginState {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PluginState".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "contentEncoding": "base64" })
    }
}

/// What an automation lane controls.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutomationTarget {
    /// The track's fader, in dB.
    Gain,
    /// The track's pan, -1 to 1.
    Pan,
    /// The level of the track's send to a bus, in dB.
    Send { bus: TrackId },
    /// A parameter of one of the track's plugins, in the parameter's range.
    Param { plugin: PluginId, param: String },
}

/// Values over time: between points the value moves in a straight line;
/// before the first and after the last it holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AutomationLane {
    pub target: AutomationTarget,
    /// Sorted by frame, one point per frame.
    pub points: Vec<AutomationPoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AutomationPoint {
    /// Timeline position in frames.
    pub frame: u64,
    pub value: f64,
}

impl AutomationLane {
    /// The lane's value at timeline frame `frame`.
    pub fn value_at(&self, frame: u64) -> Option<f64> {
        let points = &self.points;
        let next = points.partition_point(|p| p.frame <= frame);
        match (next.checked_sub(1).map(|i| points[i]), points.get(next)) {
            (None, None) => None,
            (Some(a), None) => Some(a.value),
            (None, Some(b)) => Some(b.value),
            (Some(a), Some(b)) => {
                let t = (frame - a.frame) as f64 / (b.frame - a.frame) as f64;
                Some(a.value + (b.value - a.value) * t)
            }
        }
    }
}

/// Oscillator shape of the built-in synth.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Waveform {
    #[default]
    Saw,
    Square,
    Triangle,
    Sine,
}

/// Settings of the built-in polyphonic synth on a MIDI track.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SynthParams {
    pub waveform: Waveform,
    /// Envelope attack time in milliseconds.
    pub attack_ms: f32,
    /// Envelope decay time in milliseconds.
    pub decay_ms: f32,
    /// Envelope sustain level, 0 to 1.
    pub sustain: f32,
    /// Envelope release time in milliseconds.
    pub release_ms: f32,
    /// Low-pass filter cutoff in Hz.
    pub cutoff_hz: f32,
    /// Filter resonance, 0 to 1.
    pub resonance: f32,
    /// How far the envelope opens the filter, in octaves above the cutoff.
    pub filter_env_octaves: f32,
}

impl Default for SynthParams {
    fn default() -> Self {
        Self {
            waveform: Waveform::Saw,
            attack_ms: 5.0,
            decay_ms: 300.0,
            sustain: 0.6,
            release_ms: 200.0,
            cutoff_hz: 1200.0,
            resonance: 0.2,
            filter_env_octaves: 2.0,
        }
    }
}

/// Which of Dawkey's two input channels a track records.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputChannels {
    /// Both inputs, as a stereo recording.
    #[default]
    Stereo,
    /// The left input only, as a mono recording.
    Left,
    /// The right input only, as a mono recording.
    Right,
}

impl InputChannels {
    pub fn count(self) -> u16 {
        match self {
            Self::Stereo => 2,
            Self::Left | Self::Right => 1,
        }
    }
}

/// A timeline range `[start, end)` in frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TimeRange {
    pub start: u64,
    pub end: u64,
}

impl TimeRange {
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    pub fn contains(&self, frame: u64) -> bool {
        frame >= self.start && frame < self.end
    }
}

/// A range that can be switched on and off, like the loop or punch range.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Region {
    pub range: Option<TimeRange>,
    pub enabled: bool,
}

impl Region {
    /// The range, if set and enabled.
    pub fn active(&self) -> Option<TimeRange> {
        self.range.filter(|r| self.enabled && !r.is_empty())
    }
}

/// An audio file in the project's pool. Sources stay in the pool when the
/// clips using them are removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Source {
    pub id: SourceId,
    pub name: String,
    /// Absolute, or relative to the project bundle once the file has been
    /// copied into it on save.
    pub path: PathBuf,
    pub channels: u16,
    /// Length in frames at the project sample rate.
    pub frames: u64,
}

/// Something placed on a track's timeline: a region of an audio source, or
/// a run of MIDI notes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Clip {
    pub id: ClipId,
    /// Timeline position of the clip's first frame.
    pub start: u64,
    /// Length in frames. Audio plays this much of the source; MIDI notes
    /// past the end are cut off.
    pub length: u64,
    pub gain_db: f32,
    /// Silent, but kept (e.g. earlier takes of a loop recording).
    #[serde(default)]
    pub muted: bool,
    /// Take lane: where the clip is drawn when its track shows its takes.
    /// Recordings over earlier clips go in a lane above them, so takes of
    /// the same passage stack up. Only the view uses it.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub lane: u32,
    /// What the clip is called; `None` shows its audio file's name (or, for
    /// MIDI clips, its track's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(flatten)]
    pub content: ClipContent,
}

/// What a clip plays. Serialized as a `kind` field next to the clip's
/// other fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClipContent {
    Audio {
        source: SourceId,
        /// First source frame played.
        offset: u64,
    },
    Midi {
        /// Notes, sorted by start.
        notes: Vec<Note>,
        /// Controller changes (pitch bend, CCs, channel pressure), sorted by
        /// frame.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        controls: Vec<ControlPoint>,
    },
}

impl Clip {
    /// Timeline frame just past the clip's end.
    pub fn end(&self) -> u64 {
        self.start + self.length
    }

    pub fn kind(&self) -> TrackKind {
        match self.content {
            ClipContent::Audio { .. } => TrackKind::Audio,
            ClipContent::Midi { .. } => TrackKind::Midi,
        }
    }

    /// The audio source, for audio clips.
    pub fn source(&self) -> Option<SourceId> {
        match self.content {
            ClipContent::Audio { source, .. } => Some(source),
            ClipContent::Midi { .. } => None,
        }
    }

    /// The notes of a MIDI clip; empty for audio clips.
    pub fn notes(&self) -> &[Note] {
        match &self.content {
            ClipContent::Midi { notes, .. } => notes,
            ClipContent::Audio { .. } => &[],
        }
    }

    /// The controller changes of a MIDI clip; empty for audio clips.
    pub fn controls(&self) -> &[ControlPoint] {
        match &self.content {
            ClipContent::Midi { controls, .. } => controls,
            ClipContent::Audio { .. } => &[],
        }
    }
}

/// A MIDI controller that a clip can change over time.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Controller {
    /// Pitch bend, 0 to 16383; 8192 is no bend.
    PitchBend,
    /// A control change, number 0 to 119 (1 is the mod wheel, 7 volume, 11
    /// expression, 64 the sustain pedal), value 0 to 127.
    Cc { number: u8 },
    /// Channel pressure (aftertouch), 0 to 127.
    ChannelPressure,
    /// Polyphonic key pressure on one key (pitch 0 to 127), 0 to 127.
    PolyPressure { pitch: u8 },
    /// Program change, program 0 to 127 (patch 1 to 128 on most devices).
    Program,
    /// MIDI 2.0 per-note pitch bend on one key (pitch 0 to 127): 0 to
    /// 65535, 32768 is no bend, the ends are ±48 semitones.
    NotePitchBend { pitch: u8 },
    /// A MIDI 2.0 registered per-note controller on one key (pitch 0 to
    /// 127), number 0 to 255, value 0 to 65535: 7 is the note's volume and
    /// 11 its expression (65535 full), 10 its pan and 74 its brightness
    /// (32768 center).
    NoteController { pitch: u8, number: u8 },
    /// A MIDI 2.0 assignable per-note controller on one key (pitch 0 to
    /// 127), number 0 to 255, value 0 to 65535; its meaning is up to the
    /// instrument.
    NoteAssignable { pitch: u8, number: u8 },
}

/// Highest control change number a clip may hold (120 and up are channel
/// mode messages).
pub const CC_MAX: u8 = 119;

/// MIDI 2.0 per-note messages travel as three bytes like MIDI 1.0 ones,
/// with a status below 0x80 (which MIDI 1.0 never uses): the high nibble is
/// the Universal MIDI Packet opcode, the low one the channel. The second
/// byte is the note, the third the controller number (0 for pitch bend),
/// and the 32-bit value goes alongside.
pub const PER_NOTE_REGISTERED: u8 = 0x00;
/// See [`PER_NOTE_REGISTERED`].
pub const PER_NOTE_ASSIGNABLE: u8 = 0x10;
/// See [`PER_NOTE_REGISTERED`].
pub const PER_NOTE_BEND: u8 = 0x60;
/// Per-note management (UMP opcode 0xF) can't use its opcode (0xF0 is
/// where MIDI 1.0's system messages start), so it travels as 0x70: the
/// third byte holds its flags, [`PER_NOTE_DETACH`] and [`PER_NOTE_RESET`],
/// as in the packet. Its 32-bit value is 0.
pub const PER_NOTE_MANAGEMENT: u8 = 0x70;
/// Per-note management flag: per-note controllers already sent to the
/// key's sounding notes stay with them; later ones only reach later notes.
pub const PER_NOTE_DETACH: u8 = 0x02;
/// Per-note management flag: the key's per-note controllers go back to
/// their defaults.
pub const PER_NOTE_RESET: u8 = 0x01;

/// Whether a status byte is a MIDI 2.0 per-note message (see
/// [`PER_NOTE_REGISTERED`] and [`PER_NOTE_MANAGEMENT`]).
pub fn is_per_note(status: u8) -> bool {
    matches!(
        status & 0xf0,
        PER_NOTE_REGISTERED | PER_NOTE_ASSIGNABLE | PER_NOTE_BEND | PER_NOTE_MANAGEMENT
    )
}

/// Scales a 16-bit controller value to 32 bits the way MIDI 2.0 does
/// (keeping the center and the top: 32768 is 0x8000_0000, 65535 is
/// 0xffff_ffff).
pub fn upscale_16(value: u16) -> u32 {
    let shifted = u32::from(value) << 16;
    if value <= 0x8000 {
        return shifted;
    }
    // Repeat the bits below the top one into the low half.
    let repeat = u32::from(value & 0x7fff);
    shifted | repeat << 1 | repeat >> 14
}

/// The 16 bits of a 32-bit MIDI 2.0 value a clip keeps.
pub fn downscale_32(value: u32) -> u16 {
    (value >> 16) as u16
}

impl Controller {
    /// Whether the controller's number or key is in range.
    pub fn valid(self) -> bool {
        match self {
            Self::Cc { number } => number <= CC_MAX,
            Self::PolyPressure { pitch } => pitch <= MIDI_MAX,
            _ => self.pitch().is_none_or(|p| p <= MIDI_MAX),
        }
    }

    /// The key of a per-note controller.
    pub fn pitch(self) -> Option<u8> {
        match self {
            Self::PolyPressure { pitch }
            | Self::NotePitchBend { pitch }
            | Self::NoteController { pitch, .. }
            | Self::NoteAssignable { pitch, .. } => Some(pitch),
            _ => None,
        }
    }

    /// Whether this is a MIDI 2.0 per-note controller (with 16-bit values,
    /// sent as Universal MIDI Packets).
    pub fn midi2(self) -> bool {
        matches!(
            self,
            Self::NotePitchBend { .. } | Self::NoteController { .. } | Self::NoteAssignable { .. }
        )
    }

    /// The highest value.
    pub fn max(self) -> u16 {
        match self {
            Self::PitchBend => 16_383,
            _ if self.midi2() => u16::MAX,
            _ => 127,
        }
    }

    /// The value when nothing has set it: no bend, volume 100, pan center,
    /// full expression, otherwise 0. Programs have none (see
    /// [`resets`](Self::resets)).
    pub fn default_value(self) -> u16 {
        match self {
            Self::PitchBend => 8192,
            Self::Cc { number: 7 } => 100,
            Self::Cc { number: 10 } => 64,
            Self::Cc { number: 11 } => 127,
            Self::NotePitchBend { .. } => 0x8000,
            Self::NoteController { number: 7 | 11, .. } => u16::MAX,
            Self::NoteController {
                number: 8 | 10 | 74,
                ..
            } => 0x8000,
            // Pitch 7.25: the key's own pitch.
            Self::NoteController { number: 3, pitch } => u16::from(pitch & 0x7f) << 9,
            _ => 0,
        }
    }

    /// Whether the controller goes back to its default when playback
    /// stops, and is set to it when playback starts before its first
    /// change. Not programs: the patch a device was left on stays.
    pub fn resets(self) -> bool {
        self != Self::Program
    }

    /// The MIDI 1.0 message that sets `value` on `channel` (two-byte
    /// messages end in 0), or for a per-note controller its MIDI 2.0 form
    /// (see [`PER_NOTE_REGISTERED`]; [`value32`](Self::value32) has its
    /// value).
    pub fn message(self, channel: u8, value: u16) -> [u8; 3] {
        let channel = channel & 0x0f;
        match self {
            Self::NotePitchBend { pitch } => [PER_NOTE_BEND | channel, pitch & 0x7f, 0],
            Self::NoteController { pitch, number } => {
                [PER_NOTE_REGISTERED | channel, pitch & 0x7f, number]
            }
            Self::NoteAssignable { pitch, number } => {
                [PER_NOTE_ASSIGNABLE | channel, pitch & 0x7f, number]
            }
            Self::PitchBend => {
                let v = value.min(16_383);
                [0xe0 | channel, (v & 0x7f) as u8, (v >> 7) as u8]
            }
            Self::Cc { number } => [0xb0 | channel, number & 0x7f, value.min(127) as u8],
            Self::ChannelPressure => [0xd0 | channel, value.min(127) as u8, 0],
            Self::PolyPressure { pitch } => [0xa0 | channel, pitch & 0x7f, value.min(127) as u8],
            Self::Program => [0xc0 | channel, value.min(127) as u8, 0],
        }
    }

    /// The 32-bit value of a per-note message that sets `value` (0 for MIDI
    /// 1.0 controllers).
    pub fn value32(self, value: u16) -> u32 {
        if self.midi2() { upscale_16(value) } else { 0 }
    }

    /// The controller and value a message sets, if it's one a clip can
    /// hold: a MIDI 1.0 message, or a MIDI 2.0 per-note one with its 32-bit
    /// value.
    pub fn from_event(data: [u8; 3], value32: u32) -> Option<(Self, u16)> {
        let [status, pitch, number] = data;
        let pitch = pitch & 0x7f;
        let value = downscale_32(value32);
        match status & 0xf0 {
            _ if status >= 0x80 => Self::from_message(data),
            PER_NOTE_BEND => Some((Self::NotePitchBend { pitch }, value)),
            PER_NOTE_REGISTERED => Some((Self::NoteController { pitch, number }, value)),
            PER_NOTE_ASSIGNABLE => Some((Self::NoteAssignable { pitch, number }, value)),
            _ => None,
        }
    }

    /// The controller and value a MIDI 1.0 message sets, if it's one a clip
    /// can hold.
    pub fn from_message(data: [u8; 3]) -> Option<(Self, u16)> {
        let [status, a, b] = data;
        match status & 0xf0 {
            0xe0 => Some((
                Self::PitchBend,
                u16::from(a & 0x7f) | u16::from(b & 0x7f) << 7,
            )),
            0xb0 if a <= CC_MAX => Some((Self::Cc { number: a }, u16::from(b & 0x7f))),
            0xd0 => Some((Self::ChannelPressure, u16::from(a & 0x7f))),
            0xa0 => Some((Self::PolyPressure { pitch: a & 0x7f }, u16::from(b & 0x7f))),
            0xc0 => Some((Self::Program, u16::from(a & 0x7f))),
            _ => None,
        }
    }
}

/// A controller change in a MIDI clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ControlPoint {
    pub controller: Controller,
    /// Frames from the clip's start.
    pub frame: u64,
    /// The new value, in the controller's range.
    pub value: u16,
}

/// A note in a MIDI clip. Times are in frames from the clip's start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Note {
    pub id: NoteId,
    /// MIDI pitch, 0 to 127 (60 is middle C).
    pub pitch: u8,
    /// MIDI velocity, 1 to 127.
    pub velocity: u8,
    pub start: u64,
    pub length: u64,
}

impl Note {
    pub fn end(&self) -> u64 {
        self.start + self.length
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TimeSignature {
    pub numerator: u8,
    pub denominator: u8,
}

impl TimeSignature {
    /// Quarter notes per bar.
    pub fn beats_per_bar(self) -> f64 {
        4.0 * f64::from(self.numerator) / f64::from(self.denominator)
    }

    /// Quarter notes per beat (the denominator's note).
    pub fn beat_unit(self) -> f64 {
        4.0 / f64::from(self.denominator)
    }

    /// Whether this is a time signature Dawkey supports: 1 to 32 beats of
    /// a whole, half, quarter, ... or 32nd note.
    pub fn valid(self) -> bool {
        (1..=32).contains(&self.numerator)
            && self.denominator.is_power_of_two()
            && self.denominator <= 32
    }
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self {
            numerator: 4,
            denominator: 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Project {
    pub name: String,
    pub sample_rate: u32,
    /// Tempo at the start of the project.
    pub tempo_bpm: f64,
    /// Later tempo changes, sorted by beat, each after beat 0.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tempo_changes: Vec<TempoChange>,
    /// Time signature at the start of the project.
    pub time_signature: TimeSignature,
    /// Later time signature changes, sorted by bar, each after the first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub meter_changes: Vec<MeterChange>,
    /// Tracks and buses in display order (not the master).
    pub tracks: Vec<Track>,
    /// The master bus, track 0: its plugins and fader process the whole
    /// mix.
    #[serde(default = "Track::master")]
    pub master: Track,
    /// The audio pool.
    #[serde(default)]
    pub sources: Vec<Source>,
    /// Playback loops over this range while enabled.
    #[serde(default, rename = "loop")]
    pub loop_region: Region,
    /// Recording only captures inside this range while enabled.
    #[serde(default)]
    pub punch: Region,
    /// What plugins saved of themselves, by slot. Kept for slots that were
    /// removed too (until the project is saved), so undo brings a plugin
    /// back as it was; not part of the undo history.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugin_states: BTreeMap<PluginId, PluginState>,
    /// Next IDs to hand out. Saved so IDs stay unique across sessions.
    next_track_id: u64,
    #[serde(default)]
    next_source_id: u64,
    #[serde(default)]
    next_clip_id: u64,
    #[serde(default)]
    next_note_id: u64,
    #[serde(default)]
    next_plugin_id: u64,
}

impl Default for Project {
    fn default() -> Self {
        Self::new("Untitled")
    }
}

impl Project {
    /// Timeline frame where the last clip that plays ends (0 with none):
    /// where the song ends, for exporting it.
    pub fn end(&self) -> u64 {
        self.tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| !c.muted)
            .map(Clip::end)
            .max()
            .unwrap_or(0)
    }

    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            sample_rate: 48_000,
            tempo_bpm: 120.0,
            tempo_changes: Vec::new(),
            time_signature: TimeSignature::default(),
            meter_changes: Vec::new(),
            tracks: Vec::new(),
            master: Track::master(),
            sources: Vec::new(),
            loop_region: Region::default(),
            punch: Region::default(),
            plugin_states: BTreeMap::new(),
            next_track_id: 1,
            next_source_id: 1,
            next_clip_id: 1,
            next_note_id: 1,
            next_plugin_id: 1,
        }
    }

    /// A track or bus, or the master ([`MASTER`]).
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        if id == MASTER {
            return Some(&self.master);
        }
        self.tracks.iter().find(|t| t.id == id)
    }

    pub(crate) fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        if id == MASTER {
            return Some(&mut self.master);
        }
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    /// Where a track is in [`tracks`](Self::tracks) (`None` for the master,
    /// which isn't there).
    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// Every track and bus, then the master.
    pub fn all_tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().chain(std::iter::once(&self.master))
    }

    /// Where track `id`'s signal goes: a bus, or the master. `None` for
    /// the master itself.
    pub fn destination(&self, id: TrackId) -> Option<TrackId> {
        if id == MASTER {
            return None;
        }
        Some(self.track(id)?.output.unwrap_or(MASTER))
    }

    /// Whether `from`'s signal reaches `to` (through buses, and plugin
    /// outputs sent to buses), or is `to`.
    pub fn feeds(&self, from: TrackId, to: TrackId) -> bool {
        self.reaches(from, to, |t| {
            let outputs = t.plugins.iter().flat_map(|p| p.outputs.values().copied());
            let sends = t.sends.iter().map(|s| s.bus);
            self.destination(t.id)
                .into_iter()
                .chain(outputs)
                .chain(sends)
                .collect()
        })
    }

    /// Whether track `track` takes anything from `on`, directly or not (it
    /// must render after it): `on` feeds it, or feeds a sidechain of its
    /// plugins (or of something that feeds it). True for `track == on`.
    pub fn depends_on(&self, track: TrackId, on: TrackId) -> bool {
        // Downstream from `on`: where its signal goes, and the tracks whose
        // plugins take it as a sidechain.
        self.reaches(on, track, |t| {
            let outputs = t.plugins.iter().flat_map(|p| p.outputs.values().copied());
            let sends = t.sends.iter().map(|s| s.bus);
            let sidechained = self
                .all_tracks()
                .filter(|other| other.plugins.iter().any(|p| p.sidechain == Some(t.id)))
                .map(|other| other.id);
            self.destination(t.id)
                .into_iter()
                .chain(outputs)
                .chain(sends)
                .chain(sidechained)
                .collect()
        })
    }

    /// Whether `to` can be reached from `from` along `next` (a track's
    /// successors).
    fn reaches(&self, from: TrackId, to: TrackId, next: impl Fn(&Track) -> Vec<TrackId>) -> bool {
        let mut seen = vec![from];
        let mut queue = vec![from];
        while let Some(id) = queue.pop() {
            if id == to {
                return true;
            }
            let Some(track) = self.track(id) else {
                continue;
            };
            for n in next(track) {
                if !seen.contains(&n) {
                    seen.push(n);
                    queue.push(n);
                }
            }
        }
        false
    }

    /// The tracks that must render before `track`: those sent to it (or
    /// whose plugins send outputs to it), and the sidechains of its
    /// plugins.
    pub fn inputs(&self, track: TrackId) -> Vec<TrackId> {
        let Some(this) = self.track(track) else {
            return Vec::new();
        };
        let mut inputs: Vec<TrackId> = self
            .tracks
            .iter()
            .filter(|t| {
                t.output.unwrap_or(MASTER) == track
                    || t.plugins
                        .iter()
                        .any(|p| p.outputs.values().any(|&b| b == track))
                    || t.sends.iter().any(|s| s.bus == track)
            })
            .map(|t| t.id)
            .collect();
        inputs.extend(this.plugins.iter().filter_map(|p| p.sidechain));
        inputs.sort_unstable();
        inputs.dedup();
        inputs.retain(|&t| t != track);
        inputs
    }

    pub fn source(&self, id: SourceId) -> Option<&Source> {
        self.sources.iter().find(|s| s.id == id)
    }

    /// Finds a clip and the track it is on.
    pub fn clip(&self, id: ClipId) -> Option<(&Track, &Clip)> {
        self.tracks
            .iter()
            .find_map(|t| t.clips.iter().find(|c| c.id == id).map(|c| (t, c)))
    }

    pub(crate) fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.tracks
            .iter_mut()
            .find_map(|t| t.clips.iter_mut().find(|c| c.id == id))
    }

    /// Converts seconds to frames at the project sample rate, rounding to the
    /// nearest frame.
    pub fn seconds_to_frames(&self, seconds: f64) -> u64 {
        (seconds * f64::from(self.sample_rate)).round().max(0.0) as u64
    }

    pub fn frames_to_seconds(&self, frames: u64) -> f64 {
        frames as f64 / f64::from(self.sample_rate)
    }

    /// Beat ↔ frame conversions under the project's tempo map.
    pub fn tempo_map(&self) -> TempoMap {
        TempoMap::new(self.sample_rate, self.tempo_bpm, &self.tempo_changes)
    }

    /// Where bars start under the project's time signatures.
    pub fn meter_map(&self) -> MeterMap {
        MeterMap::new(self.time_signature, &self.meter_changes)
    }

    pub(crate) fn alloc_track_id(&mut self) -> TrackId {
        let id = TrackId(self.next_track_id);
        self.next_track_id += 1;
        id
    }

    pub(crate) fn alloc_source_id(&mut self) -> SourceId {
        let id = SourceId(self.next_source_id);
        self.next_source_id += 1;
        id
    }

    pub(crate) fn alloc_clip_id(&mut self) -> ClipId {
        let id = ClipId(self.next_clip_id);
        self.next_clip_id += 1;
        id
    }

    /// Finds a plugin slot and the track (or bus, or the master) it is on.
    pub fn plugin(&self, id: PluginId) -> Option<(&Track, &PluginSlot)> {
        self.all_tracks().find_map(|t| t.plugin(id).map(|p| (t, p)))
    }

    pub(crate) fn alloc_plugin_id(&mut self) -> PluginId {
        let id = PluginId(self.next_plugin_id);
        self.next_plugin_id += 1;
        id
    }

    pub(crate) fn alloc_note_id(&mut self) -> NoteId {
        let id = NoteId(self.next_note_id);
        self.next_note_id += 1;
        id
    }

    /// Repairs invariants a hand-edited or corrupted file could break.
    pub(crate) fn sanitize(&mut self) {
        let max_track = self.tracks.iter().map(|t| t.id.0).max().unwrap_or(0);
        self.next_track_id = self.next_track_id.max(max_track + 1);
        // Track 0 is the master's.
        for i in 0..self.tracks.len() {
            if self.tracks[i].id == MASTER {
                self.tracks[i].id = self.alloc_track_id();
            }
        }
        let master = &mut self.master;
        master.id = MASTER;
        master.kind = TrackKind::Bus;
        master.name = "Master".to_owned();
        master.solo = false;
        master.output = None;
        let max_source = self.sources.iter().map(|s| s.id.0).max().unwrap_or(0);
        self.next_source_id = self.next_source_id.max(max_source + 1);
        let max_clip = self
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .map(|c| c.id.0)
            .max()
            .unwrap_or(0);
        self.next_clip_id = self.next_clip_id.max(max_clip + 1);
        let max_note = self
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .flat_map(|c| c.notes())
            .map(|n| n.id.0)
            .max()
            .unwrap_or(0);
        self.next_note_id = self.next_note_id.max(max_note + 1);
        let max_plugin = self
            .all_tracks()
            .flat_map(|t| &t.plugins)
            .map(|p| p.id.0)
            .max()
            .unwrap_or(0);
        self.next_plugin_id = self.next_plugin_id.max(max_plugin + 1);
        if !(TEMPO_BPM_MIN..=TEMPO_BPM_MAX).contains(&self.tempo_bpm) {
            self.tempo_bpm = 120.0;
        }
        self.tempo_changes.retain(|c| {
            c.beat.is_finite() && c.beat > 0.0 && (TEMPO_BPM_MIN..=TEMPO_BPM_MAX).contains(&c.bpm)
        });
        self.tempo_changes.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        self.tempo_changes.dedup_by(|b, a| a.beat == b.beat);
        if !self.time_signature.valid() {
            self.time_signature = TimeSignature::default();
        }
        self.meter_changes
            .retain(|c| c.bar > 1 && c.time_signature().valid());
        self.meter_changes.sort_by_key(|c| c.bar);
        self.meter_changes.dedup_by_key(|c| c.bar);
        // Outputs go to buses that exist, without loops.
        let buses: Vec<TrackId> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Bus)
            .map(|t| t.id)
            .collect();
        for i in 0..self.tracks.len() {
            let track = &mut self.tracks[i];
            if track
                .output
                .is_some_and(|o| o == MASTER || !buses.contains(&o))
            {
                track.output = None;
            }
            let (id, output) = (track.id, track.output);
            if let Some(bus) = output
                && self.feeds(bus, id)
            {
                self.tracks[i].output = None;
            }
        }
        // Sidechains from tracks that exist (not the master, nor the
        // plugin's own track), plugin outputs to buses; no loops.
        let ids: Vec<TrackId> = self.tracks.iter().map(|t| t.id).collect();
        for t in 0..=self.tracks.len() {
            let id = if t < self.tracks.len() {
                self.tracks[t].id
            } else {
                MASTER
            };
            let count = self.track(id).map_or(0, |t| t.plugins.len());
            for p in 0..count {
                let slot = &self.track(id).expect("exists").plugins[p];
                let (sidechain, outputs) = (slot.sidechain, slot.outputs.clone());
                let sidechain =
                    sidechain.filter(|s| ids.contains(s) && *s != id && !self.depends_on(*s, id));
                let outputs = outputs
                    .into_iter()
                    .filter(|&(_, bus)| buses.contains(&bus) && !self.depends_on(id, bus))
                    .collect();
                let slot = &mut self.track_mut(id).expect("exists").plugins[p];
                slot.sidechain = sidechain;
                slot.outputs = outputs;
            }
        }
        let tracks = self
            .tracks
            .iter_mut()
            .chain(std::iter::once(&mut self.master));
        for track in tracks {
            match track.kind {
                TrackKind::Midi => {
                    track.synth.get_or_insert_with(SynthParams::default);
                }
                TrackKind::Audio | TrackKind::Bus => {
                    track.synth = None;
                    track.midi_input = None;
                    track.midi_input_channel = None;
                    track.midi_output = None;
                }
            }
            if track.kind == TrackKind::Bus {
                track.clips.clear();
                track.armed = false;
                track.monitor = false;
            }
            track.midi_input_channel = track.midi_input_channel.filter(|c| (1..=16).contains(c));
            if let Some(output) = &mut track.midi_output {
                output.channel = output.channel.clamp(1, 16);
                output.latency_ms = if output.latency_ms.is_finite() {
                    output.latency_ms.clamp(0.0, DEVICE_LATENCY_MS_MAX)
                } else {
                    0.0
                };
            }
            // Clips of the wrong kind can't play on this track.
            let kind = track.kind;
            track.clips.retain(|c| c.kind() == kind);
            // Lanes need sorted, distinct points and a target on this track.
            let plugins: Vec<PluginId> = track.plugins.iter().map(|p| p.id).collect();
            track.automation.retain(|l| match &l.target {
                AutomationTarget::Param { plugin, .. } => plugins.contains(plugin),
                _ => true,
            });
            for lane in &mut track.automation {
                lane.points.retain(|p| p.value.is_finite());
                lane.points.sort_by_key(|p| p.frame);
                lane.points.dedup_by_key(|p| p.frame);
            }
            for clip in &mut track.clips {
                if let ClipContent::Midi { notes, controls } = &mut clip.content {
                    controls.retain(|c| c.controller.valid() && c.value <= c.controller.max());
                    controls.sort_by_key(|c| c.frame);
                    notes.retain(|n| n.length > 0 && n.pitch <= MIDI_MAX);
                    for note in notes.iter_mut() {
                        note.velocity = note.velocity.clamp(1, MIDI_MAX);
                    }
                    notes.sort_by_key(|n| (n.start, n.pitch));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_start_finds_the_nearest_gap() {
        let clip = |id, start, length| Clip {
            id: ClipId(id),
            start,
            length,
            gain_db: 0.0,
            muted: false,
            lane: 0,
            name: None,
            content: ClipContent::Audio {
                source: SourceId(1),
                offset: 0,
            },
        };
        let mut track = Track::new(TrackId(1), "T".to_owned(), TrackKind::Audio);
        // Clips at 100..200 and 250..400 (a 50-frame gap between them).
        track.clips = vec![clip(2, 250, 150), clip(1, 100, 100)];
        let free = |start, length| track.free_start(start, length, None);
        // Nothing in the way: stays.
        assert_eq!(free(0, 100), 0);
        assert_eq!(free(500, 10), 500);
        // Covering the first clip's start: before it, flush.
        assert_eq!(free(60, 80), 20);
        // Too long for the gap before it: after the last clip.
        assert_eq!(free(60, 120), 400);
        // Fits in the gap between them, flush against the nearer edge.
        assert_eq!(free(190, 30), 200);
        assert_eq!(free(240, 30), 220);
        // Too long for that gap: whichever side is nearer.
        assert_eq!(free(190, 60), 40);
        assert_eq!(free(300, 60), 400);
        // A clip doesn't get in its own way.
        assert_eq!(track.free_start(120, 100, Some(ClipId(1))), 120);
        assert_eq!(track.free_start(120, 140, Some(ClipId(1))), 110);
    }

    #[test]
    fn per_note_values_scale_like_midi2() {
        assert_eq!(upscale_16(0), 0);
        assert_eq!(upscale_16(0x8000), 0x8000_0000);
        assert_eq!(upscale_16(u16::MAX), u32::MAX);
        assert_eq!(upscale_16(0x4000), 0x4000_0000);
        for v in [0, 1, 0x7fff, 0x8000, 0x8001, 0xc000, u16::MAX] {
            assert_eq!(downscale_32(upscale_16(v)), v);
        }
    }

    #[test]
    fn per_note_messages_round_trip() {
        let cases = [
            (Controller::NotePitchBend { pitch: 60 }, [0x63, 60, 0]),
            (
                Controller::NoteController {
                    pitch: 61,
                    number: 74,
                },
                [0x03, 61, 74],
            ),
            (
                Controller::NoteAssignable {
                    pitch: 62,
                    number: 200,
                },
                [0x13, 62, 200],
            ),
        ];
        for (controller, data) in cases {
            assert!(controller.midi2() && controller.valid());
            assert_eq!(controller.max(), u16::MAX);
            assert_eq!(controller.message(3, 0x9000), data);
            assert!(is_per_note(data[0]));
            let value = controller.value32(0x9000);
            assert_eq!(
                Controller::from_event(data, value),
                Some((controller, 0x9000))
            );
        }
        assert!(!is_per_note(0x90));
        // Per-note management is a message, not a value a clip holds.
        assert!(is_per_note(PER_NOTE_MANAGEMENT | 2));
        assert_eq!(
            Controller::from_event([PER_NOTE_MANAGEMENT, 60, PER_NOTE_RESET], 0),
            None
        );
        assert_eq!(Controller::Cc { number: 1 }.value32(5), 0);
        assert_eq!(
            Controller::from_event([0xb0, 1, 5], 0),
            Some((Controller::Cc { number: 1 }, 5))
        );
        assert!(!Controller::NotePitchBend { pitch: 128 }.valid());
        let volume = Controller::NoteController {
            pitch: 60,
            number: 7,
        };
        assert_eq!(volume.default_value(), u16::MAX);
        let pitch = Controller::NoteController {
            pitch: 60,
            number: 3,
        };
        assert_eq!(pitch.default_value(), 60 << 9);
    }
}
