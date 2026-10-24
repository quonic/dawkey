// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dawkey's Wayland window.
//!
//! Decorations are Dawkey's own: we ask the compositor for client-side
//! decorations (`zxdg_decoration_manager_v1`) and draw the title bar, window
//! buttons, rounded corners and shadow ourselves. Moving, resizing and the
//! window menu go through `xdg_toplevel` requests. If the compositor insists
//! on server-side decorations, the app hides its title bar.
//!
//! The surface includes a transparent shadow margin around the window; the
//! window geometry excludes it, and resize handles live in it. HiDPI uses
//! `wp_fractional_scale_v1` with `wp_viewporter`, falling back to integer
//! buffer scales.
//!
//! The app runs on this thread. Other threads (MCP) reach it through the
//! [`Handle`] passed to `on_ready`; their jobs arrive on a calloop channel.

mod dialogs;

use std::ptr::NonNull;
use std::time::Duration;

use dawkey_app::{App, WindowInfo};
use dawkey_core::Handle;
use dawkey_core::host::{Closed, Job};
use dawkey_render::{Gpu, GpuPreference, wgpu};
use dawkey_ui::{ChromeAction, Cursor, InputEvent, Modifiers, MouseButton, Point, Size};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::compositor::{
    CompositorHandler, CompositorState, FrameCallbackData, Region,
};
use smithay_client_toolkit::data_device_manager::DataDeviceManagerState;
use smithay_client_toolkit::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{self, EventLoop, LoopHandle, PostAction};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers as KeyboardModifiers, RawModifiers,
};
use smithay_client_toolkit::seat::pointer::{
    AxisScroll, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, CursorIcon, PointerData, PointerEvent,
    PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::window::{
    DecorationMode, Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::shell::xdg::{XdgShell, XdgSurface};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{delegate_registry, registry_handlers};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_data_device::WlDataDevice;
use wayland_client::protocol::wl_data_device_manager::DndAction;
use wayland_client::protocol::wl_keyboard::WlKeyboard;
use wayland_client::protocol::{wl_output, wl_pointer, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{
    self, WpFractionalScaleV1,
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

/// Window size (excluding the shadow) before the compositor picks one.
const DEFAULT_SIZE: (u32, u32) = (1200, 760);
const MIN_SIZE: (u32, u32) = (640, 400);
/// Resize handles reach this far outside the visible window edge…
const RESIZE_OUTSIDE: f32 = 10.0;
/// …and this far inside it.
const RESIZE_INSIDE: f32 = 4.0;
/// Corner handles extend this far along each edge.
const RESIZE_CORNER: f32 = 20.0;
/// How often the app's housekeeping runs.
const TICK: Duration = Duration::from_millis(20);
/// How often an idle window checks whether it looks different now.
const STALE_CHECK: Duration = Duration::from_millis(100);

#[derive(Debug, thiserror::Error)]
pub enum GuiError {
    #[error("connecting to the Wayland compositor: {0}")]
    Connect(String),
    #[error("the compositor lacks a required protocol: {0}")]
    MissingGlobal(String),
    #[error("graphics: {0}")]
    Graphics(String),
    #[error("event loop: {0}")]
    EventLoop(String),
}

/// Opens the window and runs until it is closed. `on_ready` receives a
/// handle for other threads to reach the app. Returns the app afterwards.
pub fn run(app: App, on_ready: impl FnOnce(Handle<App>)) -> Result<App, GuiError> {
    let conn = Connection::connect_to_env().map_err(|e| GuiError::Connect(e.to_string()))?;
    let (globals, event_queue) =
        registry_queue_init::<Gui>(&conn).map_err(|e| GuiError::Connect(e.to_string()))?;
    let qh = event_queue.handle();
    let mut event_loop: EventLoop<Gui> =
        EventLoop::try_new().map_err(|e| GuiError::EventLoop(e.to_string()))?;
    WaylandSource::new(conn.clone(), event_queue)
        .insert(event_loop.handle())
        .map_err(|e| GuiError::EventLoop(e.to_string()))?;

    let missing = |what: &str| GuiError::MissingGlobal(what.to_owned());
    let compositor = CompositorState::bind(&globals, &qh).map_err(|_| missing("wl_compositor"))?;
    let xdg_shell = XdgShell::bind(&globals, &qh).map_err(|_| missing("xdg_wm_base"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|_| missing("wl_shm"))?;
    // Optional: without it, file drops just don't work.
    let data_device_manager = DataDeviceManagerState::bind(&globals, &qh).ok();

    let surface = compositor.create_surface(&qh);
    let window = xdg_shell.create_window(surface, WindowDecorations::RequestClient, &qh);
    window.set_title(app.title());
    window.set_app_id("dawkey");
    window.set_min_size(Some(MIN_SIZE));

    let fractional_manager: Option<WpFractionalScaleManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    let fractional = fractional_manager
        .as_ref()
        .map(|m| m.get_fractional_scale(window.wl_surface(), &qh, ()));
    let viewport = viewporter
        .as_ref()
        .map(|v| v.get_viewport(window.wl_surface(), &qh, ()));
    if fractional.is_none() || viewport.is_none() {
        tracing::info!("no fractional scaling; using integer buffer scales");
    }
    window.commit();

    // The wgpu surface for the window, and a device that can present to it.
    let instance = Gpu::new_instance();
    let display = NonNull::new(conn.backend().display_ptr().cast())
        .ok_or_else(|| GuiError::Graphics("null wl_display".into()))?;
    let wl_surface = NonNull::new(window.wl_surface().id().as_ptr().cast())
        .ok_or_else(|| GuiError::Graphics("null wl_surface".into()))?;
    // SAFETY: the display and surface outlive the wgpu surface: `Gui` drops
    // its `gfx` first, and the connection lives until `run` returns.
    let wgpu_surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                display,
            ))),
            raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(wl_surface)),
        })
    }
    .map_err(|e| GuiError::Graphics(e.to_string()))?;
    let gpu = Gpu::open(instance, Some(&wgpu_surface), GpuPreference::Hardware)
        .map_err(|e| GuiError::Graphics(e.to_string()))?;
    let mut app = app;
    app.set_gpu(gpu);

    let (job_tx, job_rx) = calloop::channel::channel::<Job<App>>();
    event_loop
        .handle()
        .insert_source(job_rx, |event, _, gui: &mut Gui| {
            if let calloop::channel::Event::Msg(job) = event {
                job(&mut gui.app);
                gui.after_app_change();
            }
        })
        .map_err(|e| GuiError::EventLoop(e.to_string()))?;
    event_loop
        .handle()
        .insert_source(Timer::from_duration(TICK), |_, _, gui: &mut Gui| {
            gui.app.tick();
            if gui.app.animating() {
                gui.request_redraw();
            }
            TimeoutAction::ToDuration(TICK)
        })
        .map_err(|e| GuiError::EventLoop(e.to_string()))?;
    event_loop
        .handle()
        .insert_source(Timer::from_duration(STALE_CHECK), |_, _, gui: &mut Gui| {
            if !gui.dirty && gui.app.stale() {
                gui.request_redraw();
            }
            TimeoutAction::ToDuration(STALE_CHECK)
        })
        .map_err(|e| GuiError::EventLoop(e.to_string()))?;

    let handle = Handle::from_dispatch(move |job| job_tx.send(job).map_err(|_| Closed));
    let mut gui = Gui {
        dialogs: {
            dialogs::watch_color_scheme(handle.clone());
            dialogs::Dialogs::start(handle.clone())
        },
        loop_handle: event_loop.handle(),
        keyboard: None,
        data_device_manager,
        data_devices: Vec::new(),
        drop_pos: Point::default(),
        gfx: Some(Graphics {
            surface: wgpu_surface,
            config: None,
        }),
        viewport,
        fractional_active: false,
        _fractional: fractional,
        app,
        window,
        geometry: DEFAULT_SIZE,
        scale: 1.0,
        flags: Flags {
            csd: true,
            ..Flags::default()
        },
        configured: false,
        resized: false,
        dirty: true,
        frame_pending: false,
        exit: false,
        pointer: None,
        cursor: CursorIcon::Default,
        title: String::new(),
        qh: qh.clone(),
        conn: conn.clone(),
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        shm,
    };

    on_ready(handle);

    while !gui.exit {
        event_loop
            .dispatch(None, &mut gui)
            .map_err(|e| GuiError::EventLoop(e.to_string()))?;
        gui.update();
    }
    // The wgpu surface must go before the Wayland surface.
    gui.gfx = None;
    Ok(gui.app)
}

