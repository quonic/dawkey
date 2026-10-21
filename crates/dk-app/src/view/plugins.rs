// SPDX-License-Identifier: AGPL-3.0-or-later
//! The plugin picker (choose a plugin to add to a track) and the plugin
//! editor (a generic panel of parameter sliders). Both float over the page.

use dk_ui::{Color, FontWeight, Insets, Node, Rect, Role, Sizing, TextAlign, UiState};

use super::{ViewModel, button, format_param, text_field, toggle};
use crate::state::{PARAM_W, TextField, ViewState};
use crate::theme::*;

/// Height of a row in the picker and editor lists.
pub(crate) const ROW_H: f32 = 30.0;
/// Where the first row of the picker's list is, and the editor's.
pub(crate) const PICKER_TOP: f32 = 6.0;
const EDITOR_TOP: f32 = 8.0;
const PANEL_W: f32 = 600.0;
const PANEL_H: f32 = 520.0;
/// Height of a panel's header; the list fills the rest.
const HEADER_H: f32 = 56.0;
/// Height of the picker's search field's row.
const SEARCH_H: f32 = 40.0;
/// Height of the editor's routing row.
const ROUTING_H: f32 = 36.0;
/// Width of a list's scroll bar, and its margin from the panel's edge.
const SCROLLBAR_W: f32 = 8.0;
const SCROLLBAR_MARGIN: f32 = 4.0;

/// The rows of a list scrolled by `scroll` that can be seen (with one to
/// spare at each end); building only these keeps long lists (hundreds of
/// plugins, thousands of parameters) fast.
fn visible<T>(items: &[T], scroll: f32) -> impl Iterator<Item = (usize, &T)> {
    let (first, count) = visible_window(scroll);
    items.iter().enumerate().skip(first).take(count)
}

/// The first row and how many rows a list scrolled by `scroll` shows.
pub(crate) fn visible_window(scroll: f32) -> (usize, usize) {
    let first = ((scroll / ROW_H).floor().max(0.0) as usize).saturating_sub(1);
    let count = ((PANEL_H - HEADER_H) / ROW_H).ceil() as usize + 3;
    (first, count)
}

/// How tall a list of `rows` is, with its margins.
pub(crate) fn content_height(rows: usize) -> f32 {
    rows as f32 * ROW_H + 16.0
}

/// The scroll bar's thumb, as its top and height in a bar `bar_h` high,
/// for a list `view_h` high showing `content_h` of rows scrolled by
/// `scroll`; `None` when it all shows.
pub(crate) fn thumb(scroll: f32, content_h: f32, view_h: f32, bar_h: f32) -> Option<(f32, f32)> {
    if content_h <= view_h + 0.5 || bar_h <= 0.0 {
        return None;
    }
    let h = (bar_h * view_h / content_h).clamp(24.0_f32.min(bar_h), bar_h);
    let y = (scroll / (content_h - view_h)).clamp(0.0, 1.0) * (bar_h - h);
    Some((y, h))
}

/// A scroll bar along the right of a list `view_h` high showing `rows`;
/// `None` when they all show. Dragging it (or clicking it) scrolls.
fn scrollbar(id: &str, ui: &UiState, scroll: f32, rows: usize, view_h: f32) -> Option<Node> {
    let content_h = content_height(rows);
    let bar_h = view_h - 2.0 * SCROLLBAR_MARGIN;
    let (y, h) = thumb(scroll, content_h, view_h, bar_h)?;
    let active = ui.is_pressed(id) || ui.is_hovered(id);
    let percent = (100.0 * scroll / (content_h - view_h))
        .round()
        .clamp(0.0, 100.0);
    Some(
        Node::column()
            .id(id)
            .role(Role::ScrollBar)
            .accessible_label("Scroll")
            .value(format!("{percent}%"))
            .draggable()
            .clickable()
            .absolute(Rect::new(
                PANEL_W - SCROLLBAR_W - SCROLLBAR_MARGIN,
                SCROLLBAR_MARGIN,
                SCROLLBAR_W,
                bar_h,
            ))
            .style(|s| {
                s.background = Some(pal().track);
                s.radius = [SCROLLBAR_W / 2.0; 4];
            })
            .child(
                Node::row()
                    .absolute(Rect::new(0.0, y, SCROLLBAR_W, h))
                    .style(|s| {
                        s.background = Some(if active {
                            pal().text_dim
                        } else {
                            pal().border.mix(pal().text_dim, 0.4)
                        });
                        s.radius = [SCROLLBAR_W / 2.0; 4];
                    }),
            ),
    )
}

