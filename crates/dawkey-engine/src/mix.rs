// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rendering one track for one block, on the real-time thread: sources,
//! instrument, insert plugins, delay compensation, fader. Also moves the
//! state that outlives a graph (voices, processors, delay lines) to the
//! next one.

use dawkey_dsp::PolySynth;

use crate::graph::{
    CHAIN, ControlEvent, DelayLine, External, Fader, Graph, Instrument, LIVE, NoteEvent,
    PluginNode, TrackNode, Voices, lane_value,
};
use crate::midi::{InputFilter, Message as Midi, MidiEvent, MidiOut, PerNote};
use crate::notes::{NoteTracker, push_event};
use crate::plugin::{MidiSink, PluginEvent, PluginEventKind, ProcessContext, Transport};

/// Fader automation is evaluated this often (frames), and ramped between.
const FADER_STEP: usize = 32;
/// Parameter automation is sent this often (frames).
const PARAM_STEP: usize = 64;

/// Live MIDI events for one block: offsets are relative to the process
/// buffer, and `base` is where the block starts in it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Live<'a> {
    pub(crate) events: &'a [MidiEvent],
    pub(crate) base: u32,
}

/// MIDI going out to devices this process call. Never grows past its
/// capacity; `base` is where the current block starts in the buffer, and
/// `clock` where it starts on the engine's frame clock.
#[derive(Debug)]
pub(crate) struct MidiOutput<'a> {
    pub(crate) events: &'a mut Vec<MidiOut>,
    pub(crate) base: u32,
    pub(crate) clock: u64,
}

impl MidiOutput<'_> {
    /// Sends a device's events for this block (at block offsets) on its
    /// channel, holding them back by its delay.
    fn flush(&mut self, external: &mut External) {
        let channel = external.channel;
        for event in external.events.drain(..) {
            let (data, value) = match event.kind {
                PluginEventKind::NoteOn { key, velocity, .. } => (
                    [
                        0x90 | channel,
                        key,
                        (velocity * 127.0).round().clamp(1.0, 127.0) as u8,
                    ],
                    0,
                ),
                PluginEventKind::NoteOff { key, .. } => ([0x80 | channel, key, 0], 0),
                PluginEventKind::Midi { data } => ([data[0] & 0xf0 | channel, data[1], data[2]], 0),
                PluginEventKind::PerNote {
                    key,
                    controller,
                    value,
                    ..
                } => (controller.data(channel, key), value),
                PluginEventKind::Param { .. } => continue,
            };
            // Behind held-back messages, so the order stays.
            if external.delay == 0 && external.pending.is_empty() {
                let offset = self.base + event.offset;
                push_out(self.events, offset, external.device, data, value);
            } else if external.pending.len() < external.pending.capacity() {
                let due = self.clock + u64::from(event.offset) + u64::from(external.delay);
                external.pending.push((due, data, value));
            }
        }
    }

    /// Sends a device's held-back messages that are due in the `frames`
    /// frames of this block, or all of them now with `None`.
    fn send_due(&mut self, external: &mut External, frames: Option<u32>) {
        let Self {
            events,
            base,
            clock,
        } = self;
        let device = external.device;
        external.pending.retain(|&(due, data, value)| {
            let offset = due.saturating_sub(*clock);
            let offset = match frames {
                Some(frames) if offset >= u64::from(frames) => return true,
                Some(_) => offset as u32,
                None => 0,
            };
            push_out(events, *base + offset, device, data, value);
            false
        });
    }
}

fn push_out(events: &mut Vec<MidiOut>, offset: u32, dest: u32, data: [u8; 3], value: u32) {
    if events.len() < events.capacity() {
        events.push(MidiOut {
            offset,
            dest,
            data,
            value,
        });
    }
}

