// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tool names, argument types and the published tool list. Command tools are
//! generated from [`dawkey_core::COMMAND_SPECS`]; the rest are defined here.

use std::path::PathBuf;
use std::sync::Arc;

use dawkey_core::{COMMAND_SPECS, CommandSpec, Effect, TrackId};
use rmcp::model::{Tool, ToolAnnotations};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const GET_STATE: &str = "get_state";
pub(crate) const UNDO: &str = "undo";
pub(crate) const REDO: &str = "redo";
pub(crate) const NEW_PROJECT: &str = "new_project";
pub(crate) const OPEN_PROJECT: &str = "open_project";
pub(crate) const SAVE_PROJECT: &str = "save_project";
pub(crate) const IMPORT_AUDIO: &str = "import_audio";
pub(crate) const IMPORT_MIDI: &str = "import_midi";
pub(crate) const PLAY: &str = "play";
pub(crate) const STOP: &str = "stop";
pub(crate) const RECORD: &str = "record";
pub(crate) const SET_RECORD_OFFSET: &str = "set_record_offset";
pub(crate) const LOCATE: &str = "locate";
pub(crate) const GET_ENGINE_STATUS: &str = "get_engine_status";
pub(crate) const GET_METERS: &str = "get_meters";
pub(crate) const ADVANCE_CLOCK: &str = "advance_clock";
pub(crate) const RENDER_RANGE: &str = "render_range";
pub(crate) const EXPORT_AUDIO: &str = "export_audio";
pub(crate) const UI_TREE: &str = "ui_tree";
pub(crate) const UI_CLICK: &str = "ui_click";
pub(crate) const UI_SCREENSHOT: &str = "ui_screenshot";
pub(crate) const UI_RESIZE: &str = "ui_resize";
pub(crate) const UI_DRAG: &str = "ui_drag";
pub(crate) const UI_SCROLL: &str = "ui_scroll";
pub(crate) const UI_HOVER: &str = "ui_hover";
pub(crate) const UI_KEY: &str = "ui_key";
pub(crate) const UI_TYPE: &str = "ui_type";
pub(crate) const UI_DROP_FILES: &str = "ui_drop_files";
pub(crate) const LIST_PLUGINS: &str = "list_plugins";
pub(crate) const RESCAN_PLUGINS: &str = "rescan_plugins";
pub(crate) const RELOAD_PLUGIN: &str = "reload_plugin";
pub(crate) const SHOW_PLUGIN_GUI: &str = "show_plugin_gui";
pub(crate) const GET_PLUGIN: &str = "get_plugin";
pub(crate) const GET_CHANGES: &str = "get_changes";
pub(crate) const BATCH: &str = "batch";

/// Takes no arguments.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoArgs {}

/// Get the open project (tracks, clips, audio pool) and session status
/// (path, unsaved changes, undo/redo). The whole project by default, which
/// can be large: `notes: false`, `controls: false` and `automation: false`
/// (or `summary: true` for all three) replace notes, controller points and
/// automation points with their counts, and `tracks` keeps only some
/// tracks. `summary.left_out` says what was left out.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetState {
    /// Include each MIDI clip's notes. Default true; false gives
    /// `note_count` and `pitch_range` instead.
    #[serde(default = "yes")]
    pub(crate) notes: bool,
    /// Include each MIDI clip's controller points. Default true; false
    /// gives `control_count` instead.
    #[serde(default = "yes")]
    pub(crate) controls: bool,
    /// Include automation lanes' points. Default true; false gives each
    /// lane's `point_count` instead.
    #[serde(default = "yes")]
    pub(crate) automation: bool,
    /// Only these tracks (the master always comes along).
    #[serde(default)]
    pub(crate) tracks: Option<Vec<TrackId>>,
    /// A compact state: like `notes`, `controls` and `automation` all false.
    #[serde(default)]
    pub(crate) summary: bool,
}

/// What changed the project since a revision you know (every result that
/// changes the project carries `revision`): each undo step, undo, redo and
/// source added since, oldest first, with its name and the tracks, clips,
/// notes and plugin slots it touched, and `also` for the tempo, meter, loop,
/// punch, name, plugin chains and automation. Use it to catch up with what
/// the window or another client did. Every result also carries `instance`,
/// which changes when Dawkey restarts; a new instance or another project
/// starts the revisions over (`complete: false`; call get_state then).
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetChanges {
    /// The last revision you saw.
    pub(crate) since_revision: u64,
}

