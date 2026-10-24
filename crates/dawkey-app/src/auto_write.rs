// SPDX-License-Identifier: AGPL-3.0-or-later
//! Writing automation while playing: with the transport's "Auto" on,
//! moving a fader, a pan control or a plugin parameter writes what it does
//! into the control's lane at the playhead, replacing what the lane had
//! there. Holding a control still keeps writing its value. How long it
//! writes is the mode (the button next to "Auto"):
//!
//! - **Touch**: while the control is held; once let go, the lane plays as
//!   before. One undo step per gesture.
//! - **Latch**: from when a control is grabbed until the transport stops;
//!   let go, it keeps writing the value it was left at.
//! - **Write**: every lane, and every control grabbed, from where playback
//!   starts until it stops, each at its value (what its lane had where
//!   playback started, until it's moved). Then the mode goes back to touch.
//!
//! A latch or write pass is one undo step, unless something else is done
//! while it goes on.

use dawkey_core::{AutomationLane, AutomationTarget, Command, PointSpec, TrackId, WriteAutomation};

use crate::App;
use crate::state::{AutomationMode, AutomationPass, AutomationWrite, Gesture};

/// Points closer than this (seconds) replace each other.
const MIN_STEP_S: f64 = 0.005;

impl App {
    /// A gesture moved `target` on `track` to `value`: written into its lane
    /// at the playhead while writing automation, otherwise set with `set`.
    pub(crate) fn control_moved(
        &mut self,
        track: TrackId,
        target: AutomationTarget,
        value: f64,
        merge_key: &str,
        set: Command,
    ) -> Result<(), String> {
        if !self.writing_automation() {
            return self.execute_merging(set, merge_key);
        }
        self.write_automation_point(track, target, value, merge_key)
    }

    /// Whether moving a control writes automation now.
    pub(crate) fn writing_automation(&self) -> bool {
        self.view.automation_write && self.studio.transport().playing
    }

    /// The value a control has at the playhead: its lane's, if it has one,
    /// or `fixed`. Gestures start from it.
    pub(crate) fn value_now(&self, track: TrackId, target: &AutomationTarget, fixed: f64) -> f64 {
        let project = self.studio.session().project();
        let frame = project.seconds_to_frames(self.studio.transport().position_s);
        project
            .track(track)
            .and_then(|t| t.lane(target))
            .and_then(|l: &AutomationLane| l.value_at(frame))
            .unwrap_or(fixed)
    }

    /// The transport's mode button: the next mode. A pass going on ends.
    pub(crate) fn cycle_automation_mode(&mut self) {
        let next = self.view.automation_mode.next();
        self.end_automation_pass();
        self.view.automation_mode = next;
    }

    /// The undo step automation is written in: the gesture's while
    /// touching, otherwise the pass's (started here if there's none).
    fn write_key(&mut self, gesture_key: &str) -> String {
        if self.view.automation_mode == AutomationMode::Touch {
            return gesture_key.to_owned();
        }
        let passes = &mut self.view.passes;
        self.view
            .pass
            .get_or_insert_with(|| {
                *passes += 1;
                AutomationPass {
                    merge_key: format!("automation pass {passes}"),
                    every_lane: false,
                }
            })
            .merge_key
            .clone()
    }

    fn write_automation_point(
        &mut self,
        track: TrackId,
        target: AutomationTarget,
        value: f64,
        merge_key: &str,
    ) -> Result<(), String> {
        let (command, key) = self.automation_point(track, target, value, merge_key);
        self.execute_merging(command, &key)
    }

    /// Adds a point at the playhead to the lane's stretch being written:
    /// the command that writes the stretch, and the undo step it goes in.
    fn automation_point(
        &mut self,
        track: TrackId,
        target: AutomationTarget,
        value: f64,
        merge_key: &str,
    ) -> (Command, String) {
        let now = self.studio.transport().position_s;
        let key = self.write_key(merge_key);
        let found = self
            .view
            .writing
            .iter()
            .position(|w| w.track == track && w.target == target);
        let index = match found {
            Some(i) => {
                // The playhead jumped back (a loop): a new stretch, in the
                // same undo step.
                let write = &mut self.view.writing[i];
                if write.points.last().is_some_and(|&(t, _)| t > now) {
                    write.from_s = now;
                    write.points.clear();
                }
                i
            }
            None => {
                self.view.writing.push(AutomationWrite {
                    track,
                    target: target.clone(),
                    from_s: now,
                    points: Vec::new(),
                });
                self.view.writing.len() - 1
            }
        };
        let write = &mut self.view.writing[index];
        let points = &mut write.points;
        match points[..] {
            [.., (t, _)] if now - t < MIN_STEP_S => points.last_mut().expect("matched").1 = value,
            // Held still: the stretch's end moves on.
            [.., (_, a), (_, b)] if a == value && b == value => {
                points.last_mut().expect("matched").0 = now;
            }
            _ => points.push((now, value)),
        }
        let command = WriteAutomation {
            track,
            target,
            from_s: write.from_s,
            to_s: now,
            points: points
                .iter()
                .map(|&(time_s, value)| PointSpec { time_s, value })
                .collect(),
        };
        (command.into(), key)
    }

