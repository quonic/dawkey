// SPDX-License-Identifier: AGPL-3.0-or-later
//! The drum step sequencer: a MIDI clip's notes as a fixed grid of eight
//! drum pads (rows) by sixteen 16th-note steps (one bar, 0.25 beats each),
//! for a MIDI track whose first plugin is `builtin:drums`. Pad notes and
//! names mirror `dawkey-core`'s `command.rs` and `dawkey-plugins`'
//! `builtin.rs` (must stay in sync).

use dawkey_core::{DrumPattern, TimeSignature};
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

pub(crate) const PATTERNS: &[(&str, &str, DrumPattern)] = &[
    (
        "four_on_the_floor",
        "4 On Floor",
        DrumPattern::FourOnTheFloor,
    ),
    ("boom_bap", "Boom Bap", DrumPattern::BoomBap),
    ("trap", "Trap", DrumPattern::Trap),
    ("house_shuffle", "House", DrumPattern::HouseShuffle),
    ("drum_and_bass", "D&B", DrumPattern::DrumAndBass),
    ("rock", "Rock", DrumPattern::Rock),
    ("metal", "Metal", DrumPattern::Metal),
    ("funk", "Funk", DrumPattern::Funk),
    ("jazz_swing", "Jazz", DrumPattern::JazzSwing),
    ("bossa_nova", "Bossa", DrumPattern::BossaNova),
    ("reggae_one_drop", "Reggae", DrumPattern::ReggaeOneDrop),
    ("disco", "Disco", DrumPattern::Disco),
    ("techno", "Techno", DrumPattern::Techno),
    ("half_time", "Half-Time", DrumPattern::HalfTime),
    ("afrobeat", "Afrobeat", DrumPattern::Afrobeat),
];

pub(crate) const FILLS: &[(&str, &str, DrumPattern)] = &[
    ("fill_snare_roll", "Snare Roll", DrumPattern::FillSnareRoll),
    ("fill_tom_cascade", "Tom Fill", DrumPattern::FillTomCascade),
    ("fill_trap_roll", "Trap Fill", DrumPattern::FillTrapRoll),
    ("fill_breakbeat", "Break Fill", DrumPattern::FillBreakbeat),
    (
        "fill_double_kick",
        "Metal Fill",
        DrumPattern::FillDoubleKick,
    ),
    (
        "fill_jazz_turnaround",
        "Jazz Fill",
        DrumPattern::FillJazzTurnaround,
    ),
];

pub(crate) fn pattern_from_id(id: &str) -> Option<DrumPattern> {
    PATTERNS
        .iter()
        .chain(FILLS.iter())
        .find_map(|&(slug, _, pat)| (slug == id).then_some(pat))
}

const ROW_LABEL_W: f32 = 48.0;
const PATCH_W: f32 = 56.0;
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

pub(super) fn build(model: &ViewModel, view: &ViewState, ui: &UiState, window_w: f32) -> Node {
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
    let start_beat = model.tempo.beat_at_seconds(clip.start_s);
    let sig = model.meter.meter_at(start_beat + 1e-9);
    let steps = sig.steps_per_bar();
    let step_beats = sig.step_beats();
    let playhead_step = {
        let beat = model.tempo.beat_at_seconds(model.position_s) - start_beat;
        let step = (beat / step_beats).floor();
        (step >= 0.0 && step < f64::from(steps)).then_some(step as u16)
    };
    page.child(sidebar(model, view, track, clip.id, ui)).child(
        Node::column()
            .fill()
            .clip()
            .child(header(track, clip, ui))
            .child(grid(model, track, notes, sig, playhead_step, window_w, ui)),
    )
}

/// The bus a patched pad's output goes to, or "Master" when it isn't
/// patched — the same fallback a track's own `output_name` uses.
fn bus_name(model: &ViewModel, bus: Option<dawkey_core::TrackId>) -> String {
    bus.and_then(|b| model.tracks.iter().find(|t| t.id == b))
        .map_or_else(|| "Master".to_owned(), |t| t.name.clone())
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

fn grid(
    model: &ViewModel,
    track: &TrackView,
    notes: &[NoteView],
    sig: TimeSignature,
    playhead_step: Option<u16>,
    window_w: f32,
    ui: &UiState,
) -> Node {
    let steps = sig.steps_per_bar();
    let steps_per_beat = sig.steps_per_beat();
    let step_beats = sig.step_beats();
    let avail = (window_w - SIDEBAR_W - 20.0 - ROW_LABEL_W - PATCH_W - GAP * 2.0).max(100.0);
    let cell_w = ((avail - f32::from(steps.saturating_sub(1)) * GAP) / f32::from(steps.max(1)))
        .floor()
        .clamp(6.0, CELL);
    let pad_outputs = track
        .plugins
        .first()
        .map(|p| p.pad_outputs.as_slice())
        .unwrap_or(&[]);
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
        let bus = pad_outputs.get(pad_index).copied().flatten();
        let name_text = bus_name(model, bus);
        row = row.child(
            button(
                &format!("step_seq/pad/{pad_index}/output"),
                &format!("Patch {name} to a bus"),
                ui,
                bus.is_some().then_some(pal().accent),
            )
            .text(name_text)
            .clip()
            .width(Sizing::Fixed(PATCH_W))
            .height(Sizing::Fixed(CELL))
            .padding(Insets::xy(6.0, 0.0))
            .style(|s| s.font_size = 9.0),
        );
        for step in 0..steps {
            let hit = notes.iter().find(|n| {
                n.pitch == pad_note && (n.start_beats - f64::from(step) * step_beats).abs() < 1e-6
            });
            let velocity = hit.map(|n| n.velocity);
            let is_beat = step % steps_per_beat == 0;
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
                    .width(Sizing::Fixed(cell_w))
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
                top: 4.0,
                bottom: 1.0,
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
        .height(Sizing::Fixed(20.0))
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
        .clip()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(20.0))
        .style(|s| s.font_size = 9.5)
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
                    .height(Sizing::Fixed(20.0))
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
                    .height(Sizing::Fixed(20.0))
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
                .height(Sizing::Fixed(20.0))
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
        .height(Sizing::Fixed(20.0))
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
                .child(kit_btn("natural", "Natural"))
                .child(kit_btn("tape", "Tape")),
        )
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(kit_btn("trap", "Trap"))
                .child(kit_btn("metal", "Metal")),
        );

    let pattern_grid = |items: &[(&str, &str, DrumPattern)]| {
        let mut col = Node::column().width(Sizing::Fill(1.0)).gap(4.0);
        for chunk in items.chunks(3) {
            let mut row = Node::row().width(Sizing::Fill(1.0)).gap(4.0);
            for &(id, label, _) in chunk {
                row = row.child(pattern_btn(id, label));
            }
            col = col.child(row);
        }
        col
    };

    Node::column()
        .id("step_seq/sidebar")
        .width(Sizing::Fixed(SIDEBAR_W))
        .height(Sizing::Fill(1.0))
        .padding(Insets::all(10.0))
        .gap(4.0)
        .scrollable()
        .clip()
        .style(|s| {
            s.background = Some(pal().panel);
            s.border = Some((1.0, pal().border));
        })
        .child(section("KITS"))
        .child(sound)
        .child(section("DRUM EFFECT PRESETS"))
        .child(kits)
        .child(section("PATTERNS"))
        .child(pattern_grid(PATTERNS))
        .child(section("FILLS"))
        .child(pattern_grid(FILLS))
        .child(section("YOUR PATTERNS"))
        .child(your_patterns)
}
