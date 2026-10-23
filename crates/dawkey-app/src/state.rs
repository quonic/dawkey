// SPDX-License-Identifier: AGPL-3.0-or-later
//! UI state that isn't part of the project: which page is shown, zoom and
//! scroll, selection, and gestures in progress.

use std::path::PathBuf;

use dawkey_core::{ClipId, Controller, NoteId, PluginId, TrackId};
use serde::Serialize;

/// Height of one track lane in the arrange view.
pub(crate) const LANE_H: f32 = 72.0;
/// Width of the track headers column.
pub(crate) const HEADER_W: f32 = 250.0;
/// Height of the timeline ruler.
pub(crate) const RULER_H: f32 = 28.0;
/// Travel of a mixer fader, in logical px.
pub(crate) const FADER_H: f32 = 220.0;
/// Width of a mixer pan slider.
pub(crate) const PAN_W: f32 = 76.0;

/// The top of each track's row in the arrange view (before scrolling),
/// given how many take lanes and automation lanes each shows, and the
/// height of them all.
pub(crate) fn track_tops(lanes: impl Iterator<Item = (usize, usize)>) -> (Vec<f32>, f32) {
    let mut tops = Vec::new();
    let mut y = 0.0;
    for (takes, autos) in lanes {
        tops.push(y);
        y += LANE_H + takes as f32 * TAKE_H + autos as f32 * AUTO_H;
    }
    (tops, y)
}

/// How many take lanes a track with `lanes` of them shows: all of them
/// when they're shown and there's more than one.
pub(crate) fn takes_shown(view: &ViewState, track: TrackId, lanes: usize) -> usize {
    if lanes > 1 && view.takes_shown.contains(&track) {
        lanes
    } else {
        0
    }
}

pub(crate) const MIN_ZOOM: f32 = 2.0;
pub(crate) const MAX_ZOOM: f32 = 4000.0;

