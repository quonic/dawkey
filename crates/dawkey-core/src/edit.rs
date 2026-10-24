// SPDX-License-Identifier: AGPL-3.0-or-later
//! Primitive, invertible changes to a [`Project`]. Commands lower to edits;
//! the undo history stores the inverse edits.

use crate::error::CoreError;
use crate::model::{
    AutomationLane, Clip, ClipContent, ClipId, ControlPoint, InputChannels, MidiOutput, Note,
    PluginSlot, Project, Region, Send, SynthParams, TimeSignature, Track, TrackId,
};
use crate::tempo::{MeterChange, TempoChange};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Edit {
    InsertTrack {
        index: usize,
        track: Track,
    },
    RemoveTrack {
        id: TrackId,
    },
    SetTrack {
        id: TrackId,
        prop: TrackProp,
    },
    InsertClip {
        track: TrackId,
        index: usize,
        clip: Clip,
    },
    RemoveClip {
        id: ClipId,
    },
    SetClip {
        id: ClipId,
        prop: ClipProp,
    },
    SetProjectName(String),
    /// The start tempo and later changes.
    SetTempo {
        bpm: f64,
        changes: Vec<TempoChange>,
    },
    /// The time signature at the start and later changes.
    SetMeter {
        start: TimeSignature,
        changes: Vec<MeterChange>,
    },
    SetLoop(Region),
    SetPunch(Region),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TrackProp {
    Name(String),
    GainDb(f32),
    Pan(f32),
    Mute(bool),
    Solo(bool),
    Armed(bool),
    Input(InputChannels),
    Monitor(bool),
    Synth(SynthParams),
    /// Device and channel.
    MidiInput(Option<String>, Option<u8>),
    MidiOutput(Option<MidiOutput>),
    Plugins(Vec<PluginSlot>),
    Automation(Vec<AutomationLane>),
    /// The bus it goes to (`None`: the master).
    Output(Option<TrackId>),
    /// Its aux sends.
    Sends(Vec<Send>),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ClipProp {
    Start(u64),
    Length(u64),
    /// An audio clip's first source frame.
    Offset(u64),
    GainDb(f32),
    Muted(bool),
    /// `None` is the default label.
    Name(Option<String>),
    /// A MIDI clip's notes, sorted by start.
    Notes(Vec<Note>),
    /// A MIDI clip's controller changes, sorted by frame.
    Controls(Vec<ControlPoint>),
}

impl Edit {
    /// Applies the edit and returns the edit that reverts it. On error the
    /// project is unchanged.
    pub(crate) fn apply(self, project: &mut Project) -> Result<Edit, CoreError> {
        Ok(match self {
            Edit::InsertTrack { index, track } => {
                let id = track.id;
                let index = index.min(project.tracks.len());
                project.tracks.insert(index, track);
                Edit::RemoveTrack { id }
            }
            Edit::RemoveTrack { id } => {
                let index = project
                    .track_index(id)
                    .ok_or(CoreError::TrackNotFound(id))?;
                let track = project.tracks.remove(index);
                Edit::InsertTrack { index, track }
            }
            Edit::SetTrack { id, prop } => {
                let track = project.track_mut(id).ok_or(CoreError::TrackNotFound(id))?;
                let old = match prop {
                    TrackProp::Name(v) => TrackProp::Name(std::mem::replace(&mut track.name, v)),
                    TrackProp::GainDb(v) => {
                        TrackProp::GainDb(std::mem::replace(&mut track.gain_db, v))
                    }
                    TrackProp::Pan(v) => TrackProp::Pan(std::mem::replace(&mut track.pan, v)),
                    TrackProp::Mute(v) => TrackProp::Mute(std::mem::replace(&mut track.mute, v)),
                    TrackProp::Solo(v) => TrackProp::Solo(std::mem::replace(&mut track.solo, v)),
                    TrackProp::Armed(v) => TrackProp::Armed(std::mem::replace(&mut track.armed, v)),
                    TrackProp::Input(v) => TrackProp::Input(std::mem::replace(&mut track.input, v)),
                    TrackProp::Monitor(v) => {
                        TrackProp::Monitor(std::mem::replace(&mut track.monitor, v))
                    }
                    TrackProp::MidiInput(device, channel) => TrackProp::MidiInput(
                        std::mem::replace(&mut track.midi_input, device),
                        std::mem::replace(&mut track.midi_input_channel, channel),
                    ),
                    TrackProp::MidiOutput(v) => {
                        TrackProp::MidiOutput(std::mem::replace(&mut track.midi_output, v))
                    }
                    TrackProp::Plugins(v) => {
                        TrackProp::Plugins(std::mem::replace(&mut track.plugins, v))
                    }
                    TrackProp::Automation(v) => {
                        TrackProp::Automation(std::mem::replace(&mut track.automation, v))
                    }
                    TrackProp::Output(v) => {
                        TrackProp::Output(std::mem::replace(&mut track.output, v))
                    }
                    TrackProp::Sends(v) => TrackProp::Sends(std::mem::replace(&mut track.sends, v)),
                    TrackProp::Synth(v) => {
                        let Some(synth) = &mut track.synth else {
                            return Err(CoreError::invalid(format!(
                                "track {id} has no synth (it is an audio track)"
                            )));
                        };
                        TrackProp::Synth(std::mem::replace(synth, v))
                    }
                };
                Edit::SetTrack { id, prop: old }
            }
            Edit::InsertClip { track, index, clip } => {
                let id = clip.id;
                let track = project
                    .track_mut(track)
                    .ok_or(CoreError::TrackNotFound(track))?;
                let index = index.min(track.clips.len());
                track.clips.insert(index, clip);
                Edit::RemoveClip { id }
            }
            Edit::RemoveClip { id } => {
                let (track, index) = project
                    .tracks
                    .iter_mut()
                    .find_map(|t| t.clips.iter().position(|c| c.id == id).map(|i| (t, i)))
                    .ok_or(CoreError::ClipNotFound(id))?;
                let clip = track.clips.remove(index);
                Edit::InsertClip {
                    track: track.id,
                    index,
                    clip,
                }
            }
            Edit::SetClip { id, prop } => {
                let clip = project.clip_mut(id).ok_or(CoreError::ClipNotFound(id))?;
                let old = match prop {
                    ClipProp::Start(v) => ClipProp::Start(std::mem::replace(&mut clip.start, v)),
                    ClipProp::Length(v) => ClipProp::Length(std::mem::replace(&mut clip.length, v)),
                    ClipProp::Offset(v) => {
                        let ClipContent::Audio { offset, .. } = &mut clip.content else {
                            return Err(CoreError::invalid(format!(
                                "clip {id} is a MIDI clip; only audio clips have a source offset"
                            )));
                        };
                        ClipProp::Offset(std::mem::replace(offset, v))
                    }
                    ClipProp::GainDb(v) => {
                        ClipProp::GainDb(std::mem::replace(&mut clip.gain_db, v))
                    }
                    ClipProp::Muted(v) => ClipProp::Muted(std::mem::replace(&mut clip.muted, v)),
                    ClipProp::Name(v) => ClipProp::Name(std::mem::replace(&mut clip.name, v)),
                    ClipProp::Notes(v) => {
                        let ClipContent::Midi { notes, .. } = &mut clip.content else {
                            return Err(CoreError::invalid(format!(
                                "clip {id} is an audio clip; only MIDI clips have notes"
                            )));
                        };
                        ClipProp::Notes(std::mem::replace(notes, v))
                    }
                    ClipProp::Controls(v) => {
                        let ClipContent::Midi { controls, .. } = &mut clip.content else {
                            return Err(CoreError::invalid(format!(
                                "clip {id} is an audio clip; only MIDI clips have controllers"
                            )));
                        };
                        ClipProp::Controls(std::mem::replace(controls, v))
                    }
                };
                Edit::SetClip { id, prop: old }
            }
            Edit::SetProjectName(v) => {
                Edit::SetProjectName(std::mem::replace(&mut project.name, v))
            }
            Edit::SetTempo { bpm, changes } => Edit::SetTempo {
                bpm: std::mem::replace(&mut project.tempo_bpm, bpm),
                changes: std::mem::replace(&mut project.tempo_changes, changes),
            },
            Edit::SetMeter { start, changes } => Edit::SetMeter {
                start: std::mem::replace(&mut project.time_signature, start),
                changes: std::mem::replace(&mut project.meter_changes, changes),
            },
            Edit::SetLoop(v) => Edit::SetLoop(std::mem::replace(&mut project.loop_region, v)),
            Edit::SetPunch(v) => Edit::SetPunch(std::mem::replace(&mut project.punch, v)),
        })
    }
}

/// Applies `edits` in order and returns their inverses in the order that
/// reverts them. All-or-nothing: if any edit fails, the ones already applied
/// are rolled back before the error is returned.
pub(crate) fn apply_all(project: &mut Project, edits: Vec<Edit>) -> Result<Vec<Edit>, CoreError> {
    let mut inverses = Vec::with_capacity(edits.len());
    for edit in edits {
        match edit.apply(project) {
            Ok(inverse) => inverses.push(inverse),
            Err(err) => {
                inverses.reverse();
                rollback(project, inverses);
                return Err(err);
            }
        }
    }
    inverses.reverse();
    Ok(inverses)
}

/// Applies inverse edits, given in the order that reverts them, which must
/// succeed because they undo edits that were just applied.
pub(crate) fn rollback(project: &mut Project, inverses: Vec<Edit>) {
    for inverse in inverses {
        inverse
            .apply(project)
            .expect("inverse of an applied edit must apply");
    }
}
