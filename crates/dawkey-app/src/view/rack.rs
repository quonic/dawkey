// SPDX-License-Identifier: AGPL-3.0-or-later
//! 19-inch studio guitar effects rack (`Page::Rack`).
//!
//! Displays a track selector & quick-mount sidebar on the left and a 19-inch
//! steel-rail equipment cabinet on the right. Every mounted unit—whether a
//! built-in `builtin:*` guitar processor or a hosted CLAP/LV2/VST3 plugin—
//! sits in rack ears with hardware toggles, status LEDs, unit-specific
//! studio displays, and rotary potentiometer knobs.

use dawkey_core::TrackKind;
use dawkey_studio::{ParamState, PluginDetails};
use dawkey_ui::{
    Align, Color, FontFamily, FontWeight, Insets, Justify, Node, Paint, Point, Rect, Role, Sizing,
    TextAlign, UiState,
};

use super::{TrackView, ViewModel, button, meter, name_editor, text_field, toggle, track_buttons};
use crate::state::{TextField, ViewState};
use crate::theme::*;

const SIDEBAR_W: f32 = 212.0;
const BAY_PAD: f32 = 14.0;
const EAR_W: f32 = 24.0;
const UNIT_1U_H: f32 = 126.0;
const UNIT_2U_H: f32 = 186.0;
const BLANK_H: f32 = 72.0;
const UNIT_GAP: f32 = 6.0;
const EXT_PAGE_SIZE: usize = 8;

/// Height of one rack unit chassis (`2U` for the tube preamp, `1U` for others).
pub(crate) fn unit_height(id: &str) -> f32 {
    if id == "builtin:amp" {
        UNIT_2U_H
    } else {
        UNIT_1U_H
    }
}

/// Total height of the mounted units and blanking panel inside `rack/bay`.
pub(crate) fn content_height<'a>(unit_ids: impl IntoIterator<Item = &'a str>) -> f32 {
    let units_h: f32 = unit_ids
        .into_iter()
        .map(|id| unit_height(id) + UNIT_GAP)
        .sum();
    BAY_PAD * 2.0 + units_h + BLANK_H
}

pub(super) fn build(model: &ViewModel, view: &ViewState, ui: &UiState, content_w: f32) -> Node {
    let bay_inner_w = (content_w - SIDEBAR_W - BAY_PAD * 2.0).max(520.0);
    Node::row()
        .id("rack")
        .fill()
        .background(pal().panel)
        .child(sidebar(model, view, ui))
        .child(cabinet(model, view, ui, bay_inner_w))
}

