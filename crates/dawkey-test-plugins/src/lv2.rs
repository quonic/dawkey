// SPDX-License-Identifier: AGPL-3.0-or-later
//! The LV2 test plugins, written against the raw C ABI.

mod worker;

use std::ffi::{CStr, c_char, c_void};
use std::ptr;

/// The bundle's `manifest.ttl`, describing every plugin in full.
pub const MANIFEST: &str = r#"@prefix atom: <http://lv2plug.in/ns/ext/atom#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix midi: <http://lv2plug.in/ns/ext/midi#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix patch: <http://lv2plug.in/ns/ext/patch#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix rsz: <http://lv2plug.in/ns/ext/resize-port#> .
@prefix state: <http://lv2plug.in/ns/ext/state#> .
@prefix time: <http://lv2plug.in/ns/ext/time#> .
@prefix ui: <http://lv2plug.in/ns/extensions/ui#> .
@prefix units: <http://lv2plug.in/ns/extensions/units#> .
@prefix urid: <http://lv2plug.in/ns/ext/urid#> .
@prefix work: <http://lv2plug.in/ns/ext/worker#> .

<urn:dawkey:test:gain>
    a lv2:Plugin , lv2:AmplifierPlugin ;
    lv2:binary <libdawkey_test_plugins.so> ;
    doap:name "Test Gain (LV2)" ;
    lv2:optionalFeature lv2:hardRTCapable ;
    lv2:port [
        a lv2:InputPort , lv2:AudioPort ; lv2:index 0 ; lv2:symbol "in_l" ; lv2:name "In L"
    ] , [
        a lv2:InputPort , lv2:AudioPort ; lv2:index 1 ; lv2:symbol "in_r" ; lv2:name "In R"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 2 ; lv2:symbol "out_l" ; lv2:name "Out L"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 3 ; lv2:symbol "out_r" ; lv2:name "Out R"
    ] , [
        a lv2:InputPort , lv2:ControlPort ; lv2:index 4 ; lv2:symbol "gain" ; lv2:name "Gain" ;
        lv2:default 0.0 ; lv2:minimum -60.0 ; lv2:maximum 12.0 ; units:unit units:db
    ] .

<urn:dawkey:test:beep>
    a lv2:Plugin , lv2:InstrumentPlugin ;
    lv2:binary <libdawkey_test_plugins.so> ;
    doap:name "Test Beep (LV2)" ;
    lv2:requiredFeature urid:map ;
    lv2:port [
        a lv2:InputPort , atom:AtomPort ; atom:bufferType atom:Sequence ;
        atom:supports midi:MidiEvent ;
        lv2:index 0 ; lv2:symbol "midi_in" ; lv2:name "MIDI In"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 1 ; lv2:symbol "out_l" ; lv2:name "Out L"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 2 ; lv2:symbol "out_r" ; lv2:name "Out R"
    ] , [
        a lv2:InputPort , lv2:ControlPort ; lv2:index 3 ; lv2:symbol "level" ; lv2:name "Level" ;
        lv2:default 1.0 ; lv2:minimum 0.0 ; lv2:maximum 1.0 ;
        lv2:scalePoint [ rdfs:label "Off" ; rdf:value 0.0 ] , [ rdfs:label "Full" ; rdf:value 1.0 ]
    ] .

<urn:dawkey:test:octave>
    a lv2:Plugin , lv2:MIDIPlugin ;
    lv2:binary <libdawkey_test_plugins.so> ;
    doap:name "Test Octave (LV2)" ;
    lv2:requiredFeature urid:map ;
    ui:ui <urn:dawkey:test:octave#qt> ;
    lv2:port [
        a lv2:InputPort , atom:AtomPort ; atom:bufferType atom:Sequence ;
        atom:supports midi:MidiEvent ;
        lv2:index 0 ; lv2:symbol "midi_in" ; lv2:name "MIDI In"
    ] , [
        a lv2:OutputPort , atom:AtomPort ; atom:bufferType atom:Sequence ;
        atom:supports midi:MidiEvent ;
        lv2:index 1 ; lv2:symbol "midi_out" ; lv2:name "MIDI Out"
    ] .

<urn:dawkey:test:memo#memo>
    a lv2:Parameter ;
    rdfs:label "Memo" ;
    rdfs:range atom:Float .

<urn:dawkey:test:memo>
    a lv2:Plugin ;
    lv2:binary <libdawkey_test_plugins.so> ;
    doap:name "Test Memo (LV2)" ;
    lv2:requiredFeature urid:map ;
    lv2:extensionData state:interface ;
    patch:writable <urn:dawkey:test:memo#memo> ;
    ui:ui <urn:dawkey:test:memo#gtk> ;
    lv2:port [
        a lv2:InputPort , atom:AtomPort ; atom:bufferType atom:Sequence ;
        atom:supports midi:MidiEvent ;
        lv2:index 0 ; lv2:symbol "midi_in" ; lv2:name "MIDI In"
    ] , [
        a lv2:InputPort , lv2:AudioPort ; lv2:index 1 ; lv2:symbol "in_l" ; lv2:name "In L"
    ] , [
        a lv2:InputPort , lv2:AudioPort ; lv2:index 2 ; lv2:symbol "in_r" ; lv2:name "In R"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 3 ; lv2:symbol "out_l" ; lv2:name "Out L"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 4 ; lv2:symbol "out_r" ; lv2:name "Out R"
    ] .

