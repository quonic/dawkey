// SPDX-License-Identifier: AGPL-3.0-or-later
//! The VST3 test plugins: the CLAP set again, with VST3's ways of doing
//! things (normalized parameters, a controller that edits through the
//! host's component handler, `IMidiMapping`, note IDs, buses), written
//! with the `vst3` crate.
//!
//! Note IDs are the host's: Dawkey gives a key's notes `channel << 7 |
//! key`, and the octave sends its notes (and their expressions) on with
//! the ID of the key an octave up.

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use vst3::Steinberg::Linux::*;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface, uid};

use crate::{DELAY_FRAMES, STALL_DB};

/// The test plugins, by the name tests know them by, and their class IDs.
const CLASSES: [(&str, &str, Kind); 8] = [
    ("gain", "Test Gain (VST3)", Kind::Gain),
    ("delay", "Test Delay (VST3)", Kind::Delay),
    ("beep", "Test Beep (VST3)", Kind::Beep),
    ("octave", "Test Octave (VST3)", Kind::Octave),
    ("memo", "Test Memo (VST3)", Kind::Memo),
    ("tempo", "Test Tempo (VST3)", Kind::Tempo),
    ("subtract", "Test Subtract (VST3)", Kind::Subtract),
    ("split", "Test Split (VST3)", Kind::Split),
];

/// The memo's controller, a class of its own.
const MEMO_CONTROLLER: TUID = cid(100);

const fn cid(n: u32) -> TUID {
    uid(0x4441_574B, 0x4559_5654, 0x4553_5400, n)
}

/// The Dawkey plugin ID (`vst3:<hex>`) of the test plugin called `name`
/// (`"gain"`, `"beep"`, ...).
pub fn id(name: &str) -> Option<String> {
    let n = CLASSES.iter().position(|(n, _, _)| *n == name)?;
    let hex: String = cid(n as u32)
        .iter()
        .map(|&b| format!("{:02X}", b as u8))
        .collect();
    Some(format!("vst3:{hex}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Gain,
    Delay,
    Beep,
    Octave,
    Memo,
    Tempo,
    Subtract,
    Split,
}

/// An `f64` shared between threads.
struct Value(AtomicU64);

impl Value {
    fn new(v: f64) -> Self {
        Self(AtomicU64::new(v.to_bits()))
    }

    fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }

    fn set(&self, v: f64) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
}

/// A parameter: ID, name, default (normalized), flags.
struct Param {
    id: ParamID,
    name: &'static str,
    default: f64,
    flags: i32,
}

use ParameterInfo_::ParameterFlags_::{kCanAutomate, kIsHidden, kIsReadOnly};

/// The gain's: -60 to +12 dB.
const GAIN: ParamID = 1;
/// The beep's level; its hidden "CC 7" (MIDI volume, mapped) and read-only
/// "Trigger" (key 127 sets it, and the controller turns Level to half).
const LEVEL: ParamID = 1;
const CC7: ParamID = 2;
const TRIGGER: ParamID = 3;
/// The octave's hidden pitch bend, one per channel.
const BEND: ParamID = 100;

fn params(kind: Kind) -> Vec<Param> {
    match kind {
        Kind::Gain => vec![Param {
            id: GAIN,
            name: "Gain",
            default: 60.0 / 72.0,
            flags: kCanAutomate,
        }],
        Kind::Beep => vec![
            Param {
                id: LEVEL,
                name: "Level",
                default: 1.0,
                flags: kCanAutomate,
            },
            Param {
                id: CC7,
                name: "CC 7",
                default: 1.0,
                flags: kCanAutomate | kIsHidden,
            },
            Param {
                id: TRIGGER,
                name: "Trigger",
                default: 0.0,
                flags: kIsReadOnly | kIsHidden,
            },
        ],
        Kind::Octave => (0..16)
            .map(|channel| Param {
                id: BEND + channel,
                name: "Bend",
                default: 0.5,
                flags: kIsHidden,
            })
            .collect(),
        Kind::Memo => vec![Param {
            id: 1,
            name: "Memo",
            default: 0.0,
            flags: kIsReadOnly,
        }],
        _ => Vec::new(),
    }
}

/// A gain in dB from its normalized value.
fn gain_db(normalized: f64) -> f64 {
    -60.0 + 72.0 * normalized.clamp(0.0, 1.0)
}

/// Audio buses one way: names and kinds (all stereo).
fn audio_buses(kind: Kind, input: bool) -> &'static [(&'static str, BusTypes)] {
    const MAIN: (&str, BusTypes) = ("main", BusTypes_::kMain);
    match (kind, input) {
        (Kind::Octave, _) => &[],
        (Kind::Beep | Kind::Split, true) => &[],
        (Kind::Subtract, true) => &[MAIN, ("Sidechain", BusTypes_::kAux)],
        (Kind::Split, false) => &[MAIN, ("High", BusTypes_::kAux)],
        _ => &[MAIN],
    }
}

/// Event buses one way.
fn event_buses(kind: Kind, input: bool) -> i32 {
    match (kind, input) {
        (Kind::Beep | Kind::Memo | Kind::Split | Kind::Octave, true) => 1,
        (Kind::Octave, false) => 1,
        _ => 0,
    }
}

fn write_wstring(text: &str, s: &mut [TChar]) {
    let mut n = 0;
    for (unit, slot) in text.encode_utf16().zip(s.iter_mut()) {
        *slot = unit;
        n += 1;
    }
    let end = n.min(s.len() - 1);
    s[end] = 0;
}

fn write_cstring(text: &str, s: &mut [c_char]) {
    let bytes = text.as_bytes();
    let n = bytes.len().min(s.len() - 1);
    for (slot, &b) in s.iter_mut().zip(&bytes[..n]) {
        *slot = b as c_char;
    }
    s[n] = 0;
}

/// A UTF-16 string the host passed.
unsafe fn read_wstring(s: *const TChar) -> String {
    let mut units = Vec::new();
    let mut at = s;
    // SAFETY: the host passes a NUL-terminated string.
    unsafe {
        while *at != 0 {
            units.push(*at);
            at = at.add(1);
        }
    }
    String::from_utf16_lossy(&units)
}