fn sidebar(model: &ViewModel, view: &ViewState, ui: &UiState) -> Node {
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

    let mut tracks_col = Node::column().width(Sizing::Fill(1.0)).gap(3.0);
    for t in model
        .tracks
        .iter()
        .chain(std::iter::once(&model.master_track))
    {
        let selected = t.id == model.rack_track;
        let id = format!("rack/track/{}", t.id);
        let bg = if selected {
            pal().accent.with_alpha(0.22)
        } else if ui.is_hovered(&id) {
            pal().hover
        } else {
            pal().track
        };
        let border = if selected { pal().accent } else { pal().border };
        let count = t.plugins.len();
        let badge = if count == 0 {
            "—".to_owned()
        } else {
            format!("{count}")
        };
        tracks_col = tracks_col.child(
            Node::row()
                .id(id)
                .role(Role::ListItem)
                .accessible_label(t.name.clone())
                .value(if selected { "selected" } else { "" })
                .clickable()
                .width(Sizing::Fill(1.0))
                .height(Sizing::Fixed(24.0))
                .padding(Insets::xy(8.0, 0.0))
                .gap(6.0)
                .style(move |s| {
                    s.background = Some(bg);
                    s.border = Some((1.0, border));
                    s.radius = [4.0; 4];
                })
                .child(
                    Node::label(t.name.clone())
                        .width(Sizing::Fill(1.0))
                        .clip()
                        .style(move |s| {
                            s.font_size = 11.0;
                            s.weight = if selected {
                                FontWeight::SemiBold
                            } else {
                                FontWeight::Regular
                            };
                            s.color = pal().text;
                            s.text_align = TextAlign::Start;
                        }),
                )
                .child(Node::label(badge).style(|s| {
                    s.font_size = 10.0;
                    s.family = FontFamily::Mono;
                    s.color = pal().text_dim;
                })),
        );
    }

    let preset_btn = |id: &str, label: &str| {
        button(&format!("rack/preset/{id}"), label, ui, None)
            .text(label)
            .width(Sizing::Fill(1.0))
            .height(Sizing::Fixed(24.0))
            .style(|s| s.font_size = 10.0)
    };
    let presets = Node::column()
        .width(Sizing::Fill(1.0))
        .gap(4.0)
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(preset_btn("clean", "Clean Studio"))
                .child(preset_btn("crunch", "Crunch")),
        )
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(preset_btn("lead", "High-Gain Lead"))
                .child(preset_btn("ambient", "Ambient")),
        )
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(preset_btn("blues", "Blues"))
                .child(preset_btn("metal", "Metal")),
        )
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(preset_btn("jazz", "Jazz"))
                .child(preset_btn("acoustic", "Acoustic")),
        )
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(preset_btn("bass", "Bass"))
                .child(preset_btn("bass_drive", "Bass Drive")),
        );

    let track = model.rack_track;
    let mut your_presets = Node::column().width(Sizing::Fill(1.0)).gap(4.0);
    for (i, name) in model.rack_presets.iter().enumerate() {
        your_presets = your_presets.child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(
                    button(
                        &format!("rack/saved/{i}"),
                        &format!("Mount \"{name}\""),
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
                        &format!("rack/remove_saved/{i}"),
                        &format!("Remove the \"{name}\" preset"),
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
    your_presets = your_presets.child(
        name_editor(&TextField::RackPresetName(track), "Preset name", view, ui).unwrap_or_else(
            || {
                button(
                    "rack/save_as",
                    "Save the current chain as a preset",
                    ui,
                    None,
                )
                .text("+ Save as preset…")
                .width(Sizing::Fill(1.0))
                .height(Sizing::Fixed(22.0))
                .style(|s| s.font_size = 10.0)
            },
        ),
    );

    let add_btn = |kind: &str, label: &str| {
        button(&format!("rack/add/{kind}"), label, ui, None)
            .text(label)
            .width(Sizing::Fill(1.0))
            .height(Sizing::Fixed(22.0))
            .padding(Insets::xy(6.0, 0.0))
            .style(|s| {
                s.font_size = 10.0;
                s.text_align = TextAlign::Start;
            })
    };
    let units = Node::column()
        .width(Sizing::Fill(1.0))
        .gap(3.0)
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(add_btn("gate", "+ GATE-100"))
                .child(add_btn("comp", "+ COMP-76")),
        )
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(add_btn("drive", "+ OD-808"))
                .child(add_btn("amp", "+ JMP-2000")),
        )
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(add_btn("eq", "+ PEQ-500"))
                .child(add_btn("cab", "+ CAB-IR")),
        )
        .child(
            Node::row()
                .width(Sizing::Fill(1.0))
                .gap(4.0)
                .child(add_btn("chorus", "+ DIM-C"))
                .child(add_btn("delay", "+ DDL-2290")),
        )
        .child(add_btn("reverb", "+ VERB-480 Studio Reverb"));

    Node::column()
        .id("rack/sidebar")
        .width(Sizing::Fixed(SIDEBAR_W))
        .height(Sizing::Fill(1.0))
        .padding(Insets::all(10.0))
        .gap(6.0)
        .clip()
        .style(|s| {
            s.background = Some(pal().panel);
            s.border = Some((1.0, pal().border));
        })
        .child(section("TRACKS & BUSES"))
        .child(tracks_col)
        .child(section("GUITAR RIG PRESETS"))
        .child(presets)
        .child(section("YOUR PRESETS"))
        .child(your_presets)
        .child(section("MOUNT STUDIO UNIT"))
        .child(units)
        .child(Node::spacer())
        .child(
            button("rack/add_plugin", "Mount external plugin", ui, None)
                .text("+ External Plugin…")
                .width(Sizing::Fill(1.0))
                .height(Sizing::Fixed(28.0))
                .style(|s| {
                    s.font_size = 11.0;
                    s.weight = FontWeight::SemiBold;
                }),
        )
}

fn active_track(model: &ViewModel) -> &TrackView {
    model
        .tracks
        .iter()
        .find(|t| t.id == model.rack_track)
        .unwrap_or(&model.master_track)
}

