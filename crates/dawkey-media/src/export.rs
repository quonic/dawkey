// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exporting a mix to a file: stereo audio pulled a block at a time from a
//! [`BlockSource`] (an offline render), converted to the file's sample rate,
//! analyzed, dithered to whole-number samples (TPDF) and written as WAV
//! (16 or 24-bit, or 32-bit float) or FLAC (16 or 24-bit). Nothing has to
//! fit in memory but a FLAC file's compressed frames. The file is written
//! beside its target and renamed at the end, so a failed or cancelled
//! export leaves what was there.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Indexing, Resampler};
use serde::{Deserialize, Serialize};

use crate::{Analysis, Analyzer, MediaError, SegmentAnalyzers};

/// Frames pulled from the source at a time.
const BLOCK: usize = 4096;
/// Frames per resampler chunk.
const RESAMPLE_CHUNK: usize = 1024;

/// The kind of file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileFormat {
    Wav,
    Flac,
}

impl FileFormat {
    /// The format a file name's extension asks for.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "wav" | "wave" => Some(Self::Wav),
            "flac" => Some(Self::Flac),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Flac => "flac",
        }
    }
}

/// How samples are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleFormat {
    /// 16-bit integers (CD), dithered.
    Int16,
    /// 24-bit integers, dithered.
    Int24,
    /// 32-bit float, as the engine renders (WAV only).
    Float32,
}

impl SampleFormat {
    /// Bits per sample.
    pub fn bits(self) -> u32 {
        match self {
            Self::Int16 => 16,
            Self::Int24 => 24,
            Self::Float32 => 32,
        }
    }
}

/// What an export writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportSpec {
    pub file: FileFormat,
    pub sample: SampleFormat,
    /// The file's sample rate; the audio is converted to it.
    pub sample_rate: u32,
}

impl ExportSpec {
    /// An error if the file format can't store these samples.
    pub fn check(&self) -> Result<(), String> {
        if self.file == FileFormat::Flac && self.sample == SampleFormat::Float32 {
            return Err("FLAC stores 16 or 24-bit samples, not 32-bit float".into());
        }
        if !(8_000..=384_000).contains(&self.sample_rate) {
            return Err(format!(
                "a sample rate from 8000 to 384000 Hz, not {}",
                self.sample_rate
            ));
        }
        Ok(())
    }
}

/// Stereo audio, a block at a time.
pub trait BlockSource {
    /// Fills as much of `left` and `right` as it can (they're the same
    /// length); returns how many frames. 0 once there's no more.
    fn read(&mut self, left: &mut [f32], right: &mut [f32]) -> usize;
}

/// How far an export is, shared with whoever waits for it, and a way to
/// stop it.
#[derive(Debug, Default)]
pub struct ExportProgress {
    /// Frames read from the source so far.
    pub frames: AtomicU64,
    /// Set to stop the export; it fails with [`MediaError::Cancelled`].
    pub cancel: AtomicBool,
}

/// What an export measured: the whole file, and its segments if asked.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportAnalysis {
    pub analysis: Analysis,
    /// One per segment asked for, in order.
    pub segments: Vec<Analysis>,
}

/// Writes everything `source` (at `source_rate`) has to `path`, as `spec`
/// says. Returns the analysis of what was written (before dithering).
pub fn export(
    source: &mut dyn BlockSource,
    source_rate: u32,
    spec: &ExportSpec,
    path: &Path,
    progress: &ExportProgress,
) -> Result<Analysis, MediaError> {
    export_segmented(source, source_rate, spec, path, progress, &[]).map(|r| r.analysis)
}