    /// A gesture grabbed a control: while writing automation, it's written
    /// from now, at the value it has.
    pub(crate) fn touch(&mut self, gesture: &Gesture) -> Result<(), String> {
        if !self.writing_automation() {
            return Ok(());
        }
        let (track, target, value, key) = match gesture {
            Gesture::Fader {
                track,
                start_db,
                merge_key,
            } => (
                *track,
                AutomationTarget::Gain,
                f64::from(*start_db),
                merge_key,
            ),
            Gesture::Pan {
                track,
                start_pan,
                merge_key,
            } => (
                *track,
                AutomationTarget::Pan,
                f64::from(*start_pan),
                merge_key,
            ),
            Gesture::Send {
                track,
                bus,
                start_db,
                merge_key,
            } => (
                *track,
                AutomationTarget::Send { bus: *bus },
                f64::from(*start_db),
                merge_key,
            ),
            Gesture::PluginParam {
                plugin,
                param,
                start_value,
                merge_key,
                ..
            } => {
                let project = self.studio.session().project();
                let Some((track, _)) = project.plugin(*plugin) else {
                    return Ok(());
                };
                let target = AutomationTarget::Param {
                    plugin: *plugin,
                    param: param.clone(),
                };
                (track.id, target, *start_value, merge_key)
            }
            _ => return Ok(()),
        };
        self.write_automation_point(track, target, value, &key.clone())
    }

    /// A gesture let go: its undo step ends. Touching, its writing stops;
    /// in latch and write modes it goes on, in the pass's undo step.
    pub(crate) fn release_automation(&mut self) {
        if self.view.pass.is_some() && self.writing_automation() {
            return;
        }
        self.view.writing.clear();
        self.studio.end_merge();
    }

    /// Every tick: controls held still (and, in latch and write modes, the
    /// ones let go) keep writing. Once the transport stops or "Auto" goes
    /// off, a pass is over.
    pub(crate) fn keep_writing_automation(&mut self) {
        if !self.writing_automation() {
            self.end_automation_pass();
            return;
        }
        if self.view.automation_mode == AutomationMode::Write
            && !self.view.pass.as_ref().is_some_and(|p| p.every_lane)
        {
            self.write_every_lane();
        }
        if self.view.pass.is_none() && self.view.gesture.is_none() {
            return;
        }
        let writes: Vec<(TrackId, AutomationTarget, f64)> = self
            .view
            .writing
            .iter()
            .filter_map(|w| Some((w.track, w.target.clone(), w.points.last()?.1)))
            .collect();
        let key = match &self.view.gesture {
            Some(
                Gesture::Fader { merge_key, .. }
                | Gesture::Pan { merge_key, .. }
                | Gesture::Send { merge_key, .. }
                | Gesture::PluginParam { merge_key, .. },
            ) => merge_key.clone(),
            _ => String::new(),
        };
        self.write_all(writes, &key);
    }

    /// Writes (track, target, value) at the playhead, with one recompile
    /// for them all.
    fn write_all(&mut self, writes: Vec<(TrackId, AutomationTarget, f64)>, gesture_key: &str) {
        let mut commands = Vec::with_capacity(writes.len());
        let mut key = String::new();
        for (track, target, value) in writes {
            let (command, k) = self.automation_point(track, target, value, gesture_key);
            commands.push(command);
            key = k;
        }
        if commands.is_empty() {
            return;
        }
        if let Err(e) = self.studio.execute_merging_all(commands, &key) {
            self.message = Some(e.to_string());
        }
    }

    /// Write mode, as playback starts: every lane is written from here at
    /// the value it has here.
    fn write_every_lane(&mut self) {
        let project = self.studio.session().project();
        let frame = project.seconds_to_frames(self.studio.transport().position_s);
        let lanes: Vec<(TrackId, AutomationTarget, f64)> = project
            .all_tracks()
            .flat_map(|t| {
                t.automation
                    .iter()
                    .filter_map(move |l| Some((t.id, l.target.clone(), l.value_at(frame)?)))
            })
            .collect();
        self.write_key("");
        if let Some(pass) = &mut self.view.pass {
            pass.every_lane = true;
        }
        let writing = &self.view.writing;
        let lanes = lanes
            .into_iter()
            .filter(|(track, target, _)| {
                !writing
                    .iter()
                    .any(|w| w.track == *track && w.target == *target)
            })
            .collect();
        self.write_all(lanes, "");
    }

    /// Ends a latch or write pass: the lanes stop being written, and write
    /// mode goes back to touch.
    pub(crate) fn end_automation_pass(&mut self) {
        let Some(pass) = self.view.pass.take() else {
            return;
        };
        self.view.writing.clear();
        self.studio.end_merge();
        if pass.every_lane || self.view.automation_mode == AutomationMode::Write {
            self.view.automation_mode = AutomationMode::Touch;
        }
    }
}