/// Start a new, empty project, replacing the open one. Returns a summary of
/// the project, as get_state with `summary: true`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct NewProject {
    /// Project name. Defaults to "Untitled".
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// Required to replace a project with unsaved changes.
    #[serde(default)]
    pub(crate) discard_changes: bool,
}

/// Open a project bundle (a `.dawkey` directory), replacing the open
/// project. Returns a summary of the project, as get_state with `summary:
/// true` (counts instead of notes, controller and automation points; call
/// get_state for them).
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenProject {
    /// Path to the `.dawkey` bundle.
    pub(crate) path: PathBuf,
    /// Required to replace a project with unsaved changes.
    #[serde(default)]
    pub(crate) discard_changes: bool,
}

/// Save the project. Audio files from outside the bundle are copied into it.
/// Returns the bundle path written.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SaveProject {
    /// Where to save. A `.dawkey` extension is added if missing. Defaults to
    /// the project's current path; required if it has never been saved.
    #[serde(default)]
    pub(crate) path: Option<PathBuf>,
}

/// Import an audio file (WAV, FLAC, MP3, Ogg, AIFF, ...) into the pool and
/// place it on the timeline. It is converted to the project sample rate.
/// Returns the new source, clip and track IDs. Undo removes the clip; the
/// source stays in the pool for add_clip.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportAudio {
    /// Path to the audio file.
    pub(crate) path: PathBuf,
    /// Audio track to place it on. Defaults to a new track named after the
    /// file.
    #[serde(default)]
    pub(crate) track: Option<TrackId>,
    /// Timeline position in seconds. Defaults to 0.
    #[serde(default)]
    pub(crate) position_s: f64,
}

/// Import a Standard MIDI File (.mid): each of its tracks (split by MIDI
/// channel) becomes a new MIDI track with one clip, with its notes and
/// controllers (pitch bend, CCs, channel pressure), unless `targets` maps
/// it onto an existing MIDI track, which keeps its plugins, mix and
/// routing and gets the clip in place of its clips there (or beside them
/// with `replace: false`). One undo step. Returns the track and clip IDs
/// (`tracks`, `clips`), and the clips replaced (`removed_clips`).
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportMidi {
    /// Path to the .mid file.
    pub(crate) path: PathBuf,
    /// Timeline position of the file's start, in seconds. Defaults to 0.
    #[serde(default)]
    pub(crate) position_s: f64,
    /// File tracks to put on existing MIDI tracks. The rest become new
    /// tracks.
    #[serde(default)]
    pub(crate) targets: Vec<ImportMidiTarget>,
    /// Whether a mapped track's clips that the imported clip overlaps are
    /// removed (default), or kept beside it.
    #[serde(default = "yes")]
    pub(crate) replace: bool,
    /// Take the file's tempo map and time signature, so it plays as
    /// written (MIDI clips already in the project keep their beat
    /// positions). Defaults to true if the project has no clips yet;
    /// otherwise the notes keep their beat positions at the project's
    /// tempo.
    #[serde(default)]
    pub(crate) use_file_tempo: Option<bool>,
}

/// A MIDI file track and the project track it goes on.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportMidiTarget {
    /// Index of the track in the file, counting only tracks with notes
    /// (one per file track and channel), from 0; as a plain import lists
    /// them in `tracks`.
    #[serde(default)]
    pub(crate) file_track: Option<usize>,
    /// Or the track's name in the file (the first with that name).
    #[serde(default)]
    pub(crate) file_track_name: Option<String>,
    /// The existing MIDI track to put its clip on.
    pub(crate) track: TrackId,
}

/// Plugin kinds for list_plugins.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PluginKind {
    /// Takes notes and makes sound.
    Instrument,
    /// Takes notes and sends MIDI (arpeggiators, chord makers, ...).
    MidiEffect,
    /// Processes audio only.
    Effect,
}

