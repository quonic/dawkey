// SPDX-License-Identifier: AGPL-3.0-or-later
//! Plays the engine through PipeWire.
//!
//! Dawkey appears as one `pw_filter` node with 32-bit float DSP ports
//! (`output_FL`, `output_FR`, `input_FL`, `input_FR`) and a MIDI input
//! (`midi_in`) for patching by hand. With autoconnect on, the [`router`]
//! links the outputs to WirePlumber's default sink, the inputs to the
//! default source while audio tracks are armed, and MIDI devices to ports of
//! their own so the engine knows which device each message came from or
//! goes to. Those are a fixed pool (`midi_in_1`…, `midi_out_1`…) made before
//! the filter connects, because `pw_filter` can't add ports safely while its
//! data thread runs; the router assigns devices to them and labels them
//! with the device's name (`port.alias`). The engine runs inside the
//! filter's process callback on PipeWire's real-time data thread.
//!
//! pipewire-rs has no safe `pw_filter` wrapper, so the filter is driven
//! through `pipewire-sys` directly. All unsafe code in Dawkey's audio path
//! lives in this file.

mod router;

use std::cell::UnsafeCell;
use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Once};

use dk_engine::{EngineProcessor, MidiEvent, MidiOut, device_tag, midi_message_len};
use libspa_sys::{SPA_DIRECTION_INPUT, SPA_DIRECTION_OUTPUT, spa_io_position};
use pipewire_sys as pw;
use serde::Serialize;

use crate::router::{Router, RouterMessage};

/// Largest cycle rendered into the fallback buffers used while the output
/// ports have no buffers (not linked). PipeWire's maximum quantum is 8192.
const SCRATCH_FRAMES: usize = 8192;
/// Most MIDI messages taken in one cycle; more are dropped.
const MIDI_EVENTS: usize = 512;
/// MIDI device ports, each way.
pub(crate) const MIDI_DEVICES: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum PipeWireError {
    #[error("PipeWire: could not create {0}")]
    Create(&'static str),
    #[error("PipeWire: connecting the filter failed (error {0})")]
    Connect(i32),
    #[error("PipeWire: starting the thread loop failed (error {0})")]
    Start(i32),
}

#[derive(Debug, Clone)]
pub struct Options {
    /// `node.name`; also the prefix of port names in tools like `pw-link`.
    pub node_name: String,
    /// Link the outputs to WirePlumber's default sink, and follow it when
    /// it changes.
    pub autoconnect: bool,
    /// Requested quantum, in frames.
    pub latency_frames: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            node_name: "dawkey".into(),
            autoconnect: true,
            latency_frames: 256,
        }
    }
}

/// What the PipeWire node is doing, as reported to clients.
#[derive(Debug, Clone, Serialize)]
pub struct BackendStatus {
    /// "unconnected", "connecting", "paused", "streaming" or "error".
    pub state: &'static str,
    pub error: Option<String>,
    pub node_id: Option<u32>,
    /// Node name of the sink the outputs are linked to, if any.
    pub linked_to: Option<String>,
    /// Node name of the source linked to the inputs, while capturing.
    pub capture_from: Option<String>,
    /// MIDI devices linked to Dawkey's MIDI inputs (while MIDI tracks are
    /// armed).
    pub midi_from: Vec<String>,
    /// MIDI devices Dawkey sends to (from tracks' MIDI outputs).
    pub midi_to: Vec<String>,
    /// MIDI devices (and other apps' MIDI ports, as `node:port`) that can
    /// play into a track (set_track_midi_input).
    pub midi_inputs: Vec<String>,
    /// MIDI devices (and other apps' MIDI ports) tracks can send to
    /// (set_track_midi_output).
    pub midi_outputs: Vec<String>,
    /// How long each of those takes to get MIDI, as PipeWire reports it,
    /// in milliseconds (one cycle for ALSA's MIDI bridge). Added to the
    /// latency set on tracks that send there.
    pub midi_output_latency_ms: BTreeMap<String, f64>,
    /// How late MIDI from each input device arrives, as PipeWire reports
    /// it, in milliseconds (one cycle for ALSA's MIDI bridge). Recorded
    /// MIDI is placed that much earlier.
    pub midi_input_latency_ms: BTreeMap<String, f64>,
    /// Delay from the graph to the output device, in frames, once known.
    pub output_delay: Option<u32>,
    /// Sample rate of the graph Dawkey is scheduled in, once known.
    pub graph_rate: Option<u32>,
    /// Frames per cycle, once known.
    pub quantum: Option<u32>,
    /// True if the graph runs at a different rate than the project. Dawkey
    /// outputs silence and holds the transport until the rates match.
    pub rate_mismatch: bool,
}