fn cabinet(model: &ViewModel, view: &ViewState, ui: &UiState, bay_inner_w: f32) -> Node {
    let track = active_track(model);
    let kind = if track.id == dawkey_core::MASTER {
        "Master"
    } else {
        match track.kind {
            TrackKind::Audio => "Audio",
            TrackKind::Midi => "MIDI",
            TrackKind::Bus => "Bus",
        }
    };

    let mut header = Node::row()
        .id("rack/header")
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(40.0))
        .padding(Insets::xy(14.0, 0.0))
        .gap(8.0)
        .style(|s| {
            s.background = Some(pal().toolbar);
            s.border = Some((1.0, pal().border));
        })
        .child(
            Node::label(format!("19\" RACK · {}", track.name)).style(|s| {
                s.weight = FontWeight::SemiBold;
                s.color = pal().text;
            }),
        )
        .child(Node::label(kind).padding(Insets::xy(6.0, 2.0)).style(|s| {
            s.font_size = 10.0;
            s.color = pal().text_dim;
            s.background = Some(pal().track);
            s.radius = [3.0; 4];
        }))
        .child(Node::row().gap(4.0).children(track_buttons(track, ui)));

    if track.kind == TrackKind::Audio {
        header = header.child(
            toggle(
                &format!("track/{}/monitor", track.id),
                "Input monitoring",
                "In",
                track.monitor,
                pal().monitor_on,
                ui,
            )
            .size(30.0, 24.0)
            .padding(Insets::default())
            .style(|s| s.font_size = 11.0),
        );
    }
    if track.id != dawkey_core::MASTER {
        header = header.child(
            button(&format!("track/{}/output", track.id), "Output", ui, None)
                .text(format!("→ {}", track.output_name))
                .height(Sizing::Fixed(24.0))
                .padding(Insets::xy(8.0, 0.0))
                .style(|s| {
                    s.font_size = 11.0;
                    s.color = pal().text_dim;
                }),
        );
    }
    header = header
        .child(Node::spacer())
        .child(meter(
            &format!("track/{}/meter", track.id),
            "Track level",
            track.level,
            false,
            88.0,
            14.0,
        ))
        .child(
            button(
                &format!("track/{}/add_plugin", track.id),
                "Add plugin to rack",
                ui,
                None,
            )
            .text("+ Add FX")
            .height(Sizing::Fixed(26.0))
            .padding(Insets::xy(10.0, 0.0))
            .style(|s| s.font_size = 11.0),
        );

    let mut bay = Node::column()
        .id("rack/bay")
        .scrollable()
        .fill()
        .clip()
        .padding(Insets::all(BAY_PAD))
        .background(pal().rack_rail)
        // Left & right steel rack rail trim lines.
        .child(
            Node::row()
                .background(pal().rack_ear.mix(pal().border, 0.4))
                .absolute(Rect::new(-4.0, 0.0, 4.0, 4000.0)),
        )
        .child(
            Node::row()
                .background(pal().rack_ear.mix(pal().border, 0.4))
                .absolute(Rect::new(bay_inner_w, 0.0, 4.0, 4000.0)),
        );

    let mut y = -view.rack_scroll;
    let total = model.rack_units.len();
    for (idx, unit) in model.rack_units.iter().enumerate() {
        let h = unit_height(&unit.id);
        let chassis = rack_unit(model, view, ui, unit, idx, total, bay_inner_w)
            .absolute(Rect::new(0.0, y, bay_inner_w, h));
        bay = bay.child(chassis);
        y += h + UNIT_GAP;
    }

    bay = bay.child(blank_panel(total == 0, ui).absolute(Rect::new(0.0, y, bay_inner_w, BLANK_H)));

    Node::column()
        .id("rack/cabinet")
        .fill()
        .clip()
        .child(header)
        .child(bay)
}

fn blank_panel(empty: bool, ui: &UiState) -> Node {
    let prompt = if empty {
        "Rack empty — choose a Guitar Rig Preset on the left or mount a unit below"
    } else {
        "+ Mount Studio Unit or External Plugin"
    };
    Node::row()
        .id("rack/blank")
        .fill()
        .style(|s| {
            s.background = Some(pal().rack_ear.mix(pal().panel, 0.4));
            s.border = Some((1.0, pal().border));
            s.radius = [4.0; 4];
        })
        .child(rack_ear())
        .child(
            Node::column()
                .fill()
                .justify(Justify::Center)
                .align(Align::Center)
                .gap(6.0)
                .child(
                    Node::row()
                        .width(Sizing::Fixed(320.0))
                        .height(Sizing::Fixed(4.0))
                        .style(|s| {
                            s.background = Some(pal().rack_rail);
                            s.radius = [2.0; 4];
                        }),
                )
                .child(
                    button("rack/add_plugin", "Mount plugin", ui, None)
                        .text(prompt)
                        .height(Sizing::Fixed(28.0))
                        .padding(Insets::xy(14.0, 0.0))
                        .style(|s| {
                            s.font_size = 11.0;
                            s.color = pal().text_dim;
                        }),
                )
                .child(
                    Node::row()
                        .width(Sizing::Fixed(320.0))
                        .height(Sizing::Fixed(4.0))
                        .style(|s| {
                            s.background = Some(pal().rack_rail);
                            s.radius = [2.0; 4];
                        }),
                ),
        )
        .child(rack_ear())
}

fn rack_ear() -> Node {
    let bolt = || {
        Node::row()
            .size(11.0, 11.0)
            .justify(Justify::Center)
            .align(Align::Center)
            .style(|s| {
                s.background = Some(pal().rack_screw);
                s.border = Some((1.0, pal().border));
                s.radius = [5.5; 4];
            })
            .child(Node::row().size(7.0, 1.5).background(pal().rack_rail))
    };
    Node::column()
        .width(Sizing::Fixed(EAR_W))
        .height(Sizing::Fill(1.0))
        .padding(Insets::xy(0.0, 10.0))
        .justify(Justify::Center)
        .align(Align::Center)
        .background(pal().rack_ear)
        .child(bolt())
        .child(Node::spacer())
        .child(bolt())
}

struct UnitTheme {
    faceplate: Color,
    accent: Color,
    led: Color,
    badge: &'static str,
    subtitle: &'static str,
}

