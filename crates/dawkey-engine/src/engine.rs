// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rtrb::{Consumer, Producer, PushError, RingBuffer};
use serde::Serialize;

use crate::MAX_BLOCK;
use crate::graph::{Graph, SendNode};
use crate::meter::{MeterBank, MeterReading};
use crate::midi::{MidiEvent, MidiOut};
use crate::mix::{self, Live, MidiOutput};
use crate::plugin::PluginProcessor;
use crate::record::RecordSession;

/// Stands in for missing input buffers.
static SILENCE: [f32; MAX_BLOCK] = [0.0; MAX_BLOCK];

/// Capacity of the control → real-time message queue.
const QUEUE: usize = 64;
/// Most MIDI messages sent to devices per process call; more are dropped.
pub const MIDI_OUT_EVENTS: usize = 1024;

enum Message {
    Graph(Box<Graph>),
    Play,
    Stop,
    Locate(u64),
    /// Start capturing input while playing.
    StartRecording(Box<RecordSession>),
    StopRecording,
}

/// Things the real-time thread is done with, freed by the controller.
enum Retired {
    Graph(Box<Graph>),
    #[expect(
        dead_code,
        reason = "held only so it is dropped off the real-time thread"
    )]
    Session(Box<RecordSession>),
}

/// State published by the real-time thread.
#[derive(Debug, Default)]
struct Shared {
    playing: AtomicBool,
    recording: AtomicBool,
    position: AtomicU64,
    /// Number of `process` calls that produced audio.
    cycles: AtomicU64,
}

/// The engine's message queue is full: the real-time side is not consuming
/// (the audio device is stopped or stalled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineBusy;

impl std::fmt::Display for EngineBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the audio engine is not responding (is the audio device running?)")
    }
}

impl std::error::Error for EngineBusy {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TransportStatus {
    pub playing: bool,
    pub recording: bool,
    /// Timeline position in frames.
    pub position: u64,
    /// Number of audio cycles the engine has run.
    pub cycles: u64,
}

/// Creates a connected controller/processor pair playing `graph`.
pub fn new_engine(sample_rate: u32, graph: Graph) -> (EngineController, EngineProcessor) {
    let (commands_tx, commands_rx) = RingBuffer::new(QUEUE);
    // Every graph or session message retires at most one object; leave
    // headroom so the real-time side never has to hold one back.
    let (garbage_tx, garbage_rx) = RingBuffer::new(QUEUE * 2);
    let shared = Arc::new(Shared::default());
    let controller = EngineController {
        commands: commands_tx,
        garbage: garbage_rx,
        shared: Arc::clone(&shared),
        meters: Arc::clone(&graph.meters),
        sample_rate,
        graphs_in_flight: 0,
        retired_plugins: Vec::new(),
    };
    let processor = EngineProcessor {
        graph: Box::new(graph),
        retired: None,
        commands: commands_rx,
        garbage: garbage_tx,
        shared,
        sample_rate,
        playing: false,
        position: 0,
        clock: 0,
        chase: false,
        session: None,
        lanes: [vec![0.0; MAX_BLOCK], vec![0.0; MAX_BLOCK]],
        midi_out: Vec::with_capacity(MIDI_OUT_EVENTS),
    };
    (controller, processor)
}

/// The control-thread side of the engine.
pub struct EngineController {
    commands: Producer<Message>,
    garbage: Consumer<Retired>,
    shared: Arc<Shared>,
    meters: Arc<MeterBank>,
    sample_rate: u32,
    /// Graphs sent that the real-time side hasn't swapped in yet. Each swap
    /// returns exactly one old graph.
    graphs_in_flight: usize,
    /// Plugin processors the engine is done with, by key, for their host
    /// to unload.
    retired_plugins: Vec<(u64, Box<dyn PluginProcessor>)>,
}

impl std::fmt::Debug for EngineController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineController")
            .field("sample_rate", &self.sample_rate)
            .field("transport", &self.transport())
            .finish_non_exhaustive()
    }
}

