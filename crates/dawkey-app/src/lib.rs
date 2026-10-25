// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dawkey's application layer: the [`App`] state shared by the Wayland GUI
//! and the MCP server, and the views and actions of the main window.
//!
//! The GUI and headless mode differ only in who supplies input and window
//! state: the Wayland platform, or MCP's `ui_*` tools. Either way the same
//! views are built, laid out, and drawn by the same renderer.

#![forbid(unsafe_code)]

mod auto_write;
mod clip_input;
mod confirm;
mod export;
mod input;
mod mcp_http;
mod picker;
mod plugin_input;
mod prefs;
mod prefs_input;
mod recovery;
mod roll_input;
mod state;
mod take_input;
mod tempo_input;
mod text_input;
mod theme;
mod view;

use std::path::Path;
use std::time::{Duration, Instant};

use dawkey_core::{AddClip, ClipContent, ClipId, SourceId, TrackId};
use dawkey_media::{DecodedFile, MidiFile};
use dawkey_render::{Gpu, GpuPreference, Image, Renderer, wgpu};
use dawkey_studio::{LoadedProject, MidiImport, Studio};
use dawkey_text::TextSystem;
use dawkey_ui::{
    AccessNode, ChromeAction, Color, Cursor, InputEvent, Modifiers, MouseButton, Point, Scene,
    Size, Ui,
};
use serde::Serialize;

pub use mcp_http::{
    DEFAULT_MCP_PORT, McpEndpoint, McpHttpServer, McpHttpState, default_token_path,
    load_or_create_token, mcp_url, new_token, tokens_match,
};
pub use prefs::{AudioChoice, PluginKind, Preferences, ThemeChoice};
pub use state::{
    ExportRange, ExportSettings, ImportRequest, Marker, Page, PendingAction, PlatformRequest,
    RollLane, ViewState,
};
pub use theme::{SHADOW_MARGIN, Theme};

use crate::view::{
    ClipView, ClipViewContent, ControlView, LaneView, PickerEntry, SlotView, TrackView, ViewModel,
};

/// Default size of a headless window, including the shadow margin.
pub const HEADLESS_SIZE: Size = Size::new(1280.0, 800.0);

/// Where the window lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    /// No window; the UI exists only for MCP's `ui_*` tools.
    Headless,
    Wayland,
}

/// Window state reported by the platform.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct WindowInfo {
    /// Logical size of the whole surface, including any shadow margin.
    pub size: Size,
    /// Physical pixels per logical pixel.
    pub scale: f32,
    pub maximized: bool,
    pub fullscreen: bool,
    /// Tiled against any screen edge.
    pub tiled: bool,
    pub activated: bool,
    /// Dawkey draws its own title bar (the compositor agreed to
    /// client-side decorations).
    pub csd: bool,
    /// The UI scale from the preferences, on top of `scale`: the app lays
    /// out in units this much bigger than the platform's logical pixels.
    /// The platform always reports 1.
    pub zoom: f32,
}

impl WindowInfo {
    pub fn headless() -> Self {
        Self {
            size: HEADLESS_SIZE,
            scale: 1.0,
            maximized: false,
            fullscreen: false,
            tiled: false,
            activated: true,
            csd: true,
            zoom: 1.0,
        }
    }

    /// Transparent space around the window for its shadow, in the window's
    /// units (it's always [`SHADOW_MARGIN`] logical pixels). None when the
    /// window is snapped to screen edges or drawn by the compositor.
    pub fn shadow_margin(&self) -> f32 {
        if self.floating() {
            SHADOW_MARGIN / self.zoom
        } else {
            0.0
        }
    }

    /// The window in units `zoom` times its logical pixels.
    fn zoomed(self, zoom: f32) -> Self {
        Self {
            size: Size::new(self.size.w / zoom, self.size.h / zoom),
            scale: self.scale * zoom,
            zoom,
            ..self
        }
    }

    pub fn corner_radius(&self) -> f32 {
        if self.floating() {
            theme::WINDOW_RADIUS
        } else {
            0.0
        }
    }

    fn floating(&self) -> bool {
        self.csd && !self.maximized && !self.fullscreen && !self.tiled
    }
}

/// Where a synthetic drag starts.
#[derive(Debug, Clone, PartialEq)]
pub enum DragFrom {
    /// The center of a widget.
    Widget(String),
    Point(Point),
}