<urn:dawkey:test:tempo>
    a lv2:Plugin ;
    lv2:binary <libdawkey_test_plugins.so> ;
    doap:name "Test Tempo (LV2)" ;
    lv2:requiredFeature urid:map ;
    lv2:port [
        a lv2:InputPort , atom:AtomPort ; atom:bufferType atom:Sequence ;
        atom:supports time:Position ;
        lv2:index 0 ; lv2:symbol "control" ; lv2:name "Control"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 1 ; lv2:symbol "out_l" ; lv2:name "Out L"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 2 ; lv2:symbol "out_r" ; lv2:name "Out R"
    ] , [
        a lv2:InputPort , lv2:ControlPort ; lv2:index 3 ; lv2:symbol "count_from" ;
        lv2:name "Count bars from" ; lv2:portProperty lv2:integer , lv2:enumeration ;
        lv2:default 1 ; lv2:minimum 0 ; lv2:maximum 1 ;
        lv2:scalePoint [ rdfs:label "One" ; rdf:value 1 ] , [ rdfs:label "Zero" ; rdf:value 0 ]
    ] .

<urn:dawkey:test:subtract>
    a lv2:Plugin ;
    lv2:binary <libdawkey_test_plugins.so> ;
    doap:name "Test Subtract (LV2)" ;
    lv2:port [
        a lv2:InputPort , lv2:AudioPort ; lv2:index 0 ; lv2:symbol "in_l" ; lv2:name "In L"
    ] , [
        a lv2:InputPort , lv2:AudioPort ; lv2:index 1 ; lv2:symbol "in_r" ; lv2:name "In R"
    ] , [
        a lv2:InputPort , lv2:AudioPort ; lv2:index 2 ; lv2:symbol "sc_l" ; lv2:name "Sidechain L" ;
        lv2:portProperty lv2:isSideChain
    ] , [
        a lv2:InputPort , lv2:AudioPort ; lv2:index 3 ; lv2:symbol "sc_r" ; lv2:name "Sidechain R" ;
        lv2:portProperty lv2:isSideChain
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 4 ; lv2:symbol "out_l" ; lv2:name "Out L"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 5 ; lv2:symbol "out_r" ; lv2:name "Out R"
    ] .

<urn:dawkey:test:split>
    a lv2:Plugin , lv2:InstrumentPlugin ;
    lv2:binary <libdawkey_test_plugins.so> ;
    doap:name "Test Split (LV2)" ;
    lv2:requiredFeature urid:map ;
    lv2:port [
        a lv2:InputPort , atom:AtomPort ; atom:bufferType atom:Sequence ;
        atom:supports midi:MidiEvent ;
        lv2:index 0 ; lv2:symbol "midi_in" ; lv2:name "MIDI In"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 1 ; lv2:symbol "out_l" ; lv2:name "Out L"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 2 ; lv2:symbol "out_r" ; lv2:name "Out R"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 3 ; lv2:symbol "high_l" ; lv2:name "High L"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 4 ; lv2:symbol "high_r" ; lv2:name "High R"
    ] .

<urn:dawkey:test:worker>
    a lv2:Plugin ;
    lv2:binary <libdawkey_test_plugins.so> ;
    doap:name "Test Worker (LV2)" ;
    lv2:requiredFeature urid:map , work:schedule ;
    lv2:extensionData work:interface , state:interface ;
    ui:ui <urn:dawkey:test:worker#gtk> , <urn:dawkey:test:worker#qt> , <urn:dawkey:test:worker#ui> ;
    lv2:port [
        a lv2:InputPort , atom:AtomPort ; atom:bufferType atom:Sequence ;
        atom:supports midi:MidiEvent , patch:Message ;
        lv2:index 0 ; lv2:symbol "control" ; lv2:name "Control"
    ] , [
        a lv2:OutputPort , atom:AtomPort ; atom:bufferType atom:Sequence ;
        atom:supports patch:Message ; rsz:minimumSize 100000 ;
        lv2:index 1 ; lv2:symbol "notify" ; lv2:name "Notify"
    ] , [
        a lv2:InputPort , lv2:AudioPort ; lv2:index 2 ; lv2:symbol "in_l" ; lv2:name "In L"
    ] , [
        a lv2:InputPort , lv2:AudioPort ; lv2:index 3 ; lv2:symbol "in_r" ; lv2:name "In R"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 4 ; lv2:symbol "out_l" ; lv2:name "Out L"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 5 ; lv2:symbol "out_r" ; lv2:name "Out R"
    ] , [
        a lv2:InputPort , lv2:ControlPort ; lv2:index 6 ; lv2:symbol "echo" ; lv2:name "Echo" ;
        lv2:default 0.0 ; lv2:minimum 0.0 ; lv2:maximum 10.0
    ] , [
        a lv2:OutputPort , lv2:ControlPort ; lv2:index 7 ; lv2:symbol "meter" ; lv2:name "Meter"
    ] .

<urn:dawkey:test:memo#gtk>
    a ui:GtkUI ;
    ui:binary <libdawkey_test_plugins.so> .

