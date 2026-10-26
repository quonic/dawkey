// SPDX-License-Identifier: AGPL-3.0-or-later
//! LV2 hosting through lilv. Plugins get the features most of them need
//! (URID map and unmap, options, bounded block length, state paths, the
//! worker); ones that require more are reported as unsupported. Atom ports
//! get the buffer size they ask for (`rsz:minimumSize`).

use std::any::Any;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use dawkey_engine::{
    MAX_BLOCK, MidiSink, PluginEvent, PluginEventKind, PluginProcessor, ProcessContext, Transport,
};
use lilv::World;
use lilv::instance::ActiveInstance;
use lilv::plugin::Plugin;
use lv2_raw::core::LV2Feature;

use crate::lv2_ring::{MessageReader, MessageWriter, ring};
use crate::lv2_ui::{Place, Toolkit, UiPorts, UiUrids};
use crate::lv2_worker::Worker;
use crate::{OutputInfo, ParamInfo, PluginError, PluginFormat, PluginInfo, StatePaths, ValueLabel};

const LV2: &str = "http://lv2plug.in/ns/lv2core#";
const ATOM: &str = "http://lv2plug.in/ns/ext/atom#";
const UNITS: &str = "http://lv2plug.in/ns/extensions/units#";
const URID_MAP: &CStr = c"http://lv2plug.in/ns/ext/urid#map";
const URID_UNMAP: &CStr = c"http://lv2plug.in/ns/ext/urid#unmap";
const OPTIONS: &CStr = c"http://lv2plug.in/ns/ext/options#options";
const BOUNDED_BLOCK: &CStr = c"http://lv2plug.in/ns/ext/buf-size#boundedBlockLength";
const MIDI_EVENT: &str = "http://lv2plug.in/ns/ext/midi#MidiEvent";
const STATE_INTERFACE: &CStr = c"http://lv2plug.in/ns/ext/state#interface";
const TIME_POSITION: &str = "http://lv2plug.in/ns/ext/time#Position";
const MINIMUM_SIZE: &str = "http://lv2plug.in/ns/ext/resize-port#minimumSize";
const STATE_CHANGED: &CStr = c"http://lv2plug.in/ns/ext/state#StateChanged";
const PATCH: &str = "http://lv2plug.in/ns/ext/patch#";
/// The start of saved LV2 state (then the properties).
const STATE_MAGIC: &[u8; 8] = b"DKLV2ST1";

const MAP_PATH: &CStr = c"http://lv2plug.in/ns/ext/state#mapPath";
const MAKE_PATH: &CStr = c"http://lv2plug.in/ns/ext/state#makePath";
const FREE_PATH: &CStr = c"http://lv2plug.in/ns/ext/state#freePath";

/// Features a plugin may require that we provide (or that need no data).
const SUPPORTED: &[&str] = &[
    "http://lv2plug.in/ns/ext/urid#map",
    "http://lv2plug.in/ns/ext/urid#unmap",
    "http://lv2plug.in/ns/ext/options#options",
    "http://lv2plug.in/ns/ext/buf-size#boundedBlockLength",
    "http://lv2plug.in/ns/ext/state#mapPath",
    "http://lv2plug.in/ns/ext/state#makePath",
    "http://lv2plug.in/ns/ext/state#freePath",
    "http://lv2plug.in/ns/ext/worker#schedule",
    "http://lv2plug.in/ns/lv2core#hardRTCapable",
    "http://lv2plug.in/ns/lv2core#inPlaceBroken",
    "http://lv2plug.in/ns/lv2core#isLive",
];

/// Bytes of each atom port buffer, at least (plugins may ask for more).
const ATOM_CAPACITY: usize = 16 * 1024;
/// Bytes of an atom port buffer, at most.
const ATOM_CAPACITY_MAX: usize = 64 * 1024 * 1024;
/// Bytes of the rings between a plugin and its UI, at least.
const UI_RING: usize = 256 * 1024;
/// The sample rate the catalog gives the bounds and default of ports with
/// `lv2:sampleRate` at (their values are absolute, so a frequency stays the
/// same at any rate; only the bounds move).
const NOMINAL_RATE: f32 = 48_000.0;

/// The URID map shared by every LV2 plugin in the process.
#[derive(Default)]
struct Urids {
    inner: Mutex<(HashMap<CString, u32>, Vec<CString>)>,
}

impl Urids {
    fn map(&self, uri: &CStr) -> u32 {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let (ids, uris) = &mut *guard;
        if let Some(&id) = ids.get(uri) {
            return id;
        }
        uris.push(uri.to_owned());
        let id = uris.len() as u32;
        ids.insert(uri.to_owned(), id);
        id
    }

    /// The URI of `id`, valid as long as the map lives (entries are never
    /// removed, and `CString`s don't move their bytes).
    fn unmap(&self, id: u32) -> *const c_char {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        id.checked_sub(1)
            .and_then(|i| guard.1.get(i as usize))
            .map_or(std::ptr::null(), |s| s.as_ptr())
    }
}

#[repr(C)]
struct UridMap {
    handle: *mut c_void,
    map: unsafe extern "C" fn(*mut c_void, *const c_char) -> u32,
}

#[repr(C)]
struct UridUnmap {
    handle: *mut c_void,
    unmap: unsafe extern "C" fn(*mut c_void, u32) -> *const c_char,
}

#[repr(C)]
struct LvOption {
    context: u32,
    subject: u32,
    key: u32,
    size: u32,
    kind: u32,
    value: *const c_void,
}

unsafe extern "C" fn urid_map(handle: *mut c_void, uri: *const c_char) -> u32 {
    if uri.is_null() {
        return 0;
    }
    // SAFETY: `handle` is the `Urids` in `Features`, which outlives every
    // plugin given it; `uri` is a NUL-terminated string from the plugin.
    unsafe { (*handle.cast::<Urids>()).map(CStr::from_ptr(uri)) }
}

unsafe extern "C" fn urid_unmap(handle: *mut c_void, id: u32) -> *const c_char {
    // SAFETY: as for `urid_map`.
    unsafe { (*handle.cast::<Urids>()).unmap(id) }
}

/// Everything the features point at. Boxed and never moved, so the
/// pointers plugins keep stay valid; shared by every instance at one
/// sample rate.
#[expect(
    dead_code,
    reason = "most fields are only read by plugins, through the feature pointers"
)]
struct Features {
    urids: Box<Urids>,
    map: Box<UridMap>,
    unmap: Box<UridUnmap>,
    /// min, max and nominal block length; sample rate.
    ints: Box<[i32; 3]>,
    rate: Box<f32>,
    options: Box<[LvOption; 5]>,
    list: Box<[LV2Feature; 4]>,
}

// The raw pointers point into the boxes above, which only the plugins use.
unsafe impl Send for Features {}
unsafe impl Sync for Features {}

impl Features {
    fn new(sample_rate: u32) -> Arc<Self> {
        let urids = Box::new(Urids::default());
        let handle = std::ptr::from_ref(&*urids).cast_mut().cast::<c_void>();
        let map = Box::new(UridMap {
            handle,
            map: urid_map,
        });
        let unmap = Box::new(UridUnmap {
            handle,
            unmap: urid_unmap,
        });
        let ints = Box::new([1, MAX_BLOCK as i32, 512]);
        let rate = Box::new(sample_rate as f32);
        let id = |uri: &CStr| urids.map(uri);
        let atom_int = id(c"http://lv2plug.in/ns/ext/atom#Int");
        let atom_float = id(c"http://lv2plug.in/ns/ext/atom#Float");
        let int = |key: &CStr, i: usize| LvOption {
            context: 0,
            subject: 0,
            key: id(key),
            size: 4,
            kind: atom_int,
            value: std::ptr::from_ref(&ints[i]).cast(),
        };
        let options = Box::new([
            int(c"http://lv2plug.in/ns/ext/buf-size#minBlockLength", 0),
            int(c"http://lv2plug.in/ns/ext/buf-size#maxBlockLength", 1),
            int(c"http://lv2plug.in/ns/ext/buf-size#nominalBlockLength", 2),
            LvOption {
                context: 0,
                subject: 0,
                key: id(c"http://lv2plug.in/ns/ext/parameters#sampleRate"),
                size: 4,
                kind: atom_float,
                value: std::ptr::from_ref(&*rate).cast(),
            },
            // Terminator.
            LvOption {
                context: 0,
                subject: 0,
                key: 0,
                size: 0,
                kind: 0,
                value: std::ptr::null(),
            },
        ]);
        let feature = |uri: &'static CStr, data: *const c_void| LV2Feature {
            uri: uri.as_ptr(),
            data: data.cast_mut(),
        };
        let list = Box::new([
            feature(URID_MAP, std::ptr::from_ref(&*map).cast()),
            feature(URID_UNMAP, std::ptr::from_ref(&*unmap).cast()),
            feature(OPTIONS, options.as_ptr().cast()),
            feature(BOUNDED_BLOCK, std::ptr::null()),
        ]);
        Arc::new(Self {
            urids,
            map,
            unmap,
            ints,
            rate,
            options,
            list,
        })
    }
}

/// `LV2_State_Map_Path`.
#[repr(C)]
struct MapPath {
    handle: *mut c_void,
    abstract_path: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char,
    absolute_path: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char,
}

/// `LV2_State_Make_Path`.
#[repr(C)]
struct MakePath {
    handle: *mut c_void,
    path: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char,
}

/// `LV2_State_Free_Path`.
#[repr(C)]
struct FreePath {
    handle: *mut c_void,
    free_path: unsafe extern "C" fn(*mut c_void, *mut c_char),
}

/// One instance's path features (`state:mapPath`, `makePath`,
/// `freePath`), for instantiating, saving and restoring. Boxed and never
/// moved, so the pointers stay valid while the plugin lives; the paths
/// change when the project moves.
#[expect(
    dead_code,
    reason = "the structs are only read by plugins, through the feature pointers"
)]
struct PathFeatures {
    paths: Box<Mutex<StatePaths>>,
    map: Box<MapPath>,
    make: Box<MakePath>,
    free: Box<FreePath>,
    list: Box<[LV2Feature; 3]>,
}

// The raw pointers point into the boxes above; the paths are behind a lock.
unsafe impl Send for PathFeatures {}
unsafe impl Sync for PathFeatures {}

