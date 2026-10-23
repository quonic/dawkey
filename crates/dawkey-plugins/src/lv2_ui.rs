// SPDX-License-Identifier: AGPL-3.0-or-later
//! LV2 plugin UIs in the plugin host process: `ui:X11UI` embedded in an
//! X11 window the host process made (`ui:parent`), `ui:GtkUI` and
//! `ui:Gtk3UI` in a Gtk window of their own (see [`crate::gtk`]), or
//! `ui:Qt5UI` and `ui:Qt6UI` in a Qt window of their own (see
//! [`crate::qt`]); loaded from their bundle and run on its main thread
//! (`ui:idleInterface`, and Gtk's or Qt's events). Control
//! values (the parameters', and the output ports' the plugin sets, like
//! meters) reach the UI as port events; what the UI writes to a control
//! port (and its touches, `ui:touch`) becomes a [`ParamChange`], which
//! Dawkey records and sends back to the plugin like any change of its own.
//! Atoms go both ways (`atom:eventTransfer`): what the UI sends the plugin
//! (`patch:Set` for a file, say) and what the plugin sends on its atom
//! outputs.

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use lv2_raw::core::LV2Feature;

use crate::{ParamChange, PluginError};

const UI_PARENT: &CStr = c"http://lv2plug.in/ns/extensions/ui#parent";
const UI_RESIZE: &CStr = c"http://lv2plug.in/ns/extensions/ui#resize";
const UI_IDLE: &CStr = c"http://lv2plug.in/ns/extensions/ui#idleInterface";
const UI_TOUCH: &CStr = c"http://lv2plug.in/ns/extensions/ui#touch";
const INSTANCE_ACCESS: &CStr = c"http://lv2plug.in/ns/ext/instance-access";
const DATA_ACCESS: &CStr = c"http://lv2plug.in/ns/ext/data-access";
const UI_PORT_MAP: &CStr = c"http://lv2plug.in/ns/extensions/ui#portMap";
/// `LV2UI_INVALID_PORT_INDEX`.
const INVALID_PORT: u32 = u32::MAX;

/// The plugin's `extension_data` (for `data-access`).
pub(crate) type ExtensionData = unsafe extern "C" fn(*const c_char) -> *const c_void;

/// Where a plugin's UI is, found when the plugin loads.
#[derive(Debug, Clone)]
pub(crate) struct UiInfo {
    pub(crate) uri: CString,
    pub(crate) plugin_uri: CString,
    pub(crate) binary: PathBuf,
    pub(crate) bundle: PathBuf,
    pub(crate) toolkit: Toolkit,
}

/// What a UI is made with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Toolkit {
    /// Plain X11: it goes in a window the host makes.
    X11,
    Gtk(crate::gtk::Version),
    Qt(crate::qt::Version),
}

/// The toolkit a UI's window of its own is made with, loaded.
#[derive(Debug, Clone, Copy)]
enum Loaded {
    Gtk(crate::gtk::Gtk),
    Qt(crate::qt::Qt),
}

/// A UI's window of its own.
#[derive(Debug)]
enum Window {
    Gtk(crate::gtk::Window),
    Qt(crate::qt::Window),
}

impl Window {
    /// Runs the toolkit's events; whether the user closed the window.
    fn idle(&self) -> bool {
        match self {
            Self::Gtk(window) => {
                window.gtk().iterate();
                window.closed()
            }
            Self::Qt(window) => {
                window.qt().process_events();
                window.closed()
            }
        }
    }
}

/// `LV2UI_Descriptor`.
#[repr(C)]
struct UiDescriptor {
    uri: *const c_char,
    instantiate: Option<
        unsafe extern "C" fn(
            *const UiDescriptor,
            *const c_char,
            *const c_char,
            WriteFn,
            *mut c_void,
            *mut *mut c_void,
            *const *const LV2Feature,
        ) -> *mut c_void,
    >,
    cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
    port_event: Option<unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void)>,
    extension_data: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
}

type WriteFn = unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void);

#[repr(C)]
struct Resize {
    handle: *mut c_void,
    ui_resize: unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int,
}

#[repr(C)]
struct Touch {
    handle: *mut c_void,
    touch: unsafe extern "C" fn(*mut c_void, u32, bool),
}

#[repr(C)]
struct IdleInterface {
    idle: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
}

