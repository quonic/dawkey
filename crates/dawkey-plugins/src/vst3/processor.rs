// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real-time half of a VST3 plugin: the engine's block, events and
//! transport as VST3's `ProcessData`, and what the plugin sends back.
//! Everything it hands the plugin is made when it loads, so processing
//! never allocates.

use std::any::Any;
use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dawkey_engine::{MAX_BLOCK, MidiSink, PerNote, PluginEvent, PluginEventKind, PluginProcessor};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComWrapper};

use super::plugin::Plugin;
use crate::expression::{self, DEFAULT_EXPRESSIONS, NoteExpression};
use crate::ports::{self, PortBuffer, Ports};

/// Points one parameter can get in a block; more replace the last.
const POINTS: usize = 64;

/// Events a block can carry each way; more are dropped.
const EVENTS: usize = 2048;

/// Note IDs of notes the plugin sent, remembered to place its expressions.
const NOTE_IDS: usize = 256;

/// One parameter's changes in a block (`IParamValueQueue`).
pub(crate) struct ValueQueue {
    id: ParamID,
    points: UnsafeCell<Vec<(i32, f64)>>,
}

// SAFETY: a block's queues are used by one thread at a time: the audio
// thread while it fills them and the plugin processes.
unsafe impl Send for ValueQueue {}
unsafe impl Sync for ValueQueue {}

impl Class for ValueQueue {
    type Interfaces = (IParamValueQueue,);
}

impl ValueQueue {
    #[allow(clippy::mut_from_ref)]
    fn points(&self) -> &mut Vec<(i32, f64)> {
        // SAFETY: see the `Sync` impl: no other reference is live.
        unsafe { &mut *self.points.get() }
    }

    fn add(&self, offset: i32, value: f64) -> usize {
        let points = self.points();
        // Same offset again: the later value wins.
        if let Some(last) = points.last_mut()
            && last.0 >= offset
        {
            *last = (last.0, value);
            return points.len() - 1;
        }
        if points.len() < points.capacity() {
            points.push((offset, value));
        } else if let Some(last) = points.last_mut() {
            *last = (offset, value);
        }
        points.len() - 1
    }
}

impl IParamValueQueueTrait for ValueQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id
    }

    unsafe fn getPointCount(&self) -> int32 {
        self.points().len() as int32
    }

    unsafe fn getPoint(&self, index: int32, offset: *mut int32, value: *mut ParamValue) -> tresult {
        let Some(&(at, v)) = usize::try_from(index)
            .ok()
            .and_then(|i| self.points().get(i))
        else {
            return kInvalidArgument;
        };
        // SAFETY: the plugin passes somewhere to put them.
        unsafe {
            if !offset.is_null() {
                *offset = at;
            }
            if !value.is_null() {
                *value = v;
            }
        }
        kResultOk
    }

    unsafe fn addPoint(&self, offset: int32, value: ParamValue, index: *mut int32) -> tresult {
        let at = self.add(offset, value);
        if !index.is_null() {
            unsafe { *index = at as int32 };
        }
        kResultOk
    }
}

/// A block's parameter changes (`IParameterChanges`), with a queue made
/// ahead for every parameter that can change.
pub(crate) struct ParamChanges {
    queues: Box<[ComWrapper<ValueQueue>]>,
    /// Queue by parameter ID, sorted.
    by_id: Box<[(ParamID, u32)]>,
    /// The queues with changes this block, in the order they got them.
    used: UnsafeCell<Vec<u32>>,
}

// SAFETY: as for `ValueQueue`.
unsafe impl Send for ParamChanges {}
unsafe impl Sync for ParamChanges {}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl ParamChanges {
    pub(crate) fn new(ids: impl IntoIterator<Item = ParamID>) -> Self {
        let mut ids: Vec<ParamID> = ids.into_iter().collect();
        ids.sort_unstable();
        ids.dedup();
        let queues: Box<[_]> = ids
            .iter()
            .map(|&id| {
                ComWrapper::new(ValueQueue {
                    id,
                    points: UnsafeCell::new(Vec::with_capacity(POINTS)),
                })
            })
            .collect();
        Self {
            by_id: ids
                .iter()
                .enumerate()
                .map(|(i, &id)| (id, i as u32))
                .collect(),
            used: UnsafeCell::new(Vec::with_capacity(queues.len())),
            queues,
        }
    }

