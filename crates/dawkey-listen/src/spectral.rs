// SPDX-License-Identifier: AGPL-3.0-or-later
//! Short-time Fourier transforms and what's read off a power spectrum.

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};

/// A short-time Fourier transform with a Hann window.
pub(crate) struct Stft {
    size: usize,
    hop: usize,
    window: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    /// Scales `|X|²` so a frame's bins add up to its mean square.
    norm: f32,
}

impl Stft {
    /// A transform of `size` samples (a power of two) every `hop` samples.
    pub(crate) fn new(size: usize, hop: usize) -> Self {
        let window: Vec<f32> = (0..size)
            .map(|i| {
                let x = std::f64::consts::TAU * i as f64 / size as f64;
                (0.5 - 0.5 * x.cos()) as f32
            })
            .collect();
        let energy: f32 = window.iter().map(|w| w * w).sum();
        Self {
            size,
            hop: hop.max(1),
            fft: RealFftPlanner::<f32>::new().plan_fft_forward(size),
            norm: 1.0 / (size as f32 * energy),
            window,
        }
    }

    /// The transform about `seconds` long at `rate`, rounded to a power of
    /// two, with `per_frame` hops a frame.
    pub(crate) fn about(rate: u32, seconds: f64, per_frame: usize) -> Self {
        let size = ((f64::from(rate) * seconds) as usize)
            .next_power_of_two()
            .max(64);
        Self::new(size, size / per_frame.max(1))
    }

    pub(crate) fn size(&self) -> usize {
        self.size
    }

    pub(crate) fn hop(&self) -> usize {
        self.hop
    }

    pub(crate) fn bins(&self) -> usize {
        self.size / 2 + 1
    }

    /// The frequency of bin `k` at `rate`.
    pub(crate) fn bin_hz(&self, k: f64, rate: u32) -> f64 {
        k * f64::from(rate) / self.size as f64
    }

    /// Calls `f(center, power)` for frames centered on samples 0, `hop`,
    /// 2 × `hop`, ... before `x`'s end (zero outside it). `power` has
    /// [`bins`](Self::bins) values that add up to the windowed frame's mean
    /// square, so 10 log10 of a band's sum is its level in dBFS RMS.
    pub(crate) fn frames(&self, x: &[f32], mut f: impl FnMut(usize, &[f32])) {
        self.frames_from(x, 0, x.len(), &mut f);
    }

    /// Like [`frames`](Self::frames), for frames centered from `from` to
    /// before `to`.
    pub(crate) fn frames_from(
        &self,
        x: &[f32],
        from: usize,
        to: usize,
        f: &mut impl FnMut(usize, &[f32]),
    ) {
        let mut input = self.fft.make_input_vec();
        let mut output = self.fft.make_output_vec();
        let mut scratch = self.fft.make_scratch_vec();
        let mut power = vec![0.0f32; self.bins()];
        let half = self.size / 2;
        let last = self.bins() - 1;
        let mut center = from;
        while center < to.min(x.len()) {
            for (i, (slot, w)) in input.iter_mut().zip(&self.window).enumerate() {
                let at = (center + i).checked_sub(half);
                let s = at.and_then(|at| x.get(at)).copied().unwrap_or(0.0);
                *slot = if s.is_finite() { s * w } else { 0.0 };
            }
            if self
                .fft
                .process_with_scratch(&mut input, &mut output, &mut scratch)
                .is_ok()
            {
                for (k, (p, c)) in power.iter_mut().zip(&output).enumerate() {
                    let edge = if k == 0 || k == last { 1.0 } else { 2.0 };
                    *p = edge * c.norm_sqr() * self.norm;
                }
                f(center, &power);
            }
            center += self.hop;
        }
    }
}

/// Note names with sharps, from C.
pub(crate) const PITCH_CLASSES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// The MIDI note number (fractional) of `hz`, A4 = 440 Hz = 69.
pub(crate) fn midi_of_hz(hz: f64) -> f64 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

/// The frequency of a MIDI note number.
pub(crate) fn hz_of_midi(midi: f64) -> f64 {
    440.0 * 2f64.powf((midi - 69.0) / 12.0)
}

/// A MIDI note's name with its octave as Dawkey's piano roll shows it
/// (60 is C4).
pub fn note_name(midi: i32) -> String {
    let class = PITCH_CLASSES[midi.rem_euclid(12) as usize];
    format!("{class}{}", midi.div_euclid(12) - 1)
}

/// Centers of the octave bands reported, in Hz (the ISO ones, as named).
pub(crate) const OCTAVE_CENTERS: [f64; 10] = [
    31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// Power in each octave band (centers [`OCTAVE_CENTERS`]) of a power
/// spectrum from `stft` at `rate`.
pub(crate) fn octave_powers(power: &[f64], stft: &Stft, rate: u32) -> [f64; 10] {
    let mut bands = [0.0; 10];
    for (k, p) in power.iter().enumerate() {
        let hz = stft.bin_hz(k as f64, rate);
        // Band edges are the centers times 2^(±1/2); 1000 Hz is exact.
        let octave = (hz / 1000.0).log2().round() + 5.0;
        if hz > 0.0 && (0.0..10.0).contains(&octave) {
            bands[octave as usize] += p;
        }
    }
    bands
}

/// Brightness measures of a power spectrum.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Shape {
    /// Power-weighted mean frequency.
    pub(crate) centroid_hz: f64,
    /// Below this frequency lies 85 % of the power.
    pub(crate) rolloff_hz: f64,
    /// Geometric over arithmetic mean of the power from 30 Hz to 16 kHz:
    /// near 1 for noise, near 0 for a few tones.
    pub(crate) flatness: f64,
}

