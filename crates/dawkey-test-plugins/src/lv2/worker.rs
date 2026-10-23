// SPDX-License-Identifier: AGPL-3.0-or-later
//! `urn:dawkey:test:worker` and its UI: what hosts do with the LV2 worker,
//! atom messages between a plugin and its UI, output ports and atom buffer
//! sizes, in a form tests can read.
//!
//! The plugin adds a level to its audio as DC. The level is only ever set
//! by its worker: a note on (velocity / 127) or a `patch:Set` of
//! `urn:dawkey:test:worker#level` (an `atom:Float`) on its control port,
//! and restoring its state, schedule work; the work responds with the same
//! value, and the response sets the level. After a response it says so on
//! its notify port (a `patch:Set` of the level, then `state:StateChanged`),
//! but only if the host gave that port the 100 000 bytes it asks for. Its
//! `meter` output is the loudest sample of its last run. Its `echo` input
//! does nothing; it's for the UI to report what it got.
//!
//! The UI sets the level to 0.5 when it opens (finding `echo` with
//! `ui:portMap`), sets `echo` to each `meter` value above 0 it's sent, and
//! to ten times the level of each `patch:Set` it gets from the notify port.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;

use super::{
    Atom, Descriptor, Feature, Handle, RetrieveFn, StateInterface, StoreFn, find_feature, map_uri,
};

/// What the notify port asks for (`rsz:minimumSize`).
const NOTIFY_SIZE: usize = 100_000;

pub(super) static WORKER: Descriptor = Descriptor {
    uri: c"urn:dawkey:test:worker".as_ptr(),
    instantiate: worker_instantiate,
    connect_port: worker_connect,
    activate: None,
    run: worker_run,
    deactivate: None,
    cleanup: worker_cleanup,
    extension_data: Some(worker_extension_data),
};

/// The URIDs both the plugin and its UI use.
#[derive(Clone, Copy)]
struct Urids {
    midi_event: u32,
    object: u32,
    float: u32,
    urid: u32,
    sequence: u32,
    patch_set: u32,
    property: u32,
    value: u32,
    level: u32,
    state_changed: u32,
    event_transfer: u32,
}

impl Urids {
    /// # Safety
    /// `features` must be a null-terminated array of valid features.
    unsafe fn new(features: *const *const Feature) -> Option<Self> {
        // SAFETY: as the caller promises.
        let map = |uri: &CStr| unsafe { map_uri(features, uri) };
        Some(Self {
            midi_event: map(c"http://lv2plug.in/ns/ext/midi#MidiEvent")?,
            object: map(c"http://lv2plug.in/ns/ext/atom#Object")?,
            float: map(c"http://lv2plug.in/ns/ext/atom#Float")?,
            urid: map(c"http://lv2plug.in/ns/ext/atom#URID")?,
            sequence: map(c"http://lv2plug.in/ns/ext/atom#Sequence")?,
            patch_set: map(c"http://lv2plug.in/ns/ext/patch#Set")?,
            property: map(c"http://lv2plug.in/ns/ext/patch#property")?,
            value: map(c"http://lv2plug.in/ns/ext/patch#value")?,
            level: map(c"urn:dawkey:test:worker#level")?,
            state_changed: map(c"http://lv2plug.in/ns/ext/state#StateChanged")?,
            event_transfer: map(c"http://lv2plug.in/ns/ext/atom#eventTransfer")?,
        })
    }

    /// A `patch:Set` of the level to `level`: the atom's header and body.
    fn set_level(&self, level: f32) -> [u8; 64] {
        let words: [u32; 16] = [
            56,
            self.object,
            0,
            self.patch_set,
            self.property,
            0,
            4,
            self.urid,
            self.level,
            0,
            self.value,
            0,
            4,
            self.float,
            level.to_bits(),
            0,
        ];
        let mut bytes = [0u8; 64];
        for (chunk, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
            *chunk = word.to_ne_bytes();
        }
        bytes
    }

    /// The level in `atom` (header and body), if it's a `patch:Set` of it.
    fn level_set(&self, atom: &[u8]) -> Option<f32> {
        let word = |at: usize| {
            atom.get(at..at + 4)
                .map(|w| u32::from_ne_bytes(w.try_into().expect("4 bytes")))
        };
        if word(4)? != self.object || word(12)? != self.patch_set {
            return None;
        }
        let end = (8 + word(0)? as usize).min(atom.len());
        let (mut property, mut value) = (None, None);
        let mut at = 16;
        while at + 16 <= end {
            let (key, size, kind) = (word(at)?, word(at + 8)? as usize, word(at + 12)?);
            match (key, kind) {
                (k, t) if k == self.property && t == self.urid => property = word(at + 16),
                (k, t) if k == self.value && t == self.float => {
                    value = word(at + 16).map(f32::from_bits);
                }
                _ => {}
            }
            at += 16 + size.div_ceil(8) * 8;
        }
        (property == Some(self.level)).then_some(value).flatten()
    }
}

