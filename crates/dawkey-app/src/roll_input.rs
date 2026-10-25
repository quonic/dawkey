// SPDX-License-Identifier: AGPL-3.0-or-later
//! What clicks, drags, scrolling and keys do in the piano roll and the
//! synth bar. Note edits go through the MIDI commands, so each gesture is
//! one undo step.

use dawkey_core::{
    AddMidiClip, AddMidiNotes, ClipId, ControlValue, Controller, EditMidiNotes, NoteChange, NoteId,
    NoteSpec, QuantizeMidiNotes, RemoveMidiNotes, SetMidiControls, SetSynth, SetTrackMidiInput,
    SetTrackMidiOutput, TrackId, Waveform,
};
use dawkey_ui::{Modifiers, Point, Rect};

use crate::App;
use crate::state::{
    Gesture, KEYS_W, MAX_BEAT_ZOOM, MIN_BEAT_ZOOM, Page, ROLL_LANE_H, ROW_H, RollLane, RollView,
    SLIDER_W, SynthParam, TextField, latency_position, latency_value,
};
use crate::view::{lane_value, marquee_notes, note_views};

/// A note of the edited clip, in beats from the clip's start.
#[derive(Debug, Clone, Copy)]
struct Note {
    id: NoteId,
    pitch: u8,
    velocity: u8,
    start: f64,
    length: f64,
}

/// How much one pixel of ctrl-scroll changes the zoom.
const ZOOM_PER_PX: f32 = 0.004;

impl App {
    /// Opens a MIDI clip in the piano roll, zoomed to fit it and scrolled to
    /// its notes.
    pub fn open_piano_roll(&mut self, clip: ClipId) -> Result<(), String> {
        let project = self.studio.session().project();
        let (_, c) = project
            .clip(clip)
            .ok_or_else(|| format!("clip {clip} does not exist"))?;
        if c.kind() != dawkey_core::TrackKind::Midi {
            return Err(format!(
                "clip {clip} is an audio clip; only MIDI clips have notes"
            ));
        }
        let beats = project.tempo_map().clip_beats(c.start, c.length).max(1.0);
        let (lo, hi) = c.notes().iter().fold((u8::MAX, 0), |(lo, hi), n| {
            (lo.min(n.pitch), hi.max(n.pitch))
        });
        // Estimate the grid's width from the window; layout comes later.
        let width =
            (self.zoomed.size.w - 2.0 * self.zoomed.shadow_margin() - KEYS_W - 8.0).max(200.0);
        let top = if c.notes().is_empty() {
            84
        } else {
            // Center the notes, with at least two octaves around them.
            let mid = (i32::from(lo) + i32::from(hi)) / 2;
            (mid + 18).max(i32::from(hi) + 4).min(127)
        };
        self.view.piano_roll = RollView {
            px_per_beat: (width / beats as f32).clamp(MIN_BEAT_ZOOM, MAX_BEAT_ZOOM),
            scroll_beats: 0.0,
            scroll_y: (127 - top) as f32 * ROW_H,
            grid_beats: self.view.piano_roll.grid_beats,
        };
        self.view.page = Page::PianoRoll;
        self.view.editing_clip = Some(clip);
        self.view.selected_clip = Some(clip);
        self.view.selected_notes.clear();
        self.view.selected_control = None;
        Ok(())
    }

    /// Double-click on a MIDI lane: a one-bar clip there.
    pub(crate) fn create_midi_clip(&mut self, track: TrackId, x: f32) -> Result<(), String> {
        let position_s = self.snap_to_beat(self.time_at(x).max(0.0));
        let project = self.studio.session().project();
        let map = project.tempo_map();
        let start = project.seconds_to_frames(position_s);
        let bar = project
            .meter_map()
            .meter_at(map.beat_at(start as f64))
            .beats_per_bar();
        let bar_s = project.frames_to_seconds(map.clip_frames(start, bar));
        let outcome = self
            .studio
            .execute(
                AddMidiClip {
                    track: Some(track),
                    position_s,
                    length_s: Some(bar_s),
                    ..Default::default()
                }
                .into(),
            )
            .map_err(|e| e.to_string())?;
        self.view.selected_clip = outcome.clip;
        Ok(())
    }

