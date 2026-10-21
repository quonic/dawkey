// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mixer: one channel strip per track and bus (plugins, output, pan,
//! fader, meter, mute/solo), and the master's (plugins, fader, meter).

use dk_core::{GAIN_DB_MAX, GAIN_DB_MIN};
use dk_ui::{Align, FontFamily, FontWeight, Insets, Node, Rect, Role, Sizing, TextAlign, UiState};

use super::{TrackView, ViewModel, button, meter, track_buttons};
use crate::state::{FADER_H, PAN_W};
use crate::theme::*;

const STRIP_W: f32 = 104.0;
const THUMB_H: f32 = 14.0;
/// Fader position of 0 dB (unity), 0 = bottom, 1 = top.
const UNITY: f32 = 0.75;
/// Below this position the scale steepens towards silence.
const KNEE: f32 = 0.25;
const KNEE_DB: f32 = -24.0;

/// Fader position (0 = bottom, 1 = top) for a gain.
pub(crate) fn position_from_db(db: f32) -> f32 {
    let db = db.clamp(GAIN_DB_MIN, GAIN_DB_MAX);
    if db >= KNEE_DB {
        UNITY + db / (GAIN_DB_MAX / (1.0 - UNITY))
    } else {
        KNEE * (db - GAIN_DB_MIN) / (KNEE_DB - GAIN_DB_MIN)
    }
}

/// Gain for a fader position.
pub(crate) fn db_from_position(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    let db = if p >= KNEE {
        (p - UNITY) * (GAIN_DB_MAX / (1.0 - UNITY))
    } else {
        GAIN_DB_MIN + p / KNEE * (KNEE_DB - GAIN_DB_MIN)
    };
    // Round to 0.1 dB so drags land on readable values.
    (db * 10.0).round() / 10.0
}

pub(super) fn build(model: &ViewModel, ui: &UiState) -> Node {
    let strips = model.tracks.iter().map(|t| strip(t, ui));
    let mut mixer = Node::row()
        .id("mixer")
        .fill()
        .padding(Insets::all(12.0))
        .gap(8.0)
        .align(Align::Start)
        .clip()
        .background(pal().panel)
        .children(strips);
    if model.tracks.is_empty() {
        mixer = mixer.child(
            Node::label("No tracks yet.")
                .id("tracks/empty")
                .fill()
                .style(|s| s.color = pal().text_dim),
        );
    }
    mixer.child(Node::spacer()).child(master(model, ui))
}

fn strip(track: &TrackView, ui: &UiState) -> Node {
    let id = track.id;
    Node::column()
        .id(format!("track/{id}/strip"))
        .role(Role::Group)
        .accessible_label(track.name.clone())
        .width(Sizing::Fixed(STRIP_W))
        .padding(Insets::all(10.0))
        .gap(10.0)
        .style(|s| {
            s.background = Some(pal().track);
            s.radius = [6.0; 4];
        })
        .child(
            Node::label(track.name.clone())
                .id(format!("track/{id}/name"))
                .width(Sizing::Fill(1.0))
                .clip()
                .style(|s| {
                    s.weight = FontWeight::SemiBold;
                    s.color = pal().text;
                }),
        )
        .child(slots(track, ui))
        .child(output(track, ui))
        .child(pan_slider(track, ui))
        .child(Node::row().gap(10.0).child(fader(track, ui)).child(meter(
            &format!("track/{id}/meter"),
            "Level",
            track.level,
            true,
            14.0,
            FADER_H,
        )))
        .child(gain_label(track))
        .child(Node::row().gap(4.0).children(track_buttons(track, ui)))
}

fn gain_label(track: &TrackView) -> Node {
    Node::label(format!("{:+.1} dB", track.gain_db))
        .id(format!("track/{}/gain", track.id))
        .style(|s| {
            s.family = FontFamily::Mono;
            s.font_size = 11.0;
            s.color = pal().text_dim;
        })
}

/// Where the track goes: the master or a bus; clicking picks the next.
fn output(track: &TrackView, ui: &UiState) -> Node {
    button(
        &format!("track/{}/output", track.id),
        "Output",
        ui,
        track
            .output
            .is_some()
            .then_some(pal().accent.with_alpha(0.35)),
    )
    .value(track.output_name.clone())
    .text(format!("→ {}", track.output_name))
    .size(STRIP_W - 20.0, 20.0)
    .clip()
    .padding(Insets::xy(5.0, 0.0))
    .style(|s| {
        s.font_size = 10.0;
        s.color = pal().text_dim;
        s.text_align = TextAlign::Start;
    })
}

