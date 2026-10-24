// SPDX-License-Identifier: AGPL-3.0-or-later
//! The piano roll: one MIDI clip's notes on a pitch × time grid, with a
//! keyboard on the left and the track's synth settings above.
//!
//! Times are beats (quarter notes) from the clip's start, like the MIDI
//! commands.

use dawkey_core::{Controller, MeterMap, NoteId, Waveform};
use dawkey_ui::{
    Color, FontWeight, Insets, Node, Paint, Point, Rect, Role, Sizing, TextAlign, UiState,
};

use super::{
    ClipView, ClipViewContent, ControlView, NoteView, TrackView, ViewModel, button, format_time,
    text_field, toggle,
};
use crate::state::{
    Gesture, KEYS_W, ROLL_LANE_H, ROW_H, RollLane, RollView, SLIDER_W, SynthParam, TextField,
    ViewState, latency_position,
};
use crate::theme::*;

/// Height of the beat ruler above the grid.
const RULER_H: f32 = 22.0;
/// Rows and columns are drawn this far; the grid clips the rest.
const ROWS_DRAWN: usize = 110;
const GRID_EXTENT: f32 = 8192.0;
/// Width of a note's resize handle.
const HANDLE_W: f32 = 6.0;

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// "C4" for 60.
pub(crate) fn pitch_name(pitch: u8) -> String {
    format!(
        "{}{}",
        NOTE_NAMES[usize::from(pitch % 12)],
        i32::from(pitch / 12) - 1
    )
}

fn is_black(pitch: u8) -> bool {
    matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

/// Grid-relative x of beat `b`.
pub(crate) fn x_of(roll: &RollView, b: f64) -> f32 {
    ((b - roll.scroll_beats) * f64::from(roll.px_per_beat)) as f32
}

/// Grid-relative y of the top of pitch row `p`.
pub(crate) fn y_of(roll: &RollView, p: i32) -> f32 {
    (127 - p) as f32 * ROW_H - roll.scroll_y
}

/// The edited clip and its track, if it still exists.
pub(crate) fn edited<'a>(
    model: &'a ViewModel,
    view: &ViewState,
) -> Option<(usize, &'a TrackView, &'a ClipView)> {
    let id = view.editing_clip?;
    model.tracks.iter().enumerate().find_map(|(i, t)| {
        t.clips
            .iter()
            .find(|c| c.id == id && matches!(c.content, ClipViewContent::Midi { .. }))
            .map(|c| (i, t, c))
    })
}

pub(super) fn build(model: &ViewModel, view: &ViewState, ui: &UiState) -> Node {
    let mut page = Node::column()
        .id("piano_roll")
        .accessible_label("Piano roll")
        .fill()
        .clip()
        .background(pal().panel);
    let Some((index, track, clip)) = edited(model, view) else {
        return page.child(
            Node::label("Double-click a MIDI clip to edit it")
                .fill()
                .style(|s| s.color = pal().text_dim),
        );
    };
    let ClipViewContent::Midi { notes, controls } = &clip.content else {
        unreachable!("edited() only finds MIDI clips");
    };
    let tempo = &model.tempo;
    let start_beat = tempo.beat_at_seconds(clip.start_s);
    let clip_beats = tempo.beat_at_seconds(clip.start_s + clip.length_s) - start_beat;
    let bars = Bars {
        meter: &model.meter,
        start: start_beat,
    };
    let playhead = tempo.beat_at_seconds(model.position_s) - start_beat;
    page = page
        .child(header(track, clip, notes.len(), view, ui))
        .child(
            synth_bar(track, ui)
                .child(Node::spacer())
                .children(devices(track, view, ui)),
        )
        .child(
            Node::row().fill().child(keys(&view.piano_roll)).child(
                Node::column()
                    .fill()
                    .clip()
                    .child(ruler(&view.piano_roll, clip_beats, bars, playhead))
                    .child(grid(
                        view,
                        notes,
                        track_color(index),
                        clip_beats,
                        bars,
                        playhead,
                    )),
            ),
        )
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .height(Sizing::Fixed(ROLL_LANE_H))
                .child(lane_header(view, ui))
                .child(lane(view, notes, controls, track_color(index), clip_beats)),
        );
    page
}