    /// Handles clicks on the piano roll and the synth bar. Returns `None` if
    /// the widget isn't one of theirs.
    pub(crate) fn on_roll_click(
        &mut self,
        parts: &[&str],
        pos: Point,
        count: u8,
        modifiers: Modifiers,
    ) -> Option<Result<(), String>> {
        Some(match parts {
            ["piano_roll", "close"] => {
                self.view.page = Page::Arrange;
                Ok(())
            }
            ["piano_roll", "grid", division] => {
                let division: f64 = division.parse().ok()?;
                self.view.piano_roll.grid_beats = 4.0 / division;
                Ok(())
            }
            ["piano_roll", "quantize"] => self.quantize(),
            ["piano_roll", "ruler"] => {
                let clip = self.view.editing_clip?;
                let beat = self.beat_at(pos.x).max(0.0);
                let project = self.studio.session().project();
                let start = project.clip(clip).map(|(_, c)| c.start)?;
                let frames = project.tempo_map().clip_frames(start, beat);
                let t = project.frames_to_seconds(start + frames);
                self.studio.locate(t).map_err(|e| e.to_string())
            }
            // Double-clicking the latency slider types a value.
            ["track", track, "midi_out_latency"] if count == 2 => {
                let track = TrackId(track.parse().ok()?);
                self.start_text_edit(TextField::DeviceLatency(track))
            }
            ["piano_roll", "grid"] if count == 2 => self.add_note_at(pos),
            ["piano_roll", "grid"] => {
                self.view.selected_notes.clear();
                Ok(())
            }
            ["piano_roll", "lane_kind"] => {
                self.next_lane();
                Ok(())
            }
            ["piano_roll", "lane"] if count == 2 => self.add_control_at(pos),
            ["piano_roll", "lane"] => {
                self.view.selected_control = None;
                Ok(())
            }
            ["piano_roll", "point", index] => {
                self.view.selected_control = Some(index.parse().ok()?);
                Ok(())
            }
            ["note", id] => {
                let id = NoteId(id.parse().ok()?);
                let selected = &mut self.view.selected_notes;
                if modifiers.shift || modifiers.ctrl {
                    if let Some(i) = selected.iter().position(|n| *n == id) {
                        selected.remove(i);
                    } else {
                        selected.push(id);
                    }
                } else {
                    *selected = vec![id];
                }
                Ok(())
            }
            ["track", track, kind @ ("midi_in" | "midi_out")] => {
                let track = TrackId(track.parse().ok()?);
                self.next_device(track, *kind == "midi_out")
            }
            ["track", track, "midi_in_channel"] => {
                let track = TrackId(track.parse().ok()?);
                let t = self.studio.session().project().track(track)?;
                // Every channel, then 1 to 16.
                let channel = match t.midi_input_channel {
                    None => Some(1),
                    Some(16) => None,
                    Some(c) => Some(c + 1),
                };
                let device = t.midi_input.clone();
                self.execute(
                    SetTrackMidiInput {
                        track,
                        device,
                        channel,
                    }
                    .into(),
                )
            }
            ["track", track, "synth", wave] => {
                let waveform = match *wave {
                    "saw" => Waveform::Saw,
                    "square" => Waveform::Square,
                    "triangle" => Waveform::Triangle,
                    "sine" => Waveform::Sine,
                    _ => return None,
                };
                let track = TrackId(track.parse().ok()?);
                self.execute(
                    SetSynth {
                        waveform: Some(waveform),
                        ..SetSynth::new(track)
                    }
                    .into(),
                )
            }
            _ => return None,
        })
    }