/// List the installed CLAP, LV2 and VST3 plugins, for add_plugin. The list is
/// cached; rescan_plugins refreshes it after installing plugins. Each entry
/// is compact (ID, name, vendor, format, kind) unless `detail` or `params`
/// is set; `total` counts the matches, and `first`/`count` page through
/// them (`next` is the `first` for the next page, when there is one).
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListPlugins {
    /// Only plugins whose name, vendor or ID contains this (any case).
    #[serde(default)]
    pub(crate) query: Option<String>,
    #[serde(default)]
    pub(crate) kind: Option<PluginKind>,
    /// Include each plugin's parameters (IDs, names, ranges). Implies
    /// `detail`.
    #[serde(default)]
    pub(crate) params: bool,
    /// Include each plugin's ports, sidechain, outputs and parameter count.
    #[serde(default)]
    pub(crate) detail: bool,
    /// Index of the first match to list. Defaults to 0.
    #[serde(default)]
    pub(crate) first: usize,
    /// How many matches to list. Defaults to 100.
    #[serde(default)]
    pub(crate) count: Option<usize>,
}

/// Get a plugin slot on a track: which plugin, whether it's running, and
/// its parameters (IDs, names, ranges, current values, whether automated).
/// Some plugins have thousands of parameters: `param_count` says how many,
/// and `first`/`count` page through them.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetPlugin {
    /// The slot's ID (from add_plugin or get_state).
    pub(crate) plugin: dawkey_core::PluginId,
    /// Index of the first parameter to list. Defaults to 0.
    #[serde(default)]
    pub(crate) first: usize,
    /// How many parameters to list. Defaults to 200.
    #[serde(default)]
    pub(crate) count: Option<usize>,
}

/// Load a plugin slot's plugin again, from its saved state: one that
/// crashed (get_plugin's `error` says so; the track's audio passes through
/// it meanwhile) or that failed to load. Not an undo step.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReloadPlugin {
    /// The slot's ID.
    pub(crate) plugin: dawkey_core::PluginId,
}

/// Show a plugin's own GUI in a window on the desktop, or hide it (plugins
/// that run in processes of their own; get_plugin's `has_gui` says which
/// have one). What's changed on it goes into the project like any change.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShowPluginGui {
    /// The slot's ID.
    pub(crate) plugin: dawkey_core::PluginId,
    /// Show it (default) or hide it.
    #[serde(default = "yes")]
    pub(crate) show: bool,
}

fn yes() -> bool {
    true
}

/// Move the playhead.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Locate {
    /// New playhead position in seconds.
    pub(crate) position_s: f64,
}

/// Virtual audio backend only: run the engine for this much time, as if the
/// audio device had played it, and analyze what it output. Use with play,
/// locate and get_meters to test playback deterministically. Also returns
/// the MIDI sent to devices (tracks with set_track_midi_output) under
/// `midi_output`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdvanceClock {
    /// Seconds of audio to run (up to 600).
    pub(crate) seconds: f64,
    /// An audio file to feed to the inputs from now on (then silence), for
    /// testing recording.
    #[serde(default)]
    pub(crate) input_path: Option<PathBuf>,
    /// Notes to play into the MIDI input, as from a keyboard, for testing
    /// armed MIDI tracks and MIDI recording. Note-offs after this advance
    /// arrive during later ones.
    #[serde(default)]
    pub(crate) midi_input: Vec<MidiInputNote>,
    /// Controller changes to play into the MIDI input, as from a keyboard:
    /// pitch bend, CCs, pressure, programs and MIDI 2.0 per-note
    /// controllers (see set_midi_controls for the kinds and ranges).
    #[serde(default)]
    pub(crate) midi_controls: Vec<MidiInputControl>,
    /// MIDI 2.0 per-note management messages to play into the MIDI input,
    /// as from a MIDI 2.0 keyboard.
    #[serde(default)]
    pub(crate) midi_management: Vec<MidiInputManagement>,
}

/// A MIDI 2.0 per-note management message from the virtual MIDI keyboard.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MidiInputManagement {
    /// When, in seconds from the start of this advance.
    pub(crate) at_s: f64,
    /// The key, 0 to 127.
    pub(crate) pitch: u8,
    /// The per-note controllers sent so far stay with the key's sounding
    /// notes; later ones only reach later notes.
    #[serde(default)]
    pub(crate) detach: bool,
    /// The key's per-note controllers go back to their defaults.
    #[serde(default)]
    pub(crate) reset: bool,
    /// The device it comes from (see midi_input).
    #[serde(default)]
    pub(crate) device: Option<String>,
    /// MIDI channel, 1 to 16. Defaults to 1.
    #[serde(default)]
    pub(crate) channel: Option<u8>,
}

