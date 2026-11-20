// SPDX-License-Identifier: AGPL-3.0-or-later
//! What clicks do in the drum step sequencer: toggling a step is one
//! `add_midi_notes`/`remove_midi_notes` call, like any MIDI edit, so it's
//! one undo step and the same path an AI client would use over MCP.

use dawkey_core::{
    AddMidiNotes, ApplyDrumKit, ApplyDrumPattern, ClipId, DrumKit, DrumPattern, NoteSpec,
    RemoveMidiNotes,
};
use dawkey_ui::{Modifiers, Point};

use crate::App;
use crate::state::{Page, TextField};
use crate::view::step_sequencer::{PAD_NOTES, STEP_BEATS};

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
                    "acoustic" => DrumKit::Acoustic,
                    "lofi" => DrumKit::LoFi,
                    "trap" => DrumKit::Trap,
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
                let pattern = match *name {
                    "four_on_the_floor" => DrumPattern::FourOnTheFloor,
                    "boom_bap" => DrumPattern::BoomBap,
                    "trap" => DrumPattern::Trap,
                    "house_shuffle" => DrumPattern::HouseShuffle,
                    "drum_and_bass" => DrumPattern::DrumAndBass,
                    _ => return None,
                };
                let clip = self.view.editing_clip?;
                self.execute(ApplyDrumPattern { clip, pattern }.into())
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
            ["step", pad, n] => {
                let pad_index: usize = pad.parse().ok()?;
                let step: u8 = n.parse().ok()?;
                let pad_note = *PAD_NOTES.get(pad_index)?;
                self.toggle_step(pad_note, step)
            }
            _ => return None,
        })
    }

    fn toggle_step(&mut self, pad_note: u8, step: u8) -> Result<(), String> {
        let clip = self
            .view
            .editing_clip
            .ok_or("no drum clip is being edited")?;
        let project = self.studio.session().project();
        let (_, c) = project
            .clip(clip)
            .ok_or_else(|| format!("clip {clip} does not exist"))?;
        let map = project.tempo_map();
        let target = map.clip_frames(c.start, f64::from(step) * STEP_BEATS);
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
                        start_beats: f64::from(step) * STEP_BEATS,
                        length_beats: STEP_BEATS,
                        velocity: DEFAULT_VELOCITY,
                    }],
                }
                .into(),
            ),
        }
    }
}