/// Piano roll: height of one pitch row.
pub(crate) const ROW_H: f32 = 12.0;
/// Piano roll: width of the keyboard column.
pub(crate) const KEYS_W: f32 = 56.0;
/// Piano roll zoom limits, in px per beat.
pub(crate) const MIN_BEAT_ZOOM: f32 = 8.0;
pub(crate) const MAX_BEAT_ZOOM: f32 = 1000.0;
/// Piano roll: height of the velocity/controller lane under the grid.
pub(crate) const ROLL_LANE_H: f32 = 96.0;
/// Width of a synth parameter slider; dragging across it covers the range.
pub(crate) const SLIDER_W: f32 = 84.0;
/// Height of an automation lane under its track.
pub(crate) const AUTO_H: f32 = 44.0;
/// Height of a take lane under its track.
pub(crate) const TAKE_H: f32 = 48.0;
/// Width of a plugin parameter slider in the plugin editor.
pub(crate) const PARAM_W: f32 = 180.0;
/// Height of the tempo lane under the ruler.
pub(crate) const TEMPO_H: f32 = 44.0;
/// Height of the time signature lane under the tempo lane.
pub(crate) const METER_H: f32 = 24.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Page {
    #[default]
    Arrange,
    Mixer,
    /// The MIDI note editor for one clip.
    PianoRoll,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Gesture {
    /// Moving a clip, or with Ctrl held, a copy of it; committed as one
    /// `move_clip` (or `duplicate_clip`) on release.
    Clip {
        clip: ClipId,
        /// Pointer time minus clip start when the drag began.
        grab_s: f64,
        /// Where it goes: snapped, and clear of the track's other clips.
        preview_start_s: f64,
        preview_track: TrackId,
        /// Ctrl is held: the clip stays and a copy goes there.
        copy: bool,
    },
    /// Dragging across a take lane: on release, one `comp_take` makes the
    /// lane play over the range.
    Comp {
        track: TrackId,
        /// A clip in the lane where the drag began.
        clip: ClipId,
        lane: u32,
        anchor_s: f64,
        current_s: f64,
    },
    /// Dragging a clip's start or end edge; committed as one `trim_clip` on
    /// release.
    TrimClip {
        clip: ClipId,
        edge: ClipEdge,
        /// Pointer time minus the edge's time when the drag began.
        grab_s: f64,
        /// How far the edge may go: the source's ends, and a minimum length.
        min_s: f64,
        max_s: f64,
        /// The clip's edges as dragged so far.
        start_s: f64,
        end_s: f64,
    },
    /// Ctrl-dragging a clip's end edge: on release, one `repeat_clip` lays
    /// copies of it end to end up to there.
    Repeat {
        clip: ClipId,
        /// Pointer time minus the clip's end when the drag began.
        grab_s: f64,
        /// How far the copies may go: the next clip on the track.
        max_s: f64,
        /// Where the copies end, as dragged so far.
        until_s: f64,
    },
    /// Dragging a fader or pan slider; each step is applied live and the
    /// whole gesture is one undo step.
    Fader {
        track: TrackId,
        start_db: f32,
        merge_key: String,
    },
    Pan {
        track: TrackId,
        start_pan: f32,
        merge_key: String,
    },
    /// Dragging along the ruler moves the playhead.
    Scrub,
    /// Shift- or Ctrl-dragging along the ruler sets the loop or punch range.
    Range {
        kind: RangeKind,
        anchor_s: f64,
        current_s: f64,
    },
    /// Moving notes in the piano roll; committed as one `edit_midi_notes`
    /// on release.
    MoveNotes {
        clip: ClipId,
        notes: Vec<NoteId>,
        /// Pointer beat and pitch row when the drag began.
        origin_beats: f64,
        origin_pitch: i32,
        /// Where the grabbed note started, so the move snaps it to the grid.
        anchor_beats: f64,
        delta_beats: f64,
        delta_pitch: i32,
    },
    /// Dragging a note's end in the piano roll.
    ResizeNote {
        clip: ClipId,
        note: NoteId,
        start_beats: f64,
        length_beats: f64,
    },
    /// Dragging a rectangle over the piano roll grid: the notes it touches
    /// are selected (added to `base`, the selection before, with Shift or
    /// Ctrl).
    Marquee {
        clip: ClipId,
        base: Vec<NoteId>,
        /// Corners as (beat, pitch).
        from: (f64, i32),
        to: (f64, i32),
    },
    /// Dragging a velocity bar: these notes (with their velocities before)
    /// change by `delta`; one `edit_midi_notes` on release.
    Velocity {
        clip: ClipId,
        notes: Vec<(NoteId, u8)>,
        delta: i32,
    },
    /// Moving a point in the controller lane; one `set_midi_controls` on
    /// release.
    ControlPoint {
        clip: ClipId,
        controller: Controller,
        index: usize,
        beat: f64,
        value: u16,
    },
    /// Drawing across the velocity or controller lane: (beat, value) under
    /// the pointer so far, applied on release.
    Paint {
        clip: ClipId,
        lane: RollLane,
        path: Vec<(f64, u16)>,
    },
    /// Dragging a plugin parameter; applied live, one undo step.
    PluginParam {
        plugin: PluginId,
        param: String,
        start_value: f64,
        min: f64,
        max: f64,
        stepped: bool,
        merge_key: String,
    },
    /// Dragging the picker's or editor's scroll bar: the list scrolls by
    /// `per_px` for each pixel the pointer moves, up to `max`.
    OverlayScroll {
        start_scroll: f32,
        per_px: f32,
        max: f32,
    },
    /// Moving an automation point; committed as one `set_automation` on
    /// release.
    AutoPoint {
        track: TrackId,
        lane: usize,
        index: usize,
        /// Where it's being dragged to: time in seconds, lane value.
        time_s: f64,
        value: f64,
    },
    /// Dragging a synth parameter slider; applied live, one undo step.
    SynthParam {
        track: TrackId,
        param: SynthParam,
        start_pos: f32,
        merge_key: String,
    },
    /// Moving a point in the tempo lane (the start tempo only up and
    /// down); one `set_tempo_map` on release.
    TempoPoint {
        /// Into the tempo map's points: 0 is the start tempo.
        index: usize,
        /// Pointer beat minus the point's beat when the drag began.
        grab_beats: f64,
        start_bpm: f64,
        /// Where it's going.
        beat: f64,
        bpm: f64,
        /// The lane's BPM range when the drag began, so it holds still.
        scale: (f64, f64),
    },
    /// Moving a time signature change to another bar; one
    /// `set_time_signature_map` on release.
    MeterChange {
        /// Into the meter map's changes: 0 is the start (which stays).
        index: usize,
        /// Pointer beat minus the change's beat when the drag began.
        grab_beats: f64,
        /// The bar it's going to, from 0.
        bar: u32,
    },
    /// Dragging the latency slider of a track's MIDI device; applied live,
    /// one undo step.
    DeviceLatency {
        track: TrackId,
        start_pos: f32,
        merge_key: String,
    },
}

/// One of a clip's edges, dragged to trim it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClipEdge {
    Start,
    End,
}

