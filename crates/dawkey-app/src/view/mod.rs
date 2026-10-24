// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dawkey's main window, built as a dawkey-ui tree every frame.
//!
//! Widget IDs are part of the testing interface (MCP `ui_*` tools and tests
//! use them), so keep them stable: `window/*`, `transport/*`, `view/*`,
//! `toolbar/*`, `arrange/*`, `timeline/*`, `tracks/empty`, `track/<id>/*`,
//! `clip/<id>`, `clip/<id>/{start,end}`, `track/<id>/{monitor,takes}`,
//! `track/<id>/take/<n>` (and `.../lane`), `take/<id>`, `master/*`, `status/*`,
//! `confirm/{save,discard,cancel,backdrop}`, and in the piano roll
//! `piano_roll/*`, `note/<id>`, `note/<id>/end`, `note/<id>/velocity`,
//! `track/<id>/synth/*` and
//! `track/<id>/{midi_in,midi_in_channel,midi_out,midi_out_latency}`, and in
//! the tempo lanes `tempo/*`, `tempo/<n>`, `meter/*` and `meter/<n>`.

mod arrange;
mod confirm;
mod export;
mod mixer;
mod piano_roll;
pub(crate) mod plugins;
mod prefs;
mod recovery;
pub(crate) mod tempo;

use std::sync::Arc;

use dawkey_core::{
    AutomationTarget, ClipId, Controller, MeterMap, NoteId, PluginId, SynthParams, TempoMap,
    TrackId, TrackKind,
};
use dawkey_media::Peaks;
use dawkey_studio::PluginDetails;
use dawkey_ui::{
    Chrome, Color, FontFamily, FontWeight, Icon, Insets, Node, Paint, Role, Sizing, UiState,
};

pub(crate) use mixer::{db_from_position, position_from_db};
pub(crate) use piano_roll::{edited, lane_value, marquee_notes};
#[cfg(test)]
pub(crate) use piano_roll::{x_of as roll_x, y_of as roll_y};

use crate::WindowInfo;
use crate::state::{Page, TextField, ViewState};
use crate::theme::*;

/// Everything the view shows, gathered from the studio.
#[derive(Debug, Clone)]
pub(crate) struct ViewModel {
    pub(crate) title: String,
    pub(crate) dirty: bool,
    pub(crate) path: Option<String>,
    pub(crate) playing: bool,
    pub(crate) recording: bool,
    /// Where the current recording started.
    pub(crate) record_start_s: Option<f64>,
    pub(crate) position_s: f64,
    pub(crate) sample_rate: u32,
    /// Loop and punch ranges in seconds, and whether they're on.
    pub(crate) loop_range: Option<(f64, f64)>,
    pub(crate) loop_enabled: bool,
    pub(crate) punch_range: Option<(f64, f64)>,
    pub(crate) punch_enabled: bool,
    /// Tempo at the playhead.
    pub(crate) tempo_bpm: f64,
    pub(crate) tempo: TempoMap,
    /// Where bars start.
    pub(crate) meter: MeterMap,
    /// Time signature at the playhead.
    pub(crate) time_signature: (u8, u8),
    pub(crate) tracks: Vec<TrackView>,
    /// The master bus (track 0).
    pub(crate) master_track: TrackView,
    pub(crate) master: [f32; 2],
    pub(crate) engine: String,
    pub(crate) message: Option<String>,
    /// The plugin in the editor, if open.
    pub(crate) editor: Option<PluginDetails>,
    /// The plugins the picker offers, if open.
    pub(crate) picker: Option<Vec<PickerEntry>>,
    /// Plugins are being scanned on a worker thread.
    pub(crate) scanning: bool,
    /// Moving controls while playing writes automation.
    pub(crate) automation_write: bool,
    /// Where the last clip that plays ends, in seconds.
    pub(crate) song_end_s: f64,
    /// The export running: its file's name, and how far it is (0 to 1).
    pub(crate) exporting: Option<(String, f64)>,
    pub(crate) prefs: crate::prefs::Preferences,
    /// The HTTP MCP server's state, and where its token is kept.
    pub(crate) mcp_http: crate::McpHttpState,
    pub(crate) mcp_token_path: Option<String>,
}

