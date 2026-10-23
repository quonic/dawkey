// SPDX-License-Identifier: AGPL-3.0-or-later
//! The "recover unsaved changes?" dialog, after Dawkey stopped without
//! exiting cleanly.

use dawkey_ui::{Align, Color, FontWeight, Insets, Justify, Node, Rect, Role, Sizing, UiState};

use super::button;
use crate::recovery::ago;
use crate::state::ViewState;
use crate::theme::*;

const PANEL_W: f32 = 480.0;

/// The dialog over `area` (the window below its title bar), if it's open.
pub(super) fn dialog(view: &ViewState, ui: &UiState, area: Rect, radius: f32) -> Option<Node> {
    let recovery = view.recovery.as_ref()?;
    let what = match &recovery.bundle {
        Some(bundle) => {
            let name = bundle
                .file_stem()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            format!("Dawkey stopped before “{name}” was saved.")
        }
        None => "Dawkey stopped before a new project was saved.".to_owned(),
    };
    let dim = |text: String| {
        Node::label(text).style(|s| {
            s.color = pal().text_dim;
            s.font_size = 12.0;
        })
    };
    let action_button = |id: &str, text: &str, active: Option<Color>| {
        button(id, text, ui, active)
            .text(text)
            .padding(Insets::xy(14.0, 0.0))
            .height(Sizing::Fixed(30.0))
            .style(|s| s.weight = FontWeight::SemiBold)
    };
    let panel = Node::column()
        .id("recover")
        .role(Role::Group)
        .accessible_label("Recover unsaved changes?")
        .clickable()
        .width(Sizing::Fixed(PANEL_W))
        .padding(Insets::all(20.0))
        .gap(6.0)
        .align(Align::Start)
        .style(|s| {
            s.background = Some(pal().panel);
            s.border = Some((1.0, pal().border));
            s.radius = [8.0; 4];
            s.shadow = Some((12.0, pal().shadow));
        })
        .child(Node::label("Recover unsaved changes?").style(|s| {
            s.color = pal().text;
            s.weight = FontWeight::SemiBold;
            s.font_size = 15.0;
        }))
        .child(dim(what))
        .child(dim(format!(
            "Its changes were kept {}.",
            ago(recovery.age())
        )))
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(8.0)
                .padding(Insets {
                    top: 14.0,
                    ..Insets::default()
                })
                .child(action_button("recover/discard", "Discard", None))
                .child(Node::spacer())
                .child(action_button(
                    "recover/recover",
                    "Recover",
                    Some(pal().accent),
                )),
        );
    Some(
        Node::row()
            .id("recover/backdrop")
            .accessible_label("Recover unsaved changes?")
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
