// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pictures of audio for models that read images: spectrograms, spectra
//! and chromagrams, with axes labelled in Hz, bars and dB.
//!
//! A picture is made in two steps so stems needn't all be in memory at
//! once: [`Panel::prepare`] reduces each one's audio to what's drawn, and
//! [`picture`] draws the panels.

use dawkey_text::TextSystem;
use dawkey_ui::{FontFamily, FontWeight, TextStyle};

use crate::spectral::{self, PITCH_CLASSES, Stft};
use crate::{Clip, tonal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureKind {
    /// Frequency (log) against time, level as color, with a level strip
    /// and the bar grid: one panel per stem.
    Spectrogram,
    /// The average spectrum, one curve per stem on one plot.
    Spectrum,
    /// Pitch classes against time, with the chord each bar sounds like.
    Chroma,
}

#[derive(Debug, Clone)]
pub struct PictureOptions {
    pub kind: PictureKind,
    /// Width of the whole picture in pixels.
    pub width: u32,
    /// Height of each spectrogram or chroma panel's plot, or of the
    /// spectrum plot.
    pub height: u32,
    pub min_hz: f64,
    pub max_hz: f64,
    /// How many dB below the loudest are shown (darkest color, or the
    /// plot's bottom).
    pub range_db: f64,
    /// A line at the top saying what this is.
    pub title: String,
}

impl PictureOptions {
    /// Defaults for `kind`.
    pub fn new(kind: PictureKind) -> Self {
        Self {
            kind,
            width: 1200,
            height: match kind {
                PictureKind::Spectrogram => 280,
                PictureKind::Spectrum => 520,
                PictureKind::Chroma => 180,
            },
            min_hz: 20.0,
            max_hz: 20_000.0,
            range_db: match kind {
                PictureKind::Spectrum => 80.0,
                _ => 90.0,
            },
            title: String::new(),
        }
    }
}

/// A picture: 8-bit RGB pixels, row by row.
#[derive(Clone)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
    /// What's on it, for the text that goes with it: axes, scales and
    /// the colors of the curves.
    pub caption: String,
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picture")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl Picture {
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header to a Vec");
        writer
            .write_image_data(&self.rgb)
            .expect("PNG data to a Vec");
        writer.finish().expect("PNG finish to a Vec");
        out
    }

    /// The pixel at (`x`, `y`).
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * self.width + x) * 3) as usize;
        [self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]
    }
}

/// A grid line on the time axis: seconds from the clip's start, and the
/// bar number it starts (`None` for a beat).
type GridLine = (f64, Option<u32>);

/// One stem reduced to what's drawn of it.
#[derive(Debug, Clone)]
pub struct Panel {
    label: String,
    start_s: f64,
    duration_s: f64,
    /// Bar and beat lines, if the audio is on the project's timeline.
    grid: Option<Vec<GridLine>>,
    data: PanelData,
}

#[derive(Debug, Clone)]
enum PanelData {
    /// `rows × columns` power densities (dB), bottom row first, and the
    /// peak and RMS level (dBFS) of each column.
    Spectrogram {
        columns: usize,
        rows: usize,
        db: Vec<f32>,
        peak: Vec<f32>,
        rms: Vec<f32>,
    },
    /// The spectrum's level at each x (dB, a sixth-octave mean density).
    Spectrum { db: Vec<f32> },
    /// `12 × columns` chroma, each column scaled to its maximum (and dimmed
    /// with its level), C first; the chord of each bar (or 2 s).
    Chroma {
        columns: usize,
        values: Vec<f32>,
        chords: Vec<(f64, f64, String)>,
    },
}

// Layout, in pixels.
const LEFT: u32 = 64;
const RIGHT: u32 = 72;
const TITLE: u32 = 30;
const LABEL: u32 = 20;
const STRIP: u32 = 40;
const AXIS: u32 = 22;
const GAP: u32 = 10;

