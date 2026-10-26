// SPDX-License-Identifier: AGPL-3.0-or-later
//! The CLAP test plugins.

use std::ffi::CStr;
use std::sync::atomic::{AtomicU64, Ordering};

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::latency::{PluginLatency, PluginLatencyImpl};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
    PluginNotePortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::{HostState, PluginState, PluginStateImpl};
use clack_plugin::entry::prelude::*;
use clack_plugin::events::event_types::{
    MidiEvent, NoteExpressionEvent, NoteExpressionType, NoteOffEvent, NoteOnEvent,
    ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent,
};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};

use crate::DELAY_FRAMES;

/// The gain's text for this value takes 3 s (see the crate docs).
pub const STALL_DB: f64 = -59.5;

const GAIN_ID: &CStr = c"dawkey.test.gain";
const DELAY_ID: &CStr = c"dawkey.test.delay";
const BEEP_ID: &CStr = c"dawkey.test.beep";
const OCTAVE_ID: &CStr = c"dawkey.test.octave";
const MEMO_ID: &CStr = c"dawkey.test.memo";
const TEMPO_ID: &CStr = c"dawkey.test.tempo";
const SUBTRACT_ID: &CStr = c"dawkey.test.subtract";
const SPLIT_ID: &CStr = c"dawkey.test.split";

/// The one parameter each parameterized test plugin has.
const PARAM: ClapId = ClapId::new(1);

/// An `f64` parameter value shared between threads.
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

/// What the shared state of every test plugin holds: one parameter, and
/// the memo's memory.
pub struct Shared {
    value: Value,
    spec: &'static ParamSpec,
    memo: Value,
}

impl PluginShared<'_> for Shared {}

struct ParamSpec {
    name: &'static [u8],
    min: f64,
    max: f64,
    default: f64,
}

const GAIN_PARAM: ParamSpec = ParamSpec {
    name: b"Gain",
    min: -60.0,
    max: 12.0,
    default: 0.0,
};

const LEVEL_PARAM: ParamSpec = ParamSpec {
    name: b"Level",
    min: 0.0,
    max: 1.0,
    default: 1.0,
};

impl Shared {
    fn new(spec: &'static ParamSpec) -> Self {
        Self {
            value: Value::new(spec.default),
            spec,
            memo: Value::new(0.0),
        }
    }

    fn handle(&self, event: &UnknownEvent) {
        if let Some(CoreEventSpace::ParamValue(e)) = event.as_core_event()
            && e.param_id() == PARAM
        {
            self.value
                .set(e.value().clamp(self.spec.min, self.spec.max));
        }
    }
}

pub struct MainThread<'a> {
    shared: &'a Shared,
    host: HostMainThreadHandle<'a>,
    /// Instruments have a note input and no audio input.
    instrument: bool,
    /// Has a parameter.
    params: bool,
    /// A MIDI effect: notes in and out, no audio.
    midi_fx: bool,
    /// The memo: notes in (so `instrument`), and audio in and out.
    memo: bool,
    /// A second, stereo input: a sidechain.
    sidechain: bool,
    /// A second, stereo output, "High".
    split: bool,
}

impl<'a> PluginMainThread<'a, Shared> for MainThread<'a> {
    /// The memo asks for this when its memory changes.
    fn on_main_thread(&self) {
        if let Some(state) = self.host.get_extension::<HostState>() {
            state.mark_dirty(&self.host);
        }
    }
}

impl PluginStateImpl for MainThread<'_> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        use std::io::Write;
        output
            .write_all(&self.shared.memo.get().to_le_bytes())
            .map_err(|_| PluginError::Message("writing the state failed"))
    }

    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        use std::io::Read;
        let mut bytes = [0; 8];
        input
            .read_exact(&mut bytes)
            .map_err(|_| PluginError::Message("reading the state failed"))?;
        self.shared.memo.set(f64::from_le_bytes(bytes));
        Ok(())
    }
}

