// SPDX-License-Identifier: AGPL-3.0-or-later
//! Live MIDI input: the events the audio backend hands the engine, and
//! parsers for the two wire formats PipeWire delivers (MIDI 1.0 bytes and
//! Universal MIDI Packets).

use dk_core::{
    PER_NOTE_ASSIGNABLE, PER_NOTE_BEND, PER_NOTE_DETACH, PER_NOTE_MANAGEMENT, PER_NOTE_REGISTERED,
    PER_NOTE_RESET, is_per_note,
};

/// A channel message arriving `offset` frames into the current audio
/// buffer: a MIDI 1.0 message, or a MIDI 2.0 per-note one (a status below
/// 0x80, see [`dk_core::PER_NOTE_REGISTERED`]) with its 32-bit `value`.
/// Only short channel messages (note on/off, control change, ...) are
/// kept; system messages are dropped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MidiEvent {
    pub offset: u32,
    pub data: [u8; 3],
    /// The device it came from ([`device_tag`] of its name), or 0 if
    /// unknown.
    pub source: u32,
    /// A per-note message's 32-bit value; 0 for MIDI 1.0.
    pub value: u32,
}

/// What a MIDI 2.0 per-note message controls (or does).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerNote {
    /// Per-note pitch bend (0x8000_0000 is none).
    PitchBend,
    /// A registered per-note controller.
    Registered(u8),
    /// An assignable per-note controller.
    Assignable(u8),
    /// Per-note management (no value): `detach` leaves the per-note
    /// controllers sent so far with the key's sounding notes, so later
    /// ones only reach later notes; `reset` sets the key's per-note
    /// controllers back to their defaults (after detaching).
    Management { detach: bool, reset: bool },
}

impl PerNote {
    /// The per-note message's three bytes on `channel` for `pitch`.
    pub fn data(self, channel: u8, pitch: u8) -> [u8; 3] {
        let channel = channel & 0x0f;
        match self {
            Self::PitchBend => [PER_NOTE_BEND | channel, pitch & 0x7f, 0],
            Self::Registered(n) => [PER_NOTE_REGISTERED | channel, pitch & 0x7f, n],
            Self::Assignable(n) => [PER_NOTE_ASSIGNABLE | channel, pitch & 0x7f, n],
            Self::Management { detach, reset } => {
                let flags = if detach { PER_NOTE_DETACH } else { 0 }
                    | if reset { PER_NOTE_RESET } else { 0 };
                [PER_NOTE_MANAGEMENT | channel, pitch & 0x7f, flags]
            }
        }
    }

    /// What a per-note message's bytes control, if they're one.
    pub fn from_data(data: [u8; 3]) -> Option<Self> {
        match data[0] & 0xf0 {
            _ if data[0] >= 0x80 => None,
            PER_NOTE_BEND => Some(Self::PitchBend),
            PER_NOTE_REGISTERED => Some(Self::Registered(data[2])),
            PER_NOTE_ASSIGNABLE => Some(Self::Assignable(data[2])),
            PER_NOTE_MANAGEMENT => Some(Self::Management {
                detach: data[2] & PER_NOTE_DETACH != 0,
                reset: data[2] & PER_NOTE_RESET != 0,
            }),
            _ => None,
        }
    }
}

/// Identifies a MIDI device by name on the real-time thread: a hash that
/// is never 0 (0 means "unknown device").
pub fn device_tag(name: &str) -> u32 {
    // FNV-1a.
    let hash = name.bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    hash.max(1)
}

/// Which live MIDI a track takes: from one device and/or on one channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputFilter {
    /// A [`device_tag`]; `None`: any device.
    pub device: Option<u32>,
    /// Channel 0 to 15; `None`: any channel.
    pub channel: Option<u8>,
}

impl InputFilter {
    /// The filter for a device name and a channel from 1 to 16.
    pub fn new(device: Option<&str>, channel: Option<u8>) -> Self {
        Self {
            device: device.map(device_tag),
            channel: channel.map(|c| c.clamp(1, 16) - 1),
        }
    }

    /// Whether a message `data` from device `source` passes.
    pub fn accepts(&self, source: u32, data: [u8; 3]) -> bool {
        self.device.is_none_or(|d| d == source)
            && self
                .channel
                .is_none_or(|c| data[0] < 0xf0 && data[0] & 0x0f == c)
    }
}

