// SPDX-License-Identifier: AGPL-3.0-or-later
//! The tempo map: converts between timeline frames and beats (quarter
//! notes) when the tempo changes along the timeline. And the meter map:
//! where bars start when the time signature changes.
//!
//! A project has a tempo at its start and any number of later changes, each
//! at a beat. Between changes the tempo is constant, unless the later one
//! is a ramp: then the tempo moves in a straight line (over time) from the
//! earlier tempo to the later one.
//!
//! It also has a time signature at its start and any number of later
//! changes, each at the start of a bar.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::TimeSignature;

/// From `beat` on, the tempo is `bpm`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TempoChange {
    /// Quarter notes from the start of the project. More than 0.
    pub beat: f64,
    pub bpm: f64,
    /// The tempo glides here from the previous change (or the start tempo)
    /// instead of jumping at `beat`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ramp: bool,
}

impl TempoChange {
    /// A jump to `bpm` at `beat`.
    pub fn new(beat: f64, bpm: f64) -> Self {
        Self {
            beat,
            bpm,
            ramp: false,
        }
    }
}

/// One stretch of the tempo map: constant, or a ramp to the next one.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    beat: f64,
    /// Timeline frame where the segment starts (fractional).
    frame: f64,
    /// Tempo at the start.
    bpm: f64,
    /// BPM gained per second (0 for constant tempo).
    slope: f64,
}

impl Segment {
    /// Seconds from the segment's start to `beats` beats into it. Before
    /// the start the starting tempo holds.
    fn seconds(&self, beats: f64) -> f64 {
        if beats <= 0.0 || self.slope == 0.0 {
            return beats * 60.0 / self.bpm;
        }
        // beats = (bpm t + slope t² / 2) / 60, solved for t (the stable
        // root).
        let disc = (self.bpm * self.bpm + 120.0 * self.slope * beats).max(0.0);
        120.0 * beats / (self.bpm + disc.sqrt())
    }

    /// Beats from the segment's start to `seconds` into it.
    fn beats(&self, seconds: f64) -> f64 {
        if seconds <= 0.0 {
            return seconds * self.bpm / 60.0;
        }
        (self.bpm * seconds + self.slope * seconds * seconds / 2.0) / 60.0
    }

    /// Tempo `beats` into the segment.
    fn bpm_at(&self, beats: f64) -> f64 {
        self.bpm + self.slope * self.seconds(beats).max(0.0)
    }
}

/// Beat ↔ frame conversions for a tempo map. Cheap to query (a binary
/// search); build it once for many conversions.
#[derive(Debug, Clone, PartialEq)]
pub struct TempoMap {
    sample_rate: f64,
    /// Sorted by beat; the first starts at beat 0.
    segments: Vec<Segment>,
    /// Which segments end in a ramp (parallel to `segments`; the last never
    /// does), for drawing.
    ramps: Vec<bool>,
}

impl TempoMap {
    /// The map for a start tempo and later changes (sorted by beat, each
    /// after beat 0).
    pub fn new(sample_rate: u32, start_bpm: f64, changes: &[TempoChange]) -> Self {
        let rate = f64::from(sample_rate);
        let mut points = vec![TempoChange::new(0.0, start_bpm)];
        for change in changes {
            if change.beat > points.last().expect("starts with one").beat {
                points.push(*change);
            }
        }
        let mut segments: Vec<Segment> = Vec::with_capacity(points.len());
        let mut ramps = Vec::with_capacity(points.len());
        let mut frame = 0.0;
        for (i, point) in points.iter().enumerate() {
            let next = points.get(i + 1).filter(|n| n.ramp);
            let slope = next.map_or(0.0, |n| {
                // A straight line over time from this tempo to the next:
                // the stretch lasts 120 L / (b0 + b1) seconds for L beats.
                let seconds = 120.0 * (n.beat - point.beat) / (point.bpm + n.bpm);
                (n.bpm - point.bpm) / seconds
            });
            let segment = Segment {
                beat: point.beat,
                frame,
                bpm: point.bpm,
                slope,
            };
            if let Some(n) = points.get(i + 1) {
                frame += segment.seconds(n.beat - point.beat) * rate;
            }
            segments.push(segment);
            ramps.push(next.is_some());
        }
        Self {
            sample_rate: rate,
            segments,
            ramps,
        }
    }

