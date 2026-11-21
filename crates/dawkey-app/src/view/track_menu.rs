// SPDX-License-Identifier: AGPL-3.0-or-later
//! Arrange track header context menu and the Mix Down / Delete Track
//! confirmation dialogs (with non-destructive archival by default).

use dawkey_core::{TrackId, TrackKind};
use dawkey_ui::{
    Align, Color, FontWeight, Insets, Justify, Node, Rect, Role, Sizing, TextAlign, UiState,
};

use super::{ViewModel, button};
use crate::state::{TrackPopupKind, ViewState};
use crate::theme::*;

const MENU_W: f32 = 270.0;
const CONFIRM_W: f32 = 480.0;

/// Right-click context menu anchored at `view.track_menu`, if open.
pub(super) fn menu(model: &ViewModel, view: &ViewState, ui: &UiState, area: Rect) -> Option<Node> {
    let menu = view.track_menu?;
    let target = model.tracks.iter().find(|t| t.id == menu.track)?;
    let selected_count = model
        .tracks
        .iter()
        .filter(|t| view.shared.selected_tracks.contains(&t.id))
        .count()
        .max(1);
    let mixdown_text = if selected_count > 1 {
        format!("Mix Down {selected_count} Tracks to Audio Track…")
    } else {
        "Mix Down to Audio Track…".to_owned()
    };
    let delete_text = if selected_count > 1 {
        format!("Delete {selected_count} Tracks…")
    } else {
        "Delete Track…".to_owned()
    };
    let select_clips_text = if selected_count > 1 {
        format!("Select All Clips on {selected_count} Tracks")
    } else {
        "Select All Clips".to_owned()
    };

    let item = |id: &str, text: String| {
        let bg = if ui.is_pressed(id) {
            pal().pressed
        } else if ui.is_hovered(id) {
            pal().hover
        } else {
            Color::TRANSPARENT
        };
        Node::row()
            .id(id)
            .role(Role::Button)
            .accessible_label(text.clone())
            .clickable()
            .width(Sizing::Fill(1.0))
            .height(Sizing::Fixed(28.0))
            .padding(Insets::xy(SPACING.lg, 0.0))
            .style(|s| {
                s.background = Some(bg);
                s.radius = [4.0; 4];
            })
            .child(
                Node::label(text)
                    .width(Sizing::Fill(1.0))
                    .clip()
                    .style(|s| {
                        s.color = pal().text;
                        s.font_size = 12.0;
                        s.text_align = TextAlign::Start;
                    }),
            )
    };

    let items = 9
        + usize::from(target.has_mixdown_archive)
        + usize::from(model.deleted_archive_summary.is_some());
    let est_h = items as f32 * 28.0 + (items.saturating_sub(1)) as f32 * 2.0 + 12.0;
    let x = menu.pos.x.clamp(
        area.x + 4.0,
        (area.right() - MENU_W - 4.0).max(area.x + 4.0),
    ) - area.x;
    let y = menu.pos.y.clamp(
        area.y + 4.0,
        (area.bottom() - est_h - 4.0).max(area.y + 4.0),
    ) - area.y;

    let mut panel = Node::column()
        .id("track_menu")
        .role(Role::Group)
        .accessible_label("Track actions")
        .clickable()
        .width(Sizing::Fixed(MENU_W))
        .padding(Insets::all(6.0))
        .gap(2.0)
        .align(Align::Start)
        .style(|s| {
            s.background = Some(pal().panel);
            s.border = Some((1.0, pal().border));
            s.radius = [6.0; 4];
            s.shadow = Some(ELEVATION.medium());
        })
        .child(item("track_menu/rename", "Rename".to_owned()))
        .child(item("track_menu/duplicate", "Duplicate Track".to_owned()))
        .child(item("track_menu/color", "Color…".to_owned()))
        .child(item(
            "track_menu/insert_above",
            "Insert Track Above".to_owned(),
        ))
        .child(item(
            "track_menu/insert_below",
            "Insert Track Below".to_owned(),
        ))
        .child(item("track_menu/output", "Change Output…".to_owned()))
        .child(item("track_menu/select_all_clips", select_clips_text))
        .child(item("track_menu/mixdown", mixdown_text));

    if target.has_mixdown_archive {
        panel = panel.child(item(
            "track_menu/restore_mixdown",
            "Restore Mixed-Down Tracks".to_owned(),
        ));
    }

    panel = panel.child(item("track_menu/delete", delete_text));

    if let Some(summary) = &model.deleted_archive_summary {
        panel = panel.child(item("track_menu/restore_deleted", summary.clone()));
    }

    Some(
        Node::row()
            .id("track_menu/backdrop")
            .accessible_label("Track menu")
            .clickable()
            .scrollable()
            .absolute(area)
            .child(panel.absolute(Rect::new(x, y, MENU_W, est_h))),
    )
}