/// [`export`], also analyzing `segments` of the file separately: each is
/// `[start, end)` in seconds from the start of the source.
pub fn export_segmented(
    source: &mut dyn BlockSource,
    source_rate: u32,
    spec: &ExportSpec,
    path: &Path,
    progress: &ExportProgress,
    segments: &[(f64, f64)],
) -> Result<ExportAnalysis, MediaError> {
    spec.check().map_err(|reason| MediaError::Unsupported {
        path: path.to_owned(),
        reason,
    })?;
    crate::make_output_folder(path)?;
    let partial = partial_path(path);
    let mut pipeline = Pipeline::new(source, source_rate, spec, path, progress)?;
    let frames_at_file_rate: Vec<(usize, usize)> = segments
        .iter()
        .map(|&(start, end)| {
            let frame = |s: f64| (s.max(0.0) * f64::from(spec.sample_rate)).round() as usize;
            (frame(start), frame(end))
        })
        .collect();
    pipeline.segments = SegmentAnalyzers::new(2, spec.sample_rate, &frames_at_file_rate);
    let result = match spec.file {
        FileFormat::Wav => write_wav(pipeline, spec, &partial),
        FileFormat::Flac => write_flac(pipeline, spec, &partial),
    };
    let result = result.and_then(|analysis| {
        std::fs::rename(&partial, path).map_err(|source| MediaError::Io {
            path: path.to_owned(),
            source,
        })?;
        Ok(analysis)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

/// Where a file is written before it's complete: `.name.partial` beside it.
fn partial_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.partial"))
}

/// The source at the file's rate: resampled, analyzed and handed out a
/// block at a time.
struct Pipeline<'a> {
    source: &'a mut dyn BlockSource,
    path: PathBuf,
    progress: &'a ExportProgress,
    resampler: Option<StreamResampler>,
    /// Audio at the file's rate, waiting to be written.
    queue: [VecDeque<f32>; 2],
    scratch: [Vec<f32>; 2],
    ended: bool,
    analyzer: Analyzer,
    segments: SegmentAnalyzers,
}

impl Pipeline<'_> {
    /// What was measured, once everything has been pulled.
    fn finish(self) -> ExportAnalysis {
        ExportAnalysis {
            analysis: self.analyzer.finish(),
            segments: self.segments.finish(),
        }
    }
}

impl<'a> Pipeline<'a> {
    fn new(
        source: &'a mut dyn BlockSource,
        source_rate: u32,
        spec: &ExportSpec,
        path: &Path,
        progress: &'a ExportProgress,
    ) -> Result<Self, MediaError> {
        let resampler = (source_rate != spec.sample_rate)
            .then(|| StreamResampler::new(source_rate, spec.sample_rate))
            .transpose()
            .map_err(|reason| MediaError::Unsupported {
                path: path.to_owned(),
                reason,
            })?;
        Ok(Self {
            source,
            path: path.to_owned(),
            progress,
            resampler,
            queue: [VecDeque::new(), VecDeque::new()],
            scratch: [vec![0.0; BLOCK], vec![0.0; BLOCK]],
            ended: false,
            analyzer: Analyzer::new(2, spec.sample_rate),
            segments: SegmentAnalyzers::new(2, spec.sample_rate, &[]),
        })
    }

    /// The next frames, up to `want`, into `left` and `right` (cleared
    /// first); none once it's all been handed out.
    fn pull(
        &mut self,
        want: usize,
        left: &mut Vec<f32>,
        right: &mut Vec<f32>,
    ) -> Result<(), MediaError> {
        while self.queue[0].len() < want && !self.ended {
            if self.progress.cancel.load(Ordering::Relaxed) {
                return Err(MediaError::Cancelled);
            }
            let [l, r] = &mut self.scratch;
            let n = self.source.read(l, r);
            self.progress.frames.fetch_add(n as u64, Ordering::Relaxed);
            let queue = &mut self.queue;
            let mut emit = |l: &[f32], r: &[f32]| {
                queue[0].extend(l);
                queue[1].extend(r);
            };
            let resampled = match (&mut self.resampler, n) {
                (Some(resampler), 0) => resampler.finish(&mut emit),
                (Some(resampler), n) => resampler.push(&l[..n], &r[..n], &mut emit),
                (None, n) => {
                    emit(&l[..n], &r[..n]);
                    Ok(())
                }
            };
            resampled.map_err(|reason| MediaError::Encode {
                path: self.path.clone(),
                reason,
            })?;
            self.ended = n == 0;
        }
        let n = want.min(self.queue[0].len());
        left.clear();
        right.clear();
        left.extend(self.queue[0].drain(..n));
        right.extend(self.queue[1].drain(..n));
        self.analyzer.add(&[left, right]);
        if !self.segments.is_empty() {
            self.segments.add(&[left, right]);
        }
        Ok(())
    }
}

/// A sample-rate converter fed a block at a time.
struct StreamResampler {
    resampler: Fft<f32>,
    /// Input waiting for a whole chunk.
    pending: Vec<Vec<f32>>,
    out: Vec<Vec<f32>>,
    /// Output frames still to drop: the resampler's delay.
    trim: usize,
    frames_in: u64,
    frames_out: u64,
}