impl MidiInputManagement {
    /// The message, as (seconds from now, bytes, 32-bit value).
    pub(crate) fn event(&self) -> Result<(f64, [u8; 3], u32), String> {
        if self.at_s < 0.0 || !self.at_s.is_finite() {
            return Err("at_s must be at least 0".into());
        }
        if self.pitch > 127 {
            return Err(format!("pitch must be 0 to 127, got {}", self.pitch));
        }
        let channel = self.channel.unwrap_or(1);
        if !(1..=16).contains(&channel) {
            return Err(format!("channel must be 1 to 16, got {channel}"));
        }
        let flags = if self.detach {
            dawkey_core::PER_NOTE_DETACH
        } else {
            0
        } | if self.reset {
            dawkey_core::PER_NOTE_RESET
        } else {
            0
        };
        let status = dawkey_core::PER_NOTE_MANAGEMENT | (channel - 1);
        Ok((self.at_s, [status, self.pitch, flags], 0))
    }
}

/// A controller change from the virtual MIDI keyboard.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MidiInputControl {
    /// When, in seconds from the start of this advance.
    pub(crate) at_s: f64,
    pub(crate) controller: dawkey_core::Controller,
    pub(crate) value: u16,
    /// The device it comes from (see midi_input).
    #[serde(default)]
    pub(crate) device: Option<String>,
    /// MIDI channel, 1 to 16. Defaults to 1.
    #[serde(default)]
    pub(crate) channel: Option<u8>,
}

impl MidiInputControl {
    /// The message, as (seconds from now, bytes, 32-bit value).
    pub(crate) fn event(&self) -> Result<(f64, [u8; 3], u32), String> {
        let c = self.controller;
        if !c.valid() || self.value > c.max() {
            return Err(format!(
                "controller {c:?} is out of range, or its value isn't 0 to {}",
                c.max()
            ));
        }
        if self.at_s < 0.0 || !self.at_s.is_finite() {
            return Err("at_s must be at least 0".into());
        }
        let channel = self.channel.unwrap_or(1);
        if !(1..=16).contains(&channel) {
            return Err(format!("channel must be 1 to 16, got {channel}"));
        }
        let data = c.message(channel - 1, self.value);
        Ok((self.at_s, data, c.value32(self.value)))
    }
}

/// A key played on the virtual MIDI keyboard.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MidiInputNote {
    /// When the key goes down, in seconds from the start of this advance.
    pub(crate) at_s: f64,
    /// MIDI pitch, 0 to 127 (60 is middle C).
    pub(crate) pitch: u8,
    /// How long the key is held, in seconds.
    pub(crate) length_s: f64,
    /// Velocity, 1 to 127. Defaults to 100.
    #[serde(default)]
    pub(crate) velocity: Option<u8>,
    /// The device it's played on, as a track's set_track_midi_input names
    /// it. Defaults to an unnamed device (heard by tracks that listen to
    /// every device).
    #[serde(default)]
    pub(crate) device: Option<String>,
    /// MIDI channel, 1 to 16. Defaults to 1.
    #[serde(default)]
    pub(crate) channel: Option<u8>,
}

impl MidiInputNote {
    /// Note on and note off, as (seconds from now, bytes, 32-bit value).
    pub(crate) fn events(&self) -> Result<[(f64, [u8; 3], u32); 2], String> {
        let velocity = self.velocity.unwrap_or(100);
        if self.pitch > 127 || !(1..=127).contains(&velocity) {
            return Err(format!(
                "pitch must be 0 to 127 and velocity 1 to 127, got {} and {velocity}",
                self.pitch
            ));
        }
        if !(self.at_s >= 0.0 && self.length_s > 0.0) {
            return Err("at_s must be at least 0 and length_s more than 0".into());
        }
        let channel = self.channel.unwrap_or(1);
        if !(1..=16).contains(&channel) {
            return Err(format!("channel must be 1 to 16, got {channel}"));
        }
        let channel = channel - 1;
        Ok([
            (self.at_s, [0x90 | channel, self.pitch, velocity], 0),
            (
                self.at_s + self.length_s,
                [0x80 | channel, self.pitch, 0],
                0,
            ),
        ])
    }
}

/// Correct the recording latency compensation: recordings are shifted
/// earlier by the backend's estimate plus this many milliseconds.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetRecordOffset {
    pub(crate) ms: f64,
}

