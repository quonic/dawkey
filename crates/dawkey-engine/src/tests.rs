// SPDX-License-Identifier: AGPL-3.0-or-later
//! Engine tests. The crate's test build installs `AllocDisabler`, so any
//! allocation inside `EngineProcessor::process` aborts the test binary.

use std::f32::consts::FRAC_1_SQRT_2;
use std::sync::Arc;

use dawkey_core::{
    AddClip, AddMidiClip, ClipId, ControlSpec, Controller, InputChannels, NoteSpec,
    RemoveMidiNotes, RemoveTrack, Session, SetLoop, SetTrackArm, SetTrackGain, SetTrackMidiInput,
    SetTrackMidiOutput, SetTrackMute, SetTrackPan, SetTrackSolo, SourceId, TimeRange, TrackId,
};
use dawkey_media::AudioData;

use crate::{
    Graph, MAX_BLOCK, Mark, MediaPool, MidiEvent, MidiOut, MidiRecord, device_tag, new_engine,
    record_session, render_offline,
};

const RATE: u32 = 48_000;

struct Fixture {
    session: Session,
    media: MediaPool,
}

impl Fixture {
    fn new() -> Self {
        Self {
            session: Session::default(),
            media: MediaPool::new(),
        }
    }

    fn source(&mut self, channels: Vec<Vec<f32>>) -> SourceId {
        let data = AudioData::new(channels);
        let id = self.session.add_source(
            "src",
            "/dev/null",
            data.channel_count() as u16,
            data.frames() as u64,
        );
        self.media.insert(id, Arc::new(data));
        id
    }

    /// Places `source` at `at_s` seconds on a new track.
    fn place(&mut self, source: SourceId, at_s: f64) -> TrackId {
        self.session
            .execute(AddClip::new(source, None, at_s).into())
            .unwrap()
            .track
            .unwrap()
    }

    fn graph(&self) -> Graph {
        Graph::compile(self.session.project(), &self.media)
    }

    fn render(&self, start: u64, frames: usize) -> Vec<Vec<f32>> {
        render_offline(self.graph(), RATE, start, frames)
    }

    /// A MIDI clip at `at_s` with these (pitch, start beat, length in
    /// beats) notes, on a new track.
    fn midi(&mut self, at_s: f64, notes: &[(u8, f64, f64)]) -> (TrackId, ClipId) {
        let outcome = self
            .session
            .execute(
                AddMidiClip {
                    position_s: at_s,
                    notes: notes
                        .iter()
                        .map(|&(pitch, start_beats, length_beats)| NoteSpec {
                            pitch,
                            start_beats,
                            length_beats,
                            velocity: 100,
                        })
                        .collect(),
                    ..Default::default()
                }
                .into(),
            )
            .unwrap();
        (outcome.track.unwrap(), outcome.clip.unwrap())
    }
}

/// Index of the first sample that isn't silent.
fn first_sound(samples: &[f32]) -> Option<usize> {
    samples.iter().position(|&s| s != 0.0)
}

/// Where a note started, given its output: oscillators start at a zero
/// crossing, so a note's first sample is silent and the next one isn't.
fn note_start(samples: &[f32]) -> Option<usize> {
    first_sound(samples).map(|i| i - 1)
}

/// Index just past the last sample that isn't silent.
fn last_sound(samples: &[f32]) -> Option<usize> {
    samples.iter().rposition(|&s| s != 0.0).map(|i| i + 1)
}

fn process(proc: &mut crate::EngineProcessor, frames: usize, midi: &[MidiEvent]) -> Vec<f32> {
    let mut l = vec![0.0; frames];
    let mut r = vec![0.0; frames];
    proc.process_with_input([&[], &[]], midi, &mut l, &mut r);
    l
}

