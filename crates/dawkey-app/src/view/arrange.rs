// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arrange view: track headers on the left; a ruler, track lanes with
//! clips and waveforms, and the playhead on the right.

use std::sync::Arc;

use dawkey_core::TrackKind;
use dawkey_ui::{
    Align, Color, Cursor, FontFamily, FontWeight, Insets, Node, Paint, Point, Rect, Role, Sizing,
    TextAlign, UiState,
};

use super::piano_roll::preview_rects;
use super::{
    ClipView, ClipViewContent, LaneView, NoteView, TrackView, ViewModel, button, format_bpm, meter,
    name_editor, tempo, toggle, track_buttons,
};
use crate::state::{
    AUTO_H, Gesture, HEADER_W, LANE_H, RULER_H, RangeKind, TAKE_H, TextField, ViewState,
    takes_shown, track_tops,
};
use crate::theme::*;

/// Most waveform columns drawn for one clip.
const MAX_COLUMNS: usize = 8192;
/// Lanes are drawn this tall; the lanes area clips the rest.
const LANES_EXTENT: f32 = 100_000.0;
/// Width of the strip at each end of a clip that trims it.
const TRIM_HANDLE_W: f32 = 6.0;
/// Most copies drawn while dragging out repeats (as many as one
/// `repeat_clip` makes).
const MAX_REPEAT_PREVIEWS: usize = 1000;

/// Each track's row top (before scrolling) and the take and automation
/// lanes it shows.
fn rows(model: &ViewModel, view: &ViewState) -> Vec<(f32, usize, usize)> {
    let shown = |t: &TrackView| {
        let autos = if view.automation_shown.contains(&t.id) {
            t.automation.len()
        } else {
            0
        };
        (takes_shown(view, t.id, t.take_lanes), autos)
    };
    let (tops, _) = track_tops(model.tracks.iter().map(shown));
    tops.into_iter()
        .zip(model.tracks.iter().map(shown))
        .map(|(top, (takes, autos))| (top, takes, autos))
        .collect()
}

pub(super) fn build(model: &ViewModel, view: &ViewState, ui: &UiState) -> Node {
    Node::row()
        .id("arrange")
        .fill()
        .child(headers(model, view, ui))
        .child(timeline(model, view, ui))
}

fn headers(model: &ViewModel, view: &ViewState, ui: &UiState) -> Node {
    let top = Node::row()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(RULER_H))
        .padding(Insets::xy(10.0, 0.0))
        .style(|s| s.background = Some(pal().toolbar))
        .child(Node::spacer())
        .child(
            button("arrange/add_track", "Add audio track", ui, None)
                .text("+ Audio")
                .padding(Insets::xy(6.0, 0.0))
                .height(Sizing::Fixed(20.0))
                .style(|s| s.font_size = 11.0),
        )
        .child(Node::row().width(Sizing::Fixed(4.0)))
        .child(
            button("arrange/add_midi_track", "Add MIDI track", ui, None)
                .text("+ MIDI")
                .padding(Insets::xy(6.0, 0.0))
                .height(Sizing::Fixed(20.0))
                .style(|s| s.font_size = 11.0),
        )
        .child(Node::row().width(Sizing::Fixed(4.0)))
        .child(
            button("arrange/add_bus", "Add bus", ui, None)
                .text("+ Bus")
                .padding(Insets::xy(6.0, 0.0))
                .height(Sizing::Fixed(20.0))
                .style(|s| s.font_size = 11.0),
        )
        .child(Node::row().width(Sizing::Fixed(4.0)))
        .child(
            toggle(
                "arrange/tempo",
                "Tempo and time signature lanes",
                "Tempo",
                view.tempo_shown,
                pal().accent,
                ui,
            )
            .padding(Insets::xy(6.0, 0.0))
            .height(Sizing::Fixed(20.0))
            .style(|s| s.font_size = 11.0),
        );
    let layout = rows(model, view);
    let rows = model.tracks.iter().enumerate().flat_map(|(i, track)| {
        let (top, takes, lanes) = layout[i];
        let y = top - view.scroll_y;
        let main = header(track, i, ui, view).absolute(Rect::new(0.0, y, HEADER_W, LANE_H - 1.0));
        let take_headers = (0..takes).map(move |k| {
            let ly = y + LANE_H + k as f32 * TAKE_H;
            take_header(track, k, ui).absolute(Rect::new(0.0, ly, HEADER_W, TAKE_H - 1.0))
        });
        let autos = track
            .automation
            .iter()
            .take(lanes)
            .enumerate()
            .map(move |(k, lane)| {
                let ly = y + LANE_H + takes as f32 * TAKE_H + k as f32 * AUTO_H;
                lane_header(model, track, k, lane).absolute(Rect::new(
                    0.0,
                    ly,
                    HEADER_W,
                    AUTO_H - 1.0,
                ))
            });
        std::iter::once(main).chain(take_headers).chain(autos)
    });
    Node::column()
        .width(Sizing::Fixed(HEADER_W))
        .height(Sizing::Fill(1.0))
        .style(|s| {
            s.background = Some(pal().panel);
            s.border = Some((1.0, pal().border));
        })
        .child(top)
        .children(
            view.tempo_shown
                .then(|| tempo::headers(model, view, ui))
                .into_iter()
                .flatten(),
        )
        .child(
            Node::column()
                .id("arrange/headers")
                .scrollable()
                .fill()
                .clip()
                .children(rows),
        )
}