    #[allow(clippy::mut_from_ref)]
    fn used(&self) -> &mut Vec<u32> {
        // SAFETY: see the `Sync` impl.
        unsafe { &mut *self.used.get() }
    }

    fn clear(&self) {
        for &q in self.used().iter() {
            self.queues[q as usize].points().clear();
        }
        self.used().clear();
    }

    /// The queue for `id`, marked used (`None` if there's none for it).
    fn queue(&self, id: ParamID) -> Option<(usize, &ValueQueue)> {
        let q = self
            .by_id
            .binary_search_by_key(&id, |&(id, _)| id)
            .ok()
            .map(|i| self.by_id[i].1)?;
        let used = self.used();
        let index = match used.iter().position(|&u| u == q) {
            Some(index) => index,
            None => {
                used.push(q);
                used.len() - 1
            }
        };
        Some((index, &self.queues[q as usize]))
    }

    fn add(&self, id: ParamID, offset: u32, value: f64) {
        if let Some((_, queue)) = self.queue(id) {
            queue.add(offset as i32, value.clamp(0.0, 1.0));
        }
    }

    /// Each changed parameter's last value.
    fn last_values(&self) -> impl Iterator<Item = (ParamID, f64)> + '_ {
        self.used().iter().filter_map(|&q| {
            let queue = &self.queues[q as usize];
            queue.points().last().map(|&(_, v)| (queue.id, v))
        })
    }
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        self.used().len() as int32
    }

    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        let Some(&q) = usize::try_from(index).ok().and_then(|i| self.used().get(i)) else {
            return std::ptr::null_mut();
        };
        // Not an added reference: the changes own their queues.
        self.queues[q as usize]
            .as_com_ref::<IParamValueQueue>()
            .map_or(std::ptr::null_mut(), |q| q.as_ptr())
    }

    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut int32,
    ) -> *mut IParamValueQueue {
        if id.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: the plugin passes a parameter ID.
        let Some((at, queue)) = self.queue(unsafe { *id }) else {
            return std::ptr::null_mut();
        };
        if !index.is_null() {
            unsafe { *index = at as int32 };
        }
        let _ = queue;
        unsafe { self.getParameterData(at as int32) }
    }
}

/// A block's events (`IEventList`).
pub(crate) struct EventList {
    events: UnsafeCell<Vec<Event>>,
}

// SAFETY: as for `ValueQueue`.
unsafe impl Send for EventList {}
unsafe impl Sync for EventList {}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl EventList {
    fn new() -> Self {
        Self {
            events: UnsafeCell::new(Vec::with_capacity(EVENTS)),
        }
    }

    #[allow(clippy::mut_from_ref)]
    fn events(&self) -> &mut Vec<Event> {
        // SAFETY: see the `Sync` impl.
        unsafe { &mut *self.events.get() }
    }

    fn push(&self, event: Event) {
        let events = self.events();
        if events.len() < events.capacity() {
            events.push(event);
        }
    }
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> int32 {
        self.events().len() as int32
    }

    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        let Some(event) = usize::try_from(index)
            .ok()
            .and_then(|i| self.events().get(i))
        else {
            return kInvalidArgument;
        };
        if e.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the plugin passes room for an event.
        unsafe { *e = *event };
        kResultOk
    }

    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        if e.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the plugin passes an event.
        self.push(unsafe { *e });
        kResultOk
    }
}

/// Which parameter each MIDI controller of each channel changes (the
/// plugin's `IMidiMapping`), for live changes from the control thread.
pub(crate) struct MidiMap {
    /// By `channel * CONTROLS + controller`: the parameter ID, and its step
    /// count above it (for program changes); `u64::MAX` for none.
    slots: Box<[AtomicU64]>,
}

/// Controllers VST3 maps: CCs 0-127, aftertouch, pitch bend, program
/// change.
pub(crate) const CONTROLS: usize = 131;

impl MidiMap {
    pub(crate) fn new() -> Self {
        Self {
            slots: (0..16 * CONTROLS)
                .map(|_| AtomicU64::new(u64::MAX))
                .collect(),
        }
    }