impl Panel {
    /// Reduces `clip` to what `options` draws of it.
    pub fn prepare(label: impl Into<String>, clip: &Clip<'_>, options: &PictureOptions) -> Self {
        let columns = options.width.saturating_sub(LEFT + RIGHT).max(16) as usize;
        let mono = clip.mono();
        let data = match options.kind {
            PictureKind::Spectrogram => {
                let rows = options.height.max(16) as usize;
                spectrogram(clip, &mono, columns, rows, options)
            }
            PictureKind::Spectrum => {
                // A long transform: bins narrower than the smoothing even
                // at 20 Hz.
                let stft = Stft::about(clip.sample_rate, 0.68, 4);
                let mut average = vec![0.0f64; stft.bins()];
                let mut n = 0usize;
                stft.frames(&mono, |_, p| {
                    n += 1;
                    for (a, &v) in average.iter_mut().zip(p) {
                        *a += f64::from(v);
                    }
                });
                for a in &mut average {
                    *a /= n.max(1) as f64;
                }
                let points = spectral::log_points(options.min_hz, options.max_hz, columns);
                let db =
                    spectral::smoothed_db(&average, &stft, clip.sample_rate, &points, 1.0 / 6.0)
                        .into_iter()
                        .map(|v| v as f32)
                        .collect();
                PanelData::Spectrum { db }
            }
            PictureKind::Chroma => chroma(clip, &mono, columns),
        };
        Self {
            label: label.into(),
            start_s: clip.start_s,
            duration_s: clip.duration_s(),
            grid: grid(clip),
            data,
        }
    }
}

/// Bar lines (and beat lines) over `clip`, in seconds from its start.
fn grid(clip: &Clip<'_>) -> Option<Vec<GridLine>> {
    let t = clip.timing?;
    let (from, to) = (clip.start_s, clip.start_s + clip.duration_s());
    let first_beat = t.beat_at(from).floor();
    let last_beat = t.beat_at(to).ceil();
    if last_beat - first_beat > 20_000.0 {
        return None;
    }
    let mut lines = Vec::new();
    let mut beat = first_beat;
    while beat <= last_beat {
        let at = t.seconds_at(beat);
        if at >= from - 1e-9 && at <= to + 1e-9 {
            let bar = t.bar_of_beat(beat + 1e-9);
            let starts_bar = (bar - bar.floor()) < 1e-6;
            lines.push((at - from, starts_bar.then_some(bar.floor() as u32)));
        }
        beat += 1.0;
    }
    Some(lines)
}

fn spectrogram(
    clip: &Clip<'_>,
    mono: &[f32],
    columns: usize,
    rows: usize,
    options: &PictureOptions,
) -> PanelData {
    let rate = clip.sample_rate;
    let frames = mono.len().max(1);
    let per_column = (frames as f64 / columns as f64).max(1.0);
    // Below SPLIT_HZ a long window tells low notes apart; above it a short
    // one keeps transients sharp.
    const SPLIT_HZ: f64 = 400.0;
    let short = Stft::about(rate, 0.043, 4);
    let long = Stft::about(rate, 0.085, 4);
    let short = Stft::new(short.size(), short.hop().min(per_column as usize).max(1));
    let long = Stft::new(long.size(), long.hop().min(per_column as usize).max(1));
    let (lo, hi) = (options.min_hz.ln(), options.max_hz.ln());
    // Each row's band, as a fractional bin range in its transform.
    let bands: Vec<(bool, f64, f64)> = (0..rows)
        .map(|r| {
            let f0 = (lo + (hi - lo) * r as f64 / rows as f64).exp();
            let f1 = (lo + (hi - lo) * (r + 1) as f64 / rows as f64).exp();
            let use_long = (f0 * f1).sqrt() < SPLIT_HZ;
            let size = if use_long { long.size() } else { short.size() } as f64;
            let to_bin = |f: f64| f * size / f64::from(rate);
            (use_long, to_bin(f0), to_bin(f1))
        })
        .collect();
    let mut sums = vec![0.0f64; rows * columns];
    for (stft, is_long) in [(&short, false), (&long, true)] {
        let mut counts = vec![0usize; columns];
        let mut acc = vec![0.0f64; rows * columns];
        stft.frames(mono, |center, power| {
            let column = ((center as f64 / per_column) as usize).min(columns - 1);
            counts[column] += 1;
            for (r, &(long_row, b0, b1)) in bands.iter().enumerate() {
                if long_row != is_long {
                    continue;
                }
                acc[r * columns + column] += band_density(power, b0, b1);
            }
        });
        // Columns no frame landed in take their nearest neighbor's.
        let filled: Vec<usize> = (0..columns).filter(|&c| counts[c] > 0).collect();
        if filled.is_empty() {
            continue;
        }
        let mut next = 0;
        for c in 0..columns {
            while next + 1 < filled.len()
                && (filled[next + 1] as isize - c as isize).abs()
                    <= (filled[next] as isize - c as isize).abs()
            {
                next += 1;
            }
            let src = filled[next];
            for (r, &(long_row, ..)) in bands.iter().enumerate() {
                if long_row == is_long {
                    sums[r * columns + c] = acc[r * columns + src] / counts[src] as f64;
                }
            }
        }
    }
    let db = sums
        .iter()
        .map(|&p| (10.0 * (p + 1e-24).log10()) as f32)
        .collect();
    let mut peak = vec![-200.0f32; columns];
    let mut rms = vec![-200.0f32; columns];
    for c in 0..columns {
        let a = (c as f64 * per_column) as usize;
        let b = (((c + 1) as f64 * per_column) as usize).min(clip.frames());
        if a >= b {
            continue;
        }
        let (mut p, mut sum) = (0.0f32, 0.0f64);
        for channel in clip.channels {
            for &s in &channel[a..b] {
                if s.is_finite() {
                    p = p.max(s.abs());
                    sum += f64::from(s) * f64::from(s);
                }
            }
        }
        let n = ((b - a) * clip.channels.len()).max(1) as f64;
        peak[c] = 20.0 * (p + 1e-10).log10();
        rms[c] = (10.0 * (sum / n + 1e-20).log10()) as f32;
    }
    PanelData::Spectrogram {
        columns,
        rows,
        db,
        peak,
        rms,
    }
}