    fn segment_at_beat(&self, beat: f64) -> &Segment {
        let i = self.segments.partition_point(|s| s.beat <= beat);
        &self.segments[i.saturating_sub(1)]
    }

    fn segment_at_frame(&self, frame: f64) -> &Segment {
        let i = self.segments.partition_point(|s| s.frame <= frame);
        &self.segments[i.saturating_sub(1)]
    }

    /// Timeline frame (fractional) of `beat`. Negative beats extend the
    /// start tempo backwards.
    pub fn frame_at(&self, beat: f64) -> f64 {
        let s = self.segment_at_beat(beat);
        s.frame + s.seconds(beat - s.beat) * self.sample_rate
    }

    /// Beat at timeline frame `frame` (fractional allowed).
    pub fn beat_at(&self, frame: f64) -> f64 {
        let s = self.segment_at_frame(frame);
        s.beat + s.beats((frame - s.frame) / self.sample_rate)
    }

    /// Seconds from the start of the project to `beat`.
    pub fn seconds_at(&self, beat: f64) -> f64 {
        self.frame_at(beat) / self.sample_rate
    }

    /// Beat at `seconds` from the start of the project.
    pub fn beat_at_seconds(&self, seconds: f64) -> f64 {
        self.beat_at(seconds * self.sample_rate)
    }

    /// Tempo at `beat`, in BPM.
    pub fn bpm_at(&self, beat: f64) -> f64 {
        let s = self.segment_at_beat(beat);
        s.bpm_at(beat - s.beat)
    }

    /// Length of a beat at `beat`, in frames (at the tempo there).
    pub fn frames_per_beat_at(&self, beat: f64) -> f64 {
        60.0 / self.bpm_at(beat) * self.sample_rate
    }

    /// Beats from a clip's start (timeline frame `start`) to `offset` frames
    /// into it.
    pub fn clip_beats(&self, start: u64, offset: u64) -> f64 {
        self.beat_at((start + offset) as f64) - self.beat_at(start as f64)
    }

    /// Frames from a clip's start (timeline frame `start`) to `beats` beats
    /// into it, rounded to the nearest frame.
    pub fn clip_frames(&self, start: u64, beats: f64) -> u64 {
        let at = self.frame_at(self.beat_at(start as f64) + beats).round();
        (at.max(0.0) as u64).saturating_sub(start)
    }

    /// Frames per second.
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Where the tempo changes: (beat, BPM, whether it ramps there from
    /// the previous change), the start tempo first.
    pub fn changes(&self) -> impl Iterator<Item = (f64, f64, bool)> + '_ {
        let ramped = std::iter::once(false).chain(self.ramps.iter().copied());
        self.segments
            .iter()
            .zip(ramped)
            .map(|(s, ramp)| (s.beat, s.bpm, ramp))
    }
}

/// From bar `bar` on, the time signature is `numerator/denominator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MeterChange {
    /// The bar, numbered as the ruler shows it (the first bar is 1). More
    /// than 1.
    pub bar: u32,
    pub numerator: u8,
    pub denominator: u8,
}

impl MeterChange {
    pub fn time_signature(&self) -> TimeSignature {
        TimeSignature {
            numerator: self.numerator,
            denominator: self.denominator,
        }
    }
}

/// One stretch of bars with the same time signature.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MeterSegment {
    /// First bar, counted from 0.
    bar: u32,
    /// Beat (quarter notes) where it starts.
    beat: f64,
    sig: TimeSignature,
    /// Quarter notes per bar.
    bar_beats: f64,
}

