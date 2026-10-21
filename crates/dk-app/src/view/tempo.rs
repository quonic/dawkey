// SPDX-License-Identifier: AGPL-3.0-or-later
//! The tempo and time signature lanes under the arrange view's ruler: the
//! tempo as a curve with a point per change (a ramp is a slope to its
//! point), and a chip per time signature change. Their headers edit the
//! selected point, or the one in effect at the playhead.

use dk_core::{TEMPO_BPM_MAX, TEMPO_BPM_MIN, TimeSignature};
use dk_ui::{FontFamily, FontWeight, Insets, Node, Paint, Point, Rect, Role, Sizing, UiState};

use super::{ViewModel, button, format_bpm, text_field, toggle};
use crate::state::{Gesture, HEADER_W, METER_H, Marker, TEMPO_H, TextField, ViewState};
use crate::theme::*;

/// Lanes are drawn this wide; the timeline clips the rest.
const EXTENT: f32 = 100_000.0;

/// A tempo point: (beat, BPM, ramps here from the previous point).
pub(crate) type TempoPoint = (f64, f64, bool);

/// The tempo lane's BPM range for these points: their range with some
/// room above and below.
pub(crate) fn tempo_scale(points: &[TempoPoint]) -> (f64, f64) {
    let lo = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let hi = points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    if !lo.is_finite() {
        return (100.0, 140.0);
    }
    let pad = ((hi - lo) * 0.15).max(10.0);
    ((lo - pad).max(TEMPO_BPM_MIN), (hi + pad).min(TEMPO_BPM_MAX))
}

