// SPDX-License-Identifier: AGPL-3.0-or-later
//! Standard MIDI Files (`.mid`): notes, controllers, tempo and time
//! signature, with times in beats (quarter notes).

use std::path::Path;

use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};

use crate::MediaError;

/// What a MIDI file holds, ready to place on the timeline.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MidiFile {
    /// Tempo changes as (beat, BPM), sorted by beat. Empty if the file sets
    /// no tempo (MIDI then assumes 120 BPM).
    pub tempo: Vec<(f64, f64)>,
    /// Time signatures as (beat, numerator, denominator), sorted by beat.
    /// Empty if the file sets none (MIDI then assumes 4/4).
    pub meters: Vec<(f64, u8, u8)>,
    /// One per file track and channel that has notes, in file order.
    pub tracks: Vec<MidiFileTrack>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MidiFileTrack {
    /// The track's name in the file, or "Track N".
    pub name: String,
    /// MIDI channel, 0 to 15.
    pub channel: u8,
    /// Sorted by start.
    pub notes: Vec<MidiFileNote>,
    /// Control changes, pitch bend, channel and key pressure, and program
    /// changes, sorted by beat.
    pub controls: Vec<MidiFileControl>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MidiFileNote {
    pub pitch: u8,
    /// 1 to 127.
    pub velocity: u8,
    pub start_beats: f64,
    pub length_beats: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MidiFileControl {
    pub beat: f64,
    /// The MIDI 1.0 message (channel bits included; two-byte messages end
    /// in 0).
    pub data: [u8; 3],
}

/// Reads a `.mid` file.
pub fn read_midi_file(path: &Path) -> Result<MidiFile, MediaError> {
    let bytes = std::fs::read(path).map_err(|source| MediaError::Io {
        path: path.to_owned(),
        source,
    })?;
    parse_midi_file(&bytes).map_err(|reason| MediaError::Unsupported {
        path: path.to_owned(),
        reason,
    })
}

/// Parses the bytes of a `.mid` file.
pub fn parse_midi_file(bytes: &[u8]) -> Result<MidiFile, String> {
    let smf = Smf::parse(bytes).map_err(|e| format!("not a readable MIDI file ({e})"))?;
    // Ticks to beats. Timecode files count real time: place them at 120
    // BPM.
    let (per_beat, timecode) = match smf.header.timing {
        Timing::Metrical(ppq) => (f64::from(ppq.as_int().max(1)), false),
        Timing::Timecode(fps, sub) => (f64::from(fps.as_f32()) * f64::from(sub.max(1)) / 2.0, true),
    };
    let beat = |tick: u64| tick as f64 / per_beat;
    let mut file = MidiFile::default();
    if timecode {
        file.tempo.push((0.0, 120.0));
    }
    for (index, events) in smf.tracks.iter().enumerate() {
        let mut name = None;
        // Per channel: held notes (pitch, velocity, start tick), notes,
        // controls.
        let mut channels: Vec<Option<ChannelData>> = vec![None; 16];
        let mut tick = 0u64;
        for event in events {
            tick += u64::from(event.delta.as_int());
            match event.kind {
                TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int();
                    let data = channels[usize::from(ch)].get_or_insert_with(ChannelData::default);
                    data.message(ch, message, tick, &beat);
                }
                TrackEventKind::Meta(MetaMessage::Tempo(us)) if !timecode => {
                    let bpm = 60_000_000.0 / f64::from(us.as_int().max(1));
                    file.tempo.push((beat(tick), bpm.clamp(20.0, 999.0)));
                }
                TrackEventKind::Meta(MetaMessage::TimeSignature(num, den, ..)) if den <= 5 => {
                    file.meters.push((beat(tick), num.clamp(1, 32), 1 << den));
                }
                TrackEventKind::Meta(MetaMessage::TrackName(bytes)) if name.is_none() => {
                    let text = String::from_utf8_lossy(bytes).trim().to_owned();
                    if !text.is_empty() {
                        name = Some(text);
                    }
                }
                _ => {}
            }
        }
        let name = name.unwrap_or_else(|| format!("Track {}", index + 1));
        let used: Vec<(u8, ChannelData)> = channels
            .into_iter()
            .enumerate()
            .filter_map(|(ch, data)| Some((ch as u8, data?)))
            .map(|(ch, mut data)| {
                data.finish(tick, &beat);
                (ch, data)
            })
            .filter(|(_, data)| !data.notes.is_empty())
            .collect();
        let split = used.len() > 1;
        for (channel, data) in used {
            file.tracks.push(MidiFileTrack {
                name: if split {
                    format!("{name} (ch {})", channel + 1)
                } else {
                    name.clone()
                },
                channel,
                notes: data.notes,
                controls: data.controls,
            });
        }
    }
    file.tempo.sort_by(|a, b| a.0.total_cmp(&b.0));
    // The last change at a beat wins.
    file.tempo.reverse();
    file.tempo.dedup_by(|b, a| (a.0 - b.0).abs() < 1e-9);
    file.tempo.reverse();
    file.meters.sort_by(|a, b| a.0.total_cmp(&b.0));
    file.meters.reverse();
    file.meters.dedup_by(|b, a| (a.0 - b.0).abs() < 1e-9);
    file.meters.reverse();
    Ok(file)
}

#[derive(Debug, Clone, Default)]
struct ChannelData {
    held: Vec<(u8, u8, u64)>,
    notes: Vec<MidiFileNote>,
    controls: Vec<MidiFileControl>,
}

impl ChannelData {
    fn message(&mut self, ch: u8, message: MidiMessage, tick: u64, beat: &impl Fn(u64) -> f64) {
        match message {
            MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                self.held.push((key.as_int(), vel.as_int(), tick));
            }
            MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                // The earliest held note of this pitch ends.
                if let Some(i) = self.held.iter().position(|h| h.0 == key.as_int()) {
                    let (pitch, velocity, start) = self.held.remove(i);
                    self.end(pitch, velocity, start, tick, beat);
                }
            }
            MidiMessage::Controller { controller, value } => {
                self.control(beat(tick), [0xb0 | ch, controller.as_int(), value.as_int()]);
            }
            MidiMessage::PitchBend { bend } => {
                let v = bend.0.as_int();
                self.control(beat(tick), [0xe0 | ch, (v & 0x7f) as u8, (v >> 7) as u8]);
            }
            MidiMessage::ChannelAftertouch { vel } => {
                self.control(beat(tick), [0xd0 | ch, vel.as_int(), 0]);
            }
            MidiMessage::Aftertouch { key, vel } => {
                self.control(beat(tick), [0xa0 | ch, key.as_int(), vel.as_int()]);
            }
            MidiMessage::ProgramChange { program } => {
                self.control(beat(tick), [0xc0 | ch, program.as_int(), 0]);
            }
        }
    }

    fn control(&mut self, beat: f64, data: [u8; 3]) {
        self.controls.push(MidiFileControl { beat, data });
    }

    fn end(&mut self, pitch: u8, velocity: u8, start: u64, end: u64, beat: &impl Fn(u64) -> f64) {
        let start_beats = beat(start);
        // Zero-length notes get a 64th, so they still sound.
        let length_beats = (beat(end) - start_beats).max(1.0 / 16.0);
        self.notes.push(MidiFileNote {
            pitch,
            velocity: velocity.max(1),
            start_beats,
            length_beats,
        });
    }

    /// Ends notes still held at the end of the track, and sorts.
    fn finish(&mut self, end: u64, beat: &impl Fn(u64) -> f64) {
        for (pitch, velocity, start) in std::mem::take(&mut self.held) {
            self.end(pitch, velocity, start, end, beat);
        }
        self.notes.sort_by(|a, b| {
            a.start_beats
                .total_cmp(&b.start_beats)
                .then(a.pitch.cmp(&b.pitch))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use midly::num::{u4, u7, u15, u24, u28};
    use midly::{Format, Header, TrackEvent};

    fn event(delta: u32, kind: TrackEventKind<'static>) -> TrackEvent<'static> {
        TrackEvent {
            delta: u28::new(delta),
            kind,
        }
    }

    fn midi(channel: u8, message: MidiMessage) -> TrackEventKind<'static> {
        TrackEventKind::Midi {
            channel: u4::new(channel),
            message,
        }
    }

    fn on(key: u8, vel: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            key: u7::new(key),
            vel: u7::new(vel),
        }
    }

    #[test]
    fn reads_notes_controls_and_tempo() {
        let header = Header::new(Format::Parallel, Timing::Metrical(u15::new(480)));
        let mut smf = Smf::new(header);
        smf.tracks.push(vec![
            event(
                0,
                TrackEventKind::Meta(MetaMessage::Tempo(u24::new(500_000))),
            ),
            event(
                0,
                TrackEventKind::Meta(MetaMessage::TimeSignature(3, 2, 24, 8)),
            ),
            // 100 BPM from beat 4.
            event(
                1920,
                TrackEventKind::Meta(MetaMessage::Tempo(u24::new(600_000))),
            ),
            // 6/8 from beat 6 (bar 3).
            event(
                960,
                TrackEventKind::Meta(MetaMessage::TimeSignature(6, 3, 24, 8)),
            ),
            event(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ]);
        smf.tracks.push(vec![
            event(0, TrackEventKind::Meta(MetaMessage::TrackName(b"Piano"))),
            event(
                0,
                midi(
                    0,
                    MidiMessage::ProgramChange {
                        program: u7::new(5),
                    },
                ),
            ),
            event(0, midi(0, on(60, 100))),
            event(
                120,
                midi(
                    0,
                    MidiMessage::Aftertouch {
                        key: u7::new(60),
                        vel: u7::new(70),
                    },
                ),
            ),
            event(
                120,
                midi(
                    0,
                    MidiMessage::PitchBend {
                        bend: midly::PitchBend(midly::num::u14::new(10_000)),
                    },
                ),
            ),
            // Note on with velocity 0 is a note off.
            event(240, midi(0, on(60, 0))),
            event(0, midi(1, on(36, 90))),
            event(
                480,
                midi(
                    0,
                    MidiMessage::Controller {
                        controller: u7::new(64),
                        value: u7::new(127),
                    },
                ),
            ),
            // Never released: ends with the track.
            event(0, midi(0, on(64, 80))),
            event(960, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ]);
        let mut bytes = Vec::new();
        smf.write_std(&mut bytes).unwrap();
        let file = parse_midi_file(&bytes).unwrap();
        assert_eq!(file.tempo, [(0.0, 120.0), (4.0, 100.0)]);
        assert_eq!(file.meters, [(0.0, 3, 4), (6.0, 6, 8)]);
        let names: Vec<_> = file.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Piano (ch 1)", "Piano (ch 2)"]);
        let piano = &file.tracks[0];
        let notes: Vec<_> = piano
            .notes
            .iter()
            .map(|n| (n.pitch, n.velocity, n.start_beats, n.length_beats))
            .collect();
        assert_eq!(notes, [(60, 100, 0.0, 1.0), (64, 80, 2.0, 2.0)]);
        assert_eq!(
            piano.controls,
            [
                MidiFileControl {
                    beat: 0.0,
                    data: [0xc0, 5, 0],
                },
                MidiFileControl {
                    beat: 0.25,
                    data: [0xa0, 60, 70],
                },
                MidiFileControl {
                    beat: 0.5,
                    data: [0xe0, (10_000 & 0x7f) as u8, (10_000 >> 7) as u8],
                },
                MidiFileControl {
                    beat: 2.0,
                    data: [0xb0, 64, 127],
                },
            ]
        );
        assert_eq!(file.tracks[1].channel, 1);
        assert_eq!(file.tracks[1].notes[0].length_beats, 3.0);
        assert!(parse_midi_file(b"not midi").is_err());
    }
}