fn ones(frames: usize) -> Vec<f32> {
    vec![1.0; frames]
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn mono_clip_is_panned_with_equal_power() {
    let mut f = Fixture::new();
    let src = f.source(vec![ones(100)]);
    let track = f.place(src, 0.0);

    let out = f.render(0, 100);
    assert!(out[0].iter().all(|&s| close(s, FRAC_1_SQRT_2)));
    assert!(out[1].iter().all(|&s| close(s, FRAC_1_SQRT_2)));

    f.session
        .execute(SetTrackPan { track, pan: -1.0 }.into())
        .unwrap();
    let out = f.render(0, 100);
    assert!(out[0].iter().all(|&s| close(s, 1.0)));
    assert!(out[1].iter().all(|&s| close(s, 0.0)));
}

#[test]
fn stereo_clip_keeps_channels_and_applies_gain() {
    let mut f = Fixture::new();
    let src = f.source(vec![ones(10), vec![-0.5; 10]]);
    let track = f.place(src, 0.0);
    f.session
        .execute(
            SetTrackGain {
                track,
                gain_db: -6.0,
            }
            .into(),
        )
        .unwrap();

    let out = f.render(0, 10);
    let g = 10f32.powf(-6.0 / 20.0);
    assert!(out[0].iter().all(|&s| close(s, g)));
    assert!(out[1].iter().all(|&s| close(s, -0.5 * g)));
}

#[test]
fn clips_start_on_the_exact_frame() {
    let mut f = Fixture::new();
    let mut impulse = vec![0.0; 10];
    impulse[0] = 1.0;
    let src = f.source(vec![impulse.clone(), impulse]);
    // 0.5 s = frame 24_000, which falls inside a block, not on a boundary.
    f.place(src, 0.5);

    let out = f.render(20_000, 8_000);
    let hits: Vec<usize> = (0..out[0].len()).filter(|&i| out[0][i] != 0.0).collect();
    assert_eq!(hits, [4_000]);
}

#[test]
fn rendering_is_independent_of_block_size() {
    let mut f = Fixture::new();
    let noise: Vec<f32> = (0..20_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 8) as f32 / 16_777_216.0 - 0.5)
        .collect();
    let src = f.source(vec![noise]);
    f.place(src, 0.01);
    f.place(src, 0.2);

    let whole = f.render(0, 30_000);
    let (mut ctl, mut proc) = new_engine(RATE, f.graph());
    ctl.play().unwrap();
    let mut left = vec![0.0; 30_000];
    let mut right = vec![0.0; 30_000];
    let mut at = 0;
    for size in [1, 7, 128, 1000, MAX_BLOCK + 3].iter().cycle() {
        if at == left.len() {
            break;
        }
        let end = (at + size).min(left.len());
        proc.process(&mut left[at..end], &mut right[at..end]);
        at = end;
    }
    assert_eq!(whole, [left, right]);
}

#[test]
fn mute_and_solo() {
    let mut f = Fixture::new();
    let src = f.source(vec![ones(10)]);
    let a = f.place(src, 0.0);
    let b = f.place(src, 0.0);

    let both = f.render(0, 10)[0][0];
    assert!(close(both, 2.0 * FRAC_1_SQRT_2));

    f.session
        .execute(
            SetTrackMute {
                track: a,
                mute: true,
            }
            .into(),
        )
        .unwrap();
    assert!(close(f.render(0, 10)[0][0], FRAC_1_SQRT_2));

    // Solo wins over the other tracks, but mute still wins over solo.
    f.session
        .execute(
            SetTrackSolo {
                track: a,
                solo: true,
            }
            .into(),
        )
        .unwrap();
    assert!(close(f.render(0, 10)[0][0], 0.0));
    f.session
        .execute(
            SetTrackMute {
                track: a,
                mute: false,
            }
            .into(),
        )
        .unwrap();
    assert!(close(f.render(0, 10)[0][0], FRAC_1_SQRT_2));
    let _ = b;
}

#[test]
fn transport_meters_and_live_graph_swap() {
    let mut f = Fixture::new();
    let src = f.source(vec![vec![0.5; 1000]]);
    let track = f.place(src, 0.0);

    let (mut ctl, mut proc) = new_engine(RATE, Graph::empty());
    let mut l = vec![0.0; 256];
    let mut r = vec![0.0; 256];

    // Stopped: silence, no movement.
    proc.process(&mut l, &mut r);
    assert_eq!(ctl.transport().position, 0);
    assert!(!ctl.transport().playing);

    ctl.set_graph(f.graph()).unwrap();
    ctl.play().unwrap();
    proc.process(&mut l, &mut r);
    let status = ctl.transport();
    assert!(status.playing);
    assert_eq!(status.position, 256);
    assert!(close(l[0], 0.5 * FRAC_1_SQRT_2));

    let meters = ctl.take_meters();
    let expected = 20.0 * (0.5 * std::f64::consts::FRAC_1_SQRT_2).log10();
    assert!((meters.master[0].unwrap() - expected).abs() < 1e-4);
    assert_eq!(meters.tracks[0].track, track);
    assert!((meters.tracks[0].peak_dbfs[1].unwrap() - expected).abs() < 1e-4);
    // Reading resets the peaks.
    assert_eq!(ctl.take_meters().master, [None, None]);

    // Swap graphs many times while playing; old graphs come back to be freed.
    for _ in 0..500 {
        ctl.set_graph(f.graph()).unwrap();
        proc.process(&mut l, &mut r);
    }
    ctl.locate(48_000).unwrap();
    ctl.stop().unwrap();
    proc.process(&mut l, &mut r);
    let status = ctl.transport();
    assert_eq!((status.playing, status.position), (false, 48_000));
}

#[test]
fn reports_busy_when_the_audio_side_stalls() {
    let (mut ctl, _proc) = new_engine(RATE, Graph::empty());
    let sent = (0..1000).take_while(|_| ctl.play().is_ok()).count();
    assert!(sent > 0 && sent < 1000);
}