/// A plugin the picker offers.
#[derive(Debug, Clone)]
pub(crate) struct PickerEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) vendor: String,
    pub(crate) instrument: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct TrackView {
    pub(crate) id: TrackId,
    pub(crate) name: String,
    pub(crate) kind: TrackKind,
    /// The bus it goes to (`None`: the master), and that bus's name.
    pub(crate) output: Option<TrackId>,
    pub(crate) output_name: String,
    pub(crate) gain_db: f32,
    pub(crate) pan: f32,
    pub(crate) mute: bool,
    pub(crate) solo: bool,
    pub(crate) armed: bool,
    /// Audio tracks: plays its input while armed.
    pub(crate) monitor: bool,
    /// Take lanes its clips use (the highest lane plus one).
    pub(crate) take_lanes: usize,
    /// MIDI tracks: the synth's settings.
    pub(crate) synth: Option<SynthParams>,
    /// MIDI tracks: the device listened to (`None`: all).
    pub(crate) midi_input: Option<String>,
    /// MIDI tracks: the channel listened to, 1 to 16 (`None`: all).
    pub(crate) midi_input_channel: Option<u8>,
    /// MIDI tracks: the device played instead of the instrument.
    pub(crate) midi_output: Option<String>,
    /// The output device's latency in ms.
    pub(crate) midi_output_latency_ms: f64,
    /// What the audio system reports for the output device, in ms, added
    /// to it.
    pub(crate) midi_output_reported_ms: f64,
    pub(crate) plugins: Vec<SlotView>,
    pub(crate) automation: Vec<LaneView>,
    /// Linear peak levels.
    pub(crate) level: [f32; 2],
    pub(crate) clips: Vec<ClipView>,
}

#[derive(Debug, Clone)]
pub(crate) struct ClipView {
    pub(crate) id: ClipId,
    pub(crate) name: String,
    pub(crate) start_s: f64,
    pub(crate) length_s: f64,
    pub(crate) muted: bool,
    /// Take lane.
    pub(crate) lane: usize,
    pub(crate) content: ClipViewContent,
}

#[derive(Debug, Clone)]
pub(crate) enum ClipViewContent {
    Audio {
        /// First source frame played.
        offset: u64,
        peaks: Option<Arc<Peaks>>,
    },
    Midi {
        notes: Arc<[NoteView]>,
        controls: Arc<[ControlView]>,
    },
}

/// A controller change in a MIDI clip, in beats from its start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ControlView {
    pub(crate) controller: Controller,
    pub(crate) beat: f64,
    pub(crate) value: u16,
}

/// A plugin slot on a track.
#[derive(Debug, Clone)]
pub(crate) struct SlotView {
    pub(crate) id: PluginId,
    pub(crate) name: String,
    pub(crate) bypass: bool,
    /// Couldn't be loaded.
    pub(crate) error: bool,
}

/// An automation lane.
#[derive(Debug, Clone)]
pub(crate) struct LaneView {
    pub(crate) label: String,
    pub(crate) kind: LaneKind,
    /// (time in seconds, value), sorted.
    pub(crate) points: Vec<(f64, f64)>,
}

/// How a lane's values map to its height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LaneKind {
    /// dB, on the fader's scale.
    Gain,
    Pan,
    Param {
        min: f64,
        max: f64,
        stepped: bool,
    },
}

impl LaneKind {
    /// 0 (bottom) to 1 (top) for a value.
    pub(crate) fn unit(self, value: f64) -> f32 {
        match self {
            LaneKind::Gain => position_from_db(value as f32),
            LaneKind::Pan => ((value + 1.0) / 2.0) as f32,
            LaneKind::Param { min, max, .. } if max > min => ((value - min) / (max - min)) as f32,
            LaneKind::Param { .. } => 0.5,
        }
        .clamp(0.0, 1.0)
    }

