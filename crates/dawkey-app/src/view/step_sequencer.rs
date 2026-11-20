// SPDX-License-Identifier: AGPL-3.0-or-later
//! The drum step sequencer: a MIDI clip's notes as a fixed grid of eight
//! drum pads (rows) by sixteen 16th-note steps (one bar, 0.25 beats each),
//! for a MIDI track whose first plugin is `builtin:drums`. Pad notes and
//! names mirror `dawkey-core`'s `command.rs` and `dawkey-plugins`'
//! `builtin.rs` (must stay in sync).

use dawkey_ui::{Color, FontWeight, Insets, Node, Role, Sizing, TextAlign, UiState};

use super::piano_roll::edited;
use super::{ClipView, ClipViewContent, NoteView, TrackView, ViewModel, button, name_editor};
use crate::state::{TextField, ViewState};
use crate::theme::*;

/// General MIDI drum-map note numbers builtin:drums' eight pads answer to.
pub(crate) const PAD_NOTES: [u8; 8] = [36, 38, 42, 46, 45, 39, 49, 51];
pub(crate) const PAD_NAMES: [&str; 8] = [
    "Kick", "Snare", "C.Hat", "O.Hat", "Tom", "Clap", "Crash", "Ride",
];
/// Steps in one bar's grid (16th notes).
pub(crate) const STEPS: u8 = 16;
/// Length of one step, in beats (quarter notes).
pub(crate) const STEP_BEATS: f64 = 0.25;

const ROW_LABEL_W: f32 = 48.0;
const CELL: f32 = 26.0;
const GAP: f32 = 3.0;
const SIDEBAR_W: f32 = 212.0;

fn lerp(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    Color::rgba(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
        1.0,
    )
}

pub(super) fn build(model: &ViewModel, view: &ViewState, ui: &UiState) -> Node {
    let page = Node::row()
        .id("step_seq")
        .accessible_label("Drum step sequencer")
        .fill()
        .clip()
        .background(pal().panel);
    let Some((_, track, clip)) = edited(model, view) else {
        return page.child(
            Node::label("Double-click a drum machine clip to edit it")
                .fill()
                .style(|s| s.color = pal().text_dim),
        );
    };
    let ClipViewContent::Midi { notes, .. } = &clip.content else {
        unreachable!("edited() only finds MIDI clips");
    };
    let playhead_step = {
        let start_beat = model.tempo.beat_at_seconds(clip.start_s);
        let beat = model.tempo.beat_at_seconds(model.position_s) - start_beat;
        let step = (beat / STEP_BEATS).floor();
        (step >= 0.0 && step < f64::from(STEPS)).then_some(step as u8)
    };
    page.child(sidebar(model, view, track, clip.id, ui)).child(
        Node::column()
            .fill()
            .clip()
            .child(header(track, clip, ui))
            .child(grid(notes, playhead_step)),
    )
}

fn header(track: &TrackView, clip: &ClipView, ui: &UiState) -> Node {
    Node::row()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(32.0))
        .padding(Insets::xy(10.0, 0.0))
        .gap(8.0)
        .style(|s| s.background = Some(pal().toolbar))
        .child(
            Node::label(format!("{} — {}", track.name, clip.name))
                .clip()
                .style(|s| {
                    s.weight = FontWeight::SemiBold;
                    s.color = pal().text;
                    s.font_size = 12.0;
                    s.text_align = TextAlign::Start;
                }),
        )
        .child(Node::spacer())
        .child(
            button("step_seq/close", "Back to arrange", ui, None)
                .text("Close")
                .padding(Insets::xy(8.0, 0.0))
                .height(Sizing::Fixed(22.0))
                .style(|s| s.font_size = 11.0),
        )
}

fn grid(notes: &[NoteView], playhead_step: Option<u8>) -> Node {
    let mut rows = Node::column().padding(Insets::all(10.0)).gap(GAP);
    for (pad_index, name) in PAD_NAMES.iter().enumerate() {
        let pad_note = PAD_NOTES[pad_index];
        let mut row = Node::row().gap(GAP).height(Sizing::Fixed(CELL));
        row = row.child(
            Node::label(*name)
                .width(Sizing::Fixed(ROW_LABEL_W))
                .style(|s| {
                    s.font_size = 10.0;
                    s.color = pal().text_dim;
                    s.text_align = TextAlign::Start;
                }),
        );
        for step in 0..STEPS {
            let hit = notes.iter().find(|n| {
                n.pitch == pad_note && (n.start_beats - f64::from(step) * STEP_BEATS).abs() < 1e-6
            });
            let velocity = hit.map(|n| n.velocity);
            let is_beat = step % 4 == 0;
            let is_now = playhead_step == Some(step);
            let bg = match velocity {
                Some(v) => lerp(pal().track, pal().accent, f32::from(v) / 127.0),
                None if is_beat => pal().lane_b,
                None => pal().lane_a,
            };
            let border = if is_now { pal().accent } else { pal().border };
            let border_w = if is_now { 2.0 } else { 1.0 };
            row = row.child(
                Node::row()
                    .id(format!("step/{pad_index}/{step}"))
                    .role(Role::Button)
                    .accessible_label(format!("{name} step {}", step + 1))
                    .value(if velocity.is_some() { "on" } else { "off" })
                    .clickable()
                    .width(Sizing::Fixed(CELL))
                    .height(Sizing::Fixed(CELL))
                    .style(move |s| {
                        s.background = Some(bg);
                        s.border = Some((border_w, border));
                        s.radius = [3.0; 4];
                    }),
            );
        }
        rows = rows.child(row);
    }
    rows
}