impl EngineController {
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Replaces the graph being played, at the next block boundary.
    pub fn set_graph(&mut self, graph: Graph) -> Result<(), EngineBusy> {
        let meters = Arc::clone(&graph.meters);
        self.collect_garbage();
        match self.commands.push(Message::Graph(Box::new(graph))) {
            Ok(()) => {
                self.meters = meters;
                self.graphs_in_flight += 1;
                Ok(())
            }
            Err(PushError::Full(message)) => {
                // Its plugins never ran; hand them back.
                if let Message::Graph(mut graph) = message {
                    self.retired_plugins.extend(graph.take_plugins());
                }
                Err(EngineBusy)
            }
        }
    }

    /// Plugin processors the engine has let go of (their plugins were
    /// removed, or their graph never played), by key. Their host should
    /// unload them.
    pub fn take_retired_plugins(&mut self) -> Vec<(u64, Box<dyn PluginProcessor>)> {
        self.collect_garbage();
        std::mem::take(&mut self.retired_plugins)
    }

    /// Number of graphs sent but not yet playing. Callers that update often
    /// should wait for this to reach zero, so a stalled audio thread can't
    /// fill the queue.
    pub fn graphs_in_flight(&mut self) -> usize {
        self.collect_garbage();
        self.graphs_in_flight
    }

    pub fn play(&mut self) -> Result<(), EngineBusy> {
        self.send(Message::Play)
    }

    pub fn stop(&mut self) -> Result<(), EngineBusy> {
        self.send(Message::Stop)
    }

    /// Moves the playhead to timeline frame `position`.
    pub fn locate(&mut self, position: u64) -> Result<(), EngineBusy> {
        self.send(Message::Locate(position))
    }

    /// Starts capturing input into `session` (while playing).
    pub fn start_recording(&mut self, session: RecordSession) -> Result<(), EngineBusy> {
        self.send(Message::StartRecording(Box::new(session)))
    }

    /// Stops capturing; the session is marked finished.
    pub fn stop_recording(&mut self) -> Result<(), EngineBusy> {
        self.send(Message::StopRecording)
    }

    /// Transport state as last published by the real-time thread.
    pub fn transport(&self) -> TransportStatus {
        TransportStatus {
            playing: self.shared.playing.load(Ordering::Acquire),
            recording: self.shared.recording.load(Ordering::Acquire),
            position: self.shared.position.load(Ordering::Acquire),
            cycles: self.shared.cycles.load(Ordering::Acquire),
        }
    }

    /// Peak levels since the last call.
    pub fn take_meters(&self) -> MeterReading {
        self.meters.take()
    }

    /// Frees what the real-time thread has finished with.
    pub fn collect_garbage(&mut self) {
        while let Ok(retired) = self.garbage.pop() {
            if let Retired::Graph(mut graph) = retired {
                self.graphs_in_flight = self.graphs_in_flight.saturating_sub(1);
                self.retired_plugins.extend(graph.take_plugins());
            }
        }
    }

    fn send(&mut self, message: Message) -> Result<(), EngineBusy> {
        self.collect_garbage();
        self.commands.push(message).map_err(|_| EngineBusy)
    }
}

/// The real-time side of the engine. Owned by the audio thread.
pub struct EngineProcessor {
    graph: Box<Graph>,
    /// Something replaced that didn't fit in the garbage queue yet.
    retired: Option<Retired>,
    commands: Consumer<Message>,
    garbage: Producer<Retired>,
    shared: Arc<Shared>,
    sample_rate: u32,
    playing: bool,
    position: u64,
    /// Frames processed so far, playing or not.
    clock: u64,
    /// Playback started or jumped: instruments catch up on notes and
    /// controllers at the next block.
    chase: bool,
    session: Option<Box<RecordSession>>,
    lanes: [Vec<f32>; 2],
    /// MIDI for devices, until the backend takes it.
    midi_out: Vec<MidiOut>,
}