/// Renders one block of `track` into `lanes` (which it overwrites).
/// `position` is the block's timeline position; while stopped it doesn't
/// move, clips are silent, and instruments play only live input. With
/// `chase` (playback just started or jumped), instruments first start the
/// notes already sounding at `position` and catch up on controllers. A
/// monitoring track plays `input` (as long as the lanes), playing or not.
/// A bus starts from `bus_in`, what was sent to it. Plugins see
/// `transport`, and take sidechains from the `earlier` tracks.
#[expect(clippy::too_many_arguments, reason = "one block's whole context")]
pub(crate) fn render_track(
    track: &mut TrackNode,
    earlier: &[TrackNode],
    position: u64,
    playing: bool,
    chase: bool,
    transport: &Transport,
    live: Live,
    input: [&[f32]; 2],
    bus_in: Option<[&[f32]; 2]>,
    out: &mut MidiOutput,
    lanes: [&mut [f32]; 2],
) {
    let [left, right] = lanes;
    match bus_in {
        Some([l, r]) => {
            left.copy_from_slice(l);
            right.copy_from_slice(r);
        }
        None => {
            left.fill(0.0);
            right.fill(0.0);
        }
    }
    let frames = left.len();
    if playing {
        let block_end = position + frames as u64;
        for clip in &track.clips {
            let from = position.max(clip.start);
            let to = block_end.min(clip.end);
            if from >= to {
                continue;
            }
            let len = (to - from) as usize;
            let dst = (from - position) as usize;
            let src = clip.offset + (from - clip.start) as usize;
            let first = &clip.data.channel(0)[src..src + len];
            let second = if clip.data.channel_count() > 1 {
                &clip.data.channel(1)[src..src + len]
            } else {
                first
            };
            let lanes = left[dst..dst + len]
                .iter_mut()
                .zip(&mut right[dst..dst + len]);
            for ((l, r), (a, b)) in lanes.zip(first.iter().zip(second)) {
                *l += a * clip.gain;
                *r += b * clip.gain;
            }
        }
    }
    if let Some(monitored) = track.input.filter(|i| i.monitor) {
        let [a, b] = monitored.pick(input);
        for ((l, r), (a, b)) in left.iter_mut().zip(right.iter_mut()).zip(a.iter().zip(b)) {
            *l += a;
            *r += b;
        }
    }
    if let Some(instrument) = &mut track.instrument {
        let start = playing.then_some(position);
        let Instrument {
            voices,
            notes,
            controls,
            lanes,
            longest,
            monitor,
            input,
            ..
        } = &mut **instrument;
        let (live, base) = if *monitor {
            (live.events, live.base)
        } else {
            (&[][..], 0)
        };
        let live = LiveInput {
            events: live,
            base,
            filter: *input,
            one_channel: matches!(voices, Voices::Device),
        };
        let clips = Clips { notes, controls };
        let chase = |sink: &mut Sink| {
            if chase && playing {
                for lane in lanes.iter() {
                    let before = lane.events.partition_point(|e| e.frame < position);
                    let last = before.checked_sub(1).map(|i| &lane.events[i]);
                    let data = match last {
                        Some(e) => Some((e.data, e.value)),
                        None => lane.default,
                    };
                    if let Some((data, value)) = data {
                        sink.control(0, data, value);
                    }
                }
                let from = notes.partition_point(|n| n.start < position.saturating_sub(*longest));
                let to = notes.partition_point(|n| n.start < position);
                for note in &notes[from..to] {
                    if note.end > position {
                        sink.note_on(0, note.tag, note.pitch, note.amp, note.end);
                    }
                }
            }
        };
        match voices {
            Voices::Synth(synth) => {
                let mut sink = Sink::Synth(synth);
                chase(&mut sink);
                play_notes(&mut sink, clips, start, live, left);
                right.copy_from_slice(left);
            }
            Voices::Plugin(tracker) => {
                if let Some(first) = track.plugins.first_mut() {
                    let mut sink = Sink::Plugin(tracker, &mut first.events);
                    chase(&mut sink);
                    play_notes(&mut sink, clips, start, live, left);
                }
            }
            Voices::Device => {
                if let Some(device) = &mut track.device {
                    let External {
                        tracker, events, ..
                    } = &mut **device;
                    let mut sink = Sink::Plugin(tracker, events);
                    chase(&mut sink);
                    play_notes(&mut sink, clips, start, live, left);
                }
            }
        }
    }
    // MIDI a plugin sends goes to the next one that takes notes; plugins
    // that don't take notes (or are bypassed) pass it on. On a track with
    // a device, what the leading MIDI effects send goes there instead.
    let [carry, sent] = &mut track.chain_midi;
    carry.clear();
    let mut to_device = track.to_device_after;
    for (i, node) in track.plugins.iter_mut().enumerate() {
        if to_device == Some(i) {
            if let Some(device) = &mut track.device {
                send_chain_midi(device, carry);
            }
            carry.clear();
            to_device = None;
        }
        let takes = node.notes_in && !node.bypass && node.processor.is_some();
        if takes {
            for event in carry.drain(..) {
                push_event(&mut node.events, event.offset, event.kind);
            }
        }
        sent.clear();
        run_plugin(
            node, earlier, position, playing, transport, left, right, sent,
        );
        if track.to_device_after.is_some() && to_device.is_none() {
            // Past the MIDI effects: no more MIDI.
            continue;
        }
        for event in sent.drain(..) {
            push_event(carry, event.offset, event.kind);
        }
    }
    if let Some(device) = &mut track.device {
        if to_device.is_some() {
            send_chain_midi(device, carry);
        }
        out.flush(device);
        out.send_due(device, Some(frames as u32));
    }
    // For pre-fader sends.
    if let Some([l, r]) = &mut track.pre {
        l[..frames].copy_from_slice(left);
        r[..frames].copy_from_slice(right);
    }
    apply_fader(&track.fader, position, playing, left, right);
}

