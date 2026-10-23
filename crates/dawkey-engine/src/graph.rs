// SPDX-License-Identifier: AGPL-3.0-or-later
//! The compiled form of a project that the real-time thread plays.
//!
//! A graph is immutable except for the state that must survive the swap to
//! the next graph: synth voices, plugin processors (with the parameter values
//! they were last sent), and delay lines. The real-time thread moves that
//! state across (see `mix::carry_over`).

use std::collections::{BTreeMap, HashMap};
use std::f32::consts::FRAC_PI_4;
use std::sync::Arc;

use dawkey_core::{
    AutomationTarget, ClipContent, Controller, DEVICE_LATENCY_MS_MAX, InputChannels, MeterMap,
    PluginSlot, Project, SourceId, TempoMap, TimeRange, TimeSignature, Track, TrackId, TrackKind,
};
use dawkey_dsp::PolySynth;
use dawkey_media::AudioData;

use crate::meter::MeterBank;
use crate::midi::{InputFilter, device_tag};
use crate::notes::NoteTracker;
use crate::plugin::{AuxBuffer, MAX_PLUGIN_EVENTS, PluginEvent, PluginProcessor, Transport};

/// Voice tags with this bit set belong to live input; the rest are note IDs.
pub(crate) const LIVE: u64 = 1 << 63;
/// Tags with this bit set are notes a plugin chain sent to a device.
pub(crate) const CHAIN: u64 = 1 << 62;

/// Automation points: (timeline frame, value), sorted.
pub(crate) type Lane = Box<[(u64, f64)]>;

/// Decoded audio for each source, at the project sample rate.
pub type MediaPool = HashMap<SourceId, Arc<AudioData>>;

/// Supplies plugins while a graph is compiled.
pub trait PluginProvider {
    /// What the engine needs for `slot` on `track`, or `None` if the plugin
    /// can't be loaded (the slot is then skipped).
    fn provide(&mut self, track: TrackId, slot: &PluginSlot) -> Option<ProvidedPlugin>;
}

/// A plugin as handed to the graph compiler.
#[derive(Debug)]
pub struct ProvidedPlugin {
    /// Identifies the plugin's processor from one graph to the next.
    pub key: u64,
    /// A new processor, or `None` to keep the one with this key that the
    /// playing graph has.
    pub processor: Option<Box<dyn PluginProcessor>>,
    /// Parameter IDs in index order, with their defaults.
    pub params: Vec<(String, f64)>,
    /// Frames of delay the plugin adds.
    pub latency: u32,
    /// Takes notes (an instrument or a MIDI effect).
    pub instrument: bool,
    /// Sends MIDI (a MIDI effect).
    pub midi_output: bool,
    /// Has a sidechain input.
    pub sidechain: bool,
    /// How many other outputs it has.
    pub outputs: u32,
}

/// Provides no plugins: every slot is skipped.
#[derive(Debug, Default)]
pub struct NoPlugins;

impl PluginProvider for NoPlugins {
    fn provide(&mut self, _track: TrackId, _slot: &PluginSlot) -> Option<ProvidedPlugin> {
        None
    }
}

#[derive(Debug)]
pub struct Graph {
    pub(crate) tracks: Vec<TrackNode>,
    pub(crate) meters: Arc<MeterBank>,
    /// Playback wraps from the end back to the start of this range.
    pub(crate) loop_range: Option<TimeRange>,
    /// The longest track latency (plugins, or a MIDI device's own); every
    /// track is delayed to match it.
    pub(crate) latency: u32,
    /// For the transport plugins see.
    pub(crate) tempo: TempoMap,
    pub(crate) meter: MeterMap,
}

