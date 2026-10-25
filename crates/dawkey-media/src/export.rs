// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exporting a mix to a file: stereo audio pulled a block at a time from a
//! [`BlockSource`] (an offline render), converted to the file's sample rate,
//! analyzed, dithered to whole-number samples (TPDF) and written as WAV
//! (16 or 24-bit, or 32-bit float; RF64 past 4 GiB) or FLAC (16 or 24-bit).
//! Nothing has to fit in memory but a FLAC file's compressed frames. The
//! file is written beside its target and renamed at the end, so a failed or
//! cancelled export leaves what was there.

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
    /// Ogg Vorbis (lossy, `quality`).
    Ogg,
    /// Ogg Opus (lossy, `bitrate_kbps`; always 48 kHz).
    Opus,
}

impl FileFormat {
    /// The format a file name's extension asks for.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "wav" | "wave" => Some(Self::Wav),
            "flac" => Some(Self::Flac),
            "ogg" | "oga" => Some(Self::Ogg),
            "opus" => Some(Self::Opus),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Ogg => "ogg",
            Self::Opus => "opus",
        }
    }

    /// Whether the format stores the samples as given (no, for Vorbis and
    /// Opus, which have a quality setting instead of a sample format).
    pub fn lossless(self) -> bool {
        matches!(self, Self::Wav | Self::Flac)
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

/// Ogg Vorbis quality by default, on oggenc's 0 to 10 scale.
pub const DEFAULT_VORBIS_QUALITY: f32 = 5.0;
/// Opus bitrate by default, in kbit/s.
pub const DEFAULT_OPUS_KBPS: u32 = 128;
/// Opus files are always at this rate.
pub const OPUS_RATE: u32 = 48_000;

/// What an export writes.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSpec {
    pub file: FileFormat,
    /// For WAV and FLAC; lossy formats take floats and ignore it.
    pub sample: SampleFormat,
    /// The file's sample rate; the audio is converted to it. Opus files are
    /// always 48 kHz (see [`file_rate`](Self::file_rate)).
    pub sample_rate: u32,
    /// Ogg Vorbis: the quality, 0 to 10 (oggenc's scale; about 64 to 500
    /// kbit/s for stereo).
    pub quality: f32,
    /// Opus: the bitrate in kbit/s, 6 to 510.
    pub bitrate_kbps: u32,
    /// Tags for formats that carry them (Vorbis comments): `TITLE`, `ARTIST`,
    /// `ALBUM`, ...; names are stored upper case.
    pub tags: Vec<(String, String)>,
}

impl ExportSpec {
    /// A spec with the default quality, bitrate and no tags.
    pub fn new(file: FileFormat, sample: SampleFormat, sample_rate: u32) -> Self {
        Self {
            file,
            sample,
            sample_rate,
            quality: DEFAULT_VORBIS_QUALITY,
            bitrate_kbps: DEFAULT_OPUS_KBPS,
            tags: Vec::new(),
        }
    }

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
        if self.file == FileFormat::Ogg && self.sample_rate > 192_000 {
            return Err("Ogg Vorbis stores up to 192000 Hz".into());
        }
        // The format goes higher, but flacenc doesn't.
        if self.file == FileFormat::Flac && self.sample_rate > 96_000 {
            return Err(format!(
                "FLAC exports go up to 96000 Hz, not {} (WAV goes higher)",
                self.sample_rate
            ));
        }
        if !(0.0..=10.0).contains(&self.quality) {
            return Err(format!("quality is 0 to 10, not {}", self.quality));
        }
        if !(6..=510).contains(&self.bitrate_kbps) {
            return Err(format!(
                "bitrate_kbps is 6 to 510, not {}",
                self.bitrate_kbps
            ));
        }
        for (name, _) in &self.tags {
            if name.is_empty() || name.chars().any(|c| c == '=' || !(' '..='}').contains(&c)) {
                return Err(format!("{name:?} isn't a tag name (letters, no '=')"));
            }
        }
        Ok(())
    }

    /// The rate the file is at: `sample_rate`, except Opus is always 48 kHz.
    pub fn file_rate(&self) -> u32 {
        match self.file {
            FileFormat::Opus => OPUS_RATE,
            _ => self.sample_rate,
        }
    }
}

/// Stereo audio, a block at a time.
pub trait BlockSource {
    /// Fills as much of `left` and `right` as it can (they're the same
    /// length); returns how many frames. 0 once there's no more.
    fn read(&mut self, left: &mut [f32], right: &mut [f32]) -> usize;
}