/// Logical y of `bpm` in the tempo lane.
pub(crate) fn tempo_y(scale: (f64, f64), bpm: f64) -> f32 {
    let (lo, hi) = scale;
    let unit = ((bpm - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
    6.0 + (1.0 - unit) * (TEMPO_H - 13.0)
}

/// The BPM at logical y in the tempo lane.
pub(crate) fn tempo_at_y(scale: (f64, f64), y: f32) -> f64 {
    let (lo, hi) = scale;
    let unit = 1.0 - f64::from((y - 6.0) / (TEMPO_H - 13.0));
    lo + unit.clamp(0.0, 1.0) * (hi - lo)
}

/// Index of the point in effect at `beat` (the last one at or before it).
pub(crate) fn point_at(beats: impl Iterator<Item = f64>, beat: f64) -> usize {
    beats
        .enumerate()
        .take_while(|&(i, b)| i == 0 || b <= beat + 1e-9)
        .last()
        .map_or(0, |(i, _)| i)
}

/// "beat 8.5" for positions in tool and accessible text.
fn beat_text(beat: f64) -> String {
    format!("beat {}", format_bpm(beat))
}

/// The tempo points to draw: the project's, with the one being dragged
/// where it's going.
fn tempo_points(model: &ViewModel, view: &ViewState) -> Vec<TempoPoint> {
    let mut points: Vec<TempoPoint> = model.tempo.changes().collect();
    if let Some(Gesture::TempoPoint {
        index, beat, bpm, ..
    }) = &view.gesture
        && let Some(p) = points.get_mut(*index)
    {
        p.0 = *beat;
        p.1 = *bpm;
    }
    points
}

/// The time signature changes to draw: (bar from 0, time signature), with
/// the one being dragged where it's going.
fn meter_points(model: &ViewModel, view: &ViewState) -> Vec<(u32, TimeSignature)> {
    let mut points: Vec<(u32, TimeSignature)> = model.meter.changes().collect();
    if let Some(Gesture::MeterChange { index, bar, .. }) = &view.gesture
        && let Some(p) = points.get_mut(*index)
    {
        p.0 = *bar;
    }
    points
}

/// The tempo point the header edits: the selected one, else the one in
/// effect at the playhead.
fn current_tempo(model: &ViewModel, view: &ViewState, count: usize) -> usize {
    match view.selected_marker {
        Some(Marker::Tempo(n)) if n < count => n,
        _ => {
            let beat = model.tempo.beat_at_seconds(model.position_s);
            point_at(model.tempo.changes().map(|c| c.0), beat)
        }
    }
}

fn current_meter(model: &ViewModel, view: &ViewState, count: usize) -> usize {
    match view.selected_marker {
        Some(Marker::Meter(n)) if n < count => n,
        _ => {
            let beat = model.tempo.beat_at_seconds(model.position_s);
            let bar = model.meter.bar_at(beat);
            point_at(
                model.meter.changes().map(|c| f64::from(c.0)),
                f64::from(bar),
            )
        }
    }
}

fn small_button(id: &str, label: &str, text: &str, ui: &UiState) -> Node {
    button(id, label, ui, None)
        .text(text)
        .size(20.0, 18.0)
        .style(|s| s.font_size = 11.0)
}

/// The tempo point's BPM, to click and type a new one.
fn tempo_field(view: &ViewState, n: usize, bpm: f64, ui: &UiState) -> Node {
    let editing = view
        .text_edit
        .as_ref()
        .filter(|e| e.field == TextField::Tempo(n));
    let text = editing.map_or_else(|| format_bpm(bpm), |e| e.text.clone());
    Node::row()
        .width(Sizing::Fixed(62.0))
        .gap(3.0)
        .child(text_field(
            "tempo/value",
            "Tempo in BPM",
            text,
            editing.map(|e| e.selected),
            ui,
        ))
        .child(Node::label("BPM").style(|s| {
            s.font_size = 10.0;
            s.color = pal().text_dim;
        }))
}

fn header_label(text: &str) -> Node {
    Node::label(text).width(Sizing::Fixed(40.0)).style(|s| {
        s.font_size = 11.0;
        s.color = pal().text_dim;
        s.text_align = dk_ui::TextAlign::Start;
    })
}

/// The headers of the tempo and time signature lanes.
pub(super) fn headers(model: &ViewModel, view: &ViewState, ui: &UiState) -> [Node; 2] {
    let points = tempo_points(model, view);
    let n = current_tempo(model, view, points.len());
    let (beat, bpm, ramp) = points[n];
    let at = if n == 0 {
        "at the start".to_owned()
    } else {
        format!("at {}", beat_text(beat))
    };
    let mut tempo = Node::row()
        .id("tempo/header")
        .role(Role::ListItem)
        .accessible_label("Tempo")
        .value(format!("{} BPM {at}", format_bpm(bpm)))
        .size(HEADER_W, TEMPO_H - 1.0)
        .padding(Insets::xy(14.0, 0.0))
        .gap(4.0)
        .style(|s| s.background = Some(pal().track.mix(pal().panel, 0.5)))
        .child(header_label("Tempo"))
        .child(tempo_field(view, n, bpm, ui))
        .child(small_button("tempo/down", "Slower", "−", ui))
        .child(small_button("tempo/up", "Faster", "+", ui));
    if n > 0 {
        tempo = tempo
            .child(
                toggle("tempo/ramp", "Ramp", "Ramp", ramp, pal().accent, ui)
                    .height(Sizing::Fixed(18.0))
                    .padding(Insets::xy(5.0, 0.0))
                    .style(|s| s.font_size = 10.0),
            )
            .child(small_button("tempo/remove", "Remove tempo change", "×", ui));
    }

    let changes = meter_points(model, view);
    let m = current_meter(model, view, changes.len());
    let (bar, sig) = changes[m];
    // The numerator and denominator, to click and type.
    let field = |id: &str, label: &str, field: TextField, value: u8| {
        let editing = view.text_edit.as_ref().filter(|e| e.field == field);
        let text = editing.map_or_else(|| value.to_string(), |e| e.text.clone());
        Node::row()
            .width(Sizing::Fixed(20.0))
            .justify(dk_ui::Justify::Center)
            .child(text_field(id, label, text, editing.map(|e| e.selected), ui))
    };
    let mut meter = Node::row()
        .id("meter/header")
        .role(Role::ListItem)
        .accessible_label("Time signature")
        .value(format!(
            "{}/{} from bar {}",
            sig.numerator,
            sig.denominator,
            bar + 1
        ))
        .size(HEADER_W, METER_H - 1.0)
        .padding(Insets::xy(14.0, 0.0))
        .gap(2.0)
        .style(|s| s.background = Some(pal().track.mix(pal().panel, 0.5)))
        .child(header_label("Meter"))
        .child(small_button("meter/numerator/down", "Fewer beats", "−", ui))
        .child(field(
            "meter/numerator/value",
            "Beats per bar",
            TextField::MeterNumerator(m),
            sig.numerator,
        ))
        .child(small_button("meter/numerator/up", "More beats", "+", ui))
        .child(Node::label("/").width(Sizing::Fixed(8.0)).style(|s| {
            s.font_size = 11.0;
            s.family = FontFamily::Mono;
            s.weight = FontWeight::SemiBold;
            s.color = pal().text;
        }))
        .child(small_button(
            "meter/denominator/down",
            "Longer beats",
            "−",
            ui,
        ))
        .child(field(
            "meter/denominator/value",
            "Beat length",
            TextField::MeterDenominator(m),
            sig.denominator,
        ))
        .child(small_button(
            "meter/denominator/up",
            "Shorter beats",
            "+",
            ui,
        ));
    if m > 0 {
        meter = meter
            .child(Node::row().width(Sizing::Fixed(6.0)))
            .child(small_button(
                "meter/remove",
                "Remove time signature change",
                "×",
                ui,
            ));
    }
    [tempo, meter]
}

/// Logical x of time `s` in the lanes.
fn x_of(view: &ViewState, s: f64) -> f32 {
    ((s - view.scroll_s) * f64::from(view.px_per_s)) as f32
}

/// The tempo lane and the time signature lane.
pub(super) fn lanes(model: &ViewModel, view: &ViewState) -> [Node; 2] {
    let tempo = &model.tempo;
    let points = tempo_points(model, view);
    let scale = match &view.gesture {
        Some(Gesture::TempoPoint { scale, .. }) => *scale,
        _ => tempo_scale(&points),
    };
    let h = TEMPO_H - 1.0;
    let y = |bpm: f64| tempo_y(scale, bpm);
    // Points are placed by beat on the ruler's map, so one being dragged
    // stays under the pointer.
    let x = |beat: f64| x_of(view, tempo.seconds_at(beat));
    let mut sorted: Vec<TempoPoint> = points.clone();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut curve: Vec<Point> = Vec::new();
    for (i, &(beat, bpm, _)) in sorted.iter().enumerate() {
        let (px, py) = (x(beat), y(bpm));
        if i == 0 {
            curve.push(Point::new(px.min(-10.0), py));
        }
        curve.push(Point::new(px, py));
        match sorted.get(i + 1) {
            // A jump: hold, then step.
            Some(&(next, _, false)) => curve.push(Point::new(x(next), py)),
            Some(_) => {}
            None => curve.push(Point::new(EXTENT, py)),
        }
    }
    let curve: Vec<Point> = clip_polyline(&curve, -10.0, EXTENT);
    let visible = (view.scroll_s - 1.0)..=(view.scroll_s + f64::from(EXTENT / view.px_per_s));
    let mut lane = Node::row()
        .id("tempo/lane")
        .role(Role::Lane)
        .accessible_label("Tempo")
        .clickable()
        .scrollable()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(h))
        .clip()
        .background(pal().lane_b.mix(pal().panel, 0.5))
        .child(
            Node::row()
                .absolute(Rect::new(0.0, 0.0, EXTENT, h))
                .paint(Paint::Polyline {
                    points: curve.into(),
                    width: 1.5,
                    color: pal().accent.mix(pal().text, 0.3),
                }),
        );
    for (i, &(beat, bpm, ramp)) in points.iter().enumerate() {
        if !visible.contains(&tempo.seconds_at(beat)) {
            continue;
        }
        let selected = view.selected_marker == Some(Marker::Tempo(i));
        let (px, py) = (x(beat), y(bpm));
        let value = format!(
            "{} BPM at {}{}",
            format_bpm(bpm),
            beat_text(beat),
            if ramp {
                ", ramping from the previous"
            } else {
                ""
            }
        );
        lane = lane
            .child(
                Node::row()
                    .id(format!("tempo/{i}"))
                    .role(Role::Slider)
                    .accessible_label(if i == 0 {
                        "Start tempo"
                    } else {
                        "Tempo change"
                    })
                    .value(value)
                    .clickable()
                    .draggable()
                    .checked(selected)
                    .absolute(Rect::new(px - 4.0, py - 4.0, 8.0, 8.0))
                    .style(|s| {
                        s.background = Some(if selected { pal().text } else { pal().accent });
                        s.radius = [4.0; 4];
                        s.border = Some((1.0, pal().panel));
                    }),
            )
            .child(
                Node::label(format_bpm(bpm))
                    .absolute(Rect::new(px + 6.0, (py - 13.0).max(0.0), 44.0, 12.0))
                    .style(|s| {
                        s.font_size = 9.0;
                        s.color = pal().text_dim;
                        s.text_align = dk_ui::TextAlign::Start;
                    }),
            );
    }

    let meter = &model.meter;
    let mut signatures = Node::row()
        .id("meter/lane")
        .role(Role::Lane)
        .accessible_label("Time signatures")
        .clickable()
        .scrollable()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(METER_H - 1.0))
        .clip()
        .background(pal().lane_b.mix(pal().panel, 0.5));
    for (i, (bar, sig)) in meter_points(model, view).into_iter().enumerate() {
        let t = tempo.seconds_at(meter.bar_start(bar));
        if !visible.contains(&t) {
            continue;
        }
        let selected = view.selected_marker == Some(Marker::Meter(i));
        let text = format!("{}/{}", sig.numerator, sig.denominator);
        signatures = signatures.child(
            Node::label(text.clone())
                .id(format!("meter/{i}"))
                .role(Role::Slider)
                .accessible_label(if i == 0 {
                    "Start time signature"
                } else {
                    "Time signature change"
                })
                .value(format!("{text} from bar {}", bar + 1))
                .clickable()
                .draggable()
                .checked(selected)
                .absolute(Rect::new(x_of(view, t), 3.0, 34.0, METER_H - 7.0))
                .style(|s| {
                    s.font_size = 10.0;
                    s.weight = FontWeight::SemiBold;
                    s.color = if selected { pal().panel } else { pal().text };
                    s.background = Some(if selected {
                        pal().text
                    } else {
                        pal().accent.mix(pal().panel, 0.55)
                    });
                    s.radius = [3.0; 4];
                }),
        );
    }
    [lane, signatures]
}

/// The part of a polyline between x = `from` and x = `to`, cut where it
/// crosses them (so slopes stay true at the edges).
fn clip_polyline(points: &[Point], from: f32, to: f32) -> Vec<Point> {
    let mut out = Vec::new();
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if (a.x < from && b.x < from) || (a.x > to && b.x > to) {
            continue;
        }
        let at = |x: f32| {
            if b.x == a.x {
                a
            } else {
                Point::new(x, a.y + (b.y - a.y) * (x - a.x) / (b.x - a.x))
            }
        };
        let start = if a.x < from { at(from) } else { a };
        let end = if b.x > to { at(to) } else { b };
        if out.last() != Some(&start) {
            out.push(start);
        }
        out.push(end);
    }
    out
}