/// Bar ↔ beat conversions for a meter map. Bars are counted from 0 here
/// (the ruler shows bar `n` as `n + 1`).
#[derive(Debug, Clone, PartialEq)]
pub struct MeterMap {
    /// Sorted by bar; the first starts at bar 0.
    segments: Vec<MeterSegment>,
}

impl MeterMap {
    /// The map for a time signature at the start and later changes (sorted
    /// by bar, each after the first bar).
    pub fn new(start: TimeSignature, changes: &[MeterChange]) -> Self {
        let mut segments = vec![MeterSegment {
            bar: 0,
            beat: 0.0,
            sig: start,
            bar_beats: start.beats_per_bar(),
        }];
        for change in changes {
            let last = segments.last().expect("starts with one");
            let Some(bar) = change.bar.checked_sub(1).filter(|&b| b > last.bar) else {
                continue;
            };
            let sig = change.time_signature();
            segments.push(MeterSegment {
                bar,
                beat: last.beat + f64::from(bar - last.bar) * last.bar_beats,
                sig,
                bar_beats: sig.beats_per_bar(),
            });
        }
        Self { segments }
    }

    fn segment_at_beat(&self, beat: f64) -> &MeterSegment {
        let i = self.segments.partition_point(|s| s.beat <= beat);
        &self.segments[i.saturating_sub(1)]
    }

    fn segment_of_bar(&self, bar: u32) -> &MeterSegment {
        let i = self.segments.partition_point(|s| s.bar <= bar);
        &self.segments[i.saturating_sub(1)]
    }

    /// Beat where bar `bar` (from 0) starts.
    pub fn bar_start(&self, bar: u32) -> f64 {
        let s = self.segment_of_bar(bar);
        s.beat + f64::from(bar - s.bar) * s.bar_beats
    }

    /// The bar (from 0) that `beat` is in; bar 0 for negative beats.
    pub fn bar_at(&self, beat: f64) -> u32 {
        let s = self.segment_at_beat(beat);
        let into = ((beat - s.beat) / s.bar_beats).floor().max(0.0);
        s.bar.saturating_add(into.min(f64::from(u32::MAX)) as u32)
    }

    /// The time signature at `beat`.
    pub fn meter_at(&self, beat: f64) -> TimeSignature {
        self.segment_at_beat(beat).sig
    }

    /// The time signature of bar `bar` (from 0).
    pub fn meter_of_bar(&self, bar: u32) -> TimeSignature {
        self.segment_of_bar(bar).sig
    }

    /// The first bar line at or after `beat` (a beat just past one, by
    /// rounding, counts as on it).
    pub fn bar_at_or_after(&self, beat: f64) -> f64 {
        let bar = self.bar_at(beat + 1e-9);
        let start = self.bar_start(bar);
        if (start - beat).abs() < 1e-6 {
            start
        } else {
            self.bar_start(bar + 1)
        }
    }

    /// Where the time signature changes: (bar from 0, time signature), the
    /// start first.
    pub fn changes(&self) -> impl Iterator<Item = (u32, TimeSignature)> + '_ {
        self.segments.iter().map(|s| (s.bar, s.sig))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_tempo() {
        let map = TempoMap::new(48_000, 120.0, &[]);
        assert_eq!(map.frame_at(1.0), 24_000.0);
        assert_eq!(map.beat_at(36_000.0), 1.5);
        assert_eq!(map.seconds_at(4.0), 2.0);
        assert_eq!(map.clip_frames(1000, 2.0), 48_000);
        assert!((map.clip_beats(1000, 12_000) - 0.5).abs() < 1e-12);
        assert_eq!(map.bpm_at(100.0), 120.0);
    }

