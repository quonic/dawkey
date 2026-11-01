// SPDX-License-Identifier: AGPL-3.0-or-later
//! Describing audio to someone who can't hear it: an agent driving Dawkey
//! over MCP.
//!
//! [`describe`] measures a stretch of audio and reports it the way a mix
//! engineer would talk about it (loudness and dynamics, stereo image, the
//! octave spectrum and its prominent peaks, key and chords, onsets and how
//! they sit on the beat grid, the notes of a melody line), with a timeline
//! and plain-language [`observations`](Description::observations).
//! [`Picture`] draws it for a model that reads images: a spectrogram with a
//! level strip and the bar grid, an averaged spectrum with one curve per
//! stem, and a chromagram with the chords it implies.
//!
//! Everything is deterministic and offline: the same audio gives the same
//! numbers and the same pixels.

#![forbid(unsafe_code)]

mod describe;
mod picture;
mod pitch;
mod rhythm;
mod spectral;
mod tonal;

pub use describe::{
    DescribeOptions, Description, Levels, Moment, Note, Notes, Octave, Onset, Peak, Rhythm,
    Spectrum, Stereo, Tempo, Timeliness, Tonality, describe,
};
pub use picture::{Panel, Picture, PictureKind, PictureOptions, picture, picture_of};
pub use spectral::note_name;

/// Where a stretch of audio sits on the project's timeline, so times can be
/// given in beats and bars and onsets checked against the grid. Audio that
/// isn't from the project (a file) has none.
pub trait Timing {
    /// The beat (quarter notes from the project's start) at `seconds` from
    /// the project's start.
    fn beat_at(&self, seconds: f64) -> f64;
    /// Seconds from the project's start to `beat`.
    fn seconds_at(&self, beat: f64) -> f64;
    /// The bar `beat` is in, as the ruler numbers bars (from 1), with how
    /// far into it as a fraction: 3.5 is halfway through bar 3.
    fn bar_of_beat(&self, beat: f64) -> f64;
    /// The beat where bar `bar` (as the ruler numbers it, from 1) starts.
    fn bar_start(&self, bar: u32) -> f64;
}

/// A stretch of planar audio and where it came from.
#[derive(Clone, Copy)]
pub struct Clip<'a> {
    /// One slice per channel, all the same length.
    pub channels: &'a [Vec<f32>],
    pub sample_rate: u32,
    /// Where the first sample is, in seconds from the project's start (or
    /// from the file's start).
    pub start_s: f64,
    pub timing: Option<&'a dyn Timing>,
}

impl std::fmt::Debug for Clip<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Clip")
            .field("channels", &self.channels.len())
            .field("frames", &self.frames())
            .field("sample_rate", &self.sample_rate)
            .field("start_s", &self.start_s)
            .field("timing", &self.timing.is_some())
            .finish()
    }
}

impl Clip<'_> {
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    pub fn duration_s(&self) -> f64 {
        self.frames() as f64 / f64::from(self.sample_rate)
    }

    /// The bar (fractional, as the ruler numbers them) at `seconds` from
    /// the project's start, if the audio has a timeline.
    pub(crate) fn bar_at(&self, seconds: f64) -> Option<f64> {
        self.timing.map(|t| t.bar_of_beat(t.beat_at(seconds)))
    }

    /// The beat at `seconds` from the project's start.
    pub(crate) fn beat_at(&self, seconds: f64) -> Option<f64> {
        self.timing.map(|t| t.beat_at(seconds))
    }

    /// The sum of the channels divided by their count. Non-finite samples
    /// count as silence.
    pub(crate) fn mono(&self) -> Vec<f32> {
        let n = self.channels.len().max(1) as f32;
        let mut out = vec![0.0f32; self.frames()];
        for channel in self.channels {
            for (o, &s) in out.iter_mut().zip(channel) {
                if s.is_finite() {
                    *o += s;
                }
            }
        }
        for o in &mut out {
            *o /= n;
        }
        out
    }
}

/// dB of a power (mean square) value; `None` for silence.
pub(crate) fn power_db(power: f64) -> Option<f64> {
    (power > 1e-20).then(|| round1(10.0 * power.log10()))
}

/// dBFS of a sample peak; `None` for silence.
pub(crate) fn amplitude_db(peak: f32) -> Option<f64> {
    (peak > 0.0).then(|| round1(20.0 * f64::from(peak).log10()))
}

/// Rounds to one decimal: these numbers are for reading, and more digits
/// only cost tokens.
pub(crate) fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Rounds to two decimals.
pub(crate) fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Rounds to three decimals.
pub(crate) fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}