/// `LV2_Worker_Schedule`.
#[repr(C)]
struct Schedule {
    handle: *mut c_void,
    schedule_work: unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> u32,
}

type RespondFn = unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> u32;

/// `LV2_Worker_Interface`.
#[repr(C)]
struct WorkerInterface {
    work: unsafe extern "C" fn(Handle, RespondFn, *mut c_void, u32, *const c_void) -> u32,
    work_response: unsafe extern "C" fn(Handle, u32, *const c_void) -> u32,
    end_run: Option<unsafe extern "C" fn(Handle) -> u32>,
}

static WORKER_INTERFACE: WorkerInterface = WorkerInterface {
    work,
    work_response,
    end_run: None,
};

static WORKER_STATE: StateInterface = StateInterface {
    save: worker_save,
    restore: worker_restore,
};

unsafe extern "C" fn worker_extension_data(uri: *const c_char) -> *const c_void {
    // SAFETY: hosts pass a NUL-terminated URI.
    match unsafe { CStr::from_ptr(uri) }.to_bytes() {
        b"http://lv2plug.in/ns/ext/worker#interface" => ptr::from_ref(&WORKER_INTERFACE).cast(),
        b"http://lv2plug.in/ns/ext/state#interface" => ptr::from_ref(&WORKER_STATE).cast(),
        _ => ptr::null(),
    }
}

struct WorkerPlugin {
    control: *const Atom,
    notify: *mut Atom,
    audio: [*mut f32; 4],
    echo: *const f32,
    meter: *mut f32,
    urids: Urids,
    schedule: *const Schedule,
    level: f32,
    /// A response came: say so on the notify port.
    notify_level: bool,
}

impl WorkerPlugin {
    /// # Safety
    /// `schedule` must be the host's, live.
    unsafe fn schedule(&self, schedule: *const Schedule, level: f32) {
        // SAFETY: as the caller promises.
        unsafe {
            let schedule = &*schedule;
            (schedule.schedule_work)(schedule.handle, 4, ptr::from_ref(&level).cast());
        }
    }
}

unsafe extern "C" fn worker_instantiate(
    _descriptor: *const Descriptor,
    _rate: f64,
    _bundle: *const c_char,
    features: *const *const Feature,
) -> Handle {
    // SAFETY: `features` comes from the host.
    let (urids, schedule) = unsafe {
        (
            Urids::new(features),
            find_feature(features, c"http://lv2plug.in/ns/ext/worker#schedule"),
        )
    };
    let (Some(urids), Some(schedule)) = (urids, schedule) else {
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(WorkerPlugin {
        control: ptr::null(),
        notify: ptr::null_mut(),
        audio: [ptr::null_mut(); 4],
        echo: ptr::null(),
        meter: ptr::null_mut(),
        urids,
        schedule: schedule.cast(),
        level: 0.0,
        notify_level: false,
    }))
    .cast()
}

unsafe extern "C" fn worker_connect(handle: Handle, port: u32, data: *mut c_void) {
    // SAFETY: `handle` is a `WorkerPlugin` from `worker_instantiate`.
    let plugin = unsafe { &mut *handle.cast::<WorkerPlugin>() };
    match port {
        0 => plugin.control = data.cast(),
        1 => plugin.notify = data.cast(),
        2..=5 => plugin.audio[port as usize - 2] = data.cast(),
        6 => plugin.echo = data.cast(),
        7 => plugin.meter = data.cast(),
        _ => {}
    }
}