/// Where an instrument's notes go.
enum Sink<'a> {
    Synth(&'a mut PolySynth),
    Plugin(&'a mut NoteTracker, &'a mut Vec<PluginEvent>),
}

/// The MIDI channel of a voice tag (0 for clip notes).
fn channel_of(tag: u64) -> u8 {
    if tag & LIVE != 0 {
        ((tag >> 8) & 0x0f) as u8
    } else {
        0
    }
}

fn live_tag(channel: u8, pitch: u8) -> u64 {
    LIVE | u64::from(channel) << 8 | u64::from(pitch)
}

/// Voice tag of a note a plugin chain sent to a device.
fn chain_tag(channel: u8, pitch: u8) -> u64 {
    CHAIN | u64::from(channel & 0x0f) << 8 | u64::from(pitch)
}

/// Passes what a plugin chain sent on to its track's device, keeping
/// track of the notes held there.
fn send_chain_midi(device: &mut External, midi: &[PluginEvent]) {
    let External {
        tracker, events, ..
    } = device;
    for event in midi {
        let at = event.offset;
        match event.kind {
            PluginEventKind::NoteOn {
                channel,
                key,
                velocity,
            } => tracker.note_on(
                events,
                at,
                chain_tag(channel, key),
                channel,
                key,
                velocity,
                u64::MAX,
            ),
            PluginEventKind::NoteOff { channel, key } => {
                tracker.note_off(events, at, chain_tag(channel, key))
            }
            PluginEventKind::Midi { data } => {
                if data[0] & 0xf0 == 0xb0 && matches!(data[1], 120 | 123) {
                    tracker.release_where(events, at, |t| t & CHAIN != 0);
                }
                push_event(events, at, PluginEventKind::Midi { data });
            }
            kind @ PluginEventKind::PerNote { .. } => push_event(events, at, kind),
            PluginEventKind::Param { .. } => {}
        }
    }
}

/// Gives the synth a per-note controller.
fn per_note(synth: &mut PolySynth, pitch: u8, controller: PerNote, value: u32) {
    match controller {
        PerNote::PitchBend => synth.set_note_bend(pitch, value),
        PerNote::Registered(number) => synth.set_note_control(pitch, number, value),
        PerNote::Assignable(_) => {}
        PerNote::Management { detach, reset } => synth.manage_note(pitch, detach, reset),
    }
}

