// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dawkey's audio engine.
//!
//! The project is compiled into an immutable [`Graph`] on the control side and
//! handed to the [`EngineProcessor`], which runs on the real-time thread. The
//! processor never allocates, locks or frees: graphs arrive and leave through
//! lock-free queues, and old graphs are dropped by the [`EngineController`].
//! Debug builds that install [`assert_no_alloc::AllocDisabler`] as the global
//! allocator abort if the processor allocates.

#![forbid(unsafe_code)]

mod engine;
mod graph;
mod meter;
mod midi;
mod mix;
mod notes;
mod plugin;
mod record;

pub use engine::{
    EngineBusy, EngineController, EngineProcessor, MIDI_OUT_EVENTS, TransportStatus, new_engine,
};
pub use graph::{Graph, MediaPool, NoPlugins, PluginProvider, ProvidedPlugin};
pub use meter::{MeterReading, TrackMeter};
pub use midi::{
    InputFilter, Message as MidiMessage, MidiEvent, MidiOut, PerNote, device_tag, parse_midi1,
    parse_ump,
};
pub use plugin::{
    AuxBuffer, MAX_PLUGIN_EVENTS, MidiSink, PluginEvent, PluginEventKind, PluginProcessor,
    ProcessContext, Transport, midi_message_len,
};
pub use record::{
    MIDI_RING, Mark, MidiRecord, RecordSession, RecordShared, RecordTap, RecordTaps, record_session,
};

/// Largest block the processor renders at once. Longer requests are split.
pub const MAX_BLOCK: usize = 4096;

/// Renders `frames` frames of `graph` starting at timeline frame `start`,
/// independent of any live engine (and straight through the loop). Returns
/// stereo planar audio.
pub fn render_offline(graph: Graph, sample_rate: u32, start: u64, frames: usize) -> Vec<Vec<f32>> {
    render_offline_keeping(graph, sample_rate, start, frames).0
}

/// Like [`render_offline`], and also returns the graph, whose plugins its
/// host must unload (see [`Graph::take_plugins`]). Plugin latency is
/// compensated: the output lines up with the timeline.
pub fn render_offline_keeping(
    graph: Graph,
    sample_rate: u32,
    start: u64,
    frames: usize,
) -> (Vec<Vec<f32>>, Graph) {
    let mut render = OfflineRender::new(graph, sample_rate, start, frames);
    let mut left = vec![0.0; frames];
    let mut right = vec![0.0; frames];
    let mut at = 0;
    while at < frames {
        at += render.render(&mut left[at..], &mut right[at..]);
    }
    (vec![left, right], render.into_graph())
}

/// An offline render a block at a time, so long ones (exports) needn't fit
/// in memory. Plugin latency is compensated: the output lines up with the
/// timeline, which it plays straight through, never wrapping at the loop.
pub struct OfflineRender {
    processor: EngineProcessor,
    /// Frames still to come out, and the latency still to skip.
    frames_left: usize,
    skip: usize,
    /// The block last rendered (always [`MAX_BLOCK`] at a time, as a whole
    /// render would), and how much of it was handed out.
    block: [Box<[f32]>; 2],
    block_len: usize,
    block_at: usize,
}

impl std::fmt::Debug for OfflineRender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfflineRender")
            .field("frames_left", &self.frames_left)
            .finish_non_exhaustive()
    }
}

impl OfflineRender {
    pub fn new(mut graph: Graph, sample_rate: u32, start: u64, frames: usize) -> Self {
        let latency = graph.latency() as usize;
        // Renders cover the timeline as it is: the loop is the transport's.
        graph.loop_range = None;
        let (mut controller, processor) = new_engine(sample_rate, graph);
        controller
            .locate(start)
            .expect("fresh engine has queue space");
        controller.play().expect("fresh engine has queue space");
        // The controller only queues commands, which the processor has.
        drop(controller);
        Self {
            processor,
            frames_left: frames,
            skip: latency,
            block: [(); 2].map(|()| vec![0.0; MAX_BLOCK].into_boxed_slice()),
            block_len: 0,
            block_at: 0,
        }
    }

    /// Frames still to come.
    pub fn frames_left(&self) -> usize {
        self.frames_left
    }

    /// Renders the next frames into `left` and `right` (as many as fit, up
    /// to the end); returns how many. 0 once it's done.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> usize {
        let want = left.len().min(right.len()).min(self.frames_left);
        let mut written = 0;
        while written < want {
            if self.block_at == self.block_len {
                // Everything comes out `latency` frames late: render it in
                // blocks from the start, and skip that much.
                let len = (self.frames_left - written + self.skip).min(MAX_BLOCK);
                let [l, r] = &mut self.block;
                self.processor.process(&mut l[..len], &mut r[..len]);
                self.block_len = len;
                self.block_at = self.skip.min(len);
                self.skip -= self.block_at;
                continue;
            }
            let n = (self.block_len - self.block_at).min(want - written);
            let from = self.block_at..self.block_at + n;
            left[written..written + n].copy_from_slice(&self.block[0][from.clone()]);
            right[written..written + n].copy_from_slice(&self.block[1][from]);
            self.block_at += n;
            written += n;
        }
        self.frames_left -= written;
        written
    }

    /// The graph, whose plugins its host must unload.
    pub fn into_graph(self) -> Graph {
        self.processor.into_graph()
    }
}

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[cfg(test)]
mod plugin_tests;
#[cfg(test)]
mod tests;