<urn:dawkey:test:worker#gtk>
    a ui:Gtk3UI ;
    ui:binary <libdawkey_test_plugins.so> .

<urn:dawkey:test:worker#qt>
    a ui:Qt6UI ;
    ui:binary <libdawkey_test_plugins.so> .

<urn:dawkey:test:octave#qt>
    a ui:Qt5UI ;
    ui:binary <libdawkey_test_plugins.so> .

<urn:dawkey:test:worker#ui>
    a ui:X11UI ;
    ui:binary <libdawkey_test_plugins.so> ;
    lv2:requiredFeature urid:map , ui:portMap ;
    lv2:extensionData ui:idleInterface .
"#;

type Handle = *mut c_void;

#[repr(C)]
pub struct Descriptor {
    uri: *const c_char,
    instantiate: unsafe extern "C" fn(
        *const Descriptor,
        f64,
        *const c_char,
        *const *const Feature,
    ) -> Handle,
    connect_port: unsafe extern "C" fn(Handle, u32, *mut c_void),
    activate: Option<unsafe extern "C" fn(Handle)>,
    run: unsafe extern "C" fn(Handle, u32),
    deactivate: Option<unsafe extern "C" fn(Handle)>,
    cleanup: unsafe extern "C" fn(Handle),
    /// Plugins without extensions may leave it null.
    extension_data: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
}

// The descriptors are immutable statics.
unsafe impl Sync for Descriptor {}

#[repr(C)]
pub struct Feature {
    uri: *const c_char,
    data: *mut c_void,
}

#[repr(C)]
struct UridMap {
    handle: *mut c_void,
    map: unsafe extern "C" fn(*mut c_void, *const c_char) -> u32,
}

#[repr(C)]
struct Atom {
    size: u32,
    kind: u32,
}

/// The descriptor of plugin `index` in this library.
///
/// # Safety
/// Called by LV2 hosts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lv2_descriptor(index: u32) -> *const Descriptor {
    match index {
        0 => &GAIN,
        1 => &BEEP,
        2 => &OCTAVE,
        3 => &MEMO,
        4 => &TEMPO,
        5 => &SUBTRACT,
        6 => &SPLIT,
        7 => &worker::WORKER,
        _ => ptr::null(),
    }
}

unsafe extern "C" fn extension_data(_uri: *const c_char) -> *const c_void {
    ptr::null()
}

static GAIN: Descriptor = Descriptor {
    uri: c"urn:dawkey:test:gain".as_ptr(),
    instantiate: gain_instantiate,
    connect_port: gain_connect,
    activate: Some(gain_activate),
    run: gain_run,
    deactivate: None,
    cleanup: gain_cleanup,
    // Like the SWH plugins: no extensions, so none.
    extension_data: None,
};

/// Like the SWH plugins, it reads its controls when activated: hosts must
/// connect them first.
unsafe extern "C" fn gain_activate(handle: Handle) {
    // SAFETY: `handle` is a `GainPlugin`; the gain port is connected.
    unsafe {
        let plugin = &mut *handle.cast::<GainPlugin>();
        // Volatile, so it happens: a host that hasn't connected the port
        // crashes here.
        plugin.activated_at = plugin.ports[4].read_volatile();
    }
}

#[derive(Default)]
struct GainPlugin {
    ports: [*mut f32; 5],
    /// The gain when it was activated.
    activated_at: f32,
}

unsafe extern "C" fn gain_instantiate(
    _descriptor: *const Descriptor,
    _rate: f64,
    _bundle: *const c_char,
    _features: *const *const Feature,
) -> Handle {
    Box::into_raw(Box::new(GainPlugin {
        ports: [ptr::null_mut(); 5],
        activated_at: 0.0,
    }))
    .cast()
}

unsafe extern "C" fn gain_connect(handle: Handle, port: u32, data: *mut c_void) {
    // SAFETY: `handle` is a `GainPlugin` from `gain_instantiate`.
    let plugin = unsafe { &mut *handle.cast::<GainPlugin>() };
    if let Some(slot) = plugin.ports.get_mut(port as usize) {
        *slot = data.cast();
    }
}

unsafe extern "C" fn gain_run(handle: Handle, frames: u32) {
    // SAFETY: the host connected every port to a buffer of `frames`
    // samples (one value for the control port) before running.
    unsafe {
        let plugin = &*handle.cast::<GainPlugin>();
        let [in_l, in_r, out_l, out_r, gain] = plugin.ports;
        let gain = 10f32.powf(*gain / 20.0);
        for (input, output) in [(in_l, out_l), (in_r, out_r)] {
            for i in 0..frames as usize {
                *output.add(i) = *input.add(i) * gain;
            }
        }
    }
}

unsafe extern "C" fn gain_cleanup(handle: Handle) {
    // SAFETY: `handle` came from `Box::into_raw` and is freed once.
    drop(unsafe { Box::from_raw(handle.cast::<GainPlugin>()) });
}

static BEEP: Descriptor = Descriptor {
    uri: c"urn:dawkey:test:beep".as_ptr(),
    instantiate: beep_instantiate,
    connect_port: beep_connect,
    activate: None,
    run: beep_run,
    deactivate: None,
    cleanup: beep_cleanup,
    extension_data: Some(extension_data),
};