/// The mean power density of the bins from `b0` to `b1` (fractional),
/// interpolated where the band is narrower than a bin.
fn band_density(power: &[f32], b0: f64, b1: f64) -> f64 {
    let last = power.len() - 1;
    let k0 = b0.ceil() as usize;
    let k1 = (b1.floor() as usize).min(last);
    if k1 > k0 {
        power[k0..k1].iter().map(|&p| f64::from(p)).sum::<f64>() / (k1 - k0) as f64
    } else {
        let mid = ((b0 + b1) / 2.0).clamp(0.0, last as f64);
        let k = mid.floor() as usize;
        let frac = mid - k as f64;
        let next = (k + 1).min(last);
        f64::from(power[k]) * (1.0 - frac) + f64::from(power[next]) * frac
    }
}

fn chroma(clip: &Clip<'_>, mono: &[f32], columns: usize) -> PanelData {
    let rate = clip.sample_rate;
    let stft = Stft::about(rate, 0.17, 8);
    let per_column = (mono.len().max(1) as f64 / columns as f64).max(1.0);
    let mut cells = vec![[0.0f64; 12]; columns];
    let mut energy = vec![0.0f64; columns];
    let mut counts = vec![0usize; columns];
    // Chords per bar, or per 2 s off the timeline.
    let spans: Vec<(f64, f64)> = match grid(clip) {
        Some(lines) => {
            let bars: Vec<f64> = lines
                .iter()
                .filter(|l| l.1.is_some())
                .map(|l| l.0)
                .collect();
            let mut edges = vec![0.0];
            edges.extend(bars.iter().copied().filter(|&b| b > 1e-6));
            edges.push(clip.duration_s());
            edges
                .windows(2)
                .map(|w| (w[0], w[1]))
                .filter(|(a, b)| b > a)
                .collect()
        }
        None => {
            let mut spans = Vec::new();
            let mut at = 0.0;
            while at < clip.duration_s() {
                spans.push((at, (at + 2.0).min(clip.duration_s())));
                at += 2.0;
            }
            spans
        }
    };
    let mut span_chroma = vec![tonal::ChromaSum::default(); spans.len()];
    stft.frames(mono, |center, power| {
        let column = ((center as f64 / per_column) as usize).min(columns - 1);
        let c = tonal::frame_chroma(power, &stft, rate);
        for (cell, v) in cells[column].iter_mut().zip(c) {
            *cell += v;
        }
        energy[column] += power.iter().map(|&p| f64::from(p)).sum::<f64>();
        counts[column] += 1;
        let at = center as f64 / f64::from(rate);
        if let Some(i) = spans.iter().position(|&(a, b)| at >= a && at < b) {
            span_chroma[i].add(&c);
        }
    });
    let loudest = energy
        .iter()
        .zip(&counts)
        .filter(|(_, n)| **n > 0)
        .map(|(e, n)| e / *n as f64)
        .fold(0.0, f64::max);
    let mut values = vec![0.0f32; 12 * columns];
    let filled: Vec<usize> = (0..columns).filter(|&c| counts[c] > 0).collect();
    for c in 0..columns {
        let Some(&src) = filled.iter().min_by_key(|&&f| f.abs_diff(c)) else {
            break;
        };
        let norm = tonal::normalized(&cells[src]);
        let level = energy[src] / counts[src] as f64;
        // Columns 40 dB or more below the loudest fade out.
        let dim = if loudest > 0.0 {
            ((10.0 * (level / loudest + 1e-20).log10() + 40.0) / 40.0).clamp(0.0, 1.0)
        } else {
            0.0
        };
        for (pc, v) in norm.iter().enumerate() {
            values[pc * columns + c] = (v * dim) as f32;
        }
    }
    let chords = spans
        .iter()
        .zip(&span_chroma)
        .filter_map(|(&(a, b), c)| Some((a, b, c.chord()?)))
        .collect();
    PanelData::Chroma {
        columns,
        values,
        chords,
    }
}