/// The open overlay, if any: a dimmed backdrop with the panel on it.
pub(super) fn overlay(
    model: &ViewModel,
    view: &ViewState,
    ui: &UiState,
    size: dk_ui::Size,
) -> Option<Node> {
    let panel = if let (Some(track), Some(entries)) = (view.plugin_picker, &model.picker) {
        picker(model, view, ui, track, entries)
    } else if let Some(details) = &model.editor {
        editor(model, view, ui, details)
    } else {
        return None;
    };
    Some(
        Node::row()
            .id("overlay/backdrop")
            .accessible_label("Close")
            .clickable()
            .background(Color::rgba(0.0, 0.0, 0.0, 0.45))
            .absolute(Rect::new(0.0, 0.0, size.w, size.h))
            .align(dk_ui::Align::Start)
            .padding(Insets {
                left: 40.0,
                top: 20.0,
                ..Insets::default()
            })
            .child(panel),
    )
}

fn panel(id: &str, label: &str) -> Node {
    Node::column()
        .id(id)
        .accessible_label(label)
        .role(Role::Group)
        .width(Sizing::Fixed(PANEL_W))
        .height(Sizing::Fixed(PANEL_H))
        .clip()
        .style(|s| {
            s.background = Some(pal().panel);
            s.border = Some((1.0, pal().border));
            s.radius = [8.0; 4];
            s.shadow = Some((12.0, pal().shadow));
        })
}

fn header(title: String, subtitle: String) -> Node {
    Node::column()
        .align(dk_ui::Align::Start)
        .gap(2.0)
        .child(Node::label(title).style(|s| {
            s.color = pal().text;
            s.weight = FontWeight::SemiBold;
        }))
        .child(Node::label(subtitle).style(|s| {
            s.color = pal().text_dim;
            s.font_size = 11.0;
        }))
}

fn small_button(id: &str, label: &str, text: &str, ui: &UiState) -> Node {
    button(id, label, ui, None)
        .text(text)
        .padding(Insets::xy(10.0, 0.0))
        .height(Sizing::Fixed(26.0))
}

