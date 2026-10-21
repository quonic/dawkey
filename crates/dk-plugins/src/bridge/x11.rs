// SPDX-License-Identifier: AGPL-3.0-or-later
//! The plugin host process's windows for plugin GUIs: plain X11 top-level
//! windows (through XWayland on a Wayland desktop) that a plugin's GUI is
//! embedded in. This is the only place Dawkey uses X11.

use std::os::fd::{AsRawFd, RawFd};

use x11rb::connection::Connection;
use x11rb::properties::WmSizeHints;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux,
    EventMask, PropMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// What happened to a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowEvent {
    /// The user closed it.
    Closed,
    /// It has this size now.
    Resized(u32, u32),
}

/// A connection to the X server.
pub(crate) struct X11 {
    conn: RustConnection,
    root: u32,
    black: u32,
    wm_protocols: u32,
    wm_delete_window: u32,
    net_wm_name: u32,
    utf8_string: u32,
}

impl std::fmt::Debug for X11 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11").finish_non_exhaustive()
    }
}

/// Anything going wrong talking to the X server.
fn failed(e: impl std::fmt::Display) -> String {
    format!("X11: {e}")
}

impl X11 {
    /// Connects to `$DISPLAY` (XWayland's, on a Wayland desktop).
    pub(crate) fn connect() -> Result<Self, String> {
        let (conn, screen) = RustConnection::connect(None)
            .map_err(|e| format!("no X11 display for plugin windows ({e})"))?;
        let (root, black) = {
            let screen = &conn.setup().roots[screen];
            (screen.root, screen.black_pixel)
        };
        let atom = |name: &[u8]| -> Result<u32, String> {
            Ok(conn
                .intern_atom(false, name)
                .map_err(failed)?
                .reply()
                .map_err(failed)?
                .atom)
        };
        Ok(Self {
            wm_protocols: atom(b"WM_PROTOCOLS")?,
            wm_delete_window: atom(b"WM_DELETE_WINDOW")?,
            net_wm_name: atom(b"_NET_WM_NAME")?,
            utf8_string: atom(b"UTF8_STRING")?,
            conn,
            root,
            black,
        })
    }

    /// The connection's descriptor, to poll.
    pub(crate) fn fd(&self) -> RawFd {
        self.conn.stream().as_raw_fd()
    }

    /// A new top-level window titled `title`, `width` by `height`, shown.
    /// One that can't be resized keeps its size.
    pub(crate) fn create_window(
        &self,
        title: &str,
        width: u32,
        height: u32,
        resizable: bool,
    ) -> Result<u32, String> {
        let conn = &self.conn;
        let window = conn.generate_id().map_err(failed)?;
        let (w, h) = (
            width.clamp(1, 16_384) as u16,
            height.clamp(1, 16_384) as u16,
        );
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            self.root,
            0,
            0,
            w,
            h,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(self.black)
                .event_mask(EventMask::STRUCTURE_NOTIFY),
        )
        .map_err(failed)?;
        // WM_NAME is Latin-1; window managers show _NET_WM_NAME (UTF-8).
        let latin1: Vec<u8> = title
            .chars()
            .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
            .collect();
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            &latin1,
        )
        .map_err(failed)?;
        conn.change_property8(
            PropMode::REPLACE,
            window,
            self.net_wm_name,
            self.utf8_string,
            title.as_bytes(),
        )
        .map_err(failed)?;
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"dawkey-plugin\0Dawkey\0",
        )
        .map_err(failed)?;
        conn.change_property32(
            PropMode::REPLACE,
            window,
            self.wm_protocols,
            AtomEnum::ATOM,
            &[self.wm_delete_window],
        )
        .map_err(failed)?;
        self.set_size_hints(window, width, height, resizable)?;
        conn.map_window(window).map_err(failed)?;
        conn.flush().map_err(failed)?;
        Ok(window)
    }

    fn set_size_hints(
        &self,
        window: u32,
        width: u32,
        height: u32,
        resizable: bool,
    ) -> Result<(), String> {
        let mut hints = WmSizeHints::new();
        if !resizable {
            let size = (width as i32, height as i32);
            hints.min_size = Some(size);
            hints.max_size = Some(size);
        }
        hints.set_normal_hints(&self.conn, window).map_err(failed)?;
        Ok(())
    }

    /// Resizes `window` (as the plugin asked).
    pub(crate) fn resize(
        &self,
        window: u32,
        width: u32,
        height: u32,
        resizable: bool,
    ) -> Result<(), String> {
        self.set_size_hints(window, width, height, resizable)?;
        self.conn
            .configure_window(
                window,
                &ConfigureWindowAux::new().width(width).height(height),
            )
            .map_err(failed)?;
        self.conn.flush().map_err(failed)?;
        Ok(())
    }

    /// The size of the window a GUI made in `window`, if it made one (for
    /// GUIs that leave sizing to the host, as DPF's LV2 UIs do).
    pub(crate) fn child_size(&self, window: u32) -> Option<(u32, u32)> {
        let tree = self.conn.query_tree(window).ok()?.reply().ok()?;
        let child = *tree.children.first()?;
        let geometry = self.conn.get_geometry(child).ok()?.reply().ok()?;
        let size = (u32::from(geometry.width), u32::from(geometry.height));
        (size.0 > 1 && size.1 > 1).then_some(size)
    }

    pub(crate) fn destroy(&self, window: u32) {
        let _ = self.conn.destroy_window(window);
        let _ = self.conn.flush();
    }

    /// What happened to the windows since the last call, by window.
    pub(crate) fn events(&self) -> Result<Vec<(u32, WindowEvent)>, String> {
        let mut events = Vec::new();
        while let Some(event) = self.conn.poll_for_event().map_err(failed)? {
            match event {
                Event::ClientMessage(ClientMessageEvent {
                    window,
                    type_,
                    data,
                    ..
                }) if type_ == self.wm_protocols
                    && data.as_data32()[0] == self.wm_delete_window =>
                {
                    events.push((window, WindowEvent::Closed));
                }
                Event::ConfigureNotify(e) => {
                    let resized = WindowEvent::Resized(u32::from(e.width), u32::from(e.height));
                    events.push((e.window, resized));
                }
                _ => {}
            }
        }
        Ok(events)
    }
}