/// The header of a take lane; clicking it plays that take wherever it has
/// clips.
fn take_header(track: &TrackView, k: usize, ui: &UiState) -> Node {
    let id = format!("track/{}/take/{k}", track.id);
    let clips = track.clips.iter().filter(|c| c.lane == k);
    let (playing, total) = clips.fold((0, 0), |(p, n), c| (p + usize::from(!c.muted), n + 1));
    let state = match (playing, total) {
        (0, _) => "muted",
        (p, n) if p == n => "playing",
        _ => "partly playing",
    };
    let bg = if ui.is_pressed(&id) {
        pal().pressed
    } else if ui.is_hovered(&id) {
        pal().hover
    } else {
        pal().track.mix(pal().panel, 0.5)
    };
    Node::row()
        .id(id)
        .role(Role::Button)
        .accessible_label(format!("Take {}", k + 1))
        .value(state)
        .clickable()
        .padding(Insets::xy(14.0, 0.0))
        .gap(8.0)
        .style(|s| s.background = Some(bg))
        .child(
            Node::label(format!("Take {}", k + 1))
                .width(Sizing::Fill(1.0))
                .style(|s| {
                    s.font_size = 11.0;
                    s.color = if playing > 0 {
                        pal().text
                    } else {
                        pal().text_dim
                    };
                    s.text_align = TextAlign::Start;
                }),
        )
        .child(Node::label(state).style(|s| {
            s.font_size = 10.0;
            s.color = pal().text_dim;
        }))
}

/// The header of an automation lane: what it controls, and its value at
/// the playhead.
fn lane_header(model: &ViewModel, track: &TrackView, k: usize, lane: &LaneView) -> Node {
    let value = lane_value(&lane.points, model.position_s)
        .map(|v| lane.kind.text(v))
        .unwrap_or_default();
    Node::row()
        .id(format!("track/{}/auto/{k}", track.id))
        .role(Role::ListItem)
        .accessible_label(lane.label.clone())
        .value(value.clone())
        .padding(Insets::xy(14.0, 0.0))
        .gap(8.0)
        .style(|s| s.background = Some(pal().track.mix(pal().panel, 0.5)))
        .child(
            Node::label(lane.label.clone())
                .width(Sizing::Fill(1.0))
                .clip()
                .style(|s| {
                    s.font_size = 11.0;
                    s.color = pal().text_dim;
                    s.text_align = TextAlign::Start;
                }),
        )
        .child(Node::label(value).style(|s| {
            s.font_size = 11.0;
            s.family = FontFamily::Mono;
            s.color = pal().text;
        }))
}

/// A lane's value at time `t`: linear between points, holding outside.
pub(crate) fn lane_value(points: &[(f64, f64)], t: f64) -> Option<f64> {
    let next = points.partition_point(|&(pt, _)| pt <= t);
    match (next.checked_sub(1).map(|i| points[i]), points.get(next)) {
        (None, None) => None,
        (Some((_, a)), None) => Some(a),
        (None, Some(&(_, b))) => Some(b),
        (Some((ta, a)), Some(&(tb, b))) => Some(a + (b - a) * (t - ta) / (tb - ta)),
    }
}