    #[test]
    fn tempo_changes() {
        // 120 BPM for 4 beats (2 s), then 60 BPM.
        let map = TempoMap::new(48_000, 120.0, &[TempoChange::new(4.0, 60.0)]);
        assert_eq!(map.frame_at(4.0), 96_000.0);
        assert_eq!(map.frame_at(5.0), 144_000.0);
        assert_eq!(map.beat_at(144_000.0), 5.0);
        assert_eq!(map.beat_at(48_000.0), 2.0);
        assert_eq!(map.bpm_at(3.99), 120.0);
        assert_eq!(map.bpm_at(4.0), 60.0);
        // A clip starting at beat 3 (72 000): 2 beats in reaches beat 5.
        assert_eq!(map.clip_frames(72_000, 2.0), 72_000);
        assert!((map.clip_beats(72_000, 72_000) - 2.0).abs() < 1e-12);
        assert_eq!(
            map.changes().collect::<Vec<_>>(),
            [(0.0, 120.0, false), (4.0, 60.0, false)]
        );
    }

    #[test]
    fn tempo_ramps() {
        // 60 BPM rising to 180 BPM over the first 8 beats, then steady: the
        // ramp takes 120 · 8 / 240 = 4 s, at 120 BPM on average.
        let map = TempoMap::new(
            48_000,
            60.0,
            &[TempoChange {
                beat: 8.0,
                bpm: 180.0,
                ramp: true,
            }],
        );
        let close = |a: f64, b: f64| (a - b).abs() < 1e-6;
        assert!(close(map.seconds_at(8.0), 4.0));
        assert!(close(map.seconds_at(9.0), 4.0 + 1.0 / 3.0));
        // Halfway through in time (2 s) the tempo is 120 BPM, and
        // (60 · 2 + 30 · 4 / 2) / 60 = 3 beats have passed.
        assert!(close(map.beat_at_seconds(2.0), 3.0));
        assert!(close(map.seconds_at(3.0), 2.0));
        assert!(close(map.bpm_at(3.0), 120.0));
        assert!(close(map.bpm_at(8.0), 180.0));
        assert!(close(map.bpm_at(100.0), 180.0));
        // Round trips everywhere.
        for i in 0..200 {
            let beat = f64::from(i) * 0.1 - 2.0;
            assert!(close(map.beat_at(map.frame_at(beat)), beat), "beat {beat}");
        }
        assert!(close(map.frames_per_beat_at(3.0), 24_000.0));
        assert_eq!(
            map.changes().collect::<Vec<_>>(),
            [(0.0, 60.0, false), (8.0, 180.0, true)]
        );
        // A ramp down works the same way.
        let down = TempoMap::new(
            48_000,
            180.0,
            &[TempoChange {
                beat: 8.0,
                bpm: 60.0,
                ramp: true,
            }],
        );
        assert!(close(down.seconds_at(8.0), 4.0));
        assert!(close(down.beat_at(down.frame_at(5.5)), 5.5));
    }

    #[test]
    fn meter_changes() {
        // 4/4 for 2 bars, then 7/8 (3.5 quarter notes) from bar 3, then
        // 3/4 from bar 5.
        let four = TimeSignature::default();
        let map = MeterMap::new(
            four,
            &[
                MeterChange {
                    bar: 3,
                    numerator: 7,
                    denominator: 8,
                },
                MeterChange {
                    bar: 5,
                    numerator: 3,
                    denominator: 4,
                },
            ],
        );
        assert_eq!(map.bar_start(1), 4.0);
        assert_eq!(map.bar_start(2), 8.0);
        assert_eq!(map.bar_start(3), 11.5);
        assert_eq!(map.bar_start(4), 15.0);
        assert_eq!(map.bar_start(5), 18.0);
        assert_eq!(map.bar_at(7.9), 1);
        assert_eq!(map.bar_at(11.5), 3);
        assert_eq!(map.bar_at(16.0), 4);
        assert_eq!(map.bar_at(-3.0), 0);
        assert_eq!(map.meter_at(9.0).numerator, 7);
        assert_eq!(map.meter_of_bar(4).denominator, 4);
        assert_eq!(map.bar_at_or_after(8.0), 8.0);
        assert_eq!(map.bar_at_or_after(8.1), 11.5);
        assert_eq!(
            map.changes().map(|(bar, _)| bar).collect::<Vec<_>>(),
            [0, 2, 4]
        );
    }
}