fn unit_theme(unit: &PluginDetails) -> UnitTheme {
    match unit.id.as_str() {
        "builtin:gate" => UnitTheme {
            faceplate: pal().rack_gate,
            accent: pal().solo_on,
            led: pal().rack_led_on,
            badge: "GATE-100",
            subtitle: "STUDIO SILENCER",
        },
        "builtin:comp" => UnitTheme {
            faceplate: pal().rack_comp,
            accent: pal().mute_on,
            led: pal().mute_on,
            badge: "COMP-76",
            subtitle: "FET STUDIO LIMITER",
        },
        "builtin:drive" => UnitTheme {
            faceplate: pal().rack_drive,
            accent: Color::hex(0xf5b841),
            led: pal().record_on,
            badge: "OD-808",
            subtitle: "TUBE OVERDRIVE",
        },
        "builtin:amp" => UnitTheme {
            faceplate: pal().rack_amp,
            accent: pal().rack_amp_accent,
            led: pal().record_on,
            badge: "JMP-2000",
            subtitle: "DUAL TUBE PREAMP",
        },
        "builtin:eq" => UnitTheme {
            faceplate: pal().rack_eq,
            accent: pal().accent,
            led: pal().accent,
            badge: "PEQ-500",
            subtitle: "5-BAND PARAMETRIC EQ",
        },
        "builtin:cab" => UnitTheme {
            faceplate: pal().rack_cab,
            accent: Color::hex(0xe8984f),
            led: Color::hex(0xe8984f),
            badge: "CAB-IR",
            subtitle: "STUDIO CABINET & MIC",
        },
        "builtin:chorus" => UnitTheme {
            faceplate: pal().rack_chorus,
            accent: Color::hex(0xa882ff),
            led: Color::hex(0xa882ff),
            badge: "DIM-C",
            subtitle: "TRI-STEREO CHORUS",
        },
        "builtin:delay" => UnitTheme {
            faceplate: pal().rack_delay,
            accent: pal().rack_screen_fg,
            led: pal().rack_screen_fg,
            badge: "DDL-2290",
            subtitle: "STUDIO DIGITAL DELAY",
        },
        "builtin:reverb" => UnitTheme {
            faceplate: pal().rack_reverb,
            accent: pal().monitor_on,
            led: pal().monitor_on,
            badge: "VERB-480",
            subtitle: "STUDIO REVERBERATOR",
        },
        "builtin:drums" => UnitTheme {
            faceplate: pal().rack_drums,
            accent: pal().record_on,
            led: pal().record_on,
            badge: "DK-8",
            subtitle: "SYNTHESIZED DRUM KIT",
        },
        _ => UnitTheme {
            faceplate: pal().rack_ext,
            accent: pal().accent,
            led: pal().rack_led_on,
            badge: "PLUGIN",
            subtitle: "",
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn rack_unit(
    model: &ViewModel,
    view: &ViewState,
    ui: &UiState,
    unit: &PluginDetails,
    index: usize,
    total: usize,
    bay_inner_w: f32,
) -> Node {
    let id = unit.plugin;
    let theme = unit_theme(unit);
    let led_color = if unit.error.is_some() {
        pal().record_on
    } else if unit.bypass {
        pal().rack_led_off
    } else {
        theme.led
    };
    let faceplate_w = (bay_inner_w - EAR_W * 2.0).max(460.0);

    let power = button(
        &format!("plugin/{id}/bypass"),
        "Power / Bypass",
        ui,
        (!unit.bypass).then_some(theme.accent),
    )
    .role(Role::Toggle)
    .checked(!unit.bypass)
    .text(if unit.bypass { "BYP" } else { "ON" })
    .size(34.0, 20.0)
    .style(|s| {
        s.font_size = 9.5;
        s.weight = FontWeight::SemiBold;
    });

    let led = Node::row().size(9.0, 9.0).style(move |s| {
        s.background = Some(led_color);
        s.border = Some((1.0, pal().border));
        s.radius = [4.5; 4];
    });

    let title_text = if unit.id.starts_with("builtin:") {
        theme.badge.to_owned()
    } else {
        unit.name.clone()
    };
    let sub_text = if let Some(err) = &unit.error {
        format!("ERROR: {err}")
    } else if unit.id.starts_with("builtin:") {
        theme.subtitle.to_owned()
    } else {
        let fmt = unit.id.split(':').next().unwrap_or("").to_uppercase();
        if unit.vendor.is_empty() {
            fmt
        } else {
            format!("{} · {fmt}", unit.vendor)
        }
    };

    let mut top = Node::row()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(32.0))
        .padding(Insets::xy(10.0, 0.0))
        .gap(8.0)
        .style(move |s| {
            s.background = Some(theme.faceplate.mix(pal().rack_rail, 0.2));
            s.border = Some((1.0, pal().border.with_alpha(0.5)));
        })
        .child(power)
        .child(led)
        .child(Node::label(title_text).clip().style(move |s| {
            s.weight = FontWeight::SemiBold;
            s.font_size = 12.0;
            s.color = if unit.id == "builtin:amp" {
                theme.accent
            } else {
                pal().text
            };
        }))
        .child(Node::label(sub_text).clip().style(move |s| {
            s.font_size = 10.0;
            s.color = if unit.error.is_some() {
                pal().record_on
            } else {
                pal().text_dim
            };
        }))
        .child(Node::spacer());

    if let Some(widget) = unit_header_widget(model, view, ui, unit, &theme) {
        top = top.child(widget);
    }

    let move_btn = |suffix: &str, label: &str, glyph: &str, enabled: bool| {
        button(&format!("plugin/{id}/{suffix}"), label, ui, None)
            .enabled(enabled)
            .text(glyph)
            .size(22.0, 20.0)
            .style(move |s| {
                s.font_size = 10.0;
                s.color = if enabled {
                    pal().text
                } else {
                    pal().text_dim.with_alpha(0.4)
                };
            })
    };

    top = top
        .child(move_btn("up", "Move unit up", "▲", index > 0))
        .child(move_btn("down", "Move unit down", "▼", index + 1 < total))
        .child(
            button(&format!("plugin/{id}/remove"), "Remove unit", ui, None)
                .text("✕")
                .size(22.0, 20.0)
                .style(|s| {
                    s.font_size = 10.0;
                    s.color = pal().text_dim;
                }),
        );

    let mut body = Node::column()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fill(1.0))
        .style(move |s| {
            s.background = Some(theme.faceplate);
            s.border = Some((1.0, pal().border));
        })
        .child(top);

    // 2U Tube Preamp has a woven amp grille strip above the gold knob valance.
    if unit.id == "builtin:amp" {
        body = body.child(amp_grille(unit, &theme, faceplate_w));
    }

    body = body.child(knob_row(view, ui, unit, &theme, faceplate_w));

    Node::row()
        .id(format!("rack/unit/{id}"))
        .role(Role::Group)
        .accessible_label(unit.name.clone())
        .fill()
        .style(|s| {
            s.shadow = Some((6.0, pal().shadow));
            s.radius = [4.0; 4];
        })
        .child(rack_ear())
        .child(body)
        .child(rack_ear())
}

fn amp_grille(unit: &PluginDetails, theme: &UnitTheme, faceplate_w: f32) -> Node {
    let ch = unit.params.first().map_or(0, |p| p.value.round() as usize);
    let glow_alpha = if unit.bypass {
        0.0
    } else {
        match ch {
            0 => 0.14,
            1 => 0.24,
            2 => 0.36,
            _ => 0.45,
        }
    };
    let mut grille = Node::row()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(46.0))
        .padding(Insets::xy(14.0, 6.0))
        .justify(Justify::Center)
        .align(Align::Center)
        .gap(16.0)
        .clip()
        .style(|s| {
            s.background = Some(Color::hex(0x11100e));
            s.border = Some((1.0, pal().border));
        });
    // Subtle grille ribs.
    let rib_count = ((faceplate_w / 14.0) as usize).clamp(12, 80);
    for i in 0..rib_count {
        grille = grille.child(
            Node::row()
                .background(Color::rgba(1.0, 1.0, 1.0, 0.04))
                .absolute(Rect::new(i as f32 * 14.0 + 4.0, 0.0, 2.0, 46.0)),
        );
    }
    // 4 preamp tube glow windows + brass JMP-2000 nameplate.
    for _ in 0..2 {
        grille = grille.child(tube_glow(theme.accent, glow_alpha));
    }
    grille = grille.child(
        Node::label("JMP-2000 · HIGH VOLTAGE TUBE PREAMP")
            .padding(Insets::xy(12.0, 3.0))
            .style(move |s| {
                s.font_size = 10.0;
                s.weight = FontWeight::SemiBold;
                s.color = theme.accent;
                s.background = Some(Color::hex(0x1b1712));
                s.border = Some((1.0, theme.accent.with_alpha(0.6)));
                s.radius = [2.0; 4];
            }),
    );
    for _ in 0..2 {
        grille = grille.child(tube_glow(theme.accent, glow_alpha));
    }
    grille
}