/// Value (in the lane's range) → y in the lane.
pub(crate) fn lane_y(lane: RollLane, value: f64) -> f32 {
    let (lo, hi) = lane.range();
    let unit = ((value - f64::from(lo)) / f64::from(hi - lo)).clamp(0.0, 1.0) as f32;
    4.0 + (1.0 - unit) * (ROLL_LANE_H - 9.0)
}

/// y in the lane → value, rounded.
pub(crate) fn lane_value(lane: RollLane, y: f32) -> u16 {
    let (lo, hi) = lane.range();
    let unit = (1.0 - (y - 4.0) / (ROLL_LANE_H - 9.0)).clamp(0.0, 1.0);
    (f64::from(lo) + f64::from(unit) * f64::from(hi - lo)).round() as u16
}

/// The lane's name; clicking it shows the next kind of lane.
fn lane_header(view: &ViewState, ui: &UiState) -> Node {
    let label = view.roll_lane.label();
    button("piano_roll/lane_kind", "Lane: click for the next", ui, None)
        .value(label.clone())
        .width(Sizing::Fixed(KEYS_W))
        .height(Sizing::Fill(1.0))
        .child(Node::label(label).fill().style(|s| {
            s.font_size = 10.0;
            s.color = pal().text_dim;
            s.text_align = TextAlign::Center;
        }))
}

/// Velocities, or one controller's values, under the grid.
fn lane(
    view: &ViewState,
    notes: &[NoteView],
    controls: &[ControlView],
    color: Color,
    clip_beats: f64,
) -> Node {
    let roll = &view.piano_roll;
    let kind = view.roll_lane;
    let mut lane = Node::row()
        .id("piano_roll/lane")
        .role(Role::Lane)
        .accessible_label(kind.label())
        .clickable()
        .draggable()
        .scrollable()
        .fill()
        .clip()
        .style(|s| {
            s.background = Some(pal().lane_b.mix(pal().panel, 0.5));
            s.border = Some((1.0, pal().border));
        });
    lane = lane.child(
        Node::row()
            .background(Color::rgba(0.0, 0.0, 0.0, 0.3))
            .absolute(Rect::new(
                x_of(roll, clip_beats).max(0.0),
                0.0,
                GRID_EXTENT,
                ROLL_LANE_H,
            )),
    );
    let painted = match &view.gesture {
        Some(Gesture::Paint { lane, path, .. }) if *lane == kind => Some(path.as_slice()),
        _ => None,
    };
    match kind.controller() {
        None => {
            let velocity = |note: &NoteView| -> u8 {
                match &view.gesture {
                    Some(Gesture::Velocity { notes, delta, .. }) => notes
                        .iter()
                        .find(|(id, _)| *id == note.id)
                        .map_or(note.velocity, |(_, v)| {
                            (i32::from(*v) + delta).clamp(1, 127) as u8
                        }),
                    _ => note.velocity,
                }
            };
            for note in notes {
                let v = velocity(note);
                let x = x_of(roll, note.start_beats);
                let y = lane_y(kind, f64::from(v));
                let selected = view.selected_notes.contains(&note.id);
                lane = lane.child(
                    Node::row()
                        .id(format!("note/{}/velocity", note.id))
                        .role(Role::Slider)
                        .accessible_label("Velocity")
                        .value(v.to_string())
                        .draggable()
                        .absolute(Rect::new(x - 1.0, y, 5.0, ROLL_LANE_H - y))
                        .style(|s| {
                            s.background = Some(if selected { pal().text } else { color });
                            s.radius = [2.0, 2.0, 0.0, 0.0];
                        }),
                );
            }
        }
        Some(controller) => {
            lane = control_curve(lane, view, kind, controller, controls, color);
        }
    }
    if let Some(path) = painted {
        for &(beat, value) in path {
            lane = lane.child(Node::row().background(pal().text.with_alpha(0.7)).absolute(
                Rect::new(
                    x_of(roll, beat) - 2.0,
                    lane_y(kind, f64::from(value)) - 2.0,
                    4.0,
                    4.0,
                ),
            ));
        }
    }
    lane
}

