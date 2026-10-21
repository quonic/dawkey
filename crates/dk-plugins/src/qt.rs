// SPDX-License-Identifier: AGPL-3.0-or-later
//! Qt (5 or 6) for an LV2 plugin's Qt UI, in a window of its own in the
//! plugin host process. Qt has no C interface, so a small C++ shim
//! (`qt/shim.cpp`, built against each Qt by `build.rs`) is embedded in
//! Dawkey and loaded from memory when a Qt UI opens; Dawkey doesn't link
//! Qt, and the UI's own library brings it along. Qt 5 and 6 can't share a
//! process (nor Qt and Gtk, as it happens: the instances of one plugin file
//! share one). Every call is on the process's main thread.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::OnceLock;

/// A Qt major version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Version {
    Qt5,
    Qt6,
}

impl Version {
    /// The shim built against it, if this build has one.
    fn shim(self) -> Option<&'static [u8]> {
        match self {
            #[cfg(dawkey_qt5)]
            Self::Qt5 => Some(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libdawkey-qt5.so"
            ))),
            #[cfg(dawkey_qt6)]
            Self::Qt6 => Some(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/libdawkey-qt6.so"
            ))),
            #[allow(unreachable_patterns, reason = "builds without Qt")]
            _ => None,
        }
    }
}

/// The shim's functions.
#[derive(Clone, Copy)]
pub(crate) struct Qt {
    show: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    process_events: unsafe extern "C" fn(),
    is_open: unsafe extern "C" fn(*mut c_void) -> c_int,
    forget: unsafe extern "C" fn(*mut c_void),
}

impl std::fmt::Debug for Qt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Qt")
    }
}

/// The Qt this process loaded (the first one asked for), or why it
/// couldn't.
static LOADED: OnceLock<(Version, Result<Qt, String>)> = OnceLock::new();

impl Qt {
    /// Qt `version`, its application made (once per process).
    pub(crate) fn get(version: Version) -> Result<Qt, String> {
        let (loaded, qt) = LOADED.get_or_init(|| (version, Self::load(version)));
        if *loaded != version {
            return Err(format!(
                "this process already has {loaded:?}, which can't run beside {version:?}"
            ));
        }
        qt.clone()
    }

    fn load(version: Version) -> Result<Qt, String> {
        let library = load_shim(version)?;
        /// Function `name` of the shim, as an `F` (a function pointer).
        ///
        /// # Safety
        ///
        /// `F` must be the C signature of the function of that name.
        unsafe fn symbol<F: Copy>(library: *mut c_void, name: &CStr) -> Result<F, String> {
            const { assert!(size_of::<F>() == size_of::<*mut c_void>()) };
            // SAFETY: looking a function up by name.
            let f = unsafe { libc::dlsym(library, name.as_ptr()) };
            if f.is_null() {
                return Err(format!("the Qt shim has no {}", name.to_string_lossy()));
            }
            // SAFETY: as the caller promises; the sizes match.
            Ok(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&f) })
        }
        // SAFETY: each type is the C signature in `qt/shim.cpp`.
        let (init, qt) = unsafe {
            let init: unsafe extern "C" fn(*mut c_char, usize) -> c_int =
                symbol(library, c"dawkey_qt_init")?;
            let qt = Qt {
                show: symbol(library, c"dawkey_qt_show")?,
                process_events: symbol(library, c"dawkey_qt_process_events")?,
                is_open: symbol(library, c"dawkey_qt_is_open")?,
                forget: symbol(library, c"dawkey_qt_forget")?,
            };
            (init, qt)
        };
        let mut error = [0 as c_char; 256];
        // SAFETY: the shim's own initialization, with room for a message.
        if unsafe { init(error.as_mut_ptr(), error.len()) } != 0 {
            // SAFETY: the shim wrote a NUL-terminated message.
            let why = unsafe { CStr::from_ptr(error.as_ptr()) };
            return Err(format!("{version:?}: {}", why.to_string_lossy()));
        }
        Ok(qt)
    }

    /// Runs what Qt has to do now, without waiting.
    pub(crate) fn process_events(&self) {
        // SAFETY: on the thread the application was made on.
        unsafe { (self.process_events)() }
    }
}

/// Loads the shim for `version` from memory (it's embedded), with the Qt
/// libraries it needs.
fn load_shim(version: Version) -> Result<*mut c_void, String> {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    let shim = version
        .shim()
        .ok_or_else(|| format!("this Dawkey was built without {version:?} (its headers)"))?;
    // SAFETY: a NUL-terminated name and valid flags.
    let fd = unsafe { libc::memfd_create(c"dawkey-qt".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(format!("memfd_create: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: a new descriptor, ours alone.
    let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    (&file)
        .write_all(shim)
        .map_err(|e| format!("writing the Qt shim: {e}"))?;
    let path = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd())).expect("no NUL");
    // SAFETY: loading the shim runs Qt's constructors, as any Qt program
    // does; it stays loaded for the life of the process.
    let library = unsafe {
        libc::dlopen(
            path.as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL | libc::RTLD_NODELETE,
        )
    };
    if library.is_null() {
        // SAFETY: dlerror returns a message or null.
        let why = unsafe { libc::dlerror() };
        let why = if why.is_null() {
            "can't load it".to_owned()
        } else {
            // SAFETY: a NUL-terminated message from dlerror.
            unsafe { CStr::from_ptr(why) }
                .to_string_lossy()
                .into_owned()
        };
        return Err(format!("{version:?} isn't installed ({why})"));
    }
    Ok(library)
}

/// A UI's Qt widget, shown as a window of its own.
pub(crate) struct Window {
    qt: Qt,
    window: *mut c_void,
}

impl std::fmt::Debug for Window {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Window").finish_non_exhaustive()
    }
}

impl Window {
    /// Shows `widget` (a `QWidget`) as a window titled `title`.
    ///
    /// # Safety
    ///
    /// `widget` must be a live widget of this Qt, with no parent.
    pub(crate) unsafe fn new(qt: Qt, widget: *mut c_void, title: &str) -> Self {
        let title = CString::new(title.replace('\0', "")).unwrap_or_default();
        // SAFETY: as promised; the shim copies the title.
        let window = unsafe { (qt.show)(widget, title.as_ptr()) };
        Self { qt, window }
    }

    pub(crate) fn qt(&self) -> Qt {
        self.qt
    }

    /// Whether the user closed it (or its widget went).
    pub(crate) fn closed(&self) -> bool {
        // SAFETY: the window `new` made, until `drop`.
        unsafe { (self.qt.is_open)(self.window) == 0 }
    }
}

impl Drop for Window {
    /// Hides it (the widget is the UI's, which deletes it).
    fn drop(&mut self) {
        // SAFETY: the window `new` made, forgotten once.
        unsafe { (self.qt.forget)(self.window) };
        self.qt.process_events();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shims this build has load (with the Qt they were built
    /// against) and have what Dawkey calls; making an application needs a
    /// display, so that's left to the GUI checks.
    #[test]
    fn the_embedded_shims_load() {
        for version in [Version::Qt5, Version::Qt6] {
            if version.shim().is_none() {
                continue;
            }
            let library = load_shim(version).unwrap();
            for name in [
                c"dawkey_qt_init",
                c"dawkey_qt_show",
                c"dawkey_qt_process_events",
                c"dawkey_qt_is_open",
                c"dawkey_qt_forget",
            ] {
                // SAFETY: looking a function up by name.
                assert!(
                    !unsafe { libc::dlsym(library, name.as_ptr()) }.is_null(),
                    "{name:?}"
                );
            }
        }
    }
}