    /// The value at a height (0 bottom, 1 top).
    pub(crate) fn value(self, unit: f32) -> f64 {
        let unit = unit.clamp(0.0, 1.0);
        match self {
            LaneKind::Gain => f64::from(db_from_position(unit)),
            LaneKind::Pan => ((f64::from(unit) * 2.0 - 1.0) * 100.0).round() / 100.0,
            LaneKind::Param { min, max, stepped } => {
                let v = min + f64::from(unit) * (max - min);
                if stepped { v.round() } else { v }
            }
        }
    }

    pub(crate) fn text(self, value: f64) -> String {
        match self {
            LaneKind::Gain => format!("{value:+.1} dB"),
            LaneKind::Pan if value.abs() < 0.005 => "C".to_owned(),
            LaneKind::Pan if value < 0.0 => format!("L{:.0}", -value * 100.0),
            LaneKind::Pan => format!("R{:.0}", value * 100.0),
            LaneKind::Param { .. } => format_param(value),
        }
    }
}

/// What a clip is called: its own name, else its audio file's name (or,
/// for MIDI clips, its track's).
pub(crate) fn clip_label(
    project: &dawkey_core::Project,
    track: &dawkey_core::Track,
    clip: &dawkey_core::Clip,
) -> String {
    if let Some(name) = &clip.name {
        return name.clone();
    }
    match clip.content {
        dawkey_core::ClipContent::Audio { source, .. } => project
            .source(source)
            .map_or_else(|| "Missing".to_owned(), |s| s.name.clone()),
        dawkey_core::ClipContent::Midi { .. } => track.name.clone(),
    }
}

/// What an automation lane on `track` is called and how its values scale.
pub(crate) fn lane_label_kind(
    track: &dawkey_core::Track,
    target: &AutomationTarget,
    catalog: Option<&dawkey_studio::PluginCatalog>,
) -> (String, LaneKind) {
    match target {
        AutomationTarget::Gain => ("Gain".to_owned(), LaneKind::Gain),
        AutomationTarget::Pan => ("Pan".to_owned(), LaneKind::Pan),
        AutomationTarget::Param { plugin, param } => {
            let info = track
                .plugin(*plugin)
                .and_then(|s| catalog.and_then(|c| c.get(&s.plugin)));
            let p = info.and_then(|i| i.params.iter().find(|p| &p.id == param));
            match (info, p) {
                (Some(i), Some(p)) => (
                    format!("{}: {}", i.name, p.name),
                    LaneKind::Param {
                        min: p.min,
                        max: p.max,
                        stepped: p.stepped,
                    },
                ),
                _ => (
                    format!("Plugin {plugin}: {param}"),
                    LaneKind::Param {
                        min: 0.0,
                        max: 1.0,
                        stepped: false,
                    },
                ),
            }
        }
    }
}

/// A parameter value for display.
pub(crate) fn format_param(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e6 {
        format!("{value:.0}")
    } else if value.abs() >= 100.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

/// A MIDI clip's notes for the view.
pub(crate) fn note_views(
    project: &dawkey_core::Project,
    clip: &dawkey_core::Clip,
) -> Vec<NoteView> {
    let tempo = project.tempo_map();
    clip.notes()
        .iter()
        .map(|n| {
            let start_beats = tempo.clip_beats(clip.start, n.start);
            NoteView {
                id: n.id,
                pitch: n.pitch,
                velocity: n.velocity,
                start_beats,
                length_beats: tempo.clip_beats(clip.start, n.end()) - start_beats,
                start_s: project.frames_to_seconds(n.start),
                length_s: project.frames_to_seconds(n.length),
            }
        })
        .collect()
}

/// A MIDI note, timed in beats (quarter notes) and in seconds from its
/// clip's start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct NoteView {
    pub(crate) id: NoteId,
    pub(crate) pitch: u8,
    pub(crate) velocity: u8,
    pub(crate) start_beats: f64,
    pub(crate) length_beats: f64,
    pub(crate) start_s: f64,
    pub(crate) length_s: f64,
}