impl Sink<'_> {
    fn note_on(&mut self, at: u32, tag: u64, pitch: u8, amp: f32, deadline: u64) {
        match self {
            Sink::Synth(synth) => synth.note_on(tag, pitch, amp, deadline),
            Sink::Plugin(tracker, events) => tracker.note_on(
                events,
                at,
                tag,
                channel_of(tag),
                pitch,
                amp.clamp(0.0, 1.0),
                deadline,
            ),
        }
    }

    fn release_due(&mut self, at: u32, now: u64) {
        match self {
            Sink::Synth(synth) => synth.release_due(now),
            Sink::Plugin(tracker, events) => tracker.release_due(events, at, now),
        }
    }

    fn next_deadline(&self) -> Option<u64> {
        match self {
            Sink::Synth(synth) => synth.next_deadline(),
            Sink::Plugin(tracker, _) => tracker.next_deadline(),
        }
    }

    /// A controller change from a clip (channel 0), with the 32-bit value
    /// of a per-note one.
    fn control(&mut self, at: u32, data: [u8; 3], value: u32) {
        let per = PerNote::from_data(data);
        match (self, per) {
            (Sink::Synth(synth), Some(controller)) => per_note(synth, data[1], controller, value),
            (Sink::Synth(synth), None) => match data[0] & 0xf0 {
                0xe0 => synth.set_bend(u16::from(data[1]) | u16::from(data[2]) << 7),
                0xb0 if matches!(data[1], 120 | 123) => synth.release_where(|t| t & LIVE == 0),
                0xb0 => synth.set_control(data[1], data[2]),
                _ => {}
            },
            (Sink::Plugin(_, events), Some(controller)) => {
                let kind = PluginEventKind::PerNote {
                    channel: data[0] & 0x0f,
                    key: data[1],
                    controller,
                    value,
                };
                push_event(events, at, kind);
            }
            (Sink::Plugin(_, events), None) => {
                push_event(events, at, PluginEventKind::Midi { data })
            }
        }
    }

    fn live(&mut self, at: u32, event: &MidiEvent) {
        match (self, event.message()) {
            (
                Sink::Synth(synth),
                Some(Midi::NoteOn {
                    channel,
                    pitch,
                    velocity,
                }),
            ) => synth.note_on(
                live_tag(channel, pitch),
                pitch,
                f32::from(velocity) / 127.0,
                u64::MAX,
            ),
            (Sink::Synth(synth), Some(Midi::NoteOff { channel, pitch })) => {
                synth.note_off(live_tag(channel, pitch))
            }
            (Sink::Synth(synth), Some(Midi::Sustain(down))) => synth.set_pedal(down),
            (Sink::Synth(synth), Some(Midi::AllNotesOff)) => synth.release_where(|t| t & LIVE != 0),
            (Sink::Synth(synth), Some(Midi::ResetControllers)) => synth.reset_controllers(),
            (Sink::Synth(synth), Some(Midi::Control { controller, value })) => {
                synth.set_control(controller, value)
            }
            (Sink::Synth(synth), Some(Midi::PitchBend(value))) => synth.set_bend(value),
            (
                Sink::Synth(synth),
                Some(Midi::PerNote {
                    pitch,
                    controller,
                    value,
                    ..
                }),
            ) => per_note(synth, pitch, controller, value),
            (
                Sink::Plugin(_, events),
                Some(Midi::PerNote {
                    channel,
                    pitch,
                    controller,
                    value,
                }),
            ) => push_event(
                events,
                at,
                PluginEventKind::PerNote {
                    channel,
                    key: pitch,
                    controller,
                    value,
                },
            ),
            (
                Sink::Plugin(tracker, events),
                Some(Midi::NoteOn {
                    channel,
                    pitch,
                    velocity,
                }),
            ) => tracker.note_on(
                events,
                at,
                live_tag(channel, pitch),
                channel,
                pitch,
                f32::from(velocity) / 127.0,
                u64::MAX,
            ),
            (Sink::Plugin(tracker, events), Some(Midi::NoteOff { channel, pitch })) => {
                tracker.note_off(events, at, live_tag(channel, pitch))
            }
            (Sink::Plugin(tracker, events), Some(Midi::AllNotesOff)) => {
                tracker.release_where(events, at, |t| t & LIVE != 0)
            }
            // Everything else (pedal, controllers, bend, pressure, program
            // changes) goes to the plugin as it is.
            (Sink::Plugin(_, events), _) => {
                if (0x80..0xf0).contains(&event.data[0]) {
                    push_event(events, at, PluginEventKind::Midi { data: event.data });
                }
            }
            (Sink::Synth(_), None) => {}
        }
    }

    fn render(&mut self, out: &mut [f32]) {
        if let Sink::Synth(synth) = self {
            synth.render(out);
        }
    }
}