impl PathFeatures {
    fn new(paths: StatePaths) -> Arc<Self> {
        let paths = Box::new(Mutex::new(paths));
        let handle = std::ptr::from_ref(&*paths).cast_mut().cast::<c_void>();
        let map = Box::new(MapPath {
            handle,
            abstract_path,
            absolute_path,
        });
        let make = Box::new(MakePath {
            handle,
            path: make_path,
        });
        let free = Box::new(FreePath { handle, free_path });
        let feature = |uri: &'static CStr, data: *const c_void| LV2Feature {
            uri: uri.as_ptr(),
            data: data.cast_mut(),
        };
        let list = Box::new([
            feature(MAP_PATH, std::ptr::from_ref(&*map).cast()),
            feature(MAKE_PATH, std::ptr::from_ref(&*make).cast()),
            feature(FREE_PATH, std::ptr::from_ref(&*free).cast()),
        ]);
        Arc::new(Self {
            paths,
            map,
            make,
            free,
            list,
        })
    }

    fn set(&self, paths: &StatePaths) {
        *self.paths.lock().unwrap_or_else(|e| e.into_inner()) = paths.clone();
    }

    /// These features: `makePath` only if there's a folder to make files
    /// in.
    fn list(&self) -> impl Iterator<Item = &LV2Feature> {
        let make = self
            .paths
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .files
            .is_some();
        // `list` is mapPath, makePath, freePath.
        self.list
            .iter()
            .enumerate()
            .filter(move |&(i, _)| make || i != 1)
            .map(|(_, f)| f)
    }

    /// `base`'s features and these, NUL-terminated, as LV2 functions take
    /// them.
    fn with(&self, base: &Features) -> Vec<*const LV2Feature> {
        base.list
            .iter()
            .chain(self.list())
            .map(std::ptr::from_ref)
            .chain([std::ptr::null()])
            .collect()
    }
}

/// The paths behind a path feature's handle.
///
/// # Safety
///
/// `handle` is the `paths` of a live [`PathFeatures`].
unsafe fn state_paths(handle: *mut c_void) -> StatePaths {
    // SAFETY: as the caller says.
    let paths = unsafe { &*handle.cast::<Mutex<StatePaths>>() };
    paths.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// A path from a plugin.
///
/// # Safety
///
/// `path` is NUL-terminated, or null.
unsafe fn plugin_path<'a>(path: *const c_char) -> Option<&'a Path> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: as the caller says.
    (!path.is_null()).then(|| {
        Path::new(std::ffi::OsStr::from_bytes(unsafe {
            CStr::from_ptr(path).to_bytes()
        }))
    })
}

/// `path` for a plugin: allocated with `malloc`, which it frees with
/// `free()` (or `freePath`).
fn to_plugin(path: &Path) -> *mut c_char {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `path` is NUL-terminated; strdup copies it.
    unsafe { libc::strdup(path.as_ptr()) }
}

unsafe extern "C" fn abstract_path(handle: *mut c_void, absolute: *const c_char) -> *mut c_char {
    // SAFETY: the plugin passes our handle and a NUL-terminated path.
    let (paths, Some(path)) = (unsafe { state_paths(handle) }, unsafe {
        plugin_path(absolute)
    }) else {
        return std::ptr::null_mut();
    };
    // A file the plugin made, where it's kept in the project (Dawkey copies
    // it there when saving).
    if let (Some(files), Some(kept), Some(_)) = (&paths.files, &paths.kept, &paths.base)
        && let Ok(rest) = path.strip_prefix(files)
    {
        return to_plugin(&kept.join(rest));
    }
    // Under the project, relative to it, so the project can move.
    let relative = paths
        .base
        .as_deref()
        .and_then(|b| path.strip_prefix(b).ok());
    to_plugin(relative.unwrap_or(path))
}

unsafe extern "C" fn absolute_path(handle: *mut c_void, saved: *const c_char) -> *mut c_char {
    // SAFETY: as above.
    let (paths, Some(path)) = (unsafe { state_paths(handle) }, unsafe {
        plugin_path(saved)
    }) else {
        return std::ptr::null_mut();
    };
    if path.is_absolute() {
        return to_plugin(path);
    }
    match paths.base.as_ref().or(paths.files.as_ref()) {
        Some(base) => to_plugin(&base.join(path)),
        None => to_plugin(path),
    }
}

unsafe extern "C" fn make_path(handle: *mut c_void, path: *const c_char) -> *mut c_char {
    // SAFETY: as above.
    let (paths, Some(path)) = (unsafe { state_paths(handle) }, unsafe { plugin_path(path) }) else {
        return std::ptr::null_mut();
    };
    // Only inside the slot's folder.
    let inside = path
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)));
    let Some(files) = paths.files.filter(|_| inside) else {
        return std::ptr::null_mut();
    };
    let full = files.join(path);
    if let Some(dir) = full.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        tracing::warn!("making {} for a plugin: {e}", dir.display());
        return std::ptr::null_mut();
    }
    to_plugin(&full)
}

unsafe extern "C" fn free_path(_handle: *mut c_void, path: *mut c_char) {
    // SAFETY: `path` came from `to_plugin` (strdup), or is null.
    unsafe { libc::free(path.cast()) }
}

/// `LV2_State_Store_Function`.
type StoreFn = unsafe extern "C" fn(*mut c_void, u32, *const c_void, usize, u32, u32) -> u32;
/// `LV2_State_Retrieve_Function`.
type RetrieveFn =
    unsafe extern "C" fn(*mut c_void, u32, *mut usize, *mut u32, *mut u32) -> *const c_void;

/// `LV2_State_Interface`.
#[repr(C)]
struct StateInterface {
    save: Option<
        unsafe extern "C" fn(
            *mut c_void,
            StoreFn,
            *mut c_void,
            u32,
            *const *const LV2Feature,
        ) -> u32,
    >,
    restore: Option<
        unsafe extern "C" fn(
            *mut c_void,
            RetrieveFn,
            *mut c_void,
            u32,
            *const *const LV2Feature,
        ) -> u32,
    >,
}

/// A property a plugin saved: its key and type as URIs (URIDs differ from
/// run to run), flags, and value.
#[derive(Debug, Clone, PartialEq)]
struct Property {
    key: CString,
    kind: CString,
    flags: u32,
    value: Vec<u8>,
}

/// The properties a plugin declares with `patch:writable` and
/// `patch:readable` in its TTL, with their labels and ranges.
fn declared_properties(world: &World, plugin: &Plugin) -> Vec<crate::PropertyInfo> {
    let label = world.new_uri("http://www.w3.org/2000/01/rdf-schema#label");
    let range = world.new_uri("http://www.w3.org/2000/01/rdf-schema#range");
    let mut found: Vec<crate::PropertyInfo> = Vec::new();
    for (predicate, writable) in [("writable", true), ("readable", false)] {
        let predicate = world.new_uri(&format!("{PATCH}{predicate}"));
        for node in plugin.value(&predicate).iter() {
            let Some(uri) = node.as_uri().map(str::to_owned) else {
                continue;
            };
            if let Some(known) = found.iter_mut().find(|p| p.uri == uri) {
                known.writable |= writable;
                continue;
            }
            let text = |predicate: &lilv::node::Node| {
                world.get(Some(&node), Some(predicate), None).and_then(|n| {
                    n.as_str()
                        .map(str::to_owned)
                        .or_else(|| n.as_uri().map(str::to_owned))
                })
            };
            found.push(crate::PropertyInfo {
                label: text(&label)
                    .unwrap_or_else(|| uri.rsplit(['#', '/']).next().unwrap_or(&uri).to_owned()),
                range: text(&range),
                uri,
                writable,
            });
        }
    }
    found
}

/// A property in an LV2 plugin's saved state, readable.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct StateProperty {
    /// The key's URI.
    pub key: String,
    /// The value's type URI (`http://lv2plug.in/ns/ext/atom#Path`, ...).
    pub kind: String,
    pub value: StateValue,
}

/// The size of an `atom:Literal`'s datatype and language URIDs
/// (`LV2_Atom_Literal_Body`), which come before its text.
const LITERAL_HEADER: usize = 8;

/// A saved property's value, as far as its type is known.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum StateValue {
    /// `atom:String`, `atom:Path`, `atom:URI`, `atom:Literal`.
    Text(String),
    /// `atom:Int`, `atom:Long`.
    Int(i64),
    /// `atom:Float`, `atom:Double`.
    Float(f64),
    /// `atom:Bool`.
    Bool(bool),
    /// Anything else: its size in bytes.
    Bytes { bytes: usize },
}

/// The properties in state [`Lv2Instance::save_state`] saved (`None` if it
/// isn't such state).
pub fn lv2_state_properties(state: &[u8]) -> Option<Vec<StateProperty>> {
    let properties = decode_state(state)?;
    let text = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes.strip_suffix(b"\0").unwrap_or(bytes)).into_owned()
    };
    Some(
        properties
            .iter()
            .map(|p| {
                let kind = p.kind.to_string_lossy().into_owned();
                let value = match kind.strip_prefix(ATOM) {
                    Some("String" | "Path" | "URI") => StateValue::Text(text(&p.value)),
                    // After its datatype and language (`LV2_Atom_Literal_Body`).
                    Some("Literal") if p.value.len() >= LITERAL_HEADER => {
                        StateValue::Text(text(&p.value[LITERAL_HEADER..]))
                    }
                    Some("Int") if p.value.len() == 4 => StateValue::Int(i64::from(
                        i32::from_le_bytes(p.value[..4].try_into().expect("4 bytes")),
                    )),
                    Some("Long") if p.value.len() == 8 => {
                        StateValue::Int(i64::from_le_bytes(p.value[..8].try_into().expect("8")))
                    }
                    Some("Float") if p.value.len() == 4 => StateValue::Float(f64::from(
                        f32::from_le_bytes(p.value[..4].try_into().expect("4 bytes")),
                    )),
                    Some("Double") if p.value.len() == 8 => {
                        StateValue::Float(f64::from_le_bytes(p.value[..8].try_into().expect("8")))
                    }
                    Some("Bool") if p.value.len() == 4 => StateValue::Bool(
                        i32::from_le_bytes(p.value[..4].try_into().expect("4 bytes")) != 0,
                    ),
                    _ => StateValue::Bytes {
                        bytes: p.value.len(),
                    },
                };
                StateProperty {
                    key: p.key.to_string_lossy().into_owned(),
                    kind,
                    value,
                }
            })
            .collect(),
    )
}