/// Stereo audio in memory, as a block source (for exporting a render that
/// was put together in memory, like a wrapped loop).
#[derive(Debug)]
pub struct MemorySource {
    left: Vec<f32>,
    right: Vec<f32>,
    at: usize,
}

impl MemorySource {
    /// `channels` is one or two planes of the same length (one is mono,
    /// played on both sides).
    pub fn new(mut channels: Vec<Vec<f32>>) -> Self {
        let right = channels.pop().unwrap_or_default();
        let left = channels.pop().unwrap_or_else(|| right.clone());
        Self { left, right, at: 0 }
    }
}

impl BlockSource for MemorySource {
    fn read(&mut self, left: &mut [f32], right: &mut [f32]) -> usize {
        let n = left.len().min(self.left.len() - self.at);
        left[..n].copy_from_slice(&self.left[self.at..self.at + n]);
        right[..n].copy_from_slice(&self.right[self.at..self.at + n]);
        self.at += n;
        n
    }
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
            let frame = |s: f64| (s.max(0.0) * f64::from(spec.file_rate())).round() as usize;
            (frame(start), frame(end))
        })
        .collect();
    pipeline.segments = SegmentAnalyzers::new(2, spec.file_rate(), &frames_at_file_rate);
    let result = match spec.file {
        FileFormat::Wav => write_wav(pipeline, spec, &partial),
        FileFormat::Flac => write_flac(pipeline, spec, &partial),
        FileFormat::Ogg => write_vorbis(pipeline, spec, &partial),
        FileFormat::Opus => write_opus(pipeline, spec, &partial),
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
        let resampler = (source_rate != spec.file_rate())
            .then(|| StreamResampler::new(source_rate, spec.file_rate()))
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
            analyzer: Analyzer::new(2, spec.file_rate()),
            segments: SegmentAnalyzers::new(2, spec.file_rate(), &[]),
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
    /// The rates, for working out how long the output is.
    from: u32,
    to: u32,
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
            from,
            to,
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
        // In whole numbers: the float ratio can round a frame too many.
        let expected = (self.frames_in * u64::from(self.to)).div_ceil(u64::from(self.from));
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
    use std::io::{Seek, Write};

    let target = pipeline.path.clone();
    let io_err = |source| MediaError::Io {
        path: target.clone(),
        source,
    };
    let file = std::fs::File::create(path).map_err(io_err)?;
    let mut writer = std::io::BufWriter::new(file);
    let mut header = wav_header(spec.sample, spec.sample_rate);
    writer.write_all(&header).map_err(io_err)?;
    let mut quantizer = Quantizer::new(spec.sample.bits().min(24));
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut bytes = Vec::new();
    let mut frames = 0u64;
    loop {
        pipeline.pull(BLOCK, &mut left, &mut right)?;
        if left.is_empty() {
            break;
        }
        bytes.clear();
        for (&l, &r) in left.iter().zip(&right) {
            for x in [l, r] {
                match spec.sample {
                    SampleFormat::Float32 => bytes.extend_from_slice(&x.to_le_bytes()),
                    SampleFormat::Int16 => {
                        bytes.extend_from_slice(&(quantizer.sample(x) as i16).to_le_bytes());
                    }
                    SampleFormat::Int24 => {
                        bytes.extend_from_slice(&quantizer.sample(x).to_le_bytes()[..3]);
                    }
                }
            }
        }
        writer.write_all(&bytes).map_err(io_err)?;
        frames += left.len() as u64;
    }
    set_wav_length(&mut header, spec.sample, frames);
    writer.seek(std::io::SeekFrom::Start(0)).map_err(io_err)?;
    writer.write_all(&header).map_err(io_err)?;
    writer.flush().map_err(io_err)?;
    Ok(pipeline.finish())
}

/// Where the `JUNK` chunk that makes room for RF64's `ds64` starts, and how
/// long its body is: the RIFF and data sizes and the frame count (64 bits
/// each), then an empty table's length.
const WAV_DS64_AT: usize = 12;
const WAV_DS64_LEN: usize = 28;

