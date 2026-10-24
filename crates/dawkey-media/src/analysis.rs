// SPDX-License-Identifier: AGPL-3.0-or-later
//! Measurements of rendered audio, used by tests and reported over MCP.

use ebur128::{EbuR128, Mode};
use serde::Serialize;

/// Levels are in dBFS; `None` means digital silence.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Analysis {
    pub frames: usize,
    pub duration_s: f64,
    pub channels: usize,
    /// Sample peak per channel.
    pub peak_dbfs: Vec<Option<f64>>,
    /// RMS level per channel.
    pub rms_dbfs: Vec<Option<f64>>,
    /// EBU R128 integrated loudness. `None` if the audio is too short or
    /// too quiet to measure.
    pub lufs: Option<f64>,
    /// True if every sample is zero.
    pub silent: bool,
    /// FNV-1a hash of the samples quantized to 24 bits, as hex. Equal hashes
    /// mean the audio is identical to 24-bit precision.
    pub hash: String,
    /// Levels in three bands, a rough measure of brightness.
    pub bands: Bands,
}

/// RMS levels of the channels' mean in three bands, in dBFS (`None` for
/// silence): a rough measure of brightness, for telling instruments apart
/// and spotting masking without listening. The bands are split with
/// first-order filters, so they overlap.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Bands {
    /// Below about 250 Hz.
    pub low_dbfs: Option<f64>,
    /// About 250 Hz to 4 kHz.
    pub mid_dbfs: Option<f64>,
    /// Above about 4 kHz.
    pub high_dbfs: Option<f64>,
}

/// Where the bands split, in Hz.
const BAND_LOW_HZ: f64 = 250.0;
const BAND_HIGH_HZ: f64 = 4000.0;

/// Splits a signal into three bands with two first-order low-passes.
#[derive(Debug, Clone)]
struct BandSplitter {
    /// Coefficients and states of the low-passes at the two split points.
    a: [f64; 2],
    z: [f64; 2],
    sum_sq: [f64; 3],
}

impl BandSplitter {
    fn new(sample_rate: u32) -> Self {
        let coefficient = |hz: f64| {
            let x = (-2.0 * std::f64::consts::PI * hz / f64::from(sample_rate.max(1))).exp();
            1.0 - x
        };
        Self {
            a: [coefficient(BAND_LOW_HZ), coefficient(BAND_HIGH_HZ)],
            z: [0.0; 2],
            sum_sq: [0.0; 3],
        }
    }

    fn add(&mut self, x: f64) {
        self.z[0] += self.a[0] * (x - self.z[0]);
        self.z[1] += self.a[1] * (x - self.z[1]);
        let (low, below_high) = (self.z[0], self.z[1]);
        for (sum, band) in self
            .sum_sq
            .iter_mut()
            .zip([low, below_high - low, x - below_high])
        {
            *sum += band * band;
        }
    }

    fn finish(&self, frames: usize) -> Bands {
        let level = |sum: f64| match frames {
            0 => None,
            _ => gain_to_db((sum / frames as f64).sqrt()),
        };
        Bands {
            low_dbfs: level(self.sum_sq[0]),
            mid_dbfs: level(self.sum_sq[1]),
            high_dbfs: level(self.sum_sq[2]),
        }
    }
}

/// Analyzes ranges of a block stream separately (loudness over time): a
/// segment is `[start, end)` in frames from the start of the stream.
#[derive(Debug)]
pub struct SegmentAnalyzers {
    segments: Vec<(usize, usize, Analyzer)>,
    at: usize,
}

impl SegmentAnalyzers {
    pub fn new(channels: usize, sample_rate: u32, segments: &[(usize, usize)]) -> Self {
        Self {
            segments: segments
                .iter()
                .map(|&(start, end)| (start, end, Analyzer::new(channels, sample_rate)))
                .collect(),
            at: 0,
        }
    }

    /// Whether there are segments to analyze.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Adds the next block of the stream.
    pub fn add(&mut self, planes: &[&[f32]]) {
        let frames = planes.first().map_or(0, |p| p.len());
        let (block_start, block_end) = (self.at, self.at + frames);
        for (start, end, analyzer) in &mut self.segments {
            let (from, to) = ((*start).max(block_start), (*end).min(block_end));
            if from < to {
                let slices: Vec<&[f32]> = planes
                    .iter()
                    .map(|p| &p[from - block_start..to - block_start])
                    .collect();
                analyzer.add(&slices);
            }
        }
        self.at = block_end;
    }

    /// One analysis per segment, in order.
    pub fn finish(self) -> Vec<Analysis> {
        self.segments
            .into_iter()
            .map(|(_, _, analyzer)| analyzer.finish())
            .collect()
    }
}

/// Analyzes `segments` (frames) of planar audio separately.
pub fn analyze_segments(
    channels: &[Vec<f32>],
    sample_rate: u32,
    segments: &[(usize, usize)],
) -> Vec<Analysis> {
    let mut analyzers = SegmentAnalyzers::new(channels.len(), sample_rate, segments);
    let planes: Vec<&[f32]> = channels.iter().map(Vec::as_slice).collect();
    analyzers.add(&planes);
    analyzers.finish()
}