    pub(crate) fn set(&self, channel: usize, control: usize, target: Option<(ParamID, u32)>) {
        let value = target.map_or(u64::MAX, |(id, steps)| {
            u64::from(id) | (u64::from(steps) << 32)
        });
        self.slots[channel * CONTROLS + control].store(value, Ordering::Relaxed);
    }

    fn get(&self, channel: u8, control: usize) -> Option<(ParamID, u32)> {
        let value =
            self.slots[usize::from(channel & 15) * CONTROLS + control].load(Ordering::Relaxed);
        (value != u64::MAX).then_some((value as u32, (value >> 32) as u32))
    }
}

/// What a VST3 note expression is, and its value in VST3's terms.
fn vst3_expression(expression: NoteExpression, value: f64) -> Option<(NoteExpressionTypeID, f64)> {
    use NoteExpressionTypeIDs_::*;
    Some(match expression {
        NoteExpression::Tuning => (kTuningTypeID, 0.5 + value / 240.0),
        // 0.25 is unity, 1 is +12 dB.
        NoteExpression::Volume => (kVolumeTypeID, value / 4.0),
        NoteExpression::Pan => (kPanTypeID, value),
        NoteExpression::Expression => (kExpressionTypeID, value),
        NoteExpression::Brightness => (kBrightnessTypeID, value),
        NoteExpression::Vibrato => (kVibratoTypeID, value),
        NoteExpression::Pressure => return None,
    })
}

/// The reverse of [`vst3_expression`].
#[allow(non_upper_case_globals)]
fn from_vst3_expression(id: NoteExpressionTypeID, value: f64) -> Option<(NoteExpression, f64)> {
    use NoteExpressionTypeIDs_::*;
    Some(match id {
        kTuningTypeID => (NoteExpression::Tuning, (value - 0.5) * 240.0),
        kVolumeTypeID => (NoteExpression::Volume, value * 4.0),
        kPanTypeID => (NoteExpression::Pan, value),
        kExpressionTypeID => (NoteExpression::Expression, value),
        kBrightnessTypeID => (NoteExpression::Brightness, value),
        kVibratoTypeID => (NoteExpression::Vibrato, value),
        _ => return None,
    })
}

/// The note ID Dawkey gives a key's notes: one per channel and key, so
/// expressions find the notes they're for.
pub(crate) fn note_id(channel: u8, key: u8) -> i32 {
    i32::from(channel & 15) << 7 | i32::from(key & 127)
}

pub(crate) struct Vst3Processor {
    pub(crate) plugin: Arc<Plugin>,
    inputs: Vec<PortBuffer>,
    outputs: Vec<PortBuffer>,
    /// Each bus's channel pointers (into `inputs` and `outputs`).
    _in_channels: Box<[Box<[*mut f32]>]>,
    _out_channels: Box<[Box<[*mut f32]>]>,
    in_buses: Box<[AudioBusBuffers]>,
    out_buses: Box<[AudioBusBuffers]>,
    main_in: Option<usize>,
    main_out: Option<usize>,
    side_in: Option<usize>,
    other_out: Box<[usize]>,
    params_in: ComWrapper<ParamChanges>,
    params_out: ComWrapper<ParamChanges>,
    events_in: ComWrapper<EventList>,
    events_out: ComWrapper<EventList>,
    /// The catalog's parameters (by engine index): ID, and the catalog
    /// value of 1 normalized (the step count of stepped ones).
    catalog: Box<[(ParamID, f64)]>,
    /// The catalog values applied, for the control thread (NaN: none yet).
    applied: Arc<[AtomicU64]>,
    /// Values the plugin's controller set of parameters the project
    /// doesn't keep (hidden ones, bypass): straight to the processor.
    edits: rtrb::Consumer<(ParamID, f64)>,
    /// Values the plugin sent back (meters, ...), for its controller.
    sent: rtrb::Producer<(ParamID, f64)>,
    midi_map: Arc<MidiMap>,
    /// Note IDs of notes the plugin sent, and their channel and key.
    note_ids: [(i32, u8, u8); NOTE_IDS],
    next_note: usize,
    mode: i32,
    sample_rate: f64,
    steady: u64,
}

// SAFETY: the channel pointers point into the processor's own buffers,
// which move with it (heap data); the plugin it holds is called only as
// VST3 allows from the audio thread (see `Plugin`).
unsafe impl Send for Vst3Processor {}

