// SPDX-License-Identifier: AGPL-3.0-or-later
//! Turns recorded MIDI (messages tagged with timeline positions, plus loop
//! wraps) into takes: one per loop pass that has notes, like audio takes.

use dk_core::{Controller, TimeRange};
use dk_engine::{InputFilter, MidiMessage, MidiRecord, PerNote};

/// A note in a take, in timeline frames (before latency compensation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TakeNote {
    pub(crate) pitch: u8,
    pub(crate) velocity: u8,
    pub(crate) start: u64,
    pub(crate) end: u64,
    /// The device it came from ([`dk_engine::device_tag`]).
    pub(crate) source: u32,
}

/// A controller change in a take, at a timeline frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TakeControl {
    pub(crate) frame: u64,
    pub(crate) controller: Controller,
    pub(crate) value: u16,
    pub(crate) source: u32,
}

/// One recorded pass, in timeline frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MidiTake {
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) notes: Vec<TakeNote>,
    pub(crate) controls: Vec<TakeControl>,
}

/// How a recording ran.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Span {
    /// Where recording started.
    pub(crate) start: u64,
    /// Where it stopped.
    pub(crate) stop: u64,
    /// The loop range playback wrapped around, if looping.
    pub(crate) loop_range: Option<TimeRange>,
    pub(crate) punch: Option<TimeRange>,
}

/// Builds takes from what the engine recorded (only what `input`
/// accepts). Notes still held at a wrap or at the stop end
/// there. With a punch range, only notes that start inside it are kept,
/// cut off at its end, and controller changes inside it. Passes without
/// notes make no take.
pub(crate) fn takes(records: &[MidiRecord], span: Span, input: InputFilter) -> Vec<MidiTake> {
    let mut takes = Vec::new();
    let mut held: Vec<Held> = Vec::new();
    let mut notes = Vec::new();
    let mut controls = Vec::new();
    let mut pass_start = span.start;
    let mut last = span.start;
    let close = |held: &mut Vec<Held>, notes: &mut Vec<TakeNote>, at: u64| {
        notes.extend(held.drain(..).map(|h| h.note(at)));
    };
    for record in records {
        match *record {
            MidiRecord::Event {
                position,
                data,
                source,
                value,
            } => {
                if !input.accepts(source, data) {
                    continue;
                }
                last = last.max(position);
                let event = dk_engine::MidiEvent {
                    offset: 0,
                    data,
                    source,
                    value,
                };
                match event.message() {
                    Some(MidiMessage::NoteOn {
                        channel,
                        pitch,
                        velocity,
                    }) => {
                        end_held(&mut held, &mut notes, channel, pitch, position);
                        held.push(Held {
                            channel,
                            note: TakeNote {
                                pitch,
                                velocity,
                                start: position,
                                end: position,
                                source,
                            },
                        });
                    }
                    Some(MidiMessage::NoteOff { channel, pitch }) => {
                        end_held(&mut held, &mut notes, channel, pitch, position);
                    }
                    Some(MidiMessage::AllNotesOff) => close(&mut held, &mut notes, position),
                    // A reset puts the key's per-note controllers back to
                    // their defaults. (Detaching has nothing to keep in a
                    // clip, whose controllers follow the key.)
                    Some(MidiMessage::PerNote {
                        pitch,
                        controller: PerNote::Management { reset: true, .. },
                        ..
                    }) => {
                        let mut touched: Vec<Controller> = controls
                            .iter()
                            .map(|c: &TakeControl| c.controller)
                            .filter(|c| c.midi2() && c.pitch() == Some(pitch))
                            .collect();
                        touched.sort();
                        touched.dedup();
                        controls.extend(touched.into_iter().map(|controller| TakeControl {
                            frame: position,
                            controller,
                            value: controller.default_value(),
                            source,
                        }));
                    }
                    _ => {
                        if let Some((controller, value)) = Controller::from_event(data, value) {
                            controls.push(TakeControl {
                                frame: position,
                                controller,
                                value,
                                source,
                            });
                        }
                    }
                }
            }
            MidiRecord::Wrap => {
                let Some(range) = span.loop_range else {
                    continue;
                };
                close(&mut held, &mut notes, range.end);
                let pass = (pass_start, range.end);
                push_take(&mut takes, pass, &mut notes, &mut controls, span.punch);
                pass_start = range.start;
                last = range.start;
            }
        }
    }
    let stop = span.stop.max(last + 1);
    close(&mut held, &mut notes, stop);
    push_take(
        &mut takes,
        (pass_start, stop),
        &mut notes,
        &mut controls,
        span.punch,
    );
    takes
}

