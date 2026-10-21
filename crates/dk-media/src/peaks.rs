// SPDX-License-Identifier: AGPL-3.0-or-later
//! Waveform overviews: min/max per block at several resolutions, so a clip
//! can be drawn at any zoom without touching the samples.

use crate::AudioData;

/// Frames per block at the finest level.
const BASE_BLOCK: usize = 64;
/// Each level's blocks are this many times larger than the previous one's.
const FACTOR: usize = 4;

#[derive(Debug, Clone, PartialEq)]
struct Level {
    block: usize,
    min: Vec<f32>,
    max: Vec<f32>,
}

/// Min/max overview of all channels mixed together.
#[derive(Debug, Clone, PartialEq)]
pub struct Peaks {
    frames: usize,
    levels: Vec<Level>,
}

impl Peaks {
    pub fn compute(audio: &AudioData) -> Self {
        let frames = audio.frames();
        let blocks = frames.div_ceil(BASE_BLOCK);
        let mut base = Level {
            block: BASE_BLOCK,
            min: vec![0.0; blocks],
            max: vec![0.0; blocks],
        };
        for channel in audio.channels() {
            for (i, chunk) in channel.chunks(BASE_BLOCK).enumerate() {
                for &s in chunk {
                    base.min[i] = base.min[i].min(s);
                    base.max[i] = base.max[i].max(s);
                }
            }
        }
        let mut levels = vec![base];
        while levels.last().is_some_and(|l| l.min.len() > FACTOR) {
            let prev = levels.last().expect("non-empty");
            let fold = |v: &[f32], f: fn(f32, f32) -> f32| -> Vec<f32> {
                v.chunks(FACTOR)
                    .map(|c| c.iter().copied().reduce(f).unwrap_or(0.0))
                    .collect()
            };
            let next = Level {
                block: prev.block * FACTOR,
                min: fold(&prev.min, f32::min),
                max: fold(&prev.max, f32::max),
            };
            levels.push(next);
        }
        Self { frames, levels }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Lowest and highest sample in frames `start..end` (clamped), using
    /// the coarsest level that still has at least two blocks in the range.
    /// Block boundaries are approximate by up to one block.
    pub fn range(&self, start: usize, end: usize) -> (f32, f32) {
        let end = end.min(self.frames);
        if start >= end {
            return (0.0, 0.0);
        }
        let span = end - start;
        let level = self
            .levels
            .iter()
            .rev()
            .find(|l| l.block * 2 <= span)
            .unwrap_or(&self.levels[0]);
        let first = start / level.block;
        let last = (end - 1) / level.block;
        let min = level.min[first..=last].iter().copied().fold(0.0, f32::min);
        let max = level.max[first..=last].iter().copied().fold(0.0, f32::max);
        (min, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_at_every_zoom() {
        // Silence with one loud spot at frame 10_000, and a negative one at 50_000.
        let mut samples = vec![0.0; 100_000];
        samples[10_000] = 0.9;
        samples[50_000] = -0.5;
        let peaks = Peaks::compute(&AudioData::new(vec![samples]));
        assert_eq!(peaks.frames(), 100_000);
        assert_eq!(peaks.range(0, 100_000), (-0.5, 0.9));
        assert_eq!(peaks.range(9_000, 11_000), (0.0, 0.9));
        assert_eq!(peaks.range(20_000, 40_000), (0.0, 0.0));
        assert_eq!(peaks.range(10_000, 10_001).1, 0.9);
        assert_eq!(peaks.range(99_000, 200_000), (0.0, 0.0));
        assert_eq!(peaks.range(5, 5), (0.0, 0.0));
    }

    #[test]
    fn mixes_channels() {
        let peaks = Peaks::compute(&AudioData::new(vec![vec![0.25; 1000], vec![-0.75; 1000]]));
        assert_eq!(peaks.range(0, 1000), (-0.75, 0.25));
    }
}