struct BeepPlugin {
    midi: *const Atom,
    out: [*mut f32; 2],
    level: *const f32,
    midi_event: u32,
    /// Velocity (0 to 1) of each held key.
    held: [f32; 128],
}

/// Maps `uri` with the host's URID map feature, if it gave one.
///
/// # Safety
/// `features` must be a null-terminated array of valid features.
unsafe fn map_uri(features: *const *const Feature, uri: &CStr) -> Option<u32> {
    // SAFETY: as the caller promises; the map's data is a `UridMap`.
    unsafe {
        let map = &*find_feature(features, c"http://lv2plug.in/ns/ext/urid#map")?.cast::<UridMap>();
        Some((map.map)(map.handle, uri.as_ptr()))
    }
}

/// The data of the host's feature `uri`, if it gave it.
///
/// # Safety
/// `features` must be a null-terminated array of valid features (or null).
unsafe fn find_feature(features: *const *const Feature, uri: &CStr) -> Option<*mut c_void> {
    // SAFETY: as the caller promises.
    unsafe {
        let mut at = features;
        while !at.is_null() && !(*at).is_null() {
            let feature = &**at;
            if CStr::from_ptr(feature.uri) == uri {
                return Some(feature.data);
            }
            at = at.add(1);
        }
    }
    None
}

unsafe extern "C" fn beep_instantiate(
    _descriptor: *const Descriptor,
    _rate: f64,
    _bundle: *const c_char,
    features: *const *const Feature,
) -> Handle {
    // SAFETY: `features` comes from the host.
    let Some(midi_event) =
        (unsafe { map_uri(features, c"http://lv2plug.in/ns/ext/midi#MidiEvent") })
    else {
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(BeepPlugin {
        midi: ptr::null(),
        out: [ptr::null_mut(); 2],
        level: ptr::null(),
        midi_event,
        held: [0.0; 128],
    }))
    .cast()
}

unsafe extern "C" fn beep_connect(handle: Handle, port: u32, data: *mut c_void) {
    // SAFETY: `handle` is a `BeepPlugin` from `beep_instantiate`.
    let plugin = unsafe { &mut *handle.cast::<BeepPlugin>() };
    match port {
        0 => plugin.midi = data.cast(),
        1 => plugin.out[0] = data.cast(),
        2 => plugin.out[1] = data.cast(),
        3 => plugin.level = data.cast(),
        _ => {}
    }
}

unsafe extern "C" fn beep_run(handle: Handle, frames: u32) {
    // SAFETY: every port is connected; the MIDI port holds an atom sequence
    // whose events lie inside its declared size.
    unsafe {
        let plugin = &mut *handle.cast::<BeepPlugin>();
        let level = *plugin.level;
        let mut done = 0usize;
        let fill = |plugin: &BeepPlugin, from: usize, to: usize| {
            let value = plugin.held.iter().sum::<f32>() * level;
            for out in plugin.out {
                for i in from..to {
                    *out.add(i) = value;
                }
            }
        };
        let sequence = plugin.midi;
        let total = (*sequence).size as usize;
        // Atom header (8) + sequence body (unit, pad: 8), then events.
        let base = sequence.cast::<u8>();
        let mut offset = 16usize;
        while offset + 16 <= total + 8 {
            let time = base.add(offset).cast::<i64>().read_unaligned();
            let atom = &*base.add(offset + 8).cast::<Atom>();
            let body = std::slice::from_raw_parts(base.add(offset + 16), atom.size as usize);
            let at = (time.max(0) as usize).min(frames as usize);
            fill(plugin, done, at);
            done = at;
            if atom.kind == plugin.midi_event && body.len() >= 3 {
                let key = usize::from(body[1] & 127);
                match body[0] & 0xf0 {
                    0x90 if body[2] > 0 => plugin.held[key] = f32::from(body[2]) / 127.0,
                    0x80 | 0x90 => plugin.held[key] = 0.0,
                    _ => {}
                }
            }
            offset += 16 + (atom.size as usize).div_ceil(8) * 8;
        }
        fill(plugin, done, frames as usize);
    }
}

unsafe extern "C" fn beep_cleanup(handle: Handle) {
    // SAFETY: `handle` came from `Box::into_raw` and is freed once.
    drop(unsafe { Box::from_raw(handle.cast::<BeepPlugin>()) });
}

static OCTAVE: Descriptor = Descriptor {
    uri: c"urn:dawkey:test:octave".as_ptr(),
    instantiate: octave_instantiate,
    connect_port: octave_connect,
    activate: None,
    run: octave_run,
    deactivate: None,
    cleanup: octave_cleanup,
    extension_data: Some(extension_data),
};

/// Sends every MIDI message it gets on, notes an octave up (keys above 115
/// stay).
struct OctavePlugin {
    input: *const Atom,
    output: *mut Atom,
    midi_event: u32,
    sequence: u32,
}

unsafe extern "C" fn octave_instantiate(
    _descriptor: *const Descriptor,
    _rate: f64,
    _bundle: *const c_char,
    features: *const *const Feature,
) -> Handle {
    // SAFETY: `features` comes from the host.
    let urids = unsafe {
        (
            map_uri(features, c"http://lv2plug.in/ns/ext/midi#MidiEvent"),
            map_uri(features, c"http://lv2plug.in/ns/ext/atom#Sequence"),
        )
    };
    let (Some(midi_event), Some(sequence)) = urids else {
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(OctavePlugin {
        input: ptr::null(),
        output: ptr::null_mut(),
        midi_event,
        sequence,
    }))
    .cast()
}