#[test]
fn counts_graphs_in_flight() {
    let (mut ctl, mut proc) = new_engine(RATE, Graph::empty());
    ctl.set_graph(Graph::empty()).unwrap();
    ctl.set_graph(Graph::empty()).unwrap();
    assert_eq!(ctl.graphs_in_flight(), 2);
    proc.process(&mut [], &mut []);
    assert_eq!(ctl.graphs_in_flight(), 0);
    // Empty calls apply messages but don't count as audio cycles.
    assert_eq!(ctl.transport().cycles, 0);
}

/// A ramp whose sample i is i, so positions can be read back from values.
fn ramp(frames: usize, from: usize) -> Vec<f32> {
    (from..from + frames).map(|i| i as f32).collect()
}

#[test]
fn loops_wrap_sample_accurately() {
    let mut f = Fixture::new();
    // A stereo "ramp" clip whose value at timeline frame t is t.
    let src = f.source(vec![ramp(48_000, 0), ramp(48_000, 0)]);
    let track = f.place(src, 0.0);
    f.session
        .execute(SetTrackPan { track, pan: 0.0 }.into())
        .unwrap();
    // Loop 0.25 s .. 0.5 s = frames 12_000 .. 24_000.
    f.session
        .execute(
            SetLoop {
                start_s: Some(0.25),
                end_s: Some(0.5),
                enabled: Some(true),
            }
            .into(),
        )
        .unwrap();
    let (mut ctl, mut proc) = new_engine(RATE, f.graph());
    ctl.locate(23_000).unwrap();
    ctl.play().unwrap();
    let mut l = vec![0.0; 3_000];
    let mut r = vec![0.0; 3_000];
    proc.process(&mut l, &mut r);
    // 1000 frames up to the loop end, then back to 12_000.
    assert_eq!(l[999], 23_999.0);
    assert_eq!(l[1000], 12_000.0);
    assert_eq!(l[2999], 13_999.0);
    assert_eq!(ctl.transport().position, 14_000);

    // Starting after the loop plays straight on.
    ctl.locate(30_000).unwrap();
    proc.process(&mut l, &mut r);
    assert_eq!(l[0], 30_000.0);
    assert_eq!(ctl.transport().position, 33_000);
}

#[test]
fn records_input_with_positions_through_loops_and_punch() {
    let (mut ctl, mut proc) = new_engine(RATE, Graph::empty());
    let punch = TimeRange {
        start: 100,
        end: 1_000_000,
    };
    let (session, taps) = record_session(
        &[InputChannels::Stereo, InputChannels::Right],
        false,
        Some(punch),
        10_000,
    );
    let (mut taps, shared) = (taps.audio, taps.shared);
    ctl.start_recording(session).unwrap();
    ctl.play().unwrap();

    let input_l = ramp(256, 0);
    let input_r: Vec<f32> = input_l.iter().map(|v| -v).collect();
    let mut l = vec![0.0; 256];
    let mut r = vec![0.0; 256];
    proc.process_with_input([&input_l, &input_r], &[], &mut l, &mut r);
    assert!(ctl.transport().recording);
    ctl.stop_recording().unwrap();
    proc.process(&mut [], &mut []);
    assert!(shared.finished());
    assert!(!ctl.transport().recording);

    // Punch-in at frame 100: the first 100 frames are skipped.
    let stereo = &mut taps[0];
    assert_eq!(
        stereo.marks.pop().unwrap(),
        Mark {
            start: 100,
            frames: 156
        }
    );
    assert_eq!(stereo.samples.pop().unwrap(), 100.0);
    assert_eq!(stereo.samples.pop().unwrap(), -100.0);
    // The rest of the 156 stereo frames are waiting to be read.
    assert_eq!(stereo.samples.slots(), 2 * 156 - 2);
    let mono = &mut taps[1];
    assert_eq!(mono.marks.pop().unwrap().frames, 156);
    assert_eq!(mono.samples.pop().unwrap(), -100.0);
    assert_eq!(shared.dropped_frames(), 0);
}

#[test]
fn recording_marks_restart_at_each_loop_pass() {
    let mut f = Fixture::new();
    f.session
        .execute(
            SetLoop {
                start_s: Some(0.0),
                end_s: Some(0.01),
                enabled: Some(true),
            }
            .into(),
        )
        .unwrap();
    let (mut ctl, mut proc) = new_engine(RATE, f.graph());
    let (session, taps) = record_session(&[InputChannels::Left], false, None, 10_000);
    let mut taps = taps.audio;
    ctl.start_recording(session).unwrap();
    ctl.play().unwrap();
    // 480-frame loop, 1200 frames played: passes of 480, 480 and 240.
    let mut l = vec![0.0; 1200];
    let mut r = vec![0.0; 1200];
    proc.process_with_input([&ramp(1200, 0), &[]], &[], &mut l, &mut r);
    let marks: Vec<Mark> = std::iter::from_fn(|| taps[0].marks.pop().ok()).collect();
    assert_eq!(
        marks,
        [
            Mark {
                start: 0,
                frames: 480
            },
            Mark {
                start: 0,
                frames: 480
            },
            Mark {
                start: 0,
                frames: 240
            },
        ]
    );
}