/// Render part of the project offline (independent of the transport) and
/// analyze it: peak and RMS per channel in dBFS, integrated loudness (LUFS),
/// levels in three bands (`bands`, a rough brightness), and a hash for
/// comparing renders. Optionally write it to a WAV file. `tracks` renders
/// only those tracks (as soloing them would: through their buses and the
/// master); `stems: true` renders each track separately (one render per
/// track, `stems` in the result, each with its analysis, and a file each in
/// `stems_dir`). `segment_length_s` (or `_beats`), `segment_bars` or
/// `segments` add an analysis per segment, to see how levels and loudness
/// change over time.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenderRange {
    /// Start of the range in seconds.
    pub(crate) start_s: f64,
    /// End of the range in seconds (at most 600 s after the start).
    pub(crate) end_s: f64,
    /// If given, also write the render to this path as a 32-bit float
    /// stereo WAV file.
    #[serde(default)]
    pub(crate) output_path: Option<PathBuf>,
    /// Render only these tracks (and the buses and master they go through),
    /// as if they were soloed. With `stems`, the tracks to render stems of.
    #[serde(default)]
    pub(crate) tracks: Option<Vec<TrackId>>,
    /// Render each track separately, soloed in turn: every audio and MIDI
    /// track that isn't muted, or `tracks`. The result's `stems` has one
    /// entry per track with its analysis.
    #[serde(default)]
    pub(crate) stems: bool,
    /// With `stems`, write each stem to this folder as
    /// `<track id>-<track name>.wav` (32-bit float).
    #[serde(default)]
    pub(crate) stems_dir: Option<PathBuf>,
    /// Analyze the range in segments this long (seconds), from its start.
    #[serde(default)]
    pub(crate) segment_length_s: Option<f64>,
    /// Analyze the range in segments of this many bars, cut at bar lines.
    #[serde(default)]
    pub(crate) segment_bars: Option<u32>,
    /// Analyze these parts of the range separately (absolute seconds).
    #[serde(default)]
    pub(crate) segments: Option<Vec<SegmentRange>>,
}

/// A part of a render to analyze on its own.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SegmentRange {
    pub(crate) start_s: f64,
    pub(crate) end_s: f64,
}

/// Export the mix, or part of it, to an audio file, rendered offline: WAV
/// (16 or 24-bit, or 32-bit float) or FLAC (16 or 24-bit). By default the
/// whole song, from the start to where the last clip ends, in 24-bit at the
/// project's sample rate. Reports the levels and loudness of what was
/// written. Whole-number samples are dithered. `tracks` exports only those
/// tracks (as soloing them would); `stems: true` writes a file per track
/// beside `path` (`<name>-<track id>-<track name>.<ext>`), listed under
/// `stems`. `segment_length_s` (or `_beats`), `segment_bars` or `segments`
/// add an analysis per segment. `loop: true` makes a file that loops
/// seamlessly: the range plus `tail_s` is rendered, the tail is added onto
/// the start, and exactly the range's length is written.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExportAudio {
    /// Where to write it. Its extension (.wav or .flac) picks the format
    /// unless `format` does.
    pub(crate) path: PathBuf,
    #[serde(default)]
    pub(crate) format: Option<ExportFormat>,
    /// 16, 24 or 32 (32-bit float, WAV only). Default 24.
    #[serde(default)]
    pub(crate) bit_depth: Option<u32>,
    /// The file's sample rate in Hz; the mix is converted to it. Default:
    /// the project's.
    #[serde(default)]
    pub(crate) sample_rate: Option<u32>,
    /// Start in seconds. Default 0.
    #[serde(default)]
    pub(crate) start_s: Option<f64>,
    /// End in seconds. Default: where the last clip that plays ends.
    #[serde(default)]
    pub(crate) end_s: Option<f64>,
    /// Seconds after the end to include, so reverb and delay tails ring
    /// out (with `loop`, wrapped onto the start instead). Default 0.
    #[serde(default)]
    pub(crate) tail_s: Option<f64>,
    /// Make a seamless loop of the range (the loop range, or the song):
    /// what rings out past the end for `tail_s` is added onto the start, so
    /// the decay carries across the seam when a player loops the file, and
    /// the file is exactly the range long. The result's `seam` says how big
    /// the jump from the last sample to the first is, to check for a click.
    /// The range can be at most 600 s.
    #[serde(default, rename = "loop")]
    pub(crate) wrap: bool,
    /// Export only these tracks (and the buses and master they go through),
    /// as if they were soloed. With `stems`, the tracks to export stems of.
    #[serde(default)]
    pub(crate) tracks: Option<Vec<TrackId>>,
    /// Export each track to a file of its own, soloed in turn: every audio
    /// and MIDI track that isn't muted, or `tracks`. The mix itself isn't
    /// written then.
    #[serde(default)]
    pub(crate) stems: bool,
    /// Analyze the export in segments this long (seconds), from its start.
    #[serde(default)]
    pub(crate) segment_length_s: Option<f64>,
    /// Analyze the export in segments of this many bars, cut at bar lines.
    #[serde(default)]
    pub(crate) segment_bars: Option<u32>,
    /// Analyze these parts of the export separately (absolute seconds).
    #[serde(default)]
    pub(crate) segments: Option<Vec<SegmentRange>>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExportFormat {
    Wav,
    Flac,
}