/// What the clips of a MIDI track play.
#[derive(Clone, Copy)]
struct Clips<'a> {
    notes: &'a [NoteEvent],
    controls: &'a [ControlEvent],
}

/// Live input an instrument plays: offsets count from the process buffer,
/// which the block starts `base` frames into. Only events `filter` accepts
/// play. With `one_channel` every message is taken as channel 1
/// (a device track sends on one channel, so held keys are tracked on it).
#[derive(Clone, Copy)]
struct LiveInput<'a> {
    events: &'a [MidiEvent],
    base: u32,
    filter: InputFilter,
    one_channel: bool,
}

/// Plays one block of notes and controller changes into `sink` (the synth
/// renders into `out`, mono; a plugin or device gets events). `start` is
/// the block's timeline position while playing, `None` while stopped (only
/// live input then). Notes start and end on their exact frame; controller
/// changes at a frame come before notes starting there.
fn play_notes(sink: &mut Sink, clips: Clips, start: Option<u64>, live: LiveInput, out: &mut [f32]) {
    let LiveInput {
        events: live,
        base,
        filter,
        one_channel,
    } = live;
    let Clips { notes, controls } = clips;
    let frames = out.len();
    let mut next_note = start.map_or(notes.len(), |s| notes.partition_point(|n| n.start < s));
    let mut next_control = start.map_or(controls.len(), |s| {
        controls.partition_point(|c| c.frame < s)
    });
    let mut next_live = 0;
    let mut t = 0;
    loop {
        if let Some(block_start) = start {
            let now = block_start + t as u64;
            while let Some(control) = controls.get(next_control).filter(|c| c.frame <= now) {
                sink.control(t as u32, control.data, control.value);
                next_control += 1;
            }
            sink.release_due(t as u32, now);
            while let Some(note) = notes.get(next_note).filter(|n| n.start <= now) {
                sink.note_on(t as u32, note.tag, note.pitch, note.amp, note.end);
                next_note += 1;
            }
        }
        while let Some(event) = live
            .get(next_live)
            .filter(|e| (e.offset - base) as usize <= t)
        {
            if filter.accepts(event.source, event.data) {
                let mut event = *event;
                if one_channel && event.data[0] < 0xf0 {
                    event.data[0] &= 0xf0;
                }
                sink.live(t as u32, &event);
            }
            next_live += 1;
        }
        // Render up to the next thing that happens.
        let mut until = frames;
        if let Some(block_start) = start {
            let now = block_start + t as u64;
            let block_end = block_start + frames as u64;
            let relative = |frame: u64| (frame - block_start) as usize;
            if let Some(deadline) = sink.next_deadline().filter(|&d| d > now) {
                until = until.min(relative(deadline.min(block_end)));
            }
            if let Some(note) = notes.get(next_note) {
                until = until.min(relative(note.start.min(block_end)));
            }
            if let Some(control) = controls.get(next_control) {
                until = until.min(relative(control.frame.min(block_end)));
            }
        }
        if let Some(event) = live.get(next_live) {
            until = until.min((event.offset - base) as usize);
        }
        sink.render(&mut out[t..until]);
        t = until;
        if t >= frames {
            break;
        }
    }
}