pub(crate) fn build(
    model: &ViewModel,
    view: &ViewState,
    ui: &UiState,
    window: &WindowInfo,
) -> Node {
    let margin = window.shadow_margin();
    let radius = window.corner_radius();
    let mut frame = Node::column()
        .id("window")
        .role(Role::Window)
        .accessible_label(window_title(&model.title, model.dirty))
        .fill()
        .clip()
        .style(|s| {
            s.background = Some(pal().window);
            s.radius = [radius; 4];
            if margin > 0.0 {
                s.shadow = Some((SHADOW_BLUR, pal().shadow));
                s.border = Some((1.0, pal().border));
            }
        });
    if window.csd {
        frame = frame.child(titlebar(model, ui, window, radius));
    }
    let page = match view.page {
        Page::Arrange => arrange::build(model, view, ui),
        Page::Mixer => mixer::build(model, view, ui),
        Page::PianoRoll => piano_roll::build(model, view, ui),
    };
    // The plugin picker and editor float over the page.
    let page =
        Node::column()
            .fill()
            .child(page)
            .children(plugins::overlay(model, view, ui, window.size));
    // The "save changes?" dialog covers everything below the title bar.
    let top = if window.csd { TITLEBAR_HEIGHT } else { 0.0 };
    let area = dawkey_ui::Rect::new(
        0.0,
        top,
        window.size.w - 2.0 * margin,
        window.size.h - 2.0 * margin - top,
    );
    let frame = frame
        .child(toolbar(model, view, ui, window.size.w))
        .child(page)
        .child(statusbar(model, ui, [0.0, 0.0, radius, radius]))
        .children(export::dialog(model, view, ui, area, radius))
        .children(prefs::panel(model, view, ui, area, radius))
        .children(recovery::dialog(view, ui, area, radius))
        .children(confirm::dialog(model, view, ui, area, radius));
    Node::column()
        .fill()
        .padding(Insets::all(margin))
        .child(frame)
}

pub(crate) fn window_title(name: &str, dirty: bool) -> String {
    format!("{}{name} — Dawkey", if dirty { "• " } else { "" })
}

fn titlebar(model: &ViewModel, ui: &UiState, window: &WindowInfo, radius: f32) -> Node {
    let active = window.activated;
    let fg = if active { pal().text } else { pal().text_dim };
    let maximize_icon = if window.maximized {
        Icon::Restore
    } else {
        Icon::Maximize
    };
    Node::row()
        .id("window/titlebar")
        .role(Role::TitleBar)
        .chrome(Chrome::DragArea)
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(TITLEBAR_HEIGHT))
        .padding(Insets {
            left: 14.0,
            ..Insets::default()
        })
        .gap(10.0)
        .style(|s| {
            s.background = Some(if active {
                pal().titlebar
            } else {
                pal().titlebar_inactive
            });
            s.radius = [radius, radius, 0.0, 0.0];
        })
        .child(Node::row().size(10.0, 10.0).style(|s| {
            s.background = Some(pal().accent);
            s.radius = [5.0; 4];
        }))
        .child(
            Node::label(window_title(&model.title, model.dirty))
                .id("window/title")
                .style(|s| {
                    s.color = fg;
                    s.weight = FontWeight::SemiBold;
                }),
        )
        .child(Node::spacer())
        .child(window_button(
            "window/minimize",
            "Minimize",
            Icon::Minimize,
            Chrome::Minimize,
            ui,
            fg,
            0.0,
        ))
        .child(window_button(
            "window/maximize",
            if window.maximized {
                "Restore"
            } else {
                "Maximize"
            },
            maximize_icon,
            Chrome::ToggleMaximize,
            ui,
            fg,
            0.0,
        ))
        .child(window_button(
            "window/close",
            "Close",
            Icon::Close,
            Chrome::Close,
            ui,
            fg,
            radius,
        ))
}