/// Mouse button for synthetic clicks.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Button {
    #[default]
    Left,
    Right,
    Middle,
}

impl From<Button> for dawkey_ui::MouseButton {
    fn from(b: Button) -> Self {
        match b {
            Button::Left => Self::Left,
            Button::Right => Self::Right,
            Button::Middle => Self::Middle,
        }
    }
}

/// Modifier keys held during synthetic input.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Mods {
    #[serde(default)]
    pub(crate) ctrl: bool,
    #[serde(default)]
    pub(crate) shift: bool,
    #[serde(default)]
    pub(crate) alt: bool,
}

impl From<Mods> for dawkey_ui::Modifiers {
    fn from(m: Mods) -> Self {
        Self {
            ctrl: m.ctrl,
            shift: m.shift,
            alt: m.alt,
            logo: false,
        }
    }
}

/// Click a widget by its ID (from ui_tree), or a window position. Returns
/// any window actions it triggered (e.g. close) and any dialogs it asked for
/// (headless instances can't show them). Double-click a MIDI clip to open it
/// in the piano roll, an empty MIDI lane to make a clip, or the piano roll's
/// grid to add a note.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiClick {
    /// Widget ID, e.g. "transport/play" or "track/3/mute". Clicks its center.
    #[serde(default)]
    pub(crate) id: Option<String>,
    /// Window x in logical px (instead of `id`).
    #[serde(default)]
    pub(crate) x: Option<f32>,
    /// Window y in logical px (instead of `id`).
    #[serde(default)]
    pub(crate) y: Option<f32>,
    #[serde(default)]
    pub(crate) button: Button,
    #[serde(default)]
    pub(crate) modifiers: Mods,
    /// Double-click (left button only).
    #[serde(default)]
    pub(crate) double: bool,
}

/// Drag with the left button from a widget's center (or a position) by
/// (dx, dy) logical px. Clips, faders, pan sliders, the ruler, piano roll
/// notes (note/<id> moves, note/<id>/end resizes) and synth sliders are
/// draggable. A dragged clip lands in the nearest space it fits in; with
/// ctrl, dragging clip/<id> drops a copy, and dragging clip/<id>/end
/// repeats the clip up to there. A drag is one undo step.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiDrag {
    /// Widget to start on, e.g. "clip/2" or "track/1/fader".
    #[serde(default)]
    pub(crate) id: Option<String>,
    /// Start x in logical px (instead of `id`).
    #[serde(default)]
    pub(crate) x: Option<f32>,
    /// Start y in logical px (instead of `id`).
    #[serde(default)]
    pub(crate) y: Option<f32>,
    #[serde(default)]
    pub(crate) dx: f32,
    #[serde(default)]
    pub(crate) dy: f32,
    /// Pointer moves along the way. Defaults to 8.
    #[serde(default)]
    pub(crate) steps: Option<u32>,
    /// E.g. shift to disable snapping.
    #[serde(default)]
    pub(crate) modifiers: Mods,
}

/// Move the pointer over a widget (its center) or a window position,
/// without pressing a button, as before pasting a clip there with "ctrl+v"
/// (ui_key).
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiHover {
    /// Widget ID, e.g. "track/3/lane".
    #[serde(default)]
    pub(crate) id: Option<String>,
    /// Window x in logical px (instead of `id`).
    #[serde(default)]
    pub(crate) x: Option<f32>,
    /// Window y in logical px (instead of `id`).
    #[serde(default)]
    pub(crate) y: Option<f32>,
}