#[test]
fn a_slow_disk_drops_frames_instead_of_blocking() {
    let (mut ctl, mut proc) = new_engine(RATE, Graph::empty());
    let (session, taps) = record_session(&[InputChannels::Stereo], false, None, 100);
    let shared = taps.shared;
    ctl.start_recording(session).unwrap();
    ctl.play().unwrap();
    let mut l = vec![0.0; 256];
    let mut r = vec![0.0; 256];
    proc.process_with_input([&ramp(256, 0), &ramp(256, 0)], &[], &mut l, &mut r);
    assert_eq!(shared.dropped_frames(), 256);
}

#[test]
fn midi_notes_start_on_their_frame() {
    let mut f = Fixture::new();
    // At 120 BPM a beat is 24 000 frames: the note starts 12 000 frames
    // into a clip at 0.5 s (24 000), so at 36 000.
    f.midi(0.5, &[(60, 0.5, 0.25)]);
    let out = f.render(0, 60_000);
    assert_eq!(note_start(&out[0]), Some(36_000));
    assert_eq!(note_start(&out[1]), Some(36_000));
    // A quarter of a beat (6 000 frames), then the release: 200 ms
    // (9 600 frames) per 60 dB, down to -80 dB from the sustain level.
    let end = last_sound(&out[0]).unwrap();
    assert!((42_000 + 9_600..42_000 + 12_800).contains(&end), "{end}");

    // Rendering from the middle of a note plays it from there.
    let late = f.render(37_000, 1000);
    assert_eq!(note_start(&late[0]), Some(0));
    // After it, nothing starts.
    let after = f.render(43_000, 1000);
    assert!(after[0].iter().all(|&s| s == 0.0));
}

#[test]
fn replacing_the_graph_keeps_notes_sounding() {
    let mut f = Fixture::new();
    f.midi(0.0, &[(57, 0.0, 1.0), (64, 0.0, 1.0)]);
    let (mut ctl, mut a) = new_engine(RATE, f.graph());
    let (mut ctl_b, mut b) = new_engine(RATE, f.graph());
    for c in [&mut ctl, &mut ctl_b] {
        c.play().unwrap();
    }
    let whole = process(&mut a, 8000, &[]);
    let mut split = process(&mut b, 3000, &[]);
    ctl_b.set_graph(f.graph()).unwrap();
    split.extend(process(&mut b, 5000, &[]));
    assert_eq!(whole, split);
}

#[test]
fn removing_a_sounding_note_releases_it() {
    let mut f = Fixture::new();
    let (_, clip) = f.midi(0.0, &[(60, 0.0, 4.0)]);
    let (mut ctl, mut proc) = new_engine(RATE, f.graph());
    ctl.play().unwrap();
    process(&mut proc, 4800, &[]);
    let note = f.session.project().clip(clip).unwrap().1.notes()[0].id;
    f.session
        .execute(
            RemoveMidiNotes {
                clip,
                notes: vec![note],
            }
            .into(),
        )
        .unwrap();
    ctl.set_graph(f.graph()).unwrap();
    // The 200 ms release fades out well within half a second.
    let out = process(&mut proc, 24_000, &[]);
    assert!(out[0] != 0.0);
    assert!(out[20_000..].iter().all(|&s| s == 0.0));
}

#[test]
fn armed_midi_tracks_play_live_input_while_stopped() {
    let mut f = Fixture::new();
    let (track, _) = f.midi(0.0, &[]);
    let (mut ctl, mut proc) = new_engine(RATE, f.graph());
    let on = [MidiEvent::note_on(100, 0, 60, 100)];
    // Not armed: nothing.
    assert!(process(&mut proc, 512, &on).iter().all(|&s| s == 0.0));

    f.session
        .execute(SetTrackArm { track, armed: true }.into())
        .unwrap();
    ctl.set_graph(f.graph()).unwrap();
    let out = process(&mut proc, 512, &on);
    assert_eq!(note_start(&out), Some(100));
    assert_eq!(ctl.transport().position, 0, "the transport didn't move");

    // Note off, then the release tail runs out.
    process(&mut proc, 512, &[MidiEvent::note_off(0, 0, 60)]);
    let tail = process(&mut proc, 24_000, &[]);
    assert!(tail[20_000..].iter().all(|&s| s == 0.0));

    // Disarming stops a held live note.
    process(&mut proc, 64, &on);
    f.session
        .execute(
            SetTrackArm {
                track,
                armed: false,
            }
            .into(),
        )
        .unwrap();
    ctl.set_graph(f.graph()).unwrap();
    let tail = process(&mut proc, 24_000, &[]);
    assert!(tail[20_000..].iter().all(|&s| s == 0.0));
}

