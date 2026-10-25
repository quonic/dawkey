// SPDX-License-Identifier: AGPL-3.0-or-later
//! Engine tests with plugins: fake processors stand in for hosted ones
//! (`dawkey-plugins` has the real ones), so these check what the engine does
//! with them: parameter and note events, automation, carrying processors
//! across graphs, latency compensation.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use dawkey_core::{
    AddClip, AddMidiClip, AddPlugin, AutomationTarget, ControlSpec, Controller, NoteSpec, PluginId,
    PluginSlot, PointSpec, RemovePlugin, Session, SetAutomation, SetLoop, SetPluginBypass,
    SetPluginParam, SourceId, TrackId,
};
use dawkey_media::AudioData;

use crate::{
    Graph, MediaPool, MidiSink, PluginEvent, PluginEventKind, PluginProcessor, PluginProvider,
    ProcessContext, ProvidedPlugin, new_engine, render_offline,
};

const RATE: u32 = 48_000;

/// Multiplies by parameter 0 (linear gain, default 1).
struct Scale {
    gain: f64,
}

/// Outputs how many frames it has processed (to show it's the same
/// instance after a graph swap).
struct Counter {
    frames: f32,
}

/// Delays by 64 frames (and reports it).
struct Late {
    ring: [f32; 64],
    at: usize,
}

/// An instrument: outputs the sum of held velocities, times the pitch
/// bend (1 at no bend, 2 at full bend up).
struct Keys {
    held: [f32; 128],
    bend: f32,
}