/// The result of placing imported audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Imported {
    pub source: SourceId,
    pub track: Option<TrackId>,
    pub clip: Option<ClipId>,
}

pub struct App {
    pub studio: Studio,
    platform: Platform,
    /// The window in the app's units (`window` scaled by `ui_scale`).
    zoomed: WindowInfo,
    ui: Ui,
    text: TextSystem,
    graphics: Option<(Gpu, Renderer)>,
    pub(crate) view: ViewState,
    /// A short message for the status bar, e.g. the last error.
    pub(crate) message: Option<String>,
    /// Synthetic event clock for input that doesn't come from a device.
    synthetic_ms: u32,
    /// Distinguishes undo-merge keys of separate gestures.
    pub(crate) gesture_serial: u64,
    /// Window actions from synthetic input, for the platform to carry out.
    chrome_requests: Vec<ChromeAction>,
    pub(crate) platform_requests: Vec<PlatformRequest>,
    pub(crate) imports: Vec<ImportRequest>,
    /// The window should close (the "save changes?" dialog said so).
    pub(crate) close_requested: bool,
    /// Session revision at the last rebuild.
    seen_revision: u64,
    /// Plugins were being scanned at the last tick.
    was_scanning: bool,
    /// Something changed by itself since the last rebuild (a scan
    /// finished): draw once more.
    redraw: bool,
    /// A meter showed a level at the last tick (one moving, or falling
    /// back after playback stopped).
    meters_showing: bool,
    /// The scene the window shows, from the last [`draw`](Self::draw).
    drawn: Option<Scene>,
    /// The window as the platform reports it.
    window: WindowInfo,
    ui_scale: f32,
    /// The user's preferences, and where changes to them are saved.
    pub(crate) prefs: Preferences,
    prefs_path: Option<std::path::PathBuf>,
    /// The window's MCP-over-HTTP server (`None` headless), where its token
    /// is kept, and what it's doing.
    mcp_http: Option<Box<dyn McpHttpServer>>,
    mcp_token_path: Option<std::path::PathBuf>,
    mcp_http_state: McpHttpState,
    /// The theme in use, and whether the desktop prefers dark (`None`: it
    /// doesn't say).
    theme: theme::Theme,
    desktop_dark: Option<bool>,
    /// The export running on a worker thread.
    pub(crate) export: Option<export::RunningExport>,
    /// How often unsaved changes are autosaved (`None`: never), and when
    /// it was last checked.
    pub(crate) autosave_every: Option<Duration>,
    pub(crate) last_autosave: Instant,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("platform", &self.platform)
            .field("window", &self.window)
            .field("view", &self.view)
            .finish_non_exhaustive()
    }
}

impl App {
    pub fn new(studio: Studio, platform: Platform) -> Self {
        let mut app = Self {
            studio,
            platform,
            zoomed: WindowInfo::headless(),
            window: WindowInfo::headless(),
            ui_scale: 1.0,
            prefs: Preferences::default(),
            prefs_path: None,
            mcp_http: None,
            mcp_token_path: None,
            mcp_http_state: McpHttpState::Unavailable,
            theme: theme::Theme::Dark,
            desktop_dark: None,
            ui: Ui::new(),
            text: TextSystem::new(),
            graphics: None,
            view: ViewState::default(),
            message: None,
            was_scanning: false,
            redraw: false,
            meters_showing: false,
            drawn: None,
            export: None,
            autosave_every: Some(recovery::AUTOSAVE_EVERY),
            last_autosave: Instant::now(),
            synthetic_ms: 0,
            gesture_serial: 0,
            chrome_requests: Vec::new(),
            platform_requests: Vec::new(),
            imports: Vec::new(),
            close_requested: false,
            seen_revision: 0,
        };
        app.rebuild();
        app
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// The window title: project name, with a marker for unsaved changes.
    pub fn title(&self) -> String {
        let session = self.studio.session();
        view::window_title(&session.project().name, session.is_dirty())
    }

    /// The window as the platform reported it.
    pub fn window(&self) -> WindowInfo {
        self.window
    }

    pub fn set_window(&mut self, window: WindowInfo) {
        self.window = window;
        self.zoomed = window.zoomed(self.ui_scale);
        self.rebuild();
    }

    /// Scales the whole UI by `scale` on top of the desktop's scale.
    pub fn set_ui_scale(&mut self, scale: f32) {
        self.ui_scale = scale;
        self.set_window(self.window);
    }

    pub fn view_state(&self) -> &ViewState {
        &self.view
    }

    /// Shows a short message in the status bar.
    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
    }