/// `state` (saved LV2 state, or `None` for none yet) with property `key`
/// set to `value`, encoded as `kind` (a type URI; `atom:` names like
/// `Path` are allowed): a text type takes a string, `Int`/`Long` a whole
/// number, `Float`/`Double` a number, `Bool` a boolean. Without a kind, an
/// existing property keeps its type, and a new one gets `atom:String` for
/// text (`atom:Path` if it looks like a path), `atom:Int`/`atom:Float` for
/// numbers, `atom:Bool` for booleans. A JSON `null` removes the property.
pub fn lv2_state_set(
    state: Option<&[u8]>,
    key: &str,
    kind: Option<&str>,
    value: &serde_json::Value,
) -> Result<Vec<u8>, String> {
    let mut properties = match state {
        Some(bytes) => decode_state(bytes).ok_or("the plugin's saved state is damaged")?,
        None => Vec::new(),
    };
    let key_c = CString::new(key).map_err(|_| "the key has a NUL in it")?;
    if value.is_null() {
        properties.retain(|p| p.key != key_c);
        return Ok(encode_state(&properties));
    }
    let existing = properties.iter().find(|p| p.key == key_c);
    let kind = match kind {
        Some(k) if k.contains(':') || k.contains('/') => k.to_owned(),
        Some(k) => format!("{ATOM}{k}"),
        None => match existing {
            Some(p) => p.kind.to_string_lossy().into_owned(),
            None => match value {
                serde_json::Value::String(s) if s.starts_with('/') => format!("{ATOM}Path"),
                serde_json::Value::String(_) => format!("{ATOM}String"),
                serde_json::Value::Bool(_) => format!("{ATOM}Bool"),
                serde_json::Value::Number(n) if n.is_i64() || n.is_u64() => format!("{ATOM}Int"),
                serde_json::Value::Number(_) => format!("{ATOM}Float"),
                other => {
                    return Err(format!(
                        "a property value is text, a number or a boolean, not {other}"
                    ));
                }
            },
        },
    };
    let bytes: Vec<u8> = match kind.strip_prefix(ATOM) {
        Some(text @ ("String" | "Path" | "URI" | "Literal")) => {
            // A literal has no datatype or language (both 0) before its text.
            let mut bytes = if text == "Literal" {
                vec![0; LITERAL_HEADER]
            } else {
                Vec::new()
            };
            match value {
                serde_json::Value::String(s) => bytes.extend_from_slice(s.as_bytes()),
                other => bytes.extend_from_slice(other.to_string().as_bytes()),
            }
            bytes.push(0);
            bytes
        }
        Some("Int") => {
            let n = value
                .as_i64()
                .ok_or_else(|| format!("{kind} takes a whole number, not {value}"))?;
            i32::try_from(n)
                .map_err(|_| "out of range for a 32-bit integer")?
                .to_le_bytes()
                .to_vec()
        }
        Some("Long") => value
            .as_i64()
            .ok_or_else(|| format!("{kind} takes a whole number, not {value}"))?
            .to_le_bytes()
            .to_vec(),
        Some("Float") => (value
            .as_f64()
            .ok_or_else(|| format!("{kind} takes a number, not {value}"))?
            as f32)
            .to_le_bytes()
            .to_vec(),
        Some("Double") => value
            .as_f64()
            .ok_or_else(|| format!("{kind} takes a number, not {value}"))?
            .to_le_bytes()
            .to_vec(),
        Some("Bool") => i32::from(
            value
                .as_bool()
                .ok_or_else(|| format!("{kind} takes true or false, not {value}"))?,
        )
        .to_le_bytes()
        .to_vec(),
        _ => {
            return Err(format!(
                "{kind} can't be written from JSON; give a kind such as String, Path, Int, Float \
                 or Bool"
            ));
        }
    };
    let kind_c = CString::new(kind).map_err(|_| "the kind has a NUL in it")?;
    // LV2_STATE_IS_POD; portable too for numbers and text.
    let flags = 0b11;
    match properties.iter_mut().find(|p| p.key == key_c) {
        Some(p) => {
            p.kind = kind_c;
            p.value = bytes;
            p.flags = flags;
        }
        None => properties.push(Property {
            key: key_c,
            kind: kind_c,
            flags,
            value: bytes,
        }),
    }
    Ok(encode_state(&properties))
}

/// Saved state: [`STATE_MAGIC`], a count, then each property's key, type,
/// flags and value (lengths and numbers as little-endian `u32`s).
fn encode_state(properties: &[Property]) -> Vec<u8> {
    let mut out = STATE_MAGIC.to_vec();
    out.extend_from_slice(&(properties.len() as u32).to_le_bytes());
    let mut put = |bytes: &[u8]| {
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    };
    for p in properties {
        put(p.key.as_bytes());
        put(p.kind.as_bytes());
        put(&p.flags.to_le_bytes());
        put(&p.value);
    }
    out
}

fn decode_state(bytes: &[u8]) -> Option<Vec<Property>> {
    struct Cursor<'a>(&'a [u8]);
    impl<'a> Cursor<'a> {
        fn take(&mut self, len: usize) -> Option<&'a [u8]> {
            let (bytes, rest) = self.0.split_at_checked(len)?;
            self.0 = rest;
            Some(bytes)
        }
        fn number(&mut self) -> Option<u32> {
            Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
        }
        fn field(&mut self) -> Option<&'a [u8]> {
            let len = self.number()? as usize;
            self.take(len)
        }
    }
    let mut cursor = Cursor(bytes.strip_prefix(STATE_MAGIC)?);
    let count = cursor.number()?;
    let mut properties = Vec::new();
    for _ in 0..count {
        properties.push(Property {
            key: CString::new(cursor.field()?).ok()?,
            kind: CString::new(cursor.field()?).ok()?,
            flags: u32::from_le_bytes(cursor.field()?.try_into().ok()?),
            value: cursor.field()?.to_vec(),
        });
    }
    Some(properties)
}

/// What a save collects, behind the store callback's handle.
struct Saving<'a> {
    urids: &'a Urids,
    properties: Vec<Property>,
}

unsafe extern "C" fn store(
    handle: *mut c_void,
    key: u32,
    value: *const c_void,
    size: usize,
    kind: u32,
    flags: u32,
) -> u32 {
    // SAFETY: `handle` is the `Saving` that `save_state` passed, and the
    // plugin gives `size` bytes at `value`, valid during this call.
    unsafe {
        let saving = &mut *handle.cast::<Saving>();
        let (key, kind) = (saving.urids.unmap(key), saving.urids.unmap(kind));
        if key.is_null() || kind.is_null() || (value.is_null() && size > 0) {
            // LV2_STATE_ERR_UNKNOWN
            return 1;
        }
        let value = if size == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(value.cast::<u8>(), size).to_vec()
        };
        saving.properties.push(Property {
            key: CStr::from_ptr(key).to_owned(),
            kind: CStr::from_ptr(kind).to_owned(),
            flags,
            value,
        });
    }
    0
}

/// What a restore hands out, behind the retrieve callback's handle: the
/// properties, with their keys and types mapped.
struct Restoring {
    properties: Vec<(u32, u32, Property)>,
}

unsafe extern "C" fn retrieve(
    handle: *mut c_void,
    key: u32,
    size: *mut usize,
    kind: *mut u32,
    flags: *mut u32,
) -> *const c_void {
    // SAFETY: `handle` is the `Restoring` that `restore_state` passed; the
    // out pointers come from the plugin. The value stays valid until the
    // restore returns.
    unsafe {
        let restoring = &*handle.cast::<Restoring>();
        let Some((_, mapped, p)) = restoring.properties.iter().find(|(k, ..)| *k == key) else {
            return std::ptr::null();
        };
        if !size.is_null() {
            *size = p.value.len();
        }
        if !kind.is_null() {
            *kind = *mapped;
        }
        if !flags.is_null() {
            *flags = p.flags;
        }
        p.value.as_ptr().cast()
    }
}

/// The control-thread side of an LV2 instance (whose processor owns it):
/// what saving its state needs.
pub(crate) struct Lv2Instance {
    handle: *mut c_void,
    state: Option<NonNull<StateInterface>>,
    features: Arc<Features>,
    paths: Arc<PathFeatures>,
    /// False once the processor (and with it the plugin) is gone.
    alive: Arc<AtomicBool>,
    /// Its UI, if it has one Dawkey can show; open, if it is.
    ui_info: Option<crate::lv2_ui::UiInfo>,
    ui: Option<crate::lv2_ui::Lv2Ui>,
    /// What its UI is told and tells it, shared with the processor.
    link: Arc<Link>,
    /// The rings to and from the processor, if it has a UI.
    ui_rings: Option<(MessageWriter, MessageReader)>,
    ui_ports: UiPorts,
    ui_urids: UiUrids,
    /// Its `extension_data`, for UIs that ask for data access.
    extension_data: Option<crate::lv2_ui::ExtensionData>,
}

/// What the processor and the instance share, for its UI and its state.
#[derive(Debug)]
struct Link {
    /// The output control ports' values (`f32` bits), in
    /// `Layout::control_out`'s order, as of the last block.
    outputs: Box<[AtomicU32]>,
    /// While the UI is open, what the plugin writes to its atom outputs
    /// goes to it.
    ui_open: AtomicBool,
    /// The plugin said its state changed (`state:StateChanged`), or its UI
    /// sent it a change (`patch:Set`), since it was last saved.
    dirty: AtomicBool,
}

impl Lv2Instance {
    /// The kind of UI it has, if it has one Dawkey can show.
    pub(crate) fn gui_kind(&self) -> Option<crate::GuiKind> {
        self.ui_info.as_ref().map(|ui| match ui.toolkit {
            Toolkit::X11 => crate::GuiKind::Lv2X11,
            Toolkit::Gtk(_) | Toolkit::Qt(_) => crate::GuiKind::Lv2Own,
        })
    }

    /// Opens its UI: an X11 UI in window `parent`, a Gtk UI in a window of
    /// its own titled `title`. Returns the size it asked for.
    pub(crate) fn gui_open(
        &mut self,
        parent: Option<u32>,
        title: &str,
    ) -> Result<Option<(u32, u32)>, PluginError> {
        self.gui_close();
        let info = self
            .ui_info
            .as_ref()
            .ok_or_else(|| PluginError::new("it has no UI Dawkey can show"))?;
        if !self.alive.load(Ordering::Acquire) {
            return Err(PluginError::new("the plugin is no longer running"));
        }
        // What the plugin sent while it was closed is old news.
        if let Some((_, from_plugin)) = &mut self.ui_rings {
            from_plugin.read(|_, _| {});
        }
        // SAFETY: the plugin instance and features outlive the UI (it's
        // closed before the plugin goes: `gui_close` in `Drop`); `parent`
        // is the host process's live window.
        let mut ui = unsafe {
            crate::lv2_ui::Lv2Ui::open(
                info,
                parent.map_or(Place::Own(title), Place::In),
                self.handle,
                self.extension_data,
                &self.features.list[..],
                &self.ui_ports,
                self.ui_urids,
            )?
        };
        let size = ui.take_resize();
        self.ui = Some(ui);
        self.link.ui_open.store(true, Ordering::Release);
        self.send_ui_messages();
        Ok(size)
    }

    pub(crate) fn gui_close(&mut self) {
        self.link.ui_open.store(false, Ordering::Release);
        self.ui = None;
    }

    /// Runs the open UI with the parameters' current `values` (and the
    /// output ports' and the atoms the plugin sent). Returns the size it
    /// asked for, and whether it closed itself.
    pub(crate) fn gui_idle(&mut self, values: &[f64]) -> (Option<(u32, u32)>, bool) {
        let Some(ui) = &mut self.ui else {
            return (None, false);
        };
        let outputs: Vec<f32> = self
            .link
            .outputs
            .iter()
            .map(|v| f32::from_bits(v.load(Ordering::Relaxed)))
            .collect();
        let mut atoms: Vec<(u32, Vec<u64>, usize)> = Vec::new();
        if let Some((_, from_plugin)) = &mut self.ui_rings {
            from_plugin.read(|port, parts| {
                let mut words = Vec::new();
                let len = crate::lv2_ring::gather(&mut words, parts).len();
                atoms.push((port, words, len));
            });
        }
        let atoms: Vec<(u32, &[u8])> = atoms
            .iter()
            .map(|(port, words, len)| (*port, crate::lv2_ring::bytes(words, *len)))
            .collect();
        let open = ui.idle(values, &outputs, &atoms);
        let resize = ui.take_resize();
        self.send_ui_messages();
        if !open {
            self.gui_close();
        }
        (resize, !open)
    }

