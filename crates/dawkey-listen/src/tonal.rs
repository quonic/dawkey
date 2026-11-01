// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pitch-class profiles (chroma), and the keys and chords they suggest.

use crate::spectral::{PITCH_CLASSES, Stft, midi_of_hz};

/// The range chroma is read from: the bass's fundamentals up to where
/// harmonics stop telling notes apart.
const CHROMA_LO_HZ: f64 = 55.0;
const CHROMA_HI_HZ: f64 = 4200.0;

/// The highest harmonic taken out of the peaks above a note.
const MAX_HARMONIC: usize = 8;
/// How much of a sawtooth's harmonic is taken out of a peak above a note.
const HARMONIC_SHARE: f64 = 0.5;
/// How close (as a ratio) a peak must be to a lower one's harmonic.
const HARMONIC_TOLERANCE: f64 = 0.015;

/// A power spectrum's pitch classes: each spectral peak's magnitude goes
/// to the pitch class nearest its (interpolated) frequency. Picking peaks
/// keeps a window's leakage and noise between the notes out; magnitudes
/// rather than powers keep a loud bass from drowning the notes above it.
///
/// A note's harmonics would make one note look like a chord (its third
/// harmonic is a fifth, its fifth a major third), so a peak at `n` times a
/// lower one's frequency loses half of what that harmonic would bring as a
/// sawtooth's (the lower peak's magnitude over `n`). Half, because many
/// sounds are duller than a sawtooth, and a note of the chord that happens
/// to sit on the harmonic must keep most of itself; a lone note's major
/// third still drops well below a chord tone's share.
pub(crate) fn frame_chroma(power: &[f32], stft: &Stft, rate: u32) -> [f64; 12] {
    let mut chroma = [0.0f64; 12];
    let peaks = spectral_peaks(power, stft, rate);
    let mut residual: Vec<f64> = peaks.iter().map(|p| p.1).collect();
    for i in 0..peaks.len() {
        let (hz, magnitude) = peaks[i];
        let mut expected = 0.0f64;
        for &(lower, m) in &peaks[..i] {
            let n = (hz / lower).round();
            if (2.0..=MAX_HARMONIC as f64).contains(&n)
                && (hz / (n * lower) - 1.0).abs() < HARMONIC_TOLERANCE
            {
                expected = expected.max(HARMONIC_SHARE * m / n);
            }
        }
        residual[i] = (magnitude - expected).max(0.0);
    }
    for (&(hz, _), r) in peaks.iter().zip(residual) {
        let class = midi_of_hz(hz).round().rem_euclid(12.0) as usize;
        chroma[class] += r;
    }
    chroma
}