impl std::fmt::Debug for EngineProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineProcessor")
            .field("sample_rate", &self.sample_rate)
            .field("playing", &self.playing)
            .field("position", &self.position)
            .finish_non_exhaustive()
    }
}

impl EngineProcessor {
    /// Takes back the graph being played, e.g. after an offline render.
    pub fn into_graph(self) -> Graph {
        *self.graph
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// MIDI for devices from the process calls since the last
    /// [`clear_midi_output`](Self::clear_midi_output), sorted by offset
    /// within each call. Offsets count from the start of the buffer of the
    /// call that made them. Real-time safe.
    pub fn midi_output(&self) -> &[MidiOut] {
        &self.midi_out
    }

    /// Forgets the MIDI output, once the backend has sent it. Call after
    /// every process call. Real-time safe.
    pub fn clear_midi_output(&mut self) {
        self.midi_out.clear();
    }

    /// Renders the next `left.len()` frames into `left` and `right`, with no
    /// input. Real-time safe. Zero-length buffers just apply pending
    /// messages.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.process_with_input([&[], &[]], &[], left, right);
    }

    /// Like [`process`](Self::process), with input. Each audio input is
    /// either as long as the output or empty (silence). `midi` must be sorted
    /// by offset; events at or past the end of the buffer are ignored.
    pub fn process_with_input(
        &mut self,
        input: [&[f32]; 2],
        midi: &[MidiEvent],
        left: &mut [f32],
        right: &mut [f32],
    ) {
        assert_no_alloc::assert_no_alloc(|| self.process_inner(input, midi, left, right));
    }

    /// Outputs silence without advancing the transport, e.g. when the device
    /// runs at a different rate than the project. Real-time safe.
    pub fn process_silence(&mut self, left: &mut [f32], right: &mut [f32]) {
        left.fill(0.0);
        right.fill(0.0);
        self.shared.cycles.fetch_add(1, Ordering::Relaxed);
    }