fn tube_glow(accent: Color, alpha: f32) -> Node {
    Node::row()
        .size(18.0, 26.0)
        .justify(Justify::Center)
        .align(Align::Center)
        .style(move |s| {
            s.background = Some(accent.with_alpha(alpha * 0.45));
            s.border = Some((1.0, Color::rgba(1.0, 1.0, 1.0, 0.08)));
            s.radius = [6.0; 4];
        })
        .child(Node::row().size(6.0, 14.0).style(move |s| {
            s.background = Some(Color::hex(0xff9436).with_alpha(alpha * 1.4));
            s.radius = [3.0; 4];
        }))
}

fn unit_header_widget(
    model: &ViewModel,
    view: &ViewState,
    ui: &UiState,
    unit: &PluginDetails,
    theme: &UnitTheme,
) -> Option<Node> {
    let id = unit.plugin;
    match unit.id.as_str() {
        "builtin:amp" => {
            let current = unit.params.first().map_or(2, |p| p.value.round() as usize);
            let mut row = Node::row().gap(3.0);
            for (val, label) in [(0, "CLEAN"), (1, "CRUNCH"), (2, "LEAD"), (3, "MOD")] {
                let on = current == val;
                row = row.child(
                    button(
                        &format!("plugin/{id}/choice/channel/{val}"),
                        label,
                        ui,
                        on.then_some(theme.accent),
                    )
                    .text(label)
                    .height(Sizing::Fixed(20.0))
                    .padding(Insets::xy(7.0, 0.0))
                    .style(|s| {
                        s.font_size = 9.5;
                        s.weight = FontWeight::SemiBold;
                    }),
                );
            }
            Some(row)
        }
        "builtin:eq" => Some(eq_screen(unit, theme)),
        "builtin:cab" => Some(cab_header(unit, theme, ui)),
        "builtin:delay" => Some(delay_screen(unit, theme, ui)),
        "builtin:reverb" => {
            let current = unit.params.first().map_or(1, |p| p.value.round() as usize);
            let mut row = Node::row()
                .padding(Insets::xy(4.0, 2.0))
                .gap(3.0)
                .style(|s| {
                    s.background = Some(pal().rack_screen_bg);
                    s.border = Some((1.0, pal().border));
                    s.radius = [3.0; 4];
                });
            for (val, label) in [(0, "SPRING"), (1, "PLATE"), (2, "HALL")] {
                let on = current == val;
                row = row.child(
                    button(
                        &format!("plugin/{id}/choice/mode/{val}"),
                        label,
                        ui,
                        on.then_some(theme.accent),
                    )
                    .text(label)
                    .height(Sizing::Fixed(18.0))
                    .padding(Insets::xy(6.0, 0.0))
                    .style(move |s| {
                        s.font_size = 9.0;
                        if !on {
                            s.color = pal().rack_screen_fg;
                            s.border = None;
                        }
                    }),
                );
            }
            Some(row)
        }
        "builtin:gate" | "builtin:comp" => {
            let sc_name = unit
                .sidechain
                .and_then(|t| model.tracks.iter().find(|v| v.id == t))
                .map_or_else(|| "Internal".to_owned(), |t| t.name.clone());
            Some(
                button(
                    &format!("plugin/{id}/sidechain"),
                    "Sidechain key",
                    ui,
                    unit.sidechain.map(|_| theme.accent),
                )
                .value(sc_name.clone())
                .text(format!("S/C: {sc_name}"))
                .height(Sizing::Fixed(20.0))
                .padding(Insets::xy(8.0, 0.0))
                .style(|s| s.font_size = 9.5),
            )
        }
        _ => {
            // External CLAP / LV2 / VST3 plugin controls.
            let mut row = Node::row().gap(4.0);
            if unit.error.is_some() {
                row = row.child(
                    button(&format!("plugin/{id}/reload"), "Reload plugin", ui, None)
                        .text("Reload")
                        .height(Sizing::Fixed(20.0))
                        .padding(Insets::xy(7.0, 0.0))
                        .style(|s| s.font_size = 9.5),
                );
            }
            if unit.has_gui {
                row = row.child(
                    toggle(
                        &format!("plugin/{id}/gui"),
                        "Plugin GUI",
                        "GUI",
                        unit.gui_visible,
                        theme.accent,
                        ui,
                    )
                    .height(Sizing::Fixed(20.0))
                    .padding(Insets::xy(7.0, 0.0))
                    .style(|s| s.font_size = 9.5),
                );
            }
            if unit.has_sidechain {
                let sc = unit
                    .sidechain
                    .and_then(|t| model.tracks.iter().find(|v| v.id == t))
                    .map_or_else(|| "none".to_owned(), |t| t.name.clone());
                row = row.child(
                    button(
                        &format!("plugin/{id}/sidechain"),
                        "Sidechain",
                        ui,
                        unit.sidechain.map(|_| theme.accent),
                    )
                    .text(format!("S/C: {sc}"))
                    .height(Sizing::Fixed(20.0))
                    .padding(Insets::xy(7.0, 0.0))
                    .style(|s| s.font_size = 9.5),
                );
            }
            if unit.param_count > EXT_PAGE_SIZE {
                let pages = unit.param_count.div_ceil(EXT_PAGE_SIZE);
                let page = view
                    .rack_pages
                    .get(&id)
                    .copied()
                    .unwrap_or(0)
                    .min(pages - 1);
                row = row
                    .child(
                        button(&format!("plugin/{id}/page_prev"), "Previous page", ui, None)
                            .enabled(page > 0)
                            .text("<")
                            .size(20.0, 20.0)
                            .style(|s| s.font_size = 9.5),
                    )
                    .child(Node::label(format!("{}/{pages}", page + 1)).style(|s| {
                        s.font_size = 9.5;
                        s.family = FontFamily::Mono;
                        s.color = pal().text_dim;
                    }))
                    .child(
                        button(&format!("plugin/{id}/page_next"), "Next page", ui, None)
                            .enabled(page + 1 < pages)
                            .text(">")
                            .size(20.0, 20.0)
                            .style(|s| s.font_size = 9.5),
                    );
            }
            row = row.child(
                button(&format!("plugin/{id}/edit"), "All parameters", ui, None)
                    .text("EDIT")
                    .height(Sizing::Fixed(20.0))
                    .padding(Insets::xy(7.0, 0.0))
                    .style(|s| s.font_size = 9.5),
            );
            Some(row)
        }
    }
}