    /// Starts a drag on a note, a note's end, a velocity bar, a controller
    /// point, the grid (a marquee), the lane (painting), or a synth slider.
    pub(crate) fn start_roll_gesture(
        &mut self,
        parts: &[&str],
        origin: Point,
        modifiers: Modifiers,
        merge_key: String,
    ) -> Option<Result<Option<Gesture>, String>> {
        Some(Ok(match parts {
            ["piano_roll", "grid"] => {
                let clip = self.view.editing_clip?;
                let at = (self.beat_at(origin.x), self.pitch_at(origin.y));
                let base = if modifiers.shift || modifiers.ctrl {
                    self.view.selected_notes.clone()
                } else {
                    Vec::new()
                };
                self.view.selected_notes = base.clone();
                Some(Gesture::Marquee {
                    clip,
                    base,
                    from: at,
                    to: at,
                })
            }
            ["note", id, "velocity"] => {
                let clip = self.view.editing_clip?;
                let id = NoteId(id.parse().ok()?);
                let ids = if self.view.selected_notes.contains(&id) {
                    self.view.selected_notes.clone()
                } else {
                    vec![id]
                };
                let notes = ids
                    .iter()
                    .filter_map(|id| self.note(*id))
                    .map(|n| (n.id, n.velocity))
                    .collect();
                Some(Gesture::Velocity {
                    clip,
                    notes,
                    delta: 0,
                })
            }
            ["piano_roll", "point", index] => {
                let clip = self.view.editing_clip?;
                let controller = self.view.roll_lane.controller()?;
                let index: usize = index.parse().ok()?;
                let (beat, value) = *self.control_points(controller).get(index)?;
                self.view.selected_control = Some(index);
                Some(Gesture::ControlPoint {
                    clip,
                    controller,
                    index,
                    beat,
                    value,
                })
            }
            ["piano_roll", "lane"] => {
                let clip = self.view.editing_clip?;
                let lane = self.view.roll_lane;
                let start = (
                    self.beat_at(origin.x).max(0.0),
                    self.roll_lane_value_at(origin.y),
                );
                Some(Gesture::Paint {
                    clip,
                    lane,
                    path: vec![start],
                })
            }
            ["note", id] => {
                let clip = self.view.editing_clip?;
                let id = NoteId(id.parse().ok()?);
                let note = self.note(id)?;
                if !self.view.selected_notes.contains(&id) {
                    self.view.selected_notes = vec![id];
                }
                Some(Gesture::MoveNotes {
                    clip,
                    notes: self.view.selected_notes.clone(),
                    origin_beats: self.beat_at(origin.x),
                    origin_pitch: self.pitch_at(origin.y),
                    anchor_beats: note.start,
                    delta_beats: 0.0,
                    delta_pitch: 0,
                })
            }
            ["note", id, "end"] => {
                let clip = self.view.editing_clip?;
                let note = self.note(NoteId(id.parse().ok()?))?;
                self.view.selected_notes = vec![note.id];
                Some(Gesture::ResizeNote {
                    clip,
                    note: note.id,
                    start_beats: note.start,
                    length_beats: note.length,
                })
            }
            ["track", track, "synth", param] => {
                let param = SynthParam::from_name(param)?;
                let track = TrackId(track.parse().ok()?);
                let synth = self.studio.session().project().track(track)?.synth?;
                Some(Gesture::SynthParam {
                    track,
                    param,
                    start_pos: param.position(param.get(&synth)),
                    merge_key,
                })
            }
            ["track", track, "midi_out_latency"] => {
                let track = TrackId(track.parse().ok()?);
                let output = self
                    .studio
                    .session()
                    .project()
                    .track(track)?
                    .midi_output
                    .as_ref()?;
                Some(Gesture::DeviceLatency {
                    track,
                    start_pos: latency_position(output.latency_ms),
                    merge_key,
                })
            }
            _ => return None,
        }))
    }