/// What a block of a plugin's audio thread keeps between blocks.
#[derive(Default)]
struct Audio {
    /// The gain's (and beep's level's) current normalized value.
    value: f64,
    cc7: f64,
    /// The delay's rings, one per channel.
    rings: [Vec<f32>; 2],
    at: usize,
    /// Velocity of each held key, 0 if up; each key's volume expression.
    held: Vec<f64>,
    volume: Vec<f64>,
    /// Keys of the notes it was given, by note ID.
    notes: Vec<(i32, usize)>,
}

/// The single-component test plugins: component and controller in one
/// object.
struct TestPlugin {
    kind: Kind,
    /// The controller's parameter values (normalized), by index.
    values: std::sync::Arc<Vec<Value>>,
    handler: RefCell<Option<ComPtr<IComponentHandler>>>,
    audio: RefCell<Audio>,
}

impl TestPlugin {
    fn new(kind: Kind) -> Self {
        let values =
            std::sync::Arc::new(params(kind).iter().map(|p| Value::new(p.default)).collect());
        Self {
            kind,
            values,
            handler: RefCell::new(None),
            audio: RefCell::new(Audio::default()),
        }
    }

    fn param_index(&self, id: ParamID) -> Option<usize> {
        params(self.kind).iter().position(|p| p.id == id)
    }

    /// Edits a parameter as a knob would: a gesture through the host.
    fn edit(&self, id: ParamID, value: f64) {
        if let Some(i) = self.param_index(id) {
            self.values[i].set(value);
        }
        if let Some(handler) = &*self.handler.borrow() {
            // SAFETY: the host's live handler.
            unsafe {
                handler.beginEdit(id);
                handler.performEdit(id, value);
                handler.endEdit(id);
            }
        }
    }
}

impl Class for TestPlugin {
    type Interfaces = (
        IComponent,
        IAudioProcessor,
        IEditController,
        IMidiMapping,
        IProcessContextRequirements,
    );
}

impl IPluginBaseTrait for TestPlugin {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        *self.handler.borrow_mut() = None;
        kResultOk
    }
}

/// `getBusCount` for a plugin of `kind`.
fn bus_count(kind: Kind, media: MediaType, dir: BusDirection) -> i32 {
    let input = dir == BusDirections_::kInput as i32;
    match media as u32 {
        MediaTypes_::kAudio => audio_buses(kind, input).len() as i32,
        MediaTypes_::kEvent => event_buses(kind, input),
        _ => 0,
    }
}

/// `getBusInfo` for a plugin of `kind`.
unsafe fn bus_info(
    kind: Kind,
    media: MediaType,
    dir: BusDirection,
    index: i32,
    bus: *mut BusInfo,
) -> tresult {
    let input = dir == BusDirections_::kInput as i32;
    if bus.is_null() || index < 0 {
        return kInvalidArgument;
    }
    // SAFETY: the host passes room for a BusInfo.
    let bus = unsafe { &mut *bus };
    bus.mediaType = media;
    bus.direction = dir;
    bus.flags = BusInfo_::BusFlags_::kDefaultActive;
    match media as u32 {
        MediaTypes_::kAudio => {
            let Some(&(name, kind)) = audio_buses(kind, input).get(index as usize) else {
                return kInvalidArgument;
            };
            bus.channelCount = 2;
            bus.busType = kind as i32;
            write_wstring(name, &mut bus.name);
        }
        MediaTypes_::kEvent if index < event_buses(kind, input) => {
            bus.channelCount = 16;
            bus.busType = BusTypes_::kMain as i32;
            write_wstring("notes", &mut bus.name);
        }
        _ => return kInvalidArgument,
    }
    kResultOk
}

/// `setBusArrangements`: stereo everywhere, as many as it has.
unsafe fn arrangements(
    kind: Kind,
    inputs: *mut SpeakerArrangement,
    ins: i32,
    outputs: *mut SpeakerArrangement,
    outs: i32,
) -> tresult {
    let stereo = |ptr: *mut SpeakerArrangement, n: i32| {
        // SAFETY: the host passes `n` arrangements.
        (0..n.max(0) as usize).all(|i| unsafe { *ptr.add(i) } == SpeakerArr::kStereo)
    };
    if ins as usize != audio_buses(kind, true).len()
        || outs as usize != audio_buses(kind, false).len()
        || !stereo(inputs, ins)
        || !stereo(outputs, outs)
    {
        return kResultFalse;
    }
    kResultTrue
}

impl IComponentTrait for TestPlugin {
    unsafe fn getControllerClassId(&self, _class_id: *mut TUID) -> tresult {
        kNotImplemented
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media: MediaType, dir: BusDirection) -> int32 {
        bus_count(self.kind, media, dir)
    }

    unsafe fn getBusInfo(
        &self,
        media: MediaType,
        dir: BusDirection,
        index: int32,
        bus: *mut BusInfo,
    ) -> tresult {
        unsafe { bus_info(self.kind, media, dir, index, bus) }
    }

    unsafe fn getRoutingInfo(&self, _in: *mut RoutingInfo, _out: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(
        &self,
        _media: MediaType,
        _dir: BusDirection,
        _index: int32,
        _state: TBool,
    ) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, state: TBool) -> tresult {
        if state != 0 {
            let mut audio = self.audio.borrow_mut();
            let defaults = params(self.kind);
            *audio = Audio {
                value: defaults.first().map_or(0.0, |p| p.default),
                cc7: 1.0,
                rings: [
                    vec![0.0; DELAY_FRAMES as usize],
                    vec![0.0; DELAY_FRAMES as usize],
                ],
                at: 0,
                held: vec![0.0; 128],
                volume: vec![1.0; 128],
                notes: Vec::with_capacity(256),
            };
        }
        kResultOk
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
}

impl IAudioProcessorTrait for TestPlugin {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        ins: int32,
        outputs: *mut SpeakerArrangement,
        outs: int32,
    ) -> tresult {
        unsafe { arrangements(self.kind, inputs, ins, outputs, outs) }
    }