fn picker(
    model: &ViewModel,
    view: &ViewState,
    ui: &UiState,
    track: dk_core::TrackId,
    entries: &[super::PickerEntry],
) -> Node {
    let track_name = model
        .tracks
        .iter()
        .chain(std::iter::once(&model.master_track))
        .find(|t| t.id == track)
        .map_or_else(String::new, |t| t.name.clone());
    let filter = |id: &str, text: &str, on: bool| {
        toggle(
            &format!("plugin_picker/filter/{id}"),
            text,
            text,
            on,
            pal().accent,
            ui,
        )
        .height(Sizing::Fixed(26.0))
    };
    let top = Node::row()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(HEADER_H))
        .padding(Insets::xy(14.0, 0.0))
        .gap(6.0)
        .background(pal().toolbar)
        .child(header(
            "Add a plugin".to_owned(),
            if model.scanning {
                format!("to {track_name} · scanning…")
            } else {
                format!("to {track_name}")
            },
        ))
        .child(Node::spacer())
        .child(filter("all", "All", view.picker_filter.is_none()))
        .child(filter(
            "effects",
            "Effects",
            view.picker_filter == Some(false),
        ))
        .child(filter(
            "instruments",
            "Instruments",
            view.picker_filter == Some(true),
        ))
        .child(Node::row().width(Sizing::Fixed(6.0)))
        .child(small_button(
            "plugin_picker/rescan",
            "Scan for plugins again",
            "Rescan",
            ui,
        ))
        .child(small_button("plugin_picker/close", "Close", "Close", ui));
    let list_h = PANEL_H - HEADER_H - SEARCH_H;
    let mut list = Node::column()
        .id("plugin_picker/list")
        .role(Role::List)
        .accessible_label("Plugins")
        .scrollable()
        .clickable()
        .fill()
        .clip();
    if entries.is_empty() {
        let text = if model.scanning {
            "Scanning for plugins…"
        } else if !view.picker_query.is_empty() {
            "No plugins match. Backspace or Escape to search again."
        } else {
            "No plugins found. Install CLAP, LV2 or VST3 plugins (for example lsp-plugins), then Rescan."
        };
        list = list.child(
            Node::label(text)
                .id("plugin_picker/status")
                .fill()
                .style(|s| s.color = pal().text_dim),
        );
    }
    for (i, entry) in visible(entries, view.overlay_scroll) {
        let id = format!("plugin_picker/item/{i}");
        let y = PICKER_TOP + i as f32 * ROW_H - view.overlay_scroll;
        let kind = if entry.instrument {
            "Instrument"
        } else {
            "Effect"
        };
        let format = entry.id.split(':').next().unwrap_or("").to_uppercase();
        // Return adds the highlighted one.
        let picked = i == view.picker_selected;
        list = list.child(
            button(&id, &entry.name, ui, None)
                .value(entry.id.clone())
                .style(|s| {
                    s.border = None;
                    if picked {
                        s.background = Some(pal().accent.with_alpha(0.25));
                    }
                })
                .padding(Insets::xy(10.0, 0.0))
                .gap(10.0)
                .absolute(Rect::new(
                    8.0,
                    y,
                    PANEL_W - 16.0 - SCROLLBAR_W - SCROLLBAR_MARGIN,
                    ROW_H - 4.0,
                ))
                .child(
                    Node::label(entry.name.clone())
                        .width(Sizing::Fill(1.0))
                        .clip()
                        .style(|s| {
                            s.color = pal().text;
                            s.text_align = TextAlign::Start;
                        }),
                )
                .child(Node::label(entry.vendor.clone()).style(|s| {
                    s.color = pal().text_dim;
                    s.font_size = 11.0;
                }))
                .child(Node::label(format!("{kind} · {format}")).style(|s| {
                    s.color = pal().text_dim;
                    s.font_size = 10.0;
                })),
        );
    }
    let list = list.children(scrollbar(
        "plugin_picker/scrollbar",
        ui,
        view.overlay_scroll,
        entries.len(),
        list_h,
    ));
    panel("plugin_picker", "Add a plugin")
        .child(top)
        .child(search(view, entries.len()))
        .child(list)
}

/// The picker's search field: whatever's typed while the picker is open
/// goes in it (see `picker.rs`), so it always has the caret.
fn search(view: &ViewState, found: usize) -> Node {
    let query = view.picker_query.clone();
    let count = match (found, query.is_empty()) {
        (1, true) => "1 plugin".to_owned(),
        (n, true) => format!("{n} plugins"),
        (1, false) => "1 match".to_owned(),
        (n, false) => format!("{n} matches"),
    };
    let field = Node::row()
        .id("plugin_picker/search")
        .role(Role::TextField)
        .accessible_label("Search plugins")
        .value(query.clone())
        .clickable()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(26.0))
        .padding(Insets::xy(8.0, 0.0))
        .gap(1.0)
        .clip()
        .style(|s| {
            s.background = Some(pal().track);
            s.border = Some((1.0, pal().accent));
            s.radius = [5.0; 4];
        })
        .children((!query.is_empty()).then(|| {
            Node::label(query.clone()).style(|s| {
                s.color = pal().text;
                s.font_size = 12.0;
            })
        }))
        .child(Node::row().size(1.0, 14.0).background(pal().text))
        .children(query.is_empty().then(|| {
            Node::label("Type to search by name, vendor or format").style(|s| {
                s.color = pal().text_dim;
                s.font_size = 12.0;
            })
        }));
    Node::row()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(SEARCH_H))
        .padding(Insets::xy(14.0, 0.0))
        .gap(8.0)
        .child(field)
        .child(Node::label(count).style(|s| {
            s.color = pal().text_dim;
            s.font_size = 11.0;
        }))
}