#[derive(Debug)]
pub(crate) struct TrackNode {
    pub(crate) id: TrackId,
    /// False if muted, or if another track is soloed.
    pub(crate) audible: bool,
    pub(crate) clips: Vec<ClipNode>,
    /// Armed audio tracks: the input they record.
    pub(crate) input: Option<TrackInput>,
    /// MIDI tracks: what plays the notes, and the notes.
    pub(crate) instrument: Option<Box<Instrument>>,
    /// MIDI tracks: the device the track sends to, if any.
    pub(crate) device: Option<Box<External>>,
    /// Insert plugins, in signal order.
    pub(crate) plugins: Vec<PluginNode>,
    /// MIDI on its way down the chain, and what the current plugin sent.
    /// Never grow past their capacity.
    pub(crate) chain_midi: [Vec<PluginEvent>; 2],
    /// With a device and notes going through the chain: how many MIDI
    /// effects start the chain. What they send goes to the device, and no
    /// MIDI goes further.
    pub(crate) to_device_after: Option<usize>,
    /// Delay on the way to `dest` (after the fader), so everything that
    /// arrives there lines up; on the master, the delay that lines the mix
    /// up with MIDI devices that sound late.
    pub(crate) delay: Option<DelayLine>,
    pub(crate) fader: Fader,
    /// Where the signal goes: the bus (or master) at this index of
    /// [`Graph::tracks`], always later. `None` on the master: the output.
    pub(crate) dest: Option<usize>,
    /// Buses and the master: what's sent to them this block. Two channels
    /// of [`MAX_BLOCK`](crate::MAX_BLOCK).
    pub(crate) sum: Option<[Box<[f32]>; 2]>,
    /// The track's meter (its index in the project's tracks); `None` on
    /// the master, which has its own.
    pub(crate) meter: Option<usize>,
    /// For plugins that take the track as a sidechain: the block, after
    /// the fader. Two channels of [`MAX_BLOCK`](crate::MAX_BLOCK).
    pub(crate) tap: Option<[Box<[f32]>; 2]>,
}

/// A plugin's sidechain input: another track's signal.
#[derive(Debug)]
pub(crate) struct SidechainIn {
    /// The track (at this index of [`Graph::tracks`], always earlier) and
    /// its tap.
    pub(crate) source: usize,
    /// Lines the source up with what reaches the plugin.
    pub(crate) delay: Option<DelayLine>,
    /// The block, delayed. Two channels of [`MAX_BLOCK`](crate::MAX_BLOCK).
    pub(crate) buffer: [Box<[f32]>; 2],
}

/// One of a plugin's other outputs on its way to a bus.
#[derive(Debug)]
pub(crate) struct AuxRoute {
    /// The output (an index into [`PluginNode::aux`]).
    pub(crate) output: usize,
    /// The bus, at this index of [`Graph::tracks`] (always later).
    pub(crate) dest: usize,
    pub(crate) delay: Option<DelayLine>,
}

/// The input an armed audio track records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TrackInput {
    pub(crate) channels: InputChannels,
    /// The track plays its input along with its clips. Otherwise its meter
    /// shows the input's level instead of what it plays.
    pub(crate) monitor: bool,
}

impl TrackInput {
    /// The input channels that feed the track's left and right lanes (one
    /// channel to both for mono input).
    pub(crate) fn pick(self, input: [&[f32]; 2]) -> [&[f32]; 2] {
        match self.channels {
            InputChannels::Stereo => input,
            InputChannels::Left => [input[0]; 2],
            InputChannels::Right => [input[1]; 2],
        }
    }
}

/// A MIDI track's notes and what plays them.
#[derive(Debug)]
pub(crate) struct Instrument {
    pub(crate) voices: Voices,
    /// Sorted by start.
    pub(crate) notes: Box<[NoteEvent]>,
    /// (tag, end) sorted by tag, to update notes that are sounding when the
    /// graph is replaced.
    pub(crate) ends: Box<[(u64, u64)]>,
    /// The longest note, to find notes sounding at a position quickly.
    pub(crate) longest: u64,
    /// Controller changes from the clips, sorted by frame.
    pub(crate) controls: Box<[ControlEvent]>,
    /// The same changes per controller, to chase their values.
    pub(crate) lanes: Box<[ControlLane]>,
    /// Plays live MIDI input (the track is armed).
    pub(crate) monitor: bool,
    /// Which live input it plays.
    pub(crate) input: InputFilter,
}

/// A controller change at a timeline frame, as a MIDI message (channel 0;
/// per-note ones with their 32-bit value, see [`MidiEvent`]).
///
/// [`MidiEvent`]: crate::MidiEvent
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ControlEvent {
    pub(crate) frame: u64,
    pub(crate) data: [u8; 3],
    pub(crate) value: u32,
}

/// One controller's changes, and its value before the first (`None` for
/// programs, which don't reset): message and 32-bit value.
#[derive(Debug)]
pub(crate) struct ControlLane {
    pub(crate) default: Option<([u8; 3], u32)>,
    pub(crate) events: Box<[ControlEvent]>,
}

/// What turns notes into sound.
#[derive(Debug)]
pub(crate) enum Voices {
    /// The built-in synth (mono).
    Synth(Box<PolySynth>),
    /// The first plugin in the chain, fed note events. If the track has a
    /// device, the MIDI the chain sends goes there.
    Plugin(Box<NoteTracker>),
    /// The track's device: the track sends MIDI out and makes no sound of
    /// its own.
    Device,
}

/// Most messages a device track holds back for delay compensation.
pub(crate) const MAX_PENDING_MIDI: usize = 4096;