impl PluginProcessor for Scale {
    fn process(
        &mut self,
        audio: [&mut [f32]; 2],
        _context: ProcessContext,
        events: &[PluginEvent],
        _midi_out: MidiSink,
    ) {
        let [left, right] = audio;
        let mut next = 0;
        for i in 0..left.len() {
            while let Some(e) = events.get(next).filter(|e| e.offset as usize <= i) {
                if let PluginEventKind::Param { index: 0, value } = e.kind {
                    self.gain = value;
                }
                next += 1;
            }
            left[i] *= self.gain as f32;
            right[i] *= self.gain as f32;
        }
    }
    fn stop(&mut self) {}
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

impl PluginProcessor for Counter {
    fn process(
        &mut self,
        audio: [&mut [f32]; 2],
        _context: ProcessContext,
        _events: &[PluginEvent],
        _midi_out: MidiSink,
    ) {
        let [left, right] = audio;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            *l = self.frames;
            *r = self.frames;
            self.frames += 1.0;
        }
    }
    fn stop(&mut self) {}
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

impl PluginProcessor for Late {
    fn process(
        &mut self,
        audio: [&mut [f32]; 2],
        _context: ProcessContext,
        _events: &[PluginEvent],
        _midi_out: MidiSink,
    ) {
        let [left, right] = audio;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let out = self.ring[self.at];
            self.ring[self.at] = *l;
            self.at = (self.at + 1) % 64;
            *l = out;
            *r = out;
        }
    }
    fn stop(&mut self) {}
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

impl PluginProcessor for Keys {
    fn process(
        &mut self,
        audio: [&mut [f32]; 2],
        _context: ProcessContext,
        events: &[PluginEvent],
        _midi_out: MidiSink,
    ) {
        let [left, right] = audio;
        let mut next = 0;
        for i in 0..left.len() {
            while let Some(e) = events.get(next).filter(|e| e.offset as usize <= i) {
                match e.kind {
                    PluginEventKind::NoteOn { key, velocity, .. } => {
                        self.held[usize::from(key)] = velocity
                    }
                    PluginEventKind::NoteOff { key, .. } => self.held[usize::from(key)] = 0.0,
                    PluginEventKind::Midi {
                        data: [0xe0, lsb, msb],
                    } => self.bend = f32::from(u16::from(lsb) | u16::from(msb) << 7) / 8192.0,
                    _ => {}
                }
                next += 1;
            }
            let level: f32 = self.held.iter().sum::<f32>() * self.bend;
            left[i] = level;
            right[i] = level;
        }
    }
    fn stop(&mut self) {}
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

/// A MIDI effect: sends each note an octave up, and no audio.
struct Octave;

impl PluginProcessor for Octave {
    fn process(
        &mut self,
        _audio: [&mut [f32]; 2],
        _context: ProcessContext,
        events: &[PluginEvent],
        mut out: MidiSink,
    ) {
        for e in events {
            match e.kind {
                PluginEventKind::NoteOn {
                    channel,
                    key,
                    velocity,
                } => out.push(
                    e.offset,
                    PluginEventKind::NoteOn {
                        channel,
                        key: key + 12,
                        velocity,
                    },
                ),
                PluginEventKind::NoteOff { channel, key } => out.push(
                    e.offset,
                    PluginEventKind::NoteOff {
                        channel,
                        key: key + 12,
                    },
                ),
                _ => {}
            }
        }
    }
    fn stop(&mut self) {}
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

/// Subtracts its sidechain.
struct Minus;

impl PluginProcessor for Minus {
    fn process(
        &mut self,
        audio: [&mut [f32]; 2],
        context: ProcessContext,
        _events: &[PluginEvent],
        _midi_out: MidiSink,
    ) {
        if let Some(side) = context.sidechain {
            for (channel, side) in audio.into_iter().zip(side) {
                for (s, x) in channel.iter_mut().zip(side) {
                    *s -= x;
                }
            }
        }
    }
    fn stop(&mut self) {}
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

/// Passes its input on, and twice it out of its other output.
struct Fork;

impl PluginProcessor for Fork {
    fn process(
        &mut self,
        audio: [&mut [f32]; 2],
        context: ProcessContext,
        _events: &[PluginEvent],
        _midi_out: MidiSink,
    ) {
        for aux in context.aux.iter_mut() {
            for (out, channel) in aux.channels.iter_mut().zip(&audio) {
                for (o, s) in out.iter_mut().zip(channel.iter()) {
                    *o = s * 2.0;
                }
            }
        }
    }
    fn stop(&mut self) {}
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

/// Like the studio: a new processor the first time a slot is seen, then
/// "keep the running one".
#[derive(Default)]
struct Provider {
    keys: HashMap<PluginId, u64>,
}

impl PluginProvider for Provider {
    fn provide(&mut self, _track: TrackId, slot: &PluginSlot) -> Option<ProvidedPlugin> {
        let next = self.keys.len() as u64 + 1;
        let (key, new) = match self.keys.get(&slot.id) {
            Some(&key) => (key, false),
            None => {
                self.keys.insert(slot.id, next);
                (next, true)
            }
        };
        let midi_output = slot.plugin == "clap:octave";
        let sidechain = slot.plugin == "clap:minus";
        let outputs = u32::from(slot.plugin == "clap:fork");
        let (processor, params, latency, instrument): (Box<dyn PluginProcessor>, _, _, _) =
            match slot.plugin.as_str() {
                "clap:scale" => (
                    Box::new(Scale { gain: 1.0 }),
                    vec![("gain".into(), 1.0)],
                    0,
                    false,
                ),
                "clap:counter" => (Box::new(Counter { frames: 0.0 }), vec![], 0, false),
                "clap:late" => (
                    Box::new(Late {
                        ring: [0.0; 64],
                        at: 0,
                    }),
                    vec![],
                    64,
                    false,
                ),
                "clap:keys" => (
                    Box::new(Keys {
                        held: [0.0; 128],
                        bend: 1.0,
                    }),
                    vec![],
                    0,
                    true,
                ),
                "clap:octave" => (Box::new(Octave), vec![], 0, true),
                "clap:minus" => (Box::new(Minus), vec![], 0, false),
                "clap:fork" => (Box::new(Fork), vec![], 0, false),
                _ => return None,
            };
        Some(ProvidedPlugin {
            key,
            processor: new.then_some(processor),
            params,
            latency,
            instrument,
            midi_output,
            sidechain,
            outputs,
        })
    }
}

struct Fixture {
    session: Session,
    media: MediaPool,
    provider: Provider,
}

impl Fixture {
    fn new() -> Self {
        Self {
            session: Session::default(),
            media: MediaPool::new(),
            provider: Provider::default(),
        }
    }

    /// A mono clip of `value`s at 0 s on a new track.
    fn constant(&mut self, value: f32, frames: usize) -> TrackId {
        let data = AudioData::new(vec![vec![value; frames]]);
        let id: SourceId = self.session.add_source("c", "/dev/null", 1, frames as u64);
        self.media.insert(id, Arc::new(data));
        self.session
            .execute(AddClip::new(id, None, 0.0).into())
            .unwrap()
            .track
            .unwrap()
    }