unsafe extern "C" fn octave_connect(handle: Handle, port: u32, data: *mut c_void) {
    // SAFETY: `handle` is an `OctavePlugin` from `octave_instantiate`.
    let plugin = unsafe { &mut *handle.cast::<OctavePlugin>() };
    match port {
        0 => plugin.input = data.cast(),
        1 => plugin.output = data.cast(),
        _ => {}
    }
}

unsafe extern "C" fn octave_run(handle: Handle, _frames: u32) {
    // SAFETY: both ports are connected: the input holds an atom sequence
    // whose events lie inside its declared size, and the output's size is
    // the space the host gave for the body.
    unsafe {
        let plugin = &*handle.cast::<OctavePlugin>();
        let (input, output) = (plugin.input.cast::<u8>(), plugin.output.cast::<u8>());
        let capacity = (*plugin.output).size as usize;
        // The body starts with the unit and pad (8 bytes), then events.
        output.add(8).write_bytes(0, 8);
        let mut written = 8usize;
        let total = (*plugin.input).size as usize;
        let mut offset = 16usize;
        while offset + 16 <= total + 8 {
            let time = input.add(offset).cast::<i64>().read_unaligned();
            let atom = &*input.add(offset + 8).cast::<Atom>();
            let size = atom.size as usize;
            let padded = size.div_ceil(8) * 8;
            if atom.kind == plugin.midi_event && (1..=3).contains(&size) {
                if written + 16 + padded > capacity {
                    break;
                }
                let mut data = [0u8; 8];
                ptr::copy_nonoverlapping(input.add(offset + 16), data.as_mut_ptr(), size);
                if size == 3 && matches!(data[0] & 0xf0, 0x80 | 0x90) && data[1] <= 115 {
                    data[1] += 12;
                }
                let at = output.add(8 + written);
                at.cast::<i64>().write_unaligned(time);
                at.add(8).cast::<u32>().write_unaligned(atom.size);
                at.add(12).cast::<u32>().write_unaligned(atom.kind);
                ptr::copy_nonoverlapping(data.as_ptr(), at.add(16), padded);
                written += 16 + padded;
            }
            offset += 16 + padded;
        }
        (*plugin.output).size = written as u32;
        (*plugin.output).kind = plugin.sequence;
    }
}

unsafe extern "C" fn octave_cleanup(handle: Handle) {
    // SAFETY: `handle` came from `Box::into_raw` and is freed once.
    drop(unsafe { Box::from_raw(handle.cast::<OctavePlugin>()) });
}

static MEMO: Descriptor = Descriptor {
    uri: c"urn:dawkey:test:memo".as_ptr(),
    instantiate: memo_instantiate,
    connect_port: memo_connect,
    activate: None,
    run: memo_run,
    deactivate: None,
    cleanup: memo_cleanup,
    extension_data: Some(memo_extension_data),
};

/// `LV2_State_Store_Function`.
type StoreFn = unsafe extern "C" fn(*mut c_void, u32, *const c_void, usize, u32, u32) -> u32;
/// `LV2_State_Retrieve_Function`.
type RetrieveFn =
    unsafe extern "C" fn(*mut c_void, u32, *mut usize, *mut u32, *mut u32) -> *const c_void;

/// `LV2_State_Interface`.
#[repr(C)]
struct StateInterface {
    save: unsafe extern "C" fn(Handle, StoreFn, *mut c_void, u32, *const *const Feature) -> u32,
    restore:
        unsafe extern "C" fn(Handle, RetrieveFn, *mut c_void, u32, *const *const Feature) -> u32,
}

static MEMO_STATE: StateInterface = StateInterface {
    save: memo_save,
    restore: memo_restore,
};

unsafe extern "C" fn memo_extension_data(uri: *const c_char) -> *const c_void {
    // SAFETY: hosts pass a NUL-terminated URI.
    if unsafe { CStr::from_ptr(uri) } == c"http://lv2plug.in/ns/ext/state#interface" {
        std::ptr::from_ref(&MEMO_STATE).cast()
    } else {
        ptr::null()
    }
}