    /// Continues or ends a piano roll gesture. Returns `None` for other
    /// gestures.
    pub(crate) fn on_roll_drag(
        &mut self,
        gesture: &Gesture,
        origin: Point,
        pos: Point,
        end: bool,
        modifiers: Modifiers,
    ) -> Option<Result<(), String>> {
        let snap = self.view.snap && !modifiers.shift;
        Some(match gesture.clone() {
            Gesture::MoveNotes {
                clip,
                notes,
                origin_beats,
                origin_pitch,
                anchor_beats,
                ..
            } => {
                let raw = self.beat_at(pos.x) - origin_beats;
                let target = anchor_beats + raw;
                let target = if snap {
                    self.round_to_grid(target)
                } else {
                    target
                };
                // Nothing moves before the clip's start.
                let earliest = notes
                    .iter()
                    .filter_map(|id| self.note(*id))
                    .map(|n| n.start)
                    .fold(f64::INFINITY, f64::min);
                let delta_beats = (target - anchor_beats).max(-earliest.min(anchor_beats));
                // Nor off the top or bottom of the keyboard: the chord keeps
                // its shape.
                let (lowest, highest) = notes
                    .iter()
                    .filter_map(|id| self.note(*id))
                    .fold((127, 0), |(lo, hi), n| {
                        (lo.min(i32::from(n.pitch)), hi.max(i32::from(n.pitch)))
                    });
                let delta_pitch =
                    (self.pitch_at(pos.y) - origin_pitch).clamp(-lowest, 127 - highest);
                if !end {
                    self.view.gesture = Some(Gesture::MoveNotes {
                        clip,
                        notes,
                        origin_beats,
                        origin_pitch,
                        anchor_beats,
                        delta_beats,
                        delta_pitch,
                    });
                    return Some(Ok(()));
                }
                self.view.gesture = None;
                if delta_beats.abs() < 1e-9 && delta_pitch == 0 {
                    return Some(Ok(()));
                }
                let changes: Vec<NoteChange> = notes
                    .iter()
                    .filter_map(|id| self.note(*id))
                    .map(|n| NoteChange {
                        id: n.id,
                        pitch: Some((i32::from(n.pitch) + delta_pitch).clamp(0, 127) as u8),
                        start_beats: Some((n.start + delta_beats).max(0.0)),
                        length_beats: None,
                        velocity: None,
                    })
                    .collect();
                self.execute(
                    EditMidiNotes {
                        clip,
                        notes: changes,
                    }
                    .into(),
                )
            }
            Gesture::ResizeNote {
                clip,
                note,
                start_beats,
                ..
            } => {
                let end_beats = self.beat_at(pos.x);
                let end_beats = if snap {
                    self.round_to_grid(end_beats)
                } else {
                    end_beats
                };
                let min = if snap {
                    self.view.piano_roll.grid_beats
                } else {
                    1.0 / 64.0
                };
                let length_beats = (end_beats - start_beats).max(min);
                if !end {
                    self.view.gesture = Some(Gesture::ResizeNote {
                        clip,
                        note,
                        start_beats,
                        length_beats,
                    });
                    return Some(Ok(()));
                }
                self.view.gesture = None;
                self.execute(
                    EditMidiNotes {
                        clip,
                        notes: vec![NoteChange {
                            id: note,
                            pitch: None,
                            start_beats: None,
                            length_beats: Some(length_beats),
                            velocity: None,
                        }],
                    }
                    .into(),
                )
            }
            Gesture::Marquee {
                clip, base, from, ..
            } => {
                let to = (self.beat_at(pos.x), self.pitch_at(pos.y));
                let notes = self.edited_note_views();
                let mut selected = base.clone();
                for id in marquee_notes(&notes, from, to) {
                    if !selected.contains(&id) {
                        selected.push(id);
                    }
                }
                self.view.selected_notes = selected;
                self.view.gesture = (!end).then_some(Gesture::Marquee {
                    clip,
                    base,
                    from,
                    to,
                });
                Ok(())
            }
            Gesture::Velocity { clip, notes, .. } => {
                // The lane's full height covers the whole range.
                let delta = ((origin.y - pos.y) / (ROLL_LANE_H - 9.0) * 126.0).round() as i32;
                if !end {
                    self.view.gesture = Some(Gesture::Velocity { clip, notes, delta });
                    return Some(Ok(()));
                }
                self.view.gesture = None;
                if delta == 0 {
                    return Some(Ok(()));
                }
                let changes = notes
                    .iter()
                    .map(|&(id, v)| NoteChange {
                        id,
                        pitch: None,
                        start_beats: None,
                        length_beats: None,
                        velocity: Some((i32::from(v) + delta).clamp(1, 127) as u8),
                    })
                    .collect();
                self.execute(
                    EditMidiNotes {
                        clip,
                        notes: changes,
                    }
                    .into(),
                )
            }
            Gesture::ControlPoint {
                clip,
                controller,
                index,
                ..
            } => {
                let beat = self.beat_at(pos.x).max(0.0);
                let beat = if snap { self.round_to_grid(beat) } else { beat };
                let value = self.roll_lane_value_at(pos.y);
                if !end {
                    self.view.gesture = Some(Gesture::ControlPoint {
                        clip,
                        controller,
                        index,
                        beat,
                        value,
                    });
                    return Some(Ok(()));
                }
                self.view.gesture = None;
                let mut points = self.control_points(controller);
                if index >= points.len() {
                    return Some(Ok(()));
                }
                // It takes the place of any change already at that time.
                points.remove(index);
                points.retain(|(b, _)| (b - beat).abs() > 1e-6);
                points.push((beat, value));
                points.sort_by(|a, b| a.0.total_cmp(&b.0));
                self.view.selected_control = None;
                self.set_controls(clip, controller, points)
            }
            Gesture::Paint {
                clip,
                lane,
                mut path,
            } => {
                path.push((self.beat_at(pos.x).max(0.0), self.roll_lane_value_at(pos.y)));
                if !end {
                    self.view.gesture = Some(Gesture::Paint { clip, lane, path });
                    return Some(Ok(()));
                }
                self.view.gesture = None;
                self.apply_paint(clip, lane, &path, snap)
            }
            Gesture::SynthParam {
                track,
                param,
                start_pos,
                merge_key,
            } => {
                let p = start_pos + (pos.x - origin.x) / SLIDER_W;
                let command = param.command(track, param.value(p));
                let result = self.execute_merging(command.into(), &merge_key);
                if end {
                    self.view.gesture = None;
                    self.studio.end_merge();
                }
                result
            }
            Gesture::DeviceLatency {
                track,
                start_pos,
                merge_key,
            } => {
                let output = self
                    .studio
                    .session()
                    .project()
                    .track(track)
                    .and_then(|t| t.midi_output.clone());
                let result = match output {
                    Some(output) => {
                        let p = start_pos + (pos.x - origin.x) / SLIDER_W;
                        let command = SetTrackMidiOutput {
                            track,
                            device: Some(output.device),
                            channel: Some(output.channel),
                            latency_ms: Some(latency_value(p)),
                        };
                        self.execute_merging(command.into(), &merge_key)
                    }
                    None => Ok(()),
                };
                if end {
                    self.view.gesture = None;
                    self.studio.end_merge();
                }
                result
            }
            _ => return None,
        })
    }