/// Floating popup anchored at `view.track_popup`, if open: a bus list
/// ("Change Output…") or a color swatch grid ("Color").
pub(super) fn popup(model: &ViewModel, view: &ViewState, area: Rect) -> Option<Node> {
    let popup = view.track_popup?;
    let buses: Vec<(TrackId, &str)> = model
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Bus && t.id != popup.track)
        .map(|t| (t.id, t.name.as_str()))
        .collect();
    let (w, content): (f32, Node) = match popup.kind {
        TrackPopupKind::Output => {
            let mut list = Node::column().gap(2.0).align(Align::Start);
            list = list.child(popup_item("track_popup/output/master", "Master".to_owned()));
            for (bus, name) in &buses {
                list = list.child(popup_item(
                    &format!("track_popup/output/bus/{bus}"),
                    name.to_string(),
                ));
            }
            (MENU_W, list)
        }
        TrackPopupKind::Color => {
            let mut grid = Node::row().gap(6.0).align(Align::Start);
            for (n, color) in TRACK_COLORS.iter().enumerate() {
                grid = grid.child(
                    Node::row()
                        .id(format!("track_popup/color/swatch/{n}"))
                        .role(Role::Button)
                        .accessible_label(format!("Color {}", n + 1))
                        .clickable()
                        .size(24.0, 24.0)
                        .style(|s| {
                            s.background = Some(*color);
                            s.radius = [4.0; 4];
                        }),
                );
            }
            let column = Node::column()
                .gap(SPACING.sm)
                .align(Align::Start)
                .child(grid)
                .child(popup_item(
                    "track_popup/color/default",
                    "Default".to_owned(),
                ));
            (8.0 * 24.0 + 7.0 * 6.0 + 2.0 * 6.0, column)
        }
    };
    let est_h = match popup.kind {
        TrackPopupKind::Output => (buses.len() + 1) as f32 * 28.0 + buses.len() as f32 * 2.0 + 12.0,
        TrackPopupKind::Color => 24.0 + SPACING.sm + 28.0 + 2.0 * SPACING.sm,
    };
    let x = popup
        .pos
        .x
        .clamp(area.x + 4.0, (area.right() - w - 4.0).max(area.x + 4.0))
        - area.x;
    let y = popup.pos.y.clamp(
        area.y + 4.0,
        (area.bottom() - est_h - 4.0).max(area.y + 4.0),
    ) - area.y;
    let panel = Node::column()
        .id("track_popup")
        .role(Role::Group)
        .accessible_label("Track popup")
        .clickable()
        .width(Sizing::Fixed(w))
        .padding(Insets::all(6.0))
        .align(Align::Start)
        .style(|s| {
            s.background = Some(pal().panel);
            s.border = Some((1.0, pal().border));
            s.radius = [6.0; 4];
            s.shadow = Some(ELEVATION.medium());
        })
        .child(content);
    Some(
        Node::row()
            .id("track_popup/backdrop")
            .accessible_label("Track popup")
            .clickable()
            .scrollable()
            .absolute(area)
            .child(panel.absolute(Rect::new(x, y, w, est_h))),
    )
}