impl StreamResampler {
    fn new(from: u32, to: u32) -> Result<Self, String> {
        let resampler = Fft::<f32>::new(
            from as usize,
            to as usize,
            RESAMPLE_CHUNK,
            2,
            FixedSync::Both,
        )
        .map_err(|e| e.to_string())?;
        let out_max = resampler.output_frames_max();
        Ok(Self {
            trim: resampler.output_delay(),
            resampler,
            pending: vec![Vec::new(), Vec::new()],
            out: vec![vec![0.0; out_max], vec![0.0; out_max]],
            frames_in: 0,
            frames_out: 0,
        })
    }

    fn push(
        &mut self,
        left: &[f32],
        right: &[f32],
        emit: &mut dyn FnMut(&[f32], &[f32]),
    ) -> Result<(), String> {
        self.pending[0].extend_from_slice(left);
        self.pending[1].extend_from_slice(right);
        self.frames_in += left.len() as u64;
        loop {
            let need = self.resampler.input_frames_next();
            if self.pending[0].len() < need {
                return Ok(());
            }
            let used = self.chunk(need, None, None, emit)?;
            self.pending.iter_mut().for_each(|p| {
                p.drain(..used);
            });
        }
    }

    /// The rest: what's pending, then silence until all of the input has
    /// come out.
    fn finish(&mut self, emit: &mut dyn FnMut(&[f32], &[f32])) -> Result<(), String> {
        let expected = (self.resampler.resample_ratio() * self.frames_in as f64).ceil() as u64;
        let need = self.resampler.input_frames_next();
        let real = self.pending[0].len();
        self.pending.iter_mut().for_each(|p| p.resize(need, 0.0));
        self.chunk(need, Some(real), Some(expected), emit)?;
        self.pending.iter_mut().for_each(Vec::clear);
        while self.frames_out < expected {
            let need = self.resampler.input_frames_next();
            self.pending.iter_mut().for_each(|p| p.resize(need, 0.0));
            self.chunk(need, Some(0), Some(expected), emit)?;
        }
        Ok(())
    }

    /// Resamples a chunk of `pending` (`partial` frames of it, if given),
    /// handing out what comes out past the delay, up to `limit` frames in
    /// all. Returns how many input frames it used.
    fn chunk(
        &mut self,
        frames: usize,
        partial: Option<usize>,
        limit: Option<u64>,
        emit: &mut dyn FnMut(&[f32], &[f32]),
    ) -> Result<usize, String> {
        let input =
            SequentialSliceOfVecs::new(&self.pending, 2, frames).map_err(|e| e.to_string())?;
        let out_len = self.out[0].len();
        let mut output =
            SequentialSliceOfVecs::new_mut(&mut self.out, 2, out_len).map_err(|e| e.to_string())?;
        let indexing = partial.map(|n| Indexing {
            input_offset: 0,
            output_offset: 0,
            partial_len: Some(n),
            active_channels_mask: None,
        });
        let (used, produced) = self
            .resampler
            .process_into_buffer(&input, &mut output, indexing.as_ref())
            .map_err(|e| e.to_string())?;
        let skip = self.trim.min(produced);
        self.trim -= skip;
        let mut end = produced;
        if let Some(limit) = limit {
            let room = limit.saturating_sub(self.frames_out) as usize;
            end = end.min(skip + room);
        }
        if end > skip {
            emit(&self.out[0][skip..end], &self.out[1][skip..end]);
            self.frames_out += (end - skip) as u64;
        }
        Ok(used)
    }
}

/// Turns float samples into whole numbers of `bits` bits, with TPDF dither
/// (from a fixed seed, so exports are repeatable).
struct Quantizer {
    scale: f64,
    max: i32,
    state: u64,
}

impl Quantizer {
    fn new(bits: u32) -> Self {
        Self {
            scale: f64::from(1u32 << (bits - 1)),
            max: (1i32 << (bits - 1)) - 1,
            state: 0x9e37_79b9_7f4a_7c15,
        }
    }