fn window_button(
    id: &str,
    label: &str,
    icon: Icon,
    chrome: Chrome,
    ui: &UiState,
    fg: Color,
    top_right_radius: f32,
) -> Node {
    let close = chrome == Chrome::Close;
    let (bg, color) = if ui.is_pressed(id) {
        (
            Some(if close {
                pal().close_hover.mix(Color::hex(0), 0.2)
            } else {
                pal().pressed
            }),
            pal().text,
        )
    } else if ui.is_hovered(id) {
        (
            Some(if close {
                pal().close_hover
            } else {
                pal().hover
            }),
            pal().text,
        )
    } else {
        (None, fg)
    };
    Node::row()
        .id(id)
        .role(Role::Button)
        .accessible_label(label)
        .chrome(chrome)
        .size(46.0, TITLEBAR_HEIGHT)
        .paint(Paint::Icon(icon))
        .style(|s| {
            s.background = bg;
            s.color = color;
            s.radius = [0.0, top_right_radius, 0.0, 0.0];
        })
}

/// A small button or toggle with hover and pressed states. `active` gives
/// the "on" color.
pub(crate) fn button(id: &str, label: &str, ui: &UiState, active: Option<Color>) -> Node {
    let bg = match active {
        Some(on) if ui.is_pressed(id) => on.mix(Color::hex(0), 0.2),
        Some(on) => on,
        None if ui.is_pressed(id) => pal().pressed,
        None if ui.is_hovered(id) => pal().hover,
        None => Color::TRANSPARENT,
    };
    let fg = if active.is_some() {
        pal().on_text
    } else {
        pal().text
    };
    let border = if active.is_some() { bg } else { pal().border };
    Node::row()
        .id(id)
        .accessible_label(label)
        .role(Role::Button)
        .clickable()
        .style(|s| {
            s.background = Some(bg);
            s.border = Some((1.0, border));
            s.radius = [5.0; 4];
            s.color = fg;
        })
}

/// A field to click and type in, showing `text`. While it's being typed
/// in, `editing` says whether the text is still all selected (typing
/// replaces it); otherwise a caret follows it.
pub(crate) fn text_field(
    id: &str,
    label: &str,
    text: String,
    editing: Option<bool>,
    ui: &UiState,
) -> Node {
    let border = match editing {
        Some(_) => pal().accent,
        None if ui.is_hovered(id) => pal().border.mix(pal().text_dim, 0.5),
        None => pal().border,
    };
    let selected = editing == Some(true);
    Node::row()
        .id(id)
        .role(Role::TextField)
        .accessible_label(label)
        .value(text.clone())
        .clickable()
        .padding(Insets::xy(3.0, 0.0))
        .height(Sizing::Fixed(18.0))
        .style(|s| {
            s.border = Some((1.0, border));
            s.radius = [3.0; 4];
            s.background = editing.is_some().then_some(pal().panel);
        })
        .child(Node::label(text).style(|s| {
            s.font_size = 11.0;
            s.family = FontFamily::Mono;
            s.color = pal().text;
            if selected {
                s.background = Some(pal().accent.with_alpha(0.45));
            }
        }))
        .children(
            (editing == Some(false)).then(|| Node::row().size(1.0, 12.0).background(pal().text)),
        )
}

/// The field `field` (a track's or clip's name) is typed in, while it is.
pub(crate) fn name_editor(
    field: &TextField,
    label: &str,
    view: &ViewState,
    ui: &UiState,
) -> Option<Node> {
    let edit = view.text_edit.as_ref().filter(|e| e.field == *field)?;
    let id = crate::text_input::field_id(field);
    Some(text_field(
        &id,
        label,
        edit.text.clone(),
        Some(edit.selected),
        ui,
    ))
}

/// A labelled toggle button.
pub(crate) fn toggle(
    id: &str,
    label: &str,
    text: &str,
    on: bool,
    color: Color,
    ui: &UiState,
) -> Node {
    button(id, label, ui, on.then_some(color))
        .role(Role::Toggle)
        .checked(on)
        .text(text)
        .padding(Insets::xy(10.0, 0.0))
        .height(Sizing::Fixed(28.0))
        .style(|s| s.weight = FontWeight::SemiBold)
}