/// A MIDI track's notes on their way to a device.
#[derive(Debug, Clone)]
pub(crate) struct External {
    pub(crate) tracker: NoteTracker,
    /// Events for the current block. Never grows past its capacity.
    pub(crate) events: Vec<PluginEvent>,
    /// The device's [`device_tag`].
    pub(crate) device: u32,
    /// MIDI channel, 0 to 15.
    pub(crate) channel: u8,
    /// Frames the device takes to sound after it gets MIDI (its latency).
    pub(crate) latency: u32,
    /// Frames messages are held back, so the device plays in time with
    /// audio delayed to line up plugins and devices: the graph's latency
    /// less the device's own.
    pub(crate) delay: u32,
    /// Messages held back: (when they're due on the engine's frame clock,
    /// message, 32-bit value of a per-note one), in the order they were
    /// sent. Never grows past its capacity.
    pub(crate) pending: Vec<(u64, [u8; 3], u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct NoteEvent {
    /// Timeline range `[start, end)`.
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) tag: u64,
    pub(crate) pitch: u8,
    pub(crate) amp: f32,
}

#[derive(Debug)]
pub(crate) struct ClipNode {
    /// Timeline range `[start, end)`.
    pub(crate) start: u64,
    pub(crate) end: u64,
    /// Source frame played at `start`.
    pub(crate) offset: usize,
    pub(crate) data: Arc<AudioData>,
    /// Clip gain (mono sources go to both channels).
    pub(crate) gain: f32,
}

/// A plugin in a track's chain.
#[derive(Debug)]
pub(crate) struct PluginNode {
    pub(crate) key: u64,
    /// How late what reaches the plugin is (latency before it, frames):
    /// its automation is read that much earlier, so it lines up with the
    /// audio.
    pub(crate) offset: u32,
    /// Holds the track's audio back before the plugin when its sidechain
    /// comes later (through more latency), so the two line up. Notes the
    /// plugin gets aren't held back.
    pub(crate) pre_delay: Option<DelayLine>,
    /// `None` until carried over from the previous graph.
    pub(crate) processor: Option<Box<dyn PluginProcessor>>,
    /// Each parameter's value from the project (or its default).
    pub(crate) targets: Box<[f64]>,
    /// The values last sent to the processor (NaN: never sent).
    pub(crate) sent: Box<[f64]>,
    /// Automation: (parameter index, points).
    pub(crate) lanes: Box<[(u32, Lane)]>,
    pub(crate) bypass: bool,
    pub(crate) latency: u32,
    /// Takes notes: MIDI coming down the chain stops here (and what the
    /// plugin sends goes on instead).
    pub(crate) notes_in: bool,
    /// Events for the current block. Never grows past its capacity.
    pub(crate) events: Vec<PluginEvent>,
    /// The slot it's in.
    pub(crate) slot: dawkey_core::PluginId,
    /// Has a sidechain input, and how many other outputs.
    pub(crate) takes_sidechain: bool,
    pub(crate) output_count: u32,
    pub(crate) sidechain: Option<SidechainIn>,
    /// Its other outputs that go to a bus (up to the last one that does).
    pub(crate) aux: Box<[AuxBuffer]>,
    pub(crate) aux_routes: Box<[AuxRoute]>,
}

/// A stereo delay of a fixed number of frames.
#[derive(Debug)]
pub(crate) struct DelayLine {
    pub(crate) frames: usize,
    /// Interleaved stereo ring of `frames` frames.
    pub(crate) ring: Box<[f32]>,
    pub(crate) at: usize,
}

impl DelayLine {
    /// A delay of `frames` (none for 0).
    pub(crate) fn new(frames: u32) -> Option<Self> {
        (frames > 0).then(|| Self {
            frames: frames as usize,
            ring: vec![0.0; frames as usize * 2].into_boxed_slice(),
            at: 0,
        })
    }
}

/// Fader and pan, the last stage of a track.
#[derive(Debug)]
pub(crate) struct Fader {
    pub(crate) gain_db: f32,
    pub(crate) pan: f32,
    /// The track's sources are mono: pan with equal power (-3 dB each side
    /// at center). Otherwise pan is a balance control.
    pub(crate) mono: bool,
    /// Automation, in dB and in pan units: (frame, value).
    pub(crate) gain_lane: Option<Lane>,
    pub(crate) pan_lane: Option<Lane>,
    /// The gains without automation.
    pub(crate) fixed: [f32; 2],
    /// How late the audio reaching the fader is (frames): automation is
    /// read that much earlier.
    pub(crate) offset: u32,
}

impl Fader {
    /// Gain into each output lane for a fader setting.
    pub(crate) fn gains(&self, gain_db: f32, pan: f32) -> [f32; 2] {
        let pan = if self.mono {
            constant_power_pan(pan)
        } else {
            balance(pan)
        };
        let fader = db_to_gain(gain_db);
        pan.map(|p| p * fader)
    }