impl MidiEvent {
    /// A MIDI 1.0 message.
    pub fn midi1(offset: u32, data: [u8; 3]) -> Self {
        Self {
            offset,
            data,
            ..Self::default()
        }
    }

    pub fn note_on(offset: u32, channel: u8, pitch: u8, velocity: u8) -> Self {
        Self::midi1(
            offset,
            [0x90 | (channel & 0x0f), pitch & 0x7f, velocity & 0x7f],
        )
    }

    pub fn note_off(offset: u32, channel: u8, pitch: u8) -> Self {
        Self::midi1(offset, [0x80 | (channel & 0x0f), pitch & 0x7f, 0])
    }

    /// A MIDI 2.0 per-note message.
    pub fn per_note(offset: u32, channel: u8, pitch: u8, controller: PerNote, value: u32) -> Self {
        Self {
            offset,
            data: controller.data(channel, pitch),
            source: 0,
            value,
        }
    }

    /// The same message from device `source`.
    pub fn from_device(self, source: u32) -> Self {
        Self { source, ..self }
    }

    /// What the message means to an instrument.
    pub fn message(&self) -> Option<Message> {
        let [status, a, b] = self.data;
        let channel = status & 0x0f;
        if let Some(controller) = PerNote::from_data(self.data) {
            return Some(Message::PerNote {
                channel,
                pitch: a & 0x7f,
                controller,
                value: self.value,
            });
        }
        match status & 0xf0 {
            0x90 if b > 0 => Some(Message::NoteOn {
                channel,
                pitch: a,
                velocity: b,
            }),
            0x80 | 0x90 => Some(Message::NoteOff { channel, pitch: a }),
            0xb0 => match a {
                64 => Some(Message::Sustain(b >= 64)),
                120 | 123 => Some(Message::AllNotesOff),
                121 => Some(Message::ResetControllers),
                _ => Some(Message::Control {
                    controller: a,
                    value: b,
                }),
            },
            0xe0 => Some(Message::PitchBend(u16::from(a) | u16::from(b) << 7)),
            _ => None,
        }
    }
}

/// A MIDI message for a device: `dest` is its [`device_tag`], `offset`
/// counts frames into the process buffer. Per-note messages (a status
/// below 0x80, as in [`MidiEvent`]) carry their 32-bit `value` and go out
/// as Universal MIDI Packets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MidiOut {
    pub offset: u32,
    pub dest: u32,
    pub data: [u8; 3],
    pub value: u32,
}

impl MidiOut {
    /// The message as a MIDI 2.0 Universal MIDI Packet (group 0), for
    /// per-note messages.
    pub fn ump(&self) -> Option<[u32; 2]> {
        let [status, note, index] = self.data;
        is_per_note(status).then(|| {
            // Per-note management goes out with its own opcode.
            let status = if status & 0xf0 == PER_NOTE_MANAGEMENT {
                0xf0 | status & 0x0f
            } else {
                status
            };
            let word = 0x4000_0000 | u32::from(status) << 16 | u32::from(note) << 8;
            [word | u32::from(index), self.value]
        })
    }
}

/// The messages instruments act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    NoteOn {
        channel: u8,
        pitch: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        pitch: u8,
    },
    Sustain(bool),
    AllNotesOff,
    /// Reset all controllers (CC 121).
    ResetControllers,
    /// Another control change.
    Control {
        controller: u8,
        value: u8,
    },
    /// Pitch bend, 0 to 16383 (8192 is none).
    PitchBend(u16),
    /// A MIDI 2.0 per-note controller with its 32-bit value.
    PerNote {
        channel: u8,
        pitch: u8,
        controller: PerNote,
        value: u32,
    },
}

/// Number of data bytes after a channel status byte.
fn data_len(status: u8) -> usize {
    match status & 0xf0 {
        0xc0 | 0xd0 => 1,
        _ => 2,
    }
}