/// Like the CLAP memo: adds the velocity (0 to 1) of the last note it got
/// to its audio, and keeps it only in its state. Given `state:makePath`
/// and `state:mapPath` to save with, it keeps it in a file of its own
/// (`memo.txt`), and its state says where that is.
struct MemoPlugin {
    midi: *const Atom,
    audio: [*mut f32; 4],
    midi_event: u32,
    /// The state property's key, atom:Float and atom:Path.
    key: u32,
    float: u32,
    path: u32,
    memo: f32,
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

unsafe extern "C" {
    /// What hosts allocate paths with.
    fn free(ptr: *mut c_void);
}

/// A path the host made, as a string; frees it.
///
/// # Safety
/// `path` is null, or a NUL-terminated string from `malloc`.
unsafe fn take_path(path: *mut c_char) -> Option<String> {
    if path.is_null() {
        return None;
    }
    // SAFETY: as the caller promises.
    unsafe {
        let text = CStr::from_ptr(path).to_str().ok().map(str::to_owned);
        free(path.cast());
        text
    }
}

unsafe extern "C" fn memo_instantiate(
    _descriptor: *const Descriptor,
    _rate: f64,
    _bundle: *const c_char,
    features: *const *const Feature,
) -> Handle {
    // SAFETY: `features` comes from the host.
    let urids = unsafe {
        (
            map_uri(features, c"http://lv2plug.in/ns/ext/midi#MidiEvent"),
            map_uri(features, c"urn:dawkey:test:memo#memo"),
            map_uri(features, c"http://lv2plug.in/ns/ext/atom#Float"),
            map_uri(features, c"http://lv2plug.in/ns/ext/atom#Path"),
        )
    };
    let (Some(midi_event), Some(key), Some(float), Some(path)) = urids else {
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(MemoPlugin {
        midi: ptr::null(),
        audio: [ptr::null_mut(); 4],
        midi_event,
        key,
        float,
        path,
        memo: 0.0,
    }))
    .cast()
}

unsafe extern "C" fn memo_connect(handle: Handle, port: u32, data: *mut c_void) {
    // SAFETY: `handle` is a `MemoPlugin` from `memo_instantiate`.
    let plugin = unsafe { &mut *handle.cast::<MemoPlugin>() };
    match port {
        0 => plugin.midi = data.cast(),
        1..=4 => plugin.audio[port as usize - 1] = data.cast(),
        _ => {}
    }
}

unsafe extern "C" fn memo_run(handle: Handle, frames: u32) {
    // SAFETY: every port is connected; the MIDI port holds an atom sequence
    // whose events lie inside its declared size.
    unsafe {
        let plugin = &mut *handle.cast::<MemoPlugin>();
        let sequence = plugin.midi;
        let total = (*sequence).size as usize;
        let base = sequence.cast::<u8>();
        let mut offset = 16usize;
        while offset + 16 <= total + 8 {
            let atom = &*base.add(offset + 8).cast::<Atom>();
            let body = std::slice::from_raw_parts(base.add(offset + 16), atom.size as usize);
            if atom.kind == plugin.midi_event && body.len() >= 3 && body[0] & 0xf0 == 0x90 {
                plugin.memo = f32::from(body[2]) / 127.0;
            }
            offset += 16 + (atom.size as usize).div_ceil(8) * 8;
        }
        let [in_l, in_r, out_l, out_r] = plugin.audio;
        for (input, output) in [(in_l, out_l), (in_r, out_r)] {
            for i in 0..frames as usize {
                *output.add(i) = *input.add(i) + plugin.memo;
            }
        }
    }
}

unsafe extern "C" fn memo_save(
    handle: Handle,
    store: StoreFn,
    state: *mut c_void,
    _flags: u32,
    features: *const *const Feature,
) -> u32 {
    // SAFETY: `handle` is a `MemoPlugin`; the host's store copies the value,
    // and its features are what they say.
    unsafe {
        let plugin = &*handle.cast::<MemoPlugin>();
        let make = find_feature(features, c"http://lv2plug.in/ns/ext/state#makePath");
        let map = find_feature(features, c"http://lv2plug.in/ns/ext/state#mapPath");
        if let (Some(make), Some(map)) = (make, map) {
            let (make, map) = (&*make.cast::<MakePath>(), &*map.cast::<MapPath>());
            let Some(file) = take_path((make.path)(make.handle, c"memo.txt".as_ptr())) else {
                // LV2_STATE_ERR_NO_PROPERTY
                return 5;
            };
            if std::fs::write(&file, plugin.memo.to_string()).is_err() {
                return 1;
            }
            let file = std::ffi::CString::new(file).expect("no NUL");
            let Some(saved) = take_path((map.abstract_path)(map.handle, file.as_ptr())) else {
                return 1;
            };
            let saved = std::ffi::CString::new(saved).expect("no NUL");
            let bytes = saved.as_bytes_with_nul();
            // LV2_STATE_IS_POD
            return store(
                state,
                plugin.key,
                bytes.as_ptr().cast(),
                bytes.len(),
                plugin.path,
                0b1,
            );
        }
        let value = plugin.memo;
        // LV2_STATE_IS_POD | LV2_STATE_IS_PORTABLE
        store(
            state,
            plugin.key,
            std::ptr::from_ref(&value).cast(),
            4,
            plugin.float,
            0b11,
        )
    }
}

unsafe extern "C" fn memo_restore(
    handle: Handle,
    retrieve: RetrieveFn,
    state: *mut c_void,
    _flags: u32,
    features: *const *const Feature,
) -> u32 {
    // SAFETY: `handle` is a `MemoPlugin`; the host's value is valid during
    // this call, and its features are what they say.
    unsafe {
        let plugin = &mut *handle.cast::<MemoPlugin>();
        let (mut size, mut kind, mut flags) = (0usize, 0u32, 0u32);
        let value = retrieve(state, plugin.key, &mut size, &mut kind, &mut flags);
        if value.is_null() {
            return 0;
        }
        if size == 4 && kind == plugin.float {
            plugin.memo = value.cast::<f32>().read_unaligned();
        } else if kind == plugin.path {
            let Some(map) = find_feature(features, c"http://lv2plug.in/ns/ext/state#mapPath")
            else {
                return 1;
            };
            let map = &*map.cast::<MapPath>();
            let Some(file) = take_path((map.absolute_path)(map.handle, value.cast())) else {
                return 1;
            };
            match std::fs::read_to_string(file).map(|t| t.trim().parse::<f32>()) {
                Ok(Ok(memo)) => plugin.memo = memo,
                _ => return 1,
            }
        }
    }
    0
}

unsafe extern "C" fn memo_cleanup(handle: Handle) {
    // SAFETY: `handle` came from `Box::into_raw` and is freed once.
    drop(unsafe { Box::from_raw(handle.cast::<MemoPlugin>()) });
}

static TEMPO: Descriptor = Descriptor {
    uri: c"urn:dawkey:test:tempo".as_ptr(),
    instantiate: tempo_instantiate,
    connect_port: tempo_connect,
    activate: None,
    run: tempo_run,
    deactivate: None,
    cleanup: tempo_cleanup,
    extension_data: Some(extension_data),
};

/// Like the CLAP tempo: the left output is the tempo / 1000, the right one
/// (bar + 1) / 100 while rolling, from the `time:Position` objects it gets.
/// Its one control, a choice, counts bars from 0 instead.
struct TempoPlugin {
    control: *const Atom,
    out: [*mut f32; 2],
    count_from: *const f32,
    /// atom:Object, time:Position, time:speed, time:bar,
    /// time:beatsPerMinute.
    urids: [u32; 5],
    bpm: f32,
    bar: i64,
    speed: f32,
}

unsafe extern "C" fn tempo_instantiate(
    _descriptor: *const Descriptor,
    _rate: f64,
    _bundle: *const c_char,
    features: *const *const Feature,
) -> Handle {
    let uris = [
        c"http://lv2plug.in/ns/ext/atom#Object",
        c"http://lv2plug.in/ns/ext/time#Position",
        c"http://lv2plug.in/ns/ext/time#speed",
        c"http://lv2plug.in/ns/ext/time#bar",
        c"http://lv2plug.in/ns/ext/time#beatsPerMinute",
    ];
    let mut urids = [0; 5];
    for (urid, uri) in urids.iter_mut().zip(uris) {
        // SAFETY: `features` comes from the host.
        match unsafe { map_uri(features, uri) } {
            Some(id) => *urid = id,
            None => return ptr::null_mut(),
        }
    }
    Box::into_raw(Box::new(TempoPlugin {
        control: ptr::null(),
        out: [ptr::null_mut(); 2],
        count_from: ptr::null(),
        urids,
        bpm: 0.0,
        bar: 0,
        speed: 0.0,
    }))
    .cast()
}

unsafe extern "C" fn tempo_connect(handle: Handle, port: u32, data: *mut c_void) {
    // SAFETY: `handle` is a `TempoPlugin` from `tempo_instantiate`.
    let plugin = unsafe { &mut *handle.cast::<TempoPlugin>() };
    match port {
        0 => plugin.control = data.cast(),
        1 => plugin.out[0] = data.cast(),
        2 => plugin.out[1] = data.cast(),
        3 => plugin.count_from = data.cast(),
        _ => {}
    }
}

unsafe extern "C" fn tempo_run(handle: Handle, frames: u32) {
    // SAFETY: every port is connected; the control port holds an atom
    // sequence whose events, and their objects' properties, lie inside the
    // sizes they declare.
    unsafe {
        let plugin = &mut *handle.cast::<TempoPlugin>();
        let [object, position, speed, bar, bpm] = plugin.urids;
        let sequence = plugin.control.cast::<u8>();
        let total = (*plugin.control).size as usize;
        let mut offset = 16usize;
        while offset + 16 <= total + 8 {
            let atom = &*sequence.add(offset + 8).cast::<Atom>();
            let body = sequence.add(offset + 16);
            let size = atom.size as usize;
            if atom.kind == object && body.add(4).cast::<u32>().read_unaligned() == position {
                // Properties: key, context, then an atom; padded to 8.
                let mut at = 8usize;
                while at + 16 <= size {
                    let key = body.add(at).cast::<u32>().read_unaligned();
                    let value_size = body.add(at + 8).cast::<u32>().read_unaligned() as usize;
                    let value = body.add(at + 16);
                    if key == speed {
                        plugin.speed = value.cast::<f32>().read_unaligned();
                    } else if key == bar {
                        plugin.bar = value.cast::<i64>().read_unaligned();
                    } else if key == bpm {
                        plugin.bpm = value.cast::<f32>().read_unaligned();
                    }
                    at += 16 + value_size.div_ceil(8) * 8;
                }
            }
            offset += 16 + size.div_ceil(8) * 8;
        }
        let left = plugin.bpm / 1000.0;
        let right = if plugin.speed > 0.0 {
            (plugin.bar as f32 + *plugin.count_from) / 100.0
        } else {
            0.0
        };
        for (out, value) in plugin.out.into_iter().zip([left, right]) {
            for i in 0..frames as usize {
                *out.add(i) = value;
            }
        }
    }
}

unsafe extern "C" fn tempo_cleanup(handle: Handle) {
    // SAFETY: `handle` came from `Box::into_raw` and is freed once.
    drop(unsafe { Box::from_raw(handle.cast::<TempoPlugin>()) });
}

static SUBTRACT: Descriptor = Descriptor {
    uri: c"urn:dawkey:test:subtract".as_ptr(),
    instantiate: subtract_instantiate,
    connect_port: subtract_connect,
    activate: None,
    run: subtract_run,
    deactivate: None,
    cleanup: subtract_cleanup,
    extension_data: Some(extension_data),
};

/// Like the CLAP subtract: main input less the sidechain.
struct SubtractPlugin {
    ports: [*mut f32; 6],
}

unsafe extern "C" fn subtract_instantiate(
    _descriptor: *const Descriptor,
    _rate: f64,
    _bundle: *const c_char,
    _features: *const *const Feature,
) -> Handle {
    Box::into_raw(Box::new(SubtractPlugin {
        ports: [ptr::null_mut(); 6],
    }))
    .cast()
}

unsafe extern "C" fn subtract_connect(handle: Handle, port: u32, data: *mut c_void) {
    // SAFETY: `handle` is a `SubtractPlugin` from `subtract_instantiate`.
    let plugin = unsafe { &mut *handle.cast::<SubtractPlugin>() };
    if let Some(slot) = plugin.ports.get_mut(port as usize) {
        *slot = data.cast();
    }
}

unsafe extern "C" fn subtract_run(handle: Handle, frames: u32) {
    // SAFETY: the host connected every port to `frames` samples.
    unsafe {
        let plugin = &*handle.cast::<SubtractPlugin>();
        let [in_l, in_r, sc_l, sc_r, out_l, out_r] = plugin.ports;
        for (input, side, output) in [(in_l, sc_l, out_l), (in_r, sc_r, out_r)] {
            for i in 0..frames as usize {
                *output.add(i) = *input.add(i) - *side.add(i);
            }
        }
    }
}

unsafe extern "C" fn subtract_cleanup(handle: Handle) {
    // SAFETY: `handle` came from `Box::into_raw` and is freed once.
    drop(unsafe { Box::from_raw(handle.cast::<SubtractPlugin>()) });
}

static SPLIT: Descriptor = Descriptor {
    uri: c"urn:dawkey:test:split".as_ptr(),
    instantiate: split_instantiate,
    connect_port: split_connect,
    activate: None,
    run: split_run,
    deactivate: None,
    cleanup: split_cleanup,
    extension_data: Some(extension_data),
};

/// Like the CLAP split: keys below 60 on the main outputs, the rest on
/// "High".
struct SplitPlugin {
    midi: *const Atom,
    out: [*mut f32; 4],
    midi_event: u32,
    held: [f32; 128],
}

unsafe extern "C" fn split_instantiate(
    _descriptor: *const Descriptor,
    _rate: f64,
    _bundle: *const c_char,
    features: *const *const Feature,
) -> Handle {
    // SAFETY: `features` comes from the host.
    let Some(midi_event) =
        (unsafe { map_uri(features, c"http://lv2plug.in/ns/ext/midi#MidiEvent") })
    else {
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(SplitPlugin {
        midi: ptr::null(),
        out: [ptr::null_mut(); 4],
        midi_event,
        held: [0.0; 128],
    }))
    .cast()
}

unsafe extern "C" fn split_connect(handle: Handle, port: u32, data: *mut c_void) {
    // SAFETY: `handle` is a `SplitPlugin` from `split_instantiate`.
    let plugin = unsafe { &mut *handle.cast::<SplitPlugin>() };
    match port {
        0 => plugin.midi = data.cast(),
        1..=4 => plugin.out[port as usize - 1] = data.cast(),
        _ => {}
    }
}

unsafe extern "C" fn split_run(handle: Handle, frames: u32) {
    // SAFETY: every port is connected; the MIDI port holds an atom sequence
    // whose events lie inside its declared size.
    unsafe {
        let plugin = &mut *handle.cast::<SplitPlugin>();
        let sequence = plugin.midi;
        let total = (*sequence).size as usize;
        let base = sequence.cast::<u8>();
        let mut offset = 16usize;
        while offset + 16 <= total + 8 {
            let atom = &*base.add(offset + 8).cast::<Atom>();
            let body = std::slice::from_raw_parts(base.add(offset + 16), atom.size as usize);
            if atom.kind == plugin.midi_event && body.len() >= 3 {
                let key = usize::from(body[1] & 127);
                match body[0] & 0xf0 {
                    0x90 if body[2] > 0 => plugin.held[key] = f32::from(body[2]) / 127.0,
                    0x80 | 0x90 => plugin.held[key] = 0.0,
                    _ => {}
                }
            }
            offset += 16 + (atom.size as usize).div_ceil(8) * 8;
        }
        let low: f32 = plugin.held[..60].iter().sum();
        let high: f32 = plugin.held[60..].iter().sum();
        for (out, value) in plugin.out.into_iter().zip([low, low, high, high]) {
            for i in 0..frames as usize {
                *out.add(i) = value;
            }
        }
    }
}

unsafe extern "C" fn split_cleanup(handle: Handle) {
    // SAFETY: `handle` came from `Box::into_raw` and is freed once.
    drop(unsafe { Box::from_raw(handle.cast::<SplitPlugin>()) });
}