/// A controller's values as steps, with a handle per change.
fn control_curve(
    mut lane: Node,
    view: &ViewState,
    kind: RollLane,
    controller: Controller,
    controls: &[ControlView],
    color: Color,
) -> Node {
    let roll = &view.piano_roll;
    // The point being dragged shows where it's going.
    let mut points: Vec<(usize, f64, u16)> = controls
        .iter()
        .filter(|c| c.controller == controller)
        .enumerate()
        .map(|(i, c)| match &view.gesture {
            Some(Gesture::ControlPoint {
                index, beat, value, ..
            }) if *index == i => (i, *beat, *value),
            _ => (i, c.beat, c.value),
        })
        .collect();
    points.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut y = lane_y(kind, f64::from(controller.default_value()));
    let mut curve = vec![Point::new(0.0, y)];
    for &(_, beat, value) in &points {
        let x = x_of(roll, beat).clamp(-10.0, GRID_EXTENT);
        curve.push(Point::new(x, y));
        y = lane_y(kind, f64::from(value));
        curve.push(Point::new(x, y));
    }
    curve.push(Point::new(GRID_EXTENT, y));
    lane = lane.child(
        Node::row()
            .absolute(Rect::new(0.0, 0.0, GRID_EXTENT, ROLL_LANE_H))
            .paint(Paint::Polyline {
                points: curve.into(),
                width: 1.5,
                color: color.mix(pal().text, 0.3),
            }),
    );
    let end = roll.scroll_beats + f64::from(GRID_EXTENT / roll.px_per_beat);
    for &(i, beat, value) in points
        .iter()
        .filter(|&&(_, b, _)| b >= roll.scroll_beats - 1.0 && b <= end)
    {
        let selected = view.selected_control == Some(i);
        lane = lane.child(
            Node::row()
                .id(format!("piano_roll/point/{i}"))
                .role(Role::Slider)
                .accessible_label(format!("{} change", kind.label()))
                .value(format!("{beat:.3} beats: {value}"))
                .clickable()
                .draggable()
                .checked(selected)
                .absolute(Rect::new(
                    x_of(roll, beat) - 4.0,
                    lane_y(kind, f64::from(value)) - 4.0,
                    8.0,
                    8.0,
                ))
                .style(|s| {
                    s.background = Some(if selected { pal().text } else { color });
                    s.radius = [4.0; 4];
                    s.border = Some((1.0, pal().panel));
                }),
        );
    }
    lane
}

/// The notes a marquee from `a` to `b` (beat, pitch) touches.
pub(crate) fn marquee_notes(notes: &[NoteView], a: (f64, i32), b: (f64, i32)) -> Vec<NoteId> {
    let (b0, b1) = (a.0.min(b.0), a.0.max(b.0));
    let (p0, p1) = (a.1.min(b.1), a.1.max(b.1));
    notes
        .iter()
        .filter(|n| {
            let p = i32::from(n.pitch);
            n.start_beats < b1 && n.start_beats + n.length_beats > b0 && (p0..=p1).contains(&p)
        })
        .map(|n| n.id)
        .collect()
}