struct Graphics {
    surface: wgpu::Surface<'static>,
    config: Option<wgpu::SurfaceConfiguration>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Flags {
    maximized: bool,
    fullscreen: bool,
    tiled: bool,
    activated: bool,
    csd: bool,
}

struct Gui {
    dialogs: dialogs::Dialogs,
    loop_handle: LoopHandle<'static, Gui>,
    keyboard: Option<WlKeyboard>,
    data_device_manager: Option<DataDeviceManagerState>,
    /// One per seat, for file drops.
    data_devices: Vec<DataDevice>,
    /// Pointer position of a drag-and-drop over the window.
    drop_pos: Point,
    // Declared first so it drops before the window.
    gfx: Option<Graphics>,
    viewport: Option<WpViewport>,
    _fractional: Option<WpFractionalScaleV1>,
    fractional_active: bool,
    app: App,
    window: Window,
    /// Window geometry (without the shadow margin), in logical pixels.
    geometry: (u32, u32),
    scale: f64,
    flags: Flags,
    configured: bool,
    /// The size, scale or window state changed since the last draw; only
    /// the latest is applied, once the events at hand are handled.
    resized: bool,
    dirty: bool,
    frame_pending: bool,
    exit: bool,
    pointer: Option<ThemedPointer>,
    cursor: CursorIcon,
    title: String,
    qh: QueueHandle<Gui>,
    conn: Connection,
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
}

impl Gui {
    fn window_info(&self) -> WindowInfo {
        let mut info = WindowInfo {
            size: Size::default(),
            scale: self.scale as f32,
            maximized: self.flags.maximized,
            fullscreen: self.flags.fullscreen,
            tiled: self.flags.tiled,
            activated: self.flags.activated,
            csd: self.flags.csd,
            zoom: 1.0,
        };
        let margin = info.shadow_margin();
        info.size = Size::new(
            self.geometry.0 as f32 + 2.0 * margin,
            self.geometry.1 as f32 + 2.0 * margin,
        );
        info
    }