/// Draws `panels`.
pub fn picture(panels: &[Panel], options: &PictureOptions) -> Picture {
    let canvas = Canvas::new(options.width.max(LEFT + RIGHT + 16), 10);
    match options.kind {
        PictureKind::Spectrum => draw_spectrum(canvas, panels, options),
        PictureKind::Spectrogram | PictureKind::Chroma => draw_timed(canvas, panels, options),
    }
}

const BACKGROUND: [u8; 3] = [18, 20, 26];
const TEXT: [u8; 3] = [222, 226, 232];
const DIM: [u8; 3] = [150, 156, 168];
/// Curve colors on the spectrum plot, in order.
const CURVES: [([u8; 3], &str); 8] = [
    ([76, 201, 240], "cyan"),
    ([247, 37, 133], "pink"),
    ([181, 228, 140], "green"),
    ([255, 183, 3], "amber"),
    ([157, 78, 221], "purple"),
    ([255, 107, 53], "orange"),
    ([46, 196, 182], "teal"),
    ([235, 235, 235], "white"),
];

fn draw_timed(mut canvas: Canvas, panels: &[Panel], options: &PictureOptions) -> Picture {
    let plot_h = options.height.max(16);
    let chroma = options.kind == PictureKind::Chroma;
    let strip = if chroma { 0 } else { STRIP + 4 };
    let panel_h = LABEL + strip + plot_h + AXIS;
    let height = TITLE + panels.len().max(1) as u32 * (panel_h + GAP);
    canvas.resize(height);
    let plot_w = canvas.width - LEFT - RIGHT;
    canvas.text(LEFT as i32, 6, &options.title, 15.0, true, TEXT);
    // One color scale for every panel, so stems compare.
    let top_db = panels
        .iter()
        .filter_map(|p| match &p.data {
            PanelData::Spectrogram { db, .. } => db.iter().copied().reduce(f32::max),
            _ => None,
        })
        .reduce(f32::max)
        .map(|m| (m / 10.0).ceil() * 10.0)
        .unwrap_or(0.0);
    let floor_db = top_db - options.range_db as f32;
    let mut y = TITLE;
    for panel in panels {
        canvas.text(LEFT as i32, y as i32 + 2, &panel.label, 13.0, true, TEXT);
        let x0 = LEFT;
        match &panel.data {
            PanelData::Spectrogram {
                columns,
                rows,
                db,
                peak,
                rms,
            } => {
                let sy = y + LABEL;
                draw_strip(&mut canvas, x0, sy, plot_w, STRIP, peak, rms);
                let py = sy + STRIP + 4;
                for row in 0..plot_h {
                    // Row 0 of the data is the bottom.
                    let r = ((plot_h - 1 - row) as usize * rows / plot_h as usize).min(rows - 1);
                    for col in 0..plot_w {
                        let c = (col as usize * columns / plot_w as usize).min(columns - 1);
                        let v = (db[r * columns + c] - floor_db) / (top_db - floor_db);
                        canvas.set(x0 + col, py + row, magma(v));
                    }
                }
                freq_axis(&mut canvas, x0, py, plot_w, plot_h, options);
                time_axis(&mut canvas, panel, x0, py, plot_w, plot_h, true);
            }
            PanelData::Chroma {
                columns,
                values,
                chords,
            } => {
                let py = y + LABEL;
                let row_h = plot_h as f32 / 12.0;
                for row in 0..plot_h {
                    let pc = 11 - ((row as f32 / row_h) as usize).min(11);
                    for col in 0..plot_w {
                        let c = (col as usize * columns / plot_w as usize).min(columns - 1);
                        canvas.set(x0 + col, py + row, magma(values[pc * columns + c]));
                    }
                }
                for (pc, name) in PITCH_CLASSES.iter().enumerate() {
                    let cy = py as f32 + (11 - pc) as f32 * row_h + row_h / 2.0 - 7.0;
                    canvas.text_right(x0 as i32 - 6, cy as i32, name, 11.0, DIM);
                }
                time_axis(&mut canvas, panel, x0, py, plot_w, plot_h, false);
                // Chord names over their bars, where they fit.
                let px_per_s = f64::from(plot_w) / panel.duration_s.max(1e-9);
                for (a, b, name) in chords {
                    let (xa, xb) = (a * px_per_s, b * px_per_s);
                    let w = canvas.measure(name, 12.0, true);
                    if xb - xa >= f64::from(w) + 4.0 {
                        let cx = x0 as f64 + (xa + xb) / 2.0 - f64::from(w) / 2.0;
                        canvas.fill(cx as u32 - 2, py + 2, w + 4, 16, [0, 0, 0], 0.55);
                        canvas.text(cx as i32, py as i32 + 2, name, 12.0, true, TEXT);
                    }
                }
            }
            PanelData::Spectrum { .. } => {}
        }
        y += panel_h + GAP;
    }
    let caption = match options.kind {
        PictureKind::Spectrogram => {
            draw_legend(
                &mut canvas,
                TITLE + LABEL + STRIP + 4,
                plot_h,
                top_db,
                floor_db,
            );
            format!(
                "Spectrogram: time left to right, frequency up (log scale, {} to {} Hz), color \
                 the level of each frequency (dB, {} at brightest yellow, {} at black; one scale \
                 for every panel). The strip above each shows the level over time (dBFS, peak \
                 light and RMS dark; red where it reaches 0 dBFS). Vertical lines are bars \
                 (brighter, numbered) and beats.",
                options.min_hz.round(),
                options.max_hz.round(),
                top_db,
                floor_db
            )
        }
        _ => "Chromagram: time left to right, the twelve pitch classes bottom (C) to top (B), \
              brightness how much of each sounds in each moment (each moment scaled to its \
              strongest, quiet ones dimmed). Chord names are the chord each bar (or 2 s) sounds \
              like, where one fits well."
            .to_owned(),
    };
    canvas.finish(caption)
}