fn header(track: &TrackView, index: usize, ui: &UiState, view: &ViewState) -> Node {
    let id = track.id;
    let kind = match track.kind {
        TrackKind::Audio => "Audio",
        TrackKind::Midi => "MIDI",
        TrackKind::Bus => "Bus",
    };
    Node::column()
        .id(format!("track/{id}"))
        .role(Role::ListItem)
        .accessible_label(track.name.clone())
        .padding(Insets {
            left: 14.0,
            right: 10.0,
            top: 8.0,
            bottom: 8.0,
        })
        .gap(6.0)
        .align(Align::Start)
        .style(|s| s.background = Some(pal().track))
        // A color tab matching the track's clips.
        .child(
            Node::row()
                .style(|s| s.background = Some(track_color(index)))
                .absolute(Rect::new(-14.0, -8.0, 4.0, LANE_H - 1.0)),
        )
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(8.0)
                // Double-click the name to type a new one.
                .child(
                    name_editor(&TextField::TrackName(id), "Track name", view, ui)
                        .map(|field| field.width(Sizing::Fill(1.0)))
                        .unwrap_or_else(|| {
                            Node::label(track.name.clone())
                                .id(format!("track/{id}/name"))
                                .clickable()
                                .width(Sizing::Fill(1.0))
                                .clip()
                                .style(|s| {
                                    s.text_align = TextAlign::Start;
                                    s.color = pal().text;
                                })
                        }),
                )
                .children((track.take_lanes > 1).then(|| {
                    small_toggle(
                        &format!("track/{id}/takes"),
                        "Take lanes",
                        &format!("Takes {}", track.take_lanes),
                        view.takes_shown.contains(&id),
                        pal().accent,
                        ui,
                    )
                }))
                .children((track.kind == TrackKind::Audio).then(|| {
                    small_toggle(
                        &format!("track/{id}/monitor"),
                        "Input monitoring",
                        "In",
                        track.monitor,
                        pal().monitor_on,
                        ui,
                    )
                }))
                .child(Node::label(kind).style(|s| {
                    s.color = pal().text_dim;
                    s.font_size = 11.0;
                })),
        )
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(6.0)
                .children(track_buttons(track, ui))
                .child(
                    toggle(
                        &format!("track/{id}/automation"),
                        "Automation",
                        "A",
                        view.automation_shown.contains(&id),
                        pal().accent,
                        ui,
                    )
                    .size(28.0, 24.0)
                    .padding(Insets::default()),
                )
                .child(
                    Node::label(format!("{:+.1} dB", track.gain_db))
                        .id(format!("track/{id}/gain"))
                        .width(Sizing::Fixed(50.0))
                        .style(|s| {
                            s.family = FontFamily::Mono;
                            s.font_size = 11.0;
                            s.color = pal().text_dim;
                            s.text_align = TextAlign::End;
                        }),
                )
                .child(Node::spacer())
                .child(meter(
                    &format!("track/{id}/meter"),
                    "Level",
                    track.level,
                    false,
                    30.0,
                    8.0,
                )),
        )
}

/// A toggle small enough for a track header's name row.
fn small_toggle(id: &str, label: &str, text: &str, on: bool, color: Color, ui: &UiState) -> Node {
    toggle(id, label, text, on, color, ui)
        .padding(Insets::xy(6.0, 0.0))
        .height(Sizing::Fixed(18.0))
        .style(|s| s.font_size = 10.0)
}

fn timeline(model: &ViewModel, view: &ViewState, ui: &UiState) -> Node {
    let mut lanes = Node::row()
        .id("timeline/lanes")
        .role(Role::Timeline)
        .accessible_label("Timeline")
        .clickable()
        .scrollable()
        .fill()
        .clip()
        .background(pal().panel);
    if model.tracks.is_empty() {
        lanes = lanes.child(
            Node::label("Drop audio files here, or use Import…")
                .id("tracks/empty")
                .fill()
                .style(|s| s.color = pal().text_dim),
        );
    }
    let layout = rows(model, view);
    for (i, track) in model.tracks.iter().enumerate() {
        let (top, takes, shown) = layout[i];
        let y = top - view.scroll_y;
        lanes = lanes.child(
            lane(model, view, ui, track, i, takes > 0).absolute(Rect::new(
                0.0,
                y,
                LANES_EXTENT,
                LANE_H - 1.0,
            )),
        );
        for k in 0..takes {
            let ly = y + LANE_H + k as f32 * TAKE_H;
            lanes = lanes.child(take_lane(model, view, ui, track, i, k).absolute(Rect::new(
                0.0,
                ly,
                LANES_EXTENT,
                TAKE_H - 1.0,
            )));
        }
        for (k, auto) in track.automation.iter().take(shown).enumerate() {
            let ly = y + LANE_H + takes as f32 * TAKE_H + k as f32 * AUTO_H;
            lanes = lanes.child(automation_lane(view, track, i, k, auto).absolute(Rect::new(
                0.0,
                ly,
                LANES_EXTENT,
                AUTO_H - 1.0,
            )));
        }
    }
    // A clip being dragged to another track is drawn in its target lane.
    if let Some(Gesture::Clip {
        clip,
        preview_start_s,
        preview_track,
        ..
    }) = &view.gesture
    {
        let from = model
            .tracks
            .iter()
            .find(|t| t.clips.iter().any(|c| c.id == *clip));
        let target = model.tracks.iter().position(|t| t.id == *preview_track);
        if let (Some(from), Some(target)) = (from, target)
            && from.id != *preview_track
            && let Some(c) = from.clips.iter().find(|c| c.id == *clip)
        {
            let from_index = model
                .tracks
                .iter()
                .position(|t| t.id == from.id)
                .unwrap_or(0);
            let y = layout[target].0 - view.scroll_y;
            let node = clip_node(
                model,
                view,
                ui,
                c,
                from_index,
                *preview_start_s,
                Look::Moving,
            );
            lanes = lanes.child(
                Node::row()
                    .absolute(Rect::new(0.0, y, LANES_EXTENT, LANE_H - 1.0))
                    .child(node),
            );
        }
    }
    // While recording, armed lanes show what's being captured.
    if model.recording
        && let Some(start) = model.record_start_s
    {
        let (x0, x1) = (x_of(view, start), x_of(view, model.position_s));
        for (i, track) in model.tracks.iter().enumerate() {
            if track.armed && x1 > x0 {
                let y = layout[i].0 - view.scroll_y;
                lanes = lanes.child(
                    Node::row()
                        .style(|s| {
                            s.background = Some(pal().record_on.with_alpha(0.25));
                            s.border = Some((1.0, pal().record_on));
                            s.radius = [5.0; 4];
                        })
                        .absolute(Rect::new(x0, y + 3.0, x1 - x0, LANE_H - 7.0)),
                );
            }
        }
    }
    let playhead_x = x_of(view, model.position_s);
    lanes = lanes.child(Node::row().background(pal().accent).absolute(Rect::new(
        playhead_x - 1.0,
        0.0,
        2.0,
        LANES_EXTENT,
    )));
    Node::column()
        .fill()
        .clip()
        .child(ruler(model, view))
        .children(
            view.tempo_shown
                .then(|| tempo::lanes(model, view))
                .into_iter()
                .flatten(),
        )
        .child(lanes)
}