    /// Sends the plugin what its UI sent it.
    fn send_ui_messages(&mut self) {
        let (Some(ui), Some((to_plugin, _))) = (&mut self.ui, &mut self.ui_rings) else {
            return;
        };
        let (messages, changed) = ui.take_messages();
        for (port, atom) in messages {
            if !to_plugin.write(port, &[&atom]) {
                tracing::debug!("a message from the UI to its plugin didn't fit");
            }
        }
        if changed {
            self.link.dirty.store(true, Ordering::Release);
        }
    }

    /// Whether its state changed since it was last saved, as far as the
    /// plugin or its UI said.
    pub(crate) fn state_dirty(&self) -> bool {
        self.link.dirty.load(Ordering::Acquire)
    }

    /// What its UI changed since the last call.
    pub(crate) fn take_param_changes(&mut self) -> Vec<crate::ParamChange> {
        self.ui
            .as_mut()
            .map(crate::lv2_ui::Lv2Ui::take_changes)
            .unwrap_or_default()
    }
}

impl Drop for Lv2Instance {
    fn drop(&mut self) {
        // The UI goes before the plugin.
        self.gui_close();
    }
}

impl Lv2Instance {
    /// What the plugin saves of itself, or `None` if it saves nothing. LV2
    /// lets hosts save while the plugin runs.
    pub(crate) fn save_state(
        &mut self,
        paths: &StatePaths,
    ) -> Result<Option<Vec<u8>>, crate::PluginError> {
        self.link.dirty.store(false, Ordering::Release);
        let Some(interface) = self.state else {
            return Ok(None);
        };
        if !self.alive.load(Ordering::Acquire) {
            return Err(PluginError::new("the plugin is no longer running"));
        }
        // SAFETY: the interface comes from the plugin's extension data and
        // lives as long as its library.
        let Some(save) = (unsafe { interface.as_ref() }).save else {
            return Ok(None);
        };
        let mut saving = Saving {
            urids: &self.features.urids,
            properties: Vec::new(),
        };
        self.paths.set(paths);
        let features = self.paths.with(&self.features);
        // SAFETY: the instance is alive (checked above, and it's only
        // freed on this thread); the callback's handle is `saving`.
        let status = unsafe {
            save(
                self.handle,
                store,
                std::ptr::from_mut(&mut saving).cast(),
                // LV2_STATE_IS_POD | LV2_STATE_IS_PORTABLE: what we accept.
                0b11,
                features.as_ptr(),
            )
        };
        if status != 0 {
            return Err(PluginError::new(format!(
                "saving its state failed (status {status})"
            )));
        }
        Ok(Some(encode_state(&saving.properties)))
    }
}

/// The plugin's UI Dawkey shows, if it has one: an X11 UI (embedded), or
/// else a Gtk 2 or 3 one, or else a Qt 5 or 6 one.
fn find_ui(world: &World, plugin: &Plugin, plugin_uri: &str) -> Option<crate::lv2_ui::UiInfo> {
    use crate::{gtk, qt};
    let kind = |name: &str| world.new_uri(&format!("http://lv2plug.in/ns/extensions/ui#{name}"));
    let kinds = [
        (kind("X11UI"), Toolkit::X11),
        (kind("GtkUI"), Toolkit::Gtk(gtk::Version::Gtk2)),
        (kind("Gtk3UI"), Toolkit::Gtk(gtk::Version::Gtk3)),
        (kind("Qt5UI"), Toolkit::Qt(qt::Version::Qt5)),
        (kind("Qt6UI"), Toolkit::Qt(qt::Version::Qt6)),
    ];
    let uis = plugin.uis()?;
    let path = |node: Option<lilv::node::Node>| node?.path().map(|(_, p)| PathBuf::from(p));
    // One whose library is installed (packages may leave it out).
    let (ui, toolkit) = kinds.iter().find_map(|(class, toolkit)| {
        uis.iter()
            .filter(|ui| ui.is_a(class))
            .find(|ui| path(ui.binary_uri()).is_some_and(|b| b.is_file()))
            .map(|ui| (ui, *toolkit))
    })?;
    let mut bundle = path(ui.bundle_uri())?.into_os_string();
    // LV2 bundle paths end in a slash.
    if !bundle.to_string_lossy().ends_with('/') {
        bundle.push("/");
    }
    Some(crate::lv2_ui::UiInfo {
        uri: CString::new(ui.uri().as_uri()?).ok()?,
        plugin_uri: CString::new(plugin_uri).ok()?,
        binary: path(ui.binary_uri())?,
        bundle: bundle.into(),
        toolkit,
    })
}

/// The plugin's `extension_data`, if it has one (for UIs' data access).
///
/// # Safety
///
/// As [`extension_data`].
unsafe fn descriptor_extension_data(
    instance: &lilv::instance::Instance,
) -> Option<crate::lv2_ui::ExtensionData> {
    #[repr(C)]
    struct RawDescriptor {
        uri: *const c_char,
        instantiate: *const c_void,
        connect_port: *const c_void,
        activate: *const c_void,
        run: *const c_void,
        deactivate: *const c_void,
        cleanup: *const c_void,
        extension_data: Option<crate::lv2_ui::ExtensionData>,
    }
    let descriptor = std::ptr::from_ref(instance.descriptor()?).cast::<RawDescriptor>();
    // SAFETY: as the caller promises; `RawDescriptor` lays it out the same.
    unsafe { (*descriptor).extension_data }
}

/// The plugin's extension data for `uri`, or `None`. (lilv-sys calls the
/// plugin's `extension_data` without checking it, but plugins without
/// extensions may leave it null, as the SWH plugins do.)
///
/// # Safety
///
/// Runs plugin code; the data is what the plugin says `uri` gives.
unsafe fn extension_data<T>(instance: &lilv::instance::Instance, uri: &CStr) -> Option<NonNull<T>> {
    /// `LV2_Descriptor`, with the function that may be null as an option.
    #[repr(C)]
    struct RawDescriptor {
        uri: *const c_char,
        instantiate: *const c_void,
        connect_port: *const c_void,
        activate: *const c_void,
        run: *const c_void,
        deactivate: *const c_void,
        cleanup: *const c_void,
        extension_data: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
    }
    let descriptor = std::ptr::from_ref(instance.descriptor()?).cast::<RawDescriptor>();
    // SAFETY: lilv's descriptor is the plugin's `LV2_Descriptor`, which
    // `RawDescriptor` lays out the same; the call is as the plugin defines.
    let get = unsafe { (*descriptor).extension_data }?;
    NonNull::new(unsafe { get(uri.as_ptr()) }.cast_mut().cast())
}

/// Restores state [`Lv2Instance::save_state`] saved into a fresh instance.
fn restore_state(
    instance: &lilv::instance::Instance,
    features: &Features,
    paths: &PathFeatures,
    worker: Option<&Worker>,
    state: &[u8],
) -> Result<(), String> {
    let properties = decode_state(state).ok_or("its saved state is damaged")?;
    // SAFETY: the plugin's extension data, as it gives it.
    let interface = unsafe { extension_data::<StateInterface>(instance, STATE_INTERFACE) }
        .ok_or("it doesn't save state")?;
    // SAFETY: as above.
    let restore = unsafe { interface.as_ref() }
        .restore
        .ok_or("it doesn't restore state")?;
    let urids = &features.urids;
    let restoring = Restoring {
        properties: properties
            .into_iter()
            .map(|p| (urids.map(&p.key), urids.map(&p.kind), p))
            .collect(),
    };
    // The plugin may schedule work while restoring (loading the files its
    // state names), which is done like work it schedules while running.
    let worker = worker.map(Worker::feature);
    let mut list = paths.with(features);
    list.pop();
    list.extend(worker.as_ref().map(std::ptr::from_ref));
    list.push(std::ptr::null());
    // SAFETY: the instance isn't running (it's being set up), the handle
    // is `restoring`, which outlives the call.
    let status = unsafe {
        restore(
            instance.handle(),
            retrieve,
            std::ptr::from_ref(&restoring).cast_mut().cast(),
            0,
            list.as_ptr(),
        )
    };
    match status {
        0 => Ok(()),
        status => Err(format!("restoring its state failed (status {status})")),
    }
}

/// A lilv world that reads the bundles in `path` (or lilv's standard
/// locations).
fn world(path: Option<&[PathBuf]>) -> World {
    let world = World::new();
    if let Some(dirs) = path
        && let Ok(joined) = std::env::join_paths(dirs)
    {
        let value = world.new_string(&joined.to_string_lossy());
        world.set_option("http://drobilla.net/ns/lilv#lv2-path", &value);
    }
    world.load_all();
    world
}

/// The LV2 side of a [`Host`](crate::Host): the plugin world (read once)
/// and the features handed to plugins.
pub(crate) struct Lv2Host {
    path: Option<Vec<PathBuf>>,
    world: Option<World>,
    features: Option<(u32, Arc<Features>)>,
}

impl Lv2Host {
    pub(crate) fn new(path: Option<Vec<PathBuf>>) -> Self {
        Self {
            path,
            world: None,
            features: None,
        }
    }