    /// Applies the current geometry, decorations and scale to the surface.
    fn apply_size(&mut self) {
        let info = self.window_info();
        let margin = info.shadow_margin();
        let (gw, gh) = self.geometry;
        let m = margin as u32;
        self.window.set_window_geometry(m, m, gw, gh);

        let (lw, lh) = (info.size.w as i32, info.size.h as i32);
        if let Ok(region) = Region::new(&self.compositor) {
            if margin > 0.0 {
                let reach = RESIZE_OUTSIDE.min(margin) as i32;
                region.add(
                    m as i32 - reach,
                    m as i32 - reach,
                    gw as i32 + 2 * reach,
                    gh as i32 + 2 * reach,
                );
            } else {
                region.add(0, 0, lw, lh);
            }
            self.window.set_input_region(Some(region.wl_region()));
        }

        match &self.viewport {
            Some(viewport) if self.fractional_active => viewport.set_destination(lw, lh),
            _ => {
                if let Some(viewport) = &self.viewport {
                    viewport.set_destination(-1, -1);
                }
                let scale = self.scale.round().max(1.0) as u32;
                if let Err(e) = self.window.set_buffer_scale(scale) {
                    tracing::warn!("setting buffer scale {scale}: {e:?}");
                }
            }
        }

        let physical = (
            ((lw as f64) * self.scale).round().max(1.0) as u32,
            ((lh as f64) * self.scale).round().max(1.0) as u32,
        );
        // Reconfiguring waits for the GPU and makes a new swapchain: only
        // when the size changed, not for focus changes and the like.
        let current = self
            .gfx
            .as_ref()
            .and_then(|g| g.config.as_ref())
            .map(|c| (c.width, c.height));
        if current != Some(physical) {
            self.configure_surface(physical);
        }
        self.app.set_window(info);
        self.dirty = true;
    }