/// The M and S buttons of a track.
/// The M, S and R (record arm) buttons of a track.
/// Mute, solo and record arm (buses don't record; the master only mutes).
pub(crate) fn track_buttons(track: &TrackView, ui: &UiState) -> Vec<Node> {
    let id = track.id;
    let small = |name: &str, label: &str, on: bool, color: Color| {
        button(
            &format!("track/{id}/{name}"),
            label,
            ui,
            on.then_some(color),
        )
        .role(Role::Toggle)
        .checked(on)
        .size(28.0, 24.0)
        .text(label[..1].to_owned())
        .style(|s| s.weight = FontWeight::SemiBold)
    };
    let mut buttons = vec![small("mute", "Mute", track.mute, pal().mute_on)];
    if id != dawkey_core::MASTER {
        buttons.push(small("solo", "Solo", track.solo, pal().solo_on));
    }
    if track.kind != TrackKind::Bus {
        buttons.push(small("arm", "Record arm", track.armed, pal().record_on));
    }
    buttons
}

/// The narrowest windows whose toolbar shows the Split button, without and
/// with the piano roll toggle.
const SPLIT_MIN_W: f32 = 940.0;
const SPLIT_MIN_W_EDIT: f32 = 1040.0;
/// The narrowest windows whose toolbar shows "Auto" outside the mixer,
/// without and with the piano roll toggle.
const AUTO_MIN_W: f32 = 1000.0;
const AUTO_MIN_W_EDIT: f32 = 1140.0;

/// The narrowest windows whose toolbar shows "Export…".
const EXPORT_MIN_W: f32 = 1100.0;