fn eq_screen(unit: &PluginDetails, theme: &UnitTheme) -> Node {
    let p = |i: usize, def: f32| unit.params.get(i).map_or(def, |v| v.value as f32);
    let (w, h) = (132.0_f32, 24.0_f32);
    let points: Vec<Point> = (0..=28)
        .map(|i| {
            let t = i as f32 / 28.0;
            let hz = 30.0 * (400.0_f32).powf(t);
            let db = dawkey_studio::eq_response_db(
                p(0, 60.0),
                p(1, 0.0),
                p(2, 0.0),
                p(3, 850.0),
                p(4, 0.0),
                p(5, 1.2),
                p(6, 0.0),
                p(7, 0.0),
                p(8, 0.0),
                hz,
            );
            let y = h * 0.5 - (db / 18.0).clamp(-0.9, 0.9) * (h * 0.45);
            Point::new(t * w, y)
        })
        .collect();
    Node::row()
        .size(w, h)
        .style(|s| {
            s.background = Some(pal().rack_screen_bg);
            s.border = Some((1.0, pal().border));
            s.radius = [3.0; 4];
        })
        .paint(Paint::Polyline {
            points: points.into(),
            width: 1.5,
            color: theme.accent,
        })
}

fn cab_header(unit: &PluginDetails, theme: &UnitTheme, ui: &UiState) -> Node {
    let p = |i: usize, def: f32| unit.params.get(i).map_or(def, |v| v.value as f32);
    let cab_idx = p(0, 2.0).round() as usize;
    let mic_idx = p(1, 0.0).round() as usize;
    let id = unit.plugin;
    let next_cab = (cab_idx + 1) % 4;
    let next_mic = (mic_idx + 1) % 3;
    let cab_text = unit
        .params
        .first()
        .map_or_else(|| "4x12 Vintage".to_owned(), |v| v.text.clone());
    let mic_text = unit
        .params
        .get(1)
        .map_or_else(|| "Dyn 57".to_owned(), |v| v.text.clone());

    let (w, h) = (96.0_f32, 22.0_f32);
    let points: Vec<Point> = (0..=22)
        .map(|i| {
            let t = i as f32 / 22.0;
            let hz = 40.0 * (300.0_f32).powf(t);
            let db = dawkey_studio::cab_response_db(
                cab_idx,
                mic_idx,
                p(2, 25.0),
                p(3, 75.0),
                p(4, 6500.0),
                hz,
            );
            let y = h * 0.55 - (db / 24.0).clamp(-0.9, 0.85) * (h * 0.45);
            Point::new(t * w, y)
        })
        .collect();

    Node::row()
        .gap(4.0)
        .child(
            button(
                &format!("plugin/{id}/choice/cabinet/{next_cab}"),
                "Cycle cabinet",
                ui,
                None,
            )
            .text(cab_text)
            .height(Sizing::Fixed(20.0))
            .padding(Insets::xy(7.0, 0.0))
            .style(|s| s.font_size = 9.5),
        )
        .child(
            button(
                &format!("plugin/{id}/choice/mic/{next_mic}"),
                "Cycle mic",
                ui,
                None,
            )
            .text(mic_text)
            .height(Sizing::Fixed(20.0))
            .padding(Insets::xy(7.0, 0.0))
            .style(|s| s.font_size = 9.5),
        )
        .child(
            Node::row()
                .size(w, h)
                .style(|s| {
                    s.background = Some(pal().rack_screen_bg);
                    s.border = Some((1.0, pal().border));
                    s.radius = [3.0; 4];
                })
                .paint(Paint::Polyline {
                    points: points.into(),
                    width: 1.5,
                    color: theme.accent,
                }),
        )
}