/// Logical x of time `s`, relative to the timeline's left edge.
fn x_of(view: &ViewState, s: f64) -> f32 {
    ((s - view.scroll_s) * f64::from(view.px_per_s)) as f32
}

/// An automation lane: its curve, and a handle per point.
fn automation_lane(
    view: &ViewState,
    track: &TrackView,
    index: usize,
    k: usize,
    lane: &LaneView,
) -> Node {
    let color = track_color(index);
    let h = AUTO_H - 1.0;
    let y_of = |value: f64| 4.0 + (1.0 - lane.kind.unit(value)) * (h - 8.0);
    // The point being dragged shows where it's going.
    let moving = match &view.gesture {
        Some(Gesture::AutoPoint {
            track: t,
            lane: l,
            index,
            time_s,
            value,
        }) if *t == track.id && *l == k => Some((*index, *time_s, *value)),
        _ => None,
    };
    let mut points: Vec<(usize, f64, f64)> = lane
        .points
        .iter()
        .enumerate()
        .map(|(i, &(t, v))| match moving {
            Some((m, mt, mv)) if m == i => (i, mt, mv),
            _ => (i, t, v),
        })
        .collect();
    points.sort_by(|a, b| a.1.total_cmp(&b.1));
    let visible_end = view.scroll_s + f64::from(LANES_EXTENT) / f64::from(view.px_per_s);
    let mut curve: Vec<Point> = Vec::new();
    if let Some(&(_, _, v)) = points.first() {
        curve.push(Point::new(0.0, y_of(v)));
    }
    for &(_, t, v) in &points {
        curve.push(Point::new(
            x_of(view, t).clamp(-10.0, LANES_EXTENT),
            y_of(v),
        ));
    }
    if let Some(&(_, _, v)) = points.last() {
        curve.push(Point::new(LANES_EXTENT, y_of(v)));
    }
    let mut node = Node::row()
        .id(format!("track/{}/auto/{k}/lane", track.id))
        .role(Role::Lane)
        .accessible_label(lane.label.clone())
        .clickable()
        .background(pal().lane_b.mix(pal().panel, 0.5))
        .child(
            Node::row()
                .absolute(Rect::new(0.0, 0.0, LANES_EXTENT, h))
                .paint(Paint::Polyline {
                    points: curve.into(),
                    width: 1.5,
                    color: color.mix(pal().text, 0.3),
                }),
        );
    for &(i, t, v) in points
        .iter()
        .filter(|&&(_, t, _)| t >= view.scroll_s - 1.0 && t <= visible_end)
    {
        let selected = view.selected_point == Some((track.id, k, i));
        node = node.child(
            Node::row()
                .id(format!("auto/{}/{k}/{i}", track.id))
                .role(Role::Slider)
                .accessible_label("Automation point")
                .value(format!("{t:.3} s: {}", lane.kind.text(v)))
                .clickable()
                .draggable()
                .checked(selected)
                .absolute(Rect::new(x_of(view, t) - 4.0, y_of(v) - 4.0, 8.0, 8.0))
                .style(|s| {
                    s.background = Some(if selected { pal().text } else { color });
                    s.radius = [4.0; 4];
                    s.border = Some((1.0, pal().panel));
                }),
        );
    }
    node
}