    /// Scrolling over the piano roll: vertical moves through pitches, shift
    /// scrolls time, ctrl zooms time around the pointer.
    pub(crate) fn on_roll_scroll(&mut self, pos: Point, delta: Point, modifiers: Modifiers) {
        let grid = self.grid_rect();
        let roll = &mut self.view.piano_roll;
        if modifiers.ctrl {
            let anchor = roll.scroll_beats + f64::from((pos.x - grid.x) / roll.px_per_beat);
            roll.px_per_beat = (roll.px_per_beat * (-delta.y * ZOOM_PER_PX).exp())
                .clamp(MIN_BEAT_ZOOM, MAX_BEAT_ZOOM);
            roll.scroll_beats = (anchor - f64::from((pos.x - grid.x) / roll.px_per_beat)).max(0.0);
            return;
        }
        let (dx, dy) = if modifiers.shift {
            (delta.y, 0.0)
        } else {
            (delta.x, delta.y)
        };
        roll.scroll_beats = (roll.scroll_beats + f64::from(dx / roll.px_per_beat)).max(0.0);
        let max = (128.0 * ROW_H - grid.h).max(0.0);
        roll.scroll_y = (roll.scroll_y + dy).clamp(0.0, max);
    }

    /// Keys in the piano roll. Returns `None` for keys it doesn't use.
    pub(crate) fn on_roll_key(&mut self, key: &str, m: Modifiers) -> Option<Result<(), String>> {
        let clip = self.view.editing_clip?;
        let selected = self.view.selected_notes.clone();
        let step = self.view.piano_roll.grid_beats;
        Some(match (key, m.ctrl) {
            ("delete" | "backspace", false)
                if selected.is_empty() && self.view.selected_control.is_some() =>
            {
                let index = self.view.selected_control.take().unwrap_or_default();
                let Some(controller) = self.view.roll_lane.controller() else {
                    return Some(Ok(()));
                };
                let mut points = self.control_points(controller);
                if index < points.len() {
                    points.remove(index);
                }
                self.set_controls(clip, controller, points)
            }
            ("delete" | "backspace", false) if !selected.is_empty() => {
                self.view.selected_notes.clear();
                self.execute(
                    RemoveMidiNotes {
                        clip,
                        notes: selected,
                    }
                    .into(),
                )
            }
            ("up" | "down", false) if !selected.is_empty() => {
                let by = if m.shift { 12 } else { 1 } * if key == "up" { 1 } else { -1 };
                self.shift_selected(clip, &selected, 0.0, by)
            }
            ("left" | "right", false) if !selected.is_empty() => {
                let by = if key == "right" { step } else { -step };
                self.shift_selected(clip, &selected, by, 0)
            }
            ("a", true) => {
                self.view.selected_notes = self.notes().iter().map(|n| n.id).collect();
                Ok(())
            }
            ("q", false) => self.quantize(),
            ("escape", false) => {
                if selected.is_empty() {
                    self.view.page = Page::Arrange;
                } else {
                    self.view.selected_notes.clear();
                }
                Ok(())
            }
            _ => return None,
        })
    }