/// Runs one plugin over the block, after giving it parameter changes. Its
/// sidechain comes from one of the `earlier` tracks.
#[expect(clippy::too_many_arguments, reason = "one plugin's whole context")]
fn run_plugin(
    node: &mut PluginNode,
    earlier: &[TrackNode],
    position: u64,
    playing: bool,
    transport: &Transport,
    left: &mut [f32],
    right: &mut [f32],
    midi_out: &mut Vec<PluginEvent>,
) {
    let frames = left.len();
    if node.processor.is_none() || node.bypass {
        node.events.clear();
        // Nothing comes out of its other outputs.
        for buffer in node.aux.iter_mut() {
            buffer
                .channels
                .iter_mut()
                .for_each(|c| c[..frames].fill(0.0));
        }
        return;
    }
    // Values from the project that the plugin doesn't have yet.
    for (index, (&target, sent)) in node.targets.iter().zip(node.sent.iter_mut()).enumerate() {
        let automated = node.lanes.iter().any(|(i, _)| *i as usize == index);
        if !automated && target.to_bits() != sent.to_bits() {
            *sent = target;
            push_event(
                &mut node.events,
                0,
                PluginEventKind::Param {
                    index: index as u32,
                    value: target,
                },
            );
        }
    }
    // Automation, every PARAM_STEP frames while playing.
    for (index, points) in node.lanes.iter() {
        let steps = if playing {
            frames.div_ceil(PARAM_STEP)
        } else {
            1
        };
        for step in 0..steps {
            let at = step * PARAM_STEP;
            let frame = if playing {
                position + at as u64
            } else {
                position
            };
            // What the plugin gets now played `offset` frames ago.
            let frame = frame.saturating_sub(u64::from(node.offset));
            let Some(value) = lane_value(points, frame) else {
                continue;
            };
            let sent = &mut node.sent[*index as usize];
            if value.to_bits() != sent.to_bits() {
                *sent = value;
                push_event(
                    &mut node.events,
                    at as u32,
                    PluginEventKind::Param {
                        index: *index,
                        value,
                    },
                );
            }
        }
    }
    // Notes came first, parameters after: sort by offset, keeping the
    // order of events at the same offset.
    let events = &mut node.events;
    for i in 1..events.len() {
        let mut j = i;
        while j > 0 && events[j - 1].offset > events[j].offset {
            events.swap(j - 1, j);
            j -= 1;
        }
    }
    let PluginNode {
        processor,
        pre_delay,
        sidechain,
        aux,
        events,
        ..
    } = node;
    if let Some(delay) = pre_delay {
        delay.process(left, right);
    }
    let sidechain = sidechain.as_mut().map(|sidechain| {
        let [l, r] = &mut sidechain.buffer;
        let (l, r) = (&mut l[..frames], &mut r[..frames]);
        match &earlier[sidechain.source].tap {
            Some([tl, tr]) => {
                l.copy_from_slice(&tl[..frames]);
                r.copy_from_slice(&tr[..frames]);
            }
            None => {
                l.fill(0.0);
                r.fill(0.0);
            }
        }
        if let Some(delay) = &mut sidechain.delay {
            delay.process(l, r);
        }
        [&*l, &*r]
    });
    let context = ProcessContext {
        transport: *transport,
        sidechain,
        aux,
    };
    if let Some(processor) = processor {
        processor.process([left, right], context, events, MidiSink::new(midi_out));
    }
    events.clear();
}

impl DelayLine {
    /// Delays the block by `frames`. Real-time safe.
    pub(crate) fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let slot = &mut self.ring[self.at * 2..self.at * 2 + 2];
            let (out_l, out_r) = (slot[0], slot[1]);
            slot[0] = *l;
            slot[1] = *r;
            *l = out_l;
            *r = out_r;
            self.at = (self.at + 1) % self.frames;
        }
    }
}