/// What the instance and the processor share, besides the plugin.
pub(crate) struct Links {
    pub(crate) applied: Arc<[AtomicU64]>,
    pub(crate) edits: rtrb::Producer<(ParamID, f64)>,
    pub(crate) sent: rtrb::Consumer<(ParamID, f64)>,
    pub(crate) midi_map: Arc<MidiMap>,
}

impl Vst3Processor {
    /// The processor of `plugin`, whose buses are `ins` and `outs`, with
    /// the catalog's parameters and every one of the controller's.
    pub(crate) fn new(
        plugin: Arc<Plugin>,
        ins: &Ports,
        outs: &Ports,
        catalog: Box<[(ParamID, f64)]>,
        all_params: &[ParamID],
        offline: bool,
        sample_rate: u32,
    ) -> (Self, Links) {
        let mut inputs = ins.buffers();
        let mut outputs = outs.buffers();
        let pointers = |buffers: &mut [PortBuffer]| -> Box<[Box<[*mut f32]>]> {
            buffers
                .iter_mut()
                .map(|b| {
                    (0..b.channels)
                        .map(|c| b.data[c * MAX_BLOCK..].as_mut_ptr())
                        .collect()
                })
                .collect()
        };
        let mut in_channels = pointers(&mut inputs);
        let mut out_channels = pointers(&mut outputs);
        let buses = |channels: &mut [Box<[*mut f32]>]| -> Box<[AudioBusBuffers]> {
            channels
                .iter_mut()
                .map(|c| AudioBusBuffers {
                    numChannels: c.len() as i32,
                    silenceFlags: 0,
                    __field0: AudioBusBuffers__type0 {
                        channelBuffers32: c.as_mut_ptr(),
                    },
                })
                .collect()
        };
        let in_buses = buses(&mut in_channels);
        let out_buses = buses(&mut out_channels);
        let applied: Arc<[AtomicU64]> = catalog
            .iter()
            .map(|_| AtomicU64::new(f64::NAN.to_bits()))
            .collect();
        let (edits_in, edits) = rtrb::RingBuffer::new(1024);
        let (sent, sent_out) = rtrb::RingBuffer::new(1024);
        let midi_map = Arc::new(MidiMap::new());
        let ids = all_params
            .iter()
            .copied()
            .chain(catalog.iter().map(|&(id, _)| id));
        let processor = Self {
            plugin,
            inputs,
            outputs,
            _in_channels: in_channels,
            _out_channels: out_channels,
            in_buses,
            out_buses,
            main_in: ins.main,
            main_out: outs.main,
            side_in: ins.others().next(),
            other_out: outs.others().collect(),
            params_in: ComWrapper::new(ParamChanges::new(ids.clone())),
            params_out: ComWrapper::new(ParamChanges::new(ids)),
            events_in: ComWrapper::new(EventList::new()),
            events_out: ComWrapper::new(EventList::new()),
            catalog,
            applied: Arc::clone(&applied),
            edits,
            sent,
            midi_map: Arc::clone(&midi_map),
            note_ids: [(-1, 0, 0); NOTE_IDS],
            next_note: 0,
            mode: if offline {
                ProcessModes_::kOffline as i32
            } else {
                ProcessModes_::kRealtime as i32
            },
            sample_rate: f64::from(sample_rate),
            steady: 0,
        };
        (
            processor,
            Links {
                applied,
                edits: edits_in,
                sent: sent_out,
                midi_map,
            },
        )
    }

    fn event(offset: u32, kind: u16, body: Event__type0) -> Event {
        Event {
            busIndex: 0,
            sampleOffset: offset as i32,
            ppqPosition: 0.0,
            flags: 0,
            r#type: kind,
            __field0: body,
        }
    }

    fn expression_event(
        offset: u32,
        channel: u8,
        key: u8,
        expression: NoteExpression,
        value: f64,
    ) -> Option<Event> {
        let (type_id, value) = vst3_expression(expression, value)?;
        Some(Self::event(
            offset,
            Event_::EventTypes_::kNoteExpressionValueEvent as u16,
            Event__type0 {
                noteExpressionValue: NoteExpressionValueEvent {
                    typeId: type_id,
                    noteId: note_id(channel, key),
                    value: value.clamp(0.0, 1.0),
                },
            },
        ))
    }

