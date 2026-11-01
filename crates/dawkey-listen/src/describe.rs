// SPDX-License-Identifier: AGPL-3.0-or-later
//! The listening report: what a stretch of audio sounds like, in numbers
//! and sentences.

use ebur128::{EbuR128, Mode};
use serde::Serialize;

use crate::spectral::{self, OCTAVE_CENTERS, Stft, note_name};
use crate::{Clip, pitch, power_db, rhythm, round1, round2, round3, tonal};

/// What [`describe`] reports beyond the summary.
#[derive(Debug, Clone)]
pub struct DescribeOptions {
    /// Parts to describe one by one in the timeline, in seconds on the
    /// clip's timeline (as [`Clip::start_s`] counts them).
    pub segments: Vec<(f64, f64)>,
    /// Track the pitch of a single line and list its notes.
    pub notes: bool,
    /// The most notes to list.
    pub max_notes: usize,
    /// The most onsets to list (0 lists none; they're still counted).
    pub max_onsets: usize,
}

impl Default for DescribeOptions {
    fn default() -> Self {
        Self {
            segments: Vec::new(),
            notes: true,
            max_notes: 64,
            max_onsets: 0,
        }
    }
}

/// A listening report. Levels are dBFS (`None` is silence), times seconds
/// on the clip's timeline, bars as the ruler numbers them (fractional).
#[derive(Debug, Clone, Serialize)]
pub struct Description {
    pub duration_s: f64,
    pub levels: Levels,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stereo: Option<Stereo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spectrum: Option<Spectrum>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tonality: Option<Tonality>,
    pub rhythm: Rhythm,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<Notes>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub timeline: Vec<Moment>,
    /// What stands out, in sentences.
    pub observations: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Levels {
    pub peak_dbfs: Option<f64>,
    /// Peak between the samples (4× oversampled), as converters and
    /// encoders see it.
    pub true_peak_dbtp: Option<f64>,
    pub rms_dbfs: Option<f64>,
    /// Integrated loudness (EBU R128).
    pub lufs: Option<f64>,
    /// The loudest 3 seconds.
    pub max_short_term_lufs: Option<f64>,
    /// How much the loudness varies (EBU R128 LRA), in LU.
    pub loudness_range_lu: Option<f64>,
    /// Peak minus RMS: small means dense or limited, large means punchy or
    /// sparse.
    pub crest_db: Option<f64>,
    /// Samples at or over full scale: they clip once written to a file.
    pub clipped_samples: usize,
    /// NaN and infinite samples (counted as silence).
    pub non_finite_samples: usize,
    /// Where sound (above −60 dBFS) starts and ends.
    pub sound_from_s: Option<f64>,
    pub sound_to_s: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stereo {
    /// Correlation of left and right: 1 is mono, 0 unrelated (wide), below
    /// 0 out of phase (cancels when summed to mono).
    pub correlation: f64,
    /// The side signal's level next to the mid's; `None` when left and
    /// right are identical.
    pub side_to_mid_db: Option<f64>,
    /// How much louder left is than right (negative: right is louder).
    pub balance_db: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Spectrum {
    /// Level of each octave band (RMS, dBFS). A mix usually falls a few dB
    /// an octave; pink noise is flat here.
    pub octaves: Vec<Octave>,
    /// Slope of the octave levels from 63 Hz to 8 kHz.
    pub tilt_db_per_octave: f64,
    /// The power-weighted mean frequency: higher sounds brighter.
    pub centroid_hz: f64,
    /// 85 % of the power is below this.
    pub rolloff_hz: f64,
    /// 0 for pure tones, 1 for white noise.
    pub flatness: f64,
    /// The narrow peaks that stand out most: fundamentals of held notes,
    /// resonances, hum.
    pub peaks: Vec<Peak>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Octave {
    pub hz: f64,
    pub dbfs: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Peak {
    pub hz: f64,
    /// The nearest note.
    pub note: String,
    /// Level of a sixth of an octave around it.
    pub dbfs: f64,
    /// How far it stands above the octave around it.
    pub prominence_db: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Tonality {
    /// The best fitting key (Krumhansl–Kessler profiles); a relative
    /// major and minor fit almost equally. Only with three pitch classes
    /// or more.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Correlation with the key's profile: above about 0.8 is clear.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_score: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    /// How much of each pitch class sounds, strongest first, the strongest
    /// 1 (those under 0.2 left out).
    pub pitch_classes: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rhythm {
    pub onsets: usize,
    pub onsets_per_s: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tempo: Option<Tempo>,
    /// How the onsets sit on the project's 16th-note grid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<Timeliness>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub list: Vec<Onset>,
    /// Onsets left out of `list`.
    #[serde(skip_serializing_if = "is_zero")]
    pub unlisted: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Tempo {
    /// The pulse heard, from the onsets' spacing.
    pub bpm: f64,
    /// 0 to 1: how regular the pulse is.
    pub confidence: f64,
    /// Other tempos that fit nearly as well (often half or double).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<f64>,
    /// The project's tempo here, to compare.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_bpm: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Timeliness {
    /// Positive is late.
    pub median_offset_ms: f64,
    pub mean_abs_offset_ms: f64,
    /// The share of onsets within 10 ms of a 16th note.
    pub within_10ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Onset {
    pub at_s: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bar: Option<f64>,
    /// From the nearest 16th note; positive is late.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_ms: Option<f64>,
    /// Next to the strongest, 0 to 1.
    pub strength: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Notes {
    /// The share of the sounding time with one clear pitch: near 1 for a
    /// single line, low for chords, drums or noise (where the notes listed
    /// mean little).
    pub voiced: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    pub list: Vec<Note>,
    #[serde(skip_serializing_if = "is_zero")]
    pub unlisted: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Note {
    pub pitch: String,
    pub midi: i32,
    pub start_s: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bar: Option<f64>,
    pub length_s: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length_beats: Option<f64>,
    /// Off the equal-tempered pitch (A4 = 440 Hz).
    pub cents: f64,
    pub level_dbfs: f64,
}

/// One segment of the timeline.
#[derive(Debug, Clone, Serialize)]
pub struct Moment {
    pub start_s: f64,
    pub end_s: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bar: Option<f64>,
    pub peak_dbfs: Option<f64>,
    pub rms_dbfs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub centroid_hz: Option<f64>,
    /// The chord it sounds like, when one fits well.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chord: Option<String>,
    pub onsets: usize,
    /// Notes starting in it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Below this a sample is silence for where sound starts and ends.
const SOUND_FLOOR: f32 = 0.001;

pub fn describe(clip: &Clip<'_>, options: &DescribeOptions) -> Description {
    let rate = clip.sample_rate;
    let frames = clip.frames();
    let mono = clip.mono();
    let levels = levels(clip);
    let stereo = stereo(clip);
    let silent = levels.peak_dbfs.is_none();

    let tonal_stft = Stft::about(rate, 0.17, 4);
    let mut average = vec![0.0f64; tonal_stft.bins()];
    let mut chroma = [0.0f64; 12];
    let mut segment_power: Vec<Vec<f64>> = vec![Vec::new(); options.segments.len()];
    let mut segment_chroma = vec![tonal::ChromaSum::default(); options.segments.len()];
    let mut count = 0usize;
    let seconds = |sample: usize| clip.start_s + sample as f64 / f64::from(rate);
    if !silent {
        tonal_stft.frames(&mono, |center, power| {
            count += 1;
            for (a, &p) in average.iter_mut().zip(power) {
                *a += f64::from(p);
            }
            let frame = tonal::frame_chroma(power, &tonal_stft, rate);
            for (c, v) in chroma.iter_mut().zip(frame) {
                *c += v;
            }
            let at = seconds(center);
            for (i, &(from, to)) in options.segments.iter().enumerate() {
                if at >= from && at < to {
                    let sp = &mut segment_power[i];
                    if sp.is_empty() {
                        sp.resize(power.len(), 0.0);
                    }
                    for (a, &p) in sp.iter_mut().zip(power) {
                        *a += f64::from(p);
                    }
                    segment_chroma[i].add(&frame);
                }
            }
        });
    }
    if count > 0 {
        for a in &mut average {
            *a /= count as f64;
        }
    }
    let spectrum = (!silent).then(|| spectrum(&average, &tonal_stft, rate));
    let tonality = (!silent).then(|| tonality(&chroma)).flatten();

    let found = if silent {
        rhythm::Onsets::default()
    } else {
        rhythm::onsets(&mono, rate)
    };
    let rhythm = rhythm_report(clip, &found, options.max_onsets);

    let onset_times: Vec<f64> = found.onsets.iter().map(|o| o.at_s).collect();
    let raw_notes = (options.notes && !silent).then(|| {
        let mut frames = pitch::track(&mono, rate);
        pitch::confirm(&mut frames, &mono, rate);
        let sounding = frames.iter().filter(|f| f.level_db > -60.0).count();
        let voiced = frames.iter().filter(|f| f.midi.is_some()).count();
        let share = if sounding > 0 {
            voiced as f64 / sounding as f64
        } else {
            0.0
        };
        (pitch::notes(&frames, &onset_times), share)
    });
    let notes = raw_notes
        .as_ref()
        .map(|(raw, voiced)| notes_report(clip, raw, *voiced, options.max_notes));

    let timeline = options
        .segments
        .iter()
        .enumerate()
        .map(|(i, &(from, to))| {
            let at = |s: f64| {
                (((s - clip.start_s) * f64::from(rate)).round().max(0.0) as usize).min(frames)
            };
            let (a, b) = (at(from), at(to));
            let mut peak = 0.0f32;
            let mut sum = 0.0f64;
            for channel in clip.channels {
                for &s in &channel[a..b] {
                    if s.is_finite() {
                        peak = peak.max(s.abs());
                        sum += f64::from(s) * f64::from(s);
                    }
                }
            }
            let n = ((b - a) * clip.channels.len()).max(1);
            let shape = (!segment_power[i].is_empty())
                .then(|| spectral::shape(&segment_power[i], &tonal_stft, rate));
            let chord = segment_chroma[i].chord();
            let onsets = onset_times
                .iter()
                .filter(|&&t| (from..to).contains(&(clip.start_s + t)))
                .count();
            let notes = raw_notes
                .as_ref()
                .filter(|(_, voiced)| *voiced >= LIST_VOICED)
                .map(|(raw, _)| {
                    raw.iter()
                        .filter(|n| (from..to).contains(&(clip.start_s + n.start_s)))
                        .map(|n| note_name(n.midi))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            Moment {
                start_s: round3(from),
                end_s: round3(to),
                bar: clip.bar_at(from).map(round2),
                peak_dbfs: crate::amplitude_db(peak),
                rms_dbfs: power_db(sum / n as f64),
                centroid_hz: shape
                    .filter(|s| s.centroid_hz > 0.0)
                    .map(|s| s.centroid_hz.round()),
                chord,
                onsets,
                notes,
            }
        })
        .collect();

    let mut description = Description {
        duration_s: round3(clip.duration_s()),
        levels,
        stereo,
        spectrum,
        tonality,
        rhythm,
        notes,
        timeline,
        observations: Vec::new(),
    };
    description.observations = observations(clip, &description);
    description
}

fn levels(clip: &Clip<'_>) -> Levels {
    let rate = clip.sample_rate;
    let frames = clip.frames();
    let channels = clip.channels.len();
    let mut peak = 0.0f32;
    let mut sum = 0.0f64;
    let mut clipped = 0usize;
    let mut non_finite = 0usize;
    let mut first: Option<usize> = None;
    let mut last: Option<usize> = None;
    for channel in clip.channels {
        for (i, &s) in channel.iter().enumerate() {
            if !s.is_finite() {
                non_finite += 1;
                continue;
            }
            let a = s.abs();
            peak = peak.max(a);
            sum += f64::from(s) * f64::from(s);
            if a >= 1.0 {
                clipped += 1;
            }
            if a > SOUND_FLOOR {
                first = Some(first.map_or(i, |f| f.min(i)));
                last = Some(last.map_or(i, |l| l.max(i)));
            }
        }
    }
    let rms = power_db(sum / (frames * channels).max(1) as f64);
    let mut lufs = None;
    let mut lra = None;
    let mut true_peak = None;
    let mut short_max: Option<f64> = None;
    if channels > 0
        && frames > 0
        && peak > 0.0
        && let Ok(mut meter) = EbuR128::new(
            channels as u32,
            rate,
            Mode::I | Mode::LRA | Mode::S | Mode::TRUE_PEAK,
        )
    {
        let scrubbed: Vec<Vec<f32>>;
        let planes: Vec<&[f32]> = if non_finite > 0 {
            scrubbed = clip
                .channels
                .iter()
                .map(|c| {
                    c.iter()
                        .map(|&s| if s.is_finite() { s } else { 0.0 })
                        .collect()
                })
                .collect();
            scrubbed.iter().map(Vec::as_slice).collect()
        } else {
            clip.channels.iter().map(Vec::as_slice).collect()
        };
        // Short-term loudness is read every tenth of a second, for its
        // maximum.
        let step = (rate as usize / 10).max(1);
        let mut at = 0;
        let mut ok = true;
        while at < frames && ok {
            let end = (at + step).min(frames);
            let block: Vec<&[f32]> = planes.iter().map(|p| &p[at..end]).collect();
            ok = meter.add_frames_planar_f32(&block).is_ok();
            if end >= 3 * rate as usize
                && let Ok(s) = meter.loudness_shortterm()
                && s.is_finite()
            {
                short_max = Some(short_max.map_or(s, |m: f64| m.max(s)));
            }
            at = end;
        }
        if ok {
            lufs = meter.loudness_global().ok().filter(|l| l.is_finite());
            lra = meter.loudness_range().ok().filter(|l| l.is_finite());
            true_peak = (0..channels as u32)
                .filter_map(|c| meter.true_peak(c).ok())
                .fold(None, |m: Option<f64>, p| Some(m.map_or(p, |m| m.max(p))))
                .filter(|p| *p > 0.0)
                .map(|p| round1(20.0 * p.log10()));
        }
    }
    let peak_db = crate::amplitude_db(peak);
    Levels {
        peak_dbfs: peak_db,
        true_peak_dbtp: true_peak,
        rms_dbfs: rms,
        lufs: lufs.map(round1),
        max_short_term_lufs: short_max.map(round1),
        loudness_range_lu: lra.filter(|_| lufs.is_some()).map(round1),
        crest_db: peak_db.zip(rms).map(|(p, r)| round1(p - r)),
        clipped_samples: clipped,
        non_finite_samples: non_finite,
        sound_from_s: first.map(|f| round3(clip.start_s + f as f64 / f64::from(rate))),
        sound_to_s: last.map(|l| round3(clip.start_s + (l + 1) as f64 / f64::from(rate))),
    }
}

fn stereo(clip: &Clip<'_>) -> Option<Stereo> {
    let [left, right] = clip.channels else {
        return None;
    };
    let (mut ll, mut rr, mut lr) = (0.0f64, 0.0f64, 0.0f64);
    for (&l, &r) in left.iter().zip(right) {
        let (l, r) = (
            if l.is_finite() { f64::from(l) } else { 0.0 },
            if r.is_finite() { f64::from(r) } else { 0.0 },
        );
        ll += l * l;
        rr += r * r;
        lr += l * r;
    }
    if ll <= 1e-20 && rr <= 1e-20 {
        return None;
    }
    let correlation = if ll > 0.0 && rr > 0.0 {
        lr / (ll * rr).sqrt()
    } else {
        0.0
    };
    // Mid = (L + R) / 2 and side = (L − R) / 2, as energies.
    let mid = (ll + rr + 2.0 * lr) / 4.0;
    let side = ((ll + rr - 2.0 * lr) / 4.0).max(0.0);
    // Identical channels have no side; opposite ones no mid.
    let side_to_mid_db = (side > 1e-12 * (ll + rr))
        .then(|| round1((10.0 * (side / mid.max(1e-30)).log10()).min(60.0)));
    let balance_db = 10.0 * ((ll + 1e-20) / (rr + 1e-20)).log10();
    Some(Stereo {
        correlation: round2(correlation),
        side_to_mid_db,
        balance_db: round1(balance_db.clamp(-99.0, 99.0)),
    })
}

fn spectrum(average: &[f64], stft: &Stft, rate: u32) -> Spectrum {
    let bands = spectral::octave_powers(average, stft, rate);
    let octaves: Vec<Octave> = OCTAVE_CENTERS
        .iter()
        .zip(bands)
        .filter(|(hz, _)| **hz < f64::from(rate) / 2.0)
        .map(|(&hz, p)| Octave {
            hz,
            dbfs: power_db(p).filter(|db| *db > -150.0),
        })
        .collect();
    // A least-squares slope over 63 Hz to 8 kHz, bands with sound only.
    let points: Vec<(f64, f64)> = octaves
        .iter()
        .enumerate()
        .filter(|(_, o)| (63.0..=8000.0).contains(&o.hz))
        .filter_map(|(i, o)| o.dbfs.map(|db| (i as f64, db)))
        .collect();
    let tilt = if points.len() >= 3 {
        let n = points.len() as f64;
        let mx = points.iter().map(|p| p.0).sum::<f64>() / n;
        let my = points.iter().map(|p| p.1).sum::<f64>() / n;
        let num: f64 = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
        let den: f64 = points.iter().map(|p| (p.0 - mx).powi(2)).sum();
        if den > 0.0 { num / den } else { 0.0 }
    } else {
        0.0
    };
    let shape = spectral::shape(average, stft, rate);
    Spectrum {
        octaves,
        tilt_db_per_octave: round1(tilt),
        centroid_hz: shape.centroid_hz.round(),
        rolloff_hz: shape.rolloff_hz.round(),
        flatness: round3(shape.flatness),
        peaks: peaks(average, stft, rate),
    }
}

/// Narrow peaks: maxima of the spectrum smoothed over a twelfth of an
/// octave that stand at least 6 dB above it smoothed over an octave.
fn peaks(average: &[f64], stft: &Stft, rate: u32) -> Vec<Peak> {
    let top = (f64::from(rate) / 2.0 * 0.9).min(16_000.0);
    let points = spectral::log_points(30.0, top, 480);
    let fine = spectral::smoothed_db(average, stft, rate, &points, 1.0 / 12.0);
    let wide = spectral::smoothed_db(average, stft, rate, &points, 1.0);
    let sixth = spectral::smoothed_db(average, stft, rate, &points, 1.0 / 6.0);
    let loudest = sixth.iter().copied().fold(f64::MIN, f64::max);
    let mut found: Vec<Peak> = Vec::new();
    for i in 1..points.len() - 1 {
        let prominence = fine[i] - wide[i];
        if fine[i] > fine[i - 1]
            && fine[i] >= fine[i + 1]
            && prominence >= 6.0
            && sixth[i] > loudest - 40.0
        {
            // The level of a sixth of an octave as a band: density times
            // its width in bins.
            let bins =
                points[i] * (2f64.powf(1.0 / 12.0) - 2f64.powf(-1.0 / 12.0)) * stft.size() as f64
                    / f64::from(rate);
            let midi = spectral::midi_of_hz(points[i]).round() as i32;
            found.push(Peak {
                hz: round_hz(points[i]),
                note: note_name(midi),
                dbfs: round1(sixth[i] + 10.0 * bins.max(1.0).log10()),
                prominence_db: round1(prominence),
            });
        }
    }
    found.sort_by(|a, b| b.dbfs.total_cmp(&a.dbfs));
    // One peak per semitone: neighbouring maxima are the same peak.
    let mut kept: Vec<Peak> = Vec::new();
    for peak in found {
        if kept
            .iter()
            .all(|k| (k.hz / peak.hz).log2().abs() > 1.0 / 12.0)
        {
            kept.push(peak);
        }
    }
    kept.truncate(6);
    kept
}

/// Rounds a frequency to three significant figures.
fn round_hz(hz: f64) -> f64 {
    let digits = hz.log10().floor() as i32 - 2;
    let scale = 10f64.powi(digits.max(0));
    (hz / scale).round() * scale
}

fn tonality(chroma: &[f64; 12]) -> Option<Tonality> {
    let normalized = tonal::normalized(chroma);
    if normalized.iter().all(|&c| c == 0.0) {
        return None;
    }
    let keys = tonal::keys(&normalized);
    let (key, score) = keys.first()?.clone();
    let mut classes: Vec<(usize, f64)> = normalized
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, c)| *c >= 0.2)
        .collect();
    classes.sort_by(|a, b| b.1.total_cmp(&a.1));
    let pitch_classes = classes
        .iter()
        .map(|(i, c)| format!("{} {c:.2}", spectral::PITCH_CLASSES[*i]))
        .collect::<Vec<_>>()
        .join(", ");
    // One or two pitch classes (a repeated note, a fifth) fit many keys.
    let keyed = classes.len() >= 3;
    Some(Tonality {
        key: keyed.then_some(key),
        key_score: keyed.then_some(round2(score)),
        alternatives: if keyed {
            keys[1..]
                .iter()
                .take_while(|(_, s)| *s >= score - 0.1)
                .take(3)
                .map(|(k, s)| format!("{k} ({s:.2})"))
                .collect()
        } else {
            Vec::new()
        },
        pitch_classes,
    })
}

fn rhythm_report(clip: &Clip<'_>, found: &rhythm::Onsets, max_list: usize) -> Rhythm {
    let duration = clip.duration_s();
    let tempo = rhythm::tempo(found).map(|guess| Tempo {
        bpm: guess.bpm,
        confidence: guess.confidence,
        alternatives: guess.alternatives,
        project_bpm: clip.timing.map(|t| {
            let middle = t.beat_at(clip.start_s + duration / 2.0);
            round1(60.0 / (t.seconds_at(middle + 1.0) - t.seconds_at(middle)))
        }),
    });
    let offsets: Vec<Option<f64>> = found
        .onsets
        .iter()
        .map(|o| {
            let t = clip.timing?;
            let at = clip.start_s + o.at_s;
            let sixteenth = (t.beat_at(at) * 4.0).round() / 4.0;
            Some((at - t.seconds_at(sixteenth)) * 1000.0)
        })
        .collect();
    let timed: Vec<f64> = offsets.iter().flatten().copied().collect();
    let timing = (timed.len() >= 4).then(|| {
        let mut sorted = timed.clone();
        sorted.sort_by(f64::total_cmp);
        Timeliness {
            median_offset_ms: round1(sorted[sorted.len() / 2]),
            mean_abs_offset_ms: round1(
                timed.iter().map(|o| o.abs()).sum::<f64>() / timed.len() as f64,
            ),
            within_10ms: round2(
                timed.iter().filter(|o| o.abs() <= 10.0).count() as f64 / timed.len() as f64,
            ),
        }
    });
    let list: Vec<Onset> = found
        .onsets
        .iter()
        .zip(&offsets)
        .take(max_list)
        .map(|(o, offset)| Onset {
            at_s: round3(clip.start_s + o.at_s),
            bar: clip.bar_at(clip.start_s + o.at_s).map(round3),
            offset_ms: offset.map(round1),
            strength: round2(o.strength),
        })
        .collect();
    Rhythm {
        onsets: found.onsets.len(),
        onsets_per_s: if duration > 0.0 {
            round2(found.onsets.len() as f64 / duration)
        } else {
            0.0
        },
        tempo,
        timing,
        unlisted: found.onsets.len() - list.len(),
        list,
    }
}

/// Below this share of voiced time, notes aren't listed: what pitch
/// tracking finds in chords or a mix is noise.
const LIST_VOICED: f64 = 0.3;

fn notes_report(clip: &Clip<'_>, raw: &[pitch::RawNote], voiced: f64, max: usize) -> Notes {
    let raw = if voiced >= LIST_VOICED { raw } else { &[] };
    let lo = raw.iter().map(|n| n.midi).min();
    let hi = raw.iter().map(|n| n.midi).max();
    let list: Vec<Note> = raw
        .iter()
        .take(max)
        .map(|n| {
            let (from, to) = (clip.start_s + n.start_s, clip.start_s + n.end_s);
            Note {
                pitch: note_name(n.midi),
                midi: n.midi,
                start_s: round3(from),
                bar: clip.bar_at(from).map(round3),
                length_s: round3(to - from),
                length_beats: clip
                    .beat_at(to)
                    .zip(clip.beat_at(from))
                    .map(|(b, a)| round2(b - a)),
                cents: n.cents.round(),
                level_dbfs: round1(n.level_db),
            }
        })
        .collect();
    Notes {
        voiced: round2(voiced),
        range: lo.zip(hi).map(|(lo, hi)| {
            if lo == hi {
                note_name(lo)
            } else {
                format!("{} to {}", note_name(lo), note_name(hi))
            }
        }),
        unlisted: raw.len() - list.len(),
        list,
    }
}

/// Plain sentences about what stands out, each with its number.
fn observations(clip: &Clip<'_>, d: &Description) -> Vec<String> {
    let mut out = Vec::new();
    let l = &d.levels;
    if l.non_finite_samples > 0 {
        out.push(format!(
            "{} samples are NaN or infinite: something in the chain (usually a plugin) is \
             misbehaving.",
            l.non_finite_samples
        ));
    }
    let Some(peak) = l.peak_dbfs else {
        out.push("Silent: every sample is zero.".to_owned());
        return out;
    };
    if l.sound_from_s.is_none() {
        out.push(format!(
            "Nearly silent: the peak is {peak} dBFS, below the −60 dBFS taken as sound."
        ));
    }
    let end = clip.start_s + clip.duration_s();
    if let (Some(from), Some(to)) = (l.sound_from_s, l.sound_to_s) {
        let (lead, tail) = (from - clip.start_s, end - to);
        if lead > 0.5 || tail > 0.5 {
            out.push(format!(
                "Sound runs from {from:.2} s to {to:.2} s; the {:.1} s before and {:.1} s after \
                 are (nearly) silent.",
                lead.max(0.0),
                tail.max(0.0)
            ));
        }
    }
    if l.clipped_samples > 0 {
        out.push(format!(
            "{} samples are at or over full scale (0 dBFS, peak {peak} dBFS): they clip once \
             written to a file. Turn it down or limit it.",
            l.clipped_samples
        ));
    } else if let Some(tp) = l.true_peak_dbtp.filter(|tp| *tp > -1.0) {
        out.push(format!(
            "True peak {tp} dBTP is above −1 dBTP: it may clip once encoded (Ogg, Opus, MP3)."
        ));
    }
    if let Some(lufs) = l.lufs {
        if lufs > -11.0 {
            out.push(format!(
                "Integrated loudness {lufs} LUFS is louder than streaming services' −14 LUFS; \
                 they will turn it down."
            ));
        }
        if let Some(crest) = l.crest_db.filter(|c| *c < 8.0) {
            out.push(format!(
                "Crest factor {crest} dB (peak to RMS): very dense, as heavy compression or \
                 limiting makes it."
            ));
        }
    }
    let mean: f64 = {
        let mono = clip.mono();
        mono.iter().map(|&s| f64::from(s)).sum::<f64>() / mono.len().max(1) as f64
    };
    if mean.abs() > 0.003 {
        out.push(format!(
            "DC offset of {:.1} dBFS ({mean:+.4}): the waveform sits off center, which wastes \
             headroom; a high-pass filter removes it.",
            20.0 * mean.abs().log10()
        ));
    }
    if let Some(s) = &d.stereo {
        if s.side_to_mid_db.is_none() {
            out.push("Left and right are identical: it's mono.".to_owned());
        } else if s.correlation < 0.0 {
            out.push(format!(
                "Left and right are out of phase (correlation {}): parts cancel when it's \
                 summed to mono.",
                s.correlation
            ));
        } else if let Some(side) = s.side_to_mid_db.filter(|db| *db > -1.0) {
            out.push(format!(
                "Very wide: the side signal is {side} dB next to the mid (correlation {}).",
                s.correlation
            ));
        }
        if s.balance_db.abs() >= 3.0 {
            let (louder, db) = if s.balance_db > 0.0 {
                ("Left", s.balance_db)
            } else {
                ("Right", -s.balance_db)
            };
            out.push(format!("{louder} is {db} dB louder than the other side."));
        }
    }
    if let Some(sp) = &d.spectrum {
        let level = |hz: f64| sp.octaves.iter().find(|o| o.hz == hz).and_then(|o| o.dbfs);
        // The loudest octave, as where the energy is.
        if let Some(loudest) = sp
            .octaves
            .iter()
            .filter_map(|o| o.dbfs.map(|db| (o.hz, db)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
        {
            out.push(format!(
                "Most energy is in the {} octave; the spectral centroid is {} (brightness), \
                 tilt {} dB per octave.",
                hz_text(loudest.0),
                hz_text(sp.centroid_hz),
                sp.tilt_db_per_octave
            ));
        }
        if let (Some(sub), Some(rest)) = (level(31.5), d.levels.rms_dbfs)
            && sub > rest - 6.0
        {
            out.push(format!(
                "Strong sub-bass: the 31.5 Hz octave is at {sub} dBFS, {:.1} dB under the \
                 overall RMS; small speakers can't play it.",
                rest - sub
            ));
        }
    }
    if let Some(t) = &d.tonality
        && let (Some(key), Some(score)) = (&t.key, t.key_score)
        && score >= 0.6
    {
        let also = t
            .alternatives
            .first()
            .map(|a| format!(" (or {a})"))
            .unwrap_or_default();
        out.push(format!(
            "Sounds like {key} (score {score}){also}; strongest pitch classes: {}.",
            t.pitch_classes
        ));
    }
    let chords: Vec<&str> = d
        .timeline
        .iter()
        .filter_map(|m| m.chord.as_deref())
        .collect();
    if !chords.is_empty() {
        let mut progression: Vec<&str> = Vec::new();
        for c in &chords {
            if progression.last() != Some(c) {
                progression.push(c);
            }
        }
        out.push(format!(
            "Chords heard through the timeline: {}.",
            progression.join(" → ")
        ));
    }
    let r = &d.rhythm;
    if let Some(tempo) = &r.tempo
        && tempo.confidence >= 0.2
    {
        match tempo.project_bpm {
            Some(project) => {
                let ratio = tempo.bpm / project;
                let matches = [0.5, 1.0, 2.0, 2.0 / 3.0, 1.5]
                    .iter()
                    .any(|m| (ratio / m - 1.0).abs() < 0.03);
                if matches {
                    out.push(format!(
                        "The pulse ({} BPM) fits the project's tempo ({project} BPM).",
                        tempo.bpm
                    ));
                } else {
                    out.push(format!(
                        "The pulse sounds like {} BPM, which doesn't fit the project's \
                         {project} BPM.",
                        tempo.bpm
                    ));
                }
            }
            None => {
                let also = if tempo.alternatives.is_empty() {
                    String::new()
                } else {
                    format!(
                        " (or {})",
                        tempo
                            .alternatives
                            .iter()
                            .map(|b| b.to_string())
                            .collect::<Vec<_>>()
                            .join(" or ")
                    )
                };
                out.push(format!(
                    "The pulse sounds like {} BPM{also}, confidence {}.",
                    tempo.bpm, tempo.confidence
                ));
            }
        }
    }
    if let Some(t) = &r.timing
        && r.onsets >= 8
    {
        if t.median_offset_ms.abs() >= 8.0 {
            let (way, ms) = if t.median_offset_ms > 0.0 {
                ("late", t.median_offset_ms)
            } else {
                ("early", -t.median_offset_ms)
            };
            out.push(format!(
                "Onsets land {ms} ms {way} of the 16th-note grid (median of {}): a latency, a \
                 slow attack or a placement off the grid.",
                r.onsets
            ));
        } else if t.within_10ms < 0.6 {
            out.push(format!(
                "Onsets are loose against the 16th-note grid: {:.0} % within 10 ms, {} ms off \
                 on average (swing, triplets or a looser feel).",
                t.within_10ms * 100.0,
                t.mean_abs_offset_ms
            ));
        } else {
            out.push(format!(
                "Onsets are tight on the 16th-note grid: {:.0} % within 10 ms.",
                t.within_10ms * 100.0
            ));
        }
    }
    if let Some(n) = &d.notes {
        let count = n.list.len() + n.unlisted;
        if n.voiced >= 0.6 && count > 0 {
            let cents: Vec<f64> = n.list.iter().map(|n| n.cents).collect();
            let mean = cents.iter().sum::<f64>() / cents.len().max(1) as f64;
            out.push(format!(
                "A single line: {count} notes{}.",
                n.range
                    .as_ref()
                    .map(|r| format!(", {r}"))
                    .unwrap_or_default()
            ));
            if mean.abs() >= 15.0 {
                out.push(format!(
                    "The notes are {mean:+.0} cents off on average: out of tune with A4 = \
                     440 Hz."
                ));
            }
        } else if n.voiced < 0.35
            && d.tonality
                .as_ref()
                .is_some_and(|t| t.key_score >= Some(0.6))
        {
            out.push(
                "Several pitches sound at once (chords or a mix), so no notes are listed; \
                 read pitch_classes and the timeline's chords instead, or describe the \
                 stems."
                    .to_owned(),
            );
        }
    }
    out
}

/// A frequency as text: "250 Hz", "2.5 kHz".
fn hz_text(hz: f64) -> String {
    if hz >= 1000.0 {
        let k = hz / 1000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{} kHz", k.round())
        } else {
            format!("{k:.1} kHz")
        }
    } else {
        format!("{} Hz", hz.round())
    }
}