/// Carries what `then` holds into `now` if they're as long, so a delayed
/// signal goes on across a graph swap rather than starting from silence.
fn continue_delay(now: &mut Option<DelayLine>, then: &Option<DelayLine>) {
    if let (Some(now), Some(then)) = (now, then)
        && now.frames == then.frames
    {
        now.ring.copy_from_slice(&then.ring);
        now.at = then.at;
    }
}

fn apply_fader(fader: &Fader, position: u64, playing: bool, left: &mut [f32], right: &mut [f32]) {
    // The audio here played `offset` frames ago (plugin latency).
    let position = position.saturating_sub(u64::from(fader.offset));
    if !fader.automated() {
        let [gain_l, gain_r] = fader.fixed;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            *l *= gain_l;
            *r *= gain_r;
        }
        return;
    }
    if !playing {
        let [gain_l, gain_r] = fader.gains_at(position);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            *l *= gain_l;
            *r *= gain_r;
        }
        return;
    }
    // Ramp between values every FADER_STEP frames.
    let mut from = 0;
    while from < left.len() {
        let len = (left.len() - from).min(FADER_STEP);
        let a = fader.gains_at(position + from as u64);
        let b = fader.gains_at(position + (from + len) as u64);
        for i in 0..len {
            let t = i as f32 / len as f32;
            left[from + i] *= a[0] + (b[0] - a[0]) * t;
            right[from + i] *= a[1] + (b[1] - a[1]) * t;
        }
        from += len;
    }
}

/// Releases every note that clips started (not live input), e.g. when the
/// playhead jumps or stops, and puts the controllers clips change back to
/// their defaults (a held sustain pedal would keep the notes ringing).
/// Playback that continues catches up on them again (chase).
pub(crate) fn release_clip_notes(graph: &mut Graph) {
    for track in &mut graph.tracks {
        let Some(instrument) = &mut track.instrument else {
            continue;
        };
        let lanes = &instrument.lanes;
        let mut sink = match &mut instrument.voices {
            Voices::Synth(synth) => {
                synth.release_where(|tag| tag & LIVE == 0);
                Sink::Synth(synth)
            }
            Voices::Plugin(tracker) => {
                // Notes the chain left on a device, too.
                if let Some(device) = &mut track.device {
                    let External {
                        tracker, events, ..
                    } = &mut **device;
                    tracker.release_where(events, 0, |tag| tag & LIVE == 0);
                }
                let Some(first) = track.plugins.first_mut() else {
                    continue;
                };
                tracker.release_where(&mut first.events, 0, |tag| tag & LIVE == 0);
                Sink::Plugin(tracker, &mut first.events)
            }
            Voices::Device => {
                let Some(device) = &mut track.device else {
                    continue;
                };
                let External {
                    tracker, events, ..
                } = &mut **device;
                tracker.release_where(events, 0, |tag| tag & LIVE == 0);
                Sink::Plugin(tracker, events)
            }
        };
        for (data, value) in lanes.iter().filter_map(|l| l.default) {
            sink.control(0, data, value);
        }
    }
}