    fn add(&mut self, track: TrackId, plugin: &str) -> PluginId {
        self.session
            .execute(
                AddPlugin {
                    track,
                    plugin: plugin.into(),
                    index: None,
                }
                .into(),
            )
            .unwrap()
            .plugin
            .unwrap()
    }

    fn graph(&mut self) -> Graph {
        Graph::compile_with(self.session.project(), &self.media, &mut self.provider)
    }

    /// A graph with new processors, for a new engine or render.
    fn fresh_graph(&mut self) -> Graph {
        self.provider = Provider::default();
        self.graph()
    }
}

/// Left channel of `frames` frames.
fn process(proc: &mut crate::EngineProcessor, frames: usize) -> Vec<f32> {
    let mut l = vec![0.0; frames];
    let mut r = vec![0.0; frames];
    proc.process(&mut l, &mut r);
    l
}

/// Equal-power center pan of a mono track.
const CENTER: f32 = std::f32::consts::FRAC_1_SQRT_2;

#[test]
fn params_reach_plugins_and_processors_survive_swaps() {
    let mut f = Fixture::new();
    let track = f.constant(1.0, 48_000);
    let scale = f.add(track, "clap:scale");
    let (mut ctl, mut proc) = new_engine(RATE, f.graph());
    ctl.play().unwrap();
    let out = process(&mut proc, 64);
    assert!((out[0] - CENTER).abs() < 1e-6);

    f.session
        .execute(SetPluginParam::new(scale, "gain", 0.5).into())
        .unwrap();
    // Each recompile keeps the running processor (the provider gives none).
    let counter = f.add(track, "clap:counter");
    ctl.set_graph(f.graph()).unwrap();
    process(&mut proc, 64);
    f.session
        .execute(
            SetPluginBypass {
                plugin: counter,
                bypass: true,
            }
            .into(),
        )
        .unwrap();
    ctl.set_graph(f.graph()).unwrap();
    let out = process(&mut proc, 64);
    assert!((out[10] - CENTER * 0.5).abs() < 1e-6, "{}", out[10]);

    // The counter kept counting across the swap where it was bypassed.
    f.session
        .execute(
            SetPluginBypass {
                plugin: counter,
                bypass: false,
            }
            .into(),
        )
        .unwrap();
    ctl.set_graph(f.graph()).unwrap();
    let out = process(&mut proc, 64);
    assert_eq!(out[0], 64.0 * CENTER);

    // A removed plugin's processor comes back to the controller.
    f.session
        .execute(RemovePlugin { plugin: counter }.into())
        .unwrap();
    ctl.set_graph(f.graph()).unwrap();
    process(&mut proc, 64);
    let retired = ctl.take_retired_plugins();
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].0, 2);
}

#[test]
fn automation_moves_params_and_the_fader() {
    let mut f = Fixture::new();
    let track = f.constant(1.0, 48_000);
    let scale = f.add(track, "clap:scale");
    let points = |pairs: &[(f64, f64)]| -> Vec<PointSpec> {
        pairs
            .iter()
            .map(|&(time_s, value)| PointSpec { time_s, value })
            .collect()
    };
    // Gain parameter 0 → 1 over the first 1024 frames.
    let ramp_s = 1024.0 / f64::from(RATE);
    f.session
        .execute(
            SetAutomation {
                track,
                target: AutomationTarget::Param {
                    plugin: scale,
                    param: "gain".into(),
                },
                points: points(&[(0.0, 0.0), (ramp_s, 1.0)]),
            }
            .into(),
        )
        .unwrap();
    let out = &render_offline(f.graph(), RATE, 0, 2048)[0];
    // Steps of 64 frames: frame 64 has the value at frame 64 (1/16).
    assert_eq!(out[0], 0.0);
    assert!((out[64] / CENTER - 1.0 / 16.0).abs() < 1e-6, "{}", out[64]);
    assert!((out[1500] / CENTER - 1.0).abs() < 1e-6);

    // The fader: -inf.. no: 0 dB down to -6.0206 dB (half) over 1024 frames.
    f.session
        .execute(
            SetAutomation {
                track,
                target: AutomationTarget::Param {
                    plugin: scale,
                    param: "gain".into(),
                },
                points: vec![],
            }
            .into(),
        )
        .unwrap();
    f.session
        .execute(
            SetAutomation {
                track,
                target: AutomationTarget::Gain,
                points: points(&[(0.0, 0.0), (ramp_s, -6.020_599_913_279_624)]),
            }
            .into(),
        )
        .unwrap();
    let out = &render_offline(f.graph(), RATE, 0, 2048)[0];
    assert!((out[0] - CENTER).abs() < 1e-6);
    assert!((out[2000] - CENTER * 0.5).abs() < 1e-5, "{}", out[2000]);
    // In between, it moves smoothly downwards.
    assert!(out[512] < out[256] && out[256] < out[0]);
}

#[test]
fn latency_is_compensated_across_tracks() {
    let mut f = Fixture::new();
    // Two tracks with an impulse at frame 100; one has a 64-frame plugin.
    let impulse = |f: &mut Fixture| {
        let mut samples = vec![0.0; 1000];
        samples[100] = 1.0;
        let id = f.session.add_source("i", "/dev/null", 1, 1000);
        f.media.insert(id, Arc::new(AudioData::new(vec![samples])));
        f.session
            .execute(AddClip::new(id, None, 0.0).into())
            .unwrap()
            .track
            .unwrap()
    };
    let a = impulse(&mut f);
    impulse(&mut f);
    f.add(a, "clap:late");
    let graph = f.graph();
    assert_eq!(graph.latency(), 64);
    // Offline renders line up with the timeline again.
    let out = &render_offline(graph, RATE, 0, 400)[0];
    let hits: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, s)| s.abs() > 1e-6)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(hits, [100], "both tracks' impulses at frame 100: {hits:?}");
    assert!((out[100] - 2.0 * CENTER).abs() < 1e-6);
}

impl Fixture {
    /// A new bus.
    fn bus(&mut self) -> TrackId {
        self.session
            .execute(
                dawkey_core::AddTrack {
                    kind: dawkey_core::TrackKind::Bus,
                    ..Default::default()
                }
                .into(),
            )
            .unwrap()
            .track
            .unwrap()
    }