fn header(
    track: &TrackView,
    clip: &ClipView,
    notes: usize,
    view: &ViewState,
    ui: &UiState,
) -> Node {
    let grid = view.piano_roll.grid_beats;
    let grid_button = |division: u32| {
        let on = (grid - 4.0 / f64::from(division)).abs() < 1e-9;
        toggle(
            &format!("piano_roll/grid/{division}"),
            &format!("Grid 1/{division}"),
            &format!("1/{division}"),
            on,
            pal().text_dim,
            ui,
        )
    };
    let count = match notes {
        1 => "1 note".to_owned(),
        n => format!("{n} notes"),
    };
    Node::row()
        .id("piano_roll/header")
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(40.0))
        .padding(Insets::xy(12.0, 0.0))
        .gap(8.0)
        .background(pal().toolbar)
        .child(
            Node::label(format!(
                "{} · clip at {}",
                track.name,
                format_time(clip.start_s)
            ))
            .id("piano_roll/title")
            .value(count)
            .style(|s| {
                s.color = pal().text;
                s.weight = FontWeight::SemiBold;
            }),
        )
        .child(Node::row().width(Sizing::Fixed(12.0)))
        .child(Node::label("Grid").style(|s| {
            s.color = pal().text_dim;
            s.font_size = 11.0;
        }))
        .children([4, 8, 16, 32].map(grid_button))
        .child(
            button(
                "piano_roll/quantize",
                "Quantize notes to the grid",
                ui,
                None,
            )
            .text("Quantize")
            .padding(Insets::xy(10.0, 0.0))
            .height(Sizing::Fixed(28.0)),
        )
        .child(Node::spacer())
        .child(
            button("piano_roll/close", "Back to arrange", ui, None)
                .text("Close")
                .padding(Insets::xy(12.0, 0.0))
                .height(Sizing::Fixed(28.0)),
        )
}

fn synth_bar(track: &TrackView, ui: &UiState) -> Node {
    let params = track.synth.unwrap_or_default();
    let id = track.id;
    let waves = [
        (Waveform::Saw, "saw", "Saw"),
        (Waveform::Square, "square", "Square"),
        (Waveform::Triangle, "triangle", "Tri"),
        (Waveform::Sine, "sine", "Sine"),
    ];
    let mut bar = Node::row()
        .id("piano_roll/synth")
        .accessible_label("Synth")
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(36.0))
        .padding(Insets::xy(12.0, 0.0))
        .gap(6.0)
        .style(|s| {
            s.background = Some(pal().panel);
            s.border = Some((1.0, pal().border));
        })
        .child(Node::label("Synth").style(|s| {
            s.color = pal().text_dim;
            s.font_size = 11.0;
        }));
    for (wave, name, text) in waves {
        bar = bar.child(
            toggle(
                &format!("track/{id}/synth/{name}"),
                &format!("{text} wave"),
                text,
                params.waveform == wave,
                pal().accent,
                ui,
            )
            .height(Sizing::Fixed(24.0))
            .padding(Insets::xy(8.0, 0.0))
            .style(|s| s.font_size = 11.0),
        );
    }
    bar = bar.child(Node::row().width(Sizing::Fixed(8.0)));
    for param in SynthParam::ALL {
        bar = bar.child(slider(
            &format!("track/{id}/synth/{}", param.name()),
            param,
            param.get(&params),
            ui,
        ));
    }
    bar
}

/// Buttons for the track's MIDI devices and input channel; each click
/// picks the next. With an output device, a slider for its latency
/// (double-click it to type one).
fn devices(track: &TrackView, view: &ViewState, ui: &UiState) -> Vec<Node> {
    let id = track.id;
    let device = |key: &str, label: &str, text: String, set: bool| {
        button(&format!("track/{id}/{key}"), label, ui, None)
            .value(text.clone())
            .height(Sizing::Fixed(24.0))
            .padding(Insets::xy(8.0, 0.0))
            .child(Node::label(text).width(Sizing::Fit).clip().style(|s| {
                s.font_size = 11.0;
                s.color = if set { pal().text } else { pal().text_dim };
            }))
    };
    let short = |name: &str| {
        // "Client:Port" names: the port part says enough.
        let name = name.rsplit(':').next().unwrap_or(name);
        let mut text: String = name.chars().take(18).collect();
        if name.chars().count() > 18 {
            text.push('…');
        }
        text
    };
    let typing = view
        .text_edit
        .as_ref()
        .filter(|e| e.field == TextField::DeviceLatency(id));
    let latency = track.midi_output.is_some().then(|| {
        let ms = track.midi_output_latency_ms;
        if let Some(edit) = typing {
            return Node::row()
                .size(SLIDER_W, 24.0)
                .gap(3.0)
                .child(text_field(
                    &format!("track/{id}/midi_out_latency"),
                    "MIDI output latency in ms",
                    edit.text.clone(),
                    Some(edit.selected),
                    ui,
                ))
                .child(Node::label("ms").style(|s| {
                    s.font_size = 10.0;
                    s.color = pal().text_dim;
                }));
        }
        // Plus what the audio system reports for the device.
        let text = match track.midi_output_reported_ms {
            0.0 => format!("Lat {ms:.1} ms"),
            reported => format!("Lat {ms:.1}+{reported:.1}"),
        };
        slider_node(
            &format!("track/{id}/midi_out_latency"),
            "MIDI output latency",
            latency_position(ms),
            text,
            ui,
        )
    });
    [
        device(
            "midi_in",
            "MIDI input device",
            format!(
                "In: {}",
                track.midi_input.as_deref().map_or("all".into(), short)
            ),
            track.midi_input.is_some(),
        ),
        device(
            "midi_in_channel",
            "MIDI input channel",
            match track.midi_input_channel {
                Some(c) => format!("Ch {c}"),
                None => "Ch all".into(),
            },
            track.midi_input_channel.is_some(),
        ),
        device(
            "midi_out",
            "MIDI output device",
            format!(
                "Out: {}",
                track
                    .midi_output
                    .as_deref()
                    .map_or("instrument".into(), short)
            ),
            track.midi_output.is_some(),
        ),
    ]
    .into_iter()
    .chain(latency)
    .collect()
}