    fn pressure_event(offset: u32, channel: u8, key: u8, pressure: f32) -> Event {
        Self::event(
            offset,
            Event_::EventTypes_::kPolyPressureEvent as u16,
            Event__type0 {
                polyPressure: PolyPressureEvent {
                    channel: i16::from(channel & 15),
                    pitch: i16::from(key & 127),
                    pressure,
                    noteId: note_id(channel, key),
                },
            },
        )
    }

    /// A MIDI 1.0 message the plugin maps to a parameter.
    fn map_midi(&self, data: [u8; 3]) -> Option<(ParamID, f64)> {
        let [status, a, b] = data;
        let channel = status & 15;
        let (control, value) = match status & 0xf0 {
            0xb0 if a < 128 => (usize::from(a), f64::from(b & 127) / 127.0),
            0xd0 => (
                ControllerNumbers_::kAfterTouch as usize,
                f64::from(a & 127) / 127.0,
            ),
            0xe0 => (
                ControllerNumbers_::kPitchBend as usize,
                f64::from(u16::from(b & 127) << 7 | u16::from(a & 127)) / 16383.0,
            ),
            0xc0 => (
                ControllerNumbers_::kCtrlProgramChange as usize,
                f64::from(a & 127),
            ),
            _ => return None,
        };
        let (id, steps) = self.midi_map.get(channel, control)?;
        let value = if control == ControllerNumbers_::kCtrlProgramChange as usize {
            if steps > 0 {
                (value / f64::from(steps)).min(1.0)
            } else {
                value / 127.0
            }
        } else {
            value
        };
        Some((id, value))
    }

    fn push_events(&mut self, events: &[PluginEvent]) {
        let list = &self.events_in;
        for event in events {
            let offset = event.offset;
            match event.kind {
                PluginEventKind::NoteOn {
                    channel,
                    key,
                    velocity,
                } => list.push(Self::event(
                    offset,
                    Event_::EventTypes_::kNoteOnEvent as u16,
                    Event__type0 {
                        noteOn: NoteOnEvent {
                            channel: i16::from(channel & 15),
                            pitch: i16::from(key & 127),
                            tuning: 0.0,
                            velocity,
                            length: 0,
                            noteId: note_id(channel, key),
                        },
                    },
                )),
                PluginEventKind::NoteOff { channel, key } => list.push(Self::event(
                    offset,
                    Event_::EventTypes_::kNoteOffEvent as u16,
                    Event__type0 {
                        noteOff: NoteOffEvent {
                            channel: i16::from(channel & 15),
                            pitch: i16::from(key & 127),
                            velocity: 0.0,
                            noteId: note_id(channel, key),
                            tuning: 0.0,
                        },
                    },
                )),
                PluginEventKind::Midi { data } if data[0] & 0xf0 == 0xa0 => {
                    list.push(Self::pressure_event(
                        offset,
                        data[0] & 15,
                        data[1],
                        f32::from(data[2] & 127) / 127.0,
                    ))
                }
                PluginEventKind::Midi { data } => {
                    if let Some((id, value)) = self.map_midi(data) {
                        self.params_in.add(id, offset, value);
                        // Its controller (and GUI) hear of it too.
                        let _ = self.sent.push((id, value));
                    }
                }
                // A reset sets every expression of the key back; there's
                // nothing to detach (expressions follow the key's notes).
                PluginEventKind::PerNote {
                    channel,
                    key,
                    controller: PerNote::Management { reset, .. },
                    ..
                } => {
                    if reset {
                        for (expression, value) in DEFAULT_EXPRESSIONS {
                            match Self::expression_event(offset, channel, key, expression, value) {
                                Some(event) => list.push(event),
                                None => list.push(Self::pressure_event(offset, channel, key, 0.0)),
                            }
                        }
                    }
                }
                PluginEventKind::PerNote {
                    channel,
                    key,
                    controller,
                    value,
                } => {
                    if let Some(event) = expression::from_per_note(controller, value)
                        .and_then(|(e, v)| Self::expression_event(offset, channel, key, e, v))
                    {
                        list.push(event);
                    }
                }
                PluginEventKind::Param { index, value } => {
                    if let Some(&(id, max)) = self.catalog.get(index as usize) {
                        self.params_in.add(id, offset, value / max);
                        self.applied[index as usize].store(value.to_bits(), Ordering::Relaxed);
                    }
                }
            }
        }
    }