/// The plugin's sidechain source and where its other outputs go, if it has
/// either: buttons that cycle through the choices.
fn routing(model: &ViewModel, ui: &UiState, d: &dk_studio::PluginDetails) -> Option<Node> {
    if !d.has_sidechain && d.outputs.is_empty() {
        return None;
    }
    let name = |track: Option<dk_core::TrackId>, none: &str| {
        track
            .and_then(|t| model.tracks.iter().find(|v| v.id == t))
            .map_or_else(|| none.to_owned(), |t| t.name.clone())
    };
    let choice = |id: String, label: String, text: String, set: bool| {
        button(
            &id,
            &label,
            ui,
            set.then_some(pal().accent.with_alpha(0.35)),
        )
        .value(text.clone())
        .text(format!("{label}: {text}"))
        .clip()
        .padding(Insets::xy(8.0, 0.0))
        .height(Sizing::Fixed(24.0))
        .style(|s| s.font_size = 11.0)
    };
    let mut row = Node::row()
        .id("plugin_editor/routing")
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(ROUTING_H))
        .padding(Insets::xy(14.0, 0.0))
        .gap(6.0)
        .clip()
        .style(|s| s.background = Some(pal().toolbar.mix(pal().panel, 0.5)));
    if d.has_sidechain {
        row = row.child(choice(
            format!("plugin/{}/sidechain", d.plugin),
            "Sidechain".to_owned(),
            name(d.sidechain, "none"),
            d.sidechain.is_some(),
        ));
    }
    for o in &d.outputs {
        row = row.child(choice(
            format!("plugin/{}/output/{}", d.plugin, o.output),
            o.name.clone(),
            format!("→ {}", name(o.bus, "nowhere")),
            o.bus.is_some(),
        ));
    }
    Some(row)
}

