// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pitch of a single line (a bass, a lead, a voice), frame by frame
//! with YIN, and the notes it plays.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::spectral::{Stft, hz_of_midi, midi_of_hz};

/// About the rate pitch is tracked at: enough for fundamentals up to a few
/// kHz, and four times less work than 48 kHz.
const TRACK_RATE: u32 = 11_025;
/// Seconds between pitch frames.
const HOP_S: f64 = 0.01;
/// YIN's threshold on the cumulative mean normalized difference.
const THRESHOLD: f64 = 0.15;
/// Frames above this aperiodicity have no clear pitch.
const VOICED: f64 = 0.22;
/// The highest pitch looked for, in Hz (about G6).
const MAX_HZ: f64 = 1600.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PitchFrame {
    /// Seconds from the audio's start.
    pub(crate) at_s: f64,
    /// MIDI note number (fractional), `None` where there's no clear pitch.
    pub(crate) midi: Option<f64>,
    /// RMS level in dBFS.
    pub(crate) level_db: f64,
}

/// Pitch frames every [`HOP_S`] seconds.
pub(crate) fn track(x: &[f32], rate: u32) -> Vec<PitchFrame> {
    let factor = (rate / TRACK_RATE).max(1) as usize;
    let mut y = decimate(x, factor);
    let rate = f64::from(rate) / factor as f64;
    let window = ((rate * 0.064) as usize).next_power_of_two();
    let w = window / 2;
    // Silence after the end lets the last frames reach it.
    let len = y.len();
    y.resize(len + window, 0.0);
    let hop = ((rate * HOP_S).round() as usize).max(1);
    let yin = Yin::new(window);
    let min_lag = ((rate / MAX_HZ).floor() as usize).max(2);
    let mut frames = Vec::with_capacity(y.len() / hop + 1);
    let mut levels_max = f64::MIN;
    let mut start = 0usize;
    // Frames are placed at the middle of their integration window.
    while start + w <= len {
        let frame = &y[start..start + window];
        let ms = frame[..w]
            .iter()
            .map(|s| f64::from(*s).powi(2))
            .sum::<f64>()
            / w as f64;
        let level_db = 10.0 * (ms + 1e-20).log10();
        levels_max = levels_max.max(level_db);
        let midi = (level_db > -60.0)
            .then(|| yin.pitch(frame, min_lag))
            .flatten()
            .map(|lag| midi_of_hz(rate / lag));
        frames.push(PitchFrame {
            at_s: (start + w / 2) as f64 / rate,
            midi,
            level_db,
        });
        start += hop;
    }
    // Quiet frames next to the loudest are breaths and tails, not notes.
    for f in &mut frames {
        if f.level_db < levels_max - 40.0 {
            f.midi = None;
        }
    }
    frames
}

/// How far below a moment's strongest frequency its fundamental may be.
const FUNDAMENTAL_DB: f64 = 35.0;

/// Drops the pitches whose fundamental isn't in the spectrum. A chord is
/// nearly periodic at a period longer than any of its notes', which YIN
/// takes for a low pitch where nothing sounds.
pub(crate) fn confirm(frames: &mut [PitchFrame], x: &[f32], rate: u32) {
    if frames.iter().all(|f| f.midi.is_none()) {
        return;
    }
    let size = Stft::about(rate, 0.17, 4).size();
    let hop = ((f64::from(rate) * 0.02) as usize).max(1);
    let stft = Stft::new(size, hop);
    let hop_s = hop as f64 / f64::from(rate);
    let floor = 10f64.powf(-FUNDAMENTAL_DB / 10.0);
    let mut next = 0usize;
    stft.frames(x, |center, power| {
        let t = center as f64 / f64::from(rate);
        let loudest = f64::from(power.iter().copied().fold(0.0f32, f32::max));
        while next < frames.len() && frames[next].at_s < t - hop_s / 2.0 {
            next += 1;
        }
        let mut i = next;
        while i < frames.len() && frames[i].at_s < t + hop_s / 2.0 {
            if let Some(midi) = frames[i].midi {
                let f0 = hz_of_midi(midi);
                let bin = |hz: f64| hz * size as f64 / f64::from(rate);
                let lo = (bin(f0 * 0.97).floor() as usize).min(bin(f0) as usize - 1);
                let hi = (bin(f0 * 1.03).ceil() as usize)
                    .max(bin(f0) as usize + 2)
                    .min(power.len());
                let at_f0 = power[lo.min(hi)..hi].iter().copied().fold(0.0f32, f32::max);
                if f64::from(at_f0) < loudest * floor {
                    frames[i].midi = None;
                }
            }
            i += 1;
        }
    });
}