pub(crate) fn shape(power: &[f64], stft: &Stft, rate: u32) -> Shape {
    let total: f64 = power.iter().sum();
    if total <= 1e-20 {
        return Shape::default();
    }
    let centroid = power
        .iter()
        .enumerate()
        .map(|(k, p)| stft.bin_hz(k as f64, rate) * p)
        .sum::<f64>()
        / total;
    let mut sum = 0.0;
    let mut rolloff = 0.0;
    for (k, p) in power.iter().enumerate() {
        sum += p;
        if sum >= 0.85 * total {
            rolloff = stft.bin_hz(k as f64, rate);
            break;
        }
    }
    let (mut logs, mut lin, mut n) = (0.0, 0.0, 0usize);
    for (k, &p) in power.iter().enumerate() {
        let hz = stft.bin_hz(k as f64, rate);
        if (30.0..=16_000.0).contains(&hz) {
            logs += (p + 1e-24).ln();
            lin += p;
            n += 1;
        }
    }
    let flatness = if n > 0 && lin > 0.0 {
        ((logs / n as f64).exp() / (lin / n as f64)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Shape {
        centroid_hz: centroid,
        rolloff_hz: rolloff,
        flatness,
    }
}

/// Smooths a power spectrum on a log-frequency axis: the mean power
/// density over `fraction` of an octave around each of `points` (Hz).
pub(crate) fn smoothed_db(
    power: &[f64],
    stft: &Stft,
    rate: u32,
    points: &[f64],
    fraction: f64,
) -> Vec<f64> {
    let bin_of = |hz: f64| hz * stft.size() as f64 / f64::from(rate);
    // A prefix sum makes each window's mean one subtraction.
    let mut prefix = Vec::with_capacity(power.len() + 1);
    prefix.push(0.0);
    for p in power {
        prefix.push(prefix.last().copied().unwrap_or(0.0) + p);
    }
    let half = 2f64.powf(fraction / 2.0);
    points
        .iter()
        .map(|&hz| {
            let lo = bin_of(hz / half).floor().max(1.0) as usize;
            let hi = (bin_of(hz * half).ceil() as usize)
                .max(lo + 1)
                .min(power.len());
            if lo >= hi {
                return -200.0;
            }
            let mean = (prefix[hi] - prefix[lo]) / (hi - lo) as f64;
            10.0 * (mean + 1e-24).log10()
        })
        .collect()
}

/// `n` frequencies spread evenly on a log axis from `lo` to `hi` Hz.
pub(crate) fn log_points(lo: f64, hi: f64, n: usize) -> Vec<f64> {
    let (a, b) = (lo.ln(), hi.ln());
    (0..n)
        .map(|i| (a + (b - a) * i as f64 / (n.max(2) - 1) as f64).exp())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f64, rate: u32, frames: usize, amp: f32) -> Vec<f32> {
        (0..frames)
            .map(|i| amp * (std::f64::consts::TAU * hz * i as f64 / f64::from(rate)).sin() as f32)
            .collect()
    }

    #[test]
    fn frames_add_up_to_the_mean_square() {
        let rate = 48_000;
        let x = sine(1000.0, rate, 48_000, 1.0);
        let stft = Stft::new(4096, 1024);
        let mut sums = Vec::new();
        stft.frames_from(&x, 4096, 40_000, &mut |_, p| {
            sums.push(p.iter().map(|&v| f64::from(v)).sum::<f64>())
        });
        for s in sums {
            assert!((s - 0.5).abs() < 0.01, "{s}");
        }
    }

    #[test]
    fn names_notes() {
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(69), "A4");
        assert_eq!(note_name(45), "A2");
        assert_eq!(note_name(-1), "B-2");
        assert!((midi_of_hz(440.0) - 69.0).abs() < 1e-9);
        assert!((hz_of_midi(57.0) - 220.0).abs() < 1e-9);
    }

    #[test]
    fn shape_of_a_sine() {
        let rate = 48_000;
        let x = sine(2000.0, rate, 48_000, 0.5);
        let stft = Stft::new(8192, 2048);
        let mut avg = vec![0.0f64; stft.bins()];
        stft.frames_from(&x, 8192, 40_000, &mut |_, p| {
            for (a, &v) in avg.iter_mut().zip(p) {
                *a += f64::from(v);
            }
        });
        let s = shape(&avg, &stft, rate);
        assert!((s.centroid_hz - 2000.0).abs() < 20.0, "{s:?}");
        assert!(s.flatness < 0.01, "{s:?}");
        let bands = octave_powers(&avg, &stft, rate);
        let loudest = bands
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        assert_eq!(OCTAVE_CENTERS[loudest], 2000.0);
    }
}