fn delay_screen(unit: &PluginDetails, theme: &UnitTheme, ui: &UiState) -> Node {
    let id = unit.plugin;
    let time_text = unit
        .params
        .first()
        .map_or_else(|| "375 ms".to_owned(), |v| v.text.clone());
    let sync_idx = unit.params.get(1).map_or(0, |v| v.value.round() as usize);
    let sync_text = unit
        .params
        .get(1)
        .map_or_else(|| "Free".to_owned(), |v| v.text.clone());
    let next_sync = (sync_idx + 1) % 7;
    let display = if sync_idx == 0 {
        time_text
    } else {
        format!("SYNC {sync_text}")
    };
    Node::row()
        .gap(4.0)
        .child(
            Node::label(display)
                .padding(Insets::xy(10.0, 2.0))
                .height(Sizing::Fixed(22.0))
                .style(move |s| {
                    s.family = FontFamily::Mono;
                    s.font_size = 11.0;
                    s.weight = FontWeight::SemiBold;
                    s.color = theme.accent;
                    s.background = Some(pal().rack_screen_bg);
                    s.border = Some((1.0, pal().border));
                    s.radius = [3.0; 4];
                }),
        )
        .child(
            button(
                &format!("plugin/{id}/choice/sync/{next_sync}"),
                "Cycle delay sync",
                ui,
                (sync_idx > 0).then_some(theme.accent),
            )
            .text(format!("Sync: {sync_text}"))
            .height(Sizing::Fixed(20.0))
            .padding(Insets::xy(7.0, 0.0))
            .style(|s| s.font_size = 9.5),
        )
}