/// Something that would discard unsaved changes, waiting for the user to
/// save them first, discard them, or cancel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingAction {
    /// Opening another project.
    OpenProject,
    /// Closing the window (quitting).
    Close,
}

/// A field being typed in: what it sets, and the text so far.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TextEdit {
    pub field: TextField,
    pub text: String,
    /// The text is still the value the field started with, all selected:
    /// typing replaces it.
    pub selected: bool,
}

/// What a text field sets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "index")]
pub enum TextField {
    /// The BPM of tempo point `n` (0 is the start tempo).
    Tempo(usize),
    /// The beats per bar of time signature `n` (0 is the start one).
    MeterNumerator(usize),
    /// The beat length of time signature `n`.
    MeterDenominator(usize),
    /// The latency set on a track's MIDI output, in ms.
    DeviceLatency(TrackId),
    /// A plugin parameter, typed the way the plugin shows its values.
    PluginParam { plugin: PluginId, param: String },
    /// The preferences' record offset, in ms (may be negative).
    RecordOffset,
    /// The port the HTTP MCP server listens on.
    McpPort,
}

/// Slider position (0 to 1, cubic) of a MIDI device latency in ms.
pub(crate) fn latency_position(ms: f64) -> f32 {
    ((ms / dawkey_core::DEVICE_LATENCY_MS_MAX).cbrt() as f32).clamp(0.0, 1.0)
}

/// The MIDI device latency at a slider position, to 0.1 ms.
pub(crate) fn latency_value(position: f32) -> f64 {
    let p = f64::from(position.clamp(0.0, 1.0));
    (dawkey_core::DEVICE_LATENCY_MS_MAX * p.powi(3) * 10.0).round() / 10.0
}

/// Synth settings with a slider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SynthParam {
    Attack,
    Decay,
    Sustain,
    Release,
    Cutoff,
    Resonance,
    FilterEnv,
}

impl SynthParam {
    pub(crate) const ALL: [SynthParam; 7] = [
        Self::Attack,
        Self::Decay,
        Self::Sustain,
        Self::Release,
        Self::Cutoff,
        Self::Resonance,
        Self::FilterEnv,
    ];

    /// The last part of its widget ID, `track/<id>/synth/<name>`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Attack => "attack",
            Self::Decay => "decay",
            Self::Sustain => "sustain",
            Self::Release => "release",
            Self::Cutoff => "cutoff",
            Self::Resonance => "resonance",
            Self::FilterEnv => "env",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    pub(crate) fn get(self, p: &dawkey_core::SynthParams) -> f32 {
        match self {
            Self::Attack => p.attack_ms,
            Self::Decay => p.decay_ms,
            Self::Sustain => p.sustain,
            Self::Release => p.release_ms,
            Self::Cutoff => p.cutoff_hz,
            Self::Resonance => p.resonance,
            Self::FilterEnv => p.filter_env_octaves,
        }
    }

    /// Slider position (0 to 1) of a value: cubic for times, logarithmic for
    /// the cutoff, linear otherwise.
    pub(crate) fn position(self, value: f32) -> f32 {
        let p = match self {
            Self::Attack => (value / 10_000.0).cbrt(),
            Self::Decay | Self::Release => (value / 20_000.0).cbrt(),
            Self::Cutoff => (value / 20.0).max(1.0).log10() / 3.0,
            Self::Sustain | Self::Resonance => value,
            Self::FilterEnv => value / 8.0,
        };
        p.clamp(0.0, 1.0)
    }