#[test]
fn armed_audio_tracks_meter_and_monitor_their_input() {
    let mut f = Fixture::new();
    let track = f
        .session
        .execute(dawkey_core::AddTrack::default().into())
        .unwrap()
        .track
        .unwrap();
    let arm = |f: &mut Fixture, armed: bool, monitor: bool, input: InputChannels| {
        let commands: [dawkey_core::Command; 3] = [
            SetTrackArm { track, armed }.into(),
            dawkey_core::SetTrackMonitor { track, monitor }.into(),
            dawkey_core::SetTrackInput { track, input }.into(),
        ];
        for command in commands {
            f.session.execute(command).unwrap();
        }
    };
    let (mut ctl, mut proc) = new_engine(RATE, f.graph());
    let (in_l, in_r) = (vec![0.5; 256], vec![-0.25; 256]);
    let run = |proc: &mut crate::EngineProcessor| {
        let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
        proc.process_with_input([&in_l, &in_r], &[], &mut l, &mut r);
        (l, r)
    };
    let db = |v: f64| Some(20.0 * v.log10());
    let near = |a: Option<f64>, b: Option<f64>| match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() < 1e-4,
        (a, b) => a == b,
    };

    // Not armed: the input goes nowhere.
    let (l, _) = run(&mut proc);
    assert!(l.iter().all(|&s| s == 0.0));
    assert_eq!(ctl.take_meters().tracks[0].peak_dbfs, [None, None]);

    // Armed: the meter shows the input, but the track stays silent.
    arm(&mut f, true, false, InputChannels::Stereo);
    ctl.set_graph(f.graph()).unwrap();
    let (l, _) = run(&mut proc);
    assert!(l.iter().all(|&s| s == 0.0));
    let meter = ctl.take_meters().tracks[0].peak_dbfs;
    assert!(
        near(meter[0], db(0.5)) && near(meter[1], db(0.25)),
        "{meter:?}"
    );

    // Monitoring, stopped: the input plays through the fader (stereo:
    // balance, unity at center).
    arm(&mut f, true, true, InputChannels::Stereo);
    f.session
        .execute(
            SetTrackGain {
                track,
                gain_db: -6.0,
            }
            .into(),
        )
        .unwrap();
    ctl.set_graph(f.graph()).unwrap();
    let (l, r) = run(&mut proc);
    let gain = 10f32.powf(-6.0 / 20.0);
    assert!(close(l[0], 0.5 * gain) && close(r[255], -0.25 * gain));
    assert_eq!(ctl.transport().position, 0);

    // The left input alone is mono: both sides, panned with equal power.
    arm(&mut f, true, true, InputChannels::Left);
    ctl.set_graph(f.graph()).unwrap();
    let (l, r) = run(&mut proc);
    assert!(close(l[7], 0.5 * gain * FRAC_1_SQRT_2) && close(r[7], l[7]));

    // Muted, it's silent; disarmed, monitoring stops.
    f.session
        .execute(SetTrackMute { track, mute: true }.into())
        .unwrap();
    ctl.set_graph(f.graph()).unwrap();
    assert!(run(&mut proc).0.iter().all(|&s| s == 0.0));
    arm(&mut f, false, true, InputChannels::Left);
    f.session
        .execute(SetTrackMute { track, mute: false }.into())
        .unwrap();
    ctl.set_graph(f.graph()).unwrap();
    assert!(run(&mut proc).0.iter().all(|&s| s == 0.0));
}

#[test]
fn stopping_releases_clip_notes() {
    let mut f = Fixture::new();
    f.midi(0.0, &[(60, 0.0, 8.0)]);
    let (mut ctl, mut proc) = new_engine(RATE, f.graph());
    ctl.play().unwrap();
    process(&mut proc, 4800, &[]);
    ctl.stop().unwrap();
    // The release tail plays while stopped, then silence.
    let out = process(&mut proc, 24_000, &[]);
    assert!(out[0] != 0.0);
    assert!(out[20_000..].iter().all(|&s| s == 0.0));
}

#[test]
fn records_midi_with_positions_and_loop_wraps() {
    let mut f = Fixture::new();
    f.session
        .execute(
            SetLoop {
                start_s: Some(0.0),
                end_s: Some(0.01),
                enabled: Some(true),
            }
            .into(),
        )
        .unwrap();
    let (mut ctl, mut proc) = new_engine(RATE, f.graph());
    let (session, mut taps) = record_session(&[], true, None, 10_000);
    ctl.start_recording(session).unwrap();
    ctl.locate(100).unwrap();
    ctl.play().unwrap();
    // The loop is 480 frames: an event 400 frames in lands after the wrap.
    process(
        &mut proc,
        600,
        &[
            MidiEvent::note_on(10, 0, 60, 90),
            MidiEvent::note_off(400, 0, 60),
        ],
    );
    let midi = taps.midi.as_mut().unwrap();
    let recorded: Vec<MidiRecord> = std::iter::from_fn(|| midi.pop().ok()).collect();
    assert_eq!(
        recorded,
        [
            MidiRecord::Event {
                position: 110,
                data: [0x90, 60, 90],
                source: 0,
                value: 0,
            },
            MidiRecord::Wrap,
            MidiRecord::Event {
                position: 20,
                data: [0x80, 60, 0],
                source: 0,
                value: 0,
            },
        ]
    );
    assert_eq!(taps.shared.dropped_midi(), 0);
}

