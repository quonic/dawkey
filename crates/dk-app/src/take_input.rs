// SPDX-License-Identifier: AGPL-3.0-or-later
//! Take lanes and input monitoring in the arrange view. Clicking a take
//! plays it; dragging across a take lane plays that lane over the range;
//! clicking a lane's header plays the take wherever it has clips. Each is
//! one `comp_take`, so one undo step.

use dk_core::{ClipId, CompTake, SetTrackMonitor, TrackId};
use dk_ui::{Modifiers, Point};

use crate::App;
use crate::input::{parse_id, parse_track, snap};
use crate::state::Gesture;

impl App {
    /// Clicks on take lanes and the header toggles for monitoring and
    /// takes; `None` if `parts` is something else.
    pub(crate) fn on_take_click(&mut self, parts: &[&str]) -> Option<Result<(), String>> {
        Some(match *parts {
            ["track", track, "monitor"] => parse_track(track).and_then(|track| {
                let monitor = !self
                    .studio
                    .session()
                    .project()
                    .track(track)
                    .ok_or_else(|| format!("track {track} does not exist"))?
                    .monitor;
                self.execute(SetTrackMonitor { track, monitor }.into())
            }),
            ["track", track, "takes"] => parse_track(track).map(|track| {
                let shown = &mut self.view.takes_shown;
                match shown.iter().position(|t| *t == track) {
                    Some(i) => {
                        shown.remove(i);
                    }
                    None => shown.push(track),
                }
            }),
            ["track", track, "take", lane] => parse_track(track)
                .and_then(|track| Ok((track, parse_id(lane)?)))
                .and_then(|(track, lane)| self.comp_lane(track, lane as u32)),
            ["take", clip] => parse_id(clip).and_then(|clip| self.comp(ClipId(clip), None)),
            _ => return None,
        })
    }

    /// Starts swiping across the take lane `clip` is in.
    pub(crate) fn start_comp(
        &mut self,
        clip: ClipId,
        origin: Point,
        modifiers: Modifiers,
    ) -> Result<Option<Gesture>, String> {
        let (track, c) = self
            .studio
            .session()
            .project()
            .clip(clip)
            .ok_or_else(|| format!("clip {clip} does not exist"))?;
        let (track, lane) = (track.id, c.lane);
        let anchor_s = self.comp_time(origin.x, modifiers);
        self.view.selected_clip = Some(clip);
        self.view.selected_marker = None;
        Ok(Some(Gesture::Comp {
            track,
            clip,
            lane,
            anchor_s,
            current_s: anchor_s,
        }))
    }

    /// Follows a swipe; on release, the lane plays over the range (or, if
    /// the pointer hardly moved, the whole take).
    pub(crate) fn on_comp_drag(
        &mut self,
        gesture: Gesture,
        pos: Point,
        end: bool,
        modifiers: Modifiers,
    ) -> Result<(), String> {
        let Gesture::Comp {
            track,
            clip,
            lane,
            anchor_s,
            ..
        } = gesture
        else {
            unreachable!("only called for comp swipes");
        };
        let current_s = self.comp_time(pos.x, modifiers);
        if !end {
            self.view.gesture = Some(Gesture::Comp {
                track,
                clip,
                lane,
                anchor_s,
                current_s,
            });
            return Ok(());
        }
        self.view.gesture = None;
        let (from, to) = (anchor_s.min(current_s), anchor_s.max(current_s));
        if to - from < 1e-3 {
            return self.comp(clip, None);
        }
        // Name a clip of the lane that the range covers.
        let project = self.studio.session().project();
        let (a, b) = (
            project.seconds_to_frames(from),
            project.seconds_to_frames(to),
        );
        let covered = project.track(track).and_then(|t| {
            t.clips
                .iter()
                .find(|c| c.lane == lane && c.start < b && a < c.end())
                .map(|c| c.id)
        });
        match covered {
            Some(clip) => self.comp(clip, Some((from, to))),
            None => Ok(()),
        }
    }

    /// Plays take lane `lane` of `track` wherever it has clips.
    fn comp_lane(&mut self, track: TrackId, lane: u32) -> Result<(), String> {
        let project = self.studio.session().project();
        let clips = project
            .track(track)
            .ok_or_else(|| format!("track {track} does not exist"))?
            .clips
            .iter()
            .filter(|c| c.lane == lane);
        let Some((clip, end)) = clips.fold(None, |acc: Option<(ClipId, u64)>, c| match acc {
            Some((first, end)) => Some((first, end.max(c.end()))),
            None => Some((c.id, c.end())),
        }) else {
            return Ok(());
        };
        let end_s = project.frames_to_seconds(end);
        self.comp(clip, Some((0.0, end_s)))
    }

    /// Runs `comp_take` and selects the part of the clip that now plays.
    fn comp(&mut self, clip: ClipId, range: Option<(f64, f64)>) -> Result<(), String> {
        let outcome = self
            .studio
            .execute(
                CompTake {
                    clip,
                    start_s: range.map(|r| r.0),
                    end_s: range.map(|r| r.1),
                }
                .into(),
            )
            .map_err(|e| e.to_string())?;
        self.view.selected_clip = outcome.clip;
        self.view.selected_marker = None;
        Ok(())
    }

    /// Timeline time at window x, snapped to beats unless Shift is held.
    fn comp_time(&self, x: f32, modifiers: Modifiers) -> f64 {
        let t = self.time_at(x).max(0.0);
        if self.view.snap && !modifiers.shift {
            snap(self.studio.session().project(), t)
        } else {
            t
        }
    }
}