/// Scroll at a window position. Over the timeline: scroll moves vertically,
/// shift scrolls horizontally, ctrl zooms around the pointer.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiScroll {
    pub(crate) x: f32,
    pub(crate) y: f32,
    /// Horizontal scroll in logical px.
    #[serde(default)]
    pub(crate) dx: f32,
    /// Vertical scroll in logical px (positive = towards the end).
    #[serde(default)]
    pub(crate) dy: f32,
    #[serde(default)]
    pub(crate) modifiers: Mods,
}

/// Press a key, optionally with modifiers: "space" (play/stop), "home",
/// "ctrl+z" (undo), "ctrl+shift+z" (redo), "ctrl+s" (save), "ctrl+o"
/// (open), "ctrl+i" (import), "delete" (remove selected clip), "ctrl+c"
/// (copy the selected clip), "ctrl+v" (paste it at the pointer, see
/// ui_hover: onto the track there, else its own track; in the nearest space
/// it fits in), "tab"
/// (arrange/mixer), "return" (open the selected MIDI clip in the piano
/// roll), "+"/"-" (zoom), "escape". In the piano roll: "delete" removes the
/// selected notes, "up"/"down" transpose them ("shift+up" by an octave),
/// "left"/"right" move them by the grid, "ctrl+a" selects all, "q"
/// quantizes, "escape" deselects or closes. In the plugin picker, keys
/// search (ui_type), "up"/"down" move the highlight, "return" adds the
/// highlighted plugin, and "escape" clears the search, then closes.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiKey {
    /// The key, with optional "ctrl+", "shift+", "alt+" prefixes.
    pub(crate) key: String,
}

/// Type text into the window, one key per character, as into a field
/// clicked first with ui_click (e.g. tempo/value, the tempo lane's BPM;
/// meter/numerator/value and meter/denominator/value; a double-clicked
/// track/<id>/midi_out_latency). Finish with "return" to set the value (ui_key), or "escape" to cancel.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiType {
    pub(crate) text: String,
}

/// Drop audio files onto the window at a position, as from a file manager:
/// on a lane they land on that track at that time (snapped), elsewhere on
/// new tracks at the playhead. Imports them before returning.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiDropFiles {
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) x: f32,
    pub(crate) y: f32,
}

/// Render the window to a PNG image.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiScreenshot {
    /// Physical pixels per logical pixel (0.25 to 4). Defaults to 1.
    #[serde(default)]
    pub(crate) scale: Option<f32>,
}

/// Headless only: resize the virtual window, in logical pixels (including
/// the 16 px shadow margin).
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiResize {
    pub(crate) width: f32,
    pub(crate) height: f32,
}