/// A horizontal slider showing a synth setting; drag to change it.
fn slider(id: &str, param: SynthParam, value: f32, ui: &UiState) -> Node {
    let text = param.text(value);
    slider_node(id, &text, param.position(value), text.clone(), ui)
}

/// A horizontal slider filled to `position` (0 to 1), with `text` on it.
fn slider_node(id: &str, label: &str, position: f32, text: String, ui: &UiState) -> Node {
    let active = ui.is_pressed(id) || ui.is_hovered(id);
    Node::row()
        .id(id)
        .role(Role::Slider)
        .accessible_label(label)
        .value(text.clone())
        .draggable()
        .size(SLIDER_W, 24.0)
        .clip()
        .style(|s| {
            s.background = Some(if active { pal().hover } else { pal().track });
            s.border = Some((1.0, pal().border));
            s.radius = [4.0; 4];
        })
        .child(
            Node::row()
                .style(|s| {
                    s.background = Some(pal().accent.with_alpha(0.35));
                    s.radius = [4.0; 4];
                })
                .absolute(Rect::new(0.0, 0.0, SLIDER_W * position, 24.0)),
        )
        .child(
            Node::label(text)
                .absolute(Rect::new(0.0, 0.0, SLIDER_W, 24.0))
                .style(|s| {
                    s.font_size = 10.0;
                    s.color = pal().text;
                }),
        )
}

/// Visible pitch rows, top to bottom.
fn visible_pitches(roll: &RollView) -> impl Iterator<Item = u8> {
    let top = (roll.scroll_y / ROW_H).floor().max(0.0) as i32;
    (top..(top + ROWS_DRAWN as i32).min(128)).map(|row| (127 - row) as u8)
}

fn keys(roll: &RollView) -> Node {
    let mut keys = Node::column()
        .id("piano_roll/keys")
        .accessible_label("Keyboard")
        .scrollable()
        .width(Sizing::Fixed(KEYS_W))
        .height(Sizing::Fill(1.0))
        .padding(Insets {
            top: RULER_H,
            ..Insets::default()
        })
        .clip()
        .background(pal().titlebar);
    for pitch in visible_pitches(roll) {
        let y = y_of(roll, i32::from(pitch));
        let black = is_black(pitch);
        let mut key = Node::row()
            .style(|s| {
                s.background = Some(if black {
                    Color::hex(0x2a2d33)
                } else {
                    Color::hex(0xc9ccd1)
                });
                s.border = Some((0.5, Color::hex(0x16171b)));
            })
            .absolute(Rect::new(
                0.0,
                y,
                if black { KEYS_W * 0.62 } else { KEYS_W },
                ROW_H,
            ));
        if pitch % 12 == 0 {
            key = key.child(
                Node::label(pitch_name(pitch))
                    .absolute(Rect::new(0.0, 0.0, KEYS_W - 4.0, ROW_H))
                    .style(|s| {
                        s.font_size = 9.0;
                        s.color = Color::hex(0x30333a);
                        s.text_align = TextAlign::End;
                    }),
            );
        }
        keys = keys.child(key);
    }
    keys
}