    unsafe fn getBusArrangement(
        &self,
        dir: BusDirection,
        index: int32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        let input = dir == BusDirections_::kInput as i32;
        if arr.is_null() || index < 0 || index as usize >= audio_buses(self.kind, input).len() {
            return kInvalidArgument;
        }
        unsafe { *arr = SpeakerArr::kStereo };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: int32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn getLatencySamples(&self) -> uint32 {
        if self.kind == Kind::Delay {
            DELAY_FRAMES
        } else {
            0
        }
    }

    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        // SAFETY: the host passes a live ProcessData.
        let data = unsafe { &mut *data };
        let mut audio = self.audio.borrow_mut();
        let block = unsafe { Block::new(data) };
        match self.kind {
            Kind::Gain => process_gain(&mut audio, &block),
            Kind::Delay => process_delay(&mut audio, &block),
            Kind::Beep => process_beep(&mut audio, &block),
            Kind::Octave => process_octave(&block),
            Kind::Tempo => process_tempo(&block),
            Kind::Subtract => process_subtract(&block),
            Kind::Split => process_split(&mut audio, &block),
            Kind::Memo => {}
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> uint32 {
        0
    }
}

impl IProcessContextRequirementsTrait for TestPlugin {
    unsafe fn getProcessContextRequirements(&self) -> uint32 {
        use IProcessContextRequirements_::Flags_::*;
        kNeedTempo | kNeedTimeSignature | kNeedBarPositionMusic | kNeedTransportState
    }
}

/// What the gain shows: dB with a decimal (the beep's level: percent).
fn text_of(kind: Kind, id: ParamID, value: f64) -> Option<String> {
    match (kind, id) {
        (Kind::Gain, GAIN) => {
            let db = gain_db(value);
            // A stand-in for a GUI's dialog, which holds up the main thread
            // until it's answered.
            if (db - STALL_DB).abs() < 1e-9 {
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
            Some(format!("{db:.1} dB"))
        }
        (Kind::Beep, LEVEL) => Some(format!("{:.0} %", value * 100.0)),
        _ => None,
    }
}

/// The reverse of [`text_of`].
fn value_of(kind: Kind, id: ParamID, text: &str) -> Option<f64> {
    let text = text.trim();
    match (kind, id) {
        (Kind::Gain, GAIN) => {
            let lower = text.to_ascii_lowercase();
            let number: f64 = lower
                .strip_suffix("db")
                .unwrap_or(&lower)
                .trim_end()
                .parse()
                .ok()?;
            Some(((number + 60.0) / 72.0).clamp(0.0, 1.0))
        }
        (Kind::Beep, LEVEL) => {
            let number: f64 = text.trim_end_matches('%').trim_end().parse().ok()?;
            Some((number / 100.0).clamp(0.0, 1.0))
        }
        _ => None,
    }
}

/// `getParameterInfo` for a plugin of `kind`.
unsafe fn param_info(kind: Kind, index: i32, info: *mut ParameterInfo) -> tresult {
    let all = params(kind);
    let Some(param) = usize::try_from(index).ok().and_then(|i| all.get(i)) else {
        return kInvalidArgument;
    };
    if info.is_null() {
        return kInvalidArgument;
    }
    // SAFETY: the host passes room for one.
    let info = unsafe { &mut *info };
    info.id = param.id;
    write_wstring(param.name, &mut info.title);
    write_wstring(param.name, &mut info.shortTitle);
    write_wstring("", &mut info.units);
    info.stepCount = 0;
    info.defaultNormalizedValue = param.default;
    info.unitId = kRootUnitId;
    info.flags = param.flags;
    kResultOk
}

impl IEditControllerTrait for TestPlugin {
    unsafe fn setComponentState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getParameterCount(&self) -> int32 {
        params(self.kind).len() as int32
    }

    unsafe fn getParameterInfo(&self, index: int32, info: *mut ParameterInfo) -> tresult {
        unsafe { param_info(self.kind, index, info) }
    }

    unsafe fn getParamStringByValue(
        &self,
        id: ParamID,
        value: ParamValue,
        string: *mut String128,
    ) -> tresult {
        match text_of(self.kind, id, value) {
            Some(text) if !string.is_null() => {
                // SAFETY: room for a String128.
                write_wstring(&text, unsafe { &mut *string });
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn getParamValueByString(
        &self,
        id: ParamID,
        string: *mut TChar,
        value: *mut ParamValue,
    ) -> tresult {
        if string.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        match value_of(self.kind, id, &unsafe { read_wstring(string) }) {
            Some(v) => {
                unsafe { *value = v };
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn normalizedParamToPlain(&self, id: ParamID, value: ParamValue) -> ParamValue {
        if self.kind == Kind::Gain && id == GAIN {
            gain_db(value)
        } else {
            value
        }
    }

    unsafe fn plainParamToNormalized(&self, id: ParamID, plain: ParamValue) -> ParamValue {
        if self.kind == Kind::Gain && id == GAIN {
            (plain + 60.0) / 72.0
        } else {
            plain
        }
    }

    unsafe fn getParamNormalized(&self, id: ParamID) -> ParamValue {
        self.param_index(id).map_or(0.0, |i| self.values[i].get())
    }

    unsafe fn setParamNormalized(&self, id: ParamID, value: ParamValue) -> tresult {
        let Some(i) = self.param_index(id) else {
            return kInvalidArgument;
        };
        self.values[i].set(value);
        // The beep's processor says key 127 was hit: turn Level to half,
        // as a knob on its GUI would.
        if self.kind == Kind::Beep && id == TRIGGER && value >= 0.5 {
            self.edit(LEVEL, 0.5);
        }
        kResultOk
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        // SAFETY: the host's handler (or null), kept until it's taken away.
        *self.handler.borrow_mut() = unsafe { ComRef::from_raw(handler) }.map(|h| h.to_com_ptr());
        kResultOk
    }

    unsafe fn createView(&self, name: FIDString) -> *mut IPlugView {
        // SAFETY: a NUL-terminated name.
        let editor = !name.is_null() && unsafe { CStr::from_ptr(name) } == c"editor";
        if self.kind != Kind::Gain || !editor {
            return std::ptr::null_mut();
        }
        TestView::create(self.handler.borrow().clone(), self.values.clone())
    }
}

impl IMidiMappingTrait for TestPlugin {
    unsafe fn getMidiControllerAssignment(
        &self,
        bus: int32,
        channel: int16,
        control: CtrlNumber,
        id: *mut ParamID,
    ) -> tresult {
        if bus != 0 || id.is_null() {
            return kResultFalse;
        }
        let target = match (self.kind, control as u32) {
            (Kind::Beep, 7) => CC7,
            (Kind::Octave, ControllerNumbers_::kPitchBend) => BEND + (channel as u32 & 15),
            _ => return kResultFalse,
        };
        unsafe { *id = target };
        kResultTrue
    }
}

/// One block, read from `ProcessData`.
struct Block<'a> {
    frames: usize,
    data: &'a ProcessData,
}

impl<'a> Block<'a> {
    /// SAFETY: `data` is the host's live block.
    unsafe fn new(data: &'a mut ProcessData) -> Self {
        Self {
            frames: data.numSamples.max(0) as usize,
            data,
        }
    }

    /// Channel `c` of audio bus `bus` one way, if there is one.
    fn channel(&self, input: bool, bus: usize, c: usize) -> Option<&'a mut [f32]> {
        let (buses, count) = if input {
            (self.data.inputs, self.data.numInputs)
        } else {
            (self.data.outputs, self.data.numOutputs)
        };
        if bus >= count.max(0) as usize || buses.is_null() {
            return None;
        }
        // SAFETY: the host passes `count` buses, each with its channels of
        // `frames` samples.
        unsafe {
            let bus = &*buses.add(bus);
            if c >= bus.numChannels.max(0) as usize {
                return None;
            }
            let channels = bus.__field0.channelBuffers32;
            if channels.is_null() || (*channels.add(c)).is_null() {
                return None;
            }
            Some(std::slice::from_raw_parts_mut(
                *channels.add(c),
                self.frames,
            ))
        }
    }

    /// The parameter changes coming in: (offset, ID, value), in offset
    /// order.
    fn changes(&self) -> Vec<(usize, ParamID, f64)> {
        let mut changes = Vec::new();
        // SAFETY: the host's list (or null), live for the block.
        let Some(list) = (unsafe { ComRef::from_raw(self.data.inputParameterChanges) }) else {
            return changes;
        };
        for i in 0..unsafe { list.getParameterCount() } {
            let Some(queue) = (unsafe { ComRef::from_raw(list.getParameterData(i)) }) else {
                continue;
            };
            let id = unsafe { queue.getParameterId() };
            for p in 0..unsafe { queue.getPointCount() } {
                let (mut offset, mut value) = (0, 0.0);
                if unsafe { queue.getPoint(p, &mut offset, &mut value) } == kResultOk {
                    changes.push((offset.max(0) as usize, id, value));
                }
            }
        }
        changes.sort_by_key(|c| c.0);
        changes
    }

    /// The events coming in, in offset order.
    fn events(&self) -> Vec<Event> {
        let mut events = Vec::new();
        // SAFETY: the host's list (or null), live for the block.
        let Some(list) = (unsafe { ComRef::from_raw(self.data.inputEvents) }) else {
            return events;
        };
        for i in 0..unsafe { list.getEventCount() } {
            let mut event: Event = unsafe { std::mem::zeroed() };
            if unsafe { list.getEvent(i, &mut event) } == kResultOk {
                events.push(event);
            }
        }
        events.sort_by_key(|e| e.sampleOffset);
        events
    }

    fn send(&self, mut event: Event) {
        // SAFETY: the host's list (or null), live for the block.
        if let Some(list) = unsafe { ComRef::from_raw(self.data.outputEvents) } {
            unsafe { list.addEvent(&mut event) };
        }
    }

    fn send_param(&self, id: ParamID, offset: usize, value: f64) {
        // SAFETY: the host's list (or null), live for the block.
        let Some(list) = (unsafe { ComRef::from_raw(self.data.outputParameterChanges) }) else {
            return;
        };
        let mut index = 0;
        let queue = unsafe { list.addParameterData(&id, &mut index) };
        if let Some(queue) = unsafe { ComRef::from_raw(queue) } {
            unsafe { queue.addPoint(offset as i32, value, &mut index) };
        }
    }

    fn context(&self) -> Option<&'a ProcessContext> {
        // SAFETY: the host's context (or null), live for the block.
        unsafe { self.data.processContext.as_ref() }
    }
}

fn process_gain(audio: &mut Audio, block: &Block) {
    let changes = block.changes();
    let mut next = 0;
    for s in 0..block.frames {
        while let Some(&(at, id, value)) = changes.get(next)
            && at <= s
        {
            if id == GAIN {
                audio.value = value;
            }
            next += 1;
        }
        let gain = 10f64.powf(gain_db(audio.value) / 20.0) as f32;
        for c in 0..2 {
            let input = block.channel(true, 0, c).map_or(0.0, |i| i[s]);
            if let Some(out) = block.channel(false, 0, c) {
                out[s] = input * gain;
            }
        }
    }
}

fn process_delay(audio: &mut Audio, block: &Block) {
    let start = audio.at;
    for c in 0..2 {
        let mut at = start;
        let ring = &mut audio.rings[c];
        let input: Option<Vec<f32>> = block.channel(true, 0, c).map(|i| i.to_vec());
        if let Some(out) = block.channel(false, 0, c) {
            for s in 0..block.frames {
                let x = input.as_ref().map_or(0.0, |i| i[s]);
                out[s] = ring[at];
                ring[at] = x;
                at = (at + 1) % ring.len();
            }
        }
    }
    audio.at = (start + block.frames) % DELAY_FRAMES as usize;
}

/// The key of note `id` (as it was given, or Dawkey's own ID).
fn key_of(audio: &Audio, id: i32) -> usize {
    audio
        .notes
        .iter()
        .rev()
        .find(|n| n.0 == id)
        .map_or((id & 127) as usize, |n| n.1)
}

/// What the beep and split do with an event: notes held, volumes.
fn take_note(audio: &mut Audio, event: &Event) {
    // SAFETY: the union holds the variant `type` says.
    unsafe {
        match u32::from(event.r#type) {
            Event_::EventTypes_::kNoteOnEvent => {
                let e = event.__field0.noteOn;
                let key = (e.pitch & 127) as usize;
                audio.held[key] = f64::from(e.velocity);
                if audio.notes.len() == audio.notes.capacity() {
                    audio.notes.remove(0);
                }
                audio.notes.push((e.noteId, key));
            }
            Event_::EventTypes_::kNoteOffEvent => {
                let e = event.__field0.noteOff;
                audio.held[(e.pitch & 127) as usize] = 0.0;
            }
            Event_::EventTypes_::kNoteExpressionValueEvent => {
                let e = event.__field0.noteExpressionValue;
                if e.typeId == NoteExpressionTypeIDs_::kVolumeTypeID {
                    let key = key_of(audio, e.noteId);
                    // 0.25 is unity.
                    audio.volume[key] = e.value * 4.0;
                }
            }
            _ => {}
        }
    }
}

fn process_beep(audio: &mut Audio, block: &Block) {
    let changes = block.changes();
    let events = block.events();
    let (mut next_change, mut next_event) = (0, 0);
    for s in 0..block.frames {
        while let Some(&(at, id, value)) = changes.get(next_change)
            && at <= s
        {
            match id {
                LEVEL => audio.value = value,
                CC7 => audio.cc7 = value,
                _ => {}
            }
            next_change += 1;
        }
        while let Some(event) = events.get(next_event)
            && event.sampleOffset.max(0) as usize <= s
        {
            let is_127 = u32::from(event.r#type) == Event_::EventTypes_::kNoteOnEvent
                && unsafe { event.__field0.noteOn.pitch } == 127;
            if is_127 {
                // Key 127: tell the controller, which turns Level to half.
                audio.value = 0.5;
                block.send_param(TRIGGER, s, 1.0);
            } else {
                take_note(audio, event);
            }
            next_event += 1;
        }
        let held: f64 = audio
            .held
            .iter()
            .zip(&audio.volume)
            .map(|(v, g)| v * g)
            .sum();
        let level = (held * audio.value * audio.cc7) as f32;
        for c in 0..2 {
            if let Some(out) = block.channel(false, 0, c) {
                out[s] = level;
            }
        }
    }
}

/// A note ID an octave up (Dawkey's IDs carry the key).
fn up_id(id: i32) -> i32 {
    if id >= 0 && (id & 127) <= 115 {
        id + 12
    } else {
        id
    }
}

fn up_pitch(pitch: i16) -> i16 {
    if (0..=115).contains(&pitch) {
        pitch + 12
    } else {
        pitch
    }
}

fn process_octave(block: &Block) {
    for mut event in block.events() {
        // SAFETY: the union holds the variant `type` says.
        unsafe {
            match u32::from(event.r#type) {
                Event_::EventTypes_::kNoteOnEvent => {
                    let e = &mut event.__field0.noteOn;
                    e.pitch = up_pitch(e.pitch);
                    e.noteId = up_id(e.noteId);
                }
                Event_::EventTypes_::kNoteOffEvent => {
                    let e = &mut event.__field0.noteOff;
                    e.pitch = up_pitch(e.pitch);
                    e.noteId = up_id(e.noteId);
                }
                Event_::EventTypes_::kPolyPressureEvent => {
                    let e = &mut event.__field0.polyPressure;
                    e.pitch = up_pitch(e.pitch);
                    e.noteId = up_id(e.noteId);
                }
                Event_::EventTypes_::kNoteExpressionValueEvent => {
                    let e = &mut event.__field0.noteExpressionValue;
                    e.noteId = up_id(e.noteId);
                }
                _ => continue,
            }
        }
        block.send(event);
    }
    // Pitch bend comes as a parameter, and goes on as MIDI.
    for (offset, id, value) in block.changes() {
        if !(BEND..BEND + 16).contains(&id) {
            continue;
        }
        let bend = (value.clamp(0.0, 1.0) * 16383.0).round() as u16;
        block.send(Event {
            busIndex: 0,
            sampleOffset: offset as i32,
            ppqPosition: 0.0,
            flags: 0,
            r#type: Event_::EventTypes_::kLegacyMIDICCOutEvent as u16,
            __field0: Event__type0 {
                midiCCOut: LegacyMIDICCOutEvent {
                    controlNumber: ControllerNumbers_::kPitchBend as u8,
                    channel: (id - BEND) as i8,
                    value: (bend & 127) as i8,
                    value2: (bend >> 7) as i8,
                },
            },
        });
    }
}

fn process_tempo(block: &Block) {
    use ProcessContext_::StatesAndFlags_::*;
    let (tempo, bar) = match block.context() {
        Some(t) => (
            if t.state & kTempoValid != 0 {
                t.tempo / 1000.0
            } else {
                0.0
            },
            if t.state & kPlaying != 0 {
                // VST3 says where the bar starts, in quarter notes: with
                // one time signature, that's the bar's number.
                let bar_beats =
                    4.0 * f64::from(t.timeSigNumerator) / f64::from(t.timeSigDenominator.max(1));
                let bar = (t.barPositionMusic / bar_beats).round();
                (bar + 1.0) / 100.0
            } else {
                0.0
            },
        ),
        None => (0.0, 0.0),
    };
    for (c, value) in [tempo, bar].into_iter().enumerate() {
        if let Some(out) = block.channel(false, 0, c) {
            out.fill(value as f32);
        }
    }
}

fn process_subtract(block: &Block) {
    for c in 0..2 {
        let main = block.channel(true, 0, c).map(|i| i.to_vec());
        let side = block.channel(true, 1, c).map(|i| i.to_vec());
        if let Some(out) = block.channel(false, 0, c) {
            for (s, o) in out.iter_mut().enumerate() {
                *o = main.as_ref().map_or(0.0, |m| m[s]) - side.as_ref().map_or(0.0, |x| x[s]);
            }
        }
    }
}

fn process_split(audio: &mut Audio, block: &Block) {
    for event in block.events() {
        take_note(audio, &event);
    }
    let low = audio.held[..60].iter().sum::<f64>() as f32;
    let high = audio.held[60..].iter().sum::<f64>() as f32;
    for (bus, level) in [(0, low), (1, high)] {
        for c in 0..2 {
            if let Some(out) = block.channel(false, bus, c) {
                out.fill(level);
            }
        }
    }
}

/// The memo's processor: remembers the velocity of the last note it got,
/// only in its state, and adds it to its audio as DC. It tells its
/// controller (a read-only parameter it sends), which marks the state
/// dirty. It won't start until its controller has said hello over their
/// connection.
struct MemoProcessor {
    memo: Value,
    hello: AtomicBool,
    peer: RefCell<Option<ComPtr<IConnectionPoint>>>,
}

impl Class for MemoProcessor {
    type Interfaces = (IComponent, IAudioProcessor, IConnectionPoint);
}

impl IPluginBaseTrait for MemoProcessor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for MemoProcessor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        if class_id.is_null() {
            return kInvalidArgument;
        }
        unsafe { *class_id = MEMO_CONTROLLER };
        kResultOk
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media: MediaType, dir: BusDirection) -> int32 {
        bus_count(Kind::Memo, media, dir)
    }

    unsafe fn getBusInfo(
        &self,
        media: MediaType,
        dir: BusDirection,
        index: int32,
        bus: *mut BusInfo,
    ) -> tresult {
        unsafe { bus_info(Kind::Memo, media, dir, index, bus) }
    }

    unsafe fn getRoutingInfo(&self, _in: *mut RoutingInfo, _out: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(
        &self,
        _media: MediaType,
        _dir: BusDirection,
        _index: int32,
        _state: TBool,
    ) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, state: TBool) -> tresult {
        if state != 0 && !self.hello.load(Ordering::Acquire) {
            return kResultFalse;
        }
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
            return kInvalidArgument;
        };
        let mut bytes = [0u8; 8];
        let mut read = 0;
        let result = unsafe { stream.read(bytes.as_mut_ptr() as *mut c_void, 8, &mut read) };
        if result != kResultOk || read != 8 {
            return kResultFalse;
        }
        self.memo.set(f64::from_le_bytes(bytes));
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
            return kInvalidArgument;
        };
        let mut bytes = self.memo.get().to_le_bytes();
        let mut written = 0;
        unsafe { stream.write(bytes.as_mut_ptr() as *mut c_void, 8, &mut written) }
    }
}

impl IAudioProcessorTrait for MemoProcessor {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        ins: int32,
        outputs: *mut SpeakerArrangement,
        outs: int32,
    ) -> tresult {
        unsafe { arrangements(Kind::Memo, inputs, ins, outputs, outs) }
    }

    unsafe fn getBusArrangement(
        &self,
        _dir: BusDirection,
        index: int32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        if arr.is_null() || index != 0 {
            return kInvalidArgument;
        }
        unsafe { *arr = SpeakerArr::kStereo };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: int32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn getLatencySamples(&self) -> uint32 {
        0
    }

    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        // SAFETY: the host passes a live ProcessData.
        let block = unsafe { Block::new(&mut *data) };
        let events = block.events();
        let mut next = 0;
        for s in 0..block.frames {
            while let Some(event) = events.get(next)
                && event.sampleOffset.max(0) as usize <= s
            {
                if u32::from(event.r#type) == Event_::EventTypes_::kNoteOnEvent {
                    let velocity = f64::from(unsafe { event.__field0.noteOn.velocity });
                    self.memo.set(velocity);
                    block.send_param(1, s, velocity);
                }
                next += 1;
            }
            let memo = self.memo.get() as f32;
            for c in 0..2 {
                let input = block.channel(true, 0, c).map_or(0.0, |i| i[s]);
                if let Some(out) = block.channel(false, 0, c) {
                    out[s] = input + memo;
                }
            }
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> uint32 {
        0
    }
}

impl IConnectionPointTrait for MemoProcessor {
    unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
        *self.peer.borrow_mut() = unsafe { ComRef::from_raw(other) }.map(|o| o.to_com_ptr());
        kResultOk
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        *self.peer.borrow_mut() = None;
        kResultOk
    }

    unsafe fn notify(&self, message: *mut IMessage) -> tresult {
        let Some(message) = (unsafe { ComRef::from_raw(message) }) else {
            return kInvalidArgument;
        };
        let id = unsafe { message.getMessageID() };
        if id.is_null() || unsafe { CStr::from_ptr(id) } != c"hello" {
            return kResultFalse;
        }
        let Some(attributes) = (unsafe { ComRef::from_raw(message.getAttributes()) }) else {
            return kResultFalse;
        };
        let mut value = 0;
        if unsafe { attributes.getInt(c"answer".as_ptr(), &mut value) } == kResultOk && value == 42
        {
            self.hello.store(true, Ordering::Release);
        }
        kResultOk
    }
}

/// The memo's controller: says hello to its processor when connected, and
/// marks the state dirty when the processor says it has a new memory.
struct MemoController {
    memo: Value,
    host: RefCell<Option<ComPtr<IHostApplication>>>,
    handler: RefCell<Option<ComPtr<IComponentHandler>>>,
}

impl Class for MemoController {
    type Interfaces = (IEditController, IConnectionPoint);
}

impl IPluginBaseTrait for MemoController {
    unsafe fn initialize(&self, context: *mut FUnknown) -> tresult {
        *self.host.borrow_mut() = unsafe { ComRef::from_raw(context) }.and_then(|c| c.cast());
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        *self.host.borrow_mut() = None;
        *self.handler.borrow_mut() = None;
        kResultOk
    }
}

impl IConnectionPointTrait for MemoController {
    unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
        let Some(other) = (unsafe { ComRef::from_raw(other) }) else {
            return kInvalidArgument;
        };
        let host = self.host.borrow();
        let Some(host) = host.as_ref() else {
            return kResultFalse;
        };
        // A message, made by the host.
        let mut cid = IMessage::IID;
        let mut iid = IMessage::IID;
        let mut object = std::ptr::null_mut();
        let made = unsafe {
            host.createInstance(
                &mut cid as *mut _ as *mut TUID,
                &mut iid as *mut _ as *mut TUID,
                &mut object,
            )
        };
        if made != kResultOk {
            return kResultFalse;
        }
        let Some(message) = (unsafe { ComPtr::from_raw(object as *mut IMessage) }) else {
            return kResultFalse;
        };
        unsafe {
            message.setMessageID(c"hello".as_ptr());
            if let Some(attributes) = ComRef::from_raw(message.getAttributes()) {
                attributes.setInt(c"answer".as_ptr(), 42);
            }
            other.notify(message.as_ptr());
        }
        kResultOk
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultOk
    }

    unsafe fn notify(&self, _message: *mut IMessage) -> tresult {
        kResultOk
    }
}

impl IEditControllerTrait for MemoController {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
            return kInvalidArgument;
        };
        let mut bytes = [0u8; 8];
        let mut read = 0;
        if unsafe { stream.read(bytes.as_mut_ptr() as *mut c_void, 8, &mut read) } == kResultOk
            && read == 8
        {
            self.memo.set(f64::from_le_bytes(bytes));
        }
        kResultOk
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getParameterCount(&self) -> int32 {
        1
    }

    unsafe fn getParameterInfo(&self, index: int32, info: *mut ParameterInfo) -> tresult {
        unsafe { param_info(Kind::Memo, index, info) }
    }

    unsafe fn getParamStringByValue(
        &self,
        _id: ParamID,
        value: ParamValue,
        string: *mut String128,
    ) -> tresult {
        if string.is_null() {
            return kInvalidArgument;
        }
        write_wstring(&format!("{value:.2}"), unsafe { &mut *string });
        kResultOk
    }

    unsafe fn getParamValueByString(
        &self,
        _id: ParamID,
        _string: *mut TChar,
        _value: *mut ParamValue,
    ) -> tresult {
        kResultFalse
    }

    unsafe fn normalizedParamToPlain(&self, _id: ParamID, value: ParamValue) -> ParamValue {
        value
    }

    unsafe fn plainParamToNormalized(&self, _id: ParamID, plain: ParamValue) -> ParamValue {
        plain
    }

    unsafe fn getParamNormalized(&self, id: ParamID) -> ParamValue {
        if id == 1 { self.memo.get() } else { 0.0 }
    }

    unsafe fn setParamNormalized(&self, id: ParamID, value: ParamValue) -> tresult {
        if id != 1 {
            return kInvalidArgument;
        }
        if value != self.memo.get() {
            self.memo.set(value);
            // The processor remembered something: the state changed.
            if let Some(handler) = &*self.handler.borrow()
                && let Some(handler) = handler.cast::<IComponentHandler2>()
            {
                unsafe { handler.setDirty(1) };
            }
        }
        kResultOk
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        *self.handler.borrow_mut() = unsafe { ComRef::from_raw(handler) }.map(|h| h.to_com_ptr());
        kResultOk
    }

    unsafe fn createView(&self, _name: FIDString) -> *mut IPlugView {
        std::ptr::null_mut()
    }
}

/// The gain's editor: it draws nothing (whatever window it's given), but
/// lives as an editor does: it takes a run loop timer from its frame, and
/// on the first tick moves its knob (an edit through the host) and asks
/// for a bigger window.
struct TestView {
    state: std::rc::Rc<ViewState>,
    timer: ComWrapper<ViewTimer>,
}

struct ViewState {
    handler: Option<ComPtr<IComponentHandler>>,
    /// Its controller's values, which its knob sets.
    values: std::sync::Arc<Vec<Value>>,
    frame: RefCell<Option<ComPtr<IPlugFrame>>>,
    run_loop: RefCell<Option<ComPtr<IRunLoop>>>,
    /// The view itself, as its frame knows it (not a reference).
    this: Cell<*mut IPlugView>,
    size: Cell<(i32, i32)>,
    ticks: Cell<u32>,
}

struct ViewTimer {
    state: std::rc::Rc<ViewState>,
}

impl Class for TestView {
    type Interfaces = (IPlugView,);
}

impl Class for ViewTimer {
    type Interfaces = (ITimerHandler,);
}

impl TestView {
    fn create(
        handler: Option<ComPtr<IComponentHandler>>,
        values: std::sync::Arc<Vec<Value>>,
    ) -> *mut IPlugView {
        let state = std::rc::Rc::new(ViewState {
            handler,
            values,
            frame: RefCell::new(None),
            run_loop: RefCell::new(None),
            this: Cell::new(std::ptr::null_mut()),
            size: Cell::new((200, 100)),
            ticks: Cell::new(0),
        });
        let view = ComWrapper::new(TestView {
            timer: ComWrapper::new(ViewTimer {
                state: state.clone(),
            }),
            state,
        });
        let ptr = view.to_com_ptr::<IPlugView>().expect("it's a view");
        view.state.this.set(ptr.as_ptr());
        ptr.into_raw()
    }
}

impl ITimerHandlerTrait for ViewTimer {
    unsafe fn onTimer(&self) {
        let state = &self.state;
        state.ticks.set(state.ticks.get() + 1);
        if state.ticks.get() != 1 {
            return;
        }
        // The knob, turned to -42 dB...
        state.values[0].set(0.25);
        if let Some(handler) = &state.handler {
            unsafe {
                handler.beginEdit(GAIN);
                handler.performEdit(GAIN, 0.25);
                handler.endEdit(GAIN);
            }
        }
        // ...and the editor wants more room.
        if let Some(frame) = &*state.frame.borrow() {
            let mut rect = ViewRect {
                left: 0,
                top: 0,
                right: 300,
                bottom: 150,
            };
            unsafe { frame.resizeView(state.this.get(), &mut rect) };
        }
    }
}

impl IPlugViewTrait for TestView {
    unsafe fn isPlatformTypeSupported(&self, kind: FIDString) -> tresult {
        if !kind.is_null() && unsafe { CStr::from_ptr(kind) } == c"X11EmbedWindowID" {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn attached(&self, _parent: *mut c_void, _kind: FIDString) -> tresult {
        let frame = self.state.frame.borrow();
        let Some(run_loop) = frame.as_ref().and_then(|f| f.cast::<IRunLoop>()) else {
            return kResultFalse;
        };
        let timer = self
            .timer
            .to_com_ptr::<ITimerHandler>()
            .expect("it's a timer");
        if unsafe { run_loop.registerTimer(timer.as_ptr(), 10) } != kResultOk {
            return kResultFalse;
        }
        *self.state.run_loop.borrow_mut() = Some(run_loop);
        kResultOk
    }

    unsafe fn removed(&self) -> tresult {
        if let Some(run_loop) = self.state.run_loop.borrow_mut().take() {
            let timer = self
                .timer
                .to_com_ptr::<ITimerHandler>()
                .expect("it's a timer");
            unsafe { run_loop.unregisterTimer(timer.as_ptr()) };
        }
        kResultOk
    }

    unsafe fn onWheel(&self, _distance: f32) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyDown(&self, _key: char16, _code: int16, _modifiers: int16) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyUp(&self, _key: char16, _code: int16, _modifiers: int16) -> tresult {
        kResultFalse
    }

    unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
        if size.is_null() {
            return kInvalidArgument;
        }
        let (w, h) = self.state.size.get();
        unsafe {
            *size = ViewRect {
                left: 0,
                top: 0,
                right: w,
                bottom: h,
            }
        };
        kResultOk
    }

    unsafe fn onSize(&self, size: *mut ViewRect) -> tresult {
        if size.is_null() {
            return kInvalidArgument;
        }
        let r = unsafe { *size };
        self.state.size.set((r.right - r.left, r.bottom - r.top));
        kResultOk
    }

    unsafe fn onFocus(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
        *self.state.frame.borrow_mut() = unsafe { ComRef::from_raw(frame) }.map(|f| f.to_com_ptr());
        kResultOk
    }

    unsafe fn canResize(&self) -> tresult {
        kResultTrue
    }

    unsafe fn checkSizeConstraint(&self, rect: *mut ViewRect) -> tresult {
        if rect.is_null() {
            return kInvalidArgument;
        }
        // At least 100 × 50.
        let r = unsafe { &mut *rect };
        r.right = r.right.max(r.left + 100);
        r.bottom = r.bottom.max(r.top + 50);
        kResultTrue
    }
}

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory2,);
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        if info.is_null() {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        write_cstring("Dawkey", &mut info.vendor);
        write_cstring("https://github.com/quonic/dawkey", &mut info.url);
        write_cstring("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;
        kResultOk
    }

    unsafe fn countClasses(&self) -> int32 {
        CLASSES.len() as int32 + 1
    }

    unsafe fn getClassInfo(&self, index: int32, info: *mut PClassInfo) -> tresult {
        if info.is_null() {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        let (class_id, category, name) = match class(index) {
            Some(Some((n, name, _))) => (cid(n), "Audio Module Class", name),
            Some(None) => (
                MEMO_CONTROLLER,
                "Component Controller Class",
                "Test Memo (VST3)",
            ),
            None => return kInvalidArgument,
        };
        info.cid = class_id;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        write_cstring(category, &mut info.category);
        write_cstring(name, &mut info.name);
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: 16-byte IDs.
        let class_id = unsafe { *(cid as *const TUID) };
        let object = if class_id == MEMO_CONTROLLER {
            ComWrapper::new(MemoController {
                memo: Value::new(0.0),
                host: RefCell::new(None),
                handler: RefCell::new(None),
            })
            .to_com_ptr::<FUnknown>()
        } else {
            match (0..CLASSES.len() as u32).find(|&n| self::cid(n) == class_id) {
                Some(n) if CLASSES[n as usize].2 == Kind::Memo => ComWrapper::new(MemoProcessor {
                    memo: Value::new(0.0),
                    hello: AtomicBool::new(false),
                    peer: RefCell::new(None),
                })
                .to_com_ptr::<FUnknown>(),
                Some(n) => {
                    ComWrapper::new(TestPlugin::new(CLASSES[n as usize].2)).to_com_ptr::<FUnknown>()
                }
                None => None,
            }
        };
        let Some(object) = object else {
            unsafe { *obj = std::ptr::null_mut() };
            return kInvalidArgument;
        };
        let ptr = object.as_ptr();
        // SAFETY: a live object; queryInterface adds the caller's reference.
        unsafe { ((*(*ptr).vtbl).queryInterface)(ptr, iid as *const TUID, obj) }
    }
}

/// Class `index` of the factory: a test plugin, or (`Some(None)`) the
/// memo's controller, last.
fn class(index: i32) -> Option<Option<(u32, &'static str, Kind)>> {
    let index = usize::try_from(index).ok()?;
    match CLASSES.get(index) {
        Some(&(_, name, kind)) => Some(Some((index as u32, name, kind))),
        None if index == CLASSES.len() => Some(None),
        None => None,
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: int32, info: *mut PClassInfo2) -> tresult {
        if info.is_null() {
            return kInvalidArgument;
        }
        let mut basic: PClassInfo = unsafe { std::mem::zeroed() };
        if unsafe { self.getClassInfo(index, &mut basic) } != kResultOk {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        info.cid = basic.cid;
        info.cardinality = basic.cardinality;
        info.category = basic.category;
        info.name = basic.name;
        info.classFlags = 0;
        let sub = match class(index) {
            Some(Some((_, _, Kind::Beep | Kind::Split))) => "Instrument",
            _ => "Fx",
        };
        write_cstring(sub, &mut info.subCategories);
        write_cstring("Dawkey", &mut info.vendor);
        write_cstring(env!("CARGO_PKG_VERSION"), &mut info.version);
        write_cstring("VST 3.8.0", &mut info.sdkVersion);
        kResultOk
    }
}

#[unsafe(no_mangle)]
extern "C" fn ModuleEntry(_library: *mut c_void) -> bool {
    true
}

#[unsafe(no_mangle)]
extern "C" fn ModuleExit() -> bool {
    true
}

#[unsafe(no_mangle)]
extern "C" fn GetPluginFactory() -> *mut IPluginFactory {
    // For testing crash isolation: a plugin that takes its host down.
    if std::env::var_os("dawkey_TEST_PLUGIN_CRASH").is_some() {
        std::process::abort();
    }
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory>()
        .expect("it's a factory")
        .into_raw()
}
