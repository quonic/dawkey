// SPDX-License-Identifier: AGPL-3.0-or-later
//! The preferences panel: audio and MIDI, autosave, appearance, MCP over
//! HTTP and plugin folders. Each setting is a button that steps through its
//! choices (or a field to type in), and applies at once.

use std::path::Path;

use dawkey_ui::{
    Align, Color, FontWeight, Insets, Justify, Node, Rect, Role, Sizing, TextAlign, UiState,
};

use super::{ViewModel, button, text_field, toggle};
use crate::mcp_http::{McpHttpState, mcp_url};
use crate::prefs::{AudioChoice, LayoutChoice, ThemeChoice};
use crate::state::{PrefsPage, TextField, ViewState};
use crate::text_input::format_ms;
use crate::theme::*;

const PANEL_W: f32 = 760.0;
const LABEL_W: f32 = 150.0;

/// The panel over `area` (the window below its title bar), if it's open.
pub(super) fn panel(
    model: &ViewModel,
    view: &ViewState,
    ui: &UiState,
    area: Rect,
    radius: f32,
) -> Option<Node> {
    if !view.shared.prefs_open {
        return None;
    }
    let prefs = &model.prefs;
    let heading = |text: &str| {
        Node::label(text.to_owned())
            .padding(Insets {
                top: 10.0,
                bottom: 2.0,
                ..Insets::default()
            })
            .style(|s| {
                s.color = pal().text;
                s.weight = FontWeight::SemiBold;
                s.font_size = 12.0;
                s.text_align = TextAlign::Start;
            })
    };
    let row = |label: &str, control: Node| {
        Node::row()
            .width(Sizing::Fill(1.0))
            .height(Sizing::Fixed(30.0))
            .align(Align::Center)
            .child(
                Node::label(label.to_owned())
                    .width(Sizing::Fixed(LABEL_W))
                    .style(|s| {
                        s.color = pal().text_dim;
                        s.font_size = 12.0;
                        s.text_align = TextAlign::Start;
                    }),
            )
            .child(control)
    };
    let choice = |id: &str, label: &str, value: String| {
        button(id, label, ui, None)
            .value(value.clone())
            .text(value)
            .width(Sizing::Fill(1.0))
            .height(Sizing::Fixed(26.0))
            .padding(Insets::xy(SPACING.lg, 0.0))
    };
    let switch = |id: &str, label: &str, on: bool| {
        toggle(
            id,
            label,
            if on { "On" } else { "Off" },
            on,
            pal().accent,
            ui,
        )
        .height(Sizing::Fixed(26.0))
    };
    let note = |text: &str| {
        Node::label(text.to_owned())
            .padding(Insets {
                left: LABEL_W,
                ..Insets::default()
            })
            .style(|s| {
                s.color = pal().text_dim;
                s.font_size = 10.0;
                s.text_align = TextAlign::Start;
            })
    };
    let page_header = |title: &str, subtitle: &str| {
        Node::column()
            .width(Sizing::Fill(1.0))
            .gap(3.0)
            .padding(Insets {
                bottom: SPACING.sm,
                ..Insets::default()
            })
            .child(Node::label(title.to_owned()).style(|s| {
                s.color = pal().text;
                s.weight = FontWeight::SemiBold;
                s.font_size = 17.0;
                s.text_align = TextAlign::Start;
            }))
            .child(Node::label(subtitle.to_owned()).style(|s| {
                s.color = pal().text_dim;
                s.font_size = 11.0;
                s.text_align = TextAlign::Start;
            }))
    };

    let audio = match prefs.audio {
        AudioChoice::Pipewire => "PipeWire",
        AudioChoice::Virtual => "None (virtual clock)",
    };
    let offset = view
        .shared
        .text_edit
        .as_ref()
        .filter(|e| e.field == TextField::RecordOffset);
    let offset = match offset {
        Some(edit) => text_field(
            "prefs/record_offset",
            "Record offset in ms",
            edit.text.clone(),
            Some(edit.selected),
            ui,
        ),
        None => text_field(
            "prefs/record_offset",
            "Record offset in ms",
            format_ms(prefs.record_offset_ms),
            None,
            ui,
        ),
    }
    .width(Sizing::Fixed(90.0));
    let port = view
        .shared
        .text_edit
        .as_ref()
        .filter(|e| e.field == TextField::McpPort);
    let port = match port {
        Some(edit) => text_field(
            "prefs/mcp_port",
            "MCP port",
            edit.text.clone(),
            Some(edit.selected),
            ui,
        ),
        None => text_field(
            "prefs/mcp_port",
            "MCP port",
            prefs.mcp_http_port.to_string(),
            None,
            ui,
        ),
    }
    .width(Sizing::Fixed(90.0));
    let mcp_status = match &model.mcp_http {
        McpHttpState::Unavailable => "Only the window serves MCP over HTTP.".to_owned(),
        McpHttpState::Off => "Off. Clients can still use `dawkey mcp --attach`.".to_owned(),
        McpHttpState::Serving(addr) => format!("Serving {}", mcp_url(*addr)),
        McpHttpState::Failed(e) => format!("Not serving: {e}"),
    };
    let theme = match prefs.theme {
        ThemeChoice::System => "As the desktop",
        ThemeChoice::Dark => "Dark",
        ThemeChoice::Light => "Light",
    };
    let layout = match prefs.layout {
        LayoutChoice::Standard => "One window",
        LayoutChoice::MultiMonitor => "A window per monitor",
        LayoutChoice::Widescreen => "Arrange, Rack and Mixer side by side",
    };
    let mut content = Node::column()
        .id("prefs")
        .role(Role::Group)
        .accessible_label("Preferences")
        .clickable()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fill(1.0))
        .padding(Insets::all(SPACING.xxl))
        .gap(SPACING.xs)
        .align(Align::Start)
        .clip()
        .style(|s| {
            s.background = Some(pal().panel);
        });
    let selected = view.shared.prefs_page;
    match selected {
        PrefsPage::AudioMidi => {
            content = content
                .child(page_header(
                    "Audio & MIDI",
                    "Backend, connected devices, and recording alignment.",
                ))
                .child(heading("Audio"))
                .child(
                    Node::label(model.audio_status.clone())
                        .id("prefs/audio_status")
                        .value(model.audio_status.clone())
                        .style(|s| {
                            s.color = pal().text_dim;
                            s.font_size = 11.0;
                            s.text_align = TextAlign::Start;
                        }),
                )
                .child(note(
                    "Device routing is managed by PipeWire and WirePlumber.",
                ))
                .child(row(
                    "Plays through",
                    choice("prefs/audio", "Audio backend", audio.to_owned()),
                ))
                .child(row(
                    "Connect to devices",
                    switch(
                        "prefs/autoconnect",
                        "Connect to the default devices",
                        prefs.autoconnect,
                    ),
                ))
                .child(note("These two apply when Dawkey starts again."))
                .child(heading("MIDI devices"))
                .child(device_list("MIDI inputs", &model.midi_inputs))
                .child(device_list("MIDI outputs", &model.midi_outputs))
                .child(heading("Recording"))
                .child(row("Record offset (ms)", offset));
        }
        PrefsPage::Plugins => {
            content = content
                .child(page_header("Plugins", "Process isolation, search folders, and scanning."))
        .child(heading("Plugin processes"))
        .child(row(
            "Own processes",
            switch("prefs/plugins/bridge", "Run each plugin in a process of its own", prefs.bridge_plugins),
        ))
        .child(row(
            "Share processes",
            switch("prefs/plugins/share", "Instances of the same plugin share a process", prefs.share_plugin_processes),
        ))
        .child(row(
            "Sandbox",
            switch("prefs/plugins/sandbox", "Run plugins in a sandbox", prefs.sandbox_plugins),
        ))
        .child(note("A plugin crash affects only its own process and, when shared, its instances."))
        .child(note("Sandboxed plugins have no network access and write only to their own folders."))
        .child(note("Plugin process and sandbox changes take effect after restarting Dawkey."))
        .child(heading("Plugin search folders"))
        .child(note("These folders are searched in addition to the standard CLAP, LV2 and VST3 locations."));
            let folders = prefs
                .clap_folders
                .iter()
                .enumerate()
                .map(|(n, f)| ("clap", "CLAP", n, f))
                .chain(
                    prefs
                        .lv2_folders
                        .iter()
                        .enumerate()
                        .map(|(n, f)| ("lv2", "LV2", n, f)),
                )
                .chain(
                    prefs
                        .vst3_folders
                        .iter()
                        .enumerate()
                        .map(|(n, f)| ("vst3", "VST3", n, f)),
                );
            for (kind, label, n, folder) in folders {
                content = content.child(folder_row(ui, kind, label, n, folder));
            }
            content = content.child(
                Node::row()
                    .width(Sizing::Fill(1.0))
                    .gap(SPACING.sm)
                    .padding(Insets {
                        top: SPACING.sm,
                        ..Insets::default()
                    })
                    .child(small(ui, "prefs/plugins/add_clap", "Add CLAP folder…"))
                    .child(small(ui, "prefs/plugins/add_lv2", "Add LV2 folder…"))
                    .child(small(ui, "prefs/plugins/add_vst3", "Add VST3 folder…"))
                    .child(Node::spacer())
                    .child(small(ui, "prefs/plugins/rescan", "Rescan")),
            );
        }
        PrefsPage::ProjectSafety => {
            content = content
                .child(page_header(
                    "Project safety",
                    "Choose how Dawkey protects unsaved changes.",
                ))
                .child(heading("Autosave"))
                .child(row(
                    "Keep unsaved changes",
                    switch("prefs/autosave", "Autosave", prefs.autosave),
                ))
                .child(row(
                    "Every",
                    choice(
                        "prefs/autosave_every",
                        "Autosave interval",
                        match prefs.autosave_minutes {
                            1 => "Minute".to_owned(),
                            m => format!("{m} minutes"),
                        },
                    ),
                ))
                .child(note(
                    "Autosaves help recover unsaved project changes after a crash.",
                ));
        }
        PrefsPage::Appearance => {
            content = content
                .child(page_header(
                    "Appearance",
                    "Adjust the interface to suit your display.",
                ))
                .child(heading("Appearance"))
                .child(row(
                    "Theme",
                    choice("prefs/theme", "Theme", theme.to_owned()),
                ))
                .child(row(
                    "Scale",
                    choice(
                        "prefs/scale",
                        "UI scale",
                        format!("{:.0}%", prefs.ui_scale * 100.0),
                    ),
                ))
                .child(row(
                    "Layout",
                    choice("prefs/layout", "Layout", layout.to_owned()),
                ))
                .child(note("Theme and scale apply immediately. A window per monitor needs at least two monitors, and falls back to one window without them."));
        }
        PrefsPage::Mcp => {
            content = content
                .child(page_header(
                    "MCP",
                    "Optional authenticated HTTP access for MCP clients.",
                ))
                .child(heading("MCP over HTTP"))
                .child(row(
                    "Serve over HTTP",
                    Node::row()
                        .gap(SPACING.md)
                        .align(Align::Center)
                        .child(switch(
                            "prefs/mcp_http",
                            "Serve MCP over HTTP on this computer",
                            prefs.mcp_http,
                        ))
                        .child(Node::label("Port").style(|s| {
                            s.color = pal().text_dim;
                            s.font_size = 12.0;
                        }))
                        .child(port)
                        .child(Node::spacer())
                        .child(small(ui, "prefs/mcp_token", "New token")),
                ))
                .child(
                    note(&mcp_status)
                        .id("prefs/mcp_status")
                        .accessible_label("MCP over HTTP")
                        .value(mcp_status.clone()),
                )
                .children(model.mcp_token_path.as_ref().map(|path| {
                    note(&format!(
                        "Clients send the token in {path} as a bearer token."
                    ))
                }));
        }
    }
    let nav = [
        (PrefsPage::AudioMidi, "audio_midi", "Audio & MIDI"),
        (PrefsPage::Plugins, "plugins", "Plugins"),
        (PrefsPage::ProjectSafety, "project_safety", "Project safety"),
        (PrefsPage::Appearance, "appearance", "Appearance"),
        (PrefsPage::Mcp, "mcp", "MCP"),
    ];
    let panel_w = (area.w - 32.0).clamp(320.0, PANEL_W);
    let panel_h = (area.h - 32.0).clamp(240.0, 700.0);
    let sidebar_w = if panel_w < 600.0 { 126.0 } else { 158.0 };
    let mut sidebar = Node::column()
        .width(Sizing::Fixed(sidebar_w))
        .gap(SPACING.sm)
        .padding(Insets::all(SPACING.md));
    for (page, id, label) in nav {
        let mut item = button(&format!("prefs/page/{id}"), label, ui, None)
            .text(label)
            .width(Sizing::Fill(1.0))
            .height(Sizing::Fixed(34.0))
            .padding(Insets::xy(SPACING.md, 0.0));
        if selected == page {
            item = item.style(|s| {
                s.border = Some((1.0, pal().accent));
            });
        }
        sidebar = sidebar.child(item);
    }
    let body = Node::row()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fill(1.0))
        .child(sidebar)
        .child(
            Node::row()
                .width(Sizing::Fixed(1.0))
                .height(Sizing::Fill(1.0))
                .background(pal().border),
        )
        .child(content.scrollable());
    let panel = Node::column()
        .width(Sizing::Fixed(panel_w))
        .height(Sizing::Fixed(panel_h))
        .clip()
        .style(|s| {
            s.background = Some(pal().panel);
            s.border = Some((1.0, pal().border));
            s.radius = [PANEL_RADIUS; 4];
            s.shadow = Some(ELEVATION.medium());
        })
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .height(Sizing::Fixed(48.0))
                .padding(Insets::xy(SPACING.xxl, SPACING.sm))
                .align(Align::Center)
                .child(Node::label("Preferences").style(|s| {
                    s.color = pal().text;
                    s.weight = FontWeight::SemiBold;
                    s.font_size = 15.0;
                }))
                .child(Node::spacer())
                .child(
                    button("prefs/close", "Close", ui, None)
                        .text("Close")
                        .padding(Insets::xy(12.0, 0.0))
                        .height(Sizing::Fixed(26.0)),
                ),
        )
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .height(Sizing::Fixed(1.0))
                .background(pal().border),
        )
        .child(body);
    Some(
        Node::row()
            .id("prefs/backdrop")
            .accessible_label("Preferences")
            .clickable()
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

fn device_list(label: &str, devices: &[String]) -> Node {
    let value = if devices.is_empty() {
        "None detected".to_owned()
    } else {
        devices.join(", ")
    };
    Node::row()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(30.0))
        .align(Align::Center)
        .child(
            Node::label(label.to_owned())
                .width(Sizing::Fixed(LABEL_W))
                .style(|s| {
                    s.color = pal().text_dim;
                    s.font_size = 12.0;
                    s.text_align = TextAlign::Start;
                }),
        )
        .child(
            Node::label(value.clone())
                .id(if label == "MIDI inputs" {
                    "prefs/midi_inputs"
                } else {
                    "prefs/midi_outputs"
                })
                .value(value)
                .width(Sizing::Fill(1.0))
                .clip()
                .style(|s| {
                    s.color = pal().text;
                    s.font_size = 11.0;
                    s.text_align = TextAlign::Start;
                }),
        )
}