    /// Uses `gpu` for drawing (the platform's, matched to its surface).
    pub fn set_gpu(&mut self, gpu: Gpu) {
        let renderer = Renderer::new(&gpu.device);
        self.graphics = Some((gpu, renderer));
    }

    pub fn gpu(&self) -> Option<&Gpu> {
        self.graphics.as_ref().map(|(gpu, _)| gpu)
    }

    /// Periodic housekeeping.
    pub fn tick(&mut self) {
        self.studio.tick();
        self.apply_plugin_param_changes();
        self.keep_writing_automation();
        self.poll_export();
        self.autosave_now_and_then();
        let scanning = self.studio.scanning_plugins();
        if self.was_scanning && !scanning {
            let found = self.studio.cached_catalog().map_or(0, |c| c.plugins.len());
            self.message = Some(format!("Found {found} plugins"));
            self.redraw = true;
        }
        self.was_scanning = scanning;
        // Meters move while stopped too: monitoring, live MIDI, tails, and
        // falling back after playback.
        let meters = self.studio.display_meters();
        self.meters_showing = meters
            .master
            .iter()
            .chain(meters.tracks.values().flatten())
            .any(|&level| view::meter_position(level) > 0.0);
        // A plugin's GUI changed a parameter.
        if self.studio.session().revision() != self.seen_revision {
            self.redraw = true;
        }
    }

    /// True while something on screen changes by itself (playback, meters,
    /// a plugin scan finishing).
    pub fn animating(&self) -> bool {
        self.studio.transport().playing || self.meters_showing || self.redraw
    }

    /// Whether the window would look different drawn now. For changes
    /// nothing announces (a plugin crashing or closing its GUI, PipeWire
    /// linking, ...); the platform asks now and then while idle.
    pub fn stale(&mut self) -> bool {
        let scene = self.scene();
        self.drawn.as_ref() != Some(&scene)
    }

    /// Rebuilds and lays out the view from the current state.
    pub fn rebuild(&mut self) {
        self.redraw = false;
        theme::use_palette(self.theme.palette());
        let model = self.view_model();
        // Plugins, lanes and points may be gone (undo, removal).
        let project = self.studio.session().project();
        if self
            .view
            .plugin_editor
            .is_some_and(|id| project.plugin(id).is_none())
        {
            self.view.plugin_editor = None;
        }
        if self
            .view
            .plugin_picker
            .is_some_and(|id| project.track(id).is_none())
        {
            self.view.plugin_picker = None;
        }
        if let Some((track, lane, index)) = self.view.selected_point
            && project
                .track(track)
                .and_then(|t| t.automation.get(lane))
                .is_none_or(|l| index >= l.points.len())
        {
            self.view.selected_point = None;
        }
        // A controller point is selected by index, which any change to the
        // project (an undo, say) can make point elsewhere.
        let revision = self.studio.session().revision();
        if revision != self.seen_revision {
            self.seen_revision = revision;
            self.view.selected_control = None;
        }
        // The edited clip may be gone (undo, removal).
        if self.view.editing_clip.is_some() && view::edited(&model, &self.view).is_none() {
            self.view.editing_clip = None;
            self.view.selected_notes.clear();
            if self.view.page == Page::PianoRoll {
                self.view.page = Page::Arrange;
            }
        }
        let root = view::build(&model, &self.view, self.ui.state(), &self.zoomed);
        self.ui.set_root(root, self.zoomed.size, &mut self.text);
        // A drag that will never end (its widget went away, or its release
        // was lost) drops its gesture, or a preview, an undo merge or touch
        // automation would hang on.
        if self.ui.take_dropped_drag() && self.view.gesture.is_some() {
            self.end_gesture();
            self.rebuild();
        }
    }