pub(crate) fn build_tools() -> Vec<Tool> {
    let read_only = ToolAnnotations::new().read_only(true).open_world(false);
    let mutating = ToolAnnotations::new()
        .read_only(false)
        .destructive(false)
        .open_world(false);
    let mut tools = vec![
        tool::<GetState>(GET_STATE, "", read_only.clone()),
        tool::<NoArgs>(UNDO, "Undo the last change.", mutating.clone()),
        tool::<NoArgs>(REDO, "Redo the last undone change.", mutating.clone()),
        tool::<NewProject>(NEW_PROJECT, "", mutating.clone().destructive(true)),
        tool::<OpenProject>(OPEN_PROJECT, "", mutating.clone().destructive(true)),
        tool::<SaveProject>(SAVE_PROJECT, "", mutating.clone().idempotent(true)),
        tool::<ImportAudio>(IMPORT_AUDIO, "", mutating.clone().idempotent(false)),
        tool::<ImportMidi>(IMPORT_MIDI, "", mutating.clone().idempotent(false)),
        tool::<NoArgs>(PLAY, "Start playback.", mutating.clone().idempotent(true)),
        tool::<NoArgs>(
            STOP,
            "Stop playback (and recording). The playhead stays where it is. When recording, \
             the takes become clips on their tracks (one undo step); earlier loop passes are \
             placed muted.",
            mutating.clone().idempotent(true),
        ),
        tool::<NoArgs>(
            RECORD,
            "Start recording on every armed track (set_track_arm), and playback. Respects \
             the loop and punch ranges. Stop with stop.",
            mutating.clone().idempotent(true),
        ),
        tool::<SetRecordOffset>(SET_RECORD_OFFSET, "", mutating.clone().idempotent(true)),
        tool::<Locate>(LOCATE, "", mutating.clone().idempotent(true)),
        tool::<NoArgs>(
            GET_ENGINE_STATUS,
            "Get the audio engine's status: backend (virtual or pipewire), sample rate, \
             transport (playing, position), PipeWire node state, and sources whose audio \
             could not be loaded.",
            read_only.clone(),
        ),
        tool::<NoArgs>(
            GET_METERS,
            "Get peak levels (dBFS, null = silence) of the master bus and each track since \
             the previous get_meters call.",
            read_only.clone(),
        ),
        tool::<AdvanceClock>(ADVANCE_CLOCK, "", mutating.clone()),
        tool::<RenderRange>(RENDER_RANGE, "", mutating.clone().idempotent(true)),
        tool::<ExportAudio>(EXPORT_AUDIO, "", mutating.clone().idempotent(true)),
        tool::<NoArgs>(
            UI_TREE,
            "Get the window state (size, scale, maximized, client-side decorations) and its \
             widget tree under `root`: stable IDs, roles, names, values, bounds (logical px), \
             and toggle/enabled state.",
            read_only.clone(),
        ),
        tool::<UiClick>(UI_CLICK, "", mutating.clone()),
        tool::<UiDrag>(UI_DRAG, "", mutating.clone()),
        tool::<UiScroll>(UI_SCROLL, "", mutating.clone()),
        tool::<UiHover>(UI_HOVER, "", mutating.clone()),
        tool::<UiKey>(UI_KEY, "", mutating.clone()),
        tool::<UiType>(UI_TYPE, "", mutating.clone()),
        tool::<UiDropFiles>(UI_DROP_FILES, "", mutating.clone()),
        tool::<UiScreenshot>(UI_SCREENSHOT, "", read_only.clone()),
        tool::<UiResize>(UI_RESIZE, "", mutating.clone().idempotent(true)),
        tool::<ListPlugins>(LIST_PLUGINS, "", read_only.clone()),
        tool::<NoArgs>(
            RESCAN_PLUGINS,
            "Scan for CLAP, LV2 and VST3 plugins again (after installing or removing some). \
             Each CLAP file and VST3 bundle is scanned in its own process, so a broken plugin \
             is only reported. Returns how many were found, and what failed.",
            mutating.clone().idempotent(true),
        ),
        tool::<ReloadPlugin>(RELOAD_PLUGIN, "", mutating.clone().idempotent(true)),
        tool::<ShowPluginGui>(SHOW_PLUGIN_GUI, "", mutating.clone().idempotent(true)),
        tool::<GetPlugin>(GET_PLUGIN, "", read_only.clone()),
        tool::<GetChanges>(GET_CHANGES, "", read_only),
        tool::<crate::batch::Batch>(BATCH, "", mutating),
    ];
    tools.extend(COMMAND_SPECS.iter().map(command_tool));
    tools
}

fn command_tool(spec: &CommandSpec) -> Tool {
    let base = ToolAnnotations::new().read_only(false).open_world(false);
    let annotations = match spec.effect {
        Effect::Additive => base.destructive(false).idempotent(false),
        Effect::Setter => base.destructive(false).idempotent(true),
        Effect::Destructive => base.destructive(true).idempotent(false),
    };
    make_tool(spec.name, "", (spec.schema)(), annotations)
}

fn tool<T: JsonSchema>(
    name: &'static str,
    description: &str,
    annotations: ToolAnnotations,
) -> Tool {
    make_tool(
        name,
        description,
        dawkey_core::args_schema::<T>(),
        annotations,
    )
}

/// Builds a tool from its argument schema. An empty `description` means "use
/// the argument type's doc comment".
fn make_tool(
    name: &'static str,
    description: &str,
    schema: schemars::Schema,
    annotations: ToolAnnotations,
) -> Tool {
    let Value::Object(mut schema) = schema.to_value() else {
        panic!("argument schema for {name} is not an object");
    };
    schema.remove("$schema");
    schema.remove("title");
    crate::times::augment_schema(&mut schema);
    let doc = schema
        .remove("description")
        .and_then(|d| d.as_str().map(clean_doc));
    let description = match (description, doc) {
        ("", Some(doc)) => doc,
        (d, _) => d.to_owned(),
    };
    Tool::new(name, description, Arc::new(schema)).with_annotations(annotations)
}

/// Joins a doc comment's hard-wrapped lines into one paragraph.
fn clean_doc(doc: &str) -> String {
    doc.split_whitespace().collect::<Vec<_>>().join(" ")
}