/// A stereo WAV header for `sample`s at `rate`, its lengths still to be set
/// ([`set_wav_length`]). Like hound's: PCM for 16-bit, extensible for more.
/// After `WAVE` comes a `JUNK` chunk (which readers skip), room for the
/// `ds64` chunk a file past 4 GiB needs (RF64, EBU Tech 3306).
fn wav_header(sample: SampleFormat, rate: u32) -> Vec<u8> {
    let bytes = (sample.bits() / 8) as u16;
    let extensible = sample != SampleFormat::Int16;
    let mut h = Vec::with_capacity(84);
    h.extend_from_slice(b"RIFF\0\0\0\0WAVE");
    h.extend_from_slice(b"JUNK");
    h.extend_from_slice(&(WAV_DS64_LEN as u32).to_le_bytes());
    h.resize(h.len() + WAV_DS64_LEN, 0);
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&(if extensible { 40u32 } else { 16 }).to_le_bytes());
    h.extend_from_slice(&(if extensible { 0xfffe_u16 } else { 1 }).to_le_bytes());
    h.extend_from_slice(&2u16.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * u32::from(bytes) * 2).to_le_bytes());
    h.extend_from_slice(&(bytes * 2).to_le_bytes());
    h.extend_from_slice(&(bytes * 8).to_le_bytes());
    if extensible {
        // cbSize, the valid bits, the channel mask (front left and right),
        // then the subformat's GUID: PCM (1) or IEEE float (3).
        h.extend_from_slice(&22u16.to_le_bytes());
        h.extend_from_slice(&(bytes * 8).to_le_bytes());
        h.extend_from_slice(&3u32.to_le_bytes());
        let format = if sample == SampleFormat::Float32 {
            3
        } else {
            1
        };
        h.extend_from_slice(&[
            format, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
        ]);
    }
    h.extend_from_slice(b"data\0\0\0\0");
    h
}

/// Sets a [`wav_header`]'s lengths for `frames` frames. Past 4 GiB the
/// 32-bit sizes can't hold them, so the file becomes RF64: its `JUNK` chunk
/// turns into the `ds64` chunk with the real sizes, and the 32-bit ones are
/// all ones.
fn set_wav_length(header: &mut [u8], sample: SampleFormat, frames: u64) {
    let data = frames * 2 * u64::from(sample.bits() / 8);
    let riff = (header.len() - 8) as u64 + data;
    let at = header.len() - 4;
    match (u32::try_from(riff), u32::try_from(data)) {
        (Ok(riff), Ok(data)) => {
            header[4..8].copy_from_slice(&riff.to_le_bytes());
            header[at..].copy_from_slice(&data.to_le_bytes());
        }
        _ => {
            header[..4].copy_from_slice(b"RF64");
            header[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
            header[WAV_DS64_AT..WAV_DS64_AT + 4].copy_from_slice(b"ds64");
            let ds64 = &mut header[WAV_DS64_AT + 8..WAV_DS64_AT + 8 + WAV_DS64_LEN];
            ds64[..8].copy_from_slice(&riff.to_le_bytes());
            ds64[8..16].copy_from_slice(&data.to_le_bytes());
            ds64[16..24].copy_from_slice(&frames.to_le_bytes());
            ds64[24..].fill(0);
            header[at..].copy_from_slice(&u32::MAX.to_le_bytes());
        }
    }
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
    let target = pipeline.path.clone();
    let encode_err = |reason: String| MediaError::Encode {
        path: target.clone(),
        reason,
    };
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| encode_err(format!("{e:?}")))?;
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
    let stream = stream.map_err(|e| encode_err(e.to_string()))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| encode_err(format!("{e:?}")))?;
    let mut bytes = sink.as_slice().to_vec();
    fix_min_block_size(&mut bytes);
    std::fs::write(path, &bytes).map_err(|source| MediaError::Io {
        path: target.clone(),
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

/// Ogg Vorbis, at `spec.quality` on the 0 to 10 scale, with the tags as
/// Vorbis comments.
fn write_vorbis(
    mut pipeline: Pipeline,
    spec: &ExportSpec,
    path: &Path,
) -> Result<ExportAnalysis, MediaError> {
    use std::num::{NonZeroU8, NonZeroU32};

    use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};

    let target = pipeline.path.clone();
    let encode_err = |e: vorbis_rs::VorbisError| MediaError::Encode {
        path: target.clone(),
        reason: e.to_string(),
    };
    let io_err = |source| MediaError::Io {
        path: target.clone(),
        source,
    };
    let file = std::fs::File::create(path).map_err(io_err)?;
    let rate = NonZeroU32::new(spec.sample_rate).expect("checked");
    let channels = NonZeroU8::new(2).expect("two");
    let mut builder = VorbisEncoderBuilder::new(rate, channels, std::io::BufWriter::new(file))
        .map_err(encode_err)?;
    // oggenc's -q 0..10 is libvorbis's 0..1 (its -0.2 to 0 is below q 0).
    builder.bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr {
        target_quality: (spec.quality / 10.0).clamp(0.0, 1.0),
    });
    for (name, value) in &spec.tags {
        builder
            .comment_tag(name.to_ascii_uppercase(), value.clone())
            .map_err(encode_err)?;
    }
    let mut encoder = builder.build().map_err(encode_err)?;
    let (mut left, mut right) = (Vec::new(), Vec::new());
    loop {
        pipeline.pull(BLOCK, &mut left, &mut right)?;
        if left.is_empty() {
            break;
        }
        encoder
            .encode_audio_block([&left, &right])
            .map_err(encode_err)?;
    }
    let mut out = encoder.finish().map_err(encode_err)?;
    std::io::Write::flush(&mut out).map_err(io_err)?;
    Ok(pipeline.finish())
}