    /// Moves the selected notes by `beats` and `pitch`, unless that would
    /// push one off the grid.
    fn shift_selected(
        &mut self,
        clip: ClipId,
        selected: &[NoteId],
        beats: f64,
        pitch: i32,
    ) -> Result<(), String> {
        let notes: Vec<Note> = selected.iter().filter_map(|id| self.note(*id)).collect();
        let fits = notes.iter().all(|n| {
            let p = i32::from(n.pitch) + pitch;
            (0..=127).contains(&p) && n.start + beats > -1e-9
        });
        if !fits || notes.is_empty() {
            return Ok(());
        }
        let changes = notes
            .iter()
            .map(|n| NoteChange {
                id: n.id,
                pitch: (pitch != 0).then(|| (i32::from(n.pitch) + pitch) as u8),
                start_beats: (beats != 0.0).then(|| (n.start + beats).max(0.0)),
                length_beats: None,
                velocity: None,
            })
            .collect();
        self.execute(
            EditMidiNotes {
                clip,
                notes: changes,
            }
            .into(),
        )
    }

    /// Quantizes the selected notes, or all of them, to the grid.
    fn quantize(&mut self) -> Result<(), String> {
        let Some(clip) = self.view.editing_clip else {
            return Ok(());
        };
        if self.notes().is_empty() {
            return Ok(());
        }
        let notes =
            (!self.view.selected_notes.is_empty()).then(|| self.view.selected_notes.clone());
        self.execute(
            QuantizeMidiNotes {
                clip,
                grid_beats: self.view.piano_roll.grid_beats,
                notes,
            }
            .into(),
        )
    }

    /// Double-click on the grid: a note one grid step long, starting at the
    /// grid line left of the pointer.
    fn add_note_at(&mut self, pos: Point) -> Result<(), String> {
        let Some(clip) = self.view.editing_clip else {
            return Ok(());
        };
        let grid = self.view.piano_roll.grid_beats;
        let beat = self.beat_at(pos.x).max(0.0);
        let start = if self.view.snap {
            (beat / grid).floor() * grid
        } else {
            beat
        };
        let pitch = self.pitch_at(pos.y);
        if !(0..=127).contains(&pitch) {
            return Ok(());
        }
        let outcome = self
            .studio
            .execute(
                AddMidiNotes {
                    clip,
                    notes: vec![NoteSpec {
                        pitch: pitch as u8,
                        start_beats: start,
                        length_beats: grid,
                        velocity: 100,
                    }],
                }
                .into(),
            )
            .map_err(|e| e.to_string())?;
        self.view.selected_notes = outcome.notes;
        Ok(())
    }