/// The project's bar lines, seen from a clip that starts at beat `start`.
#[derive(Clone, Copy)]
struct Bars<'a> {
    meter: &'a MeterMap,
    start: f64,
}

impl Bars<'_> {
    /// The bar (from the project's start) that clip beat `beat` is in.
    fn bar_at(&self, beat: f64) -> u32 {
        self.meter.bar_at(self.start + beat + 1e-9)
    }

    /// Where bar `bar` starts, in beats from the clip's start.
    fn bar_start(&self, bar: u32) -> f64 {
        self.meter.bar_start(bar) - self.start
    }

    /// Quarter notes in the bar at clip beat `beat`.
    fn bar_beats(&self, beat: f64) -> f64 {
        self.meter.meter_at(self.start + beat).beats_per_bar()
    }

    /// Whether clip beat `beat` is on a bar line.
    fn on_bar(&self, beat: f64) -> bool {
        (self.bar_start(self.bar_at(beat)) - beat).abs() < 1e-6
    }
}

fn ruler(roll: &RollView, clip_beats: f64, bars: Bars, playhead: f64) -> Node {
    let mut ruler = Node::row()
        .id("piano_roll/ruler")
        .role(Role::Ruler)
        .accessible_label("Beats")
        .clickable()
        .scrollable()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(RULER_H))
        .clip()
        .style(|s| {
            s.background = Some(pal().toolbar);
            s.border = Some((1.0, pal().border));
        });
    let ppb = f64::from(roll.px_per_beat);
    let bar_beats = bars.bar_beats(roll.scroll_beats);
    let mut every = 1u32;
    while bar_beats * f64::from(every) * ppb < 48.0 && every < 1024 {
        every *= 2;
    }
    // Bars are numbered as in the arrange view; the clip may start inside
    // one.
    let left = bars.bar_at(roll.scroll_beats.max(0.0));
    let mut bar = left - left % every;
    let end = roll.scroll_beats + f64::from(GRID_EXTENT) / ppb;
    while bars.bar_start(bar) <= end {
        let b = bars.bar_start(bar);
        if b > -1e-6 {
            let x = x_of(roll, b);
            ruler = ruler
                .child(
                    Node::row()
                        .background(pal().border.mix(pal().text_dim, 0.4))
                        .absolute(Rect::new(x, 8.0, 1.0, RULER_H - 8.0)),
                )
                .child(
                    Node::label(format!("{}", bar + 1))
                        .absolute(Rect::new(x + 4.0, 0.0, 40.0, 14.0))
                        .style(|s| {
                            s.font_size = 10.0;
                            s.color = pal().text_dim;
                            s.text_align = TextAlign::Start;
                        }),
                );
        }
        let Some(next) = bar.checked_add(every) else {
            break;
        };
        bar = next;
    }
    // The clip's end.
    ruler = ruler.child(
        Node::row()
            .background(pal().accent.with_alpha(0.6))
            .absolute(Rect::new(x_of(roll, clip_beats) - 1.0, 0.0, 2.0, RULER_H)),
    );
    if (0.0..=clip_beats).contains(&playhead) {
        ruler = ruler.child(Node::row().background(pal().accent).absolute(Rect::new(
            x_of(roll, playhead) - 1.0,
            0.0,
            2.0,
            RULER_H,
        )));
    }
    ruler
}