    /// The value at a slider position, rounded to a readable step.
    pub(crate) fn value(self, position: f32) -> f32 {
        let p = position.clamp(0.0, 1.0);
        let round = |v: f32, step: f32| (v / step).round() * step;
        match self {
            Self::Attack => round(10_000.0 * p.powi(3), 0.1),
            Self::Decay | Self::Release => round(20_000.0 * p.powi(3), 0.1),
            Self::Cutoff => round(20.0 * 1000f32.powf(p), 1.0).clamp(20.0, 20_000.0),
            Self::Sustain | Self::Resonance => round(p, 0.01),
            Self::FilterEnv => round(8.0 * p, 0.05),
        }
    }

    /// A short label with the value, e.g. "Rel 200 ms".
    pub(crate) fn text(self, value: f32) -> String {
        let ms = |v: f32| {
            if v >= 1000.0 {
                format!("{:.2} s", v / 1000.0)
            } else {
                format!("{v:.0} ms")
            }
        };
        match self {
            Self::Attack => format!("Att {}", ms(value)),
            Self::Decay => format!("Dec {}", ms(value)),
            Self::Sustain => format!("Sus {:.0}%", value * 100.0),
            Self::Release => format!("Rel {}", ms(value)),
            Self::Cutoff if value >= 1000.0 => format!("Cut {:.1} kHz", value / 1000.0),
            Self::Cutoff => format!("Cut {value:.0} Hz"),
            Self::Resonance => format!("Res {:.0}%", value * 100.0),
            Self::FilterEnv => format!("Env {value:.1} oct"),
        }
    }

    /// The `set_synth` that sets this parameter.
    pub(crate) fn command(self, track: TrackId, value: f32) -> dawkey_core::SetSynth {
        let mut c = dawkey_core::SetSynth::new(track);
        let field = match self {
            Self::Attack => &mut c.attack_ms,
            Self::Decay => &mut c.decay_ms,
            Self::Sustain => &mut c.sustain,
            Self::Release => &mut c.release_ms,
            Self::Cutoff => &mut c.cutoff_hz,
            Self::Resonance => &mut c.resonance,
            Self::FilterEnv => &mut c.filter_env_octaves,
        };
        *field = Some(value);
        c
    }
}

/// What the lane under the piano roll grid shows and edits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RollLane {
    #[default]
    Velocity,
    Control {
        controller: Controller,
    },
}

impl RollLane {
    /// The lanes offered before any others a clip uses.
    pub(crate) const COMMON: [RollLane; 6] = [
        Self::Velocity,
        Self::Control {
            controller: Controller::PitchBend,
        },
        Self::Control {
            controller: Controller::Cc { number: 1 },
        },
        Self::Control {
            controller: Controller::Cc { number: 11 },
        },
        Self::Control {
            controller: Controller::Cc { number: 64 },
        },
        Self::Control {
            controller: Controller::ChannelPressure,
        },
    ];

    /// Lowest and highest value.
    pub(crate) fn range(self) -> (u16, u16) {
        match self {
            Self::Velocity => (1, 127),
            Self::Control { controller } => (0, controller.max()),
        }
    }

    /// A short name that fits the lane's header.
    pub(crate) fn label(self) -> String {
        match self {
            Self::Velocity => "Vel".into(),
            Self::Control { controller } => match controller {
                Controller::PitchBend => "Bend".into(),
                Controller::ChannelPressure => "Press".into(),
                Controller::PolyPressure { pitch } => format!("PP {pitch}"),
                Controller::Program => "Prog".into(),
                Controller::Cc { number: 1 } => "Mod".into(),
                Controller::Cc { number: 7 } => "Vol".into(),
                Controller::Cc { number: 11 } => "Expr".into(),
                Controller::Cc { number: 64 } => "Sus".into(),
                Controller::Cc { number } => format!("CC {number}"),
                Controller::NotePitchBend { pitch } => format!("NB {pitch}"),
                Controller::NoteController { pitch, number } => format!("NC {pitch}/{number}"),
                Controller::NoteAssignable { pitch, number } => format!("NA {pitch}/{number}"),
            },
        }
    }

    /// The controller, for controller lanes.
    pub(crate) fn controller(self) -> Option<Controller> {
        match self {
            Self::Velocity => None,
            Self::Control { controller } => Some(controller),
        }
    }
}

