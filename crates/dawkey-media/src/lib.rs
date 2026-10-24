// SPDX-License-Identifier: AGPL-3.0-or-later
//! Audio files in and out of Dawkey: decoding (any format Symphonia reads),
//! sample-rate conversion to the project rate, WAV writing, exporting mixes
//! (WAV or FLAC, see [`export`]), and analysis.
//! Also reads Standard MIDI Files.

#![forbid(unsafe_code)]

mod analysis;
mod decode;
mod export;
mod midi_file;
mod peaks;

use std::path::{Path, PathBuf};

pub use analysis::{
    Analysis, Analyzer, Bands, SegmentAnalyzers, analyze, analyze_segments, gain_to_db,
};
pub use decode::{DecodedFile, decode_file};
pub use export::{
    BlockSource, ExportAnalysis, ExportProgress, ExportSpec, FileFormat, MemorySource,
    SampleFormat, export, export_segmented,
};
pub use midi_file::{
    MidiFile, MidiFileControl, MidiFileNote, MidiFileTrack, parse_midi_file, read_midi_file,
};
pub use peaks::Peaks;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: unsupported or unrecognized audio file: {reason}")]
    Unsupported { path: PathBuf, reason: String },
    #[error("{path}: decoding failed: {reason}")]
    Decode { path: PathBuf, reason: String },
    #[error("{path}: contains no audio")]
    Empty { path: PathBuf },
    #[error("{path}: {source}")]
    Wav {
        path: PathBuf,
        #[source]
        source: hound::Error,
    },
    #[error("{path}: encoding failed: {reason}")]
    Encode { path: PathBuf, reason: String },
    #[error("the export was cancelled")]
    Cancelled,
}

/// Planar 32-bit float audio at a known sample rate.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioData {
    channels: Vec<Vec<f32>>,
}

impl AudioData {
    /// Panics if `channels` is empty or the channels differ in length.
    pub fn new(channels: Vec<Vec<f32>>) -> Self {
        assert!(!channels.is_empty(), "audio needs at least one channel");
        let frames = channels[0].len();
        assert!(
            channels.iter().all(|c| c.len() == frames),
            "all channels must have the same length"
        );
        Self { channels }
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn frames(&self) -> usize {
        self.channels[0].len()
    }

    pub fn channel(&self, index: usize) -> &[f32] {
        &self.channels[index]
    }

    pub fn channels(&self) -> &[Vec<f32>] {
        &self.channels
    }
}

/// Makes the folder `path` goes in (and its parents), so a file can be
/// written there.
pub fn make_output_folder(path: &Path) -> Result<(), MediaError> {
    match path.parent() {
        Some(folder) if !folder.as_os_str().is_empty() => {
            std::fs::create_dir_all(folder).map_err(|source| MediaError::Io {
                path: folder.to_owned(),
                source,
            })
        }
        _ => Ok(()),
    }
}

/// Writes planar audio as a 32-bit float WAV file, making its folder if
/// needed.
pub fn write_wav(path: &Path, channels: &[Vec<f32>], sample_rate: u32) -> Result<(), MediaError> {
    let wav_err = |source| MediaError::Wav {
        path: path.to_owned(),
        source,
    };
    make_output_folder(path)?;
    let spec = hound::WavSpec {
        channels: channels.len() as u16,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(wav_err)?;
    let frames = channels.first().map_or(0, Vec::len);
    for frame in 0..frames {
        for channel in channels {
            writer.write_sample(channel[frame]).map_err(wav_err)?;
        }
    }
    writer.finalize().map_err(wav_err)
}
