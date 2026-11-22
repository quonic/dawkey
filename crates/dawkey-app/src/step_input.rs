// SPDX-License-Identifier: AGPL-3.0-or-later
//! What clicks do in the drum step sequencer: toggling a step is one
//! `add_midi_notes`/`remove_midi_notes` call, like any MIDI edit, so it's
//! one undo step and the same path an AI client would use over MCP.

use dawkey_core::{
    AddMidiNotes, ApplyDrumKit, ApplyDrumPattern, ClipId, DrumKit, NoteSpec, RemoveMidiNotes,
    SetTimeSignature,
};
use dawkey_ui::{Modifiers, Point};

use crate::App;
use crate::state::{Page, TextField};
use crate::view::step_sequencer::{METERS, PAD_NOTES, pattern_from_id};

/// Default velocity for a step clicked on.
const DEFAULT_VELOCITY: u8 = 100;

impl App {
    /// Opens a drum machine clip in the step sequencer.
    pub fn open_step_sequencer(&mut self, clip: ClipId) -> Result<(), String> {
        let project = self.studio.session().project();
        let (_, c) = project
            .clip(clip)
            .ok_or_else(|| format!("clip {clip} does not exist"))?;
        if c.kind() != dawkey_core::TrackKind::Midi {
            return Err(format!(
                "clip {clip} is an audio clip; only MIDI clips have notes"
            ));
        }
        self.view.page = Page::StepSequencer;
        self.view.editing_clip = Some(clip);
        self.view.selected_clip = Some(clip);
        self.view.selected_notes.clear();
        self.view.selected_control = None;
        Ok(())
    }

    /// Handles clicks in the step sequencer. Returns `None` if the widget
    /// isn't one of theirs.
    pub(crate) fn on_step_click(
        &mut self,
        parts: &[&str],
        _pos: Point,
        _count: u8,
        _modifiers: Modifiers,
    ) -> Option<Result<(), String>> {
        Some(match parts {
            ["step_seq", "close"] => {
                self.view.page = Page::Arrange;
                Ok(())
            }
            ["step_seq", "save_as"] => {
                let clip = self.view.editing_clip?;
                self.start_text_edit(TextField::DrumPatternName(clip))
            }
            ["step_seq", "kit", name] => {
                let kit = match *name {
                    "808" => DrumKit::Kit808,
                    "909" => DrumKit::Kit909,
                    "natural" => DrumKit::Natural,
                    "tape" => DrumKit::Tape,
                    "trap" => DrumKit::Trap,
                    "metal" => DrumKit::Metal,
                    _ => return None,
                };
                let clip = self.view.editing_clip?;
                let track = self.studio.session().project().clip(clip)?.0.id;
                self.execute(ApplyDrumKit { track, kit }.into())
            }
            ["step_seq", "sound", name] => {
                let kit = match *name {
                    "synth" => None,
                    "acoustic_vcsl" => Some(dawkey_studio::SampleKit::AcousticVcsl),
                    "jazz_virtuosity" => Some(dawkey_studio::SampleKit::JazzVirtuosity),
                    "lofi_virtuosity" => Some(dawkey_studio::SampleKit::LofiVirtuosity),
                    "vintage_vcsl" => Some(dawkey_studio::SampleKit::VintageVcsl),
                    "latin_vcsl" => Some(dawkey_studio::SampleKit::LatinVcsl),
                    "house" => Some(dawkey_studio::SampleKit::House),
                    _ => return None,
                };
                let clip = self.view.editing_clip?;
                let plugin = self
                    .studio
                    .session()
                    .project()
                    .clip(clip)?
                    .0
                    .plugins
                    .first()?
                    .id;
                self.studio
                    .set_drum_sample_kit(plugin, kit)
                    .map_err(|e| e.to_string())
            }
            ["step_seq", "pattern", name] => {
                let pattern = pattern_from_id(name)?;
                let clip = self.view.editing_clip?;
                self.execute(ApplyDrumPattern { clip, pattern }.into())
            }
            ["step_seq", "meter", slug] => {
                let &(_, numerator, denominator) = METERS.iter().find(|&&(id, ..)| id == *slug)?;
                let clip = self.view.editing_clip?;
                let project = self.studio.session().project();
                let (_, clip_data) = project.clip(clip)?;
                let bar = project
                    .meter_map()
                    .bar_at(project.tempo_map().beat_at(clip_data.start as f64) + 1e-9)
                    .saturating_add(1);
                let at_bar = (bar > 1).then_some(bar);
                self.execute(
                    SetTimeSignature {
                        numerator,
                        denominator,
                        at_bar,
                    }
                    .into(),
                )
            }
            ["step_seq", "saved", index] => {
                let index: usize = index.parse().ok()?;
                let name = self
                    .studio
                    .session()
                    .project()
                    .drum_patterns
                    .get(index)?
                    .name
                    .clone();
                let clip = self.view.editing_clip?;
                self.execute(dawkey_core::LoadSavedDrumPattern { clip, name }.into())
            }
            ["step_seq", "remove_saved", index] => {
                let index: usize = index.parse().ok()?;
                let name = self
                    .studio
                    .session()
                    .project()
                    .drum_patterns
                    .get(index)?
                    .name
                    .clone();
                self.execute(dawkey_core::RemoveSavedDrumPattern { name }.into())
            }
            ["step_seq", "pad", pad, "output"] => {
                let pad_index: usize = pad.parse().ok()?;
                let clip = self.view.editing_clip?;
                let plugin = self
                    .studio
                    .session()
                    .project()
                    .clip(clip)?
                    .0
                    .plugins
                    .first()?
                    .id;
                self.cycle_plugin_output(plugin, pad_index as u32)
            }
            ["step", pad, n] => {
                let pad_index: usize = pad.parse().ok()?;
                let step: u16 = n.parse().ok()?;
                let pad_note = *PAD_NOTES.get(pad_index)?;
                self.toggle_step(pad_note, step)
            }
            _ => return None,
        })
    }

    fn toggle_step(&mut self, pad_note: u8, step: u16) -> Result<(), String> {
        let clip = self
            .view
            .editing_clip
            .ok_or("no drum clip is being edited")?;
        let project = self.studio.session().project();
        let (_, c) = project
            .clip(clip)
            .ok_or_else(|| format!("clip {clip} does not exist"))?;
        let map = project.tempo_map();
        let sig = project
            .meter_map()
            .meter_at(map.beat_at(c.start as f64) + 1e-9);
        let steps = sig.steps_per_bar();
        if step >= steps {
            return Err(format!("step {step} is past the bar's {steps} steps"));
        }
        let step_beats = sig.step_beats();
        let target = map.clip_frames(c.start, f64::from(step) * step_beats);
        let existing = c
            .notes()
            .iter()
            .find(|n| n.pitch == pad_note && n.start == target)
            .map(|n| n.id);
        match existing {
            Some(id) => self.execute(
                RemoveMidiNotes {
                    clip,
                    notes: vec![id],
                }
                .into(),
            ),
            None => self.execute(
                AddMidiNotes {
                    clip,
                    notes: vec![NoteSpec {
                        pitch: pad_note,
                        start_beats: f64::from(step) * step_beats,
                        length_beats: step_beats,
                        velocity: DEFAULT_VELOCITY,
                    }],
                }
                .into(),
            ),
        }
    }
}