#[test]
fn tracks_hear_only_their_midi_device() {
    let mut f = Fixture::new();
    let (track, _) = f.midi(0.0, &[]);
    f.session
        .execute(SetTrackArm { track, armed: true }.into())
        .unwrap();
    f.session
        .execute(
            SetTrackMidiInput {
                track,
                device: Some("Keys".into()),
                channel: None,
            }
            .into(),
        )
        .unwrap();
    let (_ctl, mut proc) = new_engine(RATE, f.graph());
    let other = [MidiEvent::note_on(10, 0, 60, 100).from_device(device_tag("Pads"))];
    assert!(process(&mut proc, 512, &other).iter().all(|&s| s == 0.0));
    let keys = [MidiEvent::note_on(10, 0, 60, 100).from_device(device_tag("Keys"))];
    assert_eq!(note_start(&process(&mut proc, 512, &keys)), Some(10));
}

#[test]
fn tracks_hear_only_their_midi_channel() {
    let mut f = Fixture::new();
    let (track, _) = f.midi(0.0, &[]);
    f.session
        .execute(SetTrackArm { track, armed: true }.into())
        .unwrap();
    f.session
        .execute(
            SetTrackMidiInput {
                track,
                device: None,
                channel: Some(3),
            }
            .into(),
        )
        .unwrap();
    let (_ctl, mut proc) = new_engine(RATE, f.graph());
    let other = [MidiEvent::note_on(10, 0, 60, 100)];
    assert!(process(&mut proc, 512, &other).iter().all(|&s| s == 0.0));
    let third = [MidiEvent::note_on(10, 2, 60, 100)];
    assert_eq!(note_start(&process(&mut proc, 512, &third)), Some(10));
}

#[test]
fn tracks_can_play_a_midi_device() {
    let mut f = Fixture::new();
    // A note from frame 12 000 to 24 000.
    let (track, _) = f.midi(0.0, &[(60, 0.5, 0.5)]);
    f.session
        .execute(
            SetTrackMidiOutput {
                track,
                device: Some("Synth".into()),
                channel: Some(3),
                latency_ms: None,
            }
            .into(),
        )
        .unwrap();
    let (mut ctl, mut proc) = new_engine(RATE, f.graph());
    ctl.play().unwrap();
    let dest = device_tag("Synth");
    let mut sent = Vec::new();
    for block in 0..60 {
        // The track makes no sound itself.
        assert!(process(&mut proc, 512, &[]).iter().all(|&s| s == 0.0));
        sent.extend(
            proc.midi_output()
                .iter()
                .map(|m| (block * 512 + m.offset, m.dest, m.data)),
        );
        proc.clear_midi_output();
    }
    assert_eq!(
        sent,
        [
            (12_000, dest, [0x92, 60, 100]),
            (24_000, dest, [0x82, 60, 0])
        ]
    );

    // A note held when the track goes away is let go.
    ctl.locate(12_000).unwrap();
    process(&mut proc, 512, &[]);
    proc.clear_midi_output();
    f.session.execute(RemoveTrack { track }.into()).unwrap();
    ctl.set_graph(f.graph()).unwrap();
    process(&mut proc, 512, &[]);
    assert_eq!(
        proc.midi_output(),
        [MidiOut {
            offset: 0,
            dest,
            data: [0x82, 60, 0],
            value: 0,
        }]
    );
}

#[test]
fn a_device_latency_is_compensated() {
    let mut f = Fixture::new();
    // Audio from frame 0, and a note at 12 000 on two devices: one that
    // sounds 10 ms (480 frames) after it gets MIDI, one at once.
    let src = f.source(vec![ones(48_000)]);
    f.place(src, 0.0);
    let send = |f: &mut Fixture, device: &str, latency_ms: Option<f64>| {
        let (track, _) = f.midi(0.0, &[(60, 0.5, 0.5)]);
        let mut set = SetTrackMidiOutput::new(track, Some(device.into()));
        set.latency_ms = latency_ms;
        f.session.execute(set.into()).unwrap();
    };
    send(&mut f, "Slow", Some(10.0));
    send(&mut f, "Fast", None);
    let graph = f.graph();
    assert_eq!(graph.latency(), 480);
    let (mut ctl, mut proc) = new_engine(RATE, graph);
    ctl.play().unwrap();
    let mut audio = Vec::new();
    let mut sent = Vec::new();
    for block in 0..30 {
        audio.extend(process(&mut proc, 512, &[]));
        sent.extend(
            proc.midi_output()
                .iter()
                .map(|m| (block * 512 + m.offset, m.dest, m.data[0])),
        );
        proc.clear_midi_output();
    }
    // The mix waits for the slow device; it gets its note on time, and the
    // other device 480 frames later, with the mix.
    assert_eq!(first_sound(&audio), Some(480));
    assert_eq!(
        sent,
        [
            (12_000, device_tag("Slow"), 0x90),
            (12_480, device_tag("Fast"), 0x90)
        ]
    );

    // What the audio system reports for a device adds to what's set: the
    // fast one takes 5 ms now, and the slow one 10 + 2 ms, the slowest.
    let reported = [("Fast".to_owned(), 5.0), ("Slow".to_owned(), 2.0)].into();
    let graph = Graph::compile_with_devices(
        f.session.project(),
        &f.media,
        &mut crate::NoPlugins,
        &reported,
    );
    assert_eq!(graph.latency(), 576);
}

