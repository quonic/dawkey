// SPDX-License-Identifier: AGPL-3.0-or-later
//! The preferences panel: audio and MIDI, autosave, appearance, MCP over
//! HTTP and plugin folders. Each setting is a button that steps through its
//! choices (or a field to type in), and applies at once.

use std::path::Path;

use dk_ui::{
    Align, Color, FontWeight, Insets, Justify, Node, Rect, Role, Sizing, TextAlign, UiState,
};

use super::{ViewModel, button, text_field, toggle};
use crate::mcp_http::{McpHttpState, mcp_url};
use crate::prefs::{AudioChoice, ThemeChoice};
use crate::state::{TextField, ViewState};
use crate::text_input::format_ms;
use crate::theme::*;

const PANEL_W: f32 = 540.0;
const LABEL_W: f32 = 150.0;

/// The panel over `area` (the window below its title bar), if it's open.
pub(super) fn panel(
    model: &ViewModel,
    view: &ViewState,
    ui: &UiState,
    area: Rect,
    radius: f32,
) -> Option<Node> {
    if !view.prefs_open {
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
            .padding(Insets::xy(10.0, 0.0))
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

    let audio = match prefs.audio {
        AudioChoice::Pipewire => "PipeWire",
        AudioChoice::Virtual => "None (virtual clock)",
    };
    let offset = view
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
    let mut panel = Node::column()
        .id("prefs")
        .role(Role::Group)
        .accessible_label("Preferences")
        .clickable()
        .width(Sizing::Fixed(PANEL_W))
        .padding(Insets::all(20.0))
        .gap(2.0)
        .align(Align::Start)
        .clip()
        .style(|s| {
            s.background = Some(pal().panel);
            s.border = Some((1.0, pal().border));
            s.radius = [8.0; 4];
            s.shadow = Some((12.0, pal().shadow));
        })
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
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
        .child(heading("Audio & MIDI"))
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
        .child(row("Record offset (ms)", offset))
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
        .child(heading("MCP"))
        .child(row(
            "Serve over HTTP",
            Node::row()
                .gap(8.0)
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
        }))
        .child(heading("Plugins"))
        .child(row(
            "Own processes",
            switch(
                "prefs/plugins/bridge",
                "Run each plugin in a process of its own",
                prefs.bridge_plugins,
            ),
        ))
        .child(row(
            "Share processes",
            switch(
                "prefs/plugins/share",
                "Instances of the same plugin share a process",
                prefs.share_plugin_processes,
            ),
        ))
        .child(row(
            "Sandbox",
            switch(
                "prefs/plugins/sandbox",
                "Run plugins in a sandbox",
                prefs.sandbox_plugins,
            ),
        ))
        .child(note(
            "A plugin that crashes takes only itself down (shared: its instances).",
        ))
        .child(note(
            "Sandboxed: no network, and they write only to their own folders.",
        ))
        .child(note(
            "Each adds a block of latency. Applies when Dawkey starts again.",
        ))
        .child(note(
            "Folders searched besides the standard ones ($CLAP_PATH, $LV2_PATH, $VST3_PATH):",
        ));
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
        panel = panel.child(folder_row(ui, kind, label, n, folder));
    }
    panel = panel.child(
        Node::row()
            .width(Sizing::Fill(1.0))
            .gap(6.0)
            .padding(Insets {
                top: 6.0,
                ..Insets::default()
            })
            .child(small(ui, "prefs/plugins/add_clap", "Add CLAP folder…"))
            .child(small(ui, "prefs/plugins/add_lv2", "Add LV2 folder…"))
            .child(small(ui, "prefs/plugins/add_vst3", "Add VST3 folder…"))
            .child(Node::spacer())
            .child(small(ui, "prefs/plugins/rescan", "Rescan")),
    );
    Some(
        Node::row()
            .id("prefs/backdrop")
            .accessible_label("Preferences")
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

fn small(ui: &UiState, id: &str, text: &str) -> Node {
    button(id, text, ui, None)
        .text(text)
        .padding(Insets::xy(10.0, 0.0))
        .height(Sizing::Fixed(26.0))
}

/// One of the user's plugin folders, with a button to remove it.
fn folder_row(ui: &UiState, kind: &str, label: &str, n: usize, folder: &Path) -> Node {
    let path = folder.display().to_string();
    Node::row()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(26.0))
        .gap(8.0)
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