unsafe extern "C" fn worker_run(handle: Handle, frames: u32) {
    // SAFETY: every port is connected; the control port holds an atom
    // sequence whose events lie inside its declared size, and the notify
    // port a chunk of the size it says.
    unsafe {
        let plugin = &mut *handle.cast::<WorkerPlugin>();
        let urids = plugin.urids;
        let sequence = plugin.control;
        let total = (*sequence).size as usize;
        let base = sequence.cast::<u8>();
        let mut offset = 16usize;
        while offset + 16 <= total + 8 {
            let atom = &*base.add(offset + 8).cast::<Atom>();
            let bytes = std::slice::from_raw_parts(base.add(offset + 8), 8 + atom.size as usize);
            let body = &bytes[8..];
            if atom.kind == urids.midi_event
                && body.len() >= 3
                && body[0] & 0xf0 == 0x90
                && body[2] > 0
            {
                plugin.schedule(plugin.schedule, f32::from(body[2]) / 127.0);
            } else if let Some(level) = urids.level_set(bytes) {
                plugin.schedule(plugin.schedule, level);
            }
            offset += 16 + (atom.size as usize).div_ceil(8) * 8;
        }
        let [in_l, in_r, out_l, out_r] = plugin.audio;
        let mut meter = 0.0f32;
        for (input, output) in [(in_l, out_l), (in_r, out_r)] {
            for i in 0..frames as usize {
                let sample = *input.add(i) + plugin.level;
                *output.add(i) = sample;
                meter = meter.max(sample.abs());
            }
        }
        *plugin.meter = meter;
        // The notify port: a sequence, with what changed if there's room.
        let notify = plugin.notify.cast::<u8>();
        let capacity = (*plugin.notify).size as usize + 8;
        let write = |at: usize, bytes: &[u8]| {
            ptr::copy_nonoverlapping(bytes.as_ptr(), notify.add(at), bytes.len());
        };
        let mut used = 16usize;
        if plugin.notify_level && capacity >= NOTIFY_SIZE {
            plugin.notify_level = false;
            let mut changed = [0u8; 16];
            let words = [8, urids.object, 0, urids.state_changed];
            for (chunk, word) in changed.as_chunks_mut::<4>().0.iter_mut().zip(words) {
                *chunk = word.to_ne_bytes();
            }
            for event in [&urids.set_level(plugin.level)[..], &changed[..]] {
                write(used, &0i64.to_ne_bytes());
                write(used + 8, event);
                used += 8 + event.len().div_ceil(8) * 8;
            }
        }
        write(0, &((used - 8) as u32).to_ne_bytes());
        write(4, &urids.sequence.to_ne_bytes());
        write(8, &[0u8; 8]);
    }
}

unsafe extern "C" fn work(
    _handle: Handle,
    respond: RespondFn,
    respond_handle: *mut c_void,
    size: u32,
    data: *const c_void,
) -> u32 {
    // The work: hand the value back, as loading something would.
    // SAFETY: the host's respond function, with what it gave us.
    unsafe { respond(respond_handle, size, data) }
}

unsafe extern "C" fn work_response(handle: Handle, size: u32, data: *const c_void) -> u32 {
    // SAFETY: `handle` is a `WorkerPlugin`; `data` is what `work` responded.
    unsafe {
        let plugin = &mut *handle.cast::<WorkerPlugin>();
        if size == 4 {
            plugin.level = data.cast::<f32>().read_unaligned();
            plugin.notify_level = true;
        }
    }
    0
}

unsafe extern "C" fn worker_save(
    handle: Handle,
    store: StoreFn,
    state: *mut c_void,
    _flags: u32,
    _features: *const *const Feature,
) -> u32 {
    // SAFETY: `handle` is a `WorkerPlugin`; the host's store copies the
    // value.
    unsafe {
        let plugin = &*handle.cast::<WorkerPlugin>();
        let level = plugin.level;
        // LV2_STATE_IS_POD | LV2_STATE_IS_PORTABLE
        store(
            state,
            plugin.urids.level,
            ptr::from_ref(&level).cast(),
            4,
            plugin.urids.float,
            0b11,
        )
    }
}

unsafe extern "C" fn worker_restore(
    handle: Handle,
    retrieve: RetrieveFn,
    state: *mut c_void,
    _flags: u32,
    features: *const *const Feature,
) -> u32 {
    // SAFETY: `handle` is a `WorkerPlugin`; the host's value is valid during
    // this call, and its features are what they say.
    unsafe {
        let plugin = &mut *handle.cast::<WorkerPlugin>();
        let (mut size, mut kind, mut flags) = (0usize, 0u32, 0u32);
        let value = retrieve(state, plugin.urids.level, &mut size, &mut kind, &mut flags);
        if value.is_null() || size != 4 || kind != plugin.urids.float {
            return 0;
        }
        // Restoring goes through the worker too: the one the host gave
        // restore, if it did.
        let schedule = find_feature(features, c"http://lv2plug.in/ns/ext/worker#schedule")
            .map_or(plugin.schedule, |s| s.cast());
        plugin.schedule(schedule, value.cast::<f32>().read_unaligned());
    }
    0
}

unsafe extern "C" fn worker_cleanup(handle: Handle) {
    // SAFETY: `handle` came from `Box::into_raw` and is freed once.
    drop(unsafe { Box::from_raw(handle.cast::<WorkerPlugin>()) });
}