fn toolbar(model: &ViewModel, view: &ViewState, ui: &UiState, window_w: f32) -> Node {
    // The master meter takes what room the toolbar has left (up to 140 px)
    // and disappears in narrow windows rather than overflowing.
    let meter_w = ((window_w - 1000.0) / 2.0).clamp(0.0, 140.0);
    // The piano roll toggle shows when there's a clip for it: the edited
    // one, or the selected one if it's MIDI. In narrow windows it takes the
    // place of the tempo and time signature.
    let can_edit = edited(model, view).is_some()
        || model.tracks.iter().any(|t| {
            t.clips.iter().any(|c| {
                Some(c.id) == view.selected_clip
                    && matches!(c.content, ClipViewContent::Midi { .. })
            })
        });
    // So does the automation mode, while "Auto" is on (in the mixer, or in
    // windows wide enough for it).
    let show_mode = model.automation_write
        && (view.page == Page::Mixer
            || window_w
                >= if can_edit {
                    AUTO_MIN_W_EDIT
                } else {
                    AUTO_MIN_W
                });
    let show_tempo = !(can_edit || show_mode) || window_w >= 980.0;
    // Splitting the selected clip, when there's room for its button.
    let show_split = view.page == Page::Arrange
        && view.selected_clip.is_some()
        && window_w
            >= if can_edit {
                SPLIT_MIN_W_EDIT
            } else {
                SPLIT_MIN_W
            };
    // Writing automation (faders, pan, plugin parameters) is the mixer's;
    // there, in narrow windows, it takes the place of snapping, which the
    // mixer has no use for. Elsewhere it shows when there's room.
    let wide = window_w
        >= if can_edit {
            AUTO_MIN_W_EDIT
        } else {
            AUTO_MIN_W
        };
    let show_auto = view.page == Page::Mixer || wide;
    // "Export…" needs more room with the Split button and the piano roll
    // toggle.
    let export_min_w =
        EXPORT_MIN_W + if show_split { 80.0 } else { 0.0 } + if can_edit { 130.0 } else { 0.0 };
    let show_snap = view.page != Page::Mixer || wide;
    let play_active = model.playing.then_some(pal().accent);
    Node::row()
        .id("toolbar")
        .role(Role::Toolbar)
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(52.0))
        .padding(Insets::xy(12.0, 0.0))
        .gap(8.0)
        .background(pal().toolbar)
        .child(
            button("transport/play", "Play", ui, play_active)
                .role(Role::Toggle)
                .checked(model.playing)
                .size(38.0, 32.0)
                .paint(Paint::Icon(Icon::Play)),
        )
        .child(
            button("transport/stop", "Stop", ui, None)
                .size(38.0, 32.0)
                .paint(Paint::Icon(Icon::Stop)),
        )
        .child(
            button(
                "transport/record",
                "Record",
                ui,
                model.recording.then_some(pal().record_on),
            )
            .role(Role::Toggle)
            .checked(model.recording)
            .size(38.0, 32.0)
            .paint(Paint::Icon(Icon::Record))
            .style(|s| {
                if !model.recording {
                    s.color = pal().record_on;
                }
            }),
        )
        .child(
            Node::label(format_time(model.position_s))
                .id("transport/position")
                .accessible_label("Position")
                .value(format_time(model.position_s))
                .padding(Insets::xy(10.0, 0.0))
                .style(|s| {
                    s.family = FontFamily::Mono;
                    s.font_size = 18.0;
                    s.color = pal().text;
                }),
        )
        // Clicking either shows the tempo and time signature lanes.
        .children(show_tempo.then(|| {
            Node::label(format!("{:.1} BPM", model.tempo_bpm))
                .id("transport/tempo")
                .role(Role::Button)
                .accessible_label("Tempo")
                .clickable()
                .style(|s| {
                    s.color = if ui.is_hovered("transport/tempo") {
                        pal().text
                    } else {
                        pal().text_dim
                    }
                })
        }))
        .children(show_tempo.then(|| {
            Node::label(format!(
                "{}/{}",
                model.time_signature.0, model.time_signature.1
            ))
            .id("transport/time_signature")
            .role(Role::Button)
            .accessible_label("Time signature")
            .clickable()
            .style(|s| {
                s.color = if ui.is_hovered("transport/time_signature") {
                    pal().text
                } else {
                    pal().text_dim
                }
            })
        }))
        .child(Node::row().width(Sizing::Fixed(6.0)))
        .child(toggle(
            "transport/loop",
            "Loop",
            "Loop",
            model.loop_enabled,
            pal().accent,
            ui,
        ))
        .child(toggle(
            "transport/punch",
            "Punch",
            "Punch",
            model.punch_enabled,
            pal().record_on,
            ui,
        ))
        .children(show_auto.then(|| {
            toggle(
                "transport/automation",
                "Write automation",
                "Auto",
                model.automation_write,
                pal().record_on,
                ui,
            )
        }))
        // How it writes, while it's on: each click picks the next mode.
        .children(show_mode.then(|| {
            let mode = view.automation_mode.label();
            button("transport/automation_mode", "Automation mode", ui, None)
                .value(mode.to_lowercase())
                .text(mode)
                .padding(Insets::xy(8.0, 0.0))
                .height(Sizing::Fixed(28.0))
        }))
        .child(Node::row().width(Sizing::Fixed(12.0)))
        .child(toggle(
            "view/arrange",
            "Arrange",
            "Arrange",
            view.page == Page::Arrange,
            pal().accent,
            ui,
        ))
        .child(toggle(
            "view/mixer",
            "Mixer",
            "Mixer",
            view.page == Page::Mixer,
            pal().accent,
            ui,
        ))
        .children(can_edit.then(|| {
            toggle(
                "view/piano_roll",
                "Piano roll",
                "Piano roll",
                view.page == Page::PianoRoll,
                pal().accent,
                ui,
            )
        }))
        .child(Node::spacer())
        .children(show_snap.then(|| {
            toggle(
                "toolbar/snap",
                "Snap to beats",
                "Snap",
                view.snap,
                pal().text_dim,
                ui,
            )
        }))
        // Splits the selected clip at the playhead (S).
        .children(show_split.then(|| {
            button("toolbar/split", "Split clip at playhead", ui, None)
                .text("Split")
                .padding(Insets::xy(12.0, 0.0))
                .height(Sizing::Fixed(28.0))
        }))
        .child(
            button("toolbar/import", "Import audio", ui, None)
                .text("Import…")
                .padding(Insets::xy(12.0, 0.0))
                .height(Sizing::Fixed(28.0)),
        )
        // Exporting the mix, when there's room (Ctrl+E otherwise).
        .children((window_w >= export_min_w).then(|| {
            button("toolbar/export", "Export the mix", ui, None)
                .text("Export…")
                .padding(Insets::xy(12.0, 0.0))
                .height(Sizing::Fixed(28.0))
        }))
        .children((meter_w >= 40.0).then(|| {
            Node::row()
                .padding(Insets {
                    left: 8.0,
                    ..Insets::default()
                })
                .child(meter(
                    "master/meter",
                    "Master",
                    model.master,
                    false,
                    meter_w,
                    14.0,
                ))
        }))
}

