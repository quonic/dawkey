// SPDX-License-Identifier: AGPL-3.0-or-later
//! The disk side of recording: drains the real-time thread's rings into WAV
//! files. A contiguous run of frames is one take; a jump in timeline
//! position (a loop pass, a punch) starts the next take.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use dk_core::{InputChannels, TrackId};
use dk_engine::{RecordShared, RecordTap};

/// How long the disk thread sleeps when there is nothing to write.
const IDLE: Duration = Duration::from_millis(5);

/// One recorded file.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Take {
    pub(crate) track: TrackId,
    /// Timeline frame of the first recorded frame (before latency
    /// compensation).
    pub(crate) start: u64,
    pub(crate) frames: u64,
    pub(crate) path: PathBuf,
}

/// A track being recorded.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub(crate) track: TrackId,
    pub(crate) name: String,
    pub(crate) channels: InputChannels,
}

pub(crate) struct Recorder {
    thread: Option<JoinHandle<Result<Vec<Take>, String>>>,
    shared: Arc<RecordShared>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder").finish_non_exhaustive()
    }
}

struct Segment {
    start: u64,
    frames: u64,
    path: PathBuf,
    writer: hound::WavWriter<BufWriter<File>>,
}

impl Recorder {
    /// Starts the disk thread. `taps` match `targets` one to one.
    pub(crate) fn start(
        targets: Vec<Target>,
        taps: Vec<RecordTap>,
        shared: Arc<RecordShared>,
        dir: PathBuf,
        sample_rate: u32,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("dawkey-recorder".into())
            .spawn(move || run(targets, taps, &thread_shared, &dir, sample_rate))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            thread: Some(thread),
            shared,
        })
    }

    /// Waits for the real-time thread to finish (up to `wait`, then assumes
    /// it has stopped writing) and for the files to be closed.
    pub(crate) fn finish(mut self, wait: Duration) -> Result<Vec<Take>, String> {
        crate::wait_finished(&self.shared, wait);
        self.thread
            .take()
            .expect("finished once")
            .join()
            .map_err(|_| "the recorder thread panicked".to_owned())?
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.shared.finish();
            let _ = thread.join();
        }
    }
}

fn run(
    targets: Vec<Target>,
    mut taps: Vec<RecordTap>,
    shared: &RecordShared,
    dir: &Path,
    sample_rate: u32,
) -> Result<Vec<Take>, String> {
    let mut current: Vec<Option<Segment>> = targets.iter().map(|_| None).collect();
    let mut takes = Vec::new();
    let mut buffer = Vec::new();
    loop {
        // Everything written before `finished` is visible once we see it.
        let finished = shared.finished();
        let mut idle = true;
        for (i, tap) in taps.iter_mut().enumerate() {
            let target = &targets[i];
            let channels = usize::from(target.channels.count());
            while let Ok(mark) = tap.marks.pop() {
                idle = false;
                buffer.clear();
                for _ in 0..mark.frames as usize * channels {
                    buffer.push(tap.samples.pop().unwrap_or(0.0));
                }
                let continues = current[i]
                    .as_ref()
                    .is_some_and(|s| s.start + s.frames == mark.start);
                if !continues {
                    if let Some(done) = current[i].take() {
                        takes.push(close(target.track, done)?);
                    }
                    current[i] = Some(open(dir, target, mark.start, sample_rate)?);
                }
                let segment = current[i].as_mut().expect("opened above");
                for &sample in &buffer {
                    segment
                        .writer
                        .write_sample(sample)
                        .map_err(|e| format!("{}: {e}", segment.path.display()))?;
                }
                segment.frames += u64::from(mark.frames);
            }
        }
        if finished {
            break;
        }
        if idle {
            std::thread::sleep(IDLE);
        }
    }
    for (i, segment) in current.into_iter().enumerate() {
        if let Some(done) = segment {
            takes.push(close(targets[i].track, done)?);
        }
    }
    Ok(takes)
}

fn open(dir: &Path, target: &Target, start: u64, sample_rate: u32) -> Result<Segment, String> {
    let path = unique_path(dir, &target.name);
    let spec = hound::WavSpec {
        channels: target.channels.count(),
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let writer =
        hound::WavWriter::create(&path, spec).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Segment {
        start,
        frames: 0,
        path,
        writer,
    })
}

fn close(track: TrackId, segment: Segment) -> Result<Take, String> {
    segment
        .writer
        .finalize()
        .map_err(|e| format!("{}: {e}", segment.path.display()))?;
    Ok(Take {
        track,
        start: segment.start,
        frames: segment.frames,
        path: segment.path,
    })
}

/// `<dir>/<track name> take <n>.wav`, with the first unused `n`.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = if safe.trim().is_empty() {
        "Track"
    } else {
        safe.trim()
    };
    (1..)
        .map(|n| dir.join(format!("{safe} take {n}.wav")))
        .find(|p| !p.exists())
        .expect("unbounded range")
}