/// Status shared between PipeWire's threads and the controller.
#[derive(Debug, Default)]
struct Status {
    state: AtomicI32,
    error: Mutex<Option<String>>,
    router_error: Mutex<Option<String>>,
    linked_to: Mutex<Option<String>>,
    capture_from: Mutex<Option<String>>,
    midi_from: Mutex<Vec<String>>,
    midi_to: Mutex<Vec<String>>,
    midi_inputs: Mutex<Vec<String>>,
    midi_outputs: Mutex<Vec<String>>,
    /// What each MIDI output device's port reports (see
    /// [`PipeWireOutput::midi_output_latency_ms`]).
    midi_output_latency: Mutex<Vec<(String, router::PortLatency)>>,
    /// What each MIDI input device's port reports (see
    /// [`PipeWireOutput::midi_input_latency_ms`]).
    midi_input_latency: Mutex<Vec<(String, router::PortLatency)>>,
    node_id: AtomicU32,
    output_delay: AtomicU32,
    graph_rate: AtomicU32,
    quantum: AtomicU32,
    rate_mismatch: AtomicBool,
}

/// Owned by the real-time thread once the filter is connected.
struct RtState {
    processor: EngineProcessor,
    /// Output left, output right, input left, input right, MIDI input.
    ports: [*mut c_void; 5],
    scratch: [Vec<f32>; 2],
    /// This cycle's MIDI input.
    midi: [MidiEvent; MIDI_EVENTS],
}

/// A MIDI port for one device. The port is made at start; [`MidiPorts`]
/// sets which device it's for, and the data thread only reads it.
#[derive(Default)]
struct DevicePort {
    port: AtomicPtr<c_void>,
    /// The device's [`device_tag`].
    tag: AtomicU32,
}

/// The `data` pointer passed to every filter callback. `on_process` (data
/// thread) only touches `rt` and reads `inputs`/`outputs`;
/// `on_state_changed` (loop thread) only touches `status` and `filter`, so
/// nothing aliases mutably.
struct Callbacks {
    status: Arc<Status>,
    filter: AtomicPtr<pw::pw_filter>,
    router: Option<pipewire::channel::Sender<RouterMessage>>,
    rt: UnsafeCell<RtState>,
    inputs: [DevicePort; MIDI_DEVICES],
    outputs: [DevicePort; MIDI_DEVICES],
}

/// The pointers [`MidiPorts`] needs, while the filter exists.
struct Live {
    thread_loop: NonNull<pw::pw_thread_loop>,
    filter: NonNull<pw::pw_filter>,
    callbacks: NonNull<Callbacks>,
}

// The pointers are only used under `MidiPorts`' mutex, and only while the
// filter exists (`PipeWireOutput::drop` clears them first).
unsafe impl Send for Live {}

/// Assigns devices to the MIDI device ports, from any thread (the
/// router's). Does nothing once the output is being dropped.
#[derive(Clone)]
pub(crate) struct MidiPorts(Arc<Mutex<Option<Live>>>);

impl MidiPorts {
    /// Name of device port `slot` (from 0).
    pub(crate) fn port_name(slot: usize, output: bool) -> String {
        format!(
            "{}_{}",
            if output { "midi_out" } else { "midi_in" },
            slot + 1
        )
    }

    /// Makes port `slot` (an input to receive from a device, or an output
    /// to send to it) carry `device`'s MIDI, and names it after the device.
    pub(crate) fn assign(&self, slot: usize, output: bool, device: &str) {
        let Ok(guard) = self.0.lock() else { return };
        let Some(live) = guard.as_ref() else { return };
        let callbacks = unsafe { live.callbacks.as_ref() };
        let slots = if output {
            &callbacks.outputs
        } else {
            &callbacks.inputs
        };
        let Some(slot) = slots.get(slot) else { return };
        slot.tag.store(device_tag(device), Ordering::Release);
        let port = slot.port.load(Ordering::Acquire);
        let Ok(alias) = CString::new(device) else {
            return;
        };
        if port.is_null() {
            return;
        }
        let item = libspa_sys::spa_dict_item {
            key: c"port.alias".as_ptr(),
            value: alias.as_ptr(),
        };
        let dict = libspa_sys::spa_dict {
            flags: 0,
            n_items: 1,
            items: &item,
        };
        // SAFETY: the filter exists while `live` is set, `port` is one of
        // its ports, and the thread loop lock makes the call safe from this
        // thread. It only changes the port's properties, which the data
        // thread doesn't read.
        unsafe {
            pw::pw_thread_loop_lock(live.thread_loop.as_ptr());
            pw::pw_filter_update_properties(live.filter.as_ptr(), port, &dict);
            pw::pw_thread_loop_unlock(live.thread_loop.as_ptr());
        }
    }
}