pub(crate) fn meter(
    id: &str,
    label: &str,
    level: [f32; 2],
    vertical: bool,
    w: f32,
    h: f32,
) -> Node {
    let display = level.map(meter_position);
    let peak = level[0].max(level[1]);
    let value = if peak > 0.0 {
        format!("{:.1} dBFS", 20.0 * peak.log10())
    } else {
        "silence".into()
    };
    Node::row()
        .id(id)
        .role(Role::Meter)
        .accessible_label(label)
        .value(value)
        .size(w, h)
        .paint(Paint::Meter {
            levels: display,
            vertical,
            background: pal().meter,
        })
}

/// Maps a linear level to a meter position: -60 dBFS at one end, 0 at the
/// other.
pub(crate) fn meter_position(level: f32) -> f32 {
    if level <= 0.0 {
        return 0.0;
    }
    ((20.0 * level.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

fn statusbar(model: &ViewModel, ui: &UiState, radius: [f32; 4]) -> Node {
    let small = |s: &mut dawkey_ui::Style| {
        s.font_size = 11.0;
        s.color = pal().text_dim;
    };
    let project = match (&model.path, model.dirty) {
        (_, true) => "Unsaved changes".to_owned(),
        (Some(path), false) => format!("Saved: {path}"),
        (None, false) => "New project".to_owned(),
    };
    let mut bar = Node::row()
        .id("statusbar")
        .role(Role::StatusBar)
        .width(Sizing::Fill(1.0))
        .height(Sizing::Fixed(26.0))
        .padding(Insets::xy(12.0, 0.0))
        .gap(14.0)
        .style(|s| {
            s.background = Some(pal().titlebar);
            s.radius = radius;
        })
        .child(
            Node::label(model.engine.clone())
                .id("status/engine")
                .style(small),
        )
        .child(Node::spacer());
    if let Some((name, done)) = &model.exporting {
        bar = bar
            .child(
                Node::label(format!("Exporting {name}… {:.0}%", done * 100.0))
                    .id("status/export")
                    .role(Role::ProgressBar)
                    .value(format!("{:.0}%", done * 100.0))
                    .style(small),
            )
            .child(
                button("status/export_cancel", "Cancel the export", ui, None)
                    .text("Cancel")
                    .padding(Insets::xy(8.0, 0.0))
                    .height(Sizing::Fixed(20.0))
                    .style(|s| s.font_size = 11.0),
            );
    }
    if let Some(message) = &model.message {
        bar = bar.child(
            Node::label(message.clone())
                .id("status/message")
                .style(|s| {
                    small(s);
                    s.color = pal().mute_on;
                }),
        );
    }
    bar.child(Node::label(project).id("status/project").style(small))
        .child(
            button("status/preferences", "Preferences", ui, None)
                .text("Preferences")
                .padding(Insets::xy(8.0, 0.0))
                .height(Sizing::Fixed(20.0))
                .style(|s| {
                    s.font_size = 11.0;
                    s.color = pal().text_dim;
                }),
        )
}

/// "92.5" for 92.5, "100" for 100.
pub(crate) fn format_bpm(bpm: f64) -> String {
    let text = format!("{bpm:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// `MM:SS.mmm`, or `H:MM:SS.mmm` past an hour.
pub(crate) fn format_time(seconds: f64) -> String {
    let ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let (h, m, s, ms) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{m:02}:{s:02}.{ms:03}")
    }
}
