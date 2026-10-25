// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exporting the mix from the window: the toolbar's "Export…" (or Ctrl+E)
//! opens a dialog of settings (file format, sample size, sample rate, the
//! song or the loop range, a tail), its "Export…" asks the platform where
//! to (a save dialog), and [`App::start_export`] renders and writes the file
//! on a worker thread while the status bar shows how far it is, with a
//! button to cancel. [`App::tick`] picks the result up.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;

use dawkey_media::{Analysis, ExportProgress, ExportSpec, FileFormat, MediaError, SampleFormat};
use dawkey_studio::RenderLeftovers;

use crate::App;
use crate::state::{ExportRange, PlatformRequest};

/// Sample rates the dialog offers besides the project's.
const RATES: [u32; 4] = [44_100, 48_000, 88_200, 96_000];
/// Tails the dialog offers, in seconds.
const TAILS: [f64; 5] = [0.0, 1.0, 2.0, 5.0, 10.0];
/// Ogg Vorbis qualities the dialog offers.
const QUALITIES: [f32; 6] = [2.0, 4.0, 5.0, 6.0, 8.0, 10.0];
/// Opus bitrates the dialog offers, in kbit/s.
const BITRATES: [u32; 6] = [64, 96, 128, 160, 192, 256];

/// An export running on a worker thread.
pub(crate) struct RunningExport {
    pub(crate) path: PathBuf,
    pub(crate) progress: Arc<ExportProgress>,
    /// Frames it renders, at the project's rate.
    pub(crate) frames: usize,
    done: mpsc::Receiver<(Result<Analysis, MediaError>, RenderLeftovers)>,
}

impl App {
    /// Clicks on the export dialog and the status bar's cancel button.
    /// `None` if `parts` isn't one of theirs.
    pub(crate) fn on_export_click(&mut self, parts: &[&str]) -> Option<Result<(), String>> {
        let settings = &mut self.view.export;
        Some(match parts {
            ["toolbar", "export"] => {
                self.open_export_dialog();
                Ok(())
            }
            ["export"] => Ok(()),
            ["export", "cancel"] | ["export", "backdrop"] => {
                self.view.export_dialog = false;
                Ok(())
            }
            ["export", "format"] => {
                settings.format = match settings.format {
                    FileFormat::Wav => FileFormat::Flac,
                    FileFormat::Flac => FileFormat::Ogg,
                    FileFormat::Ogg => FileFormat::Opus,
                    FileFormat::Opus => FileFormat::Wav,
                };
                // FLAC holds whole numbers only.
                if settings.format == FileFormat::Flac && settings.sample == SampleFormat::Float32 {
                    settings.sample = SampleFormat::Int24;
                }
                Ok(())
            }
            // For the lossy formats this row is the quality instead.
            ["export", "bits"] => {
                match settings.format {
                    FileFormat::Ogg => settings.quality = next_in(&QUALITIES, settings.quality),
                    FileFormat::Opus => {
                        settings.bitrate_kbps = next_in(&BITRATES, settings.bitrate_kbps)
                    }
                    _ => {
                        settings.sample = match (settings.sample, settings.format) {
                            (SampleFormat::Int16, _) => SampleFormat::Int24,
                            (SampleFormat::Int24, FileFormat::Wav) => SampleFormat::Float32,
                            _ => SampleFormat::Int16,
                        }
                    }
                }
                Ok(())
            }
            ["export", "rate"] => {
                // The project's rate, then the others in turn.
                settings.sample_rate = match settings.sample_rate {
                    None => Some(RATES[0]),
                    Some(rate) => RATES
                        .iter()
                        .position(|&r| r == rate)
                        .and_then(|i| RATES.get(i + 1).copied()),
                };
                Ok(())
            }
            ["export", "range"] => {
                let loop_range = self.studio.session().project().loop_region.range;
                let has_loop = loop_range.is_some_and(|r| !r.is_empty());
                settings.range = match settings.range {
                    ExportRange::Song if has_loop => ExportRange::Loop,
                    _ => ExportRange::Song,
                };
                Ok(())
            }
            ["export", "tail"] => {
                let next = TAILS
                    .iter()
                    .position(|&t| t == settings.tail_s)
                    .map_or(0, |i| i + 1);
                settings.tail_s = TAILS[next % TAILS.len()];
                Ok(())
            }
            ["export", "start"] => self.choose_export_location(),
            ["status", "export_cancel"] => {
                if let Some(export) = &self.export {
                    export.progress.cancel.store(true, Ordering::Relaxed);
                }
                Ok(())
            }
            _ => return None,
        })
    }