/// Which MIDI devices to connect.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MidiRoutes {
    /// Devices whose input plays into Dawkey.
    pub capture: MidiCaptureRoute,
    /// Devices Dawkey sends to.
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MidiCaptureRoute {
    #[default]
    Off,
    /// Every MIDI device.
    All,
    /// These devices, by name.
    Devices(Vec<String>),
}

static EVENTS: pw::pw_filter_events = pw::pw_filter_events {
    version: pw::PW_VERSION_FILTER_EVENTS,
    destroy: None,
    state_changed: Some(on_state_changed),
    io_changed: None,
    param_changed: None,
    add_buffer: None,
    remove_buffer: None,
    process: Some(on_process),
    drained: None,
    command: None,
};

/// A running PipeWire node. Dropping it disconnects and stops the engine.
pub struct PipeWireOutput {
    thread_loop: NonNull<pw::pw_thread_loop>,
    filter: NonNull<pw::pw_filter>,
    callbacks: NonNull<Callbacks>,
    status: Arc<Status>,
    sample_rate: u32,
    /// Dropped after the filter (field drop order), so it removes our links
    /// last.
    _router: Option<Router>,
    ports: MidiPorts,
}

// The raw pointers are only used under the thread-loop lock or after the
// loop has stopped (in `drop`).
unsafe impl Send for PipeWireOutput {}