    /// A uniform random number in [0, 1).
    fn random(&mut self) -> f64 {
        // xorshift64*
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        (self.state.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn sample(&mut self, x: f32) -> i32 {
        // Triangular noise of one step peak to peak each way.
        let dither = self.random() - self.random();
        let v = (f64::from(x) * self.scale + dither).round();
        v.clamp(f64::from(-self.max - 1), f64::from(self.max)) as i32
    }
}

fn write_wav(
    mut pipeline: Pipeline,
    spec: &ExportSpec,
    path: &Path,
) -> Result<ExportAnalysis, MediaError> {
    let wav_err = |source| MediaError::Wav {
        path: pipeline.path.clone(),
        source,
    };
    let bits = spec.sample.bits();
    let wav_spec = hound::WavSpec {
        channels: 2,
        sample_rate: spec.sample_rate,
        bits_per_sample: bits as u16,
        sample_format: match spec.sample {
            SampleFormat::Float32 => hound::SampleFormat::Float,
            _ => hound::SampleFormat::Int,
        },
    };
    let mut writer = hound::WavWriter::create(path, wav_spec).map_err(wav_err)?;
    let mut quantizer = Quantizer::new(bits.min(24));
    let (mut left, mut right) = (Vec::new(), Vec::new());
    loop {
        pipeline.pull(BLOCK, &mut left, &mut right)?;
        if left.is_empty() {
            break;
        }
        for (&l, &r) in left.iter().zip(&right) {
            for x in [l, r] {
                match spec.sample {
                    SampleFormat::Float32 => writer.write_sample(x),
                    _ => writer.write_sample(quantizer.sample(x)),
                }
                .map_err(|source| MediaError::Wav {
                    path: pipeline.path.clone(),
                    source,
                })?;
            }
        }
    }
    writer.finalize().map_err(|source| MediaError::Wav {
        path: pipeline.path.clone(),
        source,
    })?;
    Ok(pipeline.finish())
}

/// The pipeline as flacenc reads it.
struct FlacSource<'a, 'p> {
    pipeline: &'p mut Pipeline<'a>,
    quantizer: Quantizer,
    sample_rate: u32,
    bits: u32,
    buffers: (Vec<f32>, Vec<f32>, Vec<i32>),
    /// What went wrong, which flacenc's errors can't carry.
    error: Option<MediaError>,
}

impl flacenc::source::Source for FlacSource<'_, '_> {
    fn channels(&self) -> usize {
        2
    }

    fn bits_per_sample(&self) -> usize {
        self.bits as usize
    }

    fn sample_rate(&self) -> usize {
        self.sample_rate as usize
    }

    fn read_samples<F: flacenc::source::Fill>(
        &mut self,
        block_size: usize,
        dest: &mut F,
    ) -> Result<usize, flacenc::error::SourceError> {
        let (left, right, interleaved) = &mut self.buffers;
        if let Err(e) = self.pipeline.pull(block_size, left, right) {
            self.error = Some(e);
            return Err(flacenc::error::SourceError::by_reason(
                flacenc::error::SourceErrorReason::IO(None),
            ));
        }
        interleaved.clear();
        for (&l, &r) in left.iter().zip(right.iter()) {
            interleaved.push(self.quantizer.sample(l));
            interleaved.push(self.quantizer.sample(r));
        }
        dest.fill_interleaved(interleaved)?;
        Ok(left.len())
    }
}

fn write_flac(
    mut pipeline: Pipeline,
    spec: &ExportSpec,
    path: &Path,
) -> Result<ExportAnalysis, MediaError> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;
    let encode_err = |path: &Path, reason: String| MediaError::Encode {
        path: path.to_owned(),
        reason,
    };
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| encode_err(path, format!("{e:?}")))?;
    let mut source = FlacSource {
        pipeline: &mut pipeline,
        quantizer: Quantizer::new(spec.sample.bits()),
        sample_rate: spec.sample_rate,
        bits: spec.sample.bits(),
        buffers: (Vec::new(), Vec::new(), Vec::new()),
        error: None,
    };
    let stream = flacenc::encode_with_fixed_block_size(&config, &mut source, config.block_size);
    if let Some(error) = source.error.take() {
        return Err(error);
    }
    let stream = stream.map_err(|e| encode_err(path, e.to_string()))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| encode_err(path, format!("{e:?}")))?;
    let mut bytes = sink.as_slice().to_vec();
    fix_min_block_size(&mut bytes);
    std::fs::write(path, &bytes).map_err(|source| MediaError::Io {
        path: path.to_owned(),
        source,
    })?;
    Ok(pipeline.finish())
}