    pub(crate) fn load(
        &mut self,
        info: &PluginInfo,
        sample_rate: u32,
        state: Option<&[u8]>,
        paths: &StatePaths,
        offline: bool,
    ) -> Result<(Lv2Instance, Lv2Processor, u32), PluginError> {
        let path = self.path.as_deref();
        let world = self.world.get_or_insert_with(|| world(path));
        let features = match &self.features {
            Some((rate, f)) if *rate == sample_rate => Arc::clone(f),
            _ => {
                let f = Features::new(sample_rate);
                self.features = Some((sample_rate, Arc::clone(&f)));
                f
            }
        };
        let uri = info.id.strip_prefix("lv2:").unwrap_or(&info.id);
        let plugin = world
            .plugins()
            .plugin(&world.new_uri(uri))
            .ok_or_else(|| PluginError::new(format!("LV2 plugin {uri} is not installed")))?;
        if let Some(missing) = unsupported_feature(&plugin) {
            return Err(PluginError::new(format!(
                "{uri} requires {missing}, which Dawkey doesn't support yet"
            )));
        }
        if let Some(problem) = unreadable(&plugin) {
            return Err(PluginError::new(format!("{uri}: {problem}")));
        }
        let layout = Layout::of(world, &plugin, info, sample_rate)?;
        let path_features = PathFeatures::new(paths.clone());
        // Offline, work is done as it's asked for, so renders come out the
        // same however long it takes.
        let schedule = world.new_uri(&crate::lv2_worker::SCHEDULE.to_string_lossy());
        let mut worker = plugin.has_feature(&schedule).then(|| Worker::new(offline));
        let worker_feature = worker.as_ref().map(Worker::feature);
        // SAFETY: instantiating runs plugin code; the features outlive the
        // instance because the processor keeps them.
        let instance = unsafe {
            plugin.instantiate(
                f64::from(sample_rate),
                features
                    .list
                    .iter()
                    .chain(path_features.list())
                    .chain(worker_feature.iter()),
            )
        }
        .ok_or_else(|| PluginError::new(format!("{uri} failed to instantiate")))?;
        if let Some(worker) = &mut worker {
            // SAFETY: the plugin's extension data, as it gives it; the
            // worker goes after the plugin (the processor drops it last).
            unsafe {
                let interface = extension_data(&instance, crate::lv2_worker::INTERFACE);
                worker.start(instance.handle(), interface);
            }
        }
        if let Some(state) = state
            && let Err(e) =
                restore_state(&instance, &features, &path_features, worker.as_ref(), state)
        {
            // The plugin still works, as it starts.
            tracing::warn!("{uri}: {e}");
        }
        // SAFETY: the plugin's extension data, as it gives it.
        let state_interface =
            unsafe { extension_data::<StateInterface>(&instance, STATE_INTERFACE) };
        let alive = Arc::new(AtomicBool::new(true));
        let ui_info = find_ui(world, &plugin, uri);
        // SAFETY: the plugin's descriptor, as lilv has it; its
        // `extension_data` may be null.
        let extension_data = unsafe { descriptor_extension_data(&instance) };
        let link = Arc::new(Link {
            outputs: layout
                .control_out
                .iter()
                .map(|_| AtomicU32::new(0))
                .collect(),
            ui_open: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
        });
        // Rings between the UI and the processor, as big as the atoms that
        // go through them (twice, for those from the plugin).
        let largest = |output: bool| {
            (layout.other_atoms.iter().copied())
                .chain(layout.midi_out.map(|p| (p, true)))
                .filter(|&(_, out)| out == output)
                .map(|(p, _)| layout.atom_capacity[p])
                .max()
                .unwrap_or(0)
        };
        let (to_ui, ui_rings) = if ui_info.is_some() {
            let (to_plugin, from_ui) = ring(largest(false).clamp(UI_RING, ATOM_CAPACITY_MAX));
            let (to_ui, from_plugin) = ring((2 * largest(true)).clamp(UI_RING, ATOM_CAPACITY_MAX));
            (Some((to_ui, from_ui)), Some((to_plugin, from_plugin)))
        } else {
            (None, None)
        };
        let urids = &features.urids;
        let id = |uri: &str| urids.map(&CString::new(uri).expect("no NUL"));
        let objects = [
            id(&format!("{ATOM}Object")),
            id(&format!("{ATOM}Blank")),
            id(&format!("{ATOM}Resource")),
        ];
        let ui_urids = UiUrids {
            event_transfer: id(&format!("{ATOM}eventTransfer")),
            atom_transfer: id(&format!("{ATOM}atomTransfer")),
            objects,
            patch_changes: ["Set", "Put", "Patch", "Insert", "Delete", "Copy", "Move"]
                .map(|name| id(&format!("{PATCH}{name}"))),
        };
        let atom_inputs = (layout.midi_in.iter().map(|&p| (p, false)))
            .chain(layout.other_atoms.iter().copied())
            .filter(|&(_, out)| !out)
            .map(|(p, _)| p as u32)
            .collect();
        let control = Lv2Instance {
            handle: instance.handle(),
            state: state_interface,
            features: Arc::clone(&features),
            paths: Arc::clone(&path_features),
            alive: Arc::clone(&alive),
            ui_info,
            ui: None,
            link: Arc::clone(&link),
            ui_rings,
            ui_ports: UiPorts {
                params: layout.params.iter().map(|&p| p as u32).collect(),
                outputs: layout.control_out.iter().map(|&p| p as u32).collect(),
                atom_inputs,
                symbols: layout.symbols.clone(),
            },
            ui_urids,
            extension_data,
        };
        let midi_event = features
            .urids
            .map(&CString::new(MIDI_EVENT).expect("no NUL"));
        let sequence = features
            .urids
            .map(c"http://lv2plug.in/ns/ext/atom#Sequence");
        let chunk = features.urids.map(c"http://lv2plug.in/ns/ext/atom#Chunk");
        let ports = layout.port_count;
        let mut processor = Lv2Processor {
            instance: None,
            layout,
            in_bufs: Vec::new(),
            out_bufs: Vec::new(),
            controls: vec![0.0; ports].into_boxed_slice(),
            atoms: Vec::new(),
            midi_event,
            sequence,
            chunk,
            time: TimeUrids::new(&features.urids),
            time_atom: None,
            sent_transport: None,
            alive,
            link,
            ui: to_ui,
            objects,
            state_changed: features.urids.map(STATE_CHANGED),
            worker,
            _features: features,
            _paths: path_features,
        };
        // Every port is connected before activating: some plugins (the SWH
        // ones) read their controls in `activate`.
        let mut instance = instance;
        processor.connect_static(&mut instance);
        // SAFETY: every port is connected.
        processor.instance = Some(unsafe { instance.activate() });
        // Run one silent block so the latency port gets a value.
        let mut left = vec![0.0; 64];
        let mut right = vec![0.0; 64];
        let mut ignored = Vec::new();
        processor.process(
            [&mut left, &mut right],
            ProcessContext::new(Transport::default()),
            &[],
            MidiSink::new(&mut ignored),
        );
        // The real transport goes to the first real block.
        processor.sent_transport = None;
        let latency = processor
            .layout
            .latency
            .map_or(0, |p| processor.controls[p].max(0.0).round() as u32);
        Ok((control, processor, latency))
    }
}

/// Why `plugin` can't be run safely from its description, if it can't: a
/// plugin whose ports lilv can't read (a broken Turtle file) seems to have
/// none, and would write to ports that were never connected.
fn unreadable(plugin: &Plugin) -> Option<&'static str> {
    (plugin.ports_count() == 0).then_some("its ports can't be read from its description")
}

/// The first feature `plugin` requires that we don't provide.
fn unsupported_feature(plugin: &Plugin) -> Option<String> {
    plugin
        .required_features()
        .iter()
        .filter_map(|n| n.as_uri().map(str::to_owned))
        .find(|uri| !SUPPORTED.contains(&uri.as_str()))
}

/// How a plugin's ports are used.
#[derive(Debug, Clone, Default)]
struct Layout {
    port_count: usize,
    audio_in: Vec<usize>,
    audio_out: Vec<usize>,
    /// Input control ports, by parameter index (catalog order).
    params: Vec<usize>,
    /// Their default values.
    param_defaults: Vec<f32>,
    /// The bounds each parameter is held to, in `params`' order: those a
    /// port with `lv2:sampleRate` declares, at this sample rate (the
    /// catalog's are at [`NOMINAL_RATE`]); none for other ports.
    param_limits: Vec<(f32, f32)>,
    /// Other control ports, with their default value.
    other_controls: Vec<(usize, f32)>,
    /// The output control ports (meters and the like).
    control_out: Vec<usize>,
    midi_in: Option<usize>,
    /// The first MIDI atom output.
    midi_out: Option<usize>,
    /// Other atom ports, and whether they're outputs.
    other_atoms: Vec<(usize, bool)>,
    /// The atom input that takes the transport (`time:Position`).
    time_in: Option<usize>,
    latency: Option<usize>,
    /// What the audio ports are for (positions in `audio_in` and
    /// `audio_out`).
    roles: AudioRoles,
    /// Each port's buffer size, if it's an atom port.
    atom_capacity: Vec<usize>,
    /// Each port's symbol.
    symbols: Vec<CString>,
}

/// What a plugin's audio ports are for, as positions among its audio
/// inputs and outputs (in port order).
#[derive(Debug, Clone, Default)]
struct AudioRoles {
    /// The track's signal goes in here (one port: mono).
    main_in: Vec<usize>,
    /// The sidechain: ports marked `lv2:isSideChain`, otherwise the next
    /// two inputs.
    side_in: Vec<usize>,
    /// The track's signal comes out here.
    main_out: Vec<usize>,
    /// The other outputs, in pairs (a last odd one alone), with a name.
    other_out: Vec<(String, Vec<usize>)>,
}

fn audio_roles(kinds: &[PortKind]) -> AudioRoles {
    let ins: Vec<&PortKind> = kinds.iter().filter(|k| k.audio && k.input).collect();
    let outs: Vec<&PortKind> = kinds.iter().filter(|k| k.audio && !k.input).collect();
    let (marked, plain): (Vec<usize>, Vec<usize>) = (0..ins.len()).partition(|&i| ins[i].sidechain);
    let side_in = if marked.is_empty() {
        plain.iter().skip(2).take(2).copied().collect()
    } else {
        marked.into_iter().take(2).collect()
    };
    let other_out = (2..outs.len())
        .collect::<Vec<_>>()
        .chunks(2)
        .map(|pair| {
            let names: Vec<&str> = pair.iter().map(|&i| outs[i].name.as_str()).collect();
            (pair_name(&names), pair.to_vec())
        })
        .collect();
    AudioRoles {
        main_in: plain.into_iter().take(2).collect(),
        side_in,
        main_out: (0..outs.len().min(2)).collect(),
        other_out,
    }
}

/// A name for an output made of these ports: what their names share
/// ("Snare L" and "Snare R" make "Snare"), or the first one's.
fn pair_name(names: &[&str]) -> String {
    let [a, b] = names else {
        return names.first().copied().unwrap_or_default().to_owned();
    };
    let common: String = a
        .chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x)
        .collect();
    let common = common.trim_end_matches([' ', '_', '-', '(']);
    if common.is_empty() {
        (*a).to_owned()
    } else {
        common.to_owned()
    }
}

/// A port's classes, read once.
struct PortKind {
    index: usize,
    input: bool,
    audio: bool,
    control: bool,
    atom: bool,
    midi: bool,
    /// Takes `time:Position` (the transport).
    time: bool,
    /// An audio input for a sidechain (`lv2:isSideChain`).
    sidechain: bool,
    optional: bool,
    /// The buffer size it asks for, if it's an atom port
    /// (`rsz:minimumSize`).
    min_size: usize,
    symbol: String,
    name: String,
    /// Its bounds and default (at [`NOMINAL_RATE`] for a port with
    /// `lv2:sampleRate`).
    range: (f32, f32, f32),
    /// The bounds it declares as fractions of the sample rate
    /// (`lv2:sampleRate`; infinite where it declares none), if it does.
    per_rate: Option<(f32, f32)>,
    stepped: bool,
    /// The unit's symbol (`units:unit`).
    unit: Option<String>,
    /// Scale points.
    labels: Vec<ValueLabel>,
}