    /// The edited clip's notes, in beats.
    fn notes(&self) -> Vec<Note> {
        let Some(clip) = self.view.editing_clip else {
            return Vec::new();
        };
        let project = self.studio.session().project();
        let map = project.tempo_map();
        project.clip(clip).map_or_else(Vec::new, |(_, c)| {
            c.notes()
                .iter()
                .map(|n| {
                    let start = map.clip_beats(c.start, n.start);
                    Note {
                        id: n.id,
                        pitch: n.pitch,
                        velocity: n.velocity,
                        start,
                        length: map.clip_beats(c.start, n.end()) - start,
                    }
                })
                .collect()
        })
    }

    fn note(&self, id: NoteId) -> Option<Note> {
        self.notes().into_iter().find(|n| n.id == id)
    }

    /// The edited clip's notes as the view sees them.
    fn edited_note_views(&self) -> Vec<crate::view::NoteView> {
        let project = self.studio.session().project();
        self.view
            .editing_clip
            .and_then(|id| project.clip(id))
            .map(|(_, c)| note_views(project, c))
            .unwrap_or_default()
    }

    /// Picks the next MIDI device for a track: every device (input) or its
    /// own instrument (output) first, then each connected device.
    fn next_device(&mut self, track: TrackId, output: bool) -> Result<(), String> {
        let project = self.studio.session().project();
        let Some(t) = project.track(track) else {
            return Ok(());
        };
        let current = if output {
            t.midi_output.as_ref().map(|o| o.device.clone())
        } else {
            t.midi_input.clone()
        };
        let channel = t.midi_output.as_ref().map(|o| o.channel);
        let input_channel = t.midi_input_channel;
        let (inputs, outputs) = self.studio.midi_devices();
        let mut choices: Vec<Option<String>> = vec![None];
        choices.extend(
            (if output { outputs } else { inputs })
                .into_iter()
                .map(Some),
        );
        if !choices.contains(&current) {
            choices.push(current.clone());
        }
        let at = choices.iter().position(|c| *c == current).unwrap_or(0);
        let device = choices[(at + 1) % choices.len()].clone();
        if output {
            self.execute(
                SetTrackMidiOutput {
                    track,
                    channel: device.as_ref().and(channel),
                    device,
                    latency_ms: None,
                }
                .into(),
            )
        } else {
            self.execute(
                SetTrackMidiInput {
                    track,
                    device,
                    channel: input_channel,
                }
                .into(),
            )
        }
    }

    /// Shows the next kind of lane under the grid: velocity, the common
    /// controllers, then any others the clip uses.
    fn next_lane(&mut self) {
        let mut lanes = RollLane::COMMON.to_vec();
        if let Some((_, c)) = self
            .view
            .editing_clip
            .and_then(|id| self.studio.session().project().clip(id))
        {
            let mut used: Vec<Controller> = c.controls().iter().map(|p| p.controller).collect();
            used.sort_unstable();
            used.dedup();
            for controller in used {
                let lane = RollLane::Control { controller };
                if !lanes.contains(&lane) {
                    lanes.push(lane);
                }
            }
        }
        let at = lanes.iter().position(|l| *l == self.view.roll_lane);
        self.view.roll_lane = lanes[at.map_or(0, |i| (i + 1) % lanes.len())];
        self.view.selected_control = None;
    }