    fn configure_surface(&mut self, (width, height): (u32, u32)) {
        let (Some(gfx), Some(gpu)) = (&mut self.gfx, self.app.gpu()) else {
            return;
        };
        let config = match &mut gfx.config {
            Some(config) => {
                config.width = width;
                config.height = height;
                config.clone()
            }
            None => {
                let caps = gfx.surface.get_capabilities(&gpu.adapter);
                // Colors are written as-is, so prefer non-sRGB formats.
                let format = caps
                    .formats
                    .iter()
                    .copied()
                    .find(|f| !f.is_srgb())
                    .unwrap_or(caps.formats[0]);
                let alpha_mode = if caps
                    .alpha_modes
                    .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
                {
                    wgpu::CompositeAlphaMode::PreMultiplied
                } else {
                    tracing::warn!("no premultiplied alpha; the window shadow will be opaque");
                    caps.alpha_modes[0]
                };
                let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
                    wgpu::PresentMode::Mailbox
                } else {
                    wgpu::PresentMode::Fifo
                };
                tracing::debug!("surface: {format:?}, {alpha_mode:?}, {present_mode:?}");
                let config = wgpu::SurfaceConfiguration {
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    format,
                    color_space: wgpu::SurfaceColorSpace::Auto,
                    width,
                    height,
                    present_mode,
                    desired_maximum_frame_latency: 2,
                    alpha_mode,
                    view_formats: vec![],
                };
                gfx.config = Some(config.clone());
                config
            }
        };
        gfx.surface.configure(&gpu.device, &config);
    }

    /// After the app may have changed: carry out its window, dialog and
    /// import requests, and redraw.
    fn after_app_change(&mut self) {
        let chrome = self.app.take_chrome_requests();
        self.chrome(chrome, None);
        for request in self.app.take_platform_requests() {
            self.dialogs.request(request);
        }
        let imports = self.app.take_imports();
        if !imports.is_empty() {
            self.dialogs.import(imports, self.app.studio.sample_rate());
        }
        self.request_redraw();
    }

    fn key(&mut self, event: KeyEvent) {
        if let Some(key) = key_name(&event) {
            let chrome = self.app.handle_input(InputEvent::Key { key });
            self.chrome(chrome, None);
            self.after_app_change();
        }
    }

    /// Asks for a draw; [`update`](Self::update) does it when the
    /// compositor is ready for a frame.
    fn request_redraw(&mut self) {
        self.dirty = true;
    }

    /// After each batch of events: applies the latest size and draws, at
    /// most once. Drawing for every configure of an interactive resize, or
    /// every pointer motion, would fall further behind the more there is
    /// to draw.
    fn update(&mut self) {
        if self.resized {
            self.resized = false;
            self.apply_size();
            // A configure must be answered with a commit even if a frame
            // is still pending.
            self.frame_pending = false;
        }
        if self.dirty && !self.frame_pending {
            self.draw();
        }
    }

