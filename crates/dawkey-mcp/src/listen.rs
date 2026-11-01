// SPDX-License-Identifier: AGPL-3.0-or-later
//! describe_audio and view_audio: listening for a model that can't hear,
//! with `dawkey_listen` doing the measuring and drawing.

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;
use dawkey_core::TrackId;
use dawkey_listen::{Clip, DescribeOptions, Panel, PictureKind, PictureOptions, Timing};
use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{Value, json};

use crate::times::{self, Maps, SegmentSpec};
use crate::tools::{AudioSource, DescribeAudio, VIEW_AUDIO, ViewAudio, ViewKind};
use crate::{DawkeyMcp, blocking, parse, run, to_json};

/// The longest stretch listened to at once, in seconds.
const MAX_LISTEN_S: f64 = 600.0;
/// The most panels in a spectrogram or chromagram.
const MAX_PANELS: usize = 12;
/// The most curves on a spectrum.
const MAX_CURVES: usize = 8;
/// The most notes a description lists (per stem).
const MAX_NOTES: usize = 64;
/// The most onsets a description lists when asked to.
const MAX_ONSETS: usize = 400;

/// What describe_audio or view_audio listens to, worked out.
struct Plan {
    start_s: f64,
    end_s: f64,
    sample_rate: u32,
    /// The project's grid; `None` for a file.
    maps: Option<Maps>,
    renders: Vec<Render>,
    /// A file's audio from `start_s` to `end_s`, instead of renders.
    file: Option<Arc<Vec<Vec<f32>>>>,
    /// The project's name or the file's, for titles.
    name: String,
    tracks: Option<Vec<TrackId>>,
    path: Option<PathBuf>,
}

/// One render of a plan: the tracks to solo (none: everything), what to
/// call it, and for a stem, its track and name.
struct Render {
    solo: Option<Vec<TrackId>>,
    label: String,
    stem: Option<(TrackId, String)>,
}