fn popup_item(id: &str, text: String) -> Node {
    Node::row()
        .id(id)
        .role(Role::Button)
        .accessible_label(text.clone())
        .clickable()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(28.0))
        .padding(Insets::xy(SPACING.lg, 0.0))
        .style(|s| s.radius = [4.0; 4])
        .child(
            Node::label(text)
                .width(Sizing::Fill(1.0))
                .clip()
                .style(|s| {
                    s.color = pal().text;
                    s.font_size = 12.0;
                    s.text_align = TextAlign::Start;
                }),
        )
}

/// Confirmation dialog for mixing down selected tracks into one stereo audio track.
pub(super) fn confirm_dialog(
    model: &ViewModel,
    view: &ViewState,
    ui: &UiState,
    area: Rect,
    radius: f32,
) -> Option<Node> {
    let confirm = view.shared.mixdown_confirm.as_ref()?;
    let names = selected_names(model, &confirm.tracks);
    if names.is_empty() {
        return None;
    }
    let summary = if names.len() <= 4 {
        format!(
            "Render {} into a single stereo audio track.",
            names.join(", ")
        )
    } else {
        format!(
            "Render {} selected tracks into a single stereo audio track.",
            names.len()
        )
    };
    Some(confirm_modal(
        DialogSpec {
            panel_id: "mixdown_confirm",
            backdrop_id: "mixdown_confirm/backdrop",
            title: "Mix down tracks?",
            summary: &summary,
            checkbox_id: "mixdown_confirm/remove_originals",
            checkbox_a11y: "Remove original tracks",
            checkbox_label: "Remove original tracks (can be restored later)",
            checked: confirm.remove_originals,
            checked_color: pal().accent,
            cancel_id: "mixdown_confirm/cancel",
            confirm_id: "mixdown_confirm/confirm",
            confirm_label: "Mix Down",
            confirm_color: pal().accent,
        },
        ui,
        area,
        radius,
    ))
}

/// Confirmation dialog for deleting selected tracks (archived non-destructively
/// by default, with a checkbox for permanent deletion).
pub(super) fn delete_confirm_dialog(
    model: &ViewModel,
    view: &ViewState,
    ui: &UiState,
    area: Rect,
    radius: f32,
) -> Option<Node> {
    let confirm = view.shared.delete_confirm.as_ref()?;
    let names = selected_names(model, &confirm.tracks);
    if names.is_empty() {
        return None;
    }
    let plural = names.len() > 1;
    let title = if plural {
        "Delete tracks?"
    } else {
        "Delete track?"
    };
    let summary = if confirm.permanent {
        if names.len() <= 4 {
            format!(
                "Permanently delete {} and all exclusive audio and settings.",
                names.join(", ")
            )
        } else {
            format!(
                "Permanently delete {} selected tracks and all exclusive audio and settings.",
                names.len()
            )
        }
    } else if names.len() <= 4 {
        format!("Archive {} (can be restored later).", names.join(", "))
    } else {
        format!(
            "Archive {} selected tracks (can be restored later).",
            names.len()
        )
    };
    let checkbox_label = if plural {
        "Permanently delete tracks and their related audio and settings"
    } else {
        "Permanently delete track and its related audio and settings"
    };
    let (confirm_label, confirm_color) = if confirm.permanent {
        ("Delete Permanently", pal().record_on)
    } else {
        ("Delete", pal().accent)
    };
    Some(confirm_modal(
        DialogSpec {
            panel_id: "delete_confirm",
            backdrop_id: "delete_confirm/backdrop",
            title,
            summary: &summary,
            checkbox_id: "delete_confirm/permanent",
            checkbox_a11y: checkbox_label,
            checkbox_label,
            checked: confirm.permanent,
            checked_color: pal().record_on,
            cancel_id: "delete_confirm/cancel",
            confirm_id: "delete_confirm/confirm",
            confirm_label,
            confirm_color,
        },
        ui,
        area,
        radius,
    ))
}

fn selected_names<'a>(model: &'a ViewModel, tracks: &[TrackId]) -> Vec<&'a str> {
    model
        .tracks
        .iter()
        .filter(|t| tracks.contains(&t.id))
        .map(|t| t.name.as_str())
        .collect()
}