    fn draw(&mut self) {
        if !self.configured {
            return;
        }
        let title = self.app.title();
        if title != self.title {
            self.window.set_title(title.clone());
            self.title = title;
        }
        let Some(gfx) = &self.gfx else { return };
        let Some(config) = gfx.config.clone() else {
            return;
        };
        let frame = match gfx.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.configure_surface((config.width, config.height));
                return;
            }
            _ => return,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        // Presenting commits the surface; ask for the next frame first.
        let surface = self.window.wl_surface();
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        self.frame_pending = true;
        self.app
            .draw(&view, config.format, (config.width, config.height));
        if let Some(gpu) = self.app.gpu() {
            gpu.queue.present(frame);
        }
        self.dirty = false;
    }

    /// Carries out window-management requests. `press` is the seat and
    /// serial of the pointer press that caused them, if any; moving and the
    /// window menu need one.
    fn chrome(&mut self, actions: Vec<ChromeAction>, press: Option<(&wl_seat::WlSeat, u32)>) {
        for action in actions {
            match action {
                ChromeAction::Move => {
                    if let Some((seat, serial)) = press {
                        self.window.move_(seat, serial);
                    }
                }
                ChromeAction::WindowMenu(p) => {
                    if let Some((seat, serial)) = press {
                        self.window
                            .show_window_menu(seat, serial, (p.x as i32, p.y as i32));
                    }
                }
                ChromeAction::ToggleMaximize => {
                    if self.flags.maximized {
                        self.window.unset_maximized();
                    } else {
                        self.window.set_maximized();
                    }
                }
                ChromeAction::Minimize => self.window.set_minimized(),
                ChromeAction::Close => self.exit = true,
            }
        }
    }

    /// The resize edge under `pos`, for a floating window.
    fn resize_edge(&self, pos: Point) -> Option<ResizeEdge> {
        let margin = self.window_info().shadow_margin();
        if margin == 0.0 {
            return None;
        }
        let (gw, gh) = (self.geometry.0 as f32, self.geometry.1 as f32);
        let (x, y) = (pos.x - margin, pos.y - margin);
        let near = |v: f32, edge: f32| v >= edge - RESIZE_OUTSIDE && v < edge + RESIZE_INSIDE;
        let near_far = |v: f32, edge: f32| v >= edge - RESIZE_INSIDE && v < edge + RESIZE_OUTSIDE;
        let (left, right) = (near(x, 0.0), near_far(x, gw));
        let (top, bottom) = (near(y, 0.0), near_far(y, gh));
        let (left_c, right_c) = (x < RESIZE_CORNER, x >= gw - RESIZE_CORNER);
        let (top_c, bottom_c) = (y < RESIZE_CORNER, y >= gh - RESIZE_CORNER);
        Some(match () {
            _ if (top && left_c) || (left && top_c) => ResizeEdge::TopLeft,
            _ if (top && right_c) || (right && top_c) => ResizeEdge::TopRight,
            _ if (bottom && left_c) || (left && bottom_c) => ResizeEdge::BottomLeft,
            _ if (bottom && right_c) || (right && bottom_c) => ResizeEdge::BottomRight,
            _ if top => ResizeEdge::Top,
            _ if bottom => ResizeEdge::Bottom,
            _ if left => ResizeEdge::Left,
            _ if right => ResizeEdge::Right,
            _ => return None,
        })
    }

    fn update_cursor(&mut self, pos: Point) {
        let icon = match self.resize_edge(pos) {
            Some(ResizeEdge::Top) => CursorIcon::NResize,
            Some(ResizeEdge::Bottom) => CursorIcon::SResize,
            Some(ResizeEdge::Left) => CursorIcon::WResize,
            Some(ResizeEdge::Right) => CursorIcon::EResize,
            Some(ResizeEdge::TopLeft) => CursorIcon::NwResize,
            Some(ResizeEdge::TopRight) => CursorIcon::NeResize,
            Some(ResizeEdge::BottomLeft) => CursorIcon::SwResize,
            Some(ResizeEdge::BottomRight) => CursorIcon::SeResize,
            _ => match self.app.cursor() {
                Some(Cursor::ResizeHorizontal) => CursorIcon::EwResize,
                None => CursorIcon::Default,
            },
        };
        self.set_cursor(icon, false);
    }

    fn set_cursor(&mut self, icon: CursorIcon, force: bool) {
        if icon == self.cursor && !force {
            return;
        }
        if let Some(pointer) = &self.pointer
            && pointer.set_cursor(&self.conn, icon).is_ok()
        {
            self.cursor = icon;
        }
    }
}