fn draw_strip(canvas: &mut Canvas, x0: u32, y0: u32, w: u32, h: u32, peak: &[f32], rms: &[f32]) {
    canvas.fill(x0, y0, w, h, [28, 31, 38], 1.0);
    let floor = -60.0f32;
    let to_h = |db: f32| (((db - floor) / -floor).clamp(0.0, 1.0) * h as f32) as u32;
    let columns = peak.len();
    for col in 0..w {
        let c = (col as usize * columns / w as usize).min(columns - 1);
        let (ph, rh) = (to_h(peak[c]), to_h(rms[c]));
        let clipped = peak[c] >= -0.0001;
        for dy in 0..ph {
            let color = if clipped {
                [235, 64, 52]
            } else if dy < rh {
                [70, 130, 180]
            } else {
                [140, 190, 230]
            };
            canvas.set(x0 + col, y0 + h - 1 - dy, color);
        }
    }
    for db in [0.0f32, -20.0, -40.0] {
        let yy = y0 + h - to_h(db).min(h);
        canvas.fill(x0, yy.min(y0 + h - 1), w, 1, [255, 255, 255], 0.12);
        canvas.text_right(
            x0 as i32 - 6,
            yy as i32 - 7,
            &format!("{db:.0} dB"),
            10.0,
            DIM,
        );
    }
}

/// Frequency ticks on the left of a log-frequency plot.
fn freq_axis(canvas: &mut Canvas, x0: u32, y0: u32, w: u32, h: u32, options: &PictureOptions) {
    let (lo, hi) = (options.min_hz.ln(), options.max_hz.ln());
    for hz in nice_hz(options.min_hz, options.max_hz) {
        let frac = (hz.ln() - lo) / (hi - lo);
        let y = y0 + h - 1 - (frac * f64::from(h - 1)).round() as u32;
        canvas.fill(x0, y, w, 1, [255, 255, 255], 0.16);
        canvas.text_right(x0 as i32 - 6, y as i32 - 7, &hz_label(hz), 11.0, DIM);
    }
}