fn sidebar(
    model: &ViewModel,
    view: &ViewState,
    track: &TrackView,
    clip: dawkey_core::ClipId,
    ui: &UiState,
) -> Node {
    let section = |title: &str| {
        Node::label(title)
            .width(Sizing::Fill(1.0))
            .padding(Insets {
                top: 6.0,
                bottom: 2.0,
                ..Insets::default()
            })
            .style(|s| {
                s.font_size = 10.0;
                s.weight = FontWeight::SemiBold;
                s.color = pal().text_dim;
                s.text_align = TextAlign::Start;
            })
    };
    let kit_btn = |id: &str, label: &str| {
        button(
            &format!("step_seq/kit/{id}"),
            &format!("Load the {label} kit"),
            ui,
            None,
        )
        .text(label)
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(22.0))
        .style(|s| s.font_size = 10.0)
    };
    let pattern_btn = |id: &str, label: &str| {
        button(
            &format!("step_seq/pattern/{id}"),
            &format!("Replace this clip's steps with the {label} pattern"),
            ui,
            None,
        )
        .text(label)
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(22.0))
        .style(|s| s.font_size = 10.0)
    };

    let mut your_patterns = Node::column().width(Sizing::Fill(1.0)).gap(4.0);
    for (i, name) in model.drum_patterns.iter().enumerate() {
        your_patterns = your_patterns.child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(
                    button(
                        &format!("step_seq/saved/{i}"),
                        &format!("Load \"{name}\""),
                        ui,
                        None,
                    )
                    .text(name.clone())
                    .clip()
                    .width(Sizing::Fill(1.0))
                    .height(Sizing::Fixed(22.0))
                    .padding(Insets::xy(6.0, 0.0))
                    .style(|s| {
                        s.font_size = 10.0;
                        s.text_align = TextAlign::Start;
                    }),
                )
                .child(
                    button(
                        &format!("step_seq/remove_saved/{i}"),
                        &format!("Remove the \"{name}\" pattern"),
                        ui,
                        None,
                    )
                    .text("×")
                    .width(Sizing::Fixed(20.0))
                    .height(Sizing::Fixed(22.0))
                    .style(|s| s.color = pal().text_dim),
                ),
        );
    }
    your_patterns = your_patterns.child(
        name_editor(&TextField::DrumPatternName(clip), "Pattern name", view, ui).unwrap_or_else(
            || {
                button(
                    "step_seq/save_as",
                    "Save this clip's steps as a pattern of your own",
                    ui,
                    None,
                )
                .text("+ Save as pattern…")
                .width(Sizing::Fill(1.0))
                .height(Sizing::Fixed(22.0))
                .style(|s| {
                    s.font_size = 10.0;
                    s.color = pal().text_dim;
                })
            },
        ),
    );

    let active_kit = track.plugins.first().and_then(|p| p.sample_kit);
    let sound_btn = |id: &str, label: &str, active: bool| {
        button(
            &format!("step_seq/sound/{id}"),
            label,
            ui,
            active.then_some(pal().accent),
        )
        .text(label)
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(22.0))
        .style(|s| s.font_size = 10.0)
    };
    let is_kit = |kit: dawkey_studio::SampleKit| active_kit == Some(kit);
    let sound = Node::column()
        .width(Sizing::Fill(1.0))
        .gap(4.0)
        .child(sound_btn("synth", "Synth", active_kit.is_none()))
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(sound_btn(
                    "acoustic_vcsl",
                    "Acoustic",
                    is_kit(dawkey_studio::SampleKit::AcousticVcsl),
                ))
                .child(sound_btn(
                    "jazz_virtuosity",
                    "Jazz",
                    is_kit(dawkey_studio::SampleKit::JazzVirtuosity),
                )),
        )
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(sound_btn(
                    "lofi_virtuosity",
                    "LoFi",
                    is_kit(dawkey_studio::SampleKit::LofiVirtuosity),
                ))
                .child(sound_btn(
                    "vintage_vcsl",
                    "Vintage",
                    is_kit(dawkey_studio::SampleKit::VintageVcsl),
                )),
        )
        .child(sound_btn(
            "latin_vcsl",
            "Latin Percussion",
            is_kit(dawkey_studio::SampleKit::LatinVcsl),
        ));

    let kits = Node::column()
        .width(Sizing::Fill(1.0))
        .gap(4.0)
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(kit_btn("808", "808"))
                .child(kit_btn("909", "909")),
        )
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(kit_btn("acoustic", "Acoustic"))
                .child(kit_btn("lofi", "LoFi")),
        )
        .child(kit_btn("trap", "Trap"));

    let patterns = Node::column()
        .width(Sizing::Fill(1.0))
        .gap(4.0)
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(pattern_btn("four_on_the_floor", "4 On Floor"))
                .child(pattern_btn("boom_bap", "Boom Bap")),
        )
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(pattern_btn("trap", "Trap"))
                .child(pattern_btn("house_shuffle", "House")),
        )
        .child(pattern_btn("drum_and_bass", "D&B"));

    Node::column()
        .id("step_seq/sidebar")
        .width(Sizing::Fixed(SIDEBAR_W))
        .height(Sizing::Fill(1.0))
        .padding(Insets::all(10.0))
        .gap(6.0)
        .scrollable()
        .clip()
        .style(|s| {
            s.background = Some(pal().panel);
            s.border = Some((1.0, pal().border));
        })
        .child(section("SOUND SOURCE"))
        .child(sound)
        .child(section("KITS"))
        .child(kits)
        .child(section("PATTERNS"))
        .child(patterns)
        .child(section("YOUR PATTERNS"))
        .child(your_patterns)
}
