// SPDX-License-Identifier: AGPL-3.0-or-later
//! Onsets (where sounds start) and the tempo their spacing suggests.

use crate::spectral::Stft;

/// A detected onset, before it's placed on a timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RawOnset {
    /// Seconds from the audio's start.
    pub(crate) at_s: f64,
    /// How strong it is next to the strongest, 0 to 1.
    pub(crate) strength: f64,
}

/// An onset envelope (spectral flux) and the onsets picked from it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Onsets {
    /// Seconds between envelope values.
    pub(crate) hop_s: f64,
    pub(crate) envelope: Vec<f64>,
    pub(crate) onsets: Vec<RawOnset>,
}

/// Magnitudes are compressed with ln(1 + GAMMA × |X|): a sound about 60 dB
/// below full scale barely registers.
const GAMMA: f64 = 1000.0;
/// Onsets closer than this are one onset.
const MIN_GAP_S: f64 = 0.03;

/// Bands a third of an octave wide (at least two bins), as bin ranges.
fn bands(stft: &Stft, rate: u32) -> Vec<(usize, usize)> {
    let top = (f64::from(rate) / 2.0).min(16_000.0);
    let mut bands = Vec::new();
    let mut from = (30.0 * stft.size() as f64 / f64::from(rate))
        .floor()
        .max(1.0) as usize;
    let mut hz = 30.0f64;
    while hz < top {
        hz *= 2f64.powf(1.0 / 3.0);
        let to = ((hz * stft.size() as f64 / f64::from(rate)).round() as usize).min(stft.bins());
        if to >= from + 2 {
            bands.push((from, to));
            from = to;
        }
    }
    bands
}

/// Spectral flux on third-octave band levels against the frame two hops
/// back, max-filtered across neighbouring bands (SuperFlux, Böck and
/// Widmer). Bands rather than bins, and the max filter, keep the beating
/// of a low note's harmonics and vibrato from reading as attacks.
pub(crate) fn onsets(x: &[f32], rate: u32) -> Onsets {
    let stft = Stft::about(rate, 0.021, 4);
    let hop_s = stft.hop() as f64 / f64::from(rate);
    let bands = bands(&stft, rate);
    let mut history: [Vec<f64>; 2] = [vec![0.0; bands.len()], vec![0.0; bands.len()]];
    let mut envelope = Vec::with_capacity(x.len() / stft.hop() + 1);
    stft.frames(x, |_, power| {
        let levels: Vec<f64> = bands
            .iter()
            .map(|&(a, b)| {
                let p: f64 = power[a..b].iter().map(|&v| f64::from(v)).sum();
                (1.0 + GAMMA * p.sqrt()).ln()
            })
            .collect();
        let back = &history[0];
        let mut flux = 0.0;
        for (i, l) in levels.iter().enumerate() {
            let lo = i.saturating_sub(1);
            let hi = (i + 2).min(back.len());
            let reference = back[lo..hi].iter().copied().fold(0.0, f64::max);
            flux += (l - reference).max(0.0);
        }
        envelope.push(flux / bands.len().max(1) as f64);
        history.swap(0, 1);
        history[1] = levels;
    });
    let peaks = pick_peaks(&envelope, hop_s);
    let max = peaks.iter().map(|&(_, v)| v).fold(0.0, f64::max);
    let mut onsets: Vec<RawOnset> = Vec::with_capacity(peaks.len());
    let mut after = 0usize;
    for (frame, value) in peaks {
        let center = frame * stft.hop();
        let at = refine(x, rate, center, stft.size(), after);
        if let Some(last) = onsets.last()
            && at as f64 / f64::from(rate) - last.at_s < MIN_GAP_S
        {
            continue;
        }
        after = at + (MIN_GAP_S * f64::from(rate) * 0.5) as usize;
        onsets.push(RawOnset {
            at_s: at as f64 / f64::from(rate),
            strength: if max > 0.0 { value / max } else { 0.0 },
        });
    }
    Onsets {
        hop_s,
        envelope,
        onsets,
    }
}