struct DialogSpec<'a> {
    panel_id: &'a str,
    backdrop_id: &'a str,
    title: &'a str,
    summary: &'a str,
    checkbox_id: &'a str,
    checkbox_a11y: &'a str,
    checkbox_label: &'a str,
    checked: bool,
    checked_color: Color,
    cancel_id: &'a str,
    confirm_id: &'a str,
    confirm_label: &'a str,
    confirm_color: Color,
}

fn confirm_modal(spec: DialogSpec<'_>, ui: &UiState, area: Rect, radius: f32) -> Node {
    let checked = spec.checked;
    let checkbox_row = Node::row()
        .id(spec.checkbox_id)
        .role(Role::Toggle)
        .accessible_label(spec.checkbox_a11y)
        .checked(checked)
        .clickable()
        .width(Sizing::Fill(1.0))
        .gap(SPACING.md)
        .padding(Insets {
            top: SPACING.md,
            bottom: SPACING.xs,
            ..Insets::default()
        })
        .child(
            Node::row()
                .size(16.0, 16.0)
                .align(Align::Center)
                .justify(Justify::Center)
                .style(|s| {
                    s.background = Some(if checked {
                        spec.checked_color
                    } else {
                        pal().track
                    });
                    s.border = Some((
                        1.0,
                        if checked {
                            spec.checked_color
                        } else {
                            pal().border
                        },
                    ));
                    s.radius = [3.0; 4];
                })
                .children(checked.then(|| {
                    Node::label("✓").style(|s| {
                        s.color = pal().on_text;
                        s.font_size = 11.0;
                        s.weight = FontWeight::SemiBold;
                    })
                })),
        )
        .child(
            Node::label(spec.checkbox_label)
                .width(Sizing::Fill(1.0))
                .style(|s| {
                    s.color = pal().text;
                    s.font_size = 12.0;
                    s.text_align = TextAlign::Start;
                }),
        );

    let action_button = |id: &str, text: &str, active: Option<Color>| {
        button(id, text, ui, active)
            .text(text)
            .padding(Insets::xy(SPACING.xl, 0.0))
            .height(Sizing::Fixed(30.0))
            .style(|s| s.weight = FontWeight::SemiBold)
    };

    let panel = Node::column()
        .id(spec.panel_id)
        .role(Role::Group)
        .accessible_label(spec.title)
        .clickable()
        .width(Sizing::Fixed(CONFIRM_W))
        .padding(Insets::all(SPACING.xxl))
        .gap(SPACING.sm)
        .align(Align::Start)
        .style(|s| {
            s.background = Some(pal().panel);
            s.border = Some((1.0, pal().border));
            s.radius = [PANEL_RADIUS; 4];
            s.shadow = Some(ELEVATION.medium());
        })
        .child(Node::label(spec.title).style(|s| {
            s.color = pal().text;
            s.weight = FontWeight::SemiBold;
            s.font_size = 15.0;
        }))
        .child(Node::label(spec.summary).style(|s| {
            s.color = pal().text_dim;
            s.font_size = 12.0;
            s.text_align = TextAlign::Start;
        }))
        .child(checkbox_row)
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(SPACING.md)
                .padding(Insets {
                    top: SPACING.lg,
                    ..Insets::default()
                })
                .child(Node::spacer())
                .child(action_button(spec.cancel_id, "Cancel", None))
                .child(action_button(
                    spec.confirm_id,
                    spec.confirm_label,
                    Some(spec.confirm_color),
                )),
        );

    Node::row()
        .id(spec.backdrop_id)
        .accessible_label(spec.title)
        .clickable()
        .scrollable()
        .absolute(area)
        .align(Align::Center)
        .justify(Justify::Center)
        .style(|s| {
            s.background = Some(Color::rgba(0.0, 0.0, 0.0, 0.45));
            s.radius = [0.0, 0.0, radius, radius];
        })
        .child(panel)
}