    /// Gains at timeline frame `frame`, with automation.
    pub(crate) fn gains_at(&self, frame: u64) -> [f32; 2] {
        let gain = self
            .gain_lane
            .as_deref()
            .and_then(|l| lane_value(l, frame))
            .map_or(self.gain_db, |v| v as f32);
        let pan = self
            .pan_lane
            .as_deref()
            .and_then(|l| lane_value(l, frame))
            .map_or(self.pan, |v| v as f32);
        self.gains(gain, pan)
    }

    pub(crate) fn automated(&self) -> bool {
        self.gain_lane.is_some() || self.pan_lane.is_some()
    }
}

/// The value of an automation lane at `frame`: linear between points,
/// holding before the first and after the last. Real-time safe.
pub(crate) fn lane_value(points: &[(u64, f64)], frame: u64) -> Option<f64> {
    let next = points.partition_point(|&(f, _)| f <= frame);
    match (next.checked_sub(1).map(|i| points[i]), points.get(next)) {
        (None, None) => None,
        (Some((_, a)), None) => Some(a),
        (None, Some(&(_, b))) => Some(b),
        (Some((fa, a)), Some(&(fb, b))) => {
            Some(a + (b - a) * ((frame - fa) as f64 / (fb - fa) as f64))
        }
    }
}

impl Graph {
    /// A graph that plays nothing.
    pub fn empty() -> Self {
        Self {
            tracks: Vec::new(),
            meters: Arc::new(MeterBank::new(Vec::new())),
            loop_range: None,
            latency: 0,
            tempo: TempoMap::new(48_000, 120.0, &[]),
            meter: MeterMap::new(TimeSignature::default(), &[]),
        }
    }

    /// Compiles `project` without plugins. Clips whose audio is not in
    /// `media` are skipped.
    pub fn compile(project: &Project, media: &MediaPool) -> Self {
        Self::compile_with(project, media, &mut NoPlugins)
    }

    /// Compiles `project`, getting plugins from `plugins`.
    pub fn compile_with(
        project: &Project,
        media: &MediaPool,
        plugins: &mut dyn PluginProvider,
    ) -> Self {
        Self::compile_with_devices(project, media, plugins, &BTreeMap::new())
    }

    /// Like [`compile_with`](Self::compile_with), adding each MIDI device's
    /// reported latency (ms, by name; what the audio system says it takes
    /// to deliver MIDI there) to the latency set on tracks that send to it.
    pub fn compile_with_devices(
        project: &Project,
        media: &MediaPool,
        plugins: &mut dyn PluginProvider,
        device_latency_ms: &BTreeMap<String, f64>,
    ) -> Self {
        let context = TrackContext {
            sample_rate: project.sample_rate,
            device_latency_ms,
        };
        let order = render_order(project);
        let index: HashMap<TrackId, usize> =
            order.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        let mut tracks: Vec<TrackNode> = order
            .iter()
            .enumerate()
            .map(|(i, track)| {
                let mut node = compile_track(track, media, plugins, &context);
                node.audible = audible(project, track);
                node.meter = project.track_index(track.id);
                node.sum = (track.kind == TrackKind::Bus)
                    .then(|| [(); 2].map(|()| vec![0.0; crate::MAX_BLOCK].into_boxed_slice()));
                // Always to a later node (a loop a damaged file made goes to
                // the master instead).
                node.dest = project
                    .destination(track.id)
                    .and_then(|d| index.get(&d).copied())
                    .filter(|&d| d > i)
                    .or_else(|| (!track.is_master()).then(|| order.len() - 1));
                node
            })
            .collect();
        route_plugins(&order, &index, &mut tracks);
        let latency = compensate(&mut tracks);
        let ids = project.tracks.iter().map(|t| t.id).collect();
        Self {
            tracks,
            meters: Arc::new(MeterBank::new(ids)),
            loop_range: project.loop_region.active(),
            latency,
            tempo: project.tempo_map(),
            meter: project.meter_map(),
        }
    }