/// Frames of `envelope` that are onsets: local maxima clearly above the
/// envelope's recent mean, with their values.
fn pick_peaks(envelope: &[f64], hop_s: f64) -> Vec<(usize, f64)> {
    let max = envelope.iter().copied().fold(0.0, f64::max);
    if max <= 1e-9 {
        return Vec::new();
    }
    let delta = 0.08 * max;
    let around = ((0.016 / hop_s).round() as usize).max(1);
    let before = ((0.1 / hop_s).round() as usize).max(1);
    let after = ((0.03 / hop_s).round() as usize).max(1);
    let gap = ((MIN_GAP_S / hop_s).round() as usize).max(1);
    let mut peaks: Vec<(usize, f64)> = Vec::new();
    for t in 0..envelope.len() {
        let v = envelope[t];
        let lo = t.saturating_sub(around);
        let hi = (t + around + 1).min(envelope.len());
        if envelope[lo..hi].iter().any(|&u| u > v) {
            continue;
        }
        let (mlo, mhi) = (
            t.saturating_sub(before),
            (t + after + 1).min(envelope.len()),
        );
        let mean = envelope[mlo..mhi].iter().sum::<f64>() / (mhi - mlo) as f64;
        if v < mean + delta {
            continue;
        }
        if let Some(&(last, _)) = peaks.last()
            && t - last < gap
        {
            continue;
        }
        peaks.push((t, v));
    }
    peaks
}

/// Places an onset found in the frame centered on `center` (`size`
/// samples) to the millisecond: the start of the 1 ms block whose level
/// rises most above the few before it. Never before `after`.
fn refine(x: &[f32], rate: u32, center: usize, size: usize, after: usize) -> usize {
    let block = ((f64::from(rate) / 1000.0) as usize).max(1);
    let from = center.saturating_sub(size / 2).max(after);
    let to = (center + size / 2).min(x.len());
    if from + block >= to {
        return center.min(x.len().saturating_sub(1));
    }
    let level = |at: usize| -> f64 {
        let end = (at + block).min(x.len());
        let peak = x[at.min(end)..end]
            .iter()
            .filter(|s| s.is_finite())
            .fold(0.0f32, |m, s| m.max(s.abs()));
        20.0 * (f64::from(peak) + 1e-6).log10()
    };
    // Levels of the blocks from four before `from` on.
    let first = from.saturating_sub(4 * block);
    let levels: Vec<(usize, f64)> = (first..to)
        .step_by(block)
        .map(|at| (at, level(at)))
        .collect();
    let mut best = (from, f64::MIN);
    for i in 1..levels.len() {
        let (at, l) = levels[i];
        if at < from {
            continue;
        }
        let before = levels[i.saturating_sub(4)..i]
            .iter()
            .map(|&(_, l)| l)
            .fold(f64::MIN, f64::max);
        let rise = l - before;
        if rise > best.1 + 1e-9 {
            best = (at, rise);
        }
    }
    best.0
}

/// A tempo estimate from an onset envelope.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TempoGuess {
    pub(crate) bpm: f64,
    /// The envelope's autocorrelation at the beat's lag next to its
    /// energy: near 1 for a steady pulse.
    pub(crate) confidence: f64,
    /// Other tempos that fit nearly as well (often half or double).
    pub(crate) alternatives: Vec<f64>,
}