/// The symbol of an LV2 unit: the ones the units extension defines, or a
/// custom unit's `units:symbol`.
fn unit_symbol(world: &World, unit: &lilv::node::Node) -> Option<String> {
    if let Some(name) = unit.as_uri().and_then(|u| u.strip_prefix(UNITS)) {
        let symbol = match name {
            "bar" => "bars",
            "beat" => "beats",
            "bpm" => "BPM",
            "cent" => "ct",
            "cm" => "cm",
            "coef" => "",
            "db" => "dB",
            "degree" => "°",
            "frame" => "frames",
            "hz" => "Hz",
            "inch" => "in",
            "khz" => "kHz",
            "km" => "km",
            "m" => "m",
            "mhz" => "MHz",
            "midiNote" => "note",
            "mile" => "mi",
            "min" => "min",
            "mm" => "mm",
            "ms" => "ms",
            "oct" => "oct",
            "pc" => "%",
            "s" => "s",
            "semitone12TET" => "semi",
            _ => return None,
        };
        return (!symbol.is_empty()).then(|| symbol.to_owned());
    }
    let symbol = world.new_uri(&format!("{UNITS}symbol"));
    world
        .get(Some(unit), Some(&symbol), None)
        .and_then(|s| s.as_str().map(str::to_owned))
        .filter(|s| !s.is_empty())
}

/// A control port's range and default from its `lv2:minimum`,
/// `lv2:maximum` and `lv2:default` (NaN where it leaves one out) and its
/// scale points. The bounds are hints a port may leave out: a bound it
/// leaves out (or both, if they're the wrong way round) is made up to hold
/// its default and scale points, and 0 to 1 as well, so a declared default
/// stays what it is. Only a declared bound moves the default.
fn control_range(min: f32, max: f32, default: f32, points: &[f32]) -> (f32, f32, f32) {
    let declared = |v: f32| v.is_finite().then_some(v);
    let (min, max) = match (declared(min), declared(max)) {
        (Some(min), Some(max)) if max <= min => (None, None),
        bounds => bounds,
    };
    let default = declared(default);
    let held = || default.into_iter().chain(points.iter().copied());
    let lo = min.unwrap_or_else(|| {
        let lowest = held().fold(0.0, f32::min);
        // Below a declared maximum.
        max.map_or(lowest, |max| lowest.min(max - 1.0))
    });
    let hi = max.unwrap_or_else(|| held().fold(1.0, f32::max).max(lo + 1.0));
    (lo, hi, default.unwrap_or(lo).clamp(lo, hi))
}

fn port_kinds(world: &World, plugin: &Plugin) -> Vec<PortKind> {
    let uri = |s: &str| world.new_uri(s);
    let (input, audio, control, atom_port, optional, midi) = (
        uri(&format!("{LV2}InputPort")),
        uri(&format!("{LV2}AudioPort")),
        uri(&format!("{LV2}ControlPort")),
        uri(&format!("{ATOM}AtomPort")),
        uri(&format!("{LV2}connectionOptional")),
        uri(MIDI_EVENT),
    );
    let (time, sidechain) = (uri(TIME_POSITION), uri(&format!("{LV2}isSideChain")));
    let stepped = [
        uri(&format!("{LV2}integer")),
        uri(&format!("{LV2}toggled")),
        uri(&format!("{LV2}enumeration")),
    ];
    let ranges = plugin.port_ranges_float();
    let sample_rate = uri(&format!("{LV2}sampleRate"));
    let unit = uri(&format!("{UNITS}unit"));
    let min_size = uri(MINIMUM_SIZE);
    plugin
        .iter_ports()
        .map(|port| {
            let index = port.index();
            let labels: Vec<ValueLabel> = port
                .scale_points()
                .iter()
                .filter_map(|p| {
                    let value = p.value();
                    let value = value
                        .as_float()
                        .map(f64::from)
                        .or_else(|| value.as_int().map(f64::from))?;
                    Some(ValueLabel {
                        value,
                        label: p.label().as_str()?.to_owned(),
                    })
                })
                .collect();
            // Bounds and a default that are fractions of the sample rate
            // ("hosts that support bounds at all must support this
            // property", LV2 core).
            let relative = port.has_property(&sample_rate);
            let scale = if relative { NOMINAL_RATE } else { 1.0 };
            let range = ranges.get(index).map_or((0.0, 0.0, 1.0), |r| {
                let points: Vec<f32> = labels.iter().map(|l| l.value as f32).collect();
                control_range(r.min * scale, r.max * scale, r.default * scale, &points)
            });
            let declared = |v: f32, otherwise: f32| if v.is_finite() { v } else { otherwise };
            let per_rate = (ranges.get(index).filter(|_| relative)).map(|r| {
                (
                    declared(r.min, f32::NEG_INFINITY),
                    declared(r.max, f32::INFINITY),
                )
            });
            PortKind {
                index,
                input: port.is_a(&input),
                audio: port.is_a(&audio),
                control: port.is_a(&control),
                atom: port.is_a(&atom_port),
                midi: port.supports_event(&midi),
                time: port.supports_event(&time),
                sidechain: port.has_property(&sidechain),
                optional: port.has_property(&optional),
                min_size: port
                    .get(&min_size)
                    .and_then(|n| n.as_int())
                    .map_or(0, |n| usize::try_from(n).unwrap_or(0)),
                symbol: port
                    .symbol()
                    .and_then(|s| s.as_str().map(str::to_owned))
                    .unwrap_or_else(|| index.to_string()),
                name: port
                    .name()
                    .and_then(|s| s.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                range,
                per_rate,
                stepped: stepped.iter().any(|p| port.has_property(p)),
                unit: port.get(&unit).and_then(|u| unit_symbol(world, &u)),
                labels,
            }
        })
        .collect()
}

impl Layout {
    fn of(
        world: &World,
        plugin: &Plugin,
        info: &PluginInfo,
        sample_rate: u32,
    ) -> Result<Self, PluginError> {
        let kinds = port_kinds(world, plugin);
        let mut layout = Layout {
            port_count: plugin.ports_count(),
            latency: plugin.latency_port_index(),
            ..Layout::default()
        };
        layout.time_in = kinds
            .iter()
            .find(|k| k.atom && k.input && k.time)
            .map(|k| k.index);
        layout.roles = audio_roles(&kinds);
        layout.atom_capacity = vec![0; layout.port_count];
        layout.symbols = vec![CString::default(); layout.port_count];
        for k in kinds.iter().filter(|k| k.index < layout.port_count) {
            if k.atom {
                layout.atom_capacity[k.index] = k
                    .min_size
                    .clamp(ATOM_CAPACITY, ATOM_CAPACITY_MAX)
                    .next_multiple_of(8);
            }
            layout.symbols[k.index] = CString::new(k.symbol.as_str()).unwrap_or_default();
        }
        for k in &kinds {
            match (k.audio, k.control, k.atom, k.input) {
                (true, _, _, true) => layout.audio_in.push(k.index),
                (true, _, _, false) => layout.audio_out.push(k.index),
                (_, true, _, true) => {}
                (_, true, _, false) => {
                    layout.other_controls.push((k.index, 0.0));
                    layout.control_out.push(k.index);
                }
                (_, _, true, true) if k.midi && layout.midi_in.is_none() => {
                    layout.midi_in = Some(k.index)
                }
                (_, _, true, false) if k.midi && layout.midi_out.is_none() => {
                    layout.midi_out = Some(k.index)
                }
                (_, _, true, input) => layout.other_atoms.push((k.index, !input)),
                _ if k.optional => {}
                _ => {
                    return Err(PluginError::new(format!(
                        "port {} has a type Dawkey doesn't support",
                        k.symbol
                    )));
                }
            }
        }
        // Parameters follow the catalog's order; unlisted inputs keep
        // their default.
        for k in kinds.iter().filter(|k| k.control && k.input) {
            if info.params.iter().all(|p| p.id != k.symbol) {
                layout.other_controls.push((k.index, k.range.2));
            }
        }
        layout.params = info
            .params
            .iter()
            .map(|p| {
                kinds
                    .iter()
                    .find(|k| k.control && k.input && k.symbol == p.id)
                    .map(|k| k.index)
                    .ok_or_else(|| PluginError::new(format!("no control port {}", p.id)))
            })
            .collect::<Result<_, _>>()?;
        let rate = sample_rate as f32;
        layout.param_limits = (layout.params.iter())
            .map(|&port| {
                kinds
                    .iter()
                    .find(|k| k.index == port)
                    .and_then(|k| k.per_rate)
                    .map_or((f32::NEG_INFINITY, f32::INFINITY), |(lo, hi)| {
                        (lo * rate, hi * rate)
                    })
            })
            .collect();
        layout.param_defaults = (info.params.iter().zip(&layout.param_limits))
            .map(|(p, &(lo, hi))| (p.default as f32).max(lo).min(hi))
            .collect();
        Ok(layout)
    }
}

/// Describes every LV2 plugin lilv finds (on `LV2_PATH`, or the standard
/// locations). Runs no plugin code.
pub(crate) fn scan(path: Option<&[PathBuf]>) -> (Vec<PluginInfo>, Vec<(PathBuf, String)>) {
    let world = world(path);
    let mut found = Vec::new();
    let mut failures = Vec::new();
    for plugin in world.plugins().iter() {
        let Some(uri) = plugin.uri().as_uri().map(str::to_owned) else {
            continue;
        };
        let path = plugin
            .bundle_uri()
            .path()
            .map(|(_, path)| PathBuf::from(path))
            .unwrap_or_default();
        if let Some(missing) = unsupported_feature(&plugin) {
            failures.push((path, format!("{uri} requires {missing}")));
            continue;
        }
        if let Some(problem) = unreadable(&plugin) {
            failures.push((path, format!("{uri}: {problem}")));
            continue;
        }
        let kinds = port_kinds(&world, &plugin);
        if let Some(bad) = kinds
            .iter()
            .find(|k| !k.audio && !k.control && !k.atom && !k.optional)
        {
            failures.push((
                path,
                format!("{uri}: port {} has an unsupported type", bad.symbol),
            ));
            continue;
        }
        let latency = plugin.latency_port_index();
        let roles = audio_roles(&kinds);
        let name = plugin
            .name()
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| uri.clone());
        let vendor = plugin
            .author_name()
            .and_then(|n| n.as_str().map(str::to_owned))
            .unwrap_or_default();
        let properties = declared_properties(&world, &plugin);
        found.push(PluginInfo {
            id: format!("lv2:{uri}"),
            format: PluginFormat::Lv2,
            name,
            vendor,
            path,
            properties,
            instrument: kinds.iter().any(|k| k.atom && k.input && k.midi),
            midi_output: kinds.iter().any(|k| k.atom && !k.input && k.midi),
            audio_inputs: roles.main_in.len() as u32,
            audio_outputs: roles.main_out.len() as u32,
            sidechain: !roles.side_in.is_empty(),
            outputs: roles
                .other_out
                .iter()
                .map(|(name, ports)| OutputInfo {
                    name: name.clone(),
                    channels: ports.len() as u32,
                })
                .collect(),
            params: kinds
                .iter()
                .filter(|k| k.control && k.input && Some(k.index) != latency)
                .map(|k| ParamInfo {
                    id: k.symbol.clone(),
                    name: if k.name.is_empty() {
                        k.symbol.clone()
                    } else {
                        k.name.clone()
                    },
                    min: f64::from(k.range.0),
                    max: f64::from(k.range.1),
                    default: f64::from(k.range.2),
                    stepped: k.stepped,
                    unit: k.unit.clone(),
                    labels: k.labels.clone(),
                })
                .collect(),
        });
    }
    (found, failures)
}