    /// Routes one input event from a device. Window-chrome requests are
    /// returned for the platform.
    pub fn handle_input(&mut self, event: InputEvent) -> Vec<ChromeAction> {
        // From the platform's logical pixels into the app's units.
        let z = self.ui_scale;
        let unzoom = |p: Point| Point::new(p.x / z, p.y / z);
        let event = match event {
            InputEvent::PointerMove(pos) => InputEvent::PointerMove(unzoom(pos)),
            InputEvent::PointerDown {
                pos,
                button,
                time_ms,
            } => InputEvent::PointerDown {
                pos: unzoom(pos),
                button,
                time_ms,
            },
            InputEvent::PointerUp { pos, button } => InputEvent::PointerUp {
                pos: unzoom(pos),
                button,
            },
            InputEvent::Scroll { pos, delta } => InputEvent::Scroll {
                pos: unzoom(pos),
                delta: unzoom(delta),
            },
            event => event,
        };
        let actions = self.ui.handle(event);
        let chrome = self.apply(actions);
        self.rebuild();
        chrome
            .into_iter()
            .map(|action| match action {
                ChromeAction::WindowMenu(p) => {
                    ChromeAction::WindowMenu(Point::new(p.x * z, p.y * z))
                }
                action => action,
            })
            .collect()
    }

    /// Runs synthetic input with `modifiers` held, queueing any window
    /// actions for the platform.
    fn synthetic(
        &mut self,
        modifiers: Modifiers,
        f: impl FnOnce(&mut Ui, u32) -> Result<Vec<dawkey_ui::UiAction>, String>,
    ) -> Result<Vec<ChromeAction>, String> {
        self.rebuild();
        self.synthetic_ms = self.synthetic_ms.wrapping_add(1000);
        let held = self.ui.state().modifiers;
        self.ui.handle(InputEvent::Modifiers(modifiers));
        let actions = f(&mut self.ui, self.synthetic_ms);
        self.ui.handle(InputEvent::Modifiers(held));
        let chrome = self.apply(actions?);
        self.chrome_requests.extend(chrome.iter().copied());
        self.rebuild();
        Ok(chrome)
    }

    /// Clicks the widget with this ID, as a pointer would.
    pub fn click(
        &mut self,
        id: &str,
        button: MouseButton,
        modifiers: Modifiers,
    ) -> Result<Vec<ChromeAction>, String> {
        self.synthetic(modifiers, |ui, t| ui.click(id, button, t))
    }

    /// Double-clicks the widget with this ID.
    pub fn double_click(
        &mut self,
        id: &str,
        modifiers: Modifiers,
    ) -> Result<Vec<ChromeAction>, String> {
        self.synthetic(modifiers, |ui, t| ui.double_click(id, t))
    }

    /// Double-clicks at a window position.
    pub fn double_click_at(
        &mut self,
        pos: Point,
        modifiers: Modifiers,
    ) -> Result<Vec<ChromeAction>, String> {
        self.synthetic(modifiers, |ui, t| Ok(ui.double_click_at(pos, t)))
    }

    /// Clicks at a window position.
    pub fn click_at(
        &mut self,
        pos: Point,
        button: MouseButton,
        modifiers: Modifiers,
    ) -> Result<Vec<ChromeAction>, String> {
        self.synthetic(modifiers, |ui, t| Ok(ui.click_at(pos, button, t)))
    }

    /// Drags with the left button from `from` by `delta` logical px.
    pub fn drag(
        &mut self,
        from: DragFrom,
        delta: Point,
        steps: u32,
        modifiers: Modifiers,
    ) -> Result<Vec<ChromeAction>, String> {
        self.rebuild();
        let start = match from {
            DragFrom::Widget(id) => self.ui.widget_center(&id)?,
            DragFrom::Point(p) => p,
        };
        let end = Point::new(start.x + delta.x, start.y + delta.y);
        self.synthetic(modifiers, |ui, t| Ok(ui.drag(start, end, steps, t)))
    }

    /// The center of the widget with this ID, in window coordinates.
    pub fn widget_center(&mut self, id: &str) -> Result<Point, String> {
        self.rebuild();
        self.ui.widget_center(id)
    }

    /// Moves the pointer to `pos`, with no button held.
    pub fn hover(&mut self, pos: Point) {
        let _ = self.synthetic(Modifiers::default(), |ui, _| {
            Ok(ui.handle(InputEvent::PointerMove(pos)))
        });
    }

    /// Scrolls by `delta` logical px at `pos`.
    pub fn scroll(&mut self, pos: Point, delta: Point, modifiers: Modifiers) {
        let _ = self.synthetic(modifiers, |ui, _| {
            Ok(ui.handle(InputEvent::Scroll { pos, delta }))
        });
    }

