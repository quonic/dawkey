// SPDX-License-Identifier: AGPL-3.0-or-later
//! Loading a `.vst3` bundle's binary and its plugin factory.

use std::collections::HashMap;
use std::ffi::{CString, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper};

use super::host::HostContext;
use crate::PluginError;

/// A loaded `.vst3` binary and its factory. Modules stay loaded for the
/// life of the process (like Gtk and Qt UI libraries): their runtimes (the
/// D runtime, JUCE's message thread) leave threads behind that unloading
/// would pull the code out from under.
pub(crate) struct Module {
    pub(crate) factory: ComPtr<IPluginFactory>,
    /// The factory's host context: the host's name, nothing per instance.
    _context: Option<ComWrapper<HostContext>>,
}

// SAFETY: the factory is only called on the control thread that loads
// plugins; the registry hands the same module to every instance of the
// bundle, which all live on that thread too. (VST3 factories are
// thread-safe in any case: hosts scan from worker threads.)
unsafe impl Send for Module {}
unsafe impl Sync for Module {}

/// Loaded modules, by binary.
static MODULES: Mutex<Option<HashMap<PathBuf, Arc<Module>>>> = Mutex::new(None);

type ModuleEntry = unsafe extern "C" fn(*mut c_void) -> bool;
type GetPluginFactory = unsafe extern "C" fn() -> *mut IPluginFactory;

/// The module of `bundle` (loading it the first time).
pub(crate) fn load(bundle: &Path) -> Result<Arc<Module>, PluginError> {
    let binary = super::bundle_binary(bundle);
    let mut modules = MODULES.lock().unwrap_or_else(|e| e.into_inner());
    let modules = modules.get_or_insert_with(HashMap::new);
    if let Some(module) = modules.get(&binary) {
        return Ok(Arc::clone(module));
    }
    let module = Arc::new(open(&binary)?);
    modules.insert(binary, Arc::clone(&module));
    Ok(module)
}

fn open(binary: &Path) -> Result<Module, PluginError> {
    let error = |what: &str| PluginError::new(format!("{}: {what}", binary.display()));
    let path =
        CString::new(binary.as_os_str().as_encoded_bytes()).map_err(|_| error("bad path"))?;
    // SAFETY: loading a plugin library runs its initialization code; that's
    // the risk every plugin host takes (and why scans run in a subprocess).
    let library = unsafe {
        libc::dlopen(
            path.as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL | libc::RTLD_NODELETE,
        )
    };
    if library.is_null() {
        // SAFETY: dlerror returns a C string (or null) owned by libc.
        let message = unsafe { libc::dlerror() };
        let message = if message.is_null() {
            "it can't be loaded".to_owned()
        } else {
            // SAFETY: non-null, NUL-terminated, valid until the next dl call.
            unsafe { std::ffi::CStr::from_ptr(message) }
                .to_string_lossy()
                .into_owned()
        };
        return Err(error(&message));
    }
    // SAFETY: `library` is a handle dlopen gave, and the names are C strings.
    let (entry, get_factory) = unsafe {
        (
            libc::dlsym(library, c"ModuleEntry".as_ptr()),
            libc::dlsym(library, c"GetPluginFactory".as_ptr()),
        )
    };
    if get_factory.is_null() {
        return Err(error("it has no GetPluginFactory (not a VST3 plugin?)"));
    }
    if !entry.is_null() {
        // SAFETY: a VST3 module's ModuleEntry takes the library handle and
        // returns whether it set itself up; it's called once per load.
        let entry: ModuleEntry = unsafe { std::mem::transmute(entry) };
        if !unsafe { entry(library) } {
            return Err(error("its ModuleEntry failed"));
        }
    }
    // SAFETY: GetPluginFactory has this signature in every VST3 module; it
    // returns an owned reference to the factory (or null).
    let get_factory: GetPluginFactory = unsafe { std::mem::transmute(get_factory) };
    let factory = unsafe { ComPtr::from_raw(get_factory()) }
        .ok_or_else(|| error("it has no plugin factory"))?;
    let context = factory.cast::<IPluginFactory3>().map(|factory3| {
        let context = ComWrapper::new(HostContext::new(None));
        let unknown = context.to_com_ptr::<FUnknown>().expect("it's an FUnknown");
        // SAFETY: the context outlives the module (kept beside it).
        unsafe { factory3.setHostContext(unknown.as_ptr()) };
        context
    });
    Ok(Module {
        factory,
        _context: context,
    })
}