    fn send(&mut self, track: TrackId, bus: Option<TrackId>) {
        self.session
            .execute(dawkey_core::SetTrackOutput { track, bus }.into())
            .unwrap();
    }

    fn set(&mut self, command: impl Into<dawkey_core::Command>) {
        self.session.execute(command.into()).unwrap();
    }

    /// Renders `frames` from the start offline, with fresh processors (like
    /// the studio's offline renders).
    fn render(&mut self, frames: usize) -> Vec<Vec<f32>> {
        self.provider = Provider::default();
        render_offline(self.graph(), RATE, 0, frames)
    }
}

#[test]
fn buses_mix_what_is_sent_to_them() {
    let mut f = Fixture::new();
    let a = f.constant(0.25, 1000);
    let b = f.constant(0.25, 1000);
    let bus = f.bus();
    f.send(a, Some(bus));
    f.send(b, Some(bus));
    // Both tracks pan their mono clips to center; the bus is stereo (no
    // pan law at center) and halves them.
    f.set(dawkey_core::SetTrackGain {
        track: bus,
        gain_db: -6.020_6,
    });
    let out = &f.render(100)[0];
    assert!((out[50] - 0.25 * CENTER).abs() < 1e-5, "{}", out[50]);
    // Plugins on the bus see the mix: the scale plugin at 2 doubles it.
    let scale = f.add(bus, "clap:scale");
    f.set(dawkey_core::SetPluginParam::new(scale, "gain", 2.0));
    let out = &f.render(100)[0];
    assert!((out[50] - 0.5 * CENTER).abs() < 1e-5, "{}", out[50]);
    // And the master (track 0) has plugins and a fader too.
    f.add(dawkey_core::MASTER, "clap:scale");
    f.set(dawkey_core::SetTrackGain {
        track: dawkey_core::MASTER,
        gain_db: -6.020_6,
    });
    let out = &f.render(100)[0];
    assert!((out[50] - 0.25 * CENTER).abs() < 1e-5, "{}", out[50]);
    // Muting the bus silences what goes through it.
    f.set(dawkey_core::SetTrackMute {
        track: bus,
        mute: true,
    });
    let out = &f.render(100)[0];
    assert_eq!(out[50], 0.0);
}

#[test]
fn soloing_follows_the_routing() {
    let mut f = Fixture::new();
    let a = f.constant(0.25, 1000);
    let b = f.constant(0.5, 1000);
    let c = f.constant(1.0, 1000);
    let bus = f.bus();
    f.send(a, Some(bus));
    f.send(b, Some(bus));
    let level = |f: &mut Fixture| f.render(100)[0][50] / CENTER;
    // Soloing a track keeps the bus it goes through.
    f.set(dawkey_core::SetTrackSolo {
        track: a,
        solo: true,
    });
    assert!((level(&mut f) - 0.25).abs() < 1e-5);
    // Soloing the bus keeps what goes into it, not the rest.
    f.set(dawkey_core::SetTrackSolo {
        track: a,
        solo: false,
    });
    f.set(dawkey_core::SetTrackSolo {
        track: bus,
        solo: true,
    });
    assert!((level(&mut f) - 0.75).abs() < 1e-5);
    f.set(dawkey_core::SetTrackSolo {
        track: c,
        solo: true,
    });
    assert!((level(&mut f) - 1.75).abs() < 1e-5);
}

#[test]
fn latency_is_compensated_through_buses() {
    let mut f = Fixture::new();
    let impulse = |f: &mut Fixture| {
        let mut samples = vec![0.0; 1000];
        samples[100] = 1.0;
        let id = f.session.add_source("i", "/dev/null", 1, 1000);
        f.media.insert(id, Arc::new(AudioData::new(vec![samples])));
        f.session
            .execute(AddClip::new(id, None, 0.0).into())
            .unwrap()
            .track
            .unwrap()
    };
    // Into the bus: a late track and a direct one. The bus is late too,
    // and a third track goes straight to the master.
    let late = impulse(&mut f);
    let direct = impulse(&mut f);
    impulse(&mut f);
    let bus = f.bus();
    f.send(late, Some(bus));
    f.send(direct, Some(bus));
    f.add(late, "clap:late");
    f.add(bus, "clap:late");
    let graph = f.graph();
    assert_eq!(graph.latency(), 128);
    let out = &render_offline(graph, RATE, 0, 400)[0];
    let hits: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, s)| s.abs() > 1e-6)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(hits, [100], "every impulse at frame 100: {hits:?}");
    assert!((out[100] - 3.0 * CENTER).abs() < 1e-6);
}