    /// Where the plugin's note `id` is (channel and key).
    fn note_of(&self, id: i32, channel: i16, pitch: i16) -> (u8, u8) {
        // What the event says itself first; then the newest note with the
        // ID (plugins reuse them).
        if channel >= 0 && pitch >= 0 {
            return ((channel & 15) as u8, (pitch & 127) as u8);
        }
        let newest_first = (0..NOTE_IDS).map(|i| (self.next_note + NOTE_IDS - 1 - i) % NOTE_IDS);
        if let Some((_, c, k)) = newest_first
            .map(|i| self.note_ids[i])
            .find(|n| n.0 == id && id >= 0)
        {
            return (c, k);
        }
        // An ID of Dawkey's own.
        (((id >> 7) & 15) as u8, (id & 127) as u8)
    }

    /// What the plugin sent: notes and MIDI to `midi_out`, parameter
    /// values to its controller.
    #[allow(non_upper_case_globals)]
    fn take_output(&mut self, midi_out: &mut MidiSink) {
        let Self {
            events_out,
            params_out,
            sent,
            ..
        } = self;
        for (id, value) in params_out.last_values() {
            // A full ring drops the value (the control thread is busy).
            let _ = sent.push((id, value));
        }
        // In offset order, as the engine takes them (plugins may add them
        // in any order): a stable sort in place, which doesn't allocate.
        let events = events_out.events();
        for i in 1..events.len() {
            let mut j = i;
            while j > 0 && events[j - 1].sampleOffset > events[j].sampleOffset {
                events.swap(j - 1, j);
                j -= 1;
            }
        }
        for i in 0..events_out.events().len() {
            let event = self.events_out.events()[i];
            let offset = event.sampleOffset.max(0) as u32;
            // SAFETY: the union holds the variant `type` says.
            let kind = unsafe {
                match u32::from(event.r#type) {
                    Event_::EventTypes_::kNoteOnEvent => {
                        let e = event.__field0.noteOn;
                        let (channel, key) = ((e.channel & 15) as u8, (e.pitch & 127) as u8);
                        if e.noteId >= 0 {
                            self.note_ids[self.next_note] = (e.noteId, channel, key);
                            self.next_note = (self.next_note + 1) % NOTE_IDS;
                        }
                        PluginEventKind::NoteOn {
                            channel,
                            key,
                            velocity: e.velocity.clamp(0.0, 1.0),
                        }
                    }
                    Event_::EventTypes_::kNoteOffEvent => {
                        let e = event.__field0.noteOff;
                        PluginEventKind::NoteOff {
                            channel: (e.channel & 15) as u8,
                            key: (e.pitch & 127) as u8,
                        }
                    }
                    Event_::EventTypes_::kPolyPressureEvent => {
                        let e = event.__field0.polyPressure;
                        let (channel, key) = self.note_of(e.noteId, e.channel, e.pitch);
                        PluginEventKind::Midi {
                            data: [
                                0xa0 | channel,
                                key,
                                (e.pressure.clamp(0.0, 1.0) * 127.0).round() as u8,
                            ],
                        }
                    }
                    Event_::EventTypes_::kNoteExpressionValueEvent => {
                        let e = event.__field0.noteExpressionValue;
                        let Some((expression, value)) = from_vst3_expression(e.typeId, e.value)
                        else {
                            continue;
                        };
                        let (channel, key) = self.note_of(e.noteId, -1, -1);
                        expression::to_per_note(expression, channel, key, value)
                    }
                    Event_::EventTypes_::kLegacyMIDICCOutEvent => {
                        let e = event.__field0.midiCCOut;
                        let channel = (e.channel as u8) & 15;
                        let (v, v2) = ((e.value as u8) & 127, (e.value2 as u8) & 127);
                        let data = match u32::from(e.controlNumber) {
                            n if n < 128 => [0xb0 | channel, n as u8, v],
                            ControllerNumbers_::kAfterTouch => [0xd0 | channel, v, 0],
                            ControllerNumbers_::kPitchBend => [0xe0 | channel, v, v2],
                            ControllerNumbers_::kCtrlProgramChange => [0xc0 | channel, v, 0],
                            ControllerNumbers_::kCtrlPolyPressure => [0xa0 | channel, v, v2],
                            _ => continue,
                        };
                        PluginEventKind::Midi { data }
                    }
                    _ => continue,
                }
            };
            midi_out.push(offset, kind);
        }
    }
}