/// flacenc gives the last (shorter) block's length as the stream's minimum
/// block size, where FLAC means the shortest but for the last: with every
/// block the same length, it must equal the maximum, or readers (Symphonia)
/// take the stream for one of varying blocks and can't read its frames.
fn fix_min_block_size(bytes: &mut [u8]) {
    // "fLaC", the STREAMINFO block's header, then its minimum and maximum
    // block sizes (16 bits each).
    if bytes.len() >= 12 && bytes.starts_with(b"fLaC") && bytes[4] & 0x7f == 0 {
        let (min, max) = bytes[8..12].split_at_mut(2);
        min.copy_from_slice(max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode_file;

    /// A sine of `frames` frames at 48 kHz, in blocks of odd sizes.
    struct Sine {
        at: usize,
        frames: usize,
    }

    impl BlockSource for Sine {
        fn read(&mut self, left: &mut [f32], right: &mut [f32]) -> usize {
            let n = left.len().min(1000).min(self.frames - self.at);
            for i in 0..n {
                let t = (self.at + i) as f32 / 48_000.0;
                left[i] = (t * 440.0 * std::f32::consts::TAU).sin() * 0.5;
                right[i] = -left[i];
            }
            self.at += n;
            n
        }
    }

    fn spec(file: FileFormat, sample: SampleFormat, sample_rate: u32) -> ExportSpec {
        ExportSpec {
            file,
            sample,
            sample_rate,
        }
    }

    #[test]
    fn writes_wav_and_flac_that_decode_to_the_same_audio() {
        let dir = tempfile::tempdir().unwrap();
        let progress = ExportProgress::default();
        let mut outs = Vec::new();
        for (name, spec) in [
            (
                "a.wav",
                spec(FileFormat::Wav, SampleFormat::Float32, 48_000),
            ),
            ("b.wav", spec(FileFormat::Wav, SampleFormat::Int24, 48_000)),
            ("c.wav", spec(FileFormat::Wav, SampleFormat::Int16, 48_000)),
            (
                "d.flac",
                spec(FileFormat::Flac, SampleFormat::Int24, 48_000),
            ),
            (
                "e.flac",
                spec(FileFormat::Flac, SampleFormat::Int16, 48_000),
            ),
        ] {
            let path = dir.path().join(name);
            let mut sine = Sine {
                at: 0,
                frames: 48_000,
            };
            let analysis = export(&mut sine, 48_000, &spec, &path, &progress).unwrap();
            assert_eq!(analysis.frames, 48_000, "{name}");
            assert!((analysis.peak_dbfs[0].unwrap() + 6.02).abs() < 0.01);
            // Nothing left beside it.
            assert_eq!(
                std::fs::read_dir(dir.path()).unwrap().count(),
                outs.len() + 1
            );
            let decoded = decode_file(&path, 48_000).unwrap().audio;
            assert_eq!(decoded.frames(), 48_000, "{name}");
            outs.push((name, spec.sample, decoded));
        }
        let float = outs[0].2.channel(0).to_vec();
        for (name, sample, decoded) in &outs[1..] {
            // Rounding plus dither: within 1.5 steps of the sample size.
            let lsb = 1.0 / (1u32 << (sample.bits() - 1)) as f32;
            let worst = decoded
                .channel(0)
                .iter()
                .zip(&float)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f32::max);
            assert!(worst <= lsb * 1.5 + 1e-7, "{name}: {worst} vs {lsb}");
        }
        assert_eq!(progress.frames.load(Ordering::Relaxed), 5 * 48_000);
    }

    #[test]
    fn converts_the_sample_rate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cd.flac");
        let mut sine = Sine {
            at: 0,
            frames: 48_000,
        };
        let spec = spec(FileFormat::Flac, SampleFormat::Int16, 44_100);
        let analysis = export(&mut sine, 48_000, &spec, &path, &ExportProgress::default()).unwrap();
        assert_eq!(analysis.frames, 44_100);
        assert!((analysis.peak_dbfs[0].unwrap() + 6.02).abs() < 0.05);
        let decoded = decode_file(&path, 44_100).unwrap();
        assert_eq!(
            (decoded.source_rate, decoded.audio.frames()),
            (44_100, 44_100)
        );
    }

    #[test]
    fn a_cancelled_export_leaves_what_was_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mix.wav");
        std::fs::write(&path, b"before").unwrap();
        let progress = ExportProgress::default();
        progress.cancel.store(true, Ordering::Relaxed);
        let mut sine = Sine {
            at: 0,
            frames: 48_000,
        };
        let spec = spec(FileFormat::Wav, SampleFormat::Int16, 48_000);
        let err = export(&mut sine, 48_000, &spec, &path, &progress).unwrap_err();
        assert!(matches!(err, MediaError::Cancelled), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), b"before");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        // FLAC can't hold float samples.
        let spec = ExportSpec {
            file: FileFormat::Flac,
            ..spec
        };
        let float = ExportSpec {
            sample: SampleFormat::Float32,
            ..spec
        };
        assert!(float.check().is_err());
    }
}