#[repr(C)]
struct DataAccess {
    data_access: Option<ExtensionData>,
}

#[repr(C)]
struct PortMap {
    handle: *mut c_void,
    port_index: unsafe extern "C" fn(*mut c_void, *const c_char) -> u32,
}

/// Where a UI goes.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Place<'a> {
    /// In this X11 window (X11 UIs).
    In(u32),
    /// In a window of its own with this title (Gtk UIs).
    Own(&'a str),
}

/// The plugin's ports, as its UI sees them.
#[derive(Debug, Clone, Default)]
pub(crate) struct UiPorts {
    /// The ports of its parameters, in order.
    pub(crate) params: Vec<u32>,
    /// Its output control ports.
    pub(crate) outputs: Vec<u32>,
    /// Its atom inputs (the UI may send them atoms).
    pub(crate) atom_inputs: Vec<u32>,
    /// Every port's symbol, by index.
    pub(crate) symbols: Vec<CString>,
}

/// The URIDs the UI's messages are read with.
#[derive(Debug, Clone, Copy)]
pub(crate) struct UiUrids {
    pub(crate) event_transfer: u32,
    pub(crate) atom_transfer: u32,
    /// `atom:Object` (and the older `atom:Blank` and `atom:Resource`).
    pub(crate) objects: [u32; 3],
    /// The `patch:` messages that change something (`Set`, `Put`, ...).
    pub(crate) patch_changes: [u32; 7],
}

impl UiUrids {
    /// Whether `atom` (header and body) is a patch message that changes
    /// something, as a UI sends to set a plugin's file or other property.
    pub(crate) fn changes(&self, atom: &[u8]) -> bool {
        let word = |at: usize| {
            atom.get(at..at + 4)
                .map(|w| u32::from_ne_bytes(w.try_into().expect("4 bytes")))
        };
        // An object's body starts with its ID and its type.
        word(4).is_some_and(|kind| self.objects.contains(&kind))
            && word(12).is_some_and(|otype| self.patch_changes.contains(&otype))
    }
}

/// What the UI tells the host, behind the handles its callbacks get.
struct Controller {
    /// Port index → parameter index.
    params: Vec<(u32, usize)>,
    atom_inputs: Vec<u32>,
    symbols: Vec<CString>,
    urids: UiUrids,
    changes: RefCell<Vec<ParamChange>>,
    /// Atoms (header and body) for the plugin, by port.
    messages: RefCell<Vec<(u32, Vec<u8>)>>,
    /// Whether one of them changes the plugin's state.
    changed_state: Cell<bool>,
    resize: Cell<Option<(u32, u32)>>,
}

impl Controller {
    fn param(&self, port: u32) -> Option<usize> {
        self.params
            .iter()
            .find(|(p, _)| *p == port)
            .map(|&(_, i)| i)
    }
}

unsafe extern "C" fn write_port(
    controller: *mut c_void,
    port: u32,
    size: u32,
    protocol: u32,
    buffer: *const c_void,
) {
    // SAFETY: `controller` is the `Controller` given at instantiation, which
    // outlives the UI; a float protocol write has 4 bytes at `buffer`.
    unsafe {
        let controller = &*controller.cast::<Controller>();
        let urids = &controller.urids;
        if buffer.is_null() {
            return;
        }
        if protocol == urids.event_transfer || protocol == urids.atom_transfer {
            // An atom: its header says its body's size.
            let bytes = std::slice::from_raw_parts(buffer.cast::<u8>(), size as usize);
            let body = bytes
                .get(..4)
                .map(|w| u32::from_ne_bytes(w.try_into().expect("4 bytes")) as usize);
            if body.is_some_and(|body| 8 + body <= bytes.len())
                && controller.atom_inputs.contains(&port)
            {
                let atom = &bytes[..8 + body.unwrap_or(0)];
                if urids.changes(atom) {
                    controller.changed_state.set(true);
                }
                controller.messages.borrow_mut().push((port, atom.to_vec()));
            }
            return;
        }
        if protocol != 0 || size != 4 {
            return;
        }
        let value = f64::from(buffer.cast::<f32>().read_unaligned());
        if let Some(index) = controller.param(port) {
            controller
                .changes
                .borrow_mut()
                .push(ParamChange::Value { index, value });
        }
    }
}