pub fn gain_to_db(gain: f64) -> Option<f64> {
    (gain > 0.0).then(|| 20.0 * gain.log10())
}

/// Analyzes planar audio. All channels must have the same length.
pub fn analyze(channels: &[Vec<f32>], sample_rate: u32) -> Analysis {
    let mut analyzer = Analyzer::new(channels.len(), sample_rate);
    let planes: Vec<&[f32]> = channels.iter().map(Vec::as_slice).collect();
    analyzer.add(&planes);
    analyzer.finish()
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// [`analyze`] a block at a time, for audio that doesn't fit in memory.
pub struct Analyzer {
    sample_rate: u32,
    frames: usize,
    peak: Vec<f32>,
    sum_sq: Vec<f64>,
    meter: Option<EbuR128>,
    hash: u64,
    bands: BandSplitter,
}

impl std::fmt::Debug for Analyzer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Analyzer")
            .field("frames", &self.frames)
            .finish_non_exhaustive()
    }
}

impl Analyzer {
    pub fn new(channels: usize, sample_rate: u32) -> Self {
        Self {
            sample_rate,
            frames: 0,
            peak: vec![0.0; channels],
            sum_sq: vec![0.0; channels],
            meter: (channels > 0)
                .then(|| EbuR128::new(channels as u32, sample_rate, Mode::I).ok())
                .flatten(),
            hash: FNV_OFFSET,
            bands: BandSplitter::new(sample_rate),
        }
    }

    /// Adds the next block: one slice per channel, all the same length.
    pub fn add(&mut self, planes: &[&[f32]]) {
        let frames = planes.first().map_or(0, |p| p.len());
        for (i, plane) in planes.iter().enumerate() {
            self.peak[i] = plane.iter().fold(self.peak[i], |m, s| m.max(s.abs()));
            self.sum_sq[i] += plane
                .iter()
                .map(|&s| f64::from(s) * f64::from(s))
                .sum::<f64>();
        }
        if let Some(meter) = &mut self.meter
            && meter.add_frames_planar_f32(planes).is_err()
        {
            self.meter = None;
        }
        let channels = planes.len().max(1) as f64;
        for frame in 0..frames {
            let mut sum = 0.0;
            for plane in planes {
                sum += f64::from(plane[frame]);
                let quantized =
                    (f64::from(plane[frame]).clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
                for byte in quantized.to_le_bytes() {
                    self.hash ^= u64::from(byte);
                    self.hash = self.hash.wrapping_mul(FNV_PRIME);
                }
            }
            self.bands.add(sum / channels);
        }
        self.frames += frames;
    }

    pub fn finish(self) -> Analysis {
        let frames = self.frames;
        let peak_dbfs: Vec<Option<f64>> = self
            .peak
            .iter()
            .map(|&p| gain_to_db(f64::from(p)))
            .collect();
        let rms_dbfs = self
            .sum_sq
            .iter()
            .map(|&sum| match frames {
                0 => None,
                _ => gain_to_db((sum / frames as f64).sqrt()),
            })
            .collect();
        Analysis {
            frames,
            duration_s: frames as f64 / f64::from(self.sample_rate),
            channels: self.peak.len(),
            silent: peak_dbfs.iter().all(Option::is_none),
            peak_dbfs,
            rms_dbfs,
            lufs: self
                .meter
                .and_then(|m| m.loudness_global().ok())
                .filter(|l| l.is_finite()),
            hash: format!("{:016x}", self.hash),
            bands: self.bands.finish(frames),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_a_full_scale_sine() {
        let rate = 48_000;
        let sine: Vec<f32> = (0..rate * 3)
            .map(|i| (i as f32 * 997.0 * std::f32::consts::TAU / rate as f32).sin())
            .collect();
        let a = analyze(&[sine.clone(), vec![0.0; sine.len()]], rate);
        assert_eq!((a.frames, a.channels), (sine.len(), 2));
        assert!(a.peak_dbfs[0].unwrap().abs() < 0.01);
        assert!((a.rms_dbfs[0].unwrap() + 3.01).abs() < 0.01);
        assert_eq!(a.peak_dbfs[1], None);
        assert!(!a.silent);
        // A full-scale 1 kHz-ish sine in one channel is about -3 LUFS.
        let lufs = a.lufs.unwrap();
        assert!((lufs + 3.0).abs() < 0.5, "{lufs}");
    }

    #[test]
    fn silence_and_hash() {
        let a = analyze(&[vec![0.0; 100]], 48_000);
        assert!(a.silent && a.lufs.is_none());
        let b = analyze(&[vec![0.0; 100]], 48_000);
        let c = analyze(&[vec![0.5; 100]], 48_000);
        assert_eq!(a.hash, b.hash);
        assert_ne!(a.hash, c.hash);
    }
}