    /// The transport for a block of `frames` frames at timeline frame
    /// `frame`. Real-time safe.
    pub(crate) fn transport(
        &self,
        frame: u64,
        frames: usize,
        playing: bool,
        recording: bool,
    ) -> Transport {
        let tempo = &self.tempo;
        let rate = tempo.sample_rate();
        let beats = tempo.beat_at(frame as f64);
        let bpm = tempo.bpm_at(beats);
        let bpm_end = tempo.bpm_at(tempo.beat_at((frame + frames as u64) as f64));
        // Just past a bar line by rounding is on it.
        let bar = self.meter.bar_at(beats + 1e-9);
        let sig = self.meter.meter_of_bar(bar);
        let loop_frames = self.loop_range.map(|r| (r.start as f64, r.end as f64));
        Transport {
            playing,
            recording,
            frame,
            seconds: frame as f64 / rate,
            beats,
            bpm,
            bpm_per_frame: if frames > 0 {
                (bpm_end - bpm) / frames as f64
            } else {
                0.0
            },
            bar,
            bar_start_beats: self.meter.bar_start(bar),
            numerator: sig.numerator,
            denominator: sig.denominator,
            loop_beats: loop_frames.map(|(a, b)| (tempo.beat_at(a), tempo.beat_at(b))),
            loop_seconds: loop_frames.map(|(a, b)| (a / rate, b / rate)),
        }
    }

    /// Frames every track is delayed by, to line up plugins and MIDI devices
    /// with latency.
    pub fn latency(&self) -> u32 {
        self.latency
    }

    /// Takes the plugin processors out (stopping them), e.g. after an
    /// offline render, so their host can unload them. Not real-time safe.
    pub fn take_plugins(&mut self) -> Vec<(u64, Box<dyn PluginProcessor>)> {
        let mut taken = Vec::new();
        for track in &mut self.tracks {
            for node in &mut track.plugins {
                if let Some(mut processor) = node.processor.take() {
                    processor.stop();
                    taken.push((node.key, processor));
                }
            }
        }
        taken
    }
}

/// What compiling a track needs to know about the rest of the project.
struct TrackContext<'a> {
    sample_rate: u32,
    device_latency_ms: &'a BTreeMap<String, f64>,
}

/// Every track and bus in an order where each comes after what it takes
/// from ([`Project::inputs`]: what's sent to it, its plugins' sidechains);
/// the master last.
fn render_order(project: &Project) -> Vec<&Track> {
    let tracks = &project.tracks;
    let inputs: Vec<Vec<TrackId>> = tracks.iter().map(|t| project.inputs(t.id)).collect();
    let mut placed: Vec<TrackId> = Vec::with_capacity(tracks.len());
    let mut order = Vec::with_capacity(tracks.len() + 1);
    loop {
        let before = order.len();
        for (i, track) in tracks.iter().enumerate() {
            if placed.contains(&track.id) || !inputs[i].iter().all(|t| placed.contains(t)) {
                continue;
            }
            placed.push(track.id);
            order.push(track);
        }
        if order.len() == before {
            break;
        }
    }
    // Only a loop (which commands don't allow) leaves tracks behind.
    order.extend(tracks.iter().filter(|t| !placed.contains(&t.id)));
    order.push(&project.master);
    order
}

/// Connects plugins' sidechains and other outputs to the tracks they come
/// from and the buses they go to (only those in render order: earlier
/// sources, later buses).
fn route_plugins(order: &[&Track], index: &HashMap<TrackId, usize>, tracks: &mut [TrackNode]) {
    let buffer = || [(); 2].map(|()| vec![0.0; crate::MAX_BLOCK].into_boxed_slice());
    let mut tapped = Vec::new();
    for (i, track) in order.iter().enumerate() {
        for node in &mut tracks[i].plugins {
            let Some(slot) = track.plugin(node.slot) else {
                continue;
            };
            let source = slot
                .sidechain
                .and_then(|s| index.get(&s).copied())
                .filter(|&s| s < i && node.takes_sidechain);
            if let Some(source) = source {
                node.sidechain = Some(SidechainIn {
                    source,
                    delay: None,
                    buffer: buffer(),
                });
                tapped.push(source);
            }
            let routes: Vec<AuxRoute> = slot
                .outputs
                .iter()
                .filter(|&(&output, _)| output < node.output_count)
                .filter_map(|(&output, bus)| {
                    let dest = index.get(bus).copied().filter(|&d| d > i)?;
                    Some(AuxRoute {
                        output: output as usize,
                        dest,
                        delay: None,
                    })
                })
                .collect();
            let count = routes.iter().map(|r| r.output + 1).max().unwrap_or(0);
            node.aux = (0..count).map(|_| AuxBuffer::new()).collect();
            node.aux_routes = routes.into();
        }
    }
    for source in tapped {
        tracks[source].tap.get_or_insert_with(buffer);
    }
}