impl std::fmt::Debug for PipeWireOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeWireOutput")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl PipeWireOutput {
    /// Creates the node and starts PipeWire's threads. Connection happens
    /// asynchronously; watch [`status`](Self::status).
    pub fn start(processor: EngineProcessor, options: &Options) -> Result<Self, PipeWireError> {
        static INIT: Once = Once::new();
        INIT.call_once(|| unsafe { pw::pw_init(ptr::null_mut(), ptr::null_mut()) });

        let sample_rate = processor.sample_rate();
        let status = Arc::new(Status::default());
        let router = options
            .autoconnect
            .then(|| Router::start(Arc::clone(&status)));
        let callbacks = Box::into_raw(Box::new(Callbacks {
            status: Arc::clone(&status),
            filter: AtomicPtr::new(ptr::null_mut()),
            router: router.as_ref().map(Router::sender),
            rt: UnsafeCell::new(RtState {
                processor,
                ports: [ptr::null_mut(); 5],
                scratch: [vec![0.0; SCRATCH_FRAMES], vec![0.0; SCRATCH_FRAMES]],
                midi: [MidiEvent::default(); MIDI_EVENTS],
            }),
            inputs: Default::default(),
            outputs: Default::default(),
        }));
        let callbacks = NonNull::new(callbacks).expect("Box is non-null");
        // Until the output exists, clean up `callbacks` ourselves on error.
        let free_callbacks = || drop(unsafe { Box::from_raw(callbacks.as_ptr()) });

        let thread_loop = match NonNull::new(unsafe {
            pw::pw_thread_loop_new(c"dawkey-pipewire".as_ptr(), ptr::null())
        }) {
            Some(tl) => tl,
            None => {
                free_callbacks();
                return Err(PipeWireError::Create("thread loop"));
            }
        };

        let props = properties(&[
            ("media.type", "Audio"),
            ("media.category", "Playback"),
            ("media.role", "Production"),
            ("media.class", "Stream/Output/Audio"),
            ("node.name", &options.node_name),
            ("node.description", "Dawkey"),
            ("application.name", "Dawkey"),
            // WirePlumber can't link a filter node; the router does.
            ("node.autoconnect", "false"),
            (
                "node.latency",
                &format!("{}/{sample_rate}", options.latency_frames),
            ),
            ("node.rate", &format!("1/{sample_rate}")),
        ]);
        let name = CString::new(options.node_name.as_str()).unwrap_or_else(|_| c"dawkey".into());
        // Takes ownership of `props`.
        let filter = unsafe {
            pw::pw_filter_new_simple(
                pw::pw_thread_loop_get_loop(thread_loop.as_ptr()),
                name.as_ptr(),
                props,
                &EVENTS,
                callbacks.as_ptr().cast(),
            )
        };
        let Some(filter) = NonNull::new(filter) else {
            unsafe { pw::pw_thread_loop_destroy(thread_loop.as_ptr()) };
            free_callbacks();
            return Err(PipeWireError::Create("filter"));
        };
        // From here on, `output`'s Drop cleans up everything.
        let ports = MidiPorts(Arc::new(Mutex::new(Some(Live {
            thread_loop,
            filter,
            callbacks,
        }))));
        if let Some(router) = &router {
            let _ = router.sender().send(RouterMessage::Ports(ports.clone()));
        }
        let output = Self {
            thread_loop,
            filter,
            callbacks,
            status,
            sample_rate,
            _router: router,
            ports,
        };
        unsafe {
            (*callbacks.as_ptr())
                .filter
                .store(filter.as_ptr(), Ordering::Release)
        };

        let audio = |direction, name: &'static str, channel: &'static str| {
            (
                direction,
                vec![
                    ("format.dsp", "32 bit float mono audio"),
                    ("port.name", name),
                    ("audio.channel", channel),
                ],
            )
        };
        let ports = [
            audio(SPA_DIRECTION_OUTPUT, "output_FL", "FL"),
            audio(SPA_DIRECTION_OUTPUT, "output_FR", "FR"),
            audio(SPA_DIRECTION_INPUT, "input_FL", "FL"),
            audio(SPA_DIRECTION_INPUT, "input_FR", "FR"),
            (
                SPA_DIRECTION_INPUT,
                vec![("format.dsp", "8 bit raw midi"), ("port.name", "midi_in")],
            ),
        ];
        for (index, (direction, props)) in ports.into_iter().enumerate() {
            let port_props = properties(&props);
            // Takes ownership of `port_props`.
            let port = unsafe {
                pw::pw_filter_add_port(
                    filter.as_ptr(),
                    direction,
                    pw::pw_filter_port_flags_PW_FILTER_PORT_FLAG_MAP_BUFFERS,
                    0,
                    port_props,
                    ptr::null_mut(),
                    0,
                )
            };
            if port.is_null() {
                return Err(PipeWireError::Create("port"));
            }
            // Not connected yet, so the data thread isn't running.
            unsafe { (*(*callbacks.as_ptr()).rt.get()).ports[index] = port };
        }
        // The device ports, unassigned (tag 0) until the router needs them.
        for output in [false, true] {
            for slot in 0..MIDI_DEVICES {
                let name = MidiPorts::port_name(slot, output);
                let props = properties(&[("format.dsp", "8 bit raw midi"), ("port.name", &name)]);
                let direction = if output {
                    SPA_DIRECTION_OUTPUT
                } else {
                    SPA_DIRECTION_INPUT
                };
                // Takes ownership of `props`.
                let port = unsafe {
                    pw::pw_filter_add_port(
                        filter.as_ptr(),
                        direction,
                        pw::pw_filter_port_flags_PW_FILTER_PORT_FLAG_MAP_BUFFERS,
                        0,
                        props,
                        ptr::null_mut(),
                        0,
                    )
                };
                if port.is_null() {
                    return Err(PipeWireError::Create("port"));
                }
                let cb = unsafe { callbacks.as_ref() };
                let slots = if output { &cb.outputs } else { &cb.inputs };
                slots[slot].port.store(port, Ordering::Release);
            }
        }

        let res = unsafe {
            pw::pw_filter_connect(
                filter.as_ptr(),
                pw::pw_filter_flags_PW_FILTER_FLAG_RT_PROCESS,
                ptr::null_mut(),
                0,
            )
        };
        if res < 0 {
            return Err(PipeWireError::Connect(res));
        }
        let res = unsafe { pw::pw_thread_loop_start(thread_loop.as_ptr()) };
        if res < 0 {
            return Err(PipeWireError::Start(res));
        }
        Ok(output)
    }

    pub fn status(&self) -> BackendStatus {
        let s = &self.status;
        let nonzero = |v: u32| (v != 0).then_some(v);
        let read = |m: &Mutex<Option<String>>| m.lock().map(|v| v.clone()).unwrap_or(None);
        BackendStatus {
            state: state_name(s.state.load(Ordering::Acquire)),
            error: read(&s.error).or_else(|| read(&s.router_error)),
            node_id: nonzero(s.node_id.load(Ordering::Acquire)).filter(|&id| id != u32::MAX),
            linked_to: read(&s.linked_to),
            capture_from: read(&s.capture_from),
            midi_from: s.midi_from.lock().map(|v| v.clone()).unwrap_or_default(),
            midi_to: s.midi_to.lock().map(|v| v.clone()).unwrap_or_default(),
            midi_inputs: s.midi_inputs.lock().map(|v| v.clone()).unwrap_or_default(),
            midi_outputs: s.midi_outputs.lock().map(|v| v.clone()).unwrap_or_default(),
            midi_output_latency_ms: self.midi_output_latency_ms(),
            midi_input_latency_ms: self.midi_input_latency_ms(),
            output_delay: nonzero(s.output_delay.load(Ordering::Relaxed)),
            graph_rate: nonzero(s.graph_rate.load(Ordering::Relaxed)),
            quantum: nonzero(s.quantum.load(Ordering::Relaxed)),
            rate_mismatch: s.rate_mismatch.load(Ordering::Relaxed),
        }
    }

    /// How long each MIDI output device takes to get MIDI, as PipeWire
    /// reports it, in milliseconds at the current cycle size and rate.
    pub fn midi_output_latency_ms(&self) -> BTreeMap<String, f64> {
        self.latency_ms(&self.status.midi_output_latency)
    }

    /// How late MIDI from each input device arrives, as PipeWire reports
    /// it, in milliseconds at the current cycle size and rate.
    pub fn midi_input_latency_ms(&self) -> BTreeMap<String, f64> {
        self.latency_ms(&self.status.midi_input_latency)
    }

    fn latency_ms(
        &self,
        reported: &Mutex<Vec<(String, router::PortLatency)>>,
    ) -> BTreeMap<String, f64> {
        let s = &self.status;
        let (quantum, rate) = (
            s.quantum.load(Ordering::Relaxed),
            s.graph_rate.load(Ordering::Relaxed),
        );
        reported
            .lock()
            .map(|v| {
                v.iter()
                    .map(|(name, latency)| (name.clone(), latency.ms(quantum, rate)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The project sample rate the engine renders at.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Connects (or disconnects) the default audio source to the inputs.
    /// Without autoconnect, links are left to the user.
    pub fn set_capture(&self, on: bool) {
        self.send_router(RouterMessage::Capture(on));
    }

    /// Connects these MIDI devices (and disconnects others). Without
    /// autoconnect, links are left to the user.
    pub fn set_midi_routes(&self, routes: MidiRoutes) {
        self.send_router(RouterMessage::Midi(routes));
    }

    fn send_router(&self, message: RouterMessage) {
        let router = unsafe { &(*self.callbacks.as_ptr()).router };
        if let Some(router) = router {
            let _ = router.send(message);
        }
    }

    /// Estimated round trip from input to output, in frames: a cycle each
    /// way plus the output device's delay. An estimate; the user can
    /// correct it.
    pub fn round_trip_estimate(&self) -> u32 {
        self.quantum() + self.playback_latency()
    }

    /// How long what the engine renders takes to be heard, in frames: a
    /// cycle plus the output device's delay.
    pub fn playback_latency(&self) -> u32 {
        self.quantum() + self.status.output_delay.load(Ordering::Relaxed)
    }

    /// Frames per cycle (0 until known).
    pub fn quantum(&self) -> u32 {
        self.status.quantum.load(Ordering::Relaxed)
    }
}

impl Drop for PipeWireOutput {
    fn drop(&mut self) {
        // The router can't add ports from here on.
        if let Ok(mut live) = self.ports.0.lock() {
            *live = None;
        }
        unsafe {
            pw::pw_thread_loop_lock(self.thread_loop.as_ptr());
            pw::pw_filter_destroy(self.filter.as_ptr());
            pw::pw_thread_loop_unlock(self.thread_loop.as_ptr());
            pw::pw_thread_loop_stop(self.thread_loop.as_ptr());
            pw::pw_thread_loop_destroy(self.thread_loop.as_ptr());
            // No callbacks can run any more.
            drop(Box::from_raw(self.callbacks.as_ptr()));
        }
    }
}

/// Builds a `pw_properties`; the caller passes ownership to PipeWire.
fn properties(pairs: &[(&str, &str)]) -> *mut pw::pw_properties {
    let props = unsafe { pw::pw_properties_new(ptr::null()) };
    for (key, value) in pairs {
        let (Ok(key), Ok(value)) = (CString::new(*key), CString::new(*value)) else {
            continue;
        };
        unsafe { pw::pw_properties_set(props, key.as_ptr(), value.as_ptr()) };
    }
    props
}

fn state_name(state: i32) -> &'static str {
    match state {
        pw::pw_filter_state_PW_FILTER_STATE_ERROR => "error",
        pw::pw_filter_state_PW_FILTER_STATE_CONNECTING => "connecting",
        pw::pw_filter_state_PW_FILTER_STATE_PAUSED => "paused",
        pw::pw_filter_state_PW_FILTER_STATE_STREAMING => "streaming",
        _ => "unconnected",
    }
}

/// Runs on the thread-loop thread.
unsafe extern "C" fn on_state_changed(
    data: *mut c_void,
    _old: pw::pw_filter_state,
    state: pw::pw_filter_state,
    error: *const c_char,
) {
    let callbacks = unsafe { &*(data as *const Callbacks) };
    let status = &callbacks.status;
    status.state.store(state, Ordering::Release);
    let filter = callbacks.filter.load(Ordering::Acquire);
    if !filter.is_null() {
        let id = unsafe { pw::pw_filter_get_node_id(filter) };
        status.node_id.store(id, Ordering::Release);
        if id != u32::MAX
            && let Some(router) = &callbacks.router
        {
            let _ = router.send(RouterMessage::OwnNode(id));
        }
    }
    let message = (!error.is_null()).then(|| {
        unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    });
    if let Some(message) = &message {
        tracing::warn!("PipeWire filter error: {message}");
    }
    tracing::debug!("PipeWire filter state: {}", state_name(state));
    if let Ok(mut slot) = status.error.lock() {
        *slot = message;
    }
}

/// Runs on PipeWire's real-time data thread. Must not allocate or block.
unsafe extern "C" fn on_process(data: *mut c_void, position: *mut spa_io_position) {
    if position.is_null() {
        return;
    }
    let callbacks = unsafe { &*(data as *const Callbacks) };
    let rt = unsafe { &mut *callbacks.rt.get() };
    let clock = unsafe { &(*position).clock };
    let frames = clock.duration as usize;
    let graph_rate = clock.rate.denom;

    let status = &callbacks.status;
    status.quantum.store(frames as u32, Ordering::Relaxed);
    status.graph_rate.store(graph_rate, Ordering::Relaxed);
    status.output_delay.store(
        clock.delay.clamp(0, i64::from(u32::MAX)) as u32,
        Ordering::Relaxed,
    );
    let mismatch = graph_rate != 0 && graph_rate != rt.processor.sample_rate();
    status.rate_mismatch.store(mismatch, Ordering::Relaxed);

    let buffer = |port: *mut c_void| unsafe {
        pw::pw_filter_get_dsp_buffer(port, frames as u32) as *mut f32
    };
    // The hand-patched input, then each device's.
    let mut midi_count = read_midi(rt.ports[4], frames as u32, 0, &mut rt.midi, 0);
    for slot in &callbacks.inputs {
        let port = slot.port.load(Ordering::Acquire);
        if !port.is_null() {
            let tag = slot.tag.load(Ordering::Relaxed);
            midi_count = read_midi(port, frames as u32, tag, &mut rt.midi, midi_count);
        }
    }
    sort_by_offset(&mut rt.midi[..midi_count]);
    let [out_l, out_r, in_l, in_r] = [0, 1, 2, 3].map(|i| buffer(rt.ports[i]));
    // Unlinked inputs have no buffers: that's silence.
    let input = |p: *mut f32| -> &[f32] {
        if p.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(p, frames) }
        }
    };
    let input = [input(in_l), input(in_r)];
    let RtState {
        processor,
        scratch,
        midi,
        ..
    } = rt;
    let midi = &midi[..midi_count];
    if out_l.is_null() || out_r.is_null() {
        // No output buffers (not linked): keep the transport running into
        // scratch space so time still advances.
        let [sl, sr] = scratch;
        let frames = frames.min(SCRATCH_FRAMES);
        let input = input.map(|i| {
            if i.len() >= frames {
                &i[..frames]
            } else {
                &[][..]
            }
        });
        run(
            processor,
            mismatch,
            input,
            midi,
            &mut sl[..frames],
            &mut sr[..frames],
        );
    } else {
        let (left, right) = unsafe {
            (
                std::slice::from_raw_parts_mut(out_l, frames),
                std::slice::from_raw_parts_mut(out_r, frames),
            )
        };
        run(processor, mismatch, input, midi, left, right);
    }
    for slot in &callbacks.outputs {
        let port = slot.port.load(Ordering::Acquire);
        if !port.is_null() {
            let tag = slot.tag.load(Ordering::Relaxed);
            write_midi(port, frames as u32, tag, processor.midi_output());
        }
    }
    processor.clear_midi_output();
}

fn run(
    processor: &mut EngineProcessor,
    mismatch: bool,
    input: [&[f32]; 2],
    midi: &[MidiEvent],
    left: &mut [f32],
    right: &mut [f32],
) {
    if mismatch {
        processor.process_silence(left, right);
    } else {
        processor.process_with_input(input, midi, left, right);
    }
}

/// Appends this cycle's MIDI from an input port to `out[count..]`, tagged
/// with the device it comes from. Returns the new count. Real-time safe.
fn read_midi(
    port: *mut c_void,
    frames: u32,
    tag: u32,
    out: &mut [MidiEvent; MIDI_EVENTS],
    mut count: usize,
) -> usize {
    if port.is_null() {
        return count;
    }
    let buffer = unsafe { pw::pw_filter_dequeue_buffer(port) };
    if buffer.is_null() {
        return count;
    }
    // SAFETY: a dequeued buffer stays valid until it is queued again, and
    // MAP_BUFFERS maps its data into our address space.
    unsafe {
        let spa = (*buffer).buffer;
        if !spa.is_null() && (*spa).n_datas > 0 && !(*spa).datas.is_null() {
            let data = &*(*spa).datas;
            if !data.data.is_null() && !data.chunk.is_null() && data.maxsize > 0 {
                let chunk = &*data.chunk;
                let offset = chunk.offset % data.maxsize;
                let size = chunk.size.min(data.maxsize - offset);
                let bytes = std::slice::from_raw_parts(
                    data.data.cast::<u8>().add(offset as usize),
                    size as usize,
                );
                parse_sequence(bytes, frames, |event| {
                    if count < MIDI_EVENTS {
                        out[count] = event.from_device(tag);
                        count += 1;
                    }
                });
            }
        }
        pw::pw_filter_queue_buffer(port, buffer);
    }
    count
}

/// Sorts by offset with a stable insertion sort, which keeps same-offset
/// messages in order and doesn't allocate.
fn sort_by_offset(events: &mut [MidiEvent]) {
    for i in 1..events.len() {
        let mut j = i;
        while j > 0 && events[j - 1].offset > events[j].offset {
            events.swap(j - 1, j);
            j -= 1;
        }
    }
}

/// Writes the messages for device `tag` (sorted by offset) to an output
/// port as this cycle's sequence. Real-time safe.
fn write_midi(port: *mut c_void, frames: u32, tag: u32, sent: &[MidiOut]) {
    let buffer = unsafe { pw::pw_filter_dequeue_buffer(port) };
    if buffer.is_null() {
        return;
    }
    // SAFETY: as in `read_midi`; the buffer is ours until queued again.
    unsafe {
        let spa = (*buffer).buffer;
        if !spa.is_null() && (*spa).n_datas > 0 && !(*spa).datas.is_null() {
            let data = &mut *(*spa).datas;
            if !data.data.is_null() && !data.chunk.is_null() {
                let bytes =
                    std::slice::from_raw_parts_mut(data.data.cast::<u8>(), data.maxsize as usize);
                let messages = sent
                    .iter()
                    .filter(|m| m.dest == tag)
                    .map(|m| (m.offset.min(frames.saturating_sub(1)), Control::of(m)));
                let size = write_sequence(bytes, messages);
                let chunk = &mut *data.chunk;
                chunk.offset = 0;
                chunk.size = size as u32;
                chunk.stride = 1;
                chunk.flags = 0;
            }
        }
        pw::pw_filter_queue_buffer(port, buffer);
    }
}

/// One control in a sequence: MIDI 1.0 bytes, or a Universal MIDI Packet
/// (MIDI 2.0 per-note messages, which MIDI 1.0 can't carry).
#[derive(Debug, Clone, Copy)]
struct Control {
    kind: u32,
    bytes: [u8; 8],
    len: usize,
}

impl Control {
    fn of(message: &MidiOut) -> Self {
        let mut bytes = [0; 8];
        match message.ump() {
            Some([a, b]) => {
                bytes[..4].copy_from_slice(&a.to_ne_bytes());
                bytes[4..].copy_from_slice(&b.to_ne_bytes());
                Self {
                    kind: libspa_sys::SPA_CONTROL_UMP,
                    bytes,
                    len: 8,
                }
            }
            None => {
                let len = midi_message_len(message.data[0]);
                bytes[..3].copy_from_slice(&message.data);
                Self {
                    kind: libspa_sys::SPA_CONTROL_Midi,
                    bytes,
                    len,
                }
            }
        }
    }
}

/// Writes a SPA POD sequence of controls into `out`, as many as fit.
/// Returns the bytes written (0 if not even the header fits).
fn write_sequence(out: &mut [u8], messages: impl Iterator<Item = (u32, Control)>) -> usize {
    let put = |out: &mut [u8], at: usize, value: u32| {
        out[at..at + 4].copy_from_slice(&value.to_ne_bytes());
    };
    if out.len() < 16 {
        return 0;
    }
    // Pod header (size, type), then the sequence body: unit, pad.
    put(out, 4, libspa_sys::SPA_TYPE_Sequence);
    put(out, 8, 0);
    put(out, 12, 0);
    let mut at = 16;
    for (offset, control) in messages {
        let bytes = &control.bytes[..control.len];
        let padded = bytes.len().div_ceil(8) * 8;
        if at + 16 + padded > out.len() {
            break;
        }
        put(out, at, offset);
        put(out, at + 4, control.kind);
        put(out, at + 8, bytes.len() as u32);
        put(out, at + 12, libspa_sys::SPA_TYPE_Bytes);
        out[at + 16..at + 16 + bytes.len()].copy_from_slice(bytes);
        out[at + 16 + bytes.len()..at + 16 + padded].fill(0);
        at += 16 + padded;
    }
    put(out, 0, (at - 8) as u32);
    at
}

/// Reads a SPA POD sequence of control events (MIDI bytes or UMP words)
/// into channel events. Offsets are clamped into the cycle. Safe on any
/// input: malformed data is skipped.
fn parse_sequence(bytes: &[u8], frames: u32, mut push: impl FnMut(MidiEvent)) {
    let word = |at: usize| -> Option<u32> {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
    };
    let (Some(size), Some(kind)) = (word(0), word(4)) else {
        return;
    };
    if kind != libspa_sys::SPA_TYPE_Sequence {
        return;
    }
    // The pod header (8 bytes) and the sequence body (unit, pad) precede
    // the controls.
    let end = (8 + size as usize).min(bytes.len());
    let mut at = 16;
    while at + 16 <= end {
        let (Some(offset), Some(control), Some(value_size)) =
            (word(at), word(at + 4), word(at + 8))
        else {
            return;
        };
        let body = at + 16;
        let Some(value) = bytes.get(body..body + value_size as usize) else {
            return;
        };
        let offset = offset.min(frames.saturating_sub(1));
        match control {
            libspa_sys::SPA_CONTROL_Midi => dk_engine::parse_midi1(value, offset, &mut push),
            libspa_sys::SPA_CONTROL_UMP => {
                let mut words = [0u32; 64];
                let n = (value.len() / 4).min(words.len());
                for (i, w) in words[..n].iter_mut().enumerate() {
                    let b = &value[i * 4..i * 4 + 4];
                    *w = u32::from_ne_bytes([b[0], b[1], b[2], b[3]]);
                }
                dk_engine::parse_ump(&words[..n], offset, &mut push);
            }
            _ => {}
        }
        // Controls are padded to 8 bytes.
        at = body + (value_size as usize).div_ceil(8) * 8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a SPA sequence pod: (offset, control type, value bytes).
    fn sequence(controls: &[(u32, u32, &[u8])]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend(0u32.to_ne_bytes()); // unit
        body.extend(0u32.to_ne_bytes()); // pad
        for (offset, kind, value) in controls {
            body.extend(offset.to_ne_bytes());
            body.extend(kind.to_ne_bytes());
            body.extend((value.len() as u32).to_ne_bytes());
            body.extend(libspa_sys::SPA_TYPE_Bytes.to_ne_bytes());
            body.extend(*value);
            body.resize(body.len().div_ceil(8) * 8, 0);
        }
        let mut pod = Vec::new();
        pod.extend((body.len() as u32).to_ne_bytes());
        pod.extend(libspa_sys::SPA_TYPE_Sequence.to_ne_bytes());
        pod.extend(body);
        pod
    }

    fn parse(bytes: &[u8], frames: u32) -> Vec<MidiEvent> {
        let mut out = Vec::new();
        parse_sequence(bytes, frames, |e| out.push(e));
        out
    }

    #[test]
    fn parses_midi_and_ump_controls() {
        let ump = 0x2090_4064u32.to_ne_bytes();
        let pod = sequence(&[
            (3, libspa_sys::SPA_CONTROL_Midi, &[0x90, 60, 100]),
            (7, libspa_sys::SPA_CONTROL_UMP, &ump),
            (900, libspa_sys::SPA_CONTROL_Midi, &[0x80, 60, 0]),
        ]);
        assert_eq!(
            parse(&pod, 256),
            [
                MidiEvent::note_on(3, 0, 60, 100),
                MidiEvent::note_on(7, 0, 64, 100),
                // Clamped into the cycle.
                MidiEvent::note_off(255, 0, 60),
            ]
        );
    }

    #[test]
    fn writes_sequences_it_can_read() {
        let mut out = [0u8; 256];
        let out_message = |data: [u8; 3], value: u32| MidiOut {
            offset: 0,
            dest: 0,
            data,
            value,
        };
        // A per-note pitch bend goes out as a Universal MIDI Packet.
        let bend = out_message([0x61, 64, 0], 0x9000_0000);
        let messages = [
            (5, Control::of(&out_message([0x91, 60, 100], 0))),
            (9, Control::of(&out_message([0xc1, 7, 0], 0))),
            (11, Control::of(&bend)),
        ];
        let size = write_sequence(&mut out, messages.into_iter());
        assert_eq!(size, 16 + 3 * 24);
        let parsed = parse(&out[..size], 256);
        assert_eq!(
            parsed,
            [
                MidiEvent::note_on(5, 1, 60, 100),
                MidiEvent::midi1(9, [0xc1, 7, 0]),
                MidiEvent::per_note(11, 1, 64, dk_engine::PerNote::PitchBend, 0x9000_0000),
            ]
        );
        assert_eq!(bend.ump(), Some([0x4061_4000, 0x9000_0000]));
        // What doesn't fit is left out.
        assert_eq!(write_sequence(&mut out[..50], messages.into_iter()), 40);
        assert_eq!(write_sequence(&mut out[..8], messages.into_iter()), 0);
    }

    #[test]
    fn ignores_malformed_sequences() {
        let pod = sequence(&[(0, libspa_sys::SPA_CONTROL_Midi, &[0x90, 60, 100])]);
        for cut in 0..pod.len() {
            parse(&pod[..cut], 256);
        }
        let mut wrong = pod.clone();
        wrong[4] = 0;
        assert!(parse(&wrong, 256).is_empty());
        assert!(parse(&[], 256).is_empty());
    }
}