#[test]
fn sidechains_and_other_outputs() {
    let mut f = Fixture::new();
    let kick = f.constant(0.25, 1000);
    let bass = f.constant(0.25, 1000);
    // The bass less the kick (after its fader and pan), and the kick.
    let minus = f.add(bass, "clap:minus");
    f.set(dawkey_core::SetPluginSidechain {
        plugin: minus,
        source: Some(kick),
    });
    let kick_out = 0.25 * CENTER;
    let bass_out = (0.25 - kick_out) * CENTER;
    let out = &f.render(100)[0];
    assert!(
        (out[50] - (kick_out + bass_out)).abs() < 1e-6,
        "{}",
        out[50]
    );
    // Muted, the kick still feeds the sidechain.
    f.set(dawkey_core::SetTrackMute {
        track: kick,
        mute: true,
    });
    assert!((f.render(100)[0][50] - bass_out).abs() < 1e-6);
    f.set(dawkey_core::SetTrackMute {
        track: kick,
        mute: false,
    });

    // The fork's other output goes to a bus: twice the kick there.
    let bus = f.bus();
    let fork = f.add(kick, "clap:fork");
    f.set(dawkey_core::SetPluginOutput {
        plugin: fork,
        output: 0,
        bus: Some(bus),
    });
    f.set(dawkey_core::RemovePlugin { plugin: minus });
    let out = &f.render(100)[0];
    // Kick, bass, and twice the kick straight from the plugin (before the
    // track's fader and pan) through the bus.
    let mix = 2.0 * 0.25 * CENTER + 0.5;
    assert!((out[50] - mix).abs() < 1e-6, "{}", out[50]);

    // A late plugin before the fork delays what it sends too, and the
    // rest waits for it.
    let late = f.add(kick, "clap:late");
    f.set(dawkey_core::MovePlugin {
        plugin: late,
        index: 0,
    });
    let graph = f.graph();
    assert_eq!(graph.latency(), 64);
    let out = &f.render(100)[0];
    assert!((out[50] - mix).abs() < 1e-6, "{}", out[50]);
}