/// A track's lane. With its take lanes showing, it shows only the clips
/// that play (the comp).
fn lane(
    model: &ViewModel,
    view: &ViewState,
    ui: &UiState,
    track: &TrackView,
    index: usize,
    comp_only: bool,
) -> Node {
    let bg = if index.is_multiple_of(2) {
        pal().lane_a
    } else {
        pal().lane_b
    };
    let dragged = match &view.gesture {
        Some(Gesture::Clip {
            clip,
            preview_track,
            preview_start_s,
            copy,
            ..
        }) => Some((*clip, *preview_track, *preview_start_s, *copy)),
        _ => None,
    };
    let repeated = match &view.gesture {
        Some(Gesture::Repeat { clip, until_s, .. }) => Some((*clip, *until_s)),
        _ => None,
    };
    let trimmed = match &view.gesture {
        Some(Gesture::TrimClip {
            clip,
            start_s,
            end_s,
            ..
        }) => Some((*clip, *start_s, *end_s)),
        _ => None,
    };
    let clips = track
        .clips
        .iter()
        .filter(|c| !(comp_only && c.muted))
        .flat_map(|c| {
            let mut nodes = Vec::new();
            match (dragged, trimmed) {
                // Copying: the clip stays.
                (Some((id, to, start, true)), _) if id == c.id => {
                    nodes.push(clip_node(
                        model,
                        view,
                        ui,
                        c,
                        index,
                        c.start_s,
                        Look::Original,
                    ));
                    if to == track.id {
                        nodes.push(clip_node(model, view, ui, c, index, start, Look::Moving));
                    }
                }
                // Moving within this track: draw at the preview position.
                (Some((id, to, start, false)), _) if id == c.id && to == track.id => {
                    nodes.push(clip_node(model, view, ui, c, index, start, Look::Moving));
                }
                // Moving to another track: drawn there instead.
                (Some((id, ..)), _) if id == c.id => {}
                (_, Some((id, start_s, end_s))) if id == c.id => {
                    let c = trim_preview(c, start_s, end_s, f64::from(model.sample_rate));
                    nodes.push(clip_node(model, view, ui, &c, index, start_s, Look::Moving));
                }
                _ => nodes.push(clip_node(
                    model,
                    view,
                    ui,
                    c,
                    index,
                    c.start_s,
                    Look::Placed,
                )),
            }
            if let Some((id, until_s)) = repeated
                && id == c.id
            {
                nodes.extend(
                    repeats(c, until_s, f64::from(model.sample_rate))
                        .map(|r| clip_node(model, view, ui, &r, index, r.start_s, Look::Repeat)),
                );
            }
            nodes
        });
    Node::row()
        .id(format!("track/{}/lane", track.id))
        .role(Role::Lane)
        .accessible_label(track.name.clone())
        .background(bg)
        .children(clips)
}

/// Take lane `k` of a track: its clips (dim where muted), and the range
/// being swiped to comp.
fn take_lane(
    model: &ViewModel,
    view: &ViewState,
    ui: &UiState,
    track: &TrackView,
    index: usize,
    k: usize,
) -> Node {
    let mut node = Node::row()
        .id(format!("track/{}/take/{k}/lane", track.id))
        .role(Role::Lane)
        .accessible_label(format!("Take {}", k + 1))
        .background(pal().lane_b.mix(pal().panel, 0.5))
        .children(
            track
                .clips
                .iter()
                .filter(|c| c.lane == k)
                .map(|c| clip_node(model, view, ui, c, index, c.start_s, Look::Take)),
        );
    if let Some(Gesture::Comp {
        track: t,
        lane,
        anchor_s,
        current_s,
        ..
    }) = &view.gesture
        && *t == track.id
        && *lane as usize == k
    {
        let (x0, x1) = (
            x_of(view, anchor_s.min(*current_s)),
            x_of(view, anchor_s.max(*current_s)),
        );
        node = node.child(
            Node::row()
                .style(|s| {
                    s.background = Some(pal().text.with_alpha(0.18));
                    s.border = Some((1.0, pal().text.with_alpha(0.7)));
                    s.radius = [3.0; 4];
                })
                .absolute(Rect::new(x0, 1.0, (x1 - x0).max(1.0), TAKE_H - 3.0)),
        );
    }
    node
}

/// How a clip is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    /// In its track's lane, with trim handles.
    Placed,
    /// Being moved or trimmed.
    Moving,
    /// In a take lane (`take/<id>`): shorter, and bright only where it
    /// plays.
    Take,
    /// Left in place while a copy of it is dragged (the copy has its ID).
    Original,
    /// A copy being dragged out by repeating the clip.
    Repeat,
}

/// The copies `repeat_clip` would lay after `clip` up to `until_s`, the
/// last one cut short.
fn repeats(clip: &ClipView, until_s: f64, rate: f64) -> impl Iterator<Item = ClipView> + '_ {
    let length_s = clip.length_s;
    (1..=MAX_REPEAT_PREVIEWS)
        .map(move |n| clip.start_s + n as f64 * length_s)
        .take_while(move |&start_s| length_s > 0.0 && start_s < until_s - 1e-9)
        .map(move |start_s| {
            let copy = ClipView {
                start_s,
                ..clip.clone()
            };
            trim_preview(&copy, start_s, (start_s + length_s).min(until_s), rate)
        })
}