fn editor(model: &ViewModel, view: &ViewState, ui: &UiState, d: &dk_studio::PluginDetails) -> Node {
    let id = d.plugin;
    let subtitle = match (&d.error, d.vendor.is_empty()) {
        (Some(e), _) => format!("Not running: {e}"),
        (None, true) => d.id.clone(),
        (None, false) => format!("{} · {}", d.vendor, d.id),
    };
    let top = Node::row()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(HEADER_H))
        .padding(Insets::xy(14.0, 0.0))
        .gap(6.0)
        .background(pal().toolbar)
        .child(header(d.name.clone(), subtitle))
        .child(Node::spacer())
        // It crashed, or didn't load: try again.
        .children(d.error.is_some().then(|| {
            small_button(
                &format!("plugin/{id}/reload"),
                "Reload plugin",
                "Reload",
                ui,
            )
        }))
        // Its own GUI, in a window of its own.
        .children(d.has_gui.then(|| {
            toggle(
                &format!("plugin/{id}/gui"),
                "Show the plugin's own GUI",
                "GUI",
                d.gui_visible,
                pal().accent,
                ui,
            )
            .height(Sizing::Fixed(26.0))
        }))
        .child(
            toggle(
                &format!("plugin/{id}/bypass"),
                "Bypass",
                "Bypass",
                d.bypass,
                pal().mute_on,
                ui,
            )
            .height(Sizing::Fixed(26.0)),
        )
        .child(small_button(
            &format!("plugin/{id}/remove"),
            "Remove plugin",
            "Remove",
            ui,
        ))
        .child(small_button("plugin_editor/close", "Close", "Close", ui));
    let routing = routing(model, ui, d);
    let list_h = PANEL_H - HEADER_H - if routing.is_some() { ROUTING_H } else { 0.0 };
    let mut list = Node::column()
        .id("plugin_editor/params")
        .role(Role::List)
        .accessible_label("Parameters")
        .scrollable()
        .fill()
        .clip();
    if d.param_count == 0 {
        list = list.child(
            Node::label("This plugin has no parameters.")
                .fill()
                .style(|s| s.color = pal().text_dim),
        );
    }
    // The model holds only the parameters in view, from `first_param`.
    for (i, p) in d
        .params
        .iter()
        .enumerate()
        .map(|(j, p)| (d.first_param + j, p))
    {
        let y = EDITOR_TOP + i as f32 * ROW_H - view.overlay_scroll;
        let slider_id = format!("plugin/{id}/param/{}", p.id);
        let unit = if p.max > p.min {
            ((p.value - p.min) / (p.max - p.min)).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let active = ui.is_pressed(&slider_id) || ui.is_hovered(&slider_id);
        let text = p.text.clone();
        let typing = view.text_edit.as_ref().filter(|e| {
            matches!(&e.field, TextField::PluginParam { plugin, param }
                if *plugin == d.plugin && *param == p.id)
        });
        list = list.child(
            Node::label(p.name.clone())
                .clip()
                .absolute(Rect::new(14.0, y, 230.0, ROW_H - 6.0))
                .style(|s| {
                    s.color = pal().text;
                    s.font_size = 12.0;
                    s.text_align = TextAlign::Start;
                }),
        );
        // Double-clicked: a field to type the value in.
        let control = match typing {
            Some(edit) => text_field(
                &slider_id,
                &p.name,
                edit.text.clone(),
                Some(edit.selected),
                ui,
            )
            .height(Sizing::Fixed(ROW_H - 8.0))
            .absolute(Rect::new(250.0, y + 1.0, PARAM_W, ROW_H - 8.0)),
            None => Node::row()
                .id(slider_id)
                .role(Role::Slider)
                .accessible_label(p.name.clone())
                .value(text.clone())
                .draggable()
                .clickable()
                .clip()
                .absolute(Rect::new(250.0, y + 1.0, PARAM_W, ROW_H - 8.0))
                .style(|s| {
                    s.background = Some(if active { pal().hover } else { pal().track });
                    s.border = Some((1.0, pal().border));
                    s.radius = [4.0; 4];
                })
                .child(
                    Node::row()
                        .style(|s| {
                            s.background = Some(pal().accent.with_alpha(0.4));
                            s.radius = [4.0; 4];
                        })
                        .absolute(Rect::new(0.0, 0.0, PARAM_W * unit, ROW_H - 8.0)),
                )
                .child(
                    Node::label(text)
                        .absolute(Rect::new(0.0, 0.0, PARAM_W, ROW_H - 8.0))
                        .style(|s| {
                            s.font_size = 11.0;
                            s.color = pal().text;
                        }),
                ),
        };
        list = list
            .child(control)
            .child(
                Node::label(format!("{} – {}", format_param(p.min), format_param(p.max)))
                    .absolute(Rect::new(440.0, y, 90.0, ROW_H - 6.0))
                    .style(|s| {
                        s.color = pal().text_dim;
                        s.font_size = 10.0;
                        s.text_align = TextAlign::Start;
                    }),
            )
            .child(
                toggle(
                    &format!("plugin/{id}/param/{}/automate", p.id),
                    "Automate",
                    "A",
                    p.automated,
                    pal().accent,
                    ui,
                )
                .size(28.0, ROW_H - 8.0)
                .padding(Insets::default())
                .absolute(Rect::new(PANEL_W - 50.0, y + 1.0, 28.0, ROW_H - 8.0)),
            );
    }
    let list = list.children(scrollbar(
        "plugin_editor/scrollbar",
        ui,
        view.overlay_scroll,
        d.param_count,
        list_h,
    ));
    panel("plugin_editor", "Plugin")
        .child(top)
        .children(routing)
        .child(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_build_only_the_rows_in_view() {
        // 464 px of list: 16 rows, plus spares.
        assert_eq!(visible_window(0.0), (0, 19));
        // Scrolled 100 rows down: from one row before the first in view.
        assert_eq!(visible_window(100.0 * ROW_H), (99, 19));
        let items: Vec<u32> = (0..10_000).collect();
        let shown: Vec<usize> = visible(&items, 5000.0 * ROW_H).map(|(i, _)| i).collect();
        assert_eq!((shown[0], shown.len()), (4999, 19));
    }

    #[test]
    fn scroll_bar_thumbs() {
        // It all shows: no bar.
        assert_eq!(thumb(0.0, 300.0, 400.0, 392.0), None);
        // Twice the view: half the bar, at the top, then the bottom.
        assert_eq!(thumb(0.0, 800.0, 400.0, 400.0), Some((0.0, 200.0)));
        assert_eq!(thumb(400.0, 800.0, 400.0, 400.0), Some((200.0, 200.0)));
        // Thousands of rows: still big enough to grab.
        let (y, h) = thumb(0.0, content_height(5000), 400.0, 400.0).unwrap();
        assert_eq!((y, h), (0.0, 24.0));
    }
}