fn grid(
    view: &ViewState,
    notes: &[NoteView],
    color: Color,
    clip_beats: f64,
    bars: Bars,
    playhead: f64,
) -> Node {
    let roll = &view.piano_roll;
    let mut grid = Node::row()
        .id("piano_roll/grid")
        .role(Role::Grid)
        .accessible_label("Notes")
        .clickable()
        .draggable()
        .scrollable()
        .fill()
        .clip()
        .background(pal().lane_a);
    // Black-key rows are darker.
    for pitch in visible_pitches(roll).filter(|&p| is_black(p)) {
        grid = grid.child(
            Node::row()
                .background(pal().lane_b.mix(pal().panel, 0.6))
                .absolute(Rect::new(
                    0.0,
                    y_of(roll, i32::from(pitch)),
                    GRID_EXTENT,
                    ROW_H,
                )),
        );
    }
    // Octave lines under each B.
    for pitch in visible_pitches(roll).filter(|p| p % 12 == 0) {
        grid = grid.child(Node::row().background(pal().border).absolute(Rect::new(
            0.0,
            y_of(roll, i32::from(pitch)) + ROW_H - 1.0,
            GRID_EXTENT,
            1.0,
        )));
    }
    // Vertical lines: grid steps if they're far enough apart, beats, bars.
    let ppb = f64::from(roll.px_per_beat);
    let end = roll.scroll_beats + f64::from(GRID_EXTENT) / ppb;
    let line = |grid: Node, b: f64, shade: Color| {
        grid.child(Node::row().background(shade).absolute(Rect::new(
            x_of(roll, b),
            0.0,
            1.0,
            GRID_EXTENT,
        )))
    };
    if ppb >= 6.0 {
        let step = if roll.grid_beats * ppb >= 6.0 {
            roll.grid_beats
        } else {
            1.0
        };
        let first = (roll.scroll_beats / step).floor().max(0.0) as u64;
        let mut i = first;
        while (i as f64) * step <= end {
            let b = i as f64 * step;
            let on = |unit: f64| ((b / unit) - (b / unit).round()).abs() < 1e-6;
            let shade = if bars.on_bar(b) {
                pal().border.mix(pal().text_dim, 0.5)
            } else if on(1.0) {
                pal().border
            } else {
                pal().border.mix(pal().lane_a, 0.5)
            };
            grid = line(grid, b, shade);
            i += 1;
        }
        // Bar lines between grid steps (after a change to an odd meter).
        let mut bar = bars.bar_at(roll.scroll_beats);
        while bars.bar_start(bar) <= end {
            let b = bars.bar_start(bar);
            if b >= 0.0 && ((b / step) - (b / step).round()).abs() >= 1e-6 {
                grid = line(grid, b, pal().border.mix(pal().text_dim, 0.5));
            }
            bar += 1;
        }
    } else {
        let mut bar = bars.bar_at(roll.scroll_beats);
        while bars.bar_start(bar) <= end {
            let b = bars.bar_start(bar);
            if b >= 0.0 {
                grid = line(grid, b, pal().border.mix(pal().text_dim, 0.5));
            }
            bar += 1;
        }
    }
    // Past the clip's end notes are cut off.
    let end_x = x_of(roll, clip_beats);
    grid = grid.child(
        Node::row()
            .background(Color::rgba(0.0, 0.0, 0.0, 0.3))
            .absolute(Rect::new(end_x.max(0.0), 0.0, GRID_EXTENT, GRID_EXTENT)),
    );

    let preview = |note: &NoteView| -> (f64, i32, f64) {
        let pitch = i32::from(note.pitch);
        match &view.gesture {
            Some(Gesture::MoveNotes {
                notes,
                delta_beats,
                delta_pitch,
                ..
            }) if notes.contains(&note.id) => (
                (note.start_beats + delta_beats).max(0.0),
                (pitch + delta_pitch).clamp(0, 127),
                note.length_beats,
            ),
            Some(Gesture::ResizeNote {
                note: id,
                length_beats,
                ..
            }) if *id == note.id => (note.start_beats, pitch, *length_beats),
            _ => (note.start_beats, pitch, note.length_beats),
        }
    };
    for note in notes {
        let (start, pitch, length) = preview(note);
        let x = x_of(roll, start);
        let w = ((length * ppb) as f32).max(3.0);
        let selected = view.selected_notes.contains(&note.id);
        let strength = 0.35 + 0.65 * f32::from(note.velocity) / 127.0;
        let fill = color.mix(pal().panel, 1.0 - strength);
        let handle_w = HANDLE_W.min(w / 3.0);
        let name = pitch_name(pitch as u8);
        grid = grid.child(
            Node::row()
                .id(format!("note/{}", note.id))
                .role(Role::Note)
                .accessible_label(name)
                .value(format!(
                    "{start:.3}-{:.3} beats · velocity {}",
                    start + length,
                    note.velocity
                ))
                .clickable()
                .draggable()
                .checked(selected)
                .absolute(Rect::new(x, y_of(roll, pitch) + 1.0, w, ROW_H - 2.0))
                .style(|s| {
                    s.background = Some(fill);
                    s.radius = [2.0; 4];
                    s.border = Some(if selected {
                        (1.5, Color::hex(0xffffff))
                    } else {
                        (1.0, color.mix(Color::hex(0), 0.3))
                    });
                })
                .child(
                    Node::row()
                        .id(format!("note/{}/end", note.id))
                        .accessible_label("Note end")
                        .draggable()
                        .cursor(dawkey_ui::Cursor::ResizeHorizontal)
                        .absolute(Rect::new(w - handle_w, 0.0, handle_w, ROW_H - 2.0)),
                ),
        );
    }
    if (0.0..=clip_beats).contains(&playhead) {
        grid = grid.child(Node::row().background(pal().accent).absolute(Rect::new(
            x_of(roll, playhead) - 1.0,
            0.0,
            2.0,
            GRID_EXTENT,
        )));
    }
    if let Some(Gesture::Marquee { from, to, .. }) = &view.gesture {
        let (x0, x1) = (x_of(roll, from.0.min(to.0)), x_of(roll, from.0.max(to.0)));
        let (y0, y1) = (
            y_of(roll, from.1.max(to.1)),
            y_of(roll, from.1.min(to.1)) + ROW_H,
        );
        grid = grid.child(
            Node::row()
                .style(|s| {
                    s.background = Some(pal().text.with_alpha(0.08));
                    s.border = Some((1.0, pal().text.with_alpha(0.6)));
                })
                .absolute(Rect::new(x0, y0, (x1 - x0).max(1.0), y1 - y0)),
        );
    }
    grid
}