    fn process_inner(
        &mut self,
        input: [&[f32]; 2],
        midi: &[MidiEvent],
        left: &mut [f32],
        right: &mut [f32],
    ) {
        debug_assert_eq!(left.len(), right.len());
        self.handle_messages();
        left.fill(0.0);
        right.fill(0.0);
        let total = left.len();
        let mut done = 0;
        // While stopped, instruments still play live input and release
        // tails; the transport doesn't move.
        while done < total {
            // Split at block size and at the loop end.
            let mut len = (total - done).min(MAX_BLOCK);
            let looping = self
                .graph
                .loop_range
                .filter(|r| self.playing && self.position >= r.start && self.position < r.end);
            if let Some(r) = looping {
                len = len.min((r.end - self.position) as usize);
            }
            let range = done..done + len;
            let from = midi.partition_point(|e| (e.offset as usize) < range.start);
            let to = midi.partition_point(|e| (e.offset as usize) < range.end);
            let live = Live {
                events: &midi[from..to],
                base: done as u32,
            };
            let chase = self.playing && std::mem::take(&mut self.chase);
            let block_input =
                [0, 1].map(|c| input[c].get(range.clone()).unwrap_or(&SILENCE[..len]));
            self.render_block(
                live,
                chase,
                done as u32,
                block_input,
                &mut left[range.clone()],
                &mut right[range.clone()],
            );
            if !self.playing {
                done += len;
                continue;
            }
            if let Some(session) = &mut self.session {
                let [in_l, in_r] = block_input;
                session.write(self.position, in_l, in_r);
                session.write_midi(self.position, live.base, live.events);
            }
            self.position += len as u64;
            if let Some(r) = looping
                && self.position == r.end
            {
                self.position = r.start;
                self.release_clip_notes();
                self.chase = true;
                if let Some(session) = &mut self.session {
                    session.wrap();
                }
            }
            done += len;
        }
        self.clock += total as u64;
        // Tracks add their MIDI a block at a time: sort it by offset,
        // keeping the order of events at the same offset.
        let out = &mut self.midi_out;
        for i in 1..out.len() {
            let mut j = i;
            while j > 0 && out[j - 1].offset > out[j].offset {
                out.swap(j - 1, j);
                j -= 1;
            }
        }
        self.shared.playing.store(self.playing, Ordering::Release);
        self.shared
            .recording
            .store(self.session.is_some(), Ordering::Release);
        self.shared.position.store(self.position, Ordering::Release);
        if !left.is_empty() {
            self.shared.cycles.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn handle_messages(&mut self) {
        if let Some(old) = self.retired.take() {
            self.retire(old);
        }
        loop {
            // Don't take anything that retires an object while still holding
            // one back.
            if self.retired.is_some()
                && matches!(
                    self.commands.peek(),
                    Ok(Message::Graph(_) | Message::StartRecording(_) | Message::StopRecording)
                )
            {
                break;
            }
            let Ok(message) = self.commands.pop() else {
                break;
            };
            match message {
                Message::Graph(graph) => {
                    let mut old = std::mem::replace(&mut self.graph, graph);
                    let mut out = MidiOutput {
                        events: &mut self.midi_out,
                        base: 0,
                        clock: self.clock,
                    };
                    mix::carry_over(&mut old, &mut self.graph, &mut out);
                    self.retire(Retired::Graph(old));
                }
                Message::Play => {
                    self.chase |= !self.playing;
                    self.playing = true;
                }
                Message::Stop => {
                    self.playing = false;
                    self.release_clip_notes();
                }
                Message::Locate(position) => {
                    self.position = position;
                    self.release_clip_notes();
                    self.chase = true;
                }
                Message::StartRecording(session) => {
                    if let Some(old) = self.session.replace(session) {
                        old.finish();
                        self.retire(Retired::Session(old));
                    }
                }
                Message::StopRecording => {
                    if let Some(old) = self.session.take() {
                        old.finish();
                        self.retire(Retired::Session(old));
                    }
                }
            }
        }
    }

    fn retire(&mut self, retired: Retired) {
        if let Err(PushError::Full(retired)) = self.garbage.push(retired) {
            self.retired = Some(retired);
        }
    }

    fn release_clip_notes(&mut self) {
        mix::release_clip_notes(&mut self.graph);
    }

    fn render_block(
        &mut self,
        live: Live,
        chase: bool,
        base: u32,
        input: [&[f32]; 2],
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let frames = out_l.len();
        let mut midi_out = MidiOutput {
            events: &mut self.midi_out,
            base,
            clock: self.clock + u64::from(base),
        };
        let (position, playing) = (self.position, self.playing);
        let transport = self
            .graph
            .transport(position, frames, playing, self.session.is_some());
        let [lane_l, lane_r] = &mut self.lanes;
        let (lane_l, lane_r) = (&mut lane_l[..frames], &mut lane_r[..frames]);
        let meters = &self.graph.meters;
        let tracks = &mut self.graph.tracks;
        for track in tracks.iter_mut() {
            for buffer in [&mut track.sum, &mut track.tap, &mut track.pre]
                .into_iter()
                .flatten()
            {
                buffer[0][..frames].fill(0.0);
                buffer[1][..frames].fill(0.0);
            }
        }
        // Tracks come after the tracks they take from, and before the
        // buses they go to; the master last.
        for i in 0..tracks.len() {
            let (earlier, rest) = tracks.split_at_mut(i);
            let (track, later) = rest.split_first_mut().expect("i is in range");
            // An armed track that doesn't monitor meters its input, muted
            // or not.
            let metered = track.input.filter(|i| !i.monitor);
            if let (Some(armed), Some(m)) = (metered, track.meter) {
                let [l, r] = armed.pick(input);
                meters.tracks[m].record(peak(l), peak(r));
            }
            // While stopped, only instruments, plugins, buses and monitored
            // input sound (live input, tails).
            let monitoring = track.input.is_some_and(|i| i.monitor);
            let quiet = track.instrument.is_none() && track.plugins.is_empty() && !monitoring;
            if !playing && track.sum.is_none() && quiet {
                continue;
            }
            // Rendered even when inaudible, so voices and plugins stay
            // current. A bus starts from what was sent to it (taken out
            // for the call, and put back).
            let sum = track.sum.take();
            mix::render_track(
                track,
                earlier,
                position,
                playing,
                chase,
                &transport,
                live,
                input,
                sum.as_ref().map(|[l, r]| [&l[..frames], &r[..frames]]),
                &mut midi_out,
                [lane_l, lane_r],
            );
            track.sum = sum;
            // For sidechains: after the fader, before lining up.
            if let Some([l, r]) = &mut track.tap {
                l[..frames].copy_from_slice(lane_l);
                r[..frames].copy_from_slice(lane_r);
            }
            if let Some(delay) = &mut track.delay {
                delay.process(lane_l, lane_r);
            }
            if !track.audible {
                continue;
            }
            // Plugins' other outputs, to their buses.
            for plugin in &mut track.plugins {
                for route in plugin.aux_routes.iter_mut() {
                    let [l, r] = &mut plugin.aux[route.output].channels;
                    let (l, r) = (&mut l[..frames], &mut r[..frames]);
                    if let Some(delay) = &mut route.delay {
                        delay.process(l, r);
                    }
                    if let Some([to_l, to_r]) = &mut later[route.dest - i - 1].sum {
                        add_into(&mut to_l[..frames], l);
                        add_into(&mut to_r[..frames], r);
                    }
                }
            }
            if let (None, Some(m)) = (metered, track.meter) {
                meters.tracks[m].record(peak(lane_l), peak(lane_r));
            }
            // Aux sends: the signal after the fader (before lining up), or
            // before it, at the send's level, to their buses.
            for send in &mut track.sends {
                let Some([to_l, to_r]) = &mut later[send.dest - i - 1].sum else {
                    continue;
                };
                let SendNode {
                    buffer,
                    lane,
                    fixed,
                    offset,
                    delay,
                    pre_fader,
                    ..
                } = send;
                let [l, r] = buffer;
                let (l, r) = (&mut l[..frames], &mut r[..frames]);
                match (&track.pre, *pre_fader) {
                    (Some([pre_l, pre_r]), true) => {
                        l.copy_from_slice(&pre_l[..frames]);
                        r.copy_from_slice(&pre_r[..frames]);
                    }
                    _ => {
                        l.copy_from_slice(lane_l);
                        r.copy_from_slice(lane_r);
                    }
                }
                let at = position.saturating_sub(u64::from(*offset));
                match lane {
                    Some(lane) if playing => {
                        for (k, (a, b)) in l.iter_mut().zip(r.iter_mut()).enumerate() {
                            let g = crate::graph::lane_value(lane, at + k as u64)
                                .map_or(*fixed, |v| crate::graph::db_to_gain(v as f32));
                            *a *= g;
                            *b *= g;
                        }
                    }
                    _ => {
                        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                            *a *= *fixed;
                            *b *= *fixed;
                        }
                    }
                }
                if let Some(delay) = delay {
                    delay.process(l, r);
                }
                add_into(&mut to_l[..frames], l);
                add_into(&mut to_r[..frames], r);
            }
            let (to_l, to_r) = match track.dest {
                Some(d) => match &mut later[d - i - 1].sum {
                    Some([l, r]) => (&mut l[..frames], &mut r[..frames]),
                    None => continue,
                },
                None => (&mut *out_l, &mut *out_r),
            };
            add_into(to_l, lane_l);
            add_into(to_r, lane_r);
        }
        meters.master.record(peak(out_l), peak(out_r));
    }
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |m, s| m.max(s.abs()))
}

fn add_into(out: &mut [f32], lane: &[f32]) {
    for (o, s) in out.iter_mut().zip(lane) {
        *o += s;
    }
}
