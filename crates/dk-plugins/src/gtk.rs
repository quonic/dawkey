// SPDX-License-Identifier: AGPL-3.0-or-later
//! Just enough Gtk (2 or 3, loaded at run time) to show an LV2 plugin's Gtk
//! UI in a window of its own, in the plugin host process: Dawkey doesn't
//! link Gtk, and the UI's own library brings it along. One process holds
//! one plugin, so one Gtk; Gtk 2 and 3 can't share a process. Every call
//! is on the process's main thread.

use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::OnceLock;

/// A Gtk major version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Version {
    Gtk2,
    Gtk3,
}

impl Version {
    fn library(self) -> &'static CStr {
        match self {
            Self::Gtk2 => c"libgtk-x11-2.0.so.0",
            Self::Gtk3 => c"libgtk-3.so.0",
        }
    }
}

type DeleteEvent = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> c_int;

/// The Gtk functions used, the same in Gtk 2 and 3.
#[derive(Clone, Copy)]
pub(crate) struct Gtk {
    init_check: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) -> c_int,
    window_new: unsafe extern "C" fn(c_int) -> *mut c_void,
    window_set_title: unsafe extern "C" fn(*mut c_void, *const c_char),
    container_add: unsafe extern "C" fn(*mut c_void, *mut c_void),
    widget_show_all: unsafe extern "C" fn(*mut c_void),
    widget_destroy: unsafe extern "C" fn(*mut c_void),
    signal_connect_data: unsafe extern "C" fn(
        *mut c_void,
        *const c_char,
        DeleteEvent,
        *mut c_void,
        *const c_void,
        c_int,
    ) -> std::ffi::c_ulong,
    main_context_iteration: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
}

impl std::fmt::Debug for Gtk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gtk")
    }
}

/// The Gtk this process loaded (the first one asked for), or why it
/// couldn't.
static LOADED: OnceLock<(Version, Result<Gtk, String>)> = OnceLock::new();

impl Gtk {
    /// Gtk `version`, loaded and initialized (once per process).
    pub(crate) fn get(version: Version) -> Result<Gtk, String> {
        let (loaded, gtk) = LOADED.get_or_init(|| (version, Self::load(version)));
        if *loaded != version {
            return Err(format!(
                "this process already has {loaded:?}, which can't run beside {version:?}"
            ));
        }
        gtk.clone()
    }

    fn load(version: Version) -> Result<Gtk, String> {
        // SAFETY: loading Gtk runs its constructors, as any Gtk program
        // does; it stays loaded for the life of the process.
        let library = unsafe {
            libc::dlopen(
                version.library().as_ptr(),
                libc::RTLD_NOW | libc::RTLD_GLOBAL | libc::RTLD_NODELETE,
            )
        };
        if library.is_null() {
            return Err(format!("{version:?} isn't installed"));
        }
        /// Function `name`, as an `F` (a function pointer type).
        ///
        /// # Safety
        ///
        /// `F` must be the C signature of the function of that name.
        unsafe fn symbol<F: Copy>(
            library: *mut c_void,
            version: Version,
            name: &CStr,
        ) -> Result<F, String> {
            const { assert!(size_of::<F>() == size_of::<*mut c_void>()) };
            // SAFETY: looking a function up by name (GLib's and GObject's
            // come from Gtk's dependencies).
            let f = unsafe { libc::dlsym(library, name.as_ptr()) };
            if f.is_null() {
                return Err(format!("{version:?} has no {}", name.to_string_lossy()));
            }
            // SAFETY: as the caller promises; the sizes match.
            Ok(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&f) })
        }
        // SAFETY: each field's type is the C signature of the Gtk, GObject
        // or GLib function named.
        let gtk = unsafe {
            Gtk {
                init_check: symbol(library, version, c"gtk_init_check")?,
                window_new: symbol(library, version, c"gtk_window_new")?,
                window_set_title: symbol(library, version, c"gtk_window_set_title")?,
                container_add: symbol(library, version, c"gtk_container_add")?,
                widget_show_all: symbol(library, version, c"gtk_widget_show_all")?,
                widget_destroy: symbol(library, version, c"gtk_widget_destroy")?,
                signal_connect_data: symbol(library, version, c"g_signal_connect_data")?,
                main_context_iteration: symbol(library, version, c"g_main_context_iteration")?,
            }
        };
        // SAFETY: Gtk's own initialization, without arguments.
        if unsafe { (gtk.init_check)(std::ptr::null_mut(), std::ptr::null_mut()) } == 0 {
            return Err(format!("{version:?} found no display to show on"));
        }
        Ok(gtk)
    }

    /// Runs what Gtk has to do now (events, timers, drawing), without
    /// waiting.
    pub(crate) fn iterate(&self) {
        // Bounded, so a UI that keeps scheduling work can't stall the
        // process.
        for _ in 0..256 {
            // SAFETY: the default main context, on the thread Gtk was
            // initialized on.
            if unsafe { (self.main_context_iteration)(std::ptr::null_mut(), 0) } == 0 {
                break;
            }
        }
    }
}

/// A top-level Gtk window holding a UI's widget.
pub(crate) struct Window {
    gtk: Gtk,
    window: *mut c_void,
    /// Set when the user closes it.
    closed: Box<Cell<bool>>,
}

impl std::fmt::Debug for Window {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Window").finish_non_exhaustive()
    }
}

unsafe extern "C" fn delete_event(
    _widget: *mut c_void,
    _event: *mut c_void,
    closed: *mut c_void,
) -> c_int {
    // SAFETY: `closed` is the window's flag, which outlives its signals.
    unsafe { (*closed.cast::<Cell<bool>>()).set(true) };
    // Handled: the window stays until it's destroyed with the UI.
    1
}

impl Window {
    /// Shows `widget` (a `GtkWidget`) in a new window titled `title`.
    ///
    /// # Safety
    ///
    /// `widget` must be a live widget of this Gtk, not in a container yet.
    pub(crate) unsafe fn new(gtk: Gtk, widget: *mut c_void, title: &str) -> Self {
        let title = CString::new(title.replace('\0', "")).unwrap_or_default();
        let closed = Box::new(Cell::new(false));
        // SAFETY: Gtk calls on its thread, with the widget as promised.
        let window = unsafe {
            let window = (gtk.window_new)(0);
            (gtk.window_set_title)(window, title.as_ptr());
            (gtk.container_add)(window, widget);
            (gtk.signal_connect_data)(
                window,
                c"delete-event".as_ptr(),
                delete_event,
                std::ptr::from_ref(&*closed).cast_mut().cast(),
                std::ptr::null(),
                0,
            );
            (gtk.widget_show_all)(window);
            window
        };
        Self {
            gtk,
            window,
            closed,
        }
    }

    pub(crate) fn gtk(&self) -> Gtk {
        self.gtk
    }

    /// Whether the user closed it.
    pub(crate) fn closed(&self) -> bool {
        self.closed.get()
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the window made in `new`, destroyed once.
        unsafe { (self.gtk.widget_destroy)(self.window) };
        self.gtk.iterate();
    }
}