/// Parses MIDI 1.0 bytes (one or more messages, with running status) into
/// channel events at `offset`.
pub fn parse_midi1(bytes: &[u8], offset: u32, mut push: impl FnMut(MidiEvent)) {
    let mut status = 0u8;
    let mut data = [0u8; 2];
    let mut have = 0;
    for &byte in bytes {
        if byte >= 0xf8 {
            // Real-time messages may appear anywhere and change nothing.
            continue;
        }
        if byte & 0x80 != 0 {
            // A system common message cancels running status.
            status = if byte < 0xf0 { byte } else { 0 };
            have = 0;
            continue;
        }
        if status == 0 {
            continue;
        }
        data[have] = byte;
        have += 1;
        if have == data_len(status) {
            push(MidiEvent::midi1(
                offset,
                [status, data[0], if have == 2 { data[1] } else { 0 }],
            ));
            have = 0;
        }
    }
}

/// Parses Universal MIDI Packets (32-bit words) into channel events at
/// `offset`. MIDI 2.0 channel voice messages become MIDI 1.0 ones, except
/// per-note controllers and pitch bend, which keep their 32-bit values;
/// other message types are skipped.
pub fn parse_ump(words: &[u32], offset: u32, mut push: impl FnMut(MidiEvent)) {
    let mut i = 0;
    while i < words.len() {
        let word = words[i];
        let kind = (word >> 28) as u8;
        let len = match kind {
            0x0..=0x2 | 0x6 | 0x7 => 1,
            0x3 | 0x4 | 0x8..=0xa => 2,
            0xb | 0xc => 3,
            _ => 4,
        };
        let status = (word >> 16) as u8;
        match kind {
            // MIDI 1.0 channel voice.
            0x2 => {
                let data = [status, (word >> 8) as u8 & 0x7f, word as u8 & 0x7f];
                // Only channel voice status bytes.
                if status >= 0x80 {
                    push(MidiEvent::midi1(offset, data));
                }
            }
            // MIDI 2.0 channel voice: 16-bit velocity, 32-bit controllers.
            0x4 if i + 1 < words.len() => {
                let value = words[i + 1];
                let index = (word >> 8) as u8 & 0x7f;
                let per_note = match status & 0xf0 {
                    PER_NOTE_REGISTERED => Some(PerNote::Registered(word as u8)),
                    PER_NOTE_ASSIGNABLE => Some(PerNote::Assignable(word as u8)),
                    PER_NOTE_BEND => Some(PerNote::PitchBend),
                    0xf0 => Some(PerNote::Management {
                        detach: word as u8 & PER_NOTE_DETACH != 0,
                        reset: word as u8 & PER_NOTE_RESET != 0,
                    }),
                    _ => None,
                };
                if let Some(controller) = per_note {
                    push(MidiEvent::per_note(
                        offset,
                        status & 0x0f,
                        index,
                        controller,
                        value,
                    ));
                }
                let data = match status & 0xf0 {
                    0x80 | 0x90 => {
                        let velocity = (value >> 25) as u8;
                        // A MIDI 2.0 note-on may have velocity 0; keep it on.
                        let velocity = if status & 0xf0 == 0x90 {
                            velocity.max(1)
                        } else {
                            velocity
                        };
                        Some([status, index, velocity])
                    }
                    0xb0 => Some([status, index, (value >> 25) as u8]),
                    0xd0 => Some([status, (value >> 25) as u8, 0]),
                    0xe0 => {
                        let bend = (value >> 18) as u16;
                        Some([status, (bend & 0x7f) as u8, (bend >> 7) as u8])
                    }
                    _ => None,
                };
                if let Some(data) = data {
                    push(MidiEvent::midi1(offset, data));
                }
            }
            _ => {}
        }
        i += len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn midi1(bytes: &[u8]) -> Vec<[u8; 3]> {
        let mut out = Vec::new();
        parse_midi1(bytes, 5, |e| {
            assert_eq!(e.offset, 5);
            out.push(e.data);
        });
        out
    }

    #[test]
    fn parses_midi1_with_running_status() {
        assert_eq!(
            midi1(&[0x90, 60, 100, 64, 90, 0xf8, 0x80, 60, 0]),
            [[0x90, 60, 100], [0x90, 64, 90], [0x80, 60, 0]]
        );
        // Program change has one data byte; sysex is skipped.
        assert_eq!(
            midi1(&[0xc1, 5, 0xf0, 1, 2, 0xf7, 0xb0, 64, 127]),
            [[0xc1, 5, 0], [0xb0, 64, 127]]
        );
    }

    #[test]
    fn parses_ump() {
        let mut out = Vec::new();
        let words = [
            0x2090_3c64, // MIDI 1.0 note on, 60, 100
            0x1000_0000, // system: skipped
            0x4091_4000,
            0xffff_0000, // MIDI 2.0 note on, ch 1, 64, full velocity
            0x4081_4000,
            0x0000_0000, // MIDI 2.0 note off
            0x40e0_0000,
            0x8000_0000, // MIDI 2.0 pitch bend, center
        ];
        parse_ump(&words, 0, |e| out.push(e.data));
        assert_eq!(
            out,
            [
                [0x90, 60, 100],
                [0x91, 64, 127],
                [0x81, 64, 0],
                [0xe0, 0, 64]
            ]
        );
    }

    #[test]
    fn parses_ump_per_note_controllers() {
        let mut out = Vec::new();
        let words = [
            0x4003_3c4a,
            0x8000_0000, // registered per-note 74 on key 60, ch 3
            0x4010_3d05,
            0x1234_5678, // assignable per-note 5 on key 61, ch 0
            0x4061_3e00,
            0xc000_0000, // per-note pitch bend on key 62, ch 1
            0x40f2_3f03,
            0x0000_0000, // per-note management on key 63, ch 2: detach, reset
        ];
        parse_ump(&words, 7, |e| out.push(e));
        assert_eq!(
            out,
            [
                MidiEvent::per_note(7, 3, 60, PerNote::Registered(74), 0x8000_0000),
                MidiEvent::per_note(7, 0, 61, PerNote::Assignable(5), 0x1234_5678),
                MidiEvent::per_note(7, 1, 62, PerNote::PitchBend, 0xc000_0000),
                MidiEvent::per_note(
                    7,
                    2,
                    63,
                    PerNote::Management {
                        detach: true,
                        reset: true
                    },
                    0
                ),
            ]
        );
        assert_eq!(out[3].data, [0x72, 63, 3]);
        assert_eq!(
            out[2].message(),
            Some(Message::PerNote {
                channel: 1,
                pitch: 62,
                controller: PerNote::PitchBend,
                value: 0xc000_0000
            })
        );
        // Back out as the same packets.
        let sent = out.iter().map(|e| MidiOut {
            offset: 0,
            dest: 0,
            data: e.data,
            value: e.value,
        });
        let packets: Vec<u32> = sent.flat_map(|m| m.ump().unwrap()).collect();
        assert_eq!(packets, words);
        // A channel filter sees their channel.
        assert!(InputFilter::new(None, Some(4)).accepts(0, out[0].data));
        assert!(!InputFilter::new(None, Some(1)).accepts(0, out[0].data));
        // MIDI 1.0 messages have no packet of their own.
        let note = MidiOut {
            data: [0x90, 60, 100],
            ..MidiOut::default()
        };
        assert_eq!(note.ump(), None);
    }

    #[test]
    fn messages() {
        let on = MidiEvent::note_on(0, 2, 60, 10);
        assert_eq!(
            on.message(),
            Some(Message::NoteOn {
                channel: 2,
                pitch: 60,
                velocity: 10
            })
        );
        let zero = MidiEvent {
            offset: 0,
            data: [0x90, 60, 0],
            source: 0,
            value: 0,
        };
        assert_eq!(
            zero.message(),
            Some(Message::NoteOff {
                channel: 0,
                pitch: 60
            })
        );
        let pedal = MidiEvent {
            offset: 0,
            data: [0xb0, 64, 100],
            source: 0,
            value: 0,
        };
        assert_eq!(pedal.message(), Some(Message::Sustain(true)));
        let bend = MidiEvent {
            offset: 0,
            data: [0xe1, 0x7f, 0x7f],
            source: 0,
            value: 0,
        };
        assert_eq!(bend.message(), Some(Message::PitchBend(16_383)));
        let wheel = MidiEvent {
            offset: 0,
            data: [0xb0, 1, 90],
            source: 0,
            value: 0,
        };
        assert_eq!(
            wheel.message(),
            Some(Message::Control {
                controller: 1,
                value: 90
            })
        );
    }
}
