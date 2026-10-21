// SPDX-License-Identifier: AGPL-3.0-or-later
//! The "save changes?" dialog, shown before opening another project or
//! closing the window would discard unsaved changes.

use dk_ui::{Align, Color, FontWeight, Insets, Justify, Node, Rect, Role, Sizing, UiState};

use super::{ViewModel, button};
use crate::state::{PendingAction, ViewState};
use crate::theme::*;

const PANEL_W: f32 = 460.0;

/// The dialog over `area` (the window below its title bar), if it's open.
/// The backdrop takes every click and scroll, so nothing under it can be
/// changed while it's up.
pub(super) fn dialog(
    model: &ViewModel,
    view: &ViewState,
    ui: &UiState,
    area: Rect,
    radius: f32,
) -> Option<Node> {
    let action = view.confirm?;
    let consequence = match action {
        PendingAction::OpenProject => "Opening another project discards them.",
        PendingAction::Close => "Closing Dawkey discards them.",
    };
    // Without a file yet, saving asks where first.
    let save = if model.path.is_some() {
        "Save"
    } else {
        "Save…"
    };
    let dim = |text: String| {
        Node::label(text).style(|s| {
            s.color = pal().text_dim;
            s.font_size = 12.0;
        })
    };
    let action_button = |id: &str, label: &str, text: &str, active: Option<Color>| {
        button(id, label, ui, active)
            .text(text)
            .padding(Insets::xy(14.0, 0.0))
            .height(Sizing::Fixed(30.0))
            .style(|s| s.weight = FontWeight::SemiBold)
    };
    let panel = Node::column()
        .id("confirm")
        .role(Role::Group)
        .accessible_label("Save changes?")
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
        .child(Node::label("Save changes?").style(|s| {
            s.color = pal().text;
            s.weight = FontWeight::SemiBold;
            s.font_size = 15.0;
        }))
        .child(dim(format!("“{}” has unsaved changes.", model.title)))
        .child(dim(consequence.to_owned()))
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(8.0)
                .padding(Insets {
                    top: 14.0,
                    ..Insets::default()
                })
                .child(action_button(
                    "confirm/discard",
                    "Don't save",
                    "Don't save",
                    None,
                ))
                .child(Node::spacer())
                .child(action_button("confirm/cancel", "Cancel", "Cancel", None))
                .child(action_button(
                    "confirm/save",
                    "Save",
                    save,
                    Some(pal().accent),
                )),
        );
    Some(
        Node::row()
            .id("confirm/backdrop")
            .accessible_label("Save changes?")
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