    /// One controller's changes in the edited clip, as (beat, value), in
    /// order.
    fn control_points(&self, controller: Controller) -> Vec<(f64, u16)> {
        let project = self.studio.session().project();
        let map = project.tempo_map();
        self.view
            .editing_clip
            .and_then(|id| project.clip(id))
            .map(|(_, c)| {
                c.controls()
                    .iter()
                    .filter(|p| p.controller == controller)
                    .map(|p| (map.clip_beats(c.start, p.frame), p.value))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn set_controls(
        &mut self,
        clip: ClipId,
        controller: Controller,
        points: Vec<(f64, u16)>,
    ) -> Result<(), String> {
        self.execute(
            SetMidiControls {
                clip,
                controller,
                points: points
                    .into_iter()
                    .map(|(time_beats, value)| ControlValue {
                        time_beats: time_beats.max(0.0),
                        value,
                    })
                    .collect(),
            }
            .into(),
        )
    }

    /// Double-click on a controller lane: a change there (on the grid line
    /// left of the pointer when snapping).
    fn add_control_at(&mut self, pos: Point) -> Result<(), String> {
        let (Some(clip), Some(controller)) =
            (self.view.editing_clip, self.view.roll_lane.controller())
        else {
            return Ok(());
        };
        let beat = self.beat_at(pos.x).max(0.0);
        let grid = self.view.piano_roll.grid_beats;
        let beat = if self.view.snap {
            (beat / grid).floor() * grid
        } else {
            beat
        };
        let mut points = self.control_points(controller);
        points.retain(|(b, _)| (b - beat).abs() > 1e-6);
        points.push((beat, self.roll_lane_value_at(pos.y)));
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.set_controls(clip, controller, points)
    }

    /// A drawn stroke across the lane. Velocities: each note starting under
    /// the stroke takes its value there. Controllers: the stroke's values
    /// replace the changes under it, one per grid step when snapping.
    fn apply_paint(
        &mut self,
        clip: ClipId,
        lane: RollLane,
        path: &[(f64, u16)],
        snap: bool,
    ) -> Result<(), String> {
        let (lo, hi) = path
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &(b, _)| {
                (lo.min(b), hi.max(b))
            });
        // The stroke's value at a beat: the nearest point of the stroke.
        let value_at = |beat: f64| {
            path.iter()
                .min_by(|a, b| (a.0 - beat).abs().total_cmp(&(b.0 - beat).abs()))
                .map_or(0, |p| p.1)
        };
        match lane.controller() {
            None => {
                let changes: Vec<NoteChange> = self
                    .notes()
                    .iter()
                    .filter(|n| n.start >= lo && n.start <= hi)
                    .map(|n| NoteChange {
                        id: n.id,
                        pitch: None,
                        start_beats: None,
                        length_beats: None,
                        velocity: Some(value_at(n.start).clamp(1, 127) as u8),
                    })
                    .collect();
                if changes.is_empty() {
                    return Ok(());
                }
                self.execute(
                    EditMidiNotes {
                        clip,
                        notes: changes,
                    }
                    .into(),
                )
            }
            Some(controller) => {
                let grid = self.view.piano_roll.grid_beats;
                let new: Vec<(f64, u16)> = if snap {
                    let (first, last) = ((lo / grid).round() as i64, (hi / grid).round() as i64);
                    (first..=last)
                        .map(|step| {
                            let beat = step as f64 * grid;
                            (beat, value_at(beat))
                        })
                        .collect()
                } else {
                    let mut points = path.to_vec();
                    points.sort_by(|a, b| a.0.total_cmp(&b.0));
                    points.dedup_by(|b, a| (a.0 - b.0).abs() < 1e-3);
                    points
                };
                let (from, to) = (
                    new.first().map_or(lo, |p| p.0),
                    new.last().map_or(hi, |p| p.0),
                );
                let mut points = self.control_points(controller);
                points.retain(|&(b, _)| b < from - 1e-6 || b > to + 1e-6);
                points.extend(new);
                points.sort_by(|a, b| a.0.total_cmp(&b.0));
                self.set_controls(clip, controller, points)
            }
        }
    }

    /// The lane's value at window y.
    fn roll_lane_value_at(&self, y: f32) -> u16 {
        let top = self
            .ui
            .find("piano_roll/lane")
            .map(|n| n.rect().y)
            .unwrap_or_default();
        lane_value(self.view.roll_lane, y - top)
    }

    fn grid_rect(&self) -> Rect {
        self.ui
            .find("piano_roll/grid")
            .map(|n| n.rect())
            .unwrap_or_default()
    }

    /// Beat (from the clip's start) at window x.
    fn beat_at(&self, x: f32) -> f64 {
        let roll = &self.view.piano_roll;
        roll.scroll_beats + f64::from((x - self.grid_rect().x) / roll.px_per_beat)
    }

    /// Pitch at window y (may be out of range).
    fn pitch_at(&self, y: f32) -> i32 {
        let row = ((y - self.grid_rect().y + self.view.piano_roll.scroll_y) / ROW_H).floor();
        127 - row as i32
    }

    fn round_to_grid(&self, beats: f64) -> f64 {
        let grid = self.view.piano_roll.grid_beats;
        (beats / grid).round() * grid
    }
}