impl DawkeyMcp {
    async fn plan_listen(&self, source: AudioSource, with_mix: bool) -> Result<Plan, String> {
        let AudioSource {
            start_s,
            end_s,
            tracks,
            stems,
            path,
        } = source;
        let start_s = start_s.unwrap_or(0.0);
        if !start_s.is_finite() || start_s < 0.0 {
            return Err(format!("start_s must be at least 0, got {start_s}"));
        }
        if let Some(path) = path {
            if tracks.is_some() || stems {
                return Err(
                    "tracks and stems pick parts of the project; leave them out with path"
                        .to_owned(),
                );
            }
            let path = std::path::absolute(&path).unwrap_or(path);
            let rate = run(&self.app, |s| Ok::<_, String>(s.sample_rate())).await?;
            let file = path.clone();
            let decoded = blocking(move || dawkey_media::decode_file(&file, rate)).await?;
            let audio = decoded.audio.channels();
            let length_s = audio[0].len() as f64 / f64::from(rate);
            let end_s = end_s.unwrap_or(length_s).min(length_s);
            check_range(start_s, end_s)?;
            let at = |s: f64| ((s * f64::from(rate)).round() as usize).min(audio[0].len());
            let (a, b) = (at(start_s), at(end_s));
            let channels: Vec<Vec<f32>> = audio.iter().map(|c| c[a..b].to_vec()).collect();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            return Ok(Plan {
                start_s,
                end_s,
                sample_rate: rate,
                maps: None,
                renders: vec![Render {
                    solo: None,
                    label: name.clone(),
                    stem: None,
                }],
                file: Some(Arc::new(channels)),
                name,
                tracks: None,
                path: Some(path),
            });
        }
        run(&self.app, move |s| {
            let project = s.session().project();
            let end_s = match end_s {
                Some(end_s) => end_s,
                None if project.end() == 0 => {
                    return Err(
                        "there's nothing to listen to: the project has no clips that \
                                play (give end_s to hear plugins' tails or live input)"
                            .to_owned(),
                    );
                }
                None => project.frames_to_seconds(project.end()),
            };
            check_range(start_s, end_s)?;
            let mix_label = match &tracks {
                Some(ids) => format!(
                    "Tracks {} (soloed)",
                    ids.iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None => "Mix".to_owned(),
            };
            let mut renders = Vec::new();
            if !stems || with_mix {
                renders.push(Render {
                    solo: tracks.clone(),
                    label: mix_label,
                    stem: None,
                });
            }
            if stems {
                for (id, name) in s
                    .stem_tracks(tracks.as_deref())
                    .map_err(|e| e.to_string())?
                {
                    renders.push(Render {
                        solo: Some(vec![id]),
                        label: format!("Track {id}: {name}"),
                        stem: Some((id, name)),
                    });
                }
            }
            Ok(Plan {
                start_s,
                end_s,
                sample_rate: project.sample_rate,
                maps: Some(Maps::of(project)),
                renders,
                file: None,
                name: project.name.clone(),
                tracks,
                path: None,
            })
        })
        .await
    }

    /// Runs `f` on each of `plan`'s renders (or its file) on a blocking
    /// thread, one render at a time, so stems needn't all be in memory.
    async fn listen_each<T, F>(&self, plan: &Plan, f: F) -> Result<Vec<T>, String>
    where
        T: Send + 'static,
        F: Fn(&Clip<'_>, &str) -> T + Send + Sync + 'static,
    {
        let f = Arc::new(f);
        let maps = plan.maps.clone().map(Arc::new);
        let (start_s, end_s, rate) = (plan.start_s, plan.end_s, plan.sample_rate);
        let mut out = Vec::with_capacity(plan.renders.len());
        if let Some(file) = &plan.file {
            let (file, f, label) = (Arc::clone(file), Arc::clone(&f), plan.name.clone());
            let result = blocking(move || {
                let clip = Clip {
                    channels: &file,
                    sample_rate: rate,
                    start_s,
                    timing: None,
                };
                Ok::<_, String>(f(&clip, &label))
            })
            .await?;
            out.push(result);
            return Ok(out);
        }
        for render in &plan.renders {
            let solo = render.solo.clone();
            let job = run(&self.app, move |s| {
                s.render_job_solo(start_s, end_s, solo.as_deref())
            })
            .await?;
            let (f, maps, label) = (Arc::clone(&f), maps.clone(), render.label.clone());
            let (result, leftovers) = blocking(move || {
                let rate = job.sample_rate();
                let (audio, leftovers) = job.run();
                let clip = Clip {
                    channels: &audio,
                    sample_rate: rate,
                    start_s,
                    timing: maps.as_deref().map(|m| m as &dyn Timing),
                };
                Ok::<_, String>((f(&clip, &label), leftovers))
            })
            .await?;
            // The render's plugin instances are unloaded on the studio's
            // thread.
            run(&self.app, move |s| {
                s.finish_render(leftovers);
                Ok::<_, String>(())
            })
            .await?;
            out.push(result);
        }
        Ok(out)
    }

    pub(crate) async fn describe_audio(&self, args: DescribeAudio) -> Result<Value, String> {
        let plan = self.plan_listen(args.source(), false).await?;
        let wanted = SegmentSpec {
            length_s: args.segment_length_s,
            bars: args.segment_bars,
            ranges: args
                .segments
                .map(|r| r.iter().map(|r| (r.start_s, r.end_s)).collect()),
        };
        let asked = wanted.length_s.is_some() || wanted.bars.is_some() || wanted.ranges.is_some();
        let segments = match &plan.maps {
            Some(maps) => {
                let wanted = if asked {
                    wanted
                } else {
                    SegmentSpec {
                        bars: Some(maps.timeline_bars(plan.start_s, plan.end_s)),
                        ..SegmentSpec::default()
                    }
                };
                maps.segments(plan.start_s, plan.end_s, &wanted)?
            }
            None => {
                if wanted.bars.is_some() {
                    return Err(
                        "a file isn't on the project's grid: give segment_length_s or segments"
                            .to_owned(),
                    );
                }
                let wanted = if asked {
                    wanted
                } else {
                    SegmentSpec {
                        length_s: Some(times::timeline_seconds(plan.end_s - plan.start_s)),
                        ..SegmentSpec::default()
                    }
                };
                // Lengths and ranges don't need a tempo map.
                let maps = run(&self.app, |s| {
                    Ok::<_, String>(Maps::of(s.session().project()))
                })
                .await?;
                maps.segments(plan.start_s, plan.end_s, &wanted)?
            }
        };
        let options = DescribeOptions {
            segments,
            notes: args.notes,
            max_notes: MAX_NOTES,
            max_onsets: if args.onsets { MAX_ONSETS } else { 0 },
        };
        let described = self
            .listen_each(&plan, move |clip, _| {
                to_json(&dawkey_listen::describe(clip, &options))
            })
            .await?;
        let mut result = json!({
            "start_s": plan.start_s,
            "end_s": plan.end_s,
            "sample_rate": plan.sample_rate,
        });
        if let Some(path) = &plan.path {
            result["path"] = json!(path);
        }
        if let Some(tracks) = &plan.tracks {
            result["tracks"] = json!(tracks);
        }
        if args.stems {
            let stems = plan
                .renders
                .iter()
                .zip(described)
                .map(|(render, mut d)| {
                    if let Some((id, name)) = &render.stem {
                        let mut stem = json!({ "track": id, "name": name });
                        if let (Value::Object(head), Value::Object(body)) = (&mut stem, d.take()) {
                            head.extend(body);
                        }
                        stem
                    } else {
                        d
                    }
                })
                .collect();
            result["stems"] = Value::Array(stems);
        } else if let Some(Value::Object(fields)) = described.into_iter().next() {
            for (key, value) in fields {
                result[key] = value;
            }
        }
        Ok(result)
    }

    /// `view_audio` returns an image, not JSON.
    pub(crate) async fn view_audio(&self, args: Value) -> CallToolResult {
        match self.view_audio_png(args).await {
            Ok((png, caption)) => CallToolResult::success(vec![
                ContentBlock::image(
                    base64::engine::general_purpose::STANDARD.encode(png),
                    "image/png",
                ),
                ContentBlock::text(caption),
            ]),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    async fn view_audio_png(&self, args: Value) -> Result<(Vec<u8>, String), String> {
        let args = self.in_seconds(VIEW_AUDIO, args).await?;
        let args: ViewAudio = parse(args)?;
        let kind = match args.kind.unwrap_or(ViewKind::Spectrogram) {
            ViewKind::Spectrogram => PictureKind::Spectrogram,
            ViewKind::Spectrum => PictureKind::Spectrum,
            ViewKind::Chroma => PictureKind::Chroma,
        };
        let mut options = PictureOptions::new(kind);
        if let Some(width) = args.width {
            if !(400..=1600).contains(&width) {
                return Err(format!("width is 400 to 1600 pixels, not {width}"));
            }
            options.width = width;
        }
        if let Some(height) = args.height {
            if !(60..=800).contains(&height) {
                return Err(format!("height is 60 to 800 pixels, not {height}"));
            }
            options.height = height;
        }
        if let Some(range) = args.range_db {
            if !(20.0..=140.0).contains(&range) {
                return Err(format!("range_db is 20 to 140, not {range}"));
            }
            options.range_db = range;
        }
        let plan = self.plan_listen(args.source(), args.with_mix).await?;
        let nyquist = f64::from(plan.sample_rate) / 2.0;
        options.min_hz = args.min_hz.unwrap_or(20.0);
        options.max_hz = args.max_hz.unwrap_or(20_000.0).min(nyquist);
        if !(options.min_hz >= 10.0 && options.max_hz >= options.min_hz * 1.5) {
            return Err(format!(
                "min_hz must be at least 10 and max_hz at least half again as high (up to \
                 {nyquist} Hz here), got {} to {}",
                options.min_hz, options.max_hz
            ));
        }
        let most = match kind {
            PictureKind::Spectrum => MAX_CURVES,
            _ => MAX_PANELS,
        };
        if plan.renders.len() > most {
            return Err(format!(
                "that's {} {}; at most {most} fit in one picture. Pick some with tracks, or \
                 leave the mix out (with_mix: false)",
                plan.renders.len(),
                if kind == PictureKind::Spectrum {
                    "curves"
                } else {
                    "panels"
                }
            ));
        }
        options.title = title(&plan, kind);
        let prepare = options.clone();
        let panels = self
            .listen_each(&plan, move |clip, label| {
                Panel::prepare(label, clip, &prepare)
            })
            .await?;
        let labels: Vec<String> = plan.renders.iter().map(|r| r.label.clone()).collect();
        blocking(move || {
            let picture = dawkey_listen::picture(&panels, &options);
            let caption = format!(
                "{} {}x{} px; {}: {}.",
                picture.caption,
                picture.width,
                picture.height,
                if labels.len() == 1 {
                    "shows"
                } else {
                    "top to bottom"
                },
                labels.join(", ")
            );
            Ok::<_, String>((picture.to_png(), caption))
        })
        .await
    }
}

fn check_range(start_s: f64, end_s: f64) -> Result<(), String> {
    if !end_s.is_finite() || end_s <= start_s {
        return Err(format!(
            "end_s must be after start_s, got {start_s} to {end_s}"
        ));
    }
    if end_s - start_s > MAX_LISTEN_S {
        return Err(format!(
            "that's {:.0} s; listen to at most {MAX_LISTEN_S:.0} s at a time (start_s, end_s)",
            end_s - start_s
        ));
    }
    Ok(())
}

/// "Song: spectrogram, bars 1 to 9 (0.00 to 16.00 s)".
fn title(plan: &Plan, kind: PictureKind) -> String {
    let what = match kind {
        PictureKind::Spectrogram => "spectrogram",
        PictureKind::Spectrum => "average spectrum",
        PictureKind::Chroma => "chromagram",
    };
    let bars = plan.maps.as_ref().map(|m| {
        let bar = |s: f64| m.bar_of_beat(m.beat_at(s));
        format!(
            "bars {} to {}, ",
            short(bar(plan.start_s)),
            short(bar(plan.end_s))
        )
    });
    format!(
        "{}: {what}, {}{:.2} to {:.2} s",
        plan.name,
        bars.unwrap_or_default(),
        plan.start_s,
        plan.end_s
    )
}

/// A number with at most two decimals, and none if it's whole.
fn short(x: f64) -> String {
    let r = (x * 100.0).round() / 100.0;
    if r.fract() == 0.0 {
        format!("{r:.0}")
    } else {
        format!("{r}")
    }
}