unsafe extern "C" fn touch(handle: *mut c_void, port: u32, grabbed: bool) {
    // SAFETY: as for `write_port`.
    let controller = unsafe { &*handle.cast::<Controller>() };
    if let Some(index) = controller.param(port) {
        controller.changes.borrow_mut().push(if grabbed {
            ParamChange::Begin { index }
        } else {
            ParamChange::End { index }
        });
    }
}

unsafe extern "C" fn port_index(handle: *mut c_void, symbol: *const c_char) -> u32 {
    if symbol.is_null() {
        return INVALID_PORT;
    }
    // SAFETY: as for `write_port`; `symbol` is a NUL-terminated string from
    // the UI.
    let (controller, symbol) = unsafe { (&*handle.cast::<Controller>(), CStr::from_ptr(symbol)) };
    controller
        .symbols
        .iter()
        .position(|s| s.as_c_str() == symbol)
        .map_or(INVALID_PORT, |i| i as u32)
}

unsafe extern "C" fn ui_resize(handle: *mut c_void, width: c_int, height: c_int) -> c_int {
    // SAFETY: as for `write_port`.
    let controller = unsafe { &*handle.cast::<Controller>() };
    if width > 0 && height > 0 {
        controller.resize.set(Some((width as u32, height as u32)));
    }
    0
}

/// A plugin's X11 UI, open.
pub(crate) struct Lv2Ui {
    library: *mut c_void,
    descriptor: *const UiDescriptor,
    handle: *mut c_void,
    idle: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    controller: Box<Controller>,
    /// What the feature pointers point at, kept for the UI's lifetime.
    _resize: Box<Resize>,
    _touch: Box<Touch>,
    _data_access: Box<DataAccess>,
    _port_map: Box<PortMap>,
    /// A Gtk or Qt UI's window.
    window: Option<Window>,
    /// Its library stays loaded (a Gtk or Qt UI's).
    resident: bool,
    /// The values last sent to each parameter's port, and to each output.
    sent: Vec<Option<f32>>,
    sent_outputs: Vec<Option<f32>>,
    outputs: Vec<u32>,
}

impl std::fmt::Debug for Lv2Ui {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lv2Ui").finish_non_exhaustive()
    }
}