impl CompositorHandler for Gui {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if !self.fractional_active {
            self.scale = f64::from(new_factor.max(1));
            self.resized = true;
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.frame_pending = false;
        if self.app.animating() {
            self.request_redraw();
        }
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl WindowHandler for Gui {
    fn request_close(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _window: &Window) {
        // With unsaved changes, the app asks first.
        if self.app.request_close() {
            self.exit = true;
        } else {
            self.request_redraw();
        }
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        self.flags = Flags {
            maximized: configure.is_maximized(),
            fullscreen: configure.is_fullscreen(),
            tiled: configure.is_tiled_left()
                || configure.is_tiled_right()
                || configure.is_tiled_top()
                || configure.is_tiled_bottom(),
            activated: configure.is_activated(),
            csd: configure.decoration_mode == DecorationMode::Client,
        };
        if !self.flags.csd {
            tracing::info!("the compositor draws the decorations");
        }
        let (w, h) = configure.new_size;
        self.geometry = (
            w.map_or(self.geometry.0, |w| w.get()).max(MIN_SIZE.0),
            h.map_or(self.geometry.1, |h| h.get()).max(MIN_SIZE.1),
        );
        self.configured = true;
        self.resized = true;
    }
}

impl SeatHandler for Gui {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if let Some(manager) = &self.data_device_manager
            && !self.data_devices.iter().any(|d| d.data().seat() == &seat)
        {
            self.data_devices.push(manager.get_data_device(qh, &seat));
        }
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            match self.seat_state.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.loop_handle.clone(),
                Box::new(|gui: &mut Gui, _, event| gui.key(event)),
            ) {
                Ok(keyboard) => self.keyboard = Some(keyboard),
                Err(e) => tracing::warn!("no keyboard: {e}"),
            }
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            let cursor_surface = self.compositor.create_surface(qh);
            match self.seat_state.get_pointer_with_theme::<_, ()>(
                qh,
                &seat,
                self.shm.wl_shm(),
                cursor_surface,
                ThemeSpec::default(),
            ) {
                Ok(pointer) => self.pointer = Some(pointer),
                Err(e) => tracing::warn!("no pointer: {e}"),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer
            && let Some(pointer) = self.pointer.take()
        {
            pointer.pointer().release();
        }
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take()
        {
            keyboard.release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for Gui {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let seat = pointer
            .data::<PointerData<()>>()
            .map(|data| data.seat().clone());
        for event in events {
            if &event.surface != self.window.wl_surface() {
                continue;
            }
            let pos = Point::new(event.position.0 as f32, event.position.1 as f32);
            match event.kind {
                PointerEventKind::Enter { .. } => {
                    self.app.handle_input(InputEvent::PointerMove(pos));
                    self.update_cursor(pos);
                    self.set_cursor(self.cursor, true);
                }
                PointerEventKind::Leave { .. } => {
                    self.app.handle_input(InputEvent::PointerLeave);
                }
                PointerEventKind::Motion { .. } => {
                    self.app.handle_input(InputEvent::PointerMove(pos));
                    self.update_cursor(pos);
                }
                PointerEventKind::Press {
                    button,
                    serial,
                    time,
                } => {
                    if button == BTN_LEFT
                        && let (Some(edge), Some(seat)) = (self.resize_edge(pos), &seat)
                    {
                        self.window.resize(seat, serial, edge);
                        continue;
                    }
                    let Some(button) = mouse_button(button) else {
                        continue;
                    };
                    let actions = self.app.handle_input(InputEvent::PointerDown {
                        pos,
                        button,
                        time_ms: time,
                    });
                    self.chrome(actions, seat.as_ref().map(|s| (s, serial)));
                }
                PointerEventKind::Release { button, .. } => {
                    let Some(button) = mouse_button(button) else {
                        continue;
                    };
                    let actions = self.app.handle_input(InputEvent::PointerUp { pos, button });
                    self.chrome(actions, None);
                    self.update_cursor(pos);
                }
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    source,
                    ..
                } => {
                    let fingers = matches!(
                        source,
                        Some(wl_pointer::AxisSource::Finger | wl_pointer::AxisSource::Continuous)
                    );
                    let delta =
                        Point::new(scroll_px(horizontal, fingers), scroll_px(vertical, fingers));
                    if delta != Point::default() {
                        self.app.handle_input(InputEvent::Scroll { pos, delta });
                    }
                }
            }
        }
        self.after_app_change();
    }
}

fn mouse_button(code: u32) -> Option<MouseButton> {
    match code {
        BTN_LEFT => Some(MouseButton::Left),
        BTN_RIGHT => Some(MouseButton::Right),
        BTN_MIDDLE => Some(MouseButton::Middle),
        _ => None,
    }
}

impl OutputHandler for Gui {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Gui {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Gui {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

impl Dispatch<WpFractionalScaleV1, ()> for Gui {
    fn event(
        gui: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            // The scale is sent as a numerator over 120.
            gui.scale = f64::from(scale) / 120.0;
            gui.fractional_active = gui.viewport.is_some();
            gui.resized = true;
        }
    }
}

/// Logical pixels to scroll for one axis. Wheels scroll 40 px per detent;
/// touchpads report pixels directly.
fn scroll_px(axis: AxisScroll, fingers: bool) -> f32 {
    if fingers {
        axis.absolute as f32
    } else if axis.value120 != 0 {
        axis.value120 as f32 / 120.0 * 40.0
    } else if axis.discrete != 0 {
        axis.discrete as f32 * 40.0
    } else {
        axis.absolute as f32
    }
}

/// The app's key names (see `dawkey_ui::InputEvent::Key`).
fn key_name(event: &KeyEvent) -> Option<String> {
    let named = match event.keysym {
        Keysym::space => "space",
        Keysym::Return | Keysym::KP_Enter => "return",
        Keysym::Escape => "escape",
        Keysym::Tab | Keysym::ISO_Left_Tab => "tab",
        Keysym::Delete | Keysym::KP_Delete => "delete",
        Keysym::BackSpace => "backspace",
        Keysym::Home | Keysym::KP_Home => "home",
        Keysym::End | Keysym::KP_End => "end",
        Keysym::Left => "left",
        Keysym::Right => "right",
        Keysym::Up => "up",
        Keysym::Down => "down",
        Keysym::F2 => "f2",
        Keysym::KP_Add => "+",
        Keysym::KP_Subtract => "-",
        keysym => {
            let c = keysym.key_char()?;
            if c.is_control() {
                return None;
            }
            return Some(c.to_lowercase().collect());
        }
    };
    Some(named.to_owned())
}

impl KeyboardHandler for Gui {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        self.app
            .handle_input(InputEvent::Modifiers(Modifiers::default()));
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(event);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(event);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        modifiers: KeyboardModifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.app.handle_input(InputEvent::Modifiers(Modifiers {
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            alt: modifiers.alt,
            logo: modifiers.logo,
        }));
    }
}

const URI_LIST: &str = "text/uri-list";

impl Gui {
    fn drag_offer(&self, device: &WlDataDevice) -> Option<DragOffer> {
        self.data_devices
            .iter()
            .find(|d| d.inner() == device)?
            .data()
            .drag_offer()
    }
}

impl DataDeviceHandler for Gui {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        x: f64,
        y: f64,
        _: &wl_surface::WlSurface,
    ) {
        self.drop_pos = Point::new(x as f32, y as f32);
        let Some(offer) = self.drag_offer(device) else {
            return;
        };
        let files = offer.with_mime_types(|types| types.iter().any(|t| t == URI_LIST));
        offer.accept_mime_type(offer.serial, files.then(|| URI_LIST.to_owned()));
        if files {
            offer.set_actions(DndAction::Copy, DndAction::Copy);
        } else {
            offer.set_actions(DndAction::empty(), DndAction::empty());
        }
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, x: f64, y: f64) {
        self.drop_pos = Point::new(x as f32, y as f32);
    }

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        let Some(offer) = self.drag_offer(device) else {
            return;
        };
        if !offer.with_mime_types(|types| types.iter().any(|t| t == URI_LIST)) {
            offer.finish();
            offer.destroy();
            return;
        }
        let pipe = match offer.receive(URI_LIST.to_owned()) {
            Ok(pipe) => pipe,
            Err(e) => {
                tracing::warn!("reading dropped files: {e}");
                offer.destroy();
                return;
            }
        };
        let mut data = Vec::new();
        // Read on the event loop as the source writes; never block here.
        let inserted = self
            .loop_handle
            .insert_source(pipe, move |_, file, gui: &mut Gui| {
                use std::io::Read;
                // SAFETY: we only read; the file descriptor stays owned by the pipe.
                let file: &mut std::fs::File = unsafe { file.get_mut() };
                let mut chunk = [0u8; 4096];
                match file.read(&mut chunk) {
                    Ok(0) => {
                        offer.finish();
                        offer.destroy();
                        let paths = dialogs::parse_uri_list(&data);
                        if !paths.is_empty() {
                            gui.app.drop_files(paths, gui.drop_pos);
                            gui.after_app_change();
                        }
                        PostAction::Remove
                    }
                    Ok(n) => {
                        data.extend_from_slice(&chunk[..n]);
                        PostAction::Continue
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => PostAction::Continue,
                    Err(e) => {
                        tracing::warn!("reading dropped files: {e}");
                        offer.finish();
                        offer.destroy();
                        PostAction::Remove
                    }
                }
            });
        if let Err(e) = inserted {
            tracing::warn!("reading dropped files: {e}");
        }
    }
}

impl DataOfferHandler for Gui {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        offer: &mut DragOffer,
        _: DndAction,
    ) {
        offer.set_actions(DndAction::Copy, DndAction::Copy);
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

wayland_client::delegate_noop!(Gui: ignore WpFractionalScaleManagerV1);
wayland_client::delegate_noop!(Gui: ignore WpViewporter);
wayland_client::delegate_noop!(Gui: ignore WpViewport);

delegate_registry!(Gui);
smithay_client_toolkit::delegate_dispatch2!(Gui);