/// A point selected in the tempo or time signature lane: an index into the
/// tempo map's points or the meter map's changes (0 is the start).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "index")]
pub enum Marker {
    Tempo(usize),
    Meter(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RangeKind {
    Loop,
    Punch,
}

/// View settings and selection.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ViewState {
    pub page: Page,
    /// Timeline zoom.
    pub px_per_s: f32,
    /// Time at the left edge of the timeline.
    pub scroll_s: f64,
    /// Vertical scroll of the track lanes, in logical px.
    pub scroll_y: f32,
    pub selected_clip: Option<ClipId>,
    /// The clip Ctrl+C copied, which Ctrl+V pastes.
    pub clipboard: Option<ClipId>,
    /// Snap clip moves to beats.
    pub snap: bool,
    /// The MIDI clip open in the piano roll.
    pub editing_clip: Option<ClipId>,
    /// Notes selected in the piano roll.
    pub selected_notes: Vec<NoteId>,
    pub piano_roll: RollView,
    /// What the lane under the piano roll grid shows.
    pub roll_lane: RollLane,
    /// The selected point in the piano roll's controller lane (an index
    /// into that controller's points).
    pub selected_control: Option<usize>,
    /// The plugin picker is open, adding to this track.
    pub plugin_picker: Option<TrackId>,
    /// The picker shows only instruments (true) or effects (false).
    pub picker_filter: Option<bool>,
    /// What's been typed in the picker to find a plugin (see `picker.rs`).
    pub picker_query: String,
    /// The highlighted plugin in the picker (an index into what it shows),
    /// which Return adds.
    pub picker_selected: usize,
    /// The plugin editor is open for this plugin.
    pub plugin_editor: Option<PluginId>,
    /// Vertical scroll of the open picker or editor, in logical px.
    pub overlay_scroll: f32,
    /// Tracks showing their automation lanes.
    pub automation_shown: Vec<TrackId>,
    /// Tracks showing their take lanes.
    pub takes_shown: Vec<TrackId>,
    /// The selected automation point: track, lane index, point index.
    pub selected_point: Option<(TrackId, usize, usize)>,
    /// The tempo and time signature lanes show under the ruler.
    pub tempo_shown: bool,
    /// The selected point in the tempo or time signature lane.
    pub selected_marker: Option<Marker>,
    /// The "save changes?" dialog is open, before doing this.
    pub confirm: Option<PendingAction>,
    /// A field being typed in.
    pub text_edit: Option<TextEdit>,
    /// Unsaved changes to offer to recover (the dialog is open).
    pub recovery: Option<dawkey_studio::Recovery>,
    /// The preferences panel is open.
    pub prefs_open: bool,
    /// The export dialog is open.
    pub export_dialog: bool,
    /// What the export dialog exports (kept between exports).
    pub export: ExportSettings,
    /// Moving faders, pan and plugin parameters while playing writes
    /// automation (the transport's "Auto").
    pub automation_write: bool,
    /// How it writes (the button next to "Auto").
    pub automation_mode: AutomationMode,
    #[serde(skip)]
    pub(crate) gesture: Option<Gesture>,
    /// The automation being written: the current gesture's, and in latch
    /// and write modes every lane written since playback started.
    #[serde(skip)]
    pub(crate) writing: Vec<AutomationWrite>,
    /// The write pass going on (latch and write modes), from the first
    /// lane it wrote until the transport stops.
    #[serde(skip)]
    pub(crate) pass: Option<AutomationPass>,
    /// Write passes so far, so each has its own undo step.
    #[serde(skip)]
    pub(crate) passes: u32,
}

/// What the export dialog writes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ExportSettings {
    pub format: dawkey_media::FileFormat,
    pub sample: dawkey_media::SampleFormat,
    /// The file's sample rate; `None` for the project's.
    pub sample_rate: Option<u32>,
    pub range: ExportRange,
    /// Seconds after the range to include, for tails to ring out.
    pub tail_s: f64,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            format: dawkey_media::FileFormat::Wav,
            sample: dawkey_media::SampleFormat::Int24,
            sample_rate: None,
            range: ExportRange::Song,
            tail_s: 0.0,
        }
    }
}

/// What part of the project an export covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportRange {
    /// From the start to where the last clip that plays ends.
    Song,
    /// The loop range.
    Loop,
}

/// How moving a control while playing writes its automation lane, with
/// "Auto" on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationMode {
    /// While a control is held; once let go, its lane plays as before.
    #[default]
    Touch,
    /// From when a control is grabbed until the transport stops: let go,
    /// it keeps writing the value it was left at.
    Latch,
    /// Every lane, and every control grabbed, from where playback starts
    /// until it stops; then back to touch, so the next pass doesn't
    /// overwrite everything again.
    Write,
}

