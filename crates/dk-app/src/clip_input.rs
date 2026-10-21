// SPDX-License-Identifier: AGPL-3.0-or-later
//! Trimming clips by dragging their edges, splitting them at the
//! playhead, copying and pasting them, and repeating them. Each is one
//! command, so one undo step. Clips placed here never overlap another: they
//! go to the nearest place they fit, and edges stop at the next clip.

use dk_core::{
    Clip, ClipContent, ClipId, DuplicateClip, RepeatClip, SplitClip, Track, TrackId, TrimClip,
};
use dk_ui::{Modifiers, Point};

use crate::App;
use crate::input::snap;
use crate::state::{ClipEdge, Gesture};

/// The shortest a drag trims a clip to, in seconds.
const MIN_TRIM_S: f64 = 0.01;

impl App {
    /// Starts dragging `clip`'s start or end edge (and selects the clip).
    pub(crate) fn start_trim(
        &mut self,
        clip: ClipId,
        edge: ClipEdge,
        origin: Point,
    ) -> Result<Gesture, String> {
        let project = self.studio.session().project();
        let (track, c) = project
            .clip(clip)
            .ok_or_else(|| format!("clip {clip} does not exist"))?;
        let start_s = project.frames_to_seconds(c.start);
        let end_s = project.frames_to_seconds(c.end());
        // An audio clip's edges stop at its source's ends.
        let (min_s, max_s) = match (edge, &c.content) {
            (ClipEdge::Start, ClipContent::Audio { offset, .. }) => (
                project.frames_to_seconds(c.start.saturating_sub(*offset)),
                end_s - MIN_TRIM_S,
            ),
            (ClipEdge::Start, ClipContent::Midi { .. }) => (0.0, end_s - MIN_TRIM_S),
            (ClipEdge::End, ClipContent::Audio { source, offset }) => {
                let end = project
                    .source(*source)
                    .map_or(c.end(), |s| c.start + s.frames.saturating_sub(*offset));
                (start_s + MIN_TRIM_S, project.frames_to_seconds(end))
            }
            (ClipEdge::End, ClipContent::Midi { .. }) => (start_s + MIN_TRIM_S, f64::INFINITY),
        };
        // And at the clips next to it.
        let (min_s, max_s) = match edge {
            ClipEdge::Start => (
                min_s.max(clip_before(track, c).map_or(0.0, |f| project.frames_to_seconds(f))),
                max_s,
            ),
            ClipEdge::End => (
                min_s,
                max_s.min(
                    clip_after(track, c).map_or(f64::INFINITY, |f| project.frames_to_seconds(f)),
                ),
            ),
        };
        let edge_s = match edge {
            ClipEdge::Start => start_s,
            ClipEdge::End => end_s,
        };
        self.view.selected_clip = Some(clip);
        self.view.selected_marker = None;
        Ok(Gesture::TrimClip {
            clip,
            edge,
            grab_s: self.time_at(origin.x) - edge_s,
            min_s,
            max_s: max_s.max(min_s),
            start_s,
            end_s,
        })
    }

    /// Moves the dragged edge (snapped to beats unless Shift is held); on
    /// release, trims the clip there.
    pub(crate) fn on_trim_drag(
        &mut self,
        gesture: Gesture,
        pos: Point,
        end: bool,
        modifiers: Modifiers,
    ) -> Result<(), String> {
        let Gesture::TrimClip {
            clip,
            edge,
            grab_s,
            min_s,
            max_s,
            mut start_s,
            mut end_s,
        } = gesture
        else {
            unreachable!("only called for trims");
        };
        let mut t = self.time_at(pos.x) - grab_s;
        if self.view.snap && !modifiers.shift {
            t = snap(self.studio.session().project(), t);
        }
        let t = t.clamp(min_s, max_s);
        match edge {
            ClipEdge::Start => start_s = t,
            ClipEdge::End => end_s = t,
        }
        if !end {
            self.view.gesture = Some(Gesture::TrimClip {
                clip,
                edge,
                grab_s,
                min_s,
                max_s,
                start_s,
                end_s,
            });
            return Ok(());
        }
        self.view.gesture = None;
        let project = self.studio.session().project();
        let (_, c) = project
            .clip(clip)
            .ok_or_else(|| format!("clip {clip} no longer exists"))?;
        let current = match edge {
            ClipEdge::Start => project.frames_to_seconds(c.start),
            ClipEdge::End => project.frames_to_seconds(c.end()),
        };
        if (t - current).abs() < 1e-9 {
            return Ok(());
        }
        let (start_s, end_s) = match edge {
            ClipEdge::Start => (Some(t), None),
            ClipEdge::End => (None, Some(t)),
        };
        self.execute(
            TrimClip {
                clip,
                start_s,
                end_s,
            }
            .into(),
        )
    }

    /// Splits the selected clip at the playhead and selects the part after
    /// it, so splitting again further on splits that part.
    pub(crate) fn split_selected(&mut self) -> Result<(), String> {
        let clip = self.view.selected_clip.ok_or("select a clip to split")?;
        let at_s = self.studio.transport().position_s;
        let project = self.studio.session().project();
        let (_, c) = project
            .clip(clip)
            .ok_or_else(|| format!("clip {clip} no longer exists"))?;
        let at = project.seconds_to_frames(at_s);
        if at <= c.start || at >= c.end() {
            return Err("move the playhead into the clip to split it".into());
        }
        let outcome = self
            .studio
            .execute(SplitClip { clip, at_s }.into())
            .map_err(|e| e.to_string())?;
        self.view.selected_clip = outcome.clip;
        Ok(())
    }