    /// Presses a key (see [`InputEvent::Key`] for names).
    pub fn key(&mut self, key: &str, modifiers: Modifiers) -> Result<Vec<ChromeAction>, String> {
        let key = key.to_lowercase();
        self.synthetic(modifiers, |ui, _| Ok(ui.handle(InputEvent::Key { key })))
    }

    /// The pointer shape the widget under the pointer (or being dragged)
    /// asks for.
    pub fn cursor(&self) -> Option<Cursor> {
        self.ui.cursor()
    }

    /// Window actions requested by synthetic input since the last call.
    pub fn take_chrome_requests(&mut self) -> Vec<ChromeAction> {
        if std::mem::take(&mut self.close_requested) {
            self.chrome_requests.push(ChromeAction::Close);
        }
        std::mem::take(&mut self.chrome_requests)
    }

    /// Dialogs the UI asked for since the last call.
    pub fn take_platform_requests(&mut self) -> Vec<PlatformRequest> {
        std::mem::take(&mut self.platform_requests)
    }

    /// Imports waiting to be decoded (from drops or the import dialog).
    pub fn take_imports(&mut self) -> Vec<ImportRequest> {
        std::mem::take(&mut self.imports)
    }

    /// Queues imports, e.g. from the platform's file dialog.
    pub fn queue_imports(&mut self, imports: impl IntoIterator<Item = ImportRequest>) {
        self.imports.extend(imports);
    }

    /// Places a decoded import: adds it to the pool and puts a clip on the
    /// timeline (one undo step).
    pub fn finish_import(
        &mut self,
        request: &ImportRequest,
        decoded: DecodedFile,
    ) -> Result<Imported, String> {
        let source = self.studio.add_source(&request.path, decoded);
        let outcome = self
            .studio
            .execute(AddClip::new(source, request.track, request.position_s).into())
            .map_err(|e| e.to_string())?;
        self.rebuild();
        Ok(Imported {
            source,
            track: outcome.track,
            clip: outcome.clip,
        })
    }

    /// Places an imported MIDI file on new tracks (one undo step).
    pub fn finish_midi_import(
        &mut self,
        request: &ImportRequest,
        file: &MidiFile,
    ) -> Result<MidiImport, String> {
        let import = self
            .studio
            .import_midi(file, request.position_s, None, &[], false)
            .map_err(|e| e.to_string())?;
        self.rebuild();
        Ok(import)
    }

    /// Saves to `path` (from the save dialog).
    pub fn save_as(&mut self, path: &Path) -> Result<(), String> {
        let saved = self.studio.save(Some(path)).map_err(|e| e.to_string())?;
        self.set_message(format!("Saved {}", saved.display()));
        Ok(())
    }

    /// Replaces the open project (from the open dialog).
    pub fn replace_project(&mut self, loaded: LoadedProject) -> Result<(), String> {
        self.studio.replace(loaded).map_err(|e| e.to_string())?;
        self.view = ViewState {
            page: self.view.page,
            export: self.view.export,
            ..ViewState::default()
        };
        // Opened in the window: changes a crash kept are offered back.
        if self.platform == Platform::Wayland {
            self.view.recovery = self.studio.bundle_recovery();
        }
        self.rebuild();
        Ok(())
    }

    pub fn access_tree(&mut self) -> AccessNode {
        self.rebuild();
        self.ui.access_tree()
    }

    pub fn scene(&mut self) -> Scene {
        self.rebuild();
        self.ui.scene()
    }

    /// Draws the window into a surface texture.
    pub fn draw(
        &mut self,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        size: (u32, u32),
    ) {
        let scene = self.scene();
        let scale = self.zoomed.scale;
        let Some((gpu, renderer)) = &mut self.graphics else {
            tracing::warn!("draw called before set_gpu");
            return;
        };
        renderer.render(
            gpu,
            &mut self.text,
            &scene,
            scale,
            target,
            format,
            size,
            Color::TRANSPARENT,
        );
        self.drawn = Some(scene);
    }