/// The transport as VST3 has it.
fn process_context(t: &dawkey_engine::Transport, sample_rate: f64, steady: u64) -> ProcessContext {
    use ProcessContext_::StatesAndFlags_::*;
    // SAFETY: a plain C struct; all zeros is a valid (empty) context.
    let mut context: ProcessContext = unsafe { std::mem::zeroed() };
    let mut state =
        kTempoValid | kTimeSigValid | kProjectTimeMusicValid | kBarPositionValid | kContTimeValid;
    if t.playing {
        state |= kPlaying;
    }
    if t.recording {
        state |= kRecording;
    }
    if let Some((start, end)) = t.loop_beats {
        state |= kCycleValid | kCycleActive;
        context.cycleStartMusic = start;
        context.cycleEndMusic = end;
    }
    context.state = state;
    context.sampleRate = sample_rate;
    context.projectTimeSamples = t.frame as i64;
    context.continousTimeSamples = steady as i64;
    context.projectTimeMusic = t.beats;
    context.barPositionMusic = t.bar_start_beats;
    context.tempo = t.bpm;
    context.timeSigNumerator = i32::from(t.numerator);
    context.timeSigDenominator = i32::from(t.denominator);
    context
}

impl PluginProcessor for Vst3Processor {
    fn process(
        &mut self,
        audio: [&mut [f32]; 2],
        context: dawkey_engine::ProcessContext,
        events: &[PluginEvent],
        mut midi_out: MidiSink,
    ) {
        let frames = audio[0].len().min(MAX_BLOCK);
        let [left, right] = audio;
        let dawkey_engine::ProcessContext {
            transport,
            sidechain,
            aux,
        } = context;
        ports::fill_inputs(
            &mut self.inputs,
            self.main_in,
            self.side_in,
            [&*left, &*right],
            sidechain,
            frames,
        );
        self.params_in.clear();
        self.params_out.clear();
        self.events_in.events().clear();
        self.events_out.events().clear();
        while let Ok((id, value)) = self.edits.pop() {
            self.params_in.add(id, 0, value);
        }
        self.push_events(events);
        for bus in self.out_buses.iter_mut() {
            bus.silenceFlags = 0;
        }
        let mut process_context = process_context(&transport, self.sample_rate, self.steady);
        let mut data = ProcessData {
            processMode: self.mode,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            numSamples: frames as i32,
            numInputs: self.in_buses.len() as i32,
            numOutputs: self.out_buses.len() as i32,
            inputs: self.in_buses.as_mut_ptr(),
            outputs: self.out_buses.as_mut_ptr(),
            inputParameterChanges: self
                .params_in
                .as_com_ref::<IParameterChanges>()
                .map_or(std::ptr::null_mut(), |p| p.as_ptr()),
            outputParameterChanges: self
                .params_out
                .as_com_ref::<IParameterChanges>()
                .map_or(std::ptr::null_mut(), |p| p.as_ptr()),
            inputEvents: self
                .events_in
                .as_com_ref::<IEventList>()
                .map_or(std::ptr::null_mut(), |e| e.as_ptr()),
            outputEvents: self
                .events_out
                .as_com_ref::<IEventList>()
                .map_or(std::ptr::null_mut(), |e| e.as_ptr()),
            processContext: &mut process_context,
        };
        let processor = &self.plugin.processor;
        // SAFETY: every pointer in `data` is live for the call, with room
        // for `frames`.
        let result = assert_no_alloc::permit_alloc(|| unsafe { processor.process(&mut data) });
        self.steady += frames as u64;
        self.take_output(&mut midi_out);
        if result != kResultOk {
            // Can't log here (that allocates); the track goes silent.
            ports::silence([left, right], aux, frames);
            return;
        }
        ports::copy_outputs(
            &self.outputs,
            self.main_out,
            &self.other_out,
            [left, right],
            aux,
            frames,
        );
    }

    fn stop(&mut self) {
        self.plugin.stop_processing();
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}