/// Tick frequencies between `lo` and `hi`: 1, 2 and 5 times powers of ten,
/// fewer when the range is wide.
fn nice_hz(lo: f64, hi: f64) -> Vec<f64> {
    let octaves = (hi / lo).log2();
    let steps: &[f64] = if octaves > 6.0 {
        &[1.0, 2.0, 5.0]
    } else {
        &[1.0, 1.5, 2.0, 3.0, 5.0, 7.0]
    };
    let mut out = Vec::new();
    let mut decade = 10f64.powf(lo.log10().floor());
    while decade <= hi {
        for s in steps {
            let hz = decade * s;
            if hz >= lo * 0.999 && hz <= hi * 1.001 {
                out.push(hz);
            }
        }
        decade *= 10.0;
    }
    out
}

fn hz_label(hz: f64) -> String {
    if hz >= 1000.0 {
        let k = hz / 1000.0;
        if k.fract() == 0.0 {
            format!("{k:.0}k")
        } else {
            format!("{k:.1}k")
        }
    } else {
        format!("{hz:.0}")
    }
}

/// Bar and beat lines over a plot, and their numbers (or seconds) under it.
fn time_axis(canvas: &mut Canvas, panel: &Panel, x0: u32, y0: u32, w: u32, h: u32, beats: bool) {
    let px_per_s = f64::from(w) / panel.duration_s.max(1e-9);
    let below = y0 as i32 + h as i32 + 3;
    match &panel.grid {
        Some(lines) => {
            let bars: Vec<(f64, u32)> = lines.iter().filter_map(|&(s, b)| Some((s, b?))).collect();
            let beat_px = lines
                .windows(2)
                .map(|w| (w[1].0 - w[0].0) * px_per_s)
                .fold(f64::MAX, f64::min);
            if beats && beat_px >= 10.0 {
                for &(s, bar) in lines {
                    if bar.is_none() {
                        let x = x0 + ((s * px_per_s) as u32).min(w - 1);
                        canvas.fill(x, y0, 1, h, [255, 255, 255], 0.08);
                    }
                }
            }
            let bar_px = bars
                .windows(2)
                .map(|w| (w[1].0 - w[0].0) * px_per_s)
                .fold(f64::MAX, f64::min);
            // Number every bar, or every 2nd, 4th, ... so labels don't touch.
            let mut every = 1u32;
            while bar_px * f64::from(every) < 34.0 && every < 1024 {
                every *= 2;
            }
            for &(s, bar) in &bars {
                let x = x0 + ((s * px_per_s) as u32).min(w - 1);
                let labelled = (bar - 1) % every == 0;
                let alpha = if labelled { 0.45 } else { 0.22 };
                canvas.fill(x, y0, 1, h, [255, 255, 255], alpha);
                if labelled {
                    canvas.text(x as i32 + 3, below, &bar.to_string(), 11.0, false, DIM);
                }
            }
            canvas.text_right(x0 as i32 - 6, below, "bar", 11.0, DIM);
        }
        None => {
            let step = [
                0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0,
            ]
            .into_iter()
            .find(|s| s * px_per_s >= 70.0)
            .unwrap_or(300.0);
            let mut t = (panel.start_s / step).ceil() * step;
            while t <= panel.start_s + panel.duration_s + 1e-9 {
                let x = x0 + (((t - panel.start_s) * px_per_s) as u32).min(w - 1);
                canvas.fill(x, y0, 1, h, [255, 255, 255], 0.3);
                canvas.text(x as i32 + 3, below, &time_label(t, step), 11.0, false, DIM);
                t += step;
            }
            canvas.text_right(x0 as i32 - 6, below, "s", 11.0, DIM);
        }
    }
}

fn time_label(t: f64, step: f64) -> String {
    if t >= 60.0 && step >= 1.0 {
        format!("{}:{:02}", (t / 60.0).floor(), (t % 60.0).round() as u32)
    } else if step < 0.1 {
        format!("{t:.2}")
    } else if step < 1.0 {
        format!("{t:.1}")
    } else {
        format!("{t:.0}")
    }
}

/// The color scale on the right.
fn draw_legend(canvas: &mut Canvas, y0: u32, h: u32, top: f32, floor: f32) {
    let x0 = canvas.width - RIGHT + 14;
    for row in 0..h {
        let v = 1.0 - row as f32 / (h - 1).max(1) as f32;
        canvas.fill(x0, y0 + row, 12, 1, magma(v), 1.0);
    }
    let mut db = top;
    while db >= floor - 0.1 {
        let y = y0 + (((top - db) / (top - floor)) * (h - 1) as f32) as u32;
        canvas.text(
            x0 as i32 + 16,
            y as i32 - 7,
            &format!("{db:.0}"),
            10.0,
            false,
            DIM,
        );
        db -= 20.0;
    }
    canvas.text(x0 as i32 - 2, y0 as i32 - 16, "dB", 10.0, false, DIM);
}