/// Ogg Opus at `spec.bitrate_kbps`, 48 kHz, 20 ms packets, with the tags.
/// The file plays back exactly as many frames as went in: the encoder's
/// delay is skipped (pre-skip) and the last packet is trimmed.
fn write_opus(
    mut pipeline: Pipeline,
    spec: &ExportSpec,
    path: &Path,
) -> Result<ExportAnalysis, MediaError> {
    use opus_pure::{
        Application, MAX_PACKET_BYTES, OggOpusWriter, OpusEncoder, OpusHead, OpusTags,
    };

    /// Frames per packet: 20 ms at 48 kHz.
    const FRAME: usize = 960;

    let target = pipeline.path.clone();
    let encode_err = |e: opus_pure::Error| MediaError::Encode {
        path: target.clone(),
        reason: e.to_string(),
    };
    let io_err = |source| MediaError::Io {
        path: target.clone(),
        source,
    };
    let mut encoder =
        OpusEncoder::new(OPUS_RATE as i32, 2, Application::Audio).map_err(encode_err)?;
    encoder.bitrate_bps = (spec.bitrate_kbps * 1000) as i32;
    let head = OpusHead::for_encoder(&encoder, spec.sample_rate);
    let pre_skip = usize::from(head.pre_skip);
    let mut tags = OpusTags::new();
    for (name, value) in &spec.tags {
        tags.push(&name.to_ascii_uppercase(), value)
            .map_err(encode_err)?;
    }
    let file = std::fs::File::create(path).map_err(io_err)?;
    let mut writer =
        OggOpusWriter::with_tags(std::io::BufWriter::new(file), head, tags).map_err(encode_err)?;

    // Frames go in 960 at a time; the encoder's delay comes out as pre-skip
    // at the start, so that much silence goes in at the end to push the
    // last of the audio out, and the final packet's granule is cut to what
    // was really there.
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut interleaved = vec![0.0f32; FRAME * 2];
    let mut packet = vec![0u8; MAX_PACKET_BYTES];
    let mut pending: (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
    let mut frames_in = 0usize;
    let mut ended = false;
    let mut encode = |l: &[f32], r: &[f32], real: usize, writer: &mut OggOpusWriter<_>| {
        for (i, (&a, &b)) in l.iter().zip(r).enumerate() {
            interleaved[2 * i] = a;
            interleaved[2 * i + 1] = b;
        }
        let n = encoder
            .encode(&interleaved, FRAME, &mut packet)
            .map_err(encode_err)?;
        if real < FRAME {
            writer
                .write_packet_with_duration(&packet[..n], real as u32)
                .map_err(encode_err)
        } else {
            writer.write_packet(&packet[..n]).map_err(encode_err)
        }
    };
    loop {
        if !ended {
            pipeline.pull(BLOCK, &mut left, &mut right)?;
            if left.is_empty() {
                ended = true;
                // The delay's worth of silence flushes the audio.
                left.resize(pre_skip, 0.0);
                right.resize(pre_skip, 0.0);
            } else {
                frames_in += left.len();
            }
            pending.0.extend_from_slice(&left);
            pending.1.extend_from_slice(&right);
        }
        while pending.0.len() >= FRAME {
            let l: Vec<f32> = pending.0.drain(..FRAME).collect();
            let r: Vec<f32> = pending.1.drain(..FRAME).collect();
            encode(&l, &r, FRAME, &mut writer)?;
        }
        if ended {
            break;
        }
    }
    if !pending.0.is_empty() || frames_in == 0 {
        // The rest, padded to a packet; its granule says how much was real.
        let real = pending.0.len();
        pending.0.resize(FRAME, 0.0);
        pending.1.resize(FRAME, 0.0);
        encode(&pending.0, &pending.1, real, &mut writer)?;
    }
    let mut out = writer.finish().map_err(encode_err)?;
    std::io::Write::flush(&mut out).map_err(io_err)?;
    Ok(pipeline.finish())
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
        ExportSpec::new(file, sample, sample_rate)
    }

    #[test]
    fn writes_ogg_vorbis_and_opus() {
        let dir = tempfile::tempdir().unwrap();
        let progress = ExportProgress::default();
        let tags = vec![
            ("title".to_owned(), "Loop".to_owned()),
            ("ARTIST".to_owned(), "Dawkey".to_owned()),
        ];

        // Vorbis: the right length and level, with the tags.
        let path = dir.path().join("a.ogg");
        let mut vorbis = spec(FileFormat::Ogg, SampleFormat::Int24, 48_000);
        vorbis.tags = tags.clone();
        vorbis.quality = 6.0;
        let mut sine = Sine {
            at: 0,
            frames: 48_000,
        };
        let analysis = export(&mut sine, 48_000, &vorbis, &path, &progress).unwrap();
        assert_eq!(analysis.frames, 48_000);
        let decoded = decode_file(&path, 48_000).unwrap();
        assert_eq!(decoded.source_rate, 48_000);
        assert_eq!(decoded.audio.frames(), 48_000);
        let peak = crate::analyze(decoded.audio.channels(), 48_000).peak_dbfs[0].unwrap();
        assert!((peak + 6.02).abs() < 0.5, "{peak}");
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"OggS"));
        assert!(bytes.windows(5).any(|w| w == b"TITLE"));

        // Opus: always 48 kHz, exactly as many frames as went in (the
        // encoder's delay skipped, the end trimmed), with the tags.
        let path = dir.path().join("b.opus");
        let mut opus = spec(FileFormat::Opus, SampleFormat::Int24, 44_100);
        opus.tags = tags;
        opus.bitrate_kbps = 96;
        let mut sine = Sine {
            at: 0,
            frames: 48_000,
        };
        let analysis = export(&mut sine, 48_000, &opus, &path, &progress).unwrap();
        assert_eq!(analysis.frames, 48_000, "the analysis is at 48 kHz");
        let file = std::fs::File::open(&path).unwrap();
        let mut reader = opus_pure::OggOpusReader::new(std::io::BufReader::new(file)).unwrap();
        assert_eq!(reader.head().input_sample_rate, 44_100);
        assert_eq!(reader.tags().get("TITLE"), Some("Loop"));
        assert_eq!(reader.tags().get("ARTIST"), Some("Dawkey"));
        let mut decoder = reader.head().decoder(48_000).unwrap();
        let mut trim = opus_pure::Trim::new(reader.head(), 48_000, 2).unwrap();
        let mut out = vec![0.0f32; 5760 * 2];
        let mut samples = Vec::new();
        while let Some(packet) = reader.read_packet().unwrap() {
            let n = decoder.decode(&packet.data, 5760, &mut out).unwrap();
            samples.extend_from_slice(trim.keep(&packet, &out[..n * 2]));
        }
        assert_eq!(samples.len(), 48_000 * 2);
        let left: Vec<f32> = samples.iter().step_by(2).copied().collect();
        let peak = crate::analyze(&[left], 48_000).peak_dbfs[0].unwrap();
        assert!((peak + 6.02).abs() < 0.5, "{peak}");

        // Bad settings are caught first.
        let mut bad = opus.clone();
        bad.bitrate_kbps = 1;
        assert!(bad.check().unwrap_err().contains("bitrate_kbps"));
        let mut bad = opus.clone();
        bad.quality = 11.0;
        assert!(bad.check().unwrap_err().contains("quality"));
        let mut bad = opus;
        bad.tags = vec![("a=b".to_owned(), "x".to_owned())];
        assert!(bad.check().unwrap_err().contains("tag name"));
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
    fn flac_goes_up_to_96k() {
        let dir = tempfile::tempdir().unwrap();
        for rate in [8_000, 12_345, 65_536, 96_000] {
            let path = dir.path().join(format!("{rate}.flac"));
            let mut sine = Sine {
                at: 0,
                frames: 12_000,
            };
            let spec = spec(FileFormat::Flac, SampleFormat::Int24, rate);
            export(&mut sine, rate, &spec, &path, &ExportProgress::default()).unwrap();
            let decoded = decode_file(&path, rate).unwrap();
            assert_eq!(
                (decoded.source_rate, decoded.audio.frames()),
                (rate, 12_000)
            );
        }
        // Past that the encoder can't, so it's refused before rendering.
        let path = dir.path().join("hi.flac");
        let spec = spec(FileFormat::Flac, SampleFormat::Int24, 176_400);
        let mut sine = Sine {
            at: 0,
            frames: 12_000,
        };
        let progress = ExportProgress::default();
        let err = export(&mut sine, 48_000, &spec, &path, &progress).unwrap_err();
        assert!(
            matches!(&err, MediaError::Unsupported { path: p, reason }
                if p == &path && reason.contains("96000")),
            "{err}"
        );
        assert_eq!(progress.frames.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_second_at_44k1_is_a_second_at_48k() {
        // 48000 / 44100 * 44100 is a hair over 48000 in floats.
        let dir = tempfile::tempdir().unwrap();
        for (name, file) in [("a.wav", FileFormat::Wav), ("a.opus", FileFormat::Opus)] {
            let path = dir.path().join(name);
            let mut sine = Sine {
                at: 0,
                frames: 44_100,
            };
            let spec = spec(file, SampleFormat::Float32, 48_000);
            let analysis =
                export(&mut sine, 44_100, &spec, &path, &ExportProgress::default()).unwrap();
            assert_eq!(analysis.frames, 48_000, "{name}");
        }
    }

    #[test]
    fn wav_headers_and_rf64() {
        // An export reads back with hound too (Symphonia reads them above),
        // the JUNK chunk skipped.
        let dir = tempfile::tempdir().unwrap();
        for sample in [
            SampleFormat::Int16,
            SampleFormat::Int24,
            SampleFormat::Float32,
        ] {
            let path = dir.path().join("a.wav");
            let mut sine = Sine {
                at: 0,
                frames: 1001,
            };
            let spec = spec(FileFormat::Wav, sample, 44_100);
            export(&mut sine, 44_100, &spec, &path, &ExportProgress::default()).unwrap();
            let reader = hound::WavReader::open(&path).unwrap();
            let got = reader.spec();
            assert_eq!(
                (
                    got.channels,
                    got.sample_rate,
                    u32::from(got.bits_per_sample)
                ),
                (2, 44_100, sample.bits())
            );
            assert_eq!(reader.duration(), 1001);
            let len = std::fs::metadata(&path).unwrap().len();
            let riff = u32::from_le_bytes(std::fs::read(&path).unwrap()[4..8].try_into().unwrap());
            assert_eq!(u64::from(riff), len - 8);
        }

        // Six hours of 24-bit stereo at 48 kHz is past what a WAV's 32-bit
        // sizes hold: it becomes RF64, the real sizes in its ds64 chunk.
        let mut header = wav_header(SampleFormat::Int24, 48_000);
        let frames = 6 * 3600 * 48_000u64;
        set_wav_length(&mut header, SampleFormat::Int24, frames);
        let u32_at = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
        let u64_at = |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().unwrap());
        assert_eq!(&header[..4], b"RF64");
        assert_eq!(u32_at(4), u32::MAX);
        assert_eq!(&header[12..16], b"ds64");
        assert_eq!(u32_at(16), 28);
        let data = frames * 6;
        assert_eq!(u64_at(20), header.len() as u64 - 8 + data);
        assert_eq!(u64_at(28), data);
        assert_eq!(u64_at(36), frames);
        assert_eq!(&header[header.len() - 8..header.len() - 4], b"data");
        assert_eq!(u32_at(header.len() - 4), u32::MAX);
        // Just under the limit, it stays a WAV.
        let mut header = wav_header(SampleFormat::Float32, 48_000);
        let frames = (u64::from(u32::MAX) - header.len() as u64 + 8) / 8;
        set_wav_length(&mut header, SampleFormat::Float32, frames);
        assert_eq!(&header[..4], b"RIFF");
        assert_eq!(&header[12..16], b"JUNK");
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