impl PluginAudioPortsImpl for MainThread<'_> {
    fn count(&self, is_input: bool) -> u32 {
        let main = u32::from(!(self.midi_fx || (is_input && self.instrument && !self.memo)));
        let other = if is_input { self.sidechain } else { self.split };
        main + u32::from(other)
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        let (name, flags): (&[u8], _) = match (index, is_input) {
            (0, _) => (b"main", AudioPortFlags::IS_MAIN),
            (1, true) if self.sidechain => (b"Sidechain", AudioPortFlags::empty()),
            (1, false) if self.split => (b"High", AudioPortFlags::empty()),
            _ => return,
        };
        writer.set(&AudioPortInfo {
            id: ClapId::new(index),
            name,
            channel_count: 2,
            flags,
            port_type: Some(AudioPortType::STEREO),
            in_place_pair: None,
        });
    }
}

impl PluginNotePortsImpl for MainThread<'_> {
    fn count(&self, is_input: bool) -> u32 {
        u32::from((is_input && self.instrument) || self.midi_fx)
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if (is_input || self.midi_fx) && index == 0 {
            writer.set(&NotePortInfo {
                id: ClapId::new(0),
                name: b"notes",
                preferred_dialect: Some(NoteDialect::Clap),
                supported_dialects: NoteDialects::CLAP | NoteDialects::MIDI,
            });
        }
    }
}

impl PluginMainThreadParams for MainThread<'_> {
    fn count(&self) -> u32 {
        u32::from(self.params)
    }

    fn get_info(&self, index: u32, info: &mut ParamInfoWriter) {
        if index != 0 || !self.params {
            return;
        }
        let spec = self.shared.spec;
        info.set(&ParamInfo {
            id: PARAM,
            flags: ParamInfoFlags::IS_AUTOMATABLE,
            cookie: Default::default(),
            name: spec.name,
            module: b"",
            min_value: spec.min,
            max_value: spec.max,
            default_value: spec.default,
        });
    }

    fn get_value(&self, id: ClapId) -> Option<f64> {
        (self.params && id == PARAM).then(|| self.shared.value.get())
    }

    fn value_to_text(
        &self,
        id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> std::fmt::Result {
        use std::fmt::Write;
        if id != PARAM {
            return Err(std::fmt::Error);
        }
        // A stand-in for a GUI's dialog, which holds up the main thread
        // until it's answered.
        if value == STALL_DB && !self.instrument {
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
        if self.instrument {
            write!(writer, "{:.0} %", value * 100.0)
        } else {
            write!(writer, "{value:.1} dB")
        }
    }

    fn text_to_value(&self, id: ClapId, text: &CStr) -> Option<f64> {
        if id != PARAM {
            return None;
        }
        let text = text.to_str().ok()?.trim();
        if self.instrument {
            let number: f64 = text.trim_end_matches('%').trim_end().parse().ok()?;
            Some(number / 100.0)
        } else {
            let lower = text.to_ascii_lowercase();
            let number = lower.strip_suffix("db").unwrap_or(&lower).trim_end();
            number.parse().ok()
        }
    }

    fn flush(&self, input: &InputEvents, _output: &mut OutputEvents) {
        for event in input {
            self.shared.handle(event);
        }
    }
}

impl PluginLatencyImpl for MainThread<'_> {
    fn get(&self) -> u32 {
        DELAY_FRAMES
    }
}

/// Stereo gain.
pub struct Gain;

impl Plugin for Gain {
    type AudioProcessor<'a> = GainProcessor<'a>;
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>();
    }
}

pub struct GainProcessor<'a> {
    shared: &'a Shared,
}

impl<'a> PluginAudioProcessor<'a, Shared, MainThread<'a>> for GainProcessor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &MainThread<'a>,
        shared: &'a Shared,
        _config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self { shared })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let mut port = audio
            .port_pair(0)
            .ok_or(PluginError::Message("no audio port"))?;
        let mut channels = port
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("expected f32 audio"))?;
        for batch in events.input.batch() {
            for event in batch.events() {
                self.shared.handle(event);
            }
            let gain = 10f64.powf(self.shared.value.get() / 20.0) as f32;
            let range = batch.sample_bounds();
            for pair in channels.iter_mut() {
                match pair {
                    ChannelPair::InPlace(buf) => buf[range].iter_mut().for_each(|s| *s *= gain),
                    ChannelPair::InputOutput(i, o) => {
                        for (o, i) in o[range].iter_mut().zip(&i[range]) {
                            *o = i * gain;
                        }
                    }
                    ChannelPair::OutputOnly(o) => o[range].fill(0.0),
                    ChannelPair::InputOnly(_) => {}
                }
            }
        }
        Ok(ProcessStatus::ContinueIfNotQuiet)
    }
}