impl Lv2Ui {
    /// Opens `ui` where `place` says (an X11 UI in an X11 window, a Gtk or
    /// Qt UI in a window of its own), for the plugin instance `instance`
    /// (with `extension_data` for data access). `features` are the
    /// plugin's (URID map and unmap, options); `ports` are what it has.
    ///
    /// # Safety
    ///
    /// `instance` and `features` must outlive the UI (it's closed before
    /// the plugin goes), and a window to go in must be a live X11 window.
    pub(crate) unsafe fn open(
        ui: &UiInfo,
        place: Place<'_>,
        instance: *mut c_void,
        extension_data: Option<ExtensionData>,
        features: &[LV2Feature],
        ports: &UiPorts,
        urids: UiUrids,
    ) -> Result<Self, PluginError> {
        let failed = |why: String| PluginError::new(format!("opening its UI failed: {why}"));
        // The toolkit first: the UI makes its widgets as it starts.
        let toolkit = match ui.toolkit {
            Toolkit::X11 => None,
            Toolkit::Gtk(version) => {
                Some(Loaded::Gtk(crate::gtk::Gtk::get(version).map_err(failed)?))
            }
            Toolkit::Qt(version) => Some(Loaded::Qt(crate::qt::Qt::get(version).map_err(failed)?)),
        };
        let parent = match (toolkit, place) {
            (None, Place::In(parent)) => Some(parent),
            (Some(_), Place::Own(_)) => None,
            (None, Place::Own(_)) => {
                return Err(failed("an X11 UI needs a window to go in".into()));
            }
            (Some(_), Place::In(_)) => {
                return Err(failed("a Gtk or Qt UI goes in a window of its own".into()));
            }
        };
        let title = match place {
            Place::Own(title) => title,
            Place::In(_) => "",
        };
        let path = CString::new(ui.binary.as_os_str().as_bytes())
            .map_err(|_| failed("bad library path".into()))?;
        // SAFETY: loading the UI's library runs its constructors, as
        // any LV2 host does.
        let library = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if library.is_null() {
            // SAFETY: dlerror returns a message or null.
            let why = unsafe { libc::dlerror() };
            let why = if why.is_null() {
                "can't load its library".to_owned()
            } else {
                // SAFETY: a NUL-terminated message from dlerror.
                unsafe { CStr::from_ptr(why) }
                    .to_string_lossy()
                    .into_owned()
            };
            return Err(failed(why));
        }
        let close = |e: PluginError| {
            // Gtk and Qt UIs stay loaded (`ui:makeResident`: they register
            // types with GObject, or Qt's).
            if toolkit.is_none() {
                // SAFETY: opened above, not used after this.
                unsafe { libc::dlclose(library) };
            }
            e
        };
        // SAFETY: looking up the LV2 UI entry point by name.
        let entry = unsafe { libc::dlsym(library, c"lv2ui_descriptor".as_ptr()) };
        if entry.is_null() {
            return Err(close(failed("no lv2ui_descriptor".into())));
        }
        // SAFETY: lv2ui_descriptor has this signature.
        let entry: unsafe extern "C" fn(u32) -> *const UiDescriptor =
            unsafe { std::mem::transmute(entry) };
        let mut descriptor = std::ptr::null();
        for index in 0.. {
            // SAFETY: the UI's own entry point; null ends the list.
            let d = unsafe { entry(index) };
            if d.is_null() {
                break;
            }
            // SAFETY: a descriptor from the library, with a NUL-terminated
            // URI.
            if unsafe { CStr::from_ptr((*d).uri) } == ui.uri.as_c_str() {
                descriptor = d;
                break;
            }
        }
        if descriptor.is_null() {
            return Err(close(failed("its library doesn't describe it".into())));
        }
        let controller = Box::new(Controller {
            params: ports
                .params
                .iter()
                .enumerate()
                .map(|(i, &port)| (port, i))
                .collect(),
            atom_inputs: ports.atom_inputs.clone(),
            symbols: ports.symbols.clone(),
            urids,
            changes: RefCell::new(Vec::new()),
            messages: RefCell::new(Vec::new()),
            changed_state: Cell::new(false),
            resize: Cell::new(None),
        });
        let handle = std::ptr::from_ref(&*controller).cast_mut().cast::<c_void>();
        let resize = Box::new(Resize { handle, ui_resize });
        let touch = Box::new(Touch { handle, touch });
        let data_access = Box::new(DataAccess {
            data_access: extension_data,
        });
        let port_map = Box::new(PortMap { handle, port_index });
        let feature = |uri: &CStr, data: *const c_void| LV2Feature {
            uri: uri.as_ptr(),
            data: data.cast_mut(),
        };
        let mut list: Vec<LV2Feature> = features
            .iter()
            .map(|f| LV2Feature {
                uri: f.uri,
                data: f.data,
            })
            .collect();
        list.extend([
            feature(UI_RESIZE, std::ptr::from_ref(&*resize).cast()),
            feature(UI_TOUCH, std::ptr::from_ref(&*touch).cast()),
            feature(UI_IDLE, std::ptr::null()),
            feature(INSTANCE_ACCESS, instance),
            feature(DATA_ACCESS, std::ptr::from_ref(&*data_access).cast()),
            feature(UI_PORT_MAP, std::ptr::from_ref(&*port_map).cast()),
        ]);
        if let Some(parent) = parent {
            list.push(feature(UI_PARENT, parent as usize as *const c_void));
        }
        let pointers: Vec<*const LV2Feature> = list
            .iter()
            .map(std::ptr::from_ref)
            .chain([std::ptr::null()])
            .collect();
        let bundle = CString::new(ui.bundle.as_os_str().as_bytes())
            .map_err(|_| close(failed("bad bundle path".into())))?;
        let mut widget = std::ptr::null_mut();
        // SAFETY: the descriptor's own functions, with the features above
        // (which outlive the UI: they're kept in `Lv2Ui`).
        let ui_handle = unsafe {
            let Some(instantiate) = (*descriptor).instantiate else {
                return Err(close(failed("it can't be instantiated".into())));
            };
            instantiate(
                descriptor,
                ui.plugin_uri.as_ptr(),
                bundle.as_ptr(),
                write_port,
                handle,
                &raw mut widget,
                pointers.as_ptr(),
            )
        };
        if ui_handle.is_null() {
            return Err(close(failed("it didn't start".into())));
        }
        let window = match toolkit {
            Some(_) if widget.is_null() => {
                // SAFETY: the UI's own cleanup, once.
                unsafe {
                    if let Some(cleanup) = (*descriptor).cleanup {
                        cleanup(ui_handle);
                    }
                }
                return Err(close(failed("it made no widget".into())));
            }
            // SAFETY: the widget the UI just made with this toolkit, in no
            // container (with no parent).
            Some(Loaded::Gtk(gtk)) => Some(Window::Gtk(unsafe {
                crate::gtk::Window::new(gtk, widget, title)
            })),
            // SAFETY: as above.
            Some(Loaded::Qt(qt)) => Some(Window::Qt(unsafe {
                crate::qt::Window::new(qt, widget, title)
            })),
            None => None,
        };
        // SAFETY: as above; the idle interface's layout is LV2's.
        let idle = unsafe {
            (*descriptor).extension_data.and_then(|data| {
                let iface = data(UI_IDLE.as_ptr()).cast::<IdleInterface>();
                (!iface.is_null()).then(|| (*iface).idle).flatten()
            })
        };
        Ok(Self {
            library,
            descriptor,
            handle: ui_handle,
            idle,
            sent: vec![None; ports.params.len()],
            sent_outputs: vec![None; ports.outputs.len()],
            outputs: ports.outputs.clone(),
            controller,
            _resize: resize,
            _touch: touch,
            _data_access: data_access,
            _port_map: port_map,
            window,
            resident: toolkit.is_some(),
        })
    }