/// The URIDs of a `time:Position` object.
#[derive(Debug, Clone, Copy)]
struct TimeUrids {
    object: u32,
    long: u32,
    float: u32,
    double: u32,
    int: u32,
    position: u32,
    frame: u32,
    speed: u32,
    bar: u32,
    bar_beat: u32,
    beat: u32,
    beat_unit: u32,
    beats_per_bar: u32,
    beats_per_minute: u32,
}

impl TimeUrids {
    fn new(urids: &Urids) -> Self {
        let atom = |name: &str| urids.map(&CString::new(format!("{ATOM}{name}")).expect("no NUL"));
        let time = |name: &str| {
            urids.map(
                &CString::new(format!("http://lv2plug.in/ns/ext/time#{name}")).expect("no NUL"),
            )
        };
        Self {
            object: atom("Object"),
            long: atom("Long"),
            float: atom("Float"),
            double: atom("Double"),
            int: atom("Int"),
            position: time("Position"),
            frame: time("frame"),
            speed: time("speed"),
            bar: time("bar"),
            bar_beat: time("barBeat"),
            beat: time("beat"),
            beat_unit: time("beatUnit"),
            beats_per_bar: time("beatsPerBar"),
            beats_per_minute: time("beatsPerMinute"),
        }
    }

    /// The body of a `time:Position` object for `t`, written into `out`.
    /// Returns its length. Beats are in the time signature's beat unit (a
    /// quarter note in 4/4, an eighth in 6/8); the tempo is quarter notes
    /// per minute.
    fn position(&self, t: &Transport, out: &mut [u8; 256]) -> usize {
        let unit = 4.0 / f64::from(t.denominator.max(1));
        let mut at = 0;
        let mut put = |bytes: &[u8]| {
            out[at..at + bytes.len()].copy_from_slice(bytes);
            at += bytes.len();
        };
        // Object body: id 0 (blank), type.
        put(&0u32.to_ne_bytes());
        put(&self.position.to_ne_bytes());
        let mut property = |key: u32, kind: u32, value: &[u8]| {
            put(&key.to_ne_bytes());
            put(&0u32.to_ne_bytes());
            put(&(value.len() as u32).to_ne_bytes());
            put(&kind.to_ne_bytes());
            put(value);
            // Pad to 8 bytes.
            let pad = value.len().div_ceil(8) * 8 - value.len();
            put(&[0; 8][..pad]);
        };
        let speed = if t.playing { 1.0f32 } else { 0.0 };
        property(self.frame, self.long, &(t.frame as i64).to_ne_bytes());
        property(self.speed, self.float, &speed.to_ne_bytes());
        property(self.bar, self.long, &i64::from(t.bar).to_ne_bytes());
        let bar_beat = ((t.beats - t.bar_start_beats) / unit) as f32;
        property(self.bar_beat, self.float, &bar_beat.to_ne_bytes());
        property(self.beat, self.double, &(t.beats / unit).to_ne_bytes());
        property(
            self.beat_unit,
            self.int,
            &i32::from(t.denominator).to_ne_bytes(),
        );
        let per_bar = f32::from(t.numerator);
        property(self.beats_per_bar, self.float, &per_bar.to_ne_bytes());
        property(
            self.beats_per_minute,
            self.float,
            &(t.bpm as f32).to_ne_bytes(),
        );
        at
    }
}

/// Whether the transport for a block isn't simply where the last one
/// left off: the plugin needs to be told.
fn transport_moved(last: Option<&(Transport, usize)>, now: &Transport) -> bool {
    let Some((last, frames)) = last else {
        return true;
    };
    let expected = last.frame + if last.playing { *frames as u64 } else { 0 };
    last.playing != now.playing
        || last.bpm != now.bpm
        || (last.numerator, last.denominator) != (now.numerator, now.denominator)
        || now.frame != expected
}

/// An 8-byte aligned atom buffer.
struct AtomBuffer {
    port: usize,
    output: bool,
    words: Box<[u64]>,
}

impl AtomBuffer {
    /// A buffer of `capacity` bytes (a multiple of 8).
    fn new(port: usize, output: bool, capacity: usize) -> Self {
        Self {
            port,
            output,
            words: vec![0; capacity.max(ATOM_CAPACITY) / 8].into_boxed_slice(),
        }
    }

    fn bytes(&mut self) -> &mut [u8] {
        let len = self.words.len() * 8;
        // SAFETY: a `[u64]` is valid as bytes, and the length matches.
        unsafe { std::slice::from_raw_parts_mut(self.words.as_mut_ptr().cast::<u8>(), len) }
    }

    /// An empty sequence (inputs), or an empty chunk with the whole
    /// capacity for the plugin to write into (outputs).
    fn reset(&mut self, sequence: u32, chunk: u32) {
        let output = self.output;
        let bytes = self.bytes();
        let (size, kind) = if output {
            ((bytes.len() - 8) as u32, chunk)
        } else {
            (8, sequence)
        };
        bytes[0..4].copy_from_slice(&size.to_ne_bytes());
        bytes[4..8].copy_from_slice(&kind.to_ne_bytes());
        bytes[8..16].fill(0);
    }

    /// The events in an output sequence the plugin wrote, as (frame, the
    /// event's atom: header and body).
    fn read_events(&mut self, sequence: u32, mut f: impl FnMut(u32, &[u8])) {
        let bytes = self.bytes();
        let word = |at: usize| u32::from_ne_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
        if word(4) != sequence {
            return;
        }
        // The body (after the 8-byte header) starts with an 8-byte unit
        // and pad, then the events.
        let end = (8 + word(0) as usize).min(bytes.len());
        let mut at = 16;
        while at + 16 <= end {
            let frame = i64::from_ne_bytes(bytes[at..at + 8].try_into().expect("8 bytes"));
            let size = word(at + 8) as usize;
            let data = at + 16;
            if data + size > end {
                break;
            }
            f(
                u32::try_from(frame.max(0)).unwrap_or(0),
                &bytes[at + 8..data + size],
            );
            at = data + size.div_ceil(8) * 8;
        }
    }

    /// The MIDI messages in an output sequence the plugin wrote, as
    /// (frame, message); two-byte messages end in 0.
    fn read_midi(&mut self, sequence: u32, midi_event: u32, mut f: impl FnMut(u32, [u8; 3])) {
        self.read_events(sequence, |frame, atom| {
            let kind = u32::from_ne_bytes(atom[4..8].try_into().expect("4 bytes"));
            let data = &atom[8..];
            if kind == midi_event
                && (1..=3).contains(&data.len())
                && (0x80..0xf0).contains(&data[0])
            {
                let mut message = [0u8; 3];
                message[..data.len()].copy_from_slice(data);
                f(frame, message);
            }
        });
    }

    /// Appends `atom` (its header and body, in two parts) at `frame` to an
    /// input sequence. Returns false if it doesn't fit or isn't an atom.
    fn push_atom(&mut self, frame: u32, parts: [&[u8]; 2]) -> bool {
        let len = parts[0].len() + parts[1].len();
        let mut header = [0u8; 8];
        for (i, byte) in header.iter_mut().enumerate() {
            match (
                parts[0].get(i),
                parts[1].get(i.wrapping_sub(parts[0].len())),
            ) {
                (Some(&b), _) | (None, Some(&b)) => *byte = b,
                (None, None) => return false,
            }
        }
        let size = u32::from_ne_bytes(header[..4].try_into().expect("4 bytes")) as usize;
        if size + 8 != len {
            return false;
        }
        let bytes = self.bytes();
        let used = u32::from_ne_bytes(bytes[0..4].try_into().expect("4 bytes")) as usize;
        let at = 8 + used;
        let padded = (len - 8).div_ceil(8) * 8;
        if at + 16 + padded > bytes.len() {
            return false;
        }
        bytes[at..at + 8].copy_from_slice(&i64::from(frame).to_ne_bytes());
        let body = at + 8;
        bytes[body..body + parts[0].len()].copy_from_slice(parts[0]);
        bytes[body + parts[0].len()..body + len].copy_from_slice(parts[1]);
        bytes[body + len..at + 16 + padded].fill(0);
        let used = (used + 16 + padded) as u32;
        bytes[0..4].copy_from_slice(&used.to_ne_bytes());
        true
    }

    /// Appends an atom of type `kind` with body `data` at `frame` to an
    /// input sequence. Returns false if it doesn't fit.
    fn push(&mut self, frame: u32, kind: u32, data: &[u8]) -> bool {
        let midi_event = kind;
        let bytes = self.bytes();
        let size = u32::from_ne_bytes(bytes[0..4].try_into().expect("4 bytes")) as usize;
        let at = 8 + size;
        let padded = data.len().div_ceil(8) * 8;
        if at + 16 + padded > bytes.len() {
            return false;
        }
        bytes[at..at + 8].copy_from_slice(&i64::from(frame).to_ne_bytes());
        bytes[at + 8..at + 12].copy_from_slice(&(data.len() as u32).to_ne_bytes());
        bytes[at + 12..at + 16].copy_from_slice(&midi_event.to_ne_bytes());
        bytes[at + 16..at + 16 + data.len()].copy_from_slice(data);
        bytes[at + 16 + data.len()..at + 16 + padded].fill(0);
        let size = (size + 16 + padded) as u32;
        bytes[0..4].copy_from_slice(&size.to_ne_bytes());
        true
    }
}

pub(crate) struct Lv2Processor {
    instance: Option<ActiveInstance>,
    layout: Layout,
    in_bufs: Vec<Box<[f32]>>,
    out_bufs: Vec<Box<[f32]>>,
    /// One value per port (used by control ports).
    controls: Box<[f32]>,
    /// The MIDI input first (if any), then the MIDI output (if any), then
    /// the other atom ports.
    atoms: Vec<AtomBuffer>,
    midi_event: u32,
    sequence: u32,
    chunk: u32,
    time: TimeUrids,
    /// Where in `atoms` the transport goes, if the plugin takes it.
    time_atom: Option<usize>,
    /// The transport last sent, and the length of its block.
    sent_transport: Option<(Transport, usize)>,
    /// Shared with the [`Lv2Instance`]: cleared when this goes.
    alive: Arc<AtomicBool>,
    link: Arc<Link>,
    /// Atoms to the UI, and from it, if it has one.
    ui: Option<(MessageWriter, MessageReader)>,
    /// `atom:Object`, `atom:Blank`, `atom:Resource`.
    objects: [u32; 3],
    state_changed: u32,
    /// Dropped after the plugin (it's declared after `instance`).
    worker: Option<Worker>,
    _features: Arc<Features>,
    _paths: Arc<PathFeatures>,
}