impl PluginAudioProcessorParams for GainProcessor<'_> {
    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        for event in input {
            self.shared.handle(event);
        }
    }
}

/// A pure delay of [`DELAY_FRAMES`], reported as latency.
pub struct Delay;

impl Plugin for Delay {
    type AudioProcessor<'a> = DelayProcessor;
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginLatency>();
    }
}

pub struct DelayProcessor {
    /// One ring per channel.
    rings: [[f32; DELAY_FRAMES as usize]; 2],
    at: usize,
}

impl<'a> PluginAudioProcessor<'a, Shared, MainThread<'a>> for DelayProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &MainThread<'a>,
        _shared: &'a Shared,
        _config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self {
            rings: [[0.0; DELAY_FRAMES as usize]; 2],
            at: 0,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let mut port = audio
            .port_pair(0)
            .ok_or(PluginError::Message("no audio port"))?;
        let mut channels = port
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("expected f32 audio"))?;
        let start = self.at;
        let mut end = start;
        for (c, pair) in channels.iter_mut().enumerate().take(2) {
            let ring = &mut self.rings[c];
            let mut at = start;
            let mut step = |x: f32| {
                let y = ring[at];
                ring[at] = x;
                at = (at + 1) % ring.len();
                y
            };
            match pair {
                ChannelPair::InPlace(buf) => buf.iter_mut().for_each(|s| *s = step(*s)),
                ChannelPair::InputOutput(i, o) => {
                    for (o, i) in o.iter_mut().zip(i.iter()) {
                        *o = step(*i);
                    }
                }
                ChannelPair::OutputOnly(o) => o.iter_mut().for_each(|s| *s = step(0.0)),
                ChannelPair::InputOnly(i) => i.iter().for_each(|s| {
                    step(*s);
                }),
            }
            end = at;
        }
        self.at = end;
        Ok(ProcessStatus::Continue)
    }
}

/// An instrument that outputs DC: the sum of `velocity × volume × level`
/// over the held keys, where `volume` is the key's volume note expression
/// (1 until one arrives).
pub struct Beep;

impl Plugin for Beep {
    type AudioProcessor<'a> = BeepProcessor<'a>;
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginParams>();
    }
}

pub struct BeepProcessor<'a> {
    shared: &'a Shared,
    /// Velocity of each held key, 0 if up.
    held: [f64; 128],
    /// Each key's volume note expression.
    volume: [f64; 128],
}