    /// Runs the UI's main-thread work, and sends it the parameters'
    /// `values` and the `outputs`' that changed, then the `atoms` (port,
    /// and the atom's header and body, 8-byte aligned) the plugin sent.
    /// Returns false once the UI wants to close.
    pub(crate) fn idle(&mut self, values: &[f64], outputs: &[f32], atoms: &[(u32, &[u8])]) -> bool {
        // SAFETY: the descriptor and handle are live until `drop`.
        unsafe {
            if let Some(port_event) = (*self.descriptor).port_event {
                let ports = self.controller.params.iter().map(|&(port, _)| port);
                let values = values.iter().map(|&v| v as f32);
                let controls = (ports.zip(values).zip(self.sent.iter_mut())).chain(
                    self.outputs
                        .iter()
                        .copied()
                        .zip(outputs.iter().copied())
                        .zip(self.sent_outputs.iter_mut()),
                );
                for ((port, value), sent) in controls {
                    if *sent == Some(value) {
                        continue;
                    }
                    *sent = Some(value);
                    port_event(self.handle, port, 4, 0, std::ptr::from_ref(&value).cast());
                }
                let transfer = self.controller.urids.event_transfer;
                for &(port, atom) in atoms {
                    port_event(
                        self.handle,
                        port,
                        atom.len() as u32,
                        transfer,
                        atom.as_ptr().cast(),
                    );
                }
            }
            let open = match self.idle {
                Some(idle) => idle(self.handle) == 0,
                None => true,
            };
            match &self.window {
                Some(window) => !window.idle() && open,
                None => open,
            }
        }
    }

    /// What the UI changed since the last call.
    pub(crate) fn take_changes(&mut self) -> Vec<ParamChange> {
        std::mem::take(&mut *self.controller.changes.borrow_mut())
    }

    /// The atoms the UI sent the plugin since the last call (port, header
    /// and body), and whether one of them changes its state.
    pub(crate) fn take_messages(&mut self) -> (Vec<(u32, Vec<u8>)>, bool) {
        (
            std::mem::take(&mut *self.controller.messages.borrow_mut()),
            self.controller.changed_state.take(),
        )
    }

    /// The size the UI asked for, if it asked since the last call.
    pub(crate) fn take_resize(&mut self) -> Option<(u32, u32)> {
        self.controller.resize.take()
    }
}

impl Drop for Lv2Ui {
    fn drop(&mut self) {
        // SAFETY: cleaned up once, then its library unloaded (nothing of
        // it is used after).
        unsafe {
            // A Qt UI's window is hidden first (its widget is the UI's,
            // which deletes it as it's cleaned up).
            let window = match self.window.take() {
                Some(Window::Qt(window)) => {
                    drop(window);
                    None
                }
                window => window,
            };
            if let Some(cleanup) = (*self.descriptor).cleanup {
                cleanup(self.handle);
            }
            // Then a Gtk UI's window, with its widget.
            drop(window);
            if !self.resident {
                libc::dlclose(self.library);
            }
        }
    }
}
