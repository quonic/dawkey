// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hosted plugins as the engine sees them. `dk-plugins` implements
//! [`PluginProcessor`] for CLAP and LV2; the engine only moves processors
//! between graphs and calls them on the real-time thread.

use std::any::Any;

use crate::midi::PerNote;

/// Most events a plugin gets in one block; more are dropped.
pub const MAX_PLUGIN_EVENTS: usize = 1024;

/// Something for a plugin to act on, `offset` frames into the block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PluginEvent {
    pub offset: u32,
    pub kind: PluginEventKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PluginEventKind {
    NoteOn {
        channel: u8,
        key: u8,
        velocity: f32,
    },
    NoteOff {
        channel: u8,
        key: u8,
    },
    /// Another MIDI 1.0 channel message: control change, pitch bend,
    /// channel or key pressure, program change. Two-byte messages end in 0.
    Midi {
        data: [u8; 3],
    },
    /// A MIDI 2.0 per-note controller on `key`, with its 32-bit value.
    PerNote {
        channel: u8,
        key: u8,
        controller: PerNote,
        value: u32,
    },
    /// Parameter `index` (in the plugin's catalog order) is now `value`.
    Param {
        index: u32,
        value: f64,
    },
}

/// Where a plugin's MIDI output goes: notes and other channel messages, in
/// offset order. Holds at most [`MAX_PLUGIN_EVENTS`]; more are dropped.
/// Real-time safe.
#[derive(Debug)]
pub struct MidiSink<'a> {
    events: &'a mut Vec<PluginEvent>,
}

impl<'a> MidiSink<'a> {
    /// A sink that adds to `events` up to its capacity (it never grows).
    pub fn new(events: &'a mut Vec<PluginEvent>) -> Self {
        Self { events }
    }

    pub fn push(&mut self, offset: u32, kind: PluginEventKind) {
        if self.events.len() < self.events.capacity() {
            self.events.push(PluginEvent { offset, kind });
        }
    }
}

impl PluginEventKind {
    /// The event for a MIDI 1.0 channel message a plugin sent: notes as
    /// note events, anything else as it is.
    pub fn from_midi(data: [u8; 3]) -> Self {
        let [status, key, velocity] = data;
        let channel = status & 0x0f;
        match status & 0xf0 {
            0x90 if velocity > 0 => Self::NoteOn {
                channel,
                key: key & 0x7f,
                velocity: f32::from(velocity & 0x7f) / 127.0,
            },
            0x80 | 0x90 => Self::NoteOff {
                channel,
                key: key & 0x7f,
            },
            _ => Self::Midi { data },
        }
    }
}

/// Where the transport is for one block, for plugins that follow it
/// (arpeggiators, tempo-synced delays, ...).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Transport {
    pub playing: bool,
    pub recording: bool,
    /// The block's timeline position.
    pub frame: u64,
    pub seconds: f64,
    /// The same in quarter notes from the start.
    pub beats: f64,
    /// Tempo at the block's start (quarter notes per minute), and how much
    /// it changes per frame through the block (tempo ramps).
    pub bpm: f64,
    pub bpm_per_frame: f64,
    /// The bar the block starts in (0 is the first), where it starts in
    /// quarter notes, and its time signature.
    pub bar: u32,
    pub bar_start_beats: f64,
    pub numerator: u8,
    pub denominator: u8,
    /// The loop, while it's on: start and end in quarter notes and seconds.
    pub loop_beats: Option<(f64, f64)>,
    pub loop_seconds: Option<(f64, f64)>,
}

/// One of a plugin's other audio outputs (beyond the main one): two
/// channels of [`MAX_BLOCK`](crate::MAX_BLOCK) frames (a mono output fills
/// both).
#[derive(Debug)]
pub struct AuxBuffer {
    pub channels: [Box<[f32]>; 2],
}

impl AuxBuffer {
    pub fn new() -> Self {
        Self {
            channels: [(); 2].map(|()| vec![0.0; crate::MAX_BLOCK].into_boxed_slice()),
        }
    }
}

impl Default for AuxBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// What a plugin gets for a block besides the track's audio and its
/// events.
#[derive(Debug)]
pub struct ProcessContext<'a> {
    pub transport: Transport,
    /// The sidechain input, if one is routed to the plugin (as long as the
    /// block).
    pub sidechain: Option<[&'a [f32]; 2]>,
    /// For the plugin's other outputs that are routed somewhere, by output
    /// (see `PluginInfo::outputs` in dk-plugins): the plugin writes the
    /// block into each, or silence.
    pub aux: &'a mut [AuxBuffer],
}

impl ProcessContext<'_> {
    /// A context with only the transport: no sidechain, no other outputs.
    pub fn new(transport: Transport) -> Self {
        Self {
            transport,
            sidechain: None,
            aux: &mut [],
        }
    }
}

/// The real-time half of a hosted plugin.
pub trait PluginProcessor: Send {
    /// Processes one block of at most [`MAX_BLOCK`](crate::MAX_BLOCK) frames
    /// in place: `audio` holds the track's stereo signal on the way in and
    /// the plugin's output on the way out (a plugin without audio outputs
    /// leaves it alone). `context` has the transport, a sidechain input and
    /// buffers for other outputs. `events` are sorted by offset; MIDI the
    /// plugin sends goes to `midi_out`. Must be real-time safe.
    fn process(
        &mut self,
        audio: [&mut [f32]; 2],
        context: ProcessContext,
        events: &[PluginEvent],
        midi_out: MidiSink,
    );

    /// Called once on the real-time thread before the processor leaves it,
    /// so it can stop processing there.
    fn stop(&mut self);

    /// For handing the processor back to its host on the control thread.
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send>;
}

/// Length in bytes of a MIDI 1.0 channel message with this status byte.
pub fn midi_message_len(status: u8) -> usize {
    match status & 0xf0 {
        0xc0 | 0xd0 => 2,
        _ => 3,
    }
}

impl std::fmt::Debug for dyn PluginProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PluginProcessor")
    }
}