impl<'a> PluginAudioProcessor<'a, Shared, MainThread<'a>> for BeepProcessor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &MainThread<'a>,
        shared: &'a Shared,
        _config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self {
            shared,
            held: [0.0; 128],
            volume: [1.0; 128],
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let mut port = audio
            .output_port(0)
            .ok_or(PluginError::Message("no output port"))?;
        let mut channels = port
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("expected f32 audio"))?;
        for batch in events.input.batch() {
            for event in batch.events() {
                self.shared.handle(event);
                match event.as_core_event() {
                    // Key 127 turns its Level to half itself, as a knob on
                    // its GUI would: a gesture, and the new value.
                    Some(CoreEventSpace::NoteOn(e)) if e.key().into_specific() == Some(127) => {
                        let time = e.header().time();
                        self.shared.value.set(0.5);
                        let _ = events
                            .output
                            .try_push(ParamGestureBeginEvent::new(time, PARAM));
                        let _ = events.output.try_push(ParamValueEvent::new(
                            time,
                            PARAM,
                            Pckn::match_all(),
                            0.5,
                        ));
                        let _ = events
                            .output
                            .try_push(ParamGestureEndEvent::new(time, PARAM));
                    }
                    Some(CoreEventSpace::NoteOn(e)) => {
                        if let Some(key) = e.key().into_specific() {
                            self.held[usize::from(key as u8 & 127)] = e.velocity();
                        }
                    }
                    Some(CoreEventSpace::NoteOff(e)) => {
                        if let Some(key) = e.key().into_specific() {
                            self.held[usize::from(key as u8 & 127)] = 0.0;
                        }
                    }
                    Some(CoreEventSpace::NoteExpression(e))
                        if e.expression_type() == Some(NoteExpressionType::Volume) =>
                    {
                        if let Some(key) = e.key().into_specific() {
                            self.volume[usize::from(key as u8 & 127)] = e.value();
                        }
                    }
                    _ => {}
                }
            }
            let held = self.held.iter().zip(&self.volume).map(|(v, g)| v * g);
            let level = (held.sum::<f64>() * self.shared.value.get()) as f32;
            let range = batch.sample_bounds();
            for channel in channels.iter_mut() {
                channel[range].fill(level);
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

impl PluginAudioProcessorParams for BeepProcessor<'_> {
    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        for event in input {
            self.shared.handle(event);
        }
    }
}

/// The note expression with this raw CLAP ID. (clack 0.2's own
/// `NoteExpressionType::from_raw` leaves out "expression".)
fn expression_from_raw(raw: i32) -> Option<NoteExpressionType> {
    use NoteExpressionType::*;
    [
        Volume, Pan, Tuning, Vibrato, Expression, Brightness, Pressure,
    ]
    .into_iter()
    .find(|t| t.into_raw() == raw)
}

/// A MIDI effect: sends every note it gets an octave up (keys above 115
/// stay), on the same channel, with its note expressions, and nothing
/// else.
pub struct Octave;

impl Plugin for Octave {
    type AudioProcessor<'a> = OctaveProcessor;
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>();
    }
}

pub struct OctaveProcessor;

impl<'a> PluginAudioProcessor<'a, Shared, MainThread<'a>> for OctaveProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &MainThread<'a>,
        _shared: &'a Shared,
        _config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn process(
        &mut self,
        _process: Process,
        _audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let up = |pckn: Pckn| {
            let key = pckn.raw_key();
            let key = if (0..=115).contains(&key) {
                key + 12
            } else {
                key
            };
            Pckn::from_raw(0, pckn.raw_channel(), key, -1)
        };
        for event in events.input {
            let time = event.header().time();
            let _ = match event.as_core_event() {
                Some(CoreEventSpace::NoteOn(e)) => {
                    events
                        .output
                        .try_push(NoteOnEvent::new(time, up(e.pckn()), e.velocity()))
                }
                Some(CoreEventSpace::NoteOff(e)) => {
                    events
                        .output
                        .try_push(NoteOffEvent::new(time, up(e.pckn()), e.velocity()))
                }
                Some(CoreEventSpace::NoteExpression(e)) => {
                    match expression_from_raw(e.as_raw().expression_id) {
                        Some(expression) => events.output.try_push(NoteExpressionEvent::new(
                            time,
                            up(e.pckn()),
                            expression,
                            e.value(),
                        )),
                        None => Ok(()),
                    }
                }
                Some(CoreEventSpace::Midi(e)) => {
                    let [status, key, velocity] = e.data();
                    let key = if matches!(status & 0xf0, 0x80 | 0x90) && key <= 115 {
                        key + 12
                    } else {
                        key
                    };
                    events
                        .output
                        .try_push(MidiEvent::new(time, 0, [status, key, velocity]))
                }
                _ => Ok(()),
            };
        }
        Ok(ProcessStatus::Continue)
    }
}

/// An effect that remembers the velocity of the last note it got, only in
/// its state (no parameter has it), and adds it to its audio as DC.
pub struct Memo;

impl Plugin for Memo {
    type AudioProcessor<'a> = MemoProcessor<'a>;
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginState>();
    }
}