/// `clip` as it looks trimmed to `start_s..end_s`: what it plays stays in
/// place on the timeline, and MIDI notes before the new start go.
fn trim_preview(clip: &ClipView, start_s: f64, end_s: f64, rate: f64) -> ClipView {
    let shift_s = start_s - clip.start_s;
    let content = match &clip.content {
        ClipViewContent::Audio { offset, peaks } => ClipViewContent::Audio {
            offset: (*offset as f64 + shift_s * rate).round().max(0.0) as u64,
            peaks: peaks.clone(),
        },
        ClipViewContent::Midi { notes, controls } => ClipViewContent::Midi {
            notes: notes
                .iter()
                .filter(|n| n.start_s >= shift_s - 1e-9)
                .map(|n| NoteView {
                    start_s: n.start_s - shift_s,
                    ..*n
                })
                .collect(),
            controls: controls.clone(),
        },
    };
    ClipView {
        start_s,
        length_s: end_s - start_s,
        content,
        ..clip.clone()
    }
}

fn clip_node(
    model: &ViewModel,
    view: &ViewState,
    ui: &UiState,
    clip: &ClipView,
    track_index: usize,
    start_s: f64,
    look: Look,
) -> Node {
    let pps = f64::from(view.px_per_s);
    let x = x_of(view, start_s);
    let w = (clip.length_s * pps) as f32;
    let (top, h) = match look {
        Look::Take => (2.0, TAKE_H - 5.0),
        Look::Placed | Look::Moving | Look::Original | Look::Repeat => (3.0, LANE_H - 7.0),
    };
    let moving = matches!(look, Look::Moving | Look::Repeat);
    // Drawn only: the clip's widget is elsewhere.
    let ghost = matches!(look, Look::Original | Look::Repeat);
    let selected = view.selected_clip == Some(clip.id) && !ghost;
    let color = track_color(track_index);
    let color = if clip.muted {
        color.mix(pal().panel, if look == Look::Take { 0.75 } else { 0.6 })
    } else {
        color
    };
    let end_s = start_s + clip.length_s;

    // Only the visible part gets waveform columns.
    let visible_from = start_s.max(view.scroll_s);
    let visible_to = end_s.min(view.scroll_s + 8192.0 / pps);
    let prefix = if look == Look::Take { "take" } else { "clip" };
    let mut node = Node::row();
    if !ghost {
        node = node
            .id(format!("{prefix}/{}", clip.id))
            .role(Role::Clip)
            .accessible_label(clip.name.clone())
            .value(format!(
                "{start_s:.3} s - {end_s:.3} s{}",
                if clip.muted { " (muted)" } else { "" }
            ))
            .clickable()
            .draggable()
            .checked(selected);
    }
    let mut node = node
        .clip()
        .absolute(Rect::new(x, top, w.max(2.0), h))
        .style(|s| {
            s.background = Some(color.mix(pal().panel, if moving { 0.55 } else { 0.35 }));
            s.radius = [5.0; 4];
            s.border = Some(if selected {
                (2.0, Color::hex(0xffffff))
            } else {
                (1.0, color)
            });
        });
    // Keep the name visible when the clip starts left of the view.
    let label_boarder = 7.0;
    let label_x = (x_of(view, visible_from) - x).max(0.0) + label_boarder;
    let label_rect = Rect::new(label_x, 2.0, (w - label_x - label_boarder).max(0.0), 16.0);
    // Clips (not takes) have their name as a handle of its own:
    // double-click it to type a new one.
    let renamable = !ghost && look != Look::Take;
    let editor = name_editor(&TextField::ClipName(clip.id), "Clip name", view, ui);
    node = node.child(match editor.filter(|_| renamable) {
        Some(editor) => editor.height(Sizing::Fixed(16.0)).absolute(label_rect),
        None => {
            let label = Node::label(clip.name.clone())
                .absolute(label_rect)
                .style(|s| {
                    s.font_size = 11.0;
                    s.weight = FontWeight::SemiBold;
                    s.color = pal().text;
                    s.text_align = TextAlign::Start;
                });
            match renamable {
                true => label
                    .id(format!("clip/{}/name", clip.id))
                    .clickable()
                    .draggable(),
                false => label,
            }
        }
    });
    if let ClipViewContent::Audio {
        offset,
        peaks: Some(peaks),
    } = &clip.content
        && visible_to > visible_from
    {
        let columns = waveform_columns(
            peaks,
            f64::from(model.sample_rate),
            pps,
            *offset,
            visible_from - start_s,
            visible_to - start_s,
        );
        let wx = x_of(view, visible_from) - x;
        node = node.child(
            Node::row()
                .absolute(Rect::new(wx, 18.0, columns.len() as f32, h - 22.0))
                .paint(Paint::Waveform {
                    columns,
                    color: color.mix(pal().text, 0.35),
                }),
        );
    }
    if let ClipViewContent::Midi { notes, .. } = &clip.content {
        let visible = (x_of(view, visible_from) - x)..(x_of(view, visible_to) - x);
        let rects = preview_rects(notes, pps, w, h - 24.0, visible);
        node = node.child(
            Node::row()
                .absolute(Rect::new(0.0, 19.0, w, h - 24.0))
                .paint(Paint::Rects {
                    rects,
                    color: color.mix(pal().text, 0.45),
                }),
        );
    }
    if look == Look::Take || ghost {
        return node;
    }
    // Dragging an edge trims the clip; the middle stays for moving it.
    let handle_w = TRIM_HANDLE_W.min(w / 4.0);
    for (edge, label, hx) in [
        ("start", "Clip start", 0.0),
        ("end", "Clip end", w - handle_w),
    ] {
        let id = format!("clip/{}/{edge}", clip.id);
        let hovered = ui.is_hovered(&id) || ui.is_pressed(&id);
        node = node.child(
            Node::row()
                .id(id)
                .accessible_label(label)
                .clickable()
                .draggable()
                .cursor(Cursor::ResizeHorizontal)
                .absolute(Rect::new(hx, 0.0, handle_w, h))
                .style(|s| {
                    if hovered {
                        s.background = Some(pal().text.with_alpha(0.25));
                    }
                }),
        );
    }
    node
}

