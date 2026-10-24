// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dawkey's core: the project model and the single command bus that every
//! frontend (GUI, MCP, tests) uses to change it.
//!
//! Every mutation is a [`Command`]. A command validates its arguments and
//! lowers to primitive, invertible edits; [`Session`] applies those edits and
//! keeps their inverses as undo history. Command argument types derive
//! `JsonSchema`, and [`COMMAND_SPECS`] lists them so the MCP layer can publish
//! one tool per command without a hand-maintained copy of the schema.

#![forbid(unsafe_code)]

mod command;
mod edit;
mod error;
pub mod host;
mod model;
pub mod project_file;
mod session;
mod tempo;

pub use command::{
    AddAutomationPoint, AddClip, AddMidiClip, AddMidiNotes, AddPlugin, AddTrack, COMMAND_SPECS,
    Command, CommandSpec, CompTake, ControlSpec, ControlValue, DuplicateClip, EditMidiNotes,
    Effect, MoveClip, MovePlugin, NoteChange, NoteSpec, PointSpec, QuantizeMidiNotes,
    RemoveAutomationPoints, RemoveClip, RemoveMidiNotes, RemovePlugin, RemoveTempoChange,
    RemoveTimeSignatureChange, RemoveTrack, RenameClip, RenameProject, RenameTrack, RepeatClip,
    SetAutomation, SetClipGain, SetClipLength, SetClipMute, SetLoop, SetMidiControls,
    SetPluginBypass, SetPluginOutput, SetPluginParam, SetPluginSidechain, SetPunch, SetSynth,
    SetTempo, SetTempoMap, SetTimeSignature, SetTimeSignatureMap, SetTrackArm, SetTrackGain,
    SetTrackInput, SetTrackMidiInput, SetTrackMidiOutput, SetTrackMonitor, SetTrackMute,
    SetTrackOutput, SetTrackPan, SetTrackSolo, SplitClip, TrimClip, WriteAutomation, args_schema,
};
pub use error::CoreError;
pub use host::Handle;
pub use model::{
    AutomationLane, AutomationPoint, AutomationTarget, CC_MAX, Clip, ClipContent, ClipId,
    ControlPoint, Controller, DEVICE_LATENCY_MS_MAX, GAIN_DB_MAX, GAIN_DB_MIN, InputChannels,
    MASTER, MIDI_MAX, MidiOutput, Note, NoteId, PER_NOTE_ASSIGNABLE, PER_NOTE_BEND,
    PER_NOTE_DETACH, PER_NOTE_MANAGEMENT, PER_NOTE_REGISTERED, PER_NOTE_RESET, PluginId,
    PluginSlot, PluginState, Project, Region, Source, SourceId, SynthParams, TEMPO_BPM_MAX,
    TEMPO_BPM_MIN, TimeRange, TimeSignature, Track, TrackId, TrackKind, Waveform, downscale_32,
    is_per_note, upscale_16,
};
pub use session::{Outcome, Session, SessionState};
pub use tempo::{MeterChange, MeterMap, TempoChange, TempoMap};