/// The track's plugins, top to bottom in signal order, and "+ FX".
fn slots(track: &TrackView, ui: &UiState) -> Node {
    let slot_w = STRIP_W - 20.0;
    let mut column = Node::column()
        .id(format!("track/{}/plugins", track.id))
        .accessible_label("Plugins")
        .gap(3.0);
    for slot in &track.plugins {
        let id = format!("plugin/{}", slot.id);
        let (color, text) = if slot.error {
            (pal().mute_on, format!("! {}", slot.name))
        } else if slot.bypass {
            (pal().text_dim.with_alpha(0.6), slot.name.clone())
        } else {
            (pal().text, slot.name.clone())
        };
        column = column.child(
            button(&id, &slot.name, ui, None)
                .value(if slot.error {
                    "not loaded"
                } else if slot.bypass {
                    "bypassed"
                } else {
                    "on"
                })
                .size(slot_w, 20.0)
                .clip()
                .text(text)
                .padding(Insets::xy(5.0, 0.0))
                .style(|s| {
                    s.font_size = 10.0;
                    s.color = color;
                    s.text_align = TextAlign::Start;
                }),
        );
    }
    column.child(
        button(
            &format!("track/{}/add_plugin", track.id),
            "Add plugin",
            ui,
            None,
        )
        .text("+ FX")
        .size(slot_w, 20.0)
        .style(|s| {
            s.font_size = 10.0;
            s.color = pal().text_dim;
        }),
    )
}

fn fader(track: &TrackView, ui: &UiState) -> Node {
    let id = format!("track/{}/fader", track.id);
    let active = ui.is_pressed(&id) || ui.is_hovered(&id);
    let p = position_from_db(track.gain_db);
    let thumb_y = (1.0 - p) * (FADER_H - THUMB_H);
    Node::column()
        .id(id)
        .role(Role::Slider)
        .accessible_label("Gain")
        .value(format!("{:.1} dB", track.gain_db))
        .draggable()
        .size(28.0, FADER_H)
        .child(
            // Groove.
            Node::row()
                .background(pal().panel)
                .style(|s| s.radius = [2.0; 4])
                .absolute(Rect::new(12.0, 0.0, 4.0, FADER_H)),
        )
        .child(
            // Unity mark.
            Node::row()
                .background(pal().border.mix(pal().text_dim, 0.4))
                .absolute(Rect::new(
                    4.0,
                    (1.0 - UNITY) * (FADER_H - THUMB_H) + THUMB_H / 2.0,
                    20.0,
                    1.0,
                )),
        )
        .child(
            Node::row()
                .style(|s| {
                    s.background = Some(if active {
                        pal().text
                    } else {
                        pal().text.mix(pal().text_dim, 0.4)
                    });
                    s.radius = [3.0; 4];
                })
                .absolute(Rect::new(0.0, thumb_y, 28.0, THUMB_H)),
        )
}

fn pan_slider(track: &TrackView, ui: &UiState) -> Node {
    let id = format!("track/{}/pan", track.id);
    let active = ui.is_pressed(&id) || ui.is_hovered(&id);
    let pan = track.pan.clamp(-1.0, 1.0);
    let value = match pan {
        p if p.abs() < 0.005 => "C".to_owned(),
        p if p < 0.0 => format!("L{:.0}", -p * 100.0),
        p => format!("R{:.0}", p * 100.0),
    };
    let center = PAN_W / 2.0;
    let x = center + pan * (center - 3.0);
    let (from, to) = if x < center { (x, center) } else { (center, x) };
    Node::row()
        .id(id)
        .role(Role::Slider)
        .accessible_label("Pan")
        .value(value.clone())
        .draggable()
        .size(PAN_W, 20.0)
        .style(|s| {
            s.background = Some(pal().panel);
            s.radius = [3.0; 4];
            s.border = Some((1.0, if active { pal().text_dim } else { pal().border }));
        })
        .child(
            Node::label(value)
                .absolute(Rect::new(0.0, 0.0, PAN_W, 15.0))
                .style(|s| {
                    s.font_size = 10.0;
                    s.color = pal().text;
                    s.text_align = TextAlign::Center;
                }),
        )
        // A thin bar from center towards the pan side, under the text.
        .child(Node::row().background(pal().accent).absolute(Rect::new(
            from,
            15.0,
            (to - from).max(2.0),
            3.0,
        )))
}

/// The master bus (track 0): its plugins, fader, the output meter, mute.
fn master(model: &ViewModel, ui: &UiState) -> Node {
    let master = &model.master_track;
    Node::column()
        .id("master/strip")
        .accessible_label("Master")
        .width(Sizing::Fixed(STRIP_W))
        .padding(Insets::all(10.0))
        .gap(10.0)
        .style(|s| {
            s.background = Some(pal().toolbar);
            s.radius = [6.0; 4];
        })
        .child(Node::label("Master").style(|s| {
            s.weight = FontWeight::SemiBold;
            s.color = pal().text;
        }))
        .child(slots(master, ui))
        .child(Node::row().gap(10.0).child(fader(master, ui)).child(meter(
            "master/strip_meter",
            "Master",
            model.master,
            true,
            24.0,
            FADER_H,
        )))
        .child(gain_label(master))
        .child(Node::row().gap(4.0).children(track_buttons(master, ui)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_law_round_trips() {
        assert_eq!(position_from_db(0.0), UNITY);
        assert_eq!(position_from_db(GAIN_DB_MAX), 1.0);
        assert_eq!(position_from_db(GAIN_DB_MIN), 0.0);
        for db in [-144.0, -60.0, -24.0, -6.0, 0.0, 3.5, 12.0] {
            assert!(
                (db_from_position(position_from_db(db)) - db).abs() < 0.11,
                "{db}"
            );
        }
        // Monotonic.
        let samples: Vec<f32> = (0..=100)
            .map(|i| db_from_position(i as f32 / 100.0))
            .collect();
        assert!(samples.windows(2).all(|w| w[1] >= w[0]));
    }
}