#[test]
fn stopping_lifts_a_clip_pedal_on_a_device() {
    let mut f = Fixture::new();
    // Pedal down at 0, a note from 0 to 0.5 beats, pedal up at the clip's
    // very end (one bar, 96 000 frames).
    let pedal = |time_beats, value| ControlSpec {
        controller: Controller::Cc { number: 64 },
        time_beats,
        value,
    };
    let track = f
        .session
        .execute(
            AddMidiClip {
                notes: vec![NoteSpec {
                    pitch: 60,
                    start_beats: 0.0,
                    length_beats: 0.5,
                    velocity: 100,
                }],
                controls: vec![pedal(0.0, 127), pedal(4.0, 0)],
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap();
    f.session
        .execute(
            SetTrackMidiOutput {
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
    process(&mut proc, 512, &[]);
    proc.clear_midi_output();
    ctl.stop().unwrap();
    process(&mut proc, 512, &[]);
    let sent: Vec<[u8; 3]> = proc.midi_output().iter().map(|m| m.data).collect();
    assert_eq!(sent, [[0x80, 60, 0], [0xb0, 64, 0]]);

    // Played through, the pedal comes up at the clip's end.
    let (mut ctl, mut proc) = new_engine(RATE, f.graph());
    ctl.play().unwrap();
    let mut last = Vec::new();
    for _ in 0..200 {
        process(&mut proc, 512, &[]);
        last.extend(proc.midi_output().iter().map(|m| m.data));
        proc.clear_midi_output();
    }
    assert_eq!(last.last(), Some(&[0xb0, 64, 0]));
}

#[test]
fn programs_and_key_pressure_reach_a_device() {
    let mut f = Fixture::new();
    // Program 5 at beat 1 (24 000), pressure on key 60 at beat 2 (48 000).
    let track = f
        .session
        .execute(
            AddMidiClip {
                notes: vec![NoteSpec {
                    pitch: 60,
                    start_beats: 1.0,
                    length_beats: 2.0,
                    velocity: 100,
                }],
                controls: vec![
                    ControlSpec {
                        controller: Controller::Program,
                        time_beats: 1.0,
                        value: 5,
                    },
                    ControlSpec {
                        controller: Controller::PolyPressure { pitch: 60 },
                        time_beats: 2.0,
                        value: 90,
                    },
                ],
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap();
    f.session
        .execute(
            SetTrackMidiOutput {
                track,
                device: Some("Synth".into()),
                channel: Some(2),
                latency_ms: None,
            }
            .into(),
        )
        .unwrap();
    let (mut ctl, mut proc) = new_engine(RATE, f.graph());
    let run = |proc: &mut crate::EngineProcessor, blocks: u32| {
        let mut sent = Vec::new();
        for block in 0..blocks {
            process(proc, 512, &[]);
            sent.extend(
                proc.midi_output()
                    .iter()
                    .map(|m| (block * 512 + m.offset, m.data)),
            );
            proc.clear_midi_output();
        }
        sent
    };
    ctl.play().unwrap();
    // Before the first program change there is nothing to chase: only the
    // key pressure's default goes out.
    assert_eq!(
        run(&mut proc, 100),
        [
            (0, [0xa1, 60, 0]),
            (24_000, [0xc1, 5, 0]),
            (24_000, [0x91, 60, 100]),
            (48_000, [0xa1, 60, 90]),
        ]
    );
    // Stopping resets the pressure but leaves the program.
    ctl.stop().unwrap();
    let stopped: Vec<[u8; 3]> = run(&mut proc, 1).iter().map(|e| e.1).collect();
    assert!(stopped.contains(&[0xa1, 60, 0]));
    assert!(!stopped.iter().any(|d| d[0] == 0xc1));
    // Starting after the change chases the program.
    ctl.locate(30_000).unwrap();
    ctl.play().unwrap();
    let chased: Vec<[u8; 3]> = run(&mut proc, 1).iter().map(|e| e.1).collect();
    assert!(chased.contains(&[0xc1, 5, 0]));
}

#[test]
fn per_note_controllers_reach_a_device_as_midi2() {
    let mut f = Fixture::new();
    // Per-note bend on key 60 at beat 1 (24 000), its note from beat 1.
    let bend = Controller::NotePitchBend { pitch: 60 };
    let track = f
        .session
        .execute(
            AddMidiClip {
                notes: vec![NoteSpec {
                    pitch: 60,
                    start_beats: 1.0,
                    length_beats: 1.0,
                    velocity: 100,
                }],
                controls: vec![ControlSpec {
                    controller: bend,
                    time_beats: 1.0,
                    value: u16::MAX,
                }],
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap();
    let mut set = SetTrackMidiOutput::new(track, Some("Synth".into()));
    set.channel = Some(2);
    f.session.execute(set.into()).unwrap();
    let (mut ctl, mut proc) = new_engine(RATE, f.graph());
    ctl.play().unwrap();
    let mut sent = Vec::new();
    for block in 0..60 {
        process(&mut proc, 512, &[]);
        sent.extend(
            proc.midi_output()
                .iter()
                .map(|m| (block * 512 + m.offset, m.data, m.value)),
        );
        proc.clear_midi_output();
    }
    // The default (no bend) is chased at the start, then the change comes
    // before the note.
    assert_eq!(
        sent[..3],
        [
            (0, [0x61, 60, 0], 0x8000_0000),
            (24_000, [0x61, 60, 0], u32::MAX),
            (24_000, [0x91, 60, 100], 0),
        ]
    );
}

#[test]
fn per_note_volume_silences_one_key_of_the_synth() {
    let mut f = Fixture::new();
    // Two notes; key 60's per-note volume is 0.
    let (track, clip) = f.midi(0.0, &[(60, 0.0, 1.0)]);
    f.session
        .execute(
            dawkey_core::SetMidiControls {
                clip,
                controller: Controller::NoteController {
                    pitch: 60,
                    number: 7,
                },
                points: vec![dawkey_core::ControlValue {
                    time_beats: 0.0,
                    value: 0,
                }],
            }
            .into(),
        )
        .unwrap();
    let silent = f.render(0, 12_000);
    assert!(silent[0].iter().all(|&s| s == 0.0));

    // Live per-note input does the same to an armed track while stopped.
    let (live, _) = f.midi(10.0, &[]);
    f.session
        .execute(
            SetTrackArm {
                track: live,
                armed: true,
            }
            .into(),
        )
        .unwrap();
    f.session
        .execute(SetTrackMute { track, mute: true }.into())
        .unwrap();
    let (_ctl, mut proc) = new_engine(RATE, f.graph());
    let volume = crate::PerNote::Registered(7);
    let input = [
        MidiEvent::per_note(0, 0, 62, volume, 0),
        MidiEvent::note_on(1, 0, 62, 100),
        MidiEvent::note_on(2, 0, 64, 100),
    ];
    let both = process(&mut proc, 4800, &input);
    let (_ctl, mut proc) = new_engine(RATE, f.graph());
    let one = process(&mut proc, 4800, &[MidiEvent::note_on(2, 0, 64, 100)]);
    assert!(one.iter().any(|&s| s != 0.0));
    assert_eq!(both, one);
}

#[test]
fn per_note_management_reaches_the_synth_and_devices() {
    let mut f = Fixture::new();
    let (track, _) = f.midi(0.0, &[]);
    f.session
        .execute(SetTrackArm { track, armed: true }.into())
        .unwrap();
    let volume = crate::PerNote::Registered(7);
    let reset = crate::PerNote::Management {
        detach: false,
        reset: true,
    };
    // Key 62's volume goes to 0, then a reset brings it back before its
    // note: it sounds as if neither had come.
    let (_ctl, mut proc) = new_engine(RATE, f.graph());
    let input = [
        MidiEvent::per_note(0, 0, 62, volume, 0),
        MidiEvent::per_note(1, 0, 62, reset, 0),
        MidiEvent::note_on(2, 0, 62, 100),
    ];
    let reset_first = process(&mut proc, 4800, &input);
    let (_ctl, mut proc) = new_engine(RATE, f.graph());
    let plain = process(&mut proc, 4800, &[MidiEvent::note_on(2, 0, 62, 100)]);
    assert!(plain.iter().any(|&s| s != 0.0));
    assert_eq!(reset_first, plain);

    // A device track passes it on as a MIDI 2.0 packet, on its channel.
    let mut set = SetTrackMidiOutput::new(track, Some("Synth".into()));
    set.channel = Some(3);
    f.session.execute(set.into()).unwrap();
    let (_ctl, mut proc) = new_engine(RATE, f.graph());
    process(&mut proc, 512, &input);
    let sent: Vec<_> = proc
        .midi_output()
        .iter()
        .map(|m| (m.data, m.ump()))
        .collect();
    assert_eq!(sent[1], ([0x72, 62, 1], Some([0x40f2_3e01, 0])), "{sent:?}");
}