/// Guesses the tempo (50 to 220 BPM) from the onset envelope's
/// autocorrelation, leaning towards tempos near 120 as listeners do.
/// `None` without at least a few seconds and a few onsets.
pub(crate) fn tempo(onsets: &Onsets) -> Option<TempoGuess> {
    let e = &onsets.envelope;
    if onsets.onsets.len() < 4 || (e.len() as f64) * onsets.hop_s < 3.0 {
        return None;
    }
    let mean = e.iter().sum::<f64>() / e.len() as f64;
    let d: Vec<f64> = e.iter().map(|v| (v - mean).max(0.0)).collect();
    let energy: f64 = d.iter().map(|v| v * v).sum();
    if energy <= 0.0 {
        return None;
    }
    let lag_of = |bpm: f64| 60.0 / bpm / onsets.hop_s;
    let (lo, hi) = (lag_of(220.0).floor() as usize, lag_of(50.0).ceil() as usize);
    let hi = hi.min(d.len() / 2);
    if lo + 2 >= hi {
        return None;
    }
    let acf =
        |lag: usize| -> f64 { d.iter().zip(&d[lag..]).map(|(a, b)| a * b).sum::<f64>() / energy };
    let values: Vec<f64> = (0..=hi + 1)
        .map(|l| if l >= lo - 1 { acf(l) } else { 0.0 })
        .collect();
    let bpm_of = |lag: f64| 60.0 / (lag * onsets.hop_s);
    let prior = |bpm: f64| (-0.5 * ((bpm / 120.0).log2() / 0.9).powi(2)).exp();
    let mut peaks: Vec<(f64, f64, f64)> = Vec::new();
    for lag in lo..hi {
        let (a, b, c) = (values[lag - 1], values[lag], values[lag + 1]);
        if b <= a || b < c || b <= 0.0 {
            continue;
        }
        let denom = a - 2.0 * b + c;
        let offset = if denom.abs() > 1e-12 {
            (0.5 * (a - c) / denom).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        let bpm = bpm_of(lag as f64 + offset);
        peaks.push((bpm, b * prior(bpm), b));
    }
    peaks.sort_by(|x, y| y.1.total_cmp(&x.1));
    let &(bpm, score, raw) = peaks.first()?;
    let alternatives = peaks[1..]
        .iter()
        .filter(|p| p.1 >= 0.6 * score)
        .filter(|p| (p.0 / bpm - 1.0).abs() > 0.04)
        .take(2)
        .map(|p| crate::round1(p.0))
        .collect();
    Some(TempoGuess {
        bpm: crate::round1(bpm),
        confidence: crate::round2(raw.clamp(0.0, 1.0)),
        alternatives,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clicks (2 ms bursts of a 3 kHz tone) at `times` seconds.
    fn clicks(times: &[f64], rate: u32, seconds: f64) -> Vec<f32> {
        let mut x = vec![0.0f32; (seconds * f64::from(rate)) as usize];
        for &t in times {
            let at = (t * f64::from(rate)) as usize;
            for i in 0..(rate as usize / 500) {
                if let Some(s) = x.get_mut(at + i) {
                    *s = 0.8
                        * (std::f64::consts::TAU * 3000.0 * i as f64 / f64::from(rate)).sin()
                            as f32;
                }
            }
        }
        x
    }

    #[test]
    fn places_clicks_to_the_millisecond() {
        let rate = 48_000;
        let times: Vec<f64> = (0..8).map(|i| 0.25 + 0.5 * f64::from(i) + 0.0137).collect();
        let found = onsets(&clicks(&times, rate, 4.5), rate);
        assert_eq!(found.onsets.len(), times.len(), "{:?}", found.onsets);
        for (o, t) in found.onsets.iter().zip(&times) {
            assert!((o.at_s - t).abs() < 0.0015, "{} vs {t}", o.at_s);
        }
    }

    #[test]
    fn hears_the_tempo_of_a_click_track() {
        let rate = 48_000;
        for bpm in [93.0, 120.0, 140.0] {
            let beat = 60.0 / bpm;
            let times: Vec<f64> = (0..(12.0 / beat) as usize)
                .map(|i| 0.1 + i as f64 * beat)
                .collect();
            let found = onsets(&clicks(&times, rate, 12.5), rate);
            let guess = tempo(&found).expect("a tempo");
            assert!((guess.bpm - bpm).abs() < 1.0, "{bpm}: {guess:?}");
        }
    }

    #[test]
    fn silence_has_no_onsets() {
        let found = onsets(&vec![0.0; 48_000], 48_000);
        assert!(found.onsets.is_empty());
        assert!(tempo(&found).is_none());
    }
}