    /// Renders the window offscreen at `scale`. Headless instances open a
    /// software GPU on first use, so screenshots are reproducible.
    pub fn screenshot(&mut self, scale: f32) -> Result<Image, String> {
        if self.graphics.is_none() {
            let gpu = Gpu::headless(GpuPreference::Software).map_err(|e| e.to_string())?;
            self.set_gpu(gpu);
        }
        let scene = self.scene();
        let (gpu, renderer) = self.graphics.as_mut().expect("set above");
        renderer
            .render_image(gpu, &mut self.text, &scene, scale, Color::TRANSPARENT)
            .map_err(|e| e.to_string())
    }

    fn view_model(&mut self) -> ViewModel {
        let meters = self.studio.display_meters();
        let status = self.studio.engine_status();
        // Plugin names come from the catalog; don't scan just to draw a
        // project without plugins.
        let wants_catalog = self.view.plugin_picker.is_some()
            || self
                .studio
                .session()
                .project()
                .all_tracks()
                .any(|t| !t.plugins.is_empty());
        if wants_catalog {
            // Starts a scan if there's no list yet; the window doesn't wait.
            self.studio.plugin_catalog_now();
        }
        let scanning = self.studio.scanning_plugins();
        let editor = self.view.plugin_editor.and_then(|id| {
            // Only the rows in view: plugins can have thousands.
            let (first, count) = view::plugins::visible_window(self.view.overlay_scroll);
            self.studio.plugin_details_window(id, first, count)
        });
        let catalog = self.studio.cached_catalog().filter(|_| wants_catalog);
        let picker = self.view.plugin_picker.map(|_| {
            catalog.map_or_else(Vec::new, |c| {
                picker::picker_matches(&c.plugins, self.view.picker_filter, &self.view.picker_query)
                    .into_iter()
                    .map(|p| PickerEntry {
                        id: p.id.clone(),
                        name: p.name.clone(),
                        vendor: p.vendor.clone(),
                        instrument: p.instrument,
                    })
                    .collect()
            })
        });
        let plugin_info = |id: &str| catalog.and_then(|c| c.get(id));
        let session = self.studio.session();
        let project = session.project();
        let rate_khz = f64::from(status.sample_rate) / 1000.0;
        let engine = match &status.pipewire {
            None => format!("Virtual clock · {rate_khz} kHz"),
            Some(pw) => {
                let mut line = format!("PipeWire · {} · {rate_khz} kHz", pw.state);
                if let Some(sink) = &pw.linked_to {
                    line.push_str(&format!(" · → {sink}"));
                }
                if !pw.midi_from.is_empty() {
                    line.push_str(&format!(" · MIDI in: {}", pw.midi_from.len()));
                }
                if pw.rate_mismatch {
                    line.push_str(" · graph rate mismatch");
                }
                line
            }
        };
        let tempo = project.tempo_map();
        // Faders and pan show what their automation has at the playhead.
        let now = project.seconds_to_frames(status.transport.position_s);
        let automated =
            |t: &dawkey_core::Track, target: dawkey_core::AutomationTarget, fixed: f32| {
                t.lane(&target)
                    .and_then(|l| l.value_at(now))
                    .map_or(fixed, |v| v as f32)
            };
        let track_view = |t: &dawkey_core::Track| TrackView {
            id: t.id,
            name: t.name.clone(),
            kind: t.kind,
            output: t.output,
            output_name: t
                .output
                .and_then(|b| project.track(b))
                .map_or_else(|| "Master".to_owned(), |b| b.name.clone()),
            sends: t
                .sends
                .iter()
                .map(|send| view::SendView {
                    bus: send.bus,
                    bus_name: project
                        .track(send.bus)
                        .map_or_else(|| format!("Bus {}", send.bus), |b| b.name.clone()),
                    gain_db: automated(
                        t,
                        dawkey_core::AutomationTarget::Send { bus: send.bus },
                        send.gain_db,
                    ),
                    pre_fader: send.pre_fader,
                })
                .collect(),
            can_add_send: !t.is_master()
                && project.tracks.iter().any(|b| {
                    b.kind == dawkey_core::TrackKind::Bus
                        && b.id != t.id
                        && !t.sends.iter().any(|s| s.bus == b.id)
                        && !project.depends_on(t.id, b.id)
                }),
            gain_db: automated(t, dawkey_core::AutomationTarget::Gain, t.gain_db),
            pan: automated(t, dawkey_core::AutomationTarget::Pan, t.pan),
            mute: t.mute,
            solo: t.solo,
            armed: t.armed,
            monitor: t.monitor,
            take_lanes: t
                .clips
                .iter()
                .map(|c| c.lane as usize + 1)
                .max()
                .unwrap_or(1),
            synth: t.synth,
            midi_input: t.midi_input.clone(),
            midi_input_channel: t.midi_input_channel,
            midi_output: t.midi_output.as_ref().map(|o| o.device.clone()),
            midi_output_latency_ms: t.midi_output.as_ref().map_or(0.0, |o| o.latency_ms),
            midi_output_reported_ms: t
                .midi_output
                .as_ref()
                .and_then(|o| status.midi_output_latency_ms.get(&o.device))
                .copied()
                .unwrap_or(0.0),
            plugins: t
                .plugins
                .iter()
                .map(|slot| SlotView {
                    id: slot.id,
                    name: plugin_info(&slot.plugin)
                        .map_or_else(|| slot.plugin.clone(), |i| i.name.clone()),
                    bypass: slot.bypass,
                    error: status.plugin_problems.iter().any(|p| p.plugin == slot.id),
                })
                .collect(),
            automation: t
                .automation
                .iter()
                .map(|lane| {
                    let (label, kind) = view::lane_label_kind(t, &lane.target, catalog);
                    LaneView {
                        label,
                        kind,
                        points: lane
                            .points
                            .iter()
                            .map(|p| (project.frames_to_seconds(p.frame), p.value))
                            .collect(),
                    }
                })
                .collect(),
            level: if t.is_master() {
                meters.master
            } else {
                meters.tracks.get(&t.id).copied().unwrap_or_default()
            },
            clips: t
                .clips
                .iter()
                .map(|c| ClipView {
                    id: c.id,
                    name: view::clip_label(project, t, c),
                    start_s: project.frames_to_seconds(c.start),
                    length_s: project.frames_to_seconds(c.length),
                    muted: c.muted,
                    lane: c.lane as usize,
                    content: match &c.content {
                        ClipContent::Audio { source, offset } => ClipViewContent::Audio {
                            offset: *offset,
                            peaks: self.studio.peaks(*source),
                        },
                        ClipContent::Midi { controls, .. } => ClipViewContent::Midi {
                            controls: controls
                                .iter()
                                .map(|p| ControlView {
                                    controller: p.controller,
                                    beat: tempo.clip_beats(c.start, p.frame),
                                    value: p.value,
                                })
                                .collect(),
                            notes: view::note_views(project, c).into(),
                        },
                    },
                })
                .collect(),
        };
        let tracks = project.tracks.iter().map(track_view).collect();
        let master_track = track_view(&project.master);
        let meter = project.meter_map();
        ViewModel {
            title: project.name.clone(),
            dirty: session.is_dirty(),
            path: session.path().map(|p| p.display().to_string()),
            playing: status.transport.playing,
            recording: status.transport.recording,
            record_start_s: self.studio.record_start_s(),
            position_s: status.transport.position_s,
            sample_rate: project.sample_rate,
            loop_range: project.loop_region.range.map(|r| {
                (
                    project.frames_to_seconds(r.start),
                    project.frames_to_seconds(r.end),
                )
            }),
            loop_enabled: project.loop_region.enabled,
            punch_range: project.punch.range.map(|r| {
                (
                    project.frames_to_seconds(r.start),
                    project.frames_to_seconds(r.end),
                )
            }),
            punch_enabled: project.punch.enabled,
            tempo_bpm: tempo.bpm_at(tempo.beat_at_seconds(status.transport.position_s)),
            time_signature: {
                let sig = meter.meter_at(tempo.beat_at_seconds(status.transport.position_s));
                (sig.numerator, sig.denominator)
            },
            tempo,
            meter,
            tracks,
            master_track,
            master: meters.master,
            engine,
            message: self.message.clone(),
            editor,
            picker,
            scanning,
            automation_write: self.view.automation_write,
            song_end_s: project.frames_to_seconds(project.end()),
            prefs: self.prefs.clone(),
            mcp_http: self.mcp_http_state.clone(),
            mcp_token_path: self.mcp_token_path().map(mcp_http::tilde),
            exporting: self.export.as_ref().map(|e| {
                let name = e
                    .path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                (name, self.export_progress().unwrap_or(0.0))
            }),
        }
    }
}

#[cfg(test)]
mod tests;