pub struct MemoProcessor<'a> {
    shared: &'a Shared,
    host: HostAudioProcessorHandle<'a>,
}

impl<'a> PluginAudioProcessor<'a, Shared, MainThread<'a>> for MemoProcessor<'a> {
    fn activate(
        host: HostAudioProcessorHandle<'a>,
        _main_thread: &MainThread<'a>,
        shared: &'a Shared,
        _config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self { shared, host })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let mut port = audio
            .port_pair(0)
            .ok_or(PluginError::Message("no audio port"))?;
        let mut channels = port
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("expected f32 audio"))?;
        for batch in events.input.batch() {
            for event in batch.events() {
                if let Some(CoreEventSpace::NoteOn(e)) = event.as_core_event() {
                    self.shared.memo.set(e.velocity());
                    // Mark the state dirty, on the main thread.
                    self.host.request_callback();
                }
            }
            let memo = self.shared.memo.get() as f32;
            let range = batch.sample_bounds();
            for pair in channels.iter_mut() {
                match pair {
                    ChannelPair::InPlace(buf) => buf[range].iter_mut().for_each(|s| *s += memo),
                    ChannelPair::InputOutput(i, o) => {
                        for (o, i) in o[range].iter_mut().zip(&i[range]) {
                            *o = i + memo;
                        }
                    }
                    ChannelPair::OutputOnly(o) => o[range].fill(memo),
                    ChannelPair::InputOnly(_) => {}
                }
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

/// Shows the transport as audio: the left channel is the tempo / 1000, the
/// right one (bar number + 1) / 100 while playing, 0 while stopped. Its
/// input is ignored.
pub struct Tempo;

impl Plugin for Tempo {
    type AudioProcessor<'a> = TempoProcessor;
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder.register::<PluginAudioPorts>();
    }
}

pub struct TempoProcessor;

impl<'a> PluginAudioProcessor<'a, Shared, MainThread<'a>> for TempoProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &MainThread<'a>,
        _shared: &'a Shared,
        _config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn process(
        &mut self,
        process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        use clack_plugin::events::event_types::TransportFlags;
        let (tempo, bar) = match process.transport {
            Some(t) => (
                if t.flags.contains(TransportFlags::HAS_TEMPO) {
                    t.tempo / 1000.0
                } else {
                    0.0
                },
                if t.flags.contains(TransportFlags::IS_PLAYING) {
                    f64::from(t.bar_number + 1) / 100.0
                } else {
                    0.0
                },
            ),
            None => (0.0, 0.0),
        };
        let mut port = audio
            .port_pair(0)
            .ok_or(PluginError::Message("no audio port"))?;
        let mut channels = port
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("expected f32 audio"))?;
        for (c, pair) in channels.iter_mut().enumerate() {
            let value = if c == 0 { tempo } else { bar } as f32;
            match pair {
                ChannelPair::InPlace(buf) => buf.fill(value),
                ChannelPair::InputOutput(_, o) | ChannelPair::OutputOnly(o) => o.fill(value),
                ChannelPair::InputOnly(_) => {}
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

/// Subtracts its sidechain input from its main input.
pub struct Subtract;

impl Plugin for Subtract {
    type AudioProcessor<'a> = SubtractProcessor;
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder.register::<PluginAudioPorts>();
    }
}

pub struct SubtractProcessor {
    /// The sidechain, copied out (two channels).
    side: [Vec<f32>; 2],
}

impl<'a> PluginAudioProcessor<'a, Shared, MainThread<'a>> for SubtractProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &MainThread<'a>,
        _shared: &'a Shared,
        config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let len = config.max_frames_count as usize;
        Ok(Self {
            side: [vec![0.0; len], vec![0.0; len]],
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let frames = audio.frames_count() as usize;
        for side in &mut self.side {
            side[..frames].fill(0.0);
        }
        if let Some(channels) = audio
            .input_port(1)
            .and_then(|p| p.channels().ok())
            .and_then(|c| c.into_f32())
        {
            for (side, data) in self.side.iter_mut().zip(channels.iter()) {
                side[..frames].copy_from_slice(&data[..frames]);
            }
        }
        let mut port = audio
            .port_pair(0)
            .ok_or(PluginError::Message("no audio port"))?;
        let mut channels = port
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("expected f32 audio"))?;
        for (pair, side) in channels.iter_mut().zip(&self.side) {
            match pair {
                ChannelPair::InPlace(buf) => {
                    for (s, x) in buf.iter_mut().zip(side) {
                        *s -= x;
                    }
                }
                ChannelPair::InputOutput(i, o) => {
                    for ((o, i), x) in o.iter_mut().zip(i.iter()).zip(side) {
                        *o = i - x;
                    }
                }
                ChannelPair::OutputOnly(o) => o.fill(0.0),
                ChannelPair::InputOnly(_) => {}
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

/// An instrument with two outputs: the main one outputs the sum of the
/// held keys' velocities below middle C (60) as DC, "High" those from 60
/// up.
pub struct Split;

impl Plugin for Split {
    type AudioProcessor<'a> = SplitProcessor;
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>();
    }
}

pub struct SplitProcessor {
    held: [f64; 128],
}

impl<'a> PluginAudioProcessor<'a, Shared, MainThread<'a>> for SplitProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &MainThread<'a>,
        _shared: &'a Shared,
        _config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self { held: [0.0; 128] })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        for event in events.input {
            match event.as_core_event() {
                Some(CoreEventSpace::NoteOn(e)) => {
                    if let Some(key) = e.key().into_specific() {
                        self.held[usize::from(key as u8 & 127)] = e.velocity();
                    }
                }
                Some(CoreEventSpace::NoteOff(e)) => {
                    if let Some(key) = e.key().into_specific() {
                        self.held[usize::from(key as u8 & 127)] = 0.0;
                    }
                }
                _ => {}
            }
        }
        let low = self.held[..60].iter().sum::<f64>() as f32;
        let high = self.held[60..].iter().sum::<f64>() as f32;
        for (index, level) in [(0, low), (1, high)] {
            let Some(mut port) = audio.output_port(index) else {
                continue;
            };
            let mut channels = port
                .channels()?
                .into_f32()
                .ok_or(PluginError::Message("expected f32 audio"))?;
            for channel in channels.iter_mut() {
                channel.fill(level);
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

pub struct Entry {
    factory: PluginFactoryWrapper<Factory>,
}

impl clack_plugin::entry::Entry for Entry {
    fn new(_bundle_path: Option<&CStr>) -> Result<Self, EntryLoadError> {
        use clack_plugin::plugin::features::*;
        // For testing crash isolation: a plugin that takes its host down.
        if std::env::var_os("dawkey_TEST_PLUGIN_CRASH").is_some() {
            std::process::abort();
        }
        crate::chatter();
        Ok(Self {
            factory: PluginFactoryWrapper::new(Factory {
                descriptors: [
                    PluginDescriptor::new("dawkey.test.gain", "Test Gain")
                        .with_vendor("Dawkey")
                        .with_features([AUDIO_EFFECT, STEREO]),
                    PluginDescriptor::new("dawkey.test.delay", "Test Delay")
                        .with_vendor("Dawkey")
                        .with_features([AUDIO_EFFECT, STEREO]),
                    PluginDescriptor::new("dawkey.test.beep", "Test Beep")
                        .with_vendor("Dawkey")
                        .with_features([INSTRUMENT, STEREO]),
                    PluginDescriptor::new("dawkey.test.octave", "Test Octave")
                        .with_vendor("Dawkey")
                        .with_features([NOTE_EFFECT]),
                    PluginDescriptor::new("dawkey.test.memo", "Test Memo")
                        .with_vendor("Dawkey")
                        .with_features([AUDIO_EFFECT, STEREO]),
                    PluginDescriptor::new("dawkey.test.tempo", "Test Tempo")
                        .with_vendor("Dawkey")
                        .with_features([AUDIO_EFFECT, STEREO]),
                    PluginDescriptor::new("dawkey.test.subtract", "Test Subtract")
                        .with_vendor("Dawkey")
                        .with_features([AUDIO_EFFECT, STEREO]),
                    PluginDescriptor::new("dawkey.test.split", "Test Split")
                        .with_vendor("Dawkey")
                        .with_features([INSTRUMENT, STEREO]),
                ],
            }),
        })
    }

    fn declare_factories<'a>(&'a self, builder: &mut EntryFactories<'a>) {
        builder.register_factory(&self.factory);
    }
}

pub struct Factory {
    descriptors: [PluginDescriptor; 8],
}

impl PluginFactoryImpl for Factory {
    fn plugin_count(&self) -> u32 {
        self.descriptors.len() as u32
    }

    fn plugin_descriptor(&self, index: u32) -> Option<&PluginDescriptor> {
        self.descriptors.get(index as usize)
    }

    fn create_plugin<'a>(
        &'a self,
        host_info: HostInfo<'a>,
        plugin_id: &CStr,
    ) -> Option<PluginInstance<'a>> {
        let main = |instrument, params| {
            move |host: HostMainThreadHandle<'a>, shared: &'a Shared| {
                Ok(MainThread {
                    shared,
                    host,
                    instrument,
                    params,
                    midi_fx: false,
                    memo: false,
                    sidechain: false,
                    split: false,
                })
            }
        };
        let [gain, delay, beep, octave, memo, tempo, subtract, split] = &self.descriptors;
        Some(if plugin_id == GAIN_ID {
            PluginInstance::new::<Gain>(
                host_info,
                gain,
                |_| Ok(Shared::new(&GAIN_PARAM)),
                main(false, true),
            )
        } else if plugin_id == DELAY_ID {
            PluginInstance::new::<Delay>(
                host_info,
                delay,
                |_| Ok(Shared::new(&GAIN_PARAM)),
                main(false, false),
            )
        } else if plugin_id == BEEP_ID {
            PluginInstance::new::<Beep>(
                host_info,
                beep,
                |_| Ok(Shared::new(&LEVEL_PARAM)),
                main(true, true),
            )
        } else if plugin_id == OCTAVE_ID {
            PluginInstance::new::<Octave>(
                host_info,
                octave,
                |_| Ok(Shared::new(&GAIN_PARAM)),
                |host, shared| {
                    Ok(MainThread {
                        shared,
                        host,
                        instrument: true,
                        params: false,
                        midi_fx: true,
                        memo: false,
                        sidechain: false,
                        split: false,
                    })
                },
            )
        } else if plugin_id == SUBTRACT_ID {
            PluginInstance::new::<Subtract>(
                host_info,
                subtract,
                |_| Ok(Shared::new(&GAIN_PARAM)),
                |host, shared| {
                    Ok(MainThread {
                        shared,
                        host,
                        instrument: false,
                        params: false,
                        midi_fx: false,
                        memo: false,
                        sidechain: true,
                        split: false,
                    })
                },
            )
        } else if plugin_id == SPLIT_ID {
            PluginInstance::new::<Split>(
                host_info,
                split,
                |_| Ok(Shared::new(&GAIN_PARAM)),
                |host, shared| {
                    Ok(MainThread {
                        shared,
                        host,
                        instrument: true,
                        params: false,
                        midi_fx: false,
                        memo: false,
                        sidechain: false,
                        split: true,
                    })
                },
            )
        } else if plugin_id == TEMPO_ID {
            PluginInstance::new::<Tempo>(
                host_info,
                tempo,
                |_| Ok(Shared::new(&GAIN_PARAM)),
                main(false, false),
            )
        } else if plugin_id == MEMO_ID {
            PluginInstance::new::<Memo>(
                host_info,
                memo,
                |_| Ok(Shared::new(&GAIN_PARAM)),
                |host, shared| {
                    Ok(MainThread {
                        shared,
                        host,
                        instrument: true,
                        params: false,
                        midi_fx: false,
                        memo: true,
                        sidechain: false,
                        split: false,
                    })
                },
            )
        } else {
            return None;
        })
    }
}

clack_export_entry!(Entry);