// The ActiveInstance is only used from one thread at a time: the control
// thread before and after, the real-time thread in between.
unsafe impl Send for Lv2Processor {}

impl Lv2Processor {
    /// Allocates buffers and connects the ports that never move: controls
    /// and atoms.
    fn connect_static(&mut self, instance: &mut lilv::instance::Instance) {
        let layout = &self.layout;
        self.in_bufs = layout
            .audio_in
            .iter()
            .map(|_| vec![0.0; MAX_BLOCK].into_boxed_slice())
            .collect();
        self.out_bufs = layout
            .audio_out
            .iter()
            .map(|_| vec![0.0; MAX_BLOCK].into_boxed_slice())
            .collect();
        for (&port, &default) in layout
            .params
            .iter()
            .zip(&layout.param_defaults)
            .chain(layout.other_controls.iter().map(|(p, d)| (p, d)))
        {
            self.controls[port] = default;
        }
        let atom = |(p, out): (usize, bool)| AtomBuffer::new(p, out, layout.atom_capacity[p]);
        self.atoms = layout
            .midi_in
            .map(|p| atom((p, false)))
            .into_iter()
            .chain(layout.midi_out.map(|p| atom((p, true))))
            .chain(layout.other_atoms.iter().copied().map(atom))
            .collect();
        self.time_atom = layout
            .time_in
            .and_then(|port| self.atoms.iter().position(|a| a.port == port));
        let controls = self.controls.as_mut_ptr();
        for port in layout
            .params
            .iter()
            .copied()
            .chain(layout.other_controls.iter().map(|&(p, _)| p))
        {
            // SAFETY: `controls` has one slot per port and is never moved.
            unsafe { instance.connect_port_mut(port, controls.add(port)) };
        }
        for atom in &mut self.atoms {
            // SAFETY: the atom buffers are boxed and never moved.
            unsafe { instance.connect_port_mut(atom.port, atom.words.as_mut_ptr()) };
        }
        // Audio ports are connected again for each run (from an offset);
        // until then, to the start of their buffers.
        let audio = (self.in_bufs.iter_mut().zip(&layout.audio_in))
            .chain(self.out_bufs.iter_mut().zip(&layout.audio_out));
        for (buf, &port) in audio {
            // SAFETY: the buffers are boxed and never moved.
            unsafe { instance.connect_port_mut(port, buf.as_mut_ptr()) };
        }
    }

    /// Reads what the plugin wrote to its atom outputs after a run: whether
    /// it says its state changed, and (while its UI is open) the atoms for
    /// it. Real-time safe.
    fn read_atom_outputs(&mut self) {
        let ui_open = self.link.ui_open.load(Ordering::Acquire);
        let (sequence, objects, state_changed) = (self.sequence, self.objects, self.state_changed);
        let link = &self.link;
        let mut to_ui = self.ui.as_mut().filter(|_| ui_open).map(|(to_ui, _)| to_ui);
        for output in self.atoms.iter_mut().filter(|a| a.output) {
            let port = output.port as u32;
            output.read_events(sequence, |_, atom| {
                let word = |at: usize| {
                    atom.get(at..at + 4)
                        .map(|w| u32::from_ne_bytes(w.try_into().expect("4 bytes")))
                };
                if word(4).is_some_and(|kind| objects.contains(&kind))
                    && word(12) == Some(state_changed)
                {
                    link.dirty.store(true, Ordering::Release);
                }
                if let Some(to_ui) = &mut to_ui {
                    // A UI that's behind misses some.
                    to_ui.write(port, &[atom]);
                }
            });
        }
    }

    /// Runs the plugin for `frames` frames starting `from` into the
    /// buffers. Real-time safe.
    fn run(&mut self, from: usize, frames: usize) {
        let Some(active) = &mut self.instance else {
            return;
        };
        let instance = active.instance_mut();
        for (buf, &port) in self.in_bufs.iter_mut().zip(&self.layout.audio_in) {
            // SAFETY: the buffer holds MAX_BLOCK frames and `from + frames`
            // is within it.
            unsafe { instance.connect_port_mut(port, buf[from..].as_mut_ptr()) };
        }
        for (buf, &port) in self.out_bufs.iter_mut().zip(&self.layout.audio_out) {
            // SAFETY: as above.
            unsafe { instance.connect_port_mut(port, buf[from..].as_mut_ptr()) };
        }
        // SAFETY: every port is connected to a buffer of at least `frames`.
        assert_no_alloc::permit_alloc(|| unsafe { active.run(frames) });
    }
}

impl PluginProcessor for Lv2Processor {
    fn process(
        &mut self,
        audio: [&mut [f32]; 2],
        context: ProcessContext,
        events: &[PluginEvent],
        mut midi_out: MidiSink,
    ) {
        let frames = audio[0].len().min(MAX_BLOCK);
        let [left, right] = audio;
        let ProcessContext {
            transport,
            sidechain,
            aux,
        } = context;
        // The track's signal into the main inputs, the sidechain into its
        // own; silence elsewhere.
        for buf in &mut self.in_bufs {
            buf[..frames].fill(0.0);
        }
        let roles = &self.layout.roles;
        let feeds = [
            (&roles.main_in, Some([&*left, &*right])),
            (&roles.side_in, sidechain),
        ];
        for (ports, source) in feeds {
            let Some([l, r]) = source else {
                continue;
            };
            match ports[..] {
                [mono] => {
                    for (s, (l, r)) in self.in_bufs[mono].iter_mut().zip(l.iter().zip(r.iter())) {
                        *s = (l + r) * 0.5;
                    }
                }
                [a, b] => {
                    self.in_bufs[a][..frames].copy_from_slice(&l[..frames]);
                    self.in_bufs[b][..frames].copy_from_slice(&r[..frames]);
                }
                _ => {}
            }
        }
        // Split the block where parameters change, so they apply on their
        // frame; MIDI events carry their own times. `params` walks the
        // events for parameter changes, `midi` for everything else.
        let (mut params, mut midi) = (0, 0);
        let mut from = 0;
        while from < frames {
            while let Some(event) = events.get(params).filter(|e| e.offset as usize <= from) {
                if let PluginEventKind::Param { index, value } = event.kind
                    && let Some(&port) = self.layout.params.get(index as usize)
                {
                    let (lo, hi) = self.layout.param_limits[index as usize];
                    self.controls[port] = (value as f32).max(lo).min(hi);
                }
                params += 1;
            }
            let to = events[params..]
                .iter()
                .find(|e| matches!(e.kind, PluginEventKind::Param { .. }))
                .map_or(frames, |e| (e.offset as usize).clamp(from + 1, frames));
            let (sequence, chunk, midi_event) = (self.sequence, self.chunk, self.midi_event);
            for atom in &mut self.atoms {
                atom.reset(sequence, chunk);
            }
            // The transport, first thing in the block, when it moved.
            let transport = &transport;
            if from == 0
                && let Some(i) = self.time_atom
                && transport_moved(self.sent_transport.as_ref(), transport)
            {
                let mut body = [0u8; 256];
                let len = self.time.position(transport, &mut body);
                self.atoms[i].push(0, self.time.object, &body[..len]);
            }
            // What the UI sent, first thing in the block too.
            if from == 0
                && let Some((_, from_ui)) = &mut self.ui
            {
                let atoms = &mut self.atoms;
                from_ui.read(|port, parts| {
                    if let Some(input) = atoms
                        .iter_mut()
                        .find(|a| a.port == port as usize && !a.output)
                    {
                        input.push_atom(0, parts);
                    }
                });
            }
            while let Some(event) = events.get(midi).filter(|e| (e.offset as usize) < to) {
                midi += 1;
                let data = match event.kind {
                    PluginEventKind::NoteOn {
                        channel,
                        key,
                        velocity,
                    } => [
                        0x90 | channel,
                        key,
                        (velocity * 127.0).round().clamp(1.0, 127.0) as u8,
                    ],
                    PluginEventKind::NoteOff { channel, key } => [0x80 | channel, key, 0],
                    PluginEventKind::Midi { data } => data,
                    // LV2 MIDI is MIDI 1.0, which has no per-note controllers.
                    PluginEventKind::Param { .. } | PluginEventKind::PerNote { .. } => continue,
                };
                if self.layout.midi_in.is_some() {
                    let frame = event.offset.saturating_sub(from as u32);
                    let len = dawkey_engine::midi_message_len(data[0]);
                    self.atoms[0].push(frame, midi_event, &data[..len]);
                }
            }
            self.run(from, to - from);
            if let Some(worker) = &mut self.worker {
                // Plugins may allocate in their responses; the engine may not.
                assert_no_alloc::permit_alloc(|| worker.end_run());
            }
            self.read_atom_outputs();
            if self.layout.midi_out.is_some() {
                let out = usize::from(self.layout.midi_in.is_some());
                self.atoms[out].read_midi(sequence, midi_event, |frame, data| {
                    let offset = (from as u32 + frame).min(to as u32 - 1);
                    midi_out.push(offset, PluginEventKind::from_midi(data));
                });
            }
            from = to;
        }
        if self.time_atom.is_some() {
            self.sent_transport = Some((transport, frames));
        }
        for (value, &port) in self.link.outputs.iter().zip(&self.layout.control_out) {
            value.store(self.controls[port].to_bits(), Ordering::Relaxed);
        }
        let roles = &self.layout.roles;
        let out = &self.out_bufs;
        match roles.main_out[..] {
            // No audio output: the signal passes by.
            [] => {}
            [mono] => {
                left[..frames].copy_from_slice(&out[mono][..frames]);
                right[..frames].copy_from_slice(&out[mono][..frames]);
            }
            [a, b, ..] => {
                left[..frames].copy_from_slice(&out[a][..frames]);
                right[..frames].copy_from_slice(&out[b][..frames]);
            }
        }
        // The other outputs that go somewhere.
        for (n, buffer) in aux.iter_mut().enumerate() {
            let [l, r] = &mut buffer.channels;
            match roles.other_out.get(n).map(|(_, ports)| &ports[..]) {
                Some(&[mono]) => {
                    l[..frames].copy_from_slice(&out[mono][..frames]);
                    r[..frames].copy_from_slice(&out[mono][..frames]);
                }
                Some(&[a, b]) => {
                    l[..frames].copy_from_slice(&out[a][..frames]);
                    r[..frames].copy_from_slice(&out[b][..frames]);
                }
                _ => {
                    l[..frames].fill(0.0);
                    r[..frames].fill(0.0);
                }
            }
        }
    }

    fn stop(&mut self) {}

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

impl Drop for Lv2Processor {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        // The worker stops before the plugin goes; what it points at goes
        // after (its field is dropped after this).
        if let Some(worker) = &mut self.worker {
            worker.stop();
        }
        if let Some(active) = self.instance.take() {
            // SAFETY: called on the control thread, once.
            drop(unsafe { active.deactivate() });
        }
    }
}