/// Note bars for a MIDI clip in the arrange view: `w`×`h` px for the whole
/// clip, `px_per_s` px per second, keeping only what's in `visible_x`.
pub(crate) fn preview_rects(
    notes: &[NoteView],
    px_per_s: f64,
    w: f32,
    h: f32,
    visible_x: std::ops::Range<f32>,
) -> std::sync::Arc<[Rect]> {
    let (lo, hi) = notes.iter().fold((u8::MAX, 0u8), |(lo, hi), n| {
        (lo.min(n.pitch), hi.max(n.pitch))
    });
    if notes.is_empty() {
        return std::sync::Arc::new([]);
    }
    // At least an octave of range, so a single note sits mid-clip.
    let span = f32::from(hi - lo).max(12.0) + 1.0;
    let mid = (f32::from(lo) + f32::from(hi)) / 2.0;
    let row = (h / span).clamp(1.0, 6.0);
    notes
        .iter()
        .filter_map(|n| {
            let x = (n.start_s * px_per_s) as f32;
            let nw = ((n.length_s * px_per_s) as f32).max(1.0).min(w - x);
            if x >= w || x + nw < visible_x.start || x > visible_x.end {
                return None;
            }
            let y = h / 2.0 - (f32::from(n.pitch) - mid) * h / span - row / 2.0;
            Some(Rect::new(x, y.clamp(0.0, h - row), nw, row))
        })
        .collect()
}

fn track_color(index: usize) -> Color {
    TRACK_COLORS[index % TRACK_COLORS.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_pitches() {
        assert_eq!(pitch_name(60), "C4");
        assert_eq!(pitch_name(69), "A4");
        assert_eq!(pitch_name(0), "C-1");
        assert_eq!(pitch_name(127), "G9");
        assert!(is_black(61) && !is_black(64));
    }
}