/// Min/max per logical pixel for clip time `from_s..to_s` (seconds from the
/// clip's start).
fn waveform_columns(
    peaks: &dawkey_media::Peaks,
    rate: f64,
    pps: f64,
    offset: u64,
    from_s: f64,
    to_s: f64,
) -> Arc<[(f32, f32)]> {
    let n = (((to_s - from_s) * pps).ceil().max(0.0) as usize).min(MAX_COLUMNS);
    let frame = |s: f64| (offset as f64 + s * rate).max(0.0) as usize;
    (0..n)
        .map(|i| {
            let t0 = from_s + i as f64 / pps;
            let t1 = t0 + 1.0 / pps;
            peaks.range(frame(t0), frame(t1).max(frame(t0) + 1))
        })
        .collect()
}

fn ruler(model: &ViewModel, view: &ViewState) -> Node {
    let pps = f64::from(view.px_per_s);
    let tempo = &model.tempo;
    let meter = &model.meter;
    // Sizes at the left edge of the view (the tempo and time signature may
    // change further on).
    let left_beat = tempo.beat_at_seconds(view.scroll_s).max(0.0);
    let sig = meter.meter_at(left_beat);
    let beat_s =
        sig.beat_unit() * tempo.frames_per_beat_at(left_beat) / f64::from(model.sample_rate);
    let bar = beat_s * f64::from(sig.numerator.max(1));
    // Label every n bars, with n a power of two at least 64 px apart.
    let mut every = 1u32;
    while bar * f64::from(every) * pps < 64.0 && every < (1 << 20) {
        every *= 2;
    }
    let show_beats = beat_s * pps >= 14.0 && every == 1;
    let mut ruler = Node::row()
        .id("timeline/ruler")
        .role(Role::Ruler)
        .accessible_label("Ruler")
        .clickable()
        .draggable()
        .scrollable()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(RULER_H))
        .clip()
        .style(|s| {
            s.background = Some(pal().toolbar);
            s.border = Some((1.0, pal().border));
        });
    let view_end = view.scroll_s + 8192.0 / pps;
    let first_bar = meter.bar_at(left_beat);
    let mut index = first_bar - first_bar % every;
    while tempo.seconds_at(meter.bar_start(index)) <= view_end {
        let bar_start = meter.bar_start(index);
        let sig = meter.meter_of_bar(index);
        let x = x_of(view, tempo.seconds_at(bar_start));
        ruler = ruler
            .child(
                Node::row()
                    .background(pal().border.mix(pal().text_dim, 0.4))
                    .absolute(Rect::new(x, 10.0, 1.0, RULER_H - 10.0)),
            )
            .child(
                Node::label(format!("{}", index + 1))
                    .absolute(Rect::new(x + 4.0, 0.0, 40.0, 16.0))
                    .style(|s| {
                        s.font_size = 10.0;
                        s.color = pal().text_dim;
                        s.text_align = TextAlign::Start;
                    }),
            );
        if show_beats {
            for b in 1..sig.numerator {
                let bx = x_of(
                    view,
                    tempo.seconds_at(bar_start + f64::from(b) * sig.beat_unit()),
                );
                ruler = ruler.child(Node::row().background(pal().border).absolute(Rect::new(
                    bx,
                    20.0,
                    1.0,
                    RULER_H - 20.0,
                )));
            }
        }
        let Some(next) = index.checked_add(every) else {
            break;
        };
        index = next;
    }
    // Time signature changes, after the bar number, and tempo changes
    // (unless their lanes show them).
    let marked = if view.tempo_shown { 0 } else { usize::MAX };
    for (bar, sig) in meter.changes().skip(1).take(marked) {
        let t = tempo.seconds_at(meter.bar_start(bar));
        if t < view.scroll_s - 60.0 / pps || t > view_end {
            continue;
        }
        let x = x_of(view, t);
        ruler = ruler.child(
            Node::label(format!("{}/{}", sig.numerator, sig.denominator))
                .accessible_label("Time signature change")
                .value(format!(
                    "{}/{} from bar {}",
                    sig.numerator,
                    sig.denominator,
                    bar + 1
                ))
                .absolute(Rect::new(x + 30.0, 0.0, 40.0, 14.0))
                .style(|s| {
                    s.font_size = 9.0;
                    s.color = pal().accent.mix(pal().text, 0.4);
                    s.text_align = TextAlign::Start;
                }),
        );
    }
    // Tempo changes; a ramp is a line from where it starts.
    let mut previous_s = 0.0;
    for (beat, bpm, ramp) in tempo.changes().skip(1).take(marked) {
        let t = tempo.seconds_at(beat);
        let from = std::mem::replace(&mut previous_s, t);
        if t < view.scroll_s - 60.0 / pps || (ramp && from > view_end) || (!ramp && t > view_end) {
            continue;
        }
        let x = x_of(view, t);
        if ramp {
            let x0 = x_of(view, from).max(-1.0);
            ruler = ruler.child(
                Node::row()
                    .background(pal().accent.mix(pal().text_dim, 0.3))
                    .absolute(Rect::new(x0, RULER_H - 2.0, (x - x0).max(1.0), 1.0)),
            );
        }
        let value = if ramp {
            format!("{bpm} BPM at beat {beat}, ramping from the previous tempo")
        } else {
            format!("{bpm} BPM at beat {beat}")
        };
        ruler = ruler
            .child(
                Node::row()
                    .background(pal().accent.mix(pal().text_dim, 0.3))
                    .absolute(Rect::new(x, 14.0, 1.0, RULER_H - 14.0)),
            )
            .child(
                Node::label(format!("{} BPM", format_bpm(bpm)))
                    .accessible_label(if ramp { "Tempo ramp" } else { "Tempo change" })
                    .value(value)
                    .absolute(Rect::new(x + 3.0, 14.0, 60.0, 12.0))
                    .style(|s| {
                        s.font_size = 9.0;
                        s.color = pal().accent.mix(pal().text, 0.4);
                        s.text_align = TextAlign::Start;
                    }),
            );
    }
    // Loop and punch ranges; a range being dragged shows instead.
    let dragged = match &view.gesture {
        Some(Gesture::Range {
            kind,
            anchor_s,
            current_s,
        }) => Some((*kind, anchor_s.min(*current_s), anchor_s.max(*current_s))),
        _ => None,
    };
    let ranges = [
        (
            RangeKind::Loop,
            model.loop_range,
            model.loop_enabled,
            pal().accent,
            0.0,
        ),
        (
            RangeKind::Punch,
            model.punch_range,
            model.punch_enabled,
            pal().record_on,
            4.0,
        ),
    ];
    for (kind, range, enabled, color, y) in ranges {
        let range = match dragged {
            Some((k, a, b)) if k == kind => Some((a, b)),
            _ => range,
        };
        let Some((start, end)) = range else {
            continue;
        };
        let (x0, x1) = (x_of(view, start), x_of(view, end));
        let name = match kind {
            RangeKind::Loop => "loop",
            RangeKind::Punch => "punch",
        };
        ruler = ruler.child(
            Node::row()
                .id(format!("timeline/{name}"))
                .accessible_label(if kind == RangeKind::Loop {
                    "Loop range"
                } else {
                    "Punch range"
                })
                .value(format!(
                    "{start:.3} s - {end:.3} s{}",
                    if enabled { "" } else { " (off)" }
                ))
                .style(|s| {
                    s.background = Some(color.with_alpha(if enabled { 0.55 } else { 0.2 }));
                    s.radius = [2.0; 4];
                })
                .absolute(Rect::new(x0, y, (x1 - x0).max(1.0), 4.0)),
        );
    }
    let px = x_of(view, model.position_s);
    ruler.child(Node::row().background(pal().accent).absolute(Rect::new(
        px - 1.0,
        0.0,
        2.0,
        RULER_H,
    )))
}

fn track_color(index: usize) -> Color {
    TRACK_COLORS[index % TRACK_COLORS.len()]
}