/// Moves the state that outlives a graph from `old` to `new`: sounding
/// voices, plugin processors and the values they were sent, and delay line
/// contents. Processors left in `old` (removed plugins) are stopped; notes
/// held on a device the new graph no longer sends to are released through
/// `out`. Real-time safe.
pub(crate) fn carry_over(old: &mut Graph, new: &mut Graph, out: &mut MidiOutput) {
    for track in &mut new.tracks {
        let previous = old.tracks.iter_mut().find(|t| t.id == track.id);
        let Some(previous) = previous else {
            continue;
        };
        if let (Some(instrument), Some(before)) = (&mut track.instrument, &mut previous.instrument)
        {
            match (&mut instrument.voices, &mut before.voices) {
                (Voices::Synth(now), Voices::Synth(then)) => now.continue_from(then),
                // Copies into the existing box: no allocation.
                (Voices::Plugin(now), Voices::Plugin(then)) => **now = (**then).clone(),
                _ => {}
            }
            let monitor = instrument.monitor;
            let ends = &instrument.ends;
            let retarget = |tag: u64, deadline: u64| {
                if tag & LIVE != 0 {
                    monitor.then_some(deadline)
                } else if tag & CHAIN != 0 {
                    Some(deadline)
                } else {
                    ends.binary_search_by_key(&tag, |&(t, _)| t)
                        .ok()
                        .map(|i| ends[i].1)
                }
            };
            match &mut instrument.voices {
                Voices::Synth(synth) => synth.retarget(retarget),
                Voices::Plugin(tracker) => tracker.retarget(retarget),
                Voices::Device => {}
            }
            if let (Some(now), Some(then)) = (&mut track.device, &mut previous.device)
                && now.device == then.device
                && now.channel == then.channel
            {
                // Moved, so the old graph doesn't release them below.
                now.tracker = std::mem::take(&mut then.tracker);
                for event in then.events.drain(..) {
                    push_event(&mut now.events, event.offset, event.kind);
                }
                // Held-back messages move with the change in delay
                // (earlier ones first, so the order stays).
                let (from, to) = (u64::from(then.delay), u64::from(now.delay));
                for (due, data, value) in then.pending.drain(..) {
                    if now.pending.len() < now.pending.capacity() {
                        now.pending
                            .push(((due + to).saturating_sub(from), data, value));
                    }
                }
                now.tracker.retarget(retarget);
            }
        }
        continue_delay(&mut track.delay, &previous.delay);
    }
    // Sends, by the bus they go to.
    for i in 0..new.tracks.len() {
        let id = new.tracks[i].id;
        let Some(previous) = old.tracks.iter().find(|t| t.id == id) else {
            continue;
        };
        for j in 0..new.tracks[i].sends.len() {
            let bus = new.tracks[new.tracks[i].sends[j].dest].id;
            let then = previous.sends.iter().find(|s| old.tracks[s.dest].id == bus);
            if let Some(then) = then {
                continue_delay(&mut new.tracks[i].sends[j].delay, &then.delay);
            }
        }
    }
    for t in 0..new.tracks.len() {
        // Sidechains come from earlier tracks.
        let (earlier, rest) = new.tracks.split_at_mut(t);
        for node in rest[0].plugins.iter_mut().filter(|n| n.processor.is_none()) {
            let found = old.tracks.iter().enumerate().find_map(|(i, track)| {
                let at = track
                    .plugins
                    .iter()
                    .position(|n| n.key == node.key && n.processor.is_some());
                at.map(|j| (i, j))
            });
            let Some((i, j)) = found else {
                continue;
            };
            let source = old.tracks[i].plugins[j]
                .sidechain
                .as_ref()
                .map(|s| old.tracks[s.source].id);
            let before = &mut old.tracks[i].plugins[j];
            node.processor = before.processor.take();
            if node.sent.len() == before.sent.len() {
                node.sent.copy_from_slice(&before.sent);
            }
            continue_delay(&mut node.pre_delay, &before.pre_delay);
            // Its sidechain's, if it still comes from the same track.
            if let (Some(now), Some(then)) = (&mut node.sidechain, &before.sidechain)
                && source == Some(earlier[now.source].id)
            {
                continue_delay(&mut now.delay, &then.delay);
            }
            // Its other outputs', by output.
            for route in node.aux_routes.iter_mut() {
                let then = before.aux_routes.iter().find(|r| r.output == route.output);
                if let Some(then) = then {
                    continue_delay(&mut route.delay, &then.delay);
                }
            }
        }
    }
    for node in old.tracks.iter_mut().flat_map(|t| t.plugins.iter_mut()) {
        if let Some(processor) = &mut node.processor {
            processor.stop();
        }
    }
    // Notes left on devices: their tracks are gone, or send elsewhere now.
    for track in &mut old.tracks {
        if let Some(device) = &mut track.device {
            let External {
                tracker, events, ..
            } = &mut **device;
            tracker.release_where(events, 0, |_| true);
            out.flush(device);
            out.send_due(device, None);
        }
    }
}