fn draw_spectrum(mut canvas: Canvas, panels: &[Panel], options: &PictureOptions) -> Picture {
    let plot_h = options.height.max(32);
    canvas.resize(TITLE + plot_h + AXIS + 8);
    let plot_w = canvas.width - LEFT - RIGHT;
    let (x0, y0) = (LEFT, TITLE);
    canvas.text(LEFT as i32, 6, &options.title, 15.0, true, TEXT);
    canvas.fill(x0, y0, plot_w, plot_h, [24, 27, 34], 1.0);
    let curves: Vec<&Vec<f32>> = panels
        .iter()
        .filter_map(|p| match &p.data {
            PanelData::Spectrum { db } => Some(db),
            _ => None,
        })
        .collect();
    let top = curves
        .iter()
        .flat_map(|c| c.iter().copied())
        .reduce(f32::max)
        .map(|m| (m / 10.0).ceil() * 10.0)
        .unwrap_or(0.0);
    let floor = top - options.range_db as f32;
    let to_y = |db: f32| -> f32 {
        y0 as f32 + ((top - db) / (top - floor)).clamp(0.0, 1.0) * (plot_h - 1) as f32
    };
    let mut db = top;
    while db >= floor - 0.1 {
        let y = to_y(db) as u32;
        canvas.fill(x0, y, plot_w, 1, [255, 255, 255], 0.12);
        canvas.text_right(x0 as i32 - 6, y as i32 - 7, &format!("{db:.0}"), 11.0, DIM);
        db -= 10.0;
    }
    canvas.text_right(
        x0 as i32 - 6,
        y0 as i32 + plot_h as i32 + 3,
        "dB",
        11.0,
        DIM,
    );
    let (lo, hi) = (options.min_hz.ln(), options.max_hz.ln());
    for hz in nice_hz(options.min_hz, options.max_hz) {
        let x = x0 + (((hz.ln() - lo) / (hi - lo)) * f64::from(plot_w - 1)).round() as u32;
        canvas.fill(x, y0, 1, plot_h, [255, 255, 255], 0.12);
        canvas.text(
            x as i32 + 3,
            y0 as i32 + plot_h as i32 + 3,
            &hz_label(hz),
            11.0,
            false,
            DIM,
        );
    }
    for (i, curve) in curves.iter().enumerate() {
        let color = CURVES[i % CURVES.len()].0;
        let n = curve.len();
        let mut previous: Option<f32> = None;
        for col in 0..plot_w {
            let c = (col as usize * n / plot_w as usize).min(n - 1);
            let y = to_y(curve[c]);
            let (a, b) = match previous {
                Some(p) => (p.min(y), p.max(y)),
                None => (y, y),
            };
            canvas.fill(x0 + col, a as u32, 1, (b - a) as u32 + 2, color, 1.0);
            previous = Some(y);
        }
    }
    // The legend, top right inside the plot.
    let mut ly = y0 as i32 + 6;
    let mut lines = Vec::new();
    for (i, panel) in panels.iter().enumerate() {
        let (color, name) = CURVES[i % CURVES.len()];
        let w = canvas.measure(&panel.label, 12.0, false);
        let lx = (x0 + plot_w) as i32 - w as i32 - 30;
        canvas.fill(lx as u32 - 4, ly as u32, w + 32, 18, [0, 0, 0], 0.6);
        canvas.fill(lx as u32, ly as u32 + 8, 18, 3, color, 1.0);
        canvas.text(lx + 24, ly, &panel.label, 12.0, false, TEXT);
        ly += 18;
        lines.push(format!("{} = {name}", panel.label));
    }
    let caption = format!(
        "Average spectrum: frequency left to right (log scale, {} to {} Hz), level up (dB of \
         the power density, smoothed over a sixth of an octave; same scale for every curve, \
         so their heights compare). Curves: {}.",
        options.min_hz.round(),
        options.max_hz.round(),
        lines.join(", ")
    );
    canvas.finish(caption)
}