/// Low-passes `x` below the new Nyquist frequency and keeps every
/// `factor`th sample.
fn decimate(x: &[f32], factor: usize) -> Vec<f32> {
    if factor == 1 {
        return x
            .iter()
            .map(|s| if s.is_finite() { *s } else { 0.0 })
            .collect();
    }
    let half = 8 * factor;
    let cutoff = 0.45 / factor as f64;
    let taps: Vec<f32> = (0..=2 * half)
        .map(|i| {
            let n = i as f64 - half as f64;
            let sinc = if n == 0.0 {
                2.0 * cutoff
            } else {
                (std::f64::consts::TAU * cutoff * n).sin() / (std::f64::consts::PI * n)
            };
            let hann = 0.5 + 0.5 * (std::f64::consts::PI * n / (half as f64 + 1.0)).cos();
            (sinc * hann) as f32
        })
        .collect();
    (0..x.len().div_ceil(factor))
        .map(|j| {
            let center = j * factor;
            let mut acc = 0.0f32;
            for (i, t) in taps.iter().enumerate() {
                if let Some(s) = (center + i).checked_sub(half).and_then(|at| x.get(at))
                    && s.is_finite()
                {
                    acc += s * t;
                }
            }
            acc
        })
        .collect()
}

/// YIN with the difference function computed through an FFT.
struct Yin {
    window: usize,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
}

impl Yin {
    fn new(window: usize) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        Self {
            window,
            forward: planner.plan_fft_forward(2 * window),
            inverse: planner.plan_fft_inverse(2 * window),
        }
    }

    /// The period of `frame` (`window` samples) in samples, interpolated;
    /// `None` if it has no clear one.
    fn pitch(&self, frame: &[f32], min_lag: usize) -> Option<f64> {
        let w = self.window / 2;
        let max_lag = self.window - w;
        let n = 2 * self.window;
        // cross[τ] = Σ_{j<w} x[j] x[j+τ], by correlating the first half
        // with the whole frame.
        let mut a = vec![0.0f32; n];
        a[..w].copy_from_slice(&frame[..w]);
        let mut b = vec![0.0f32; n];
        b[..self.window].copy_from_slice(frame);
        let mut fa = self.forward.make_output_vec();
        let mut fb = self.forward.make_output_vec();
        self.forward.process(&mut a, &mut fa).ok()?;
        self.forward.process(&mut b, &mut fb).ok()?;
        let mut product: Vec<Complex<f32>> =
            fa.iter().zip(&fb).map(|(x, y)| x.conj() * y).collect();
        // The inverse wants real DC and Nyquist bins.
        if let Some(first) = product.first_mut() {
            first.im = 0.0;
        }
        if let Some(last) = product.last_mut() {
            last.im = 0.0;
        }
        let mut cross = self.inverse.make_output_vec();
        self.inverse.process(&mut product, &mut cross).ok()?;
        let mut prefix = Vec::with_capacity(self.window + 1);
        prefix.push(0.0f64);
        for s in frame {
            prefix.push(prefix.last().copied().unwrap_or(0.0) + f64::from(*s).powi(2));
        }
        let e0 = prefix[w];
        if e0 <= 1e-12 {
            return None;
        }
        // Cumulative mean normalized difference.
        let mut cmnd = vec![1.0f64; max_lag + 1];
        let mut sum = 0.0;
        for (tau, c) in cmnd.iter_mut().enumerate().skip(1) {
            let et = prefix[tau + w] - prefix[tau];
            let d = (e0 + et - 2.0 * f64::from(cross[tau]) / n as f64).max(0.0);
            sum += d;
            *c = if sum > 0.0 { d * tau as f64 / sum } else { 1.0 };
        }
        let mut tau = None;
        let mut t = min_lag;
        while t < max_lag {
            if cmnd[t] < THRESHOLD {
                while t + 1 < max_lag && cmnd[t + 1] < cmnd[t] {
                    t += 1;
                }
                tau = Some(t);
                break;
            }
            t += 1;
        }
        let tau = match tau {
            Some(t) => t,
            None => {
                let (t, v) = (min_lag..max_lag)
                    .map(|t| (t, cmnd[t]))
                    .min_by(|a, b| a.1.total_cmp(&b.1))?;
                if v > VOICED {
                    return None;
                }
                t
            }
        };
        if cmnd[tau] > VOICED {
            return None;
        }
        let (a, b, c) = (cmnd[tau - 1], cmnd[tau], cmnd[(tau + 1).min(max_lag)]);
        let denom = a - 2.0 * b + c;
        let offset = if denom.abs() > 1e-12 {
            (0.5 * (a - c) / denom).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        Some(tau as f64 + offset)
    }
}

/// A note heard in a pitch track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RawNote {
    pub(crate) start_s: f64,
    pub(crate) end_s: f64,
    pub(crate) midi: i32,
    /// How far off the equal-tempered pitch it is on average (A4 = 440).
    pub(crate) cents: f64,
    pub(crate) level_db: f64,
}

/// Notes shorter than this are glitches between notes.
const MIN_NOTE_S: f64 = 0.045;