/// A note being played.
struct Held {
    channel: u8,
    note: TakeNote,
}

impl Held {
    /// The note, ended at `at`.
    fn note(self, at: u64) -> TakeNote {
        TakeNote {
            end: at.max(self.note.start + 1),
            ..self.note
        }
    }
}

fn end_held(held: &mut Vec<Held>, notes: &mut Vec<TakeNote>, channel: u8, pitch: u8, at: u64) {
    if let Some(i) = held
        .iter()
        .position(|h| h.channel == channel && h.note.pitch == pitch)
    {
        notes.push(held.remove(i).note(at));
    }
}

fn push_take(
    takes: &mut Vec<MidiTake>,
    (start, end): (u64, u64),
    notes: &mut Vec<TakeNote>,
    controls: &mut Vec<TakeControl>,
    punch: Option<TimeRange>,
) {
    let mut notes = std::mem::take(notes);
    let mut controls = std::mem::take(controls);
    let (start, end) = match punch {
        Some(p) => (start.max(p.start), end.min(p.end)),
        None => (start, end),
    };
    notes.retain(|n| n.start >= start && n.start < end);
    for note in &mut notes {
        note.end = note.end.min(end);
    }
    if notes.is_empty() || end <= start {
        return;
    }
    notes.sort_by_key(|n| (n.start, n.pitch));
    controls.retain(|c| c.frame >= start && c.frame < end);
    takes.push(MidiTake {
        start,
        end,
        notes,
        controls,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(position: u64, pitch: u8) -> MidiRecord {
        MidiRecord::Event {
            position,
            data: [0x90, pitch, 100],
            source: 0,
            value: 0,
        }
    }

    fn off(position: u64, pitch: u8) -> MidiRecord {
        MidiRecord::Event {
            position,
            data: [0x80, pitch, 0],
            source: 0,
            value: 0,
        }
    }

    fn note(pitch: u8, start: u64, end: u64) -> TakeNote {
        TakeNote {
            pitch,
            velocity: 100,
            start,
            end,
            source: 0,
        }
    }

    fn any() -> InputFilter {
        InputFilter::default()
    }

    fn span(start: u64, stop: u64) -> Span {
        Span {
            start,
            stop,
            loop_range: None,
            punch: None,
        }
    }

    #[test]
    fn pairs_notes_and_closes_held_ones_at_stop() {
        let records = [on(10, 60), on(20, 64), off(30, 60), on(40, 60)];
        assert_eq!(
            takes(&records, span(5, 50), any()),
            [MidiTake {
                start: 5,
                end: 50,
                notes: vec![note(60, 10, 30), note(64, 20, 50), note(60, 40, 50)],
                controls: Vec::new(),
            }]
        );
        assert!(takes(&[], span(0, 100), any()).is_empty());
    }

    #[test]
    fn loop_passes_become_takes() {
        let records = [
            on(150, 60),
            MidiRecord::Wrap,
            // Pass 2 has no notes.
            MidiRecord::Wrap,
            on(110, 62),
            off(120, 62),
        ];
        let span = Span {
            loop_range: Some(TimeRange {
                start: 100,
                end: 200,
            }),
            ..span(140, 130)
        };
        assert_eq!(
            takes(&records, span, any()),
            [
                MidiTake {
                    start: 140,
                    end: 200,
                    notes: vec![note(60, 150, 200)],
                    controls: Vec::new(),
                },
                MidiTake {
                    start: 100,
                    end: 130,
                    notes: vec![note(62, 110, 120)],
                    controls: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn keeps_only_the_tracks_device() {
        let from = |record: MidiRecord, device: u32| match record {
            MidiRecord::Event { position, data, .. } => MidiRecord::Event {
                position,
                data,
                source: device,
                value: 0,
            },
            other => other,
        };
        let records = [
            from(on(10, 60), 1),
            from(on(12, 64), 2),
            from(off(20, 64), 2),
        ];
        let device = InputFilter {
            device: Some(2),
            channel: None,
        };
        let take = &takes(&records, span(0, 30), device)[0];
        // Notes remember their device, whose latency they're placed by.
        assert_eq!(
            take.notes,
            [TakeNote {
                source: 2,
                ..note(64, 12, 20)
            }]
        );
        assert_eq!(takes(&records, span(0, 30), any())[0].notes.len(), 2);
    }

    #[test]
    fn keeps_only_the_tracks_channel() {
        let on_channel = |position, pitch, channel: u8| MidiRecord::Event {
            position,
            data: [0x90 | channel, pitch, 100],
            source: 0,
            value: 0,
        };
        let records = [
            on_channel(10, 60, 0),
            on_channel(12, 64, 3),
            MidiRecord::Event {
                position: 20,
                data: [0x83, 64, 0],
                source: 0,
                value: 0,
            },
        ];
        let channel = InputFilter::new(None, Some(4));
        let take = &takes(&records, span(0, 30), channel)[0];
        assert_eq!(take.notes, [note(64, 12, 20)]);
    }

    #[test]
    fn records_controllers() {
        let bend = |position, value: u16| MidiRecord::Event {
            position,
            data: Controller::PitchBend.message(0, value),
            source: 0,
            value: 0,
        };
        let records = [
            bend(5, 9000),
            on(10, 60),
            MidiRecord::Event {
                position: 12,
                data: [0xb0, 1, 70],
                source: 0,
                value: 0,
            },
            // Channel mode messages aren't controllers.
            MidiRecord::Event {
                position: 13,
                data: [0xb0, 123, 0],
                source: 0,
                value: 0,
            },
            bend(15, 8192),
        ];
        let takes = takes(&records, span(0, 20), any());
        assert_eq!(
            takes[0].controls,
            [
                TakeControl {
                    frame: 5,
                    controller: Controller::PitchBend,
                    value: 9000,
                    source: 0,
                },
                TakeControl {
                    frame: 12,
                    controller: Controller::Cc { number: 1 },
                    value: 70,
                    source: 0,
                },
                TakeControl {
                    frame: 15,
                    controller: Controller::PitchBend,
                    value: 8192,
                    source: 0,
                },
            ]
        );
    }

    #[test]
    fn records_per_note_controllers() {
        let brightness = Controller::NoteController {
            pitch: 60,
            number: 74,
        };
        let records = [
            on(10, 60),
            MidiRecord::Event {
                position: 12,
                data: brightness.message(0, 0),
                source: 0,
                value: 0xc000_1234,
            },
        ];
        let takes = takes(&records, span(0, 20), any());
        assert_eq!(
            takes[0].controls,
            [TakeControl {
                frame: 12,
                controller: brightness,
                value: 0xc000,
                source: 0,
            }]
        );
    }

    #[test]
    fn a_per_note_reset_records_defaults() {
        let (volume, bend) = (
            Controller::NoteController {
                pitch: 60,
                number: 7,
            },
            Controller::NotePitchBend { pitch: 60 },
        );
        let event = |position, data, value| MidiRecord::Event {
            position,
            data,
            source: 0,
            value,
        };
        let manage = |position, reset| {
            let flags = PerNote::Management {
                detach: true,
                reset,
            };
            event(position, flags.data(0, 60), 0)
        };
        let records = [
            on(10, 60),
            event(12, volume.message(0, 0), 0x2000_0000),
            event(13, bend.message(0, 0), 0x9000_0000),
            // Another key's controller stays.
            event(14, Controller::NotePitchBend { pitch: 61 }.message(0, 0), 0),
            manage(15, false),
            manage(16, true),
        ];
        let controls: Vec<_> = takes(&records, span(0, 20), any())[0]
            .controls
            .iter()
            .map(|c| (c.frame, c.controller, c.value))
            .collect();
        assert_eq!(
            controls,
            [
                (12, volume, 0x2000),
                (13, bend, 0x9000),
                (14, Controller::NotePitchBend { pitch: 61 }, 0),
                (16, bend, 0x8000),
                (16, volume, u16::MAX),
            ]
        );
    }

    #[test]
    fn punch_keeps_notes_that_start_inside() {
        let records = [on(10, 60), on(50, 62), off(70, 60), off(200, 62)];
        let span = Span {
            punch: Some(TimeRange {
                start: 40,
                end: 100,
            }),
            ..span(0, 300)
        };
        assert_eq!(
            takes(&records, span, any()),
            [MidiTake {
                start: 40,
                end: 100,
                notes: vec![note(62, 50, 100)],
                controls: Vec::new(),
            }]
        );
    }
}