/// The magma color map: perceptually even, dark purple through orange to
/// pale yellow, readable in grayscale.
fn magma(v: f32) -> [u8; 3] {
    const STOPS: [[f32; 3]; 9] = [
        [0.0, 0.0, 4.0],
        [28.0, 16.0, 68.0],
        [79.0, 18.0, 123.0],
        [129.0, 37.0, 129.0],
        [181.0, 54.0, 122.0],
        [229.0, 80.0, 100.0],
        [251.0, 135.0, 97.0],
        [254.0, 194.0, 135.0],
        [252.0, 253.0, 191.0],
    ];
    let v = if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let x = v * (STOPS.len() - 1) as f32;
    let i = (x.floor() as usize).min(STOPS.len() - 2);
    let f = x - i as f32;
    let (a, b) = (STOPS[i], STOPS[i + 1]);
    [
        (a[0] + (b[0] - a[0]) * f) as u8,
        (a[1] + (b[1] - a[1]) * f) as u8,
        (a[2] + (b[2] - a[2]) * f) as u8,
    ]
}

/// An RGB canvas with text.
struct Canvas {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
    text: TextSystem,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        let mut canvas = Self {
            width,
            height: 0,
            rgb: Vec::new(),
            text: TextSystem::new(),
        };
        canvas.resize(height);
        canvas
    }

    fn resize(&mut self, height: u32) {
        self.height = height;
        self.rgb = BACKGROUND
            .iter()
            .copied()
            .cycle()
            .take((self.width * height * 3) as usize)
            .collect();
    }

    fn set(&mut self, x: u32, y: u32, color: [u8; 3]) {
        if x < self.width && y < self.height {
            let i = ((y * self.width + x) * 3) as usize;
            self.rgb[i..i + 3].copy_from_slice(&color);
        }
    }

    fn blend(&mut self, x: i32, y: i32, color: [u8; 3], alpha: f32) {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        let i = ((y as u32 * self.width + x as u32) * 3) as usize;
        for (c, new) in self.rgb[i..i + 3].iter_mut().zip(color) {
            *c = (f32::from(*c) * (1.0 - alpha) + f32::from(new) * alpha).round() as u8;
        }
    }

    fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, color: [u8; 3], alpha: f32) {
        for yy in y..(y + h).min(self.height) {
            for xx in x..(x + w).min(self.width) {
                self.blend(xx as i32, yy as i32, color, alpha);
            }
        }
    }

    fn style(size: f32, bold: bool) -> TextStyle {
        TextStyle {
            size,
            weight: if bold {
                FontWeight::SemiBold
            } else {
                FontWeight::Regular
            },
            family: FontFamily::Sans,
        }
    }

    fn measure(&mut self, text: &str, size: f32, bold: bool) -> u32 {
        self.text
            .shape(text, &Self::style(size, bold), 1.0)
            .width
            .ceil() as u32
    }

    /// Draws `text` with its line box's top left at (`x`, `y`).
    fn text(&mut self, x: i32, y: i32, text: &str, size: f32, bold: bool, color: [u8; 3]) {
        if text.is_empty() {
            return;
        }
        let line = self.text.shape(text, &Self::style(size, bold), 1.0);
        for glyph in line.glyphs {
            let Some(image) = self.text.rasterize(glyph.key) else {
                continue;
            };
            let gx = x + glyph.x + image.left;
            let gy = y + glyph.y - image.top;
            for row in 0..image.height {
                for col in 0..image.width {
                    let a = image.alpha[(row * image.width + col) as usize];
                    if a > 0 {
                        self.blend(
                            gx + col as i32,
                            gy + row as i32,
                            color,
                            f32::from(a) / 255.0,
                        );
                    }
                }
            }
        }
    }

    /// Draws `text` ending at `x`.
    fn text_right(&mut self, x: i32, y: i32, text: &str, size: f32, color: [u8; 3]) {
        let w = self.measure(text, size, false) as i32;
        self.text(x - w, y, text, size, false, color);
    }

    fn finish(self, caption: String) -> Picture {
        Picture {
            width: self.width,
            height: self.height,
            rgb: self.rgb,
            caption,
        }
    }
}

/// Prepares and draws a picture of each `(label, clip)`.
pub fn picture_of(clips: &[(String, Clip<'_>)], options: &PictureOptions) -> Picture {
    let panels: Vec<Panel> = clips
        .iter()
        .map(|(label, clip)| Panel::prepare(label.clone(), clip, options))
        .collect();
    picture(&panels, options)
}