/// The notes in a pitch track: runs of frames on one semitone (after a
/// median filter, so vibrato and a stray frame don't split them), split
/// where an onset re-attacks the same pitch.
pub(crate) fn notes(frames: &[PitchFrame], onsets_s: &[f64]) -> Vec<RawNote> {
    let quantized: Vec<Option<i32>> = frames
        .iter()
        .map(|f| f.midi.map(|m| m.round() as i32))
        .collect();
    let smoothed: Vec<Option<i32>> = (0..frames.len())
        .map(|i| {
            quantized[i]?;
            let mut around: Vec<i32> = quantized[i.saturating_sub(2)..(i + 3).min(frames.len())]
                .iter()
                .flatten()
                .copied()
                .collect();
            around.sort_unstable();
            Some(around[around.len() / 2])
        })
        .collect();
    let mut notes = Vec::new();
    let mut i = 0;
    while i < frames.len() {
        let Some(pitch) = smoothed[i] else {
            i += 1;
            continue;
        };
        let start = i;
        let mut end = i;
        let mut j = i + 1;
        // Bridge a gap of a frame or two on the same pitch.
        while j < frames.len() {
            match smoothed[j] {
                Some(p) if p == pitch => {
                    end = j;
                    j += 1;
                }
                None if j - end <= 2 => j += 1,
                _ => break,
            }
        }
        push_split(&mut notes, &frames[start..=end], pitch, onsets_s);
        i = end + 1;
    }
    notes
}

/// Adds the note `frames` play at `pitch`, cut in pieces where onsets
/// re-attack it.
fn push_split(notes: &mut Vec<RawNote>, frames: &[PitchFrame], pitch: i32, onsets_s: &[f64]) {
    let hop = HOP_S;
    let start_s = frames[0].at_s - hop / 2.0;
    let end_s = frames[frames.len() - 1].at_s + hop / 2.0;
    let mut cuts: Vec<f64> = onsets_s
        .iter()
        .copied()
        .filter(|&t| t > start_s + MIN_NOTE_S && t < end_s - MIN_NOTE_S)
        .collect();
    cuts.push(end_s);
    let mut from = start_s;
    for to in cuts {
        let part: Vec<&PitchFrame> = frames
            .iter()
            .filter(|f| f.at_s >= from && f.at_s < to)
            .collect();
        // The median filter can carry a stray frame (an octave off, say)
        // into the note; its tuning leaves those out.
        let voiced: Vec<f64> = part
            .iter()
            .filter_map(|f| f.midi)
            .filter(|m| (m - f64::from(pitch)).abs() < 1.0)
            .collect();
        if to - from >= MIN_NOTE_S && !voiced.is_empty() {
            let cents = voiced
                .iter()
                .map(|m| (m - f64::from(pitch)) * 100.0)
                .sum::<f64>()
                / voiced.len() as f64;
            let level_db = part.iter().map(|f| f.level_db).fold(f64::MIN, f64::max);
            notes.push(RawNote {
                start_s: from,
                end_s: to,
                midi: pitch,
                cents,
                level_db,
            });
        }
        from = to;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sawtooth line: (MIDI note, seconds) pairs, played back to back.
    fn line(notes: &[(f64, f64)], rate: u32) -> Vec<f32> {
        let mut x = Vec::new();
        let mut phase = 0.0f64;
        for &(midi, seconds) in notes {
            let hz = hz_of_midi(midi);
            for _ in 0..(seconds * f64::from(rate)) as usize {
                x.push((0.4 * (2.0 * phase - 1.0)) as f32);
                phase = (phase + hz / f64::from(rate)).fract();
            }
        }
        x
    }

    #[test]
    fn hears_a_bass_line() {
        let rate = 48_000;
        // A1, C2, E2, G2 (55 to 98 Hz), a quarter second each.
        let x = line(
            &[(33.0, 0.25), (36.0, 0.25), (40.0, 0.25), (43.0, 0.25)],
            rate,
        );
        let frames = track(&x, rate);
        let found = notes(&frames, &[]);
        let pitches: Vec<i32> = found.iter().map(|n| n.midi).collect();
        assert_eq!(pitches, vec![33, 36, 40, 43], "{found:?}");
        for n in &found {
            assert!(n.cents.abs() < 10.0, "{n:?}");
        }
        assert!((found[1].start_s - 0.25).abs() < 0.03, "{found:?}");
    }

    #[test]
    fn hears_a_melody_and_its_tuning() {
        let rate = 44_100;
        // E5, a quarter tone sharp, then B4.
        let x = line(&[(76.25, 0.3), (71.0, 0.3)], rate);
        let found = notes(&track(&x, rate), &[]);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].midi, 76);
        assert!((found[0].cents - 25.0).abs() < 6.0, "{found:?}");
        assert_eq!(found[1].midi, 71);
    }

    #[test]
    fn splits_repeated_notes_at_onsets() {
        let rate = 48_000;
        let x = line(&[(45.0, 0.6)], rate);
        let found = notes(&track(&x, rate), &[0.3]);
        assert_eq!(found.len(), 2, "{found:?}");
    }
}