/// The spectral peaks between [`CHROMA_LO_HZ`] and [`CHROMA_HI_HZ`] as
/// (interpolated frequency, magnitude), lowest first.
fn spectral_peaks(power: &[f32], stft: &Stft, rate: u32) -> Vec<(f64, f64)> {
    let mut peaks = Vec::new();
    let lo = (CHROMA_LO_HZ * stft.size() as f64 / f64::from(rate)).floor() as usize;
    let hi = ((CHROMA_HI_HZ * stft.size() as f64 / f64::from(rate)).ceil() as usize)
        .min(power.len().saturating_sub(1));
    if lo.max(1) + 1 >= hi {
        return peaks;
    }
    let floor = power[lo.max(1)..hi.max(lo + 1)]
        .iter()
        .copied()
        .fold(0.0f32, f32::max)
        * 1e-4;
    for k in lo.max(1)..hi {
        let (a, b, c) = (power[k - 1], power[k], power[k + 1]);
        if b <= a || b < c || b <= floor {
            continue;
        }
        // Parabolic interpolation on log power finds the peak between bins.
        let (la, lb, lc) = (
            f64::from(a + 1e-30).ln(),
            f64::from(b).ln(),
            f64::from(c + 1e-30).ln(),
        );
        let denom = la - 2.0 * lb + lc;
        let offset = if denom.abs() > 1e-12 {
            (0.5 * (la - lc) / denom).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        let hz = stft.bin_hz(k as f64 + offset, rate);
        peaks.push((hz, f64::from(a + b + c).sqrt()));
    }
    peaks
}

/// Krumhansl and Kessler's key profiles: how well each scale degree fits
/// a major or minor key.
const MAJOR: [f64; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR: [f64; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

fn correlation(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let ma = a.iter().sum::<f64>() / 12.0;
    let mb = b.iter().sum::<f64>() / 12.0;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        num += (a[i] - ma) * (b[i] - mb);
        da += (a[i] - ma).powi(2);
        db += (b[i] - mb).powi(2);
    }
    if da <= 0.0 || db <= 0.0 {
        0.0
    } else {
        num / (da * db).sqrt()
    }
}

/// Keys ranked by how well `chroma` fits them: (name, correlation), best
/// first.
pub(crate) fn keys(chroma: &[f64; 12]) -> Vec<(String, f64)> {
    let mut keys = Vec::with_capacity(24);
    for tonic in 0..12 {
        for (profile, mode) in [(&MAJOR, "major"), (&MINOR, "minor")] {
            let mut rotated = [0.0; 12];
            for (i, r) in rotated.iter_mut().enumerate() {
                *r = profile[(i + 12 - tonic) % 12];
            }
            keys.push((
                format!("{} {mode}", PITCH_CLASSES[tonic]),
                correlation(chroma, &rotated),
            ));
        }
    }
    keys.sort_by(|a, b| b.1.total_cmp(&a.1));
    keys
}

/// Chord shapes, as intervals above the root, with the suffix that names
/// them. Triads come first, so they win ties.
const CHORDS: [(&str, &[usize]); 9] = [
    ("", &[0, 4, 7]),
    ("m", &[0, 3, 7]),
    ("dim", &[0, 3, 6]),
    ("aug", &[0, 4, 8]),
    ("sus4", &[0, 5, 7]),
    ("sus2", &[0, 2, 7]),
    ("7", &[0, 4, 7, 10]),
    ("maj7", &[0, 4, 7, 11]),
    ("m7", &[0, 3, 7, 10]),
];

/// A chord is named when chroma fits its notes this well (cosine
/// similarity). A loud bass on the root keeps a full mix's chords around
/// 0.8; noise and drums stay well under.
const CHORD_FIT: f64 = 0.75;
/// Each of a chord's notes must be at least this strong next to the
/// strongest: a single note's harmonics (its fifth, and weaker, its major
/// third) mustn't pass for a major chord.
const CHORD_TONE: f64 = 0.2;
/// Chroma must hold this steady from frame to frame for a chord to be
/// named: drums and pitch sweeps change too fast.
const STEADY: f64 = 0.85;

/// Chroma over a stretch of frames, and how steady it held.
#[derive(Debug, Clone, Default)]
pub(crate) struct ChromaSum {
    pub(crate) sum: [f64; 12],
    previous: Option<[f64; 12]>,
    steady: f64,
    weight: f64,
}

impl ChromaSum {
    pub(crate) fn add(&mut self, frame: &[f64; 12]) {
        let norm = |c: &[f64; 12]| c.iter().map(|v| v * v).sum::<f64>().sqrt();
        let n = norm(frame);
        if let Some(previous) = &self.previous {
            let p = norm(previous);
            if n > 0.0 && p > 0.0 {
                let cos = frame.iter().zip(previous).map(|(a, b)| a * b).sum::<f64>() / (n * p);
                // Louder frames count for more.
                let w = (n * p).sqrt();
                self.steady += cos * w;
                self.weight += w;
            }
        }
        self.previous = Some(*frame);
        for (s, v) in self.sum.iter_mut().zip(frame) {
            *s += v;
        }
    }

    /// The weighted mean cosine similarity of consecutive frames.
    pub(crate) fn steadiness(&self) -> f64 {
        if self.weight > 0.0 {
            self.steady / self.weight
        } else {
            0.0
        }
    }

    /// The chord the stretch sounds like: when its chroma held steady and
    /// fits a chord whose every note sounds.
    pub(crate) fn chord(&self) -> Option<String> {
        if self.steadiness() < STEADY {
            return None;
        }
        chord(&normalized(&self.sum))
            .filter(|(_, score)| *score >= CHORD_FIT)
            .map(|(name, _)| name)
    }
}

/// The chord `chroma` sounds most like, as (name, cosine similarity with
/// its template), of those whose every note sounds; `None` if none do.
pub(crate) fn chord(chroma: &[f64; 12]) -> Option<(String, f64)> {
    let norm = chroma.iter().map(|c| c * c).sum::<f64>().sqrt();
    if norm <= 0.0 {
        return None;
    }
    let mut best: Option<(String, f64)> = None;
    for root in 0..12 {
        for (suffix, intervals) in CHORDS {
            let max = chroma.iter().copied().fold(0.0, f64::max);
            if intervals
                .iter()
                .any(|i| chroma[(root + i) % 12] < CHORD_TONE * max)
            {
                continue;
            }
            let dot: f64 = intervals.iter().map(|i| chroma[(root + i) % 12]).sum();
            let score = dot / (norm * (intervals.len() as f64).sqrt());
            if best.as_ref().is_none_or(|(_, b)| score > *b + 1e-9) {
                best = Some((format!("{}{suffix}", PITCH_CLASSES[root]), score));
            }
        }
    }
    best
}

/// `chroma` scaled so its largest value is 1.
pub(crate) fn normalized(chroma: &[f64; 12]) -> [f64; 12] {
    let max = chroma.iter().copied().fold(0.0, f64::max);
    if max <= 0.0 {
        return [0.0; 12];
    }
    chroma.map(|c| c / max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(classes: &[usize]) -> [f64; 12] {
        let mut c = [0.0; 12];
        for &i in classes {
            c[i] = 1.0;
        }
        c
    }

    #[test]
    fn names_chords() {
        assert_eq!(chord(&profile(&[0, 4, 7])).unwrap().0, "C");
        assert_eq!(chord(&profile(&[9, 0, 4])).unwrap().0, "Am");
        assert_eq!(chord(&profile(&[7, 11, 2, 5])).unwrap().0, "G7");
        assert_eq!(chord(&profile(&[2, 5, 9, 0])).unwrap().0, "Dm7");
        assert!(chord(&[0.0; 12]).is_none());
        // A sawtooth's A: its fifth (E) at 0.31, its major third (C#) at
        // 0.16.
        let mut a = [0.0; 12];
        a[9] = 1.0;
        a[4] = 0.31;
        a[1] = 0.16;
        assert!(chord(&a).is_none_or(|(name, _)| name != "A"));
    }

    #[test]
    fn finds_keys() {
        // The C major scale, tonic and fifth weighted: C major.
        let mut c = profile(&[0, 2, 4, 5, 7, 9, 11]);
        c[0] = 2.0;
        c[7] = 1.5;
        assert_eq!(keys(&c)[0].0, "C major");
        // A natural minor, weighted on A and E.
        let mut a = profile(&[9, 11, 0, 2, 4, 5, 7]);
        a[9] = 2.0;
        a[4] = 1.5;
        assert_eq!(keys(&a)[0].0, "A minor");
    }
}