#[test]
fn a_late_sidechain_holds_its_plugin_back() {
    let mut f = Fixture::new();
    let impulse = |f: &mut Fixture| {
        let mut samples = vec![0.0; 1000];
        samples[100] = 1.0;
        let id = f.session.add_source("i", "/dev/null", 1, 1000);
        f.media.insert(id, Arc::new(AudioData::new(vec![samples])));
        f.session
            .execute(AddClip::new(id, None, 0.0).into())
            .unwrap()
            .track
            .unwrap()
    };
    // The kick goes through a plugin with latency before it feeds the
    // bass's sidechain: the bass waits for it.
    let kick = impulse(&mut f);
    let bass = impulse(&mut f);
    f.add(kick, "clap:late");
    let minus = f.add(bass, "clap:minus");
    f.set(dawkey_core::SetPluginSidechain {
        plugin: minus,
        source: Some(kick),
    });
    let graph = f.graph();
    assert_eq!(graph.latency(), 64);
    let out = &render_offline(graph, RATE, 0, 400)[0];
    let hits: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, s)| s.abs() > 1e-6)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(hits, [100], "{hits:?}");
    // Renders come out aligned. The kick, and the bass less the kick (after its fader and pan).
    let mix = CENTER + (1.0 - CENTER) * CENTER;
    assert!((out[100] - mix).abs() < 1e-6, "{}", out[100]);

    // Bypassed, it doesn't wait: both still line up at the output.
    f.set(SetPluginBypass {
        plugin: minus,
        bypass: true,
    });
    let out = &render_offline(f.fresh_graph(), RATE, 0, 400)[0];
    assert!((out[100] - 2.0 * CENTER).abs() < 1e-6, "{}", out[100]);
}