fn knob_row(
    view: &ViewState,
    ui: &UiState,
    unit: &PluginDetails,
    theme: &UnitTheme,
    faceplate_w: f32,
) -> Node {
    let id = unit.plugin;
    // builtin:drums has 24 (8 pads x 3), far more than the rack units other
    // builtins have; it needs the same paging an external plugin's params
    // get, not the single unpaged row every other builtin's handful fits.
    let params: &[ParamState] = if unit.id.starts_with("builtin:") && unit.id != "builtin:drums" {
        &unit.params
    } else {
        let pages = unit.param_count.div_ceil(EXT_PAGE_SIZE).max(1);
        let page = view
            .rack_pages
            .get(&id)
            .copied()
            .unwrap_or(0)
            .min(pages - 1);
        let start = (page * EXT_PAGE_SIZE).min(unit.params.len());
        let end = (start + EXT_PAGE_SIZE).min(unit.params.len());
        &unit.params[start..end]
    };

    if params.is_empty() {
        return Node::row()
            .fill()
            .justify(Justify::Center)
            .align(Align::Center)
            .child(Node::label("No adjustable parameters").style(|s| {
                s.font_size = 11.0;
                s.color = pal().text_dim;
            }));
    }

    let pod_w = ((faceplate_w - 24.0) / params.len().max(1) as f32).clamp(58.0, 96.0);
    let mut row = Node::row()
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fill(1.0))
        .padding(Insets::xy(10.0, 6.0))
        .gap(4.0)
        .justify(Justify::Center)
        .align(Align::Center);

    for p in params {
        row = row.child(knob_pod(view, ui, id, p, theme, pod_w));
    }
    row
}

fn knob_pod(
    view: &ViewState,
    ui: &UiState,
    plugin: dawkey_core::PluginId,
    p: &ParamState,
    theme: &UnitTheme,
    pod_w: f32,
) -> Node {
    let param_id = format!("plugin/{plugin}/param/{}", p.id);
    let span = p.max - p.min;
    let unit_val = if span > 0.0 {
        ((p.value - p.min) / span).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let unit_def = if span > 0.0 {
        ((p.default - p.min) / span).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let bipolar = p.min < 0.0 && p.max > 0.0;
    let hovered = ui.is_hovered(&param_id) || ui.is_pressed(&param_id);
    let rim = if hovered {
        theme.accent
    } else {
        pal().knob_rim
    };
    let typing = view.text_edit.as_ref().filter(|e| {
        matches!(&e.field, TextField::PluginParam { plugin: pl, param }
            if *pl == plugin && param == &p.id)
    });

    let readout = match typing {
        Some(edit) => text_field(
            &param_id,
            &p.name,
            edit.text.clone(),
            Some(edit.selected),
            ui,
        )
        .width(Sizing::Fixed((pod_w - 10.0).max(46.0)))
        .height(Sizing::Fixed(16.0)),
        None => Node::row()
            .gap(3.0)
            .align(Align::Center)
            .child(
                Node::label(p.text.clone())
                    .clip()
                    .width(Sizing::Fixed((pod_w - 22.0).max(34.0)))
                    .style(|s| {
                        s.font_size = 9.5;
                        s.family = FontFamily::Mono;
                        s.color = pal().text;
                    }),
            )
            .child(
                toggle(
                    &format!("{param_id}/automate"),
                    "Automate parameter",
                    "A",
                    p.automated,
                    theme.accent,
                    ui,
                )
                .size(16.0, 14.0)
                .padding(Insets::default())
                .style(|s| s.font_size = 8.5),
            ),
    };

    let knob_id = if typing.is_some() {
        format!("{param_id}/knob")
    } else {
        param_id
    };

    Node::column()
        .width(Sizing::Fixed(pod_w))
        .height(Sizing::Fill(1.0))
        .justify(Justify::Center)
        .align(Align::Center)
        .gap(2.0)
        .child(
            Node::label(p.name.to_uppercase())
                .width(Sizing::Fill(1.0))
                .clip()
                .style(|s| {
                    s.font_size = 9.0;
                    s.weight = FontWeight::SemiBold;
                    s.color = pal().text_dim;
                }),
        )
        .child(
            Node::row()
                .id(knob_id)
                .role(Role::Slider)
                .accessible_label(p.name.clone())
                .value(p.text.clone())
                .draggable()
                .clickable()
                .size(38.0, 38.0)
                .paint(Paint::Knob {
                    value: unit_val,
                    default: unit_def,
                    bipolar,
                    accent: theme.accent,
                    body: pal().knob_body,
                    rim,
                    tick: pal().knob_tick,
                }),
        )
        .child(readout)
}