/// Whether a track is heard: not muted, and with something soloed, it's
/// soloed, sent (through buses) to a soloed bus, or a bus a soloed track
/// goes through.
fn audible(project: &Project, track: &Track) -> bool {
    if track.mute {
        return false;
    }
    let soloed = project.tracks.iter().filter(|t| t.solo);
    track.is_master()
        || !project.tracks.iter().any(|t| t.solo)
        || soloed
            .into_iter()
            .any(|t| project.feeds(t.id, track.id) || project.feeds(track.id, t.id))
}

/// Delay compensation for tracks in render order: everything that arrives
/// at a bus (and the master) lines up, as does the mix with MIDI devices
/// that sound late, and each sidechain with the plugin it feeds (holding
/// the plugin's track back when the sidechain is the later one). Sets each
/// track's delay on its way out, and where its plugins and fader are in
/// time; returns the graph's latency.
fn compensate(tracks: &mut [TrackNode]) -> u32 {
    // How late what reaches each track is, and what leaves it. Sources come
    // earlier in render order, so they're known by the time they're used.
    let mut arriving = vec![0u32; tracks.len()];
    let mut leaving = vec![0u32; tracks.len()];
    for i in 0..tracks.len() {
        let track = &mut tracks[i];
        let mut at = arriving[i];
        for plugin in &mut track.plugins {
            plugin.pre_delay = None;
            if let Some(sidechain) = &mut plugin.sidechain {
                let source = leaving[sidechain.source];
                if source > at && !plugin.bypass {
                    plugin.pre_delay = DelayLine::new(source - at);
                    at = source;
                }
                sidechain.delay = DelayLine::new(at.saturating_sub(source));
            }
            plugin.offset = at;
            if !plugin.bypass {
                at += plugin.latency;
            }
            // Other outputs leave from their plugin.
            for route in plugin.aux_routes.iter() {
                arriving[route.dest] = arriving[route.dest].max(at);
            }
        }
        leaving[i] = at;
        if let Some(d) = track.dest {
            arriving[d] = arriving[d].max(at);
        }
    }
    let mix = tracks
        .iter()
        .zip(&leaving)
        .filter(|(t, _)| t.dest.is_none())
        .map(|(_, &l)| l)
        .max()
        .unwrap_or(0);
    // A device that sounds late counts like a plugin with latency: the
    // rest of the mix waits for it.
    let devices = tracks
        .iter()
        .filter_map(|t| t.device.as_ref().map(|d| d.latency))
        .max()
        .unwrap_or(0);
    let latency = mix.max(devices);
    for (i, track) in tracks.iter_mut().enumerate() {
        let due = track.dest.map_or(latency, |d| arriving[d]);
        track.delay = DelayLine::new(due - leaving[i]);
        track.fader.offset = leaving[i];
        for plugin in &mut track.plugins {
            let at = plugin.offset + if plugin.bypass { 0 } else { plugin.latency };
            for route in plugin.aux_routes.iter_mut() {
                route.delay = DelayLine::new(arriving[route.dest] - at);
            }
        }
        if let Some(device) = &mut track.device {
            device.delay = latency - device.latency;
        }
    }
    latency
}

