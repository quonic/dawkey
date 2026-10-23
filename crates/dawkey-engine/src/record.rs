// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recording: the real-time thread copies input frames into lock-free rings,
//! tagged with their timeline position; a disk thread drains them. MIDI
//! input goes into one more ring, which the control side drains.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use dawkey_core::{InputChannels, TimeRange};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::midi::MidiEvent;

/// What the real-time thread records from MIDI input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiRecord {
    /// A channel message at timeline frame `position`, from device
    /// `source` (a [`device_tag`](crate::device_tag); 0 if unknown), with
    /// the 32-bit value of a per-note one (see
    /// [`MidiEvent`](crate::MidiEvent)).
    Event {
        position: u64,
        data: [u8; 3],
        source: u32,
        value: u32,
    },
    /// Playback wrapped from the loop end to the loop start.
    Wrap,
}

/// A run of consecutive input frames that starts at timeline frame `start`.
/// Its samples (interleaved for stereo) are pushed before the mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub start: u64,
    pub frames: u32,
}

/// State shared between the real-time thread and the disk thread.
#[derive(Debug, Default)]
pub struct RecordShared {
    /// Set once the real-time thread has written its last frame.
    finished: AtomicBool,
    /// Frames lost because a ring was full (the disk fell behind).
    dropped: AtomicU64,
    /// MIDI messages lost because the MIDI ring was full.
    dropped_midi: AtomicU64,
}

impl RecordShared {
    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// Marks the session finished from the control side, for when the audio
    /// thread isn't running to do it.
    pub fn finish(&self) {
        self.finished.store(true, Ordering::Release);
    }

    pub fn dropped_frames(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn dropped_midi(&self) -> u64 {
        self.dropped_midi.load(Ordering::Relaxed)
    }
}

/// The real-time half of a recording: one target per armed audio track,
/// and a MIDI ring if any MIDI track is armed.
pub struct RecordSession {
    pub(crate) targets: Vec<RecordTarget>,
    pub(crate) midi: Option<Producer<MidiRecord>>,
    pub(crate) punch: Option<TimeRange>,
    pub(crate) shared: Arc<RecordShared>,
}

impl std::fmt::Debug for RecordSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordSession")
            .field("targets", &self.targets.len())
            .field("punch", &self.punch)
            .finish_non_exhaustive()
    }
}

pub(crate) struct RecordTarget {
    pub(crate) channels: InputChannels,
    pub(crate) samples: Producer<f32>,
    pub(crate) marks: Producer<Mark>,
}

/// The disk-thread half of one target.
pub struct RecordTap {
    pub channels: InputChannels,
    pub samples: Consumer<f32>,
    pub marks: Consumer<Mark>,
}

impl std::fmt::Debug for RecordTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordTap")
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

/// The control-side halves of a recording.
#[derive(Debug)]
pub struct RecordTaps {
    /// One per audio target, in order.
    pub audio: Vec<RecordTap>,
    /// Recorded MIDI, if requested. Drain it often: it holds
    /// [`MIDI_RING`] messages.
    pub midi: Option<Consumer<MidiRecord>>,
    pub shared: Arc<RecordShared>,
}

/// Capacity of the MIDI recording ring, in messages.
pub const MIDI_RING: usize = 8192;

/// Creates a recording for audio targets with these input channels, and
/// for MIDI if `midi` is set. Each audio ring holds `buffer_frames` frames,
/// which is how far the disk may fall behind.
pub fn record_session(
    targets: &[InputChannels],
    midi: bool,
    punch: Option<TimeRange>,
    buffer_frames: usize,
) -> (RecordSession, RecordTaps) {
    let shared = Arc::new(RecordShared::default());
    let mut rt = Vec::with_capacity(targets.len());
    let mut taps = Vec::with_capacity(targets.len());
    for &channels in targets {
        let (samples_tx, samples_rx) =
            RingBuffer::new(buffer_frames * usize::from(channels.count()));
        // One mark per audio block at worst.
        let (marks_tx, marks_rx) = RingBuffer::new(buffer_frames / 32 + 64);
        rt.push(RecordTarget {
            channels,
            samples: samples_tx,
            marks: marks_tx,
        });
        taps.push(RecordTap {
            channels,
            samples: samples_rx,
            marks: marks_rx,
        });
    }
    let (midi_tx, midi_rx) = if midi {
        let (tx, rx) = RingBuffer::new(MIDI_RING);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let session = RecordSession {
        targets: rt,
        midi: midi_tx,
        punch,
        shared: Arc::clone(&shared),
    };
    let taps = RecordTaps {
        audio: taps,
        midi: midi_rx,
        shared,
    };
    (session, taps)
}

impl RecordSession {
    /// Writes input frames that play at timeline `start..start + len`.
    /// Real-time safe: if a ring is full, the frames are dropped and counted.
    pub(crate) fn write(&mut self, start: u64, left: &[f32], right: &[f32]) {
        let frames = left.len() as u64;
        let (from, to) = match self.punch {
            Some(p) => (start.max(p.start), (start + frames).min(p.end)),
            None => (start, start + frames),
        };
        if from >= to {
            return;
        }
        let (a, b) = ((from - start) as usize, (to - start) as usize);
        let (left, right) = (&left[a..b], &right[a..b]);
        for target in &mut self.targets {
            let count = left.len() * usize::from(target.channels.count());
            if target.samples.slots() < count || target.marks.slots() < 1 {
                self.shared
                    .dropped
                    .fetch_add(left.len() as u64, Ordering::Relaxed);
                continue;
            }
            match target.channels {
                InputChannels::Stereo => {
                    for (l, r) in left.iter().zip(right) {
                        let _ = target.samples.push(*l);
                        let _ = target.samples.push(*r);
                    }
                }
                InputChannels::Left => left.iter().for_each(|s| {
                    let _ = target.samples.push(*s);
                }),
                InputChannels::Right => right.iter().for_each(|s| {
                    let _ = target.samples.push(*s);
                }),
            }
            let _ = target.marks.push(Mark {
                start: from,
                frames: left.len() as u32,
            });
        }
    }

    /// Records MIDI input that arrived at timeline `start + offset - base`.
    /// Real-time safe.
    pub(crate) fn write_midi(&mut self, start: u64, base: u32, events: &[MidiEvent]) {
        let Some(ring) = &mut self.midi else {
            return;
        };
        for event in events {
            let record = MidiRecord::Event {
                position: start + u64::from(event.offset - base),
                data: event.data,
                source: event.source,
                value: event.value,
            };
            if ring.push(record).is_err() {
                self.shared.dropped_midi.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Notes that playback wrapped around the loop. Real-time safe.
    pub(crate) fn wrap(&mut self) {
        if let Some(ring) = &mut self.midi
            && ring.push(MidiRecord::Wrap).is_err()
        {
            self.shared.dropped_midi.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn finish(&self) {
        self.shared.finish();
    }
}