    /// Keys while the export dialog is open: Return exports, Escape closes
    /// it. `None` if it's closed.
    pub(crate) fn on_export_key(&mut self, key: &str) -> Option<Result<(), String>> {
        if !self.view.export_dialog {
            return None;
        }
        Some(match key {
            "return" => self.choose_export_location(),
            "escape" => {
                self.view.export_dialog = false;
                Ok(())
            }
            _ => Ok(()),
        })
    }

    pub(crate) fn open_export_dialog(&mut self) {
        self.close_overlays();
        self.view.export_dialog = true;
    }

    /// The dialog's "Export…": closes it and asks where to.
    fn choose_export_location(&mut self) -> Result<(), String> {
        self.export_range()?;
        if self.export.is_some() {
            return Err("an export is already running".into());
        }
        self.view.export_dialog = false;
        let format = self.view.export.format;
        let name = format!(
            "{}.{}",
            self.studio.session().project().name,
            format.extension()
        );
        self.platform_requests
            .push(PlatformRequest::ChooseExportLocation { name, format });
        Ok(())
    }

    /// The range the settings cover, in seconds, tail included.
    fn export_range(&self) -> Result<(f64, f64), String> {
        let project = self.studio.session().project();
        let settings = &self.view.export;
        let (start, end) = match settings.range {
            ExportRange::Song => (0, project.end()),
            // The loop range, whether looping is on or not.
            ExportRange::Loop => project
                .loop_region
                .range
                .map_or((0, 0), |r| (r.start, r.end)),
        };
        if end <= start {
            return Err(match settings.range {
                ExportRange::Song => "there's nothing to export: no clips play".into(),
                ExportRange::Loop => "set a loop range to export it".into(),
            });
        }
        Ok((
            project.frames_to_seconds(start),
            project.frames_to_seconds(end) + settings.tail_s,
        ))
    }

    /// Exports the mix to `path` as the export settings say, on a worker
    /// thread; [`tick`](Self::tick) finishes it.
    pub fn start_export(&mut self, path: &Path) -> Result<(), String> {
        if self.export.is_some() {
            return Err("an export is already running".into());
        }
        let (start_s, end_s) = self.export_range()?;
        let settings = self.view.export;
        let spec = ExportSpec {
            quality: settings.quality,
            bitrate_kbps: settings.bitrate_kbps,
            tags: vec![(
                "TITLE".to_owned(),
                self.studio.session().project().name.clone(),
            )],
            ..ExportSpec::new(
                FileFormat::from_path(path).unwrap_or(settings.format),
                settings.sample,
                settings
                    .sample_rate
                    .unwrap_or(self.studio.session().project().sample_rate),
            )
        };
        spec.check()?;
        let job = self
            .studio
            .export_job(start_s, end_s)
            .map_err(|e| e.to_string())?;
        let frames = job.frames();
        let progress = Arc::new(ExportProgress::default());
        let (tx, rx) = mpsc::channel();
        let (worker_progress, worker_path) = (Arc::clone(&progress), path.to_owned());
        std::thread::Builder::new()
            .name("dawkey-export".into())
            .spawn(move || {
                let _ = tx.send(job.run(&spec, &worker_path, &worker_progress));
            })
            .map_err(|e| format!("starting the export: {e}"))?;
        self.export = Some(RunningExport {
            path: path.to_owned(),
            progress,
            frames,
            done: rx,
        });
        Ok(())
    }

    /// How far the running export is (0 to 1), if one is.
    pub fn export_progress(&self) -> Option<f64> {
        self.export.as_ref().map(|e| {
            let done = e.progress.frames.load(Ordering::Relaxed) as f64;
            (done / e.frames.max(1) as f64).min(1.0)
        })
    }

    /// Every tick: an export that's done is finished (its plugins unloaded)
    /// and reported.
    pub(crate) fn poll_export(&mut self) {
        let Some(export) = &self.export else {
            return;
        };
        let (result, leftovers) = match export.done.try_recv() {
            Ok(done) => done,
            Err(mpsc::TryRecvError::Empty) => {
                self.redraw = true;
                return;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.export = None;
                self.message = Some("The export stopped unexpectedly".into());
                return;
            }
        };
        let export = self.export.take().expect("checked above");
        self.studio.finish_render(leftovers);
        let name = export
            .path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        self.message = Some(match result {
            Ok(analysis) => match analysis.lufs {
                Some(lufs) => format!("Exported {name} ({lufs:.1} LUFS)"),
                None => format!("Exported {name}"),
            },
            Err(MediaError::Cancelled) => "Export cancelled".into(),
            Err(e) => e.to_string(),
        });
        self.redraw = true;
    }
}

/// The choice after `current` in `choices` (the first after the last, or
/// if `current` isn't one of them).
fn next_in<T: Copy + PartialEq>(choices: &[T], current: T) -> T {
    choices
        .iter()
        .position(|&c| c == current)
        .and_then(|i| choices.get(i + 1).copied())
        .unwrap_or(choices[0])
}