    /// Where clip `clip` goes if placed at `start_s` on `track`: there, or
    /// if another clip is in the way, the nearest place it fits (see
    /// [`Track::free_start`]). `moving`: the clip itself isn't in the way.
    pub(crate) fn free_start_s(
        &self,
        clip: ClipId,
        track: TrackId,
        start_s: f64,
        moving: bool,
    ) -> f64 {
        let project = self.studio.session().project();
        let (Some((_, c)), Some(t)) = (project.clip(clip), project.track(track)) else {
            return start_s;
        };
        let start = project.seconds_to_frames(start_s);
        let free = t.free_start(start, c.length, moving.then_some(clip));
        if free == start {
            start_s
        } else {
            project.frames_to_seconds(free)
        }
    }

    /// Copies `clip` to `start_s` on `track` and selects the copy.
    pub(crate) fn duplicate_clip(
        &mut self,
        clip: ClipId,
        track: TrackId,
        start_s: f64,
    ) -> Result<(), String> {
        let outcome = self
            .studio
            .execute(
                DuplicateClip {
                    clip,
                    position_s: Some(start_s),
                    track: Some(track),
                }
                .into(),
            )
            .map_err(|e| e.to_string())?;
        self.view.selected_clip = outcome.clip;
        self.view.selected_marker = None;
        Ok(())
    }

    /// Pastes the copied clip (Ctrl+V) at the pointer: onto the track under
    /// it if that's a track of the clip's kind, else onto the clip's own
    /// track; at the playhead when the pointer isn't over the timeline.
    pub(crate) fn paste_clip(&mut self) -> Result<(), String> {
        let clip = self.view.clipboard.ok_or("copy a clip first (Ctrl+C)")?;
        let project = self.studio.session().project();
        let (from, c) = project
            .clip(clip)
            .ok_or_else(|| format!("clip {clip} no longer exists"))?;
        let (from, kind) = (from.id, c.kind());
        let lanes = self.lanes_rect();
        let pointer = self
            .ui
            .pointer()
            .filter(|p| p.x >= lanes.x && p.x < lanes.x + lanes.w);
        let start_s = match pointer {
            Some(p) => {
                let t = self.time_at(p.x).max(0.0);
                if self.view.snap { snap(project, t) } else { t }
            }
            None => self.studio.transport().position_s,
        };
        let track = pointer
            .and_then(|p| self.track_at(p.y))
            .filter(|t| project.track(*t).map(|t| t.kind) == Some(kind))
            .unwrap_or(from);
        let start_s = self.free_start_s(clip, track, start_s, false);
        self.duplicate_clip(clip, track, start_s)
    }

    /// Starts Ctrl-dragging `clip`'s end edge to repeat it (and selects
    /// the clip).
    pub(crate) fn start_repeat(&mut self, clip: ClipId, origin: Point) -> Result<Gesture, String> {
        let project = self.studio.session().project();
        let (track, c) = project
            .clip(clip)
            .ok_or_else(|| format!("clip {clip} does not exist"))?;
        let end_s = project.frames_to_seconds(c.end());
        let max_s = clip_after(track, c).map_or(f64::INFINITY, |f| project.frames_to_seconds(f));
        self.view.selected_clip = Some(clip);
        self.view.selected_marker = None;
        Ok(Gesture::Repeat {
            clip,
            grab_s: self.time_at(origin.x) - end_s,
            max_s,
            until_s: end_s,
        })
    }

    /// Drags out the repeats (snapped to beats unless Shift is held, and up
    /// to the next clip); on release, repeats the clip up to there.
    pub(crate) fn on_repeat_drag(
        &mut self,
        gesture: Gesture,
        pos: Point,
        end: bool,
        modifiers: Modifiers,
    ) -> Result<(), String> {
        let Gesture::Repeat {
            clip,
            grab_s,
            max_s,
            ..
        } = gesture
        else {
            unreachable!("only called for repeats");
        };
        let project = self.studio.session().project();
        let (_, c) = project
            .clip(clip)
            .ok_or_else(|| format!("clip {clip} no longer exists"))?;
        let (clip_end, end_s) = (c.end(), project.frames_to_seconds(c.end()));
        let mut t = self.time_at(pos.x) - grab_s;
        if self.view.snap && !modifiers.shift {
            t = snap(project, t);
        }
        let until_s = t.clamp(end_s, max_s.max(end_s));
        if !end {
            self.view.gesture = Some(Gesture::Repeat {
                clip,
                grab_s,
                max_s,
                until_s,
            });
            return Ok(());
        }
        self.view.gesture = None;
        if project.seconds_to_frames(until_s) <= clip_end {
            return Ok(());
        }
        self.execute(RepeatClip { clip, until_s }.into())
    }
}

/// Where the nearest clip before `clip` on `track` ends (of those that end
/// by its start).
fn clip_before(track: &Track, clip: &Clip) -> Option<u64> {
    track
        .clips
        .iter()
        .filter(|c| c.id != clip.id && c.end() <= clip.start)
        .map(Clip::end)
        .max()
}

/// Where the nearest clip after `clip` on `track` starts (of those that
/// start at its end or later).
fn clip_after(track: &Track, clip: &Clip) -> Option<u64> {
    track
        .clips
        .iter()
        .filter(|c| c.id != clip.id && c.start >= clip.end())
        .map(|c| c.start)
        .min()
}