fn small(ui: &UiState, id: &str, text: &str) -> Node {
    button(id, text, ui, None)
        .text(text)
        .padding(Insets::xy(SPACING.lg, 0.0))
        .height(Sizing::Fixed(26.0))
}

/// One of the user's plugin folders, with a button to remove it.
fn folder_row(ui: &UiState, kind: &str, label: &str, n: usize, folder: &Path) -> Node {
    let path = folder.display().to_string();
    Node::row()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(26.0))
        .gap(SPACING.md)
        .align(Align::Center)
        .child(
            Node::label(label.to_owned())
                .width(Sizing::Fixed(40.0))
                .style(|s| {
                    s.color = pal().text_dim;
                    s.font_size = 11.0;
                    s.text_align = TextAlign::Start;
                }),
        )
        .child(
            Node::label(path.clone())
                .id(format!("prefs/plugins/folder/{kind}/{n}"))
                .accessible_label(format!("{label} folder"))
                .value(path)
                .width(Sizing::Fill(1.0))
                .clip()
                .style(|s| {
                    s.color = pal().text;
                    s.font_size = 11.0;
                    s.text_align = TextAlign::Start;
                }),
        )
        .child(small(
            ui,
            &format!("prefs/plugins/remove/{kind}/{n}"),
            "Remove",
        ))
}