fn compile_track(
    track: &Track,
    media: &MediaPool,
    provider: &mut dyn PluginProvider,
    context: &TrackContext,
) -> TrackNode {
    let &TrackContext { sample_rate, .. } = context;
    // (slot index, plugin) for the slots that could be loaded.
    let plugins: Vec<(usize, CompiledPlugin)> = track
        .plugins
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| {
            let provided = provider.provide(track.id, slot)?;
            Some((i, compile_plugin(track, slot, provided)))
        })
        .collect();
    // An unbypassed instrument plugin first in the chain plays the notes.
    let first = plugins
        .first()
        .filter(|(i, p)| *i == 0 && p.node.notes_in && !p.node.bypass);
    let plugin_instrument = track.kind == TrackKind::Midi && first.is_some();
    // With a device, notes go through the chain only if it starts with a
    // MIDI effect; otherwise straight to the device.
    let device = (track.kind == TrackKind::Midi)
        .then_some(track.midi_output.as_ref())
        .flatten()
        .map(|output| {
            Box::new(External {
                tracker: NoteTracker::default(),
                events: Vec::with_capacity(MAX_PLUGIN_EVENTS),
                device: device_tag(&output.device),
                channel: output.channel.clamp(1, 16) - 1,
                latency: {
                    let reported = context.device_latency_ms.get(&output.device);
                    let ms = output.latency_ms.clamp(0.0, DEVICE_LATENCY_MS_MAX)
                        + reported.map_or(0.0, |ms| ms.clamp(0.0, DEVICE_LATENCY_MS_MAX));
                    (ms / 1000.0 * f64::from(sample_rate)).round() as u32
                },
                delay: 0,
                pending: Vec::with_capacity(MAX_PENDING_MIDI),
            })
        });
    let through_chain = first.is_some_and(|(_, p)| p.midi_output);
    let midi_effects = plugins
        .iter()
        .enumerate()
        .take_while(|(n, (i, p))| *n == *i && p.node.notes_in && p.midi_output && !p.node.bypass)
        .count();
    let to_device_after = (device.is_some() && through_chain).then_some(midi_effects);
    let voices = match (&device, plugin_instrument) {
        (Some(_), _) if !through_chain => Choice::Device,
        (_, true) => Choice::Plugin,
        _ => Choice::Synth,
    };
    let instrument = (track.kind == TrackKind::Midi)
        .then(|| Box::new(compile_instrument(track, sample_rate, voices)));
    let clips = compile_clips(track, media);
    let input = (track.kind == TrackKind::Audio && track.armed).then_some(TrackInput {
        channels: track.input,
        monitor: track.monitor,
    });
    let stereo_input = input.is_some_and(|i| i.monitor && i.channels == InputChannels::Stereo);
    let mono = match track.kind {
        TrackKind::Midi => !plugin_instrument || track.midi_output.is_some(),
        TrackKind::Audio => clips.iter().all(|c| c.data.channel_count() == 1) && !stereo_input,
        TrackKind::Bus => false,
    };
    let lane = |target: AutomationTarget| {
        track
            .lane(&target)
            .map(|l| l.points.iter().map(|p| (p.frame, p.value)).collect())
    };
    let mut fader = Fader {
        gain_db: track.gain_db,
        pan: track.pan,
        mono,
        gain_lane: lane(AutomationTarget::Gain),
        pan_lane: lane(AutomationTarget::Pan),
        fixed: [0.0; 2],
        offset: 0,
    };
    fader.fixed = fader.gains(track.gain_db, track.pan);
    let chain_midi = if plugins.is_empty() {
        [Vec::new(), Vec::new()]
    } else {
        [(); 2].map(|()| Vec::with_capacity(MAX_PLUGIN_EVENTS))
    };
    TrackNode {
        id: track.id,
        audible: true,
        clips,
        input,
        instrument,
        device,
        plugins: plugins.into_iter().map(|(_, p)| p.node).collect(),
        chain_midi,
        to_device_after,
        delay: None,
        fader,
        dest: None,
        sum: None,
        meter: None,
        tap: None,
    }
}

/// What plays a MIDI track's notes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Synth,
    Plugin,
    Device,
}

/// A compiled plugin, and whether it sends MIDI.
struct CompiledPlugin {
    node: PluginNode,
    midi_output: bool,
}

fn compile_plugin(track: &Track, slot: &PluginSlot, provided: ProvidedPlugin) -> CompiledPlugin {
    let targets: Box<[f64]> = provided
        .params
        .iter()
        .map(|(id, default)| slot.params.get(id).copied().unwrap_or(*default))
        .collect();
    let lanes: Box<[(u32, Lane)]> = track
        .automation
        .iter()
        .filter_map(|lane| match &lane.target {
            AutomationTarget::Param { plugin, param } if *plugin == slot.id => {
                let index = provided.params.iter().position(|(id, _)| id == param)?;
                let points = lane.points.iter().map(|p| (p.frame, p.value)).collect();
                Some((index as u32, points))
            }
            _ => None,
        })
        .collect();
    CompiledPlugin {
        midi_output: provided.midi_output,
        node: PluginNode {
            key: provided.key,
            offset: 0,
            pre_delay: None,
            processor: provided.processor,
            sent: vec![f64::NAN; targets.len()].into_boxed_slice(),
            targets,
            lanes,
            bypass: slot.bypass,
            latency: provided.latency,
            notes_in: provided.instrument,
            events: Vec::with_capacity(MAX_PLUGIN_EVENTS),
            slot: slot.id,
            takes_sidechain: provided.sidechain,
            output_count: provided.outputs,
            sidechain: None,
            aux: Box::new([]),
            aux_routes: Box::new([]),
        },
    }
}