/// `LV2UI_Descriptor`.
#[repr(C)]
pub struct UiDescriptor {
    uri: *const c_char,
    instantiate: unsafe extern "C" fn(
        *const UiDescriptor,
        *const c_char,
        *const c_char,
        WriteFn,
        *mut c_void,
        *mut *mut c_void,
        *const *const Feature,
    ) -> Handle,
    cleanup: unsafe extern "C" fn(Handle),
    port_event: unsafe extern "C" fn(Handle, u32, u32, u32, *const c_void),
    extension_data: unsafe extern "C" fn(*const c_char) -> *const c_void,
}

// The descriptor is an immutable static.
unsafe impl Sync for UiDescriptor {}

type WriteFn = unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void);

/// `LV2UI_Port_Map`.
#[repr(C)]
struct PortMap {
    handle: *mut c_void,
    port_index: unsafe extern "C" fn(*mut c_void, *const c_char) -> u32,
}

#[repr(C)]
struct Idle {
    idle: unsafe extern "C" fn(Handle) -> c_int,
}

static UI: UiDescriptor = UiDescriptor {
    uri: c"urn:dawkey:test:worker#ui".as_ptr(),
    instantiate: ui_instantiate,
    cleanup: ui_cleanup,
    port_event: ui_port_event,
    extension_data: ui_extension_data,
};

static IDLE: Idle = Idle { idle: ui_idle };

/// The descriptor of UI `index` in this library.
///
/// # Safety
/// Called by LV2 hosts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lv2ui_descriptor(index: u32) -> *const UiDescriptor {
    if index == 0 { &UI } else { ptr::null() }
}

struct TestUi {
    write: WriteFn,
    controller: *mut c_void,
    urids: Urids,
    echo: u32,
}

impl TestUi {
    fn echo(&self, value: f32) {
        // SAFETY: the host's write function and controller.
        unsafe {
            (self.write)(
                self.controller,
                self.echo,
                4,
                0,
                ptr::from_ref(&value).cast(),
            );
        }
    }
}

unsafe extern "C" fn ui_instantiate(
    _descriptor: *const UiDescriptor,
    _plugin: *const c_char,
    _bundle: *const c_char,
    write: WriteFn,
    controller: *mut c_void,
    widget: *mut *mut c_void,
    features: *const *const Feature,
) -> Handle {
    // SAFETY: `features` comes from the host, the port map's data is a
    // `PortMap`, and `widget` is the host's to fill.
    unsafe {
        *widget = ptr::null_mut();
        let Some(urids) = Urids::new(features) else {
            return ptr::null_mut();
        };
        let Some(map) = find_feature(features, c"http://lv2plug.in/ns/extensions/ui#portMap")
        else {
            return ptr::null_mut();
        };
        let map = &*map.cast::<PortMap>();
        let echo = (map.port_index)(map.handle, c"echo".as_ptr());
        let ui = TestUi {
            write,
            controller,
            urids,
            echo,
        };
        let set = urids.set_level(0.5);
        write(
            controller,
            0,
            set.len() as u32,
            urids.event_transfer,
            set.as_ptr().cast(),
        );
        Box::into_raw(Box::new(ui)).cast()
    }
}

unsafe extern "C" fn ui_port_event(
    handle: Handle,
    port: u32,
    size: u32,
    format: u32,
    buffer: *const c_void,
) {
    // SAFETY: `handle` is a `TestUi`; the host's buffer holds `size` bytes.
    unsafe {
        let ui = &*handle.cast::<TestUi>();
        match (port, format) {
            (7, 0) if size == 4 => {
                let meter = buffer.cast::<f32>().read_unaligned();
                if meter > 0.0 {
                    ui.echo(meter);
                }
            }
            (1, f) if f == ui.urids.event_transfer => {
                let atom = std::slice::from_raw_parts(buffer.cast::<u8>(), size as usize);
                if let Some(level) = ui.urids.level_set(atom) {
                    ui.echo(level * 10.0);
                }
            }
            _ => {}
        }
    }
}

unsafe extern "C" fn ui_idle(_handle: Handle) -> c_int {
    0
}

unsafe extern "C" fn ui_extension_data(uri: *const c_char) -> *const c_void {
    // SAFETY: hosts pass a NUL-terminated URI.
    if unsafe { CStr::from_ptr(uri) } == c"http://lv2plug.in/ns/extensions/ui#idleInterface" {
        ptr::from_ref(&IDLE).cast()
    } else {
        ptr::null()
    }
}

unsafe extern "C" fn ui_cleanup(handle: Handle) {
    // SAFETY: `handle` came from `Box::into_raw` and is freed once.
    drop(unsafe { Box::from_raw(handle.cast::<TestUi>()) });
}