#[test]
fn instrument_plugins_play_the_notes() {
    let mut f = Fixture::new();
    let track = f
        .session
        .execute(
            AddMidiClip {
                notes: vec![NoteSpec {
                    pitch: 60,
                    start_beats: 0.5,
                    length_beats: 0.25,
                    velocity: 127,
                }],
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap();
    f.add(track, "clap:keys");
    let out = &render_offline(f.graph(), RATE, 0, 24_000)[0];
    // A beat is 24 000 frames: the note covers 12 000..18 000, at full
    // velocity, through a balance (not equal-power) pan.
    assert!(out[..12_000].iter().all(|&s| s == 0.0));
    assert!(out[12_000..18_000].iter().all(|&s| (s - 1.0).abs() < 1e-6));
    assert!(out[18_000..].iter().all(|&s| s == 0.0));
}

/// A MIDI clip at 0 s with a full-velocity note over `beats`, played by
/// the Keys instrument.
fn keys_clip(f: &mut Fixture, beats: (f64, f64), controls: Vec<ControlSpec>) -> TrackId {
    let track = f
        .session
        .execute(
            AddMidiClip {
                notes: vec![NoteSpec {
                    pitch: 60,
                    start_beats: beats.0,
                    length_beats: beats.1 - beats.0,
                    velocity: 127,
                }],
                controls,
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap();
    f.add(track, "clap:keys");
    track
}

#[test]
fn notes_under_the_playhead_are_chased() {
    let mut f = Fixture::new();
    keys_clip(&mut f, (0.0, 2.0), vec![]);
    // Starting in the middle of the note (frames 0..48 000) plays it.
    let out = &render_offline(f.fresh_graph(), RATE, 24_000, 30_000)[0];
    assert!(out[..24_000].iter().all(|&s| (s - 1.0).abs() < 1e-6));
    assert!(out[24_000..].iter().all(|&s| s == 0.0));

    // Looping from inside the note back to inside it keeps it playing.
    f.session
        .execute(
            SetLoop {
                start_s: Some(0.25),
                end_s: Some(0.75),
                enabled: Some(true),
            }
            .into(),
        )
        .unwrap();
    let (mut ctl, mut proc) = new_engine(RATE, f.fresh_graph());
    ctl.locate(30_000).unwrap();
    ctl.play().unwrap();
    let out = process(&mut proc, 12_000);
    assert!(out.iter().all(|&s| (s - 1.0).abs() < 1e-6));
}

#[test]
fn clip_controllers_reach_plugins_and_are_chased() {
    let mut f = Fixture::new();
    // Full bend up at beat 1 (frame 24 000).
    let bend = ControlSpec {
        controller: Controller::PitchBend,
        time_beats: 1.0,
        value: 16_383,
    };
    keys_clip(&mut f, (0.0, 4.0), vec![bend]);
    let up = 16_383.0 / 8192.0;
    let out = &render_offline(f.fresh_graph(), RATE, 0, 48_000)[0];
    assert!((out[23_999] - 1.0).abs() < 1e-6);
    assert!((out[24_000] - up).abs() < 1e-6);
    // From after the change, the bend is caught up on at once; from
    // before it, it starts at no bend.
    let late = &render_offline(f.fresh_graph(), RATE, 36_000, 100)[0];
    assert!((late[0] - up).abs() < 1e-6);
    let early = &render_offline(f.fresh_graph(), RATE, 12_000, 13_000)[0];
    assert!((early[0] - 1.0).abs() < 1e-6);
    assert!((early[12_000] - up).abs() < 1e-6);
}

#[test]
fn device_midi_waits_for_delayed_audio() {
    let mut f = Fixture::new();
    // A 64-frame plugin on an audio track delays every track by 64.
    let audio = f.constant(1.0, 48_000);
    let late = f.add(audio, "clap:late");
    // A note at 0 sent to a device.
    let track = f
        .session
        .execute(
            AddMidiClip {
                notes: vec![NoteSpec {
                    pitch: 60,
                    start_beats: 0.0,
                    length_beats: 0.25,
                    velocity: 100,
                }],
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap();
    f.session
        .execute(
            dawkey_core::SetTrackMidiOutput {
                track,
                device: Some("Synth".into()),
                channel: None,
                latency_ms: None,
            }
            .into(),
        )
        .unwrap();
    let (mut ctl, mut proc) = new_engine(RATE, f.graph());
    ctl.play().unwrap();
    let mut sent = Vec::new();
    let run = |proc: &mut crate::EngineProcessor, sent: &mut Vec<_>, from: u32, blocks: u32| {
        for block in from..from + blocks {
            process(proc, 48);
            sent.extend(
                proc.midi_output()
                    .iter()
                    .map(|m| (block * 48 + m.offset, m.data)),
            );
            proc.clear_midi_output();
        }
    };
    run(&mut proc, &mut sent, 0, 4);
    // The note-on comes out 64 frames late, in the second block.
    assert_eq!(sent, [(64, [0x90, 60, 100])]);

    // Bypassing the plugin removes the delay; the held-back note-off still
    // follows the note-on, now on time (6000 frames into the clip).
    f.session
        .execute(
            SetPluginBypass {
                plugin: late,
                bypass: true,
            }
            .into(),
        )
        .unwrap();
    ctl.set_graph(f.graph()).unwrap();
    run(&mut proc, &mut sent, 4, 200);
    assert_eq!(sent, [(64, [0x90, 60, 100]), (6000, [0x80, 60, 0])]);
}

/// A one-note clip (pitch 60, 12 000..18 000) on a new MIDI track.
fn note_clip(f: &mut Fixture) -> TrackId {
    f.session
        .execute(
            AddMidiClip {
                notes: vec![NoteSpec {
                    pitch: 60,
                    start_beats: 0.5,
                    length_beats: 0.25,
                    velocity: 127,
                }],
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap()
}

#[test]
fn midi_effects_feed_the_next_plugin() {
    let mut f = Fixture::new();
    let track = note_clip(&mut f);
    f.add(track, "clap:octave");
    f.add(track, "clap:keys");
    let out = &render_offline(f.graph(), RATE, 0, 24_000)[0];
    assert!(out[..12_000].iter().all(|&s| s == 0.0));
    assert!(out[12_000..18_000].iter().all(|&s| (s - 1.0).abs() < 1e-6));
    assert!(out[18_000..].iter().all(|&s| s == 0.0));

    // Alone, the effect makes no sound, and its MIDI goes nowhere.
    let mut f = Fixture::new();
    let track = note_clip(&mut f);
    f.add(track, "clap:octave");
    let out = &render_offline(f.graph(), RATE, 0, 24_000)[0];
    assert!(out.iter().all(|&s| s == 0.0));
}

#[test]
fn midi_effects_play_a_device() {
    let mut f = Fixture::new();
    let track = note_clip(&mut f);
    let octave = f.add(track, "clap:octave");
    f.session
        .execute(
            dawkey_core::SetTrackMidiOutput {
                track,
                device: Some("Synth".into()),
                channel: Some(2),
                latency_ms: None,
            }
            .into(),
        )
        .unwrap();
    let (mut ctl, mut proc) = new_engine(RATE, f.graph());
    ctl.play().unwrap();
    let mut sent = Vec::new();
    for block in 0..40 {
        process(&mut proc, 512);
        sent.extend(
            proc.midi_output()
                .iter()
                .map(|m| (block * 512 + m.offset, m.data)),
        );
        proc.clear_midi_output();
    }
    assert_eq!(sent, [(12_000, [0x91, 72, 127]), (18_000, [0x81, 72, 0])]);

    // A note the chain left on the device is let go when stopping.
    ctl.locate(15_000).unwrap();
    process(&mut proc, 512);
    proc.clear_midi_output();
    ctl.stop().unwrap();
    process(&mut proc, 512);
    let stopped: Vec<[u8; 3]> = proc.midi_output().iter().map(|m| m.data).collect();
    proc.clear_midi_output();
    assert_eq!(stopped, [[0x81, 72, 0]]);

    // Bypassed, the notes go straight to the device again.
    f.session
        .execute(
            SetPluginBypass {
                plugin: octave,
                bypass: true,
            }
            .into(),
        )
        .unwrap();
    ctl.set_graph(f.graph()).unwrap();
    ctl.locate(12_000).unwrap();
    ctl.play().unwrap();
    process(&mut proc, 512);
    let direct: Vec<[u8; 3]> = proc.midi_output().iter().map(|m| m.data).collect();
    assert_eq!(direct, [[0x91, 60, 127]]);
}

/// Plays `frames` from the start twice, with fresh processors: straight
/// through, and with the same project recompiled and swapped in half way
/// (as any edit during playback does). Returns the frames that differ.
fn swap_differences(f: &mut Fixture, frames: usize) -> Vec<(usize, f32, f32)> {
    let (mut ctl, mut proc) = new_engine(RATE, f.fresh_graph());
    ctl.play().unwrap();
    let whole = process(&mut proc, frames);
    let (mut ctl, mut proc) = new_engine(RATE, f.fresh_graph());
    ctl.play().unwrap();
    let mut split = process(&mut proc, frames / 2);
    ctl.set_graph(f.graph()).unwrap();
    split.extend(process(&mut proc, frames - frames / 2));
    (0..frames)
        .filter(|&i| whole[i] != split[i])
        .map(|i| (i, whole[i], split[i]))
        .collect()
}

#[test]
fn delay_lines_survive_graph_swaps() {
    // A sends to a bus that also gets C through a 64-frame plugin, so A's
    // send is delayed 64 frames to line up.
    let mut f = Fixture::new();
    let a = f.constant(1.0, 48_000);
    let c = f.constant(0.0, 48_000);
    f.add(c, "clap:late");
    let bus = f.bus();
    f.send(c, Some(bus));
    f.set(dawkey_core::SetTrackSend {
        track: a,
        bus,
        gain_db: Some(0.0),
        pre_fader: None,
    });
    let differs = swap_differences(&mut f, 2048);
    assert!(differs.is_empty(), "send: {:?}", &differs[..1]);

    // A fork's other output to a bus, delayed to line up with a late
    // track going there too.
    let mut f = Fixture::new();
    let a = f.constant(0.5, 48_000);
    let c = f.constant(0.0, 48_000);
    let fork = f.add(a, "clap:fork");
    f.add(c, "clap:late");
    let bus = f.bus();
    f.send(c, Some(bus));
    f.set(dawkey_core::SetPluginOutput {
        plugin: fork,
        output: 0,
        bus: Some(bus),
    });
    let differs = swap_differences(&mut f, 2048);
    assert!(differs.is_empty(), "other output: {:?}", &differs[..1]);

    // A sidechain from a track that's earlier than the plugin it feeds,
    // which comes after a late one.
    let mut f = Fixture::new();
    let kick = f.constant(0.25, 48_000);
    let bass = f.constant(1.0, 48_000);
    f.add(bass, "clap:late");
    let minus = f.add(bass, "clap:minus");
    f.set(dawkey_core::SetPluginSidechain {
        plugin: minus,
        source: Some(kick),
    });
    let differs = swap_differences(&mut f, 2048);
    assert!(differs.is_empty(), "sidechain: {:?}", &differs[..1]);
}