impl AutomationMode {
    /// The next mode, for the button that cycles through them.
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Touch => Self::Latch,
            Self::Latch => Self::Write,
            Self::Write => Self::Touch,
        }
    }

    /// The button's text.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Touch => "Touch",
            Self::Latch => "Latch",
            Self::Write => "Write",
        }
    }
}

/// A stretch of a lane being written, from where it started to the
/// playhead.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AutomationWrite {
    pub(crate) track: TrackId,
    pub(crate) target: dawkey_core::AutomationTarget,
    pub(crate) from_s: f64,
    /// (time, value), in time order.
    pub(crate) points: Vec<(f64, f64)>,
}

/// A latch or write pass: one undo step for everything it writes (unless
/// something else is done in between).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AutomationPass {
    pub(crate) merge_key: String,
    /// Every lane is being written (write mode).
    pub(crate) every_lane: bool,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            page: Page::Arrange,
            px_per_s: 80.0,
            scroll_s: 0.0,
            scroll_y: 0.0,
            selected_clip: None,
            clipboard: None,
            snap: true,
            editing_clip: None,
            selected_notes: Vec::new(),
            piano_roll: RollView::default(),
            roll_lane: RollLane::default(),
            selected_control: None,
            plugin_picker: None,
            picker_filter: None,
            picker_query: String::new(),
            picker_selected: 0,
            plugin_editor: None,
            overlay_scroll: 0.0,
            automation_shown: Vec::new(),
            takes_shown: Vec::new(),
            selected_point: None,
            tempo_shown: false,
            selected_marker: None,
            confirm: None,
            text_edit: None,
            recovery: None,
            prefs_open: false,
            export_dialog: false,
            export: ExportSettings::default(),
            automation_write: false,
            automation_mode: AutomationMode::default(),
            gesture: None,
            writing: Vec::new(),
            pass: None,
            passes: 0,
        }
    }
}

/// Zoom, scroll and grid of the piano roll. Times are beats (quarter
/// notes) from the clip's start.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RollView {
    pub px_per_beat: f32,
    /// Beat at the left edge of the grid.
    pub scroll_beats: f64,
    /// Vertical scroll in logical px; 0 shows pitch 127 at the top.
    pub scroll_y: f32,
    /// Grid (and snap) size in beats: 0.25 is sixteenth notes.
    pub grid_beats: f64,
}

impl Default for RollView {
    fn default() -> Self {
        Self {
            px_per_beat: 80.0,
            scroll_beats: 0.0,
            // C6 at the top.
            scroll_y: (127 - 84) as f32 * ROW_H,
            grid_beats: 0.25,
        }
    }
}

/// Something only the platform can do (show a dialog). Headless instances
/// report these to MCP clients instead.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "request")]
pub enum PlatformRequest {
    /// Let the user pick audio files to import at this place.
    ChooseAudioFiles {
        track: Option<TrackId>,
        position_s: f64,
    },
    ChooseProjectToOpen,
    ChooseSaveLocation {
        /// What to do once the project is saved (from the "save changes?"
        /// dialog).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<PendingAction>,
    },
    /// Let the user pick a folder to look for plugins of `kind` in (then
    /// [`App::add_plugin_folder`](crate::App::add_plugin_folder)).
    ChooseFolder {
        kind: crate::prefs::PluginKind,
    },
    /// Let the user pick where to export the mix (then
    /// [`App::start_export`](crate::App::start_export)).
    ChooseExportLocation {
        /// A file name to offer, e.g. "Song.flac".
        name: String,
        format: dawkey_media::FileFormat,
    },
}

/// An audio or MIDI file waiting to be decoded and placed on the timeline.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportRequest {
    pub path: PathBuf,
    /// Audio track to place it on; `None` makes a new track. (MIDI files
    /// always get new tracks.)
    pub track: Option<TrackId>,
    pub position_s: f64,
}

impl ImportRequest {
    /// A Standard MIDI File (by its extension), not audio.
    pub fn is_midi(&self) -> bool {
        self.path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mid" | "midi" | "smf"))
    }
}
