// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs::File;
use std::path::Path;

use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Resampler};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::{AudioData, MediaError};

/// Resampler chunk size, in input frames.
const RESAMPLE_CHUNK: usize = 1024;

/// A decoded file, converted to the requested sample rate.
#[derive(Debug, Clone)]
pub struct DecodedFile {
    pub audio: AudioData,
    /// The file's own sample rate, before conversion.
    pub source_rate: u32,
}

/// Decodes the first audio track of `path` and converts it to `target_rate`.
pub fn decode_file(path: &Path, target_rate: u32) -> Result<DecodedFile, MediaError> {
    let (channels, source_rate) = decode_native(path)?;
    if channels.is_empty() || channels[0].is_empty() {
        return Err(MediaError::Empty {
            path: path.to_owned(),
        });
    }
    let channels = if source_rate == target_rate {
        channels
    } else {
        resample(&channels, source_rate, target_rate).map_err(|reason| MediaError::Decode {
            path: path.to_owned(),
            reason,
        })?
    };
    Ok(DecodedFile {
        audio: AudioData::new(channels),
        source_rate,
    })
}

/// Decodes to planar f32 at the file's own rate.
fn decode_native(path: &Path) -> Result<(Vec<Vec<f32>>, u32), MediaError> {
    let unsupported = |reason: String| MediaError::Unsupported {
        path: path.to_owned(),
        reason,
    };
    let decode_err = |reason: String| MediaError::Decode {
        path: path.to_owned(),
        reason,
    };

    let file = File::open(path).map_err(|source| MediaError::Io {
        path: path.to_owned(),
        source,
    })?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| unsupported(e.to_string()))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| unsupported("no audio track".into()))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| unsupported("no audio codec parameters".into()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| unsupported(e.to_string()))?;

    let mut channels: Vec<Vec<f32>> = Vec::new();
    let mut rate = 0;
    let mut planes: Vec<Vec<f32>> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            // A new logical stream (chained Ogg) starts; treat it as the end.
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(decode_err(e.to_string())),
        };
        if packet.track_id != track_id {
            continue;
        }
        let buffer = match decoder.decode(&packet) {
            Ok(buffer) => buffer,
            // Skip corrupt packets, as players do.
            Err(SymphoniaError::DecodeError(_) | SymphoniaError::IoError(_)) => continue,
            Err(e) => return Err(decode_err(e.to_string())),
        };
        let spec = buffer.spec();
        if channels.is_empty() {
            rate = spec.rate();
            channels = vec![Vec::new(); spec.channels().count()];
        } else if spec.channels().count() != channels.len() || spec.rate() != rate {
            return Err(decode_err(
                "channel layout or rate changes mid-stream".into(),
            ));
        }
        buffer.copy_to_vecs_planar(&mut planes);
        for (channel, plane) in channels.iter_mut().zip(&planes) {
            channel.extend_from_slice(plane);
        }
    }
    Ok((channels, rate))
}

fn resample(channels: &[Vec<f32>], from: u32, to: u32) -> Result<Vec<Vec<f32>>, String> {
    let count = channels.len();
    let frames = channels[0].len();
    let mut resampler = Fft::<f32>::new(
        from as usize,
        to as usize,
        RESAMPLE_CHUNK,
        count,
        FixedSync::Both,
    )
    .map_err(|e| e.to_string())?;
    let input = SequentialSliceOfVecs::new(channels, count, frames).map_err(|e| e.to_string())?;
    let output = resampler
        .process_all(&input, frames, None)
        .map_err(|e| e.to_string())?;
    let mut interleaved = output.take_data();
    // rubato rounds the length up from a float ratio, which can come out a
    // frame long (48000 / 44100 * 44100 is a hair over 48000).
    let exact = (frames as u64 * u64::from(to)).div_ceil(u64::from(from)) as usize;
    interleaved.truncate(exact * count);
    let mut planar = vec![Vec::with_capacity(interleaved.len() / count); count];
    for frame in interleaved.chunks_exact(count) {
        for (channel, sample) in planar.iter_mut().zip(frame) {
            channel.push(*sample);
        }
    }
    Ok(planar)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write_wav;

    fn sine(frames: usize, rate: u32, freq: f32) -> Vec<f32> {
        (0..frames)
            .map(|i| (i as f32 * freq * std::f32::consts::TAU / rate as f32).sin() * 0.5)
            .collect()
    }

    #[test]
    fn decodes_wav_at_native_rate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        let left = sine(4800, 48_000, 440.0);
        let right: Vec<f32> = left.iter().map(|s| -s).collect();
        write_wav(&path, &[left.clone(), right.clone()], 48_000).unwrap();

        let decoded = decode_file(&path, 48_000).unwrap();
        assert_eq!(decoded.source_rate, 48_000);
        assert_eq!(decoded.audio.channels(), &[left, right]);
    }

    #[test]
    fn resamples_to_target_rate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        write_wav(&path, &[sine(44_100, 44_100, 1000.0)], 44_100).unwrap();

        let decoded = decode_file(&path, 48_000).unwrap();
        assert_eq!(decoded.source_rate, 44_100);
        // Exactly a second: the length is rounded in whole numbers.
        assert_eq!(decoded.audio.frames(), 48_000);
        let peak = decoded
            .audio
            .channel(0)
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.02, "peak {peak}");
    }

    #[test]
    fn reports_bad_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("junk.wav");
        std::fs::write(&path, b"definitely not audio").unwrap();
        assert!(matches!(
            decode_file(&path, 48_000),
            Err(MediaError::Unsupported { .. })
        ));
        assert!(matches!(
            decode_file(&dir.path().join("missing.wav"), 48_000),
            Err(MediaError::Io { .. })
        ));
    }
}
