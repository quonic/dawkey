// SPDX-License-Identifier: AGPL-3.0-or-later
//! The export dialog: what to write (format, sample size, sample rate, the
//! song or the loop range, a tail), each a button that steps through its
//! choices, and "Export…", which asks where to.

use dawkey_media::{FileFormat, SampleFormat};
use dawkey_ui::{
    Align, Color, FontWeight, Insets, Justify, Node, Rect, Role, Sizing, TextAlign, UiState,
};

use super::{ViewModel, button, format_time};
use crate::state::{ExportRange, ViewState};
use crate::theme::*;

const PANEL_W: f32 = 420.0;

/// The dialog over `area` (the window below its title bar), if it's open.
pub(super) fn dialog(
    model: &ViewModel,
    view: &ViewState,
    ui: &UiState,
    area: Rect,
    radius: f32,
) -> Option<Node> {
    if !view.export_dialog {
        return None;
    }
    let settings = &view.export;
    let format = match settings.format {
        FileFormat::Wav => "WAV",
        FileFormat::Flac => "FLAC",
    };
    let bits = match settings.sample {
        SampleFormat::Int16 => "16-bit",
        SampleFormat::Int24 => "24-bit",
        SampleFormat::Float32 => "32-bit float",
    };
    let khz = |rate: u32| format!("{} kHz", f64::from(rate) / 1000.0);
    let rate = match settings.sample_rate {
        None => format!("Project ({})", khz(model.sample_rate)),
        Some(rate) => khz(rate),
    };
    let range = match (settings.range, model.loop_range) {
        (ExportRange::Loop, Some((start, end))) => {
            format!("Loop ({} - {})", format_time(start), format_time(end))
        }
        _ => format!("Song (to {})", format_time(model.song_end_s)),
    };
    let tail = match settings.tail_s {
        0.0 => "None".to_owned(),
        s => format!("{s:.0} s"),
    };
    let row = |id: &str, label: &str, value: String| {
        Node::row()
            .width(Sizing::Fill(1.0))
            .height(Sizing::Fixed(30.0))
            .align(Align::Center)
            .child(
                Node::label(label.to_owned())
                    .width(Sizing::Fixed(110.0))
                    .style(|s| {
                        s.color = pal().text_dim;
                        s.font_size = 12.0;
                        s.text_align = TextAlign::Start;
                    }),
            )
            .child(
                button(id, label, ui, None)
                    .value(value.clone())
                    .text(value)
                    .width(Sizing::Fill(1.0))
                    .height(Sizing::Fixed(26.0))
                    .padding(Insets::xy(10.0, 0.0)),
            )
    };
    let action_button = |id: &str, text: &str, active: Option<Color>| {
        button(id, text, ui, active)
            .text(text)
            .padding(Insets::xy(14.0, 0.0))
            .height(Sizing::Fixed(30.0))
            .style(|s| s.weight = FontWeight::SemiBold)
    };
    let panel = Node::column()
        .id("export")
        .role(Role::Group)
        .accessible_label("Export")
        // Clicks between the buttons stay in the dialog.
        .clickable()
        .width(Sizing::Fixed(PANEL_W))
        .padding(Insets::all(20.0))
        .gap(4.0)
        .align(Align::Start)
        .style(|s| {
            s.background = Some(pal().panel);
            s.border = Some((1.0, pal().border));
            s.radius = [8.0; 4];
            s.shadow = Some((12.0, pal().shadow));
        })
        .child(
            Node::label("Export the mix")
                .style(|s| {
                    s.color = pal().text;
                    s.weight = FontWeight::SemiBold;
                    s.font_size = 15.0;
                })
                .padding(Insets {
                    bottom: 8.0,
                    ..Insets::default()
                }),
        )
        .child(row("export/format", "Format", format.to_owned()))
        .child(row("export/bits", "Samples", bits.to_owned()))
        .child(row("export/rate", "Sample rate", rate))
        .child(row("export/range", "Range", range))
        .child(row("export/tail", "Tail", tail))
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(8.0)
                .padding(Insets {
                    top: 14.0,
                    ..Insets::default()
                })
                .child(Node::spacer())
                .child(action_button("export/cancel", "Cancel", None))
                .child(action_button("export/start", "Export…", Some(pal().accent))),
        );
    Some(
        Node::row()
            .id("export/backdrop")
            .accessible_label("Export")
            .clickable()
            .scrollable()
            .absolute(area)
            .align(Align::Center)
            .justify(Justify::Center)
            .style(|s| {
                s.background = Some(Color::rgba(0.0, 0.0, 0.0, 0.45));
                s.radius = [0.0, 0.0, radius, radius];
            })
            .child(panel),
    )
}