fn compile_instrument(track: &Track, sample_rate: u32, voices: Choice) -> Instrument {
    let mut notes: Vec<NoteEvent> = track
        .clips
        .iter()
        .filter(|clip| !clip.muted)
        .flat_map(|clip| {
            let gain = db_to_gain(clip.gain_db);
            clip.notes()
                .iter()
                .filter(move |n| n.start < clip.length)
                .map(move |n| {
                    let start = clip.start + n.start;
                    NoteEvent {
                        start,
                        end: (start + n.length).min(clip.end()),
                        tag: n.id.0 & !(LIVE | CHAIN),
                        pitch: n.pitch,
                        amp: f32::from(n.velocity) / 127.0 * gain,
                    }
                })
        })
        .collect();
    notes.sort_by_key(|n| (n.start, n.tag));
    let mut ends: Vec<(u64, u64)> = notes.iter().map(|n| (n.tag, n.end)).collect();
    ends.sort_unstable();
    let longest = notes.iter().map(|n| n.end - n.start).max().unwrap_or(0);
    let mut controls: Vec<(Controller, ControlEvent)> = track
        .clips
        .iter()
        .filter(|clip| !clip.muted)
        .flat_map(|clip| {
            clip.controls()
                .iter()
                // A change at the very end (a pedal lifted with the last
                // note) still plays.
                .filter(move |c| c.frame <= clip.length)
                .map(move |c| {
                    let event = ControlEvent {
                        frame: clip.start + c.frame,
                        data: c.controller.message(0, c.value),
                        value: c.controller.value32(c.value),
                    };
                    (c.controller, event)
                })
        })
        .collect();
    controls.sort_by_key(|(_, e)| e.frame);
    let mut used: Vec<Controller> = controls.iter().map(|(c, _)| *c).collect();
    used.sort_unstable();
    used.dedup();
    let lanes = used
        .into_iter()
        .map(|controller| ControlLane {
            default: controller.resets().then(|| {
                let value = controller.default_value();
                (controller.message(0, value), controller.value32(value))
            }),
            events: controls
                .iter()
                .filter(|(c, _)| *c == controller)
                .map(|(_, e)| *e)
                .collect(),
        })
        .collect();
    let voices = match voices {
        Choice::Device => Voices::Device,
        Choice::Plugin => Voices::Plugin(Box::default()),
        Choice::Synth => Voices::Synth(Box::new(PolySynth::new(
            sample_rate,
            &track.synth.unwrap_or_default(),
        ))),
    };
    Instrument {
        voices,
        notes: notes.into(),
        ends: ends.into(),
        longest,
        controls: controls.into_iter().map(|(_, e)| e).collect(),
        lanes,
        monitor: track.armed,
        input: InputFilter::new(track.midi_input.as_deref(), track.midi_input_channel),
    }
}

fn compile_clips(track: &Track, media: &MediaPool) -> Vec<ClipNode> {
    track
        .clips
        .iter()
        .filter(|clip| !clip.muted)
        .filter_map(|clip| {
            let ClipContent::Audio { source, offset } = clip.content else {
                return None;
            };
            let data = media.get(&source)?;
            let offset = usize::try_from(offset).ok()?;
            // The decoded audio can be slightly shorter than the model says
            // (e.g. after resampling); never read past it.
            let available = data.frames().checked_sub(offset)? as u64;
            let length = clip.length.min(available);
            if length == 0 {
                return None;
            }
            Some(ClipNode {
                start: clip.start,
                end: clip.start + length,
                offset,
                data: Arc::clone(data),
                gain: db_to_gain(clip.gain_db),
            })
        })
        .collect()
}

pub(crate) fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Pans a mono signal with equal power: -3 dB in each channel at center.
pub(crate) fn constant_power_pan(pan: f32) -> [f32; 2] {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * FRAC_PI_4;
    [angle.cos(), angle.sin()]
}

/// Balances a stereo signal: unity at center, attenuating the opposite side.
fn balance(pan: f32) -> [f32; 2] {
    let pan = pan.clamp(-1.0, 1.0);
    [(1.0 - pan).min(1.0), (1.0 + pan).min(1.0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pan_laws() {
        let [l, r] = constant_power_pan(0.0);
        assert!((l - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6 && (l - r).abs() < 1e-6);
        let [l, r] = constant_power_pan(-1.0);
        assert!((l - 1.0).abs() < 1e-6 && r.abs() < 1e-6);
        assert_eq!(balance(0.0), [1.0, 1.0]);
        assert_eq!(balance(1.0), [0.0, 1.0]);
        assert_eq!(balance(-0.5), [1.0, 0.5]);
    }

    #[test]
    fn lanes_interpolate_and_hold() {
        let lane = [(100, 0.0), (200, 1.0)];
        assert_eq!(lane_value(&lane, 0), Some(0.0));
        assert_eq!(lane_value(&lane, 150), Some(0.5));
        assert_eq!(lane_value(&lane, 200), Some(1.0));
        assert_eq!(lane_value(&lane, 999), Some(1.0));
        assert_eq!(lane_value(&[], 5), None);
    }
}
