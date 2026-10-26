// SPDX-License-Identifier: AGPL-3.0-or-later
//! What clicks, drags and keys do in the tempo and time signature lanes.
//! Every change is one command, so one undo step; MIDI clips keep their
//! beats when the tempo changes (see `set_tempo`).

use dawkey_core::{
    MeterChange, RemoveTempoChange, RemoveTimeSignatureChange, SetTempo, SetTempoMap,
    SetTimeSignature, SetTimeSignatureMap, TEMPO_BPM_MAX, TEMPO_BPM_MIN, TempoChange,
    TimeSignature,
};
use dawkey_ui::{Modifiers, Point};

use crate::App;
use crate::input::snap_beat;
use crate::state::{Gesture, Marker, Page, TextField};
use crate::view::tempo::{TempoPoint, point_at, tempo_at_y, tempo_scale};

/// BPM per pixel of a vertical tempo point drag; with Ctrl, finer.
const BPM_PER_PX: f64 = 0.5;
const FINE_BPM_PER_PX: f64 = 0.05;

impl App {
    /// The project's tempo points: the start, then each change.
    fn tempo_points(&self) -> Vec<TempoPoint> {
        let project = self.studio.session().project();
        std::iter::once((0.0, project.tempo_bpm, false))
            .chain(
                project
                    .tempo_changes
                    .iter()
                    .map(|c| (c.beat, c.bpm, c.ramp)),
            )
            .collect()
    }

    /// The project's time signatures: (bar from 0, time signature), the
    /// start first.
    pub(crate) fn meter_points(&self) -> Vec<(u32, TimeSignature)> {
        let project = self.studio.session().project();
        std::iter::once((0, project.time_signature))
            .chain(
                project
                    .meter_changes
                    .iter()
                    .map(|c| (c.bar.saturating_sub(1), c.time_signature())),
            )
            .collect()
    }

    fn playhead_beat(&self) -> f64 {
        let project = self.studio.session().project();
        project
            .tempo_map()
            .beat_at_seconds(self.studio.transport().position_s)
    }

    /// The tempo point the lane's header edits: the selected one, else the
    /// one in effect at the playhead.
    fn current_tempo(&self) -> usize {
        let points = self.tempo_points();
        match self.view.selected_marker {
            Some(Marker::Tempo(n)) if n < points.len() => n,
            _ => point_at(points.iter().map(|p| p.0), self.playhead_beat()),
        }
    }

    fn current_meter(&self) -> usize {
        let points = self.meter_points();
        match self.view.selected_marker {
            Some(Marker::Meter(n)) if n < points.len() => n,
            _ => {
                let bar = self.studio.session().project().meter_map();
                let bar = bar.bar_at(self.playhead_beat());
                point_at(points.iter().map(|p| f64::from(p.0)), f64::from(bar))
            }
        }
    }

    /// Clicks in the tempo and time signature lanes. `None` if `parts`
    /// isn't one of theirs.
    pub(crate) fn on_tempo_click(
        &mut self,
        parts: &[&str],
        pos: Point,
        count: u8,
    ) -> Option<Result<(), String>> {
        Some(match parts {
            ["arrange", "tempo"] | ["transport", "tempo" | "time_signature"] => {
                self.view.tempo_shown = !self.view.tempo_shown;
                if !self.view.tempo_shown {
                    self.view.selected_marker = None;
                }
                Ok(())
            }
            ["tempo", "up" | "down"] => {
                let by = if parts[1] == "up" { 1.0 } else { -1.0 };
                self.nudge_tempo(self.current_tempo(), by)
            }
            ["tempo", "ramp"] => self.toggle_ramp(self.current_tempo()),
            ["tempo", "value"] => self.start_text_edit(TextField::Tempo(self.current_tempo())),
            ["tempo", "remove"] => self.remove_tempo(self.current_tempo()),
            ["tempo", "lane"] => {
                self.view.selected_marker = None;
                if count == 2 {
                    return Some(self.add_tempo_change(pos));
                }
                Ok(())
            }
            ["tempo", n] => {
                let n: usize = n.parse().ok()?;
                self.select_marker(Marker::Tempo(n));
                self.view.selected_clip = None;
                Ok(())
            }
            [
                "meter",
                which @ ("numerator" | "denominator"),
                dir @ ("up" | "down"),
            ] => {
                let up = *dir == "up";
                self.nudge_meter(self.current_meter(), *which == "numerator", up)
            }
            ["meter", "numerator", "value"] => {
                self.start_text_edit(TextField::MeterNumerator(self.current_meter()))
            }
            ["meter", "denominator", "value"] => {
                self.start_text_edit(TextField::MeterDenominator(self.current_meter()))
            }
            ["meter", "remove"] => self.remove_meter(self.current_meter()),
            ["meter", "lane"] => {
                self.view.selected_marker = None;
                if count == 2 {
                    return Some(self.add_meter_change(pos));
                }
                Ok(())
            }
            ["meter", n] => {
                let n: usize = n.parse().ok()?;
                self.select_marker(Marker::Meter(n));
                self.view.selected_clip = None;
                Ok(())
            }
            _ => return None,
        })
    }

    /// Starts dragging a tempo point or time signature change. `None` if
    /// `parts` isn't one.
    pub(crate) fn start_tempo_gesture(
        &mut self,
        parts: &[&str],
        origin: Point,
    ) -> Option<Result<Option<Gesture>, String>> {
        match parts {
            ["tempo", n] => {
                let index: usize = n.parse().ok()?;
                let points = self.tempo_points();
                let &(beat, bpm, _) = points.get(index)?;
                self.select_marker(Marker::Tempo(index));
                let pointer = self.beat_at_x(origin.x);
                Some(Ok(Some(Gesture::TempoPoint {
                    index,
                    grab_beats: pointer - beat,
                    start_bpm: bpm,
                    beat,
                    bpm,
                    scale: tempo_scale(&points),
                })))
            }
            ["meter", n] => {
                let index: usize = n.parse().ok()?;
                let &(bar, _) = self.meter_points().get(index)?;
                self.select_marker(Marker::Meter(index));
                let start = self.studio.session().project().meter_map().bar_start(bar);
                Some(Ok(Some(Gesture::MeterChange {
                    index,
                    grab_beats: self.beat_at_x(origin.x) - start,
                    bar,
                })))
            }
            _ => None,
        }
    }

    /// Continues or ends a tempo lane drag. `None` for other gestures.
    pub(crate) fn on_tempo_drag(
        &mut self,
        gesture: &Gesture,
        origin: Point,
        pos: Point,
        end: bool,
        modifiers: Modifiers,
    ) -> Option<Result<(), String>> {
        Some(match gesture.clone() {
            Gesture::TempoPoint {
                index,
                grab_beats,
                start_bpm,
                beat: previous_beat,
                scale,
                ..
            } => {
                let points = self.tempo_points();
                let per_px = if modifiers.ctrl {
                    FINE_BPM_PER_PX
                } else {
                    BPM_PER_PX
                };
                let step = if modifiers.ctrl { 0.01 } else { 1.0 };
                let bpm = start_bpm - f64::from(pos.y - origin.y) * per_px;
                let bpm = ((bpm / step).round() * step).clamp(TEMPO_BPM_MIN, TEMPO_BPM_MAX);
                // The start tempo stays at the start.
                let beat = if index == 0 {
                    0.0
                } else {
                    let mut beat = self.beat_at_x(pos.x) - grab_beats;
                    if self.view.snap && !modifiers.shift {
                        beat = snap_beat(self.studio.session().project(), beat);
                    }
                    let beat = beat.max(1e-3);
                    // Never onto another change (it would replace it).
                    let taken = points
                        .iter()
                        .enumerate()
                        .any(|(i, p)| i != index && (p.0 - beat).abs() < 1e-6);
                    if taken { previous_beat } else { beat }
                };
                if !end {
                    self.view.gesture = Some(Gesture::TempoPoint {
                        index,
                        grab_beats,
                        start_bpm,
                        beat,
                        bpm,
                        scale,
                    });
                    return Some(Ok(()));
                }
                self.view.gesture = None;
                let Some(&(old_beat, old_bpm, ramp)) = points.get(index) else {
                    return Some(Ok(()));
                };
                if (old_beat - beat).abs() < 1e-9 && (old_bpm - bpm).abs() < 1e-9 {
                    return Some(Ok(()));
                }
                let mut points = points;
                points[index] = (beat, bpm, ramp);
                let result = self.execute(tempo_map(&points).into());
                self.select_tempo_at(beat);
                result
            }
            Gesture::MeterChange {
                index,
                grab_beats,
                bar: previous_bar,
            } => {
                let points = self.meter_points();
                let bar = if index == 0 {
                    0
                } else {
                    let meter = self.studio.session().project().meter_map();
                    let beat = self.beat_at_x(pos.x) - grab_beats;
                    let bar = meter.bar_at(beat);
                    let (start, next) = (meter.bar_start(bar), meter.bar_start(bar + 1));
                    let nearest = if beat - start > (next - start) / 2.0 {
                        bar + 1
                    } else {
                        bar
                    };
                    let nearest = nearest.max(1);
                    let taken = points
                        .iter()
                        .enumerate()
                        .any(|(i, p)| i != index && p.0 == nearest);
                    if taken { previous_bar } else { nearest }
                };
                if !end {
                    self.view.gesture = Some(Gesture::MeterChange {
                        index,
                        grab_beats,
                        bar,
                    });
                    return Some(Ok(()));
                }
                self.view.gesture = None;
                if points.get(index).is_none_or(|p| p.0 == bar) {
                    return Some(Ok(()));
                }
                let mut points = points;
                points[index].0 = bar;
                let result = self.execute(meter_map(&points).into());
                self.select_meter_at(bar);
                result
            }
            _ => return None,
        })
    }

    /// Keys for the selected tempo point or time signature change: Delete
    /// removes it, Up and Down change it (with Shift, by 10 BPM or the
    /// denominator). `None` if nothing there is selected or it's another
    /// key.
    pub(crate) fn on_tempo_key(&mut self, key: &str, m: Modifiers) -> Option<Result<(), String>> {
        if self.view.page != Page::Arrange || m.ctrl {
            return None;
        }
        let marker = self.view.selected_marker?;
        Some(match (marker, key) {
            (_, "escape") => {
                self.view.selected_marker = None;
                Ok(())
            }
            (Marker::Tempo(n), "delete" | "backspace") => self.remove_tempo(n),
            (Marker::Tempo(n), "up" | "down") => {
                let by = if m.shift { 10.0 } else { 1.0 };
                self.nudge_tempo(n, if key == "up" { by } else { -by })
            }
            (Marker::Meter(n), "delete" | "backspace") => self.remove_meter(n),
            (Marker::Meter(n), "up" | "down") => self.nudge_meter(n, !m.shift, key == "up"),
            _ => return None,
        })
    }

    /// Beat (on the project's tempo map) at window x in the timeline.
    fn beat_at_x(&self, x: f32) -> f64 {
        self.studio
            .session()
            .project()
            .tempo_map()
            .beat_at_seconds(self.time_at(x))
    }

    /// Selects a tempo point or time signature change.
    fn select_marker(&mut self, marker: Marker) {
        self.view.marker_anchor = match marker {
            Marker::Tempo(n) => self.tempo_points().get(n).map_or(f64::NAN, |p| p.0),
            Marker::Meter(n) => self
                .meter_points()
                .get(n)
                .map_or(f64::NAN, |p| f64::from(p.0)),
        };
        self.view.selected_marker = Some(marker);
    }

    fn select_tempo_at(&mut self, beat: f64) {
        let points = self.tempo_points();
        let found = points.iter().position(|p| (p.0 - beat).abs() < 1e-6);
        self.view.marker_anchor = beat;
        self.view.selected_marker = found.map(Marker::Tempo);
    }

    fn select_meter_at(&mut self, bar: u32) {
        let found = self.meter_points().iter().position(|p| p.0 == bar);
        self.view.marker_anchor = f64::from(bar);
        self.view.selected_marker = found.map(Marker::Meter);
    }

    /// After the project changed (an undo, say), finds the selected tempo
    /// point or time signature change again by its beat or bar, as its
    /// index may now be another's; or lets it go if it's gone.
    pub(crate) fn refind_marker(&mut self) {
        let at = self.view.marker_anchor;
        self.view.selected_marker = match self.view.selected_marker {
            Some(Marker::Tempo(_)) => self
                .tempo_points()
                .iter()
                .position(|p| (p.0 - at).abs() < 1e-6)
                .map(Marker::Tempo),
            Some(Marker::Meter(_)) => self
                .meter_points()
                .iter()
                .position(|p| f64::from(p.0) == at)
                .map(Marker::Meter),
            None => None,
        };
    }

    /// Changes tempo point `n` by `by` BPM.
    fn nudge_tempo(&mut self, n: usize, by: f64) -> Result<(), String> {
        let Some(&(beat, bpm, ramp)) = self.tempo_points().get(n) else {
            return Ok(());
        };
        let bpm = (bpm + by).clamp(TEMPO_BPM_MIN, TEMPO_BPM_MAX);
        self.execute(
            SetTempo {
                bpm,
                at_beats: (n > 0).then_some(beat),
                ramp,
            }
            .into(),
        )
    }

    /// Makes tempo change `n` ramp from the previous tempo, or jump.
    fn toggle_ramp(&mut self, n: usize) -> Result<(), String> {
        let Some(&(beat, bpm, ramp)) = self.tempo_points().get(n).filter(|_| n > 0) else {
            return Err("the start tempo can't ramp; select a tempo change".into());
        };
        self.execute(
            SetTempo {
                bpm,
                at_beats: Some(beat),
                ramp: !ramp,
            }
            .into(),
        )
    }

    fn remove_tempo(&mut self, n: usize) -> Result<(), String> {
        let Some(&(beat, ..)) = self.tempo_points().get(n).filter(|_| n > 0) else {
            return Err("the start tempo stays; select a tempo change to remove".into());
        };
        self.view.selected_marker = None;
        self.execute(RemoveTempoChange { at_beats: beat }.into())
    }

    /// Double-clicking the tempo lane adds a change at that beat (on the
    /// beat grid when snapping), at the tempo under the pointer.
    fn add_tempo_change(&mut self, pos: Point) -> Result<(), String> {
        let project = self.studio.session().project();
        let mut beat = self.beat_at_x(pos.x);
        if self.view.snap {
            beat = snap_beat(project, beat);
        }
        if beat < 1e-3 {
            return Err("the start tempo is already there: drag its point to change it".into());
        }
        let lane = self
            .ui
            .find("tempo/lane")
            .map(|n| n.rect())
            .unwrap_or_default();
        let bpm = tempo_at_y(tempo_scale(&self.tempo_points()), pos.y - lane.y).round();
        let result = self.execute(
            SetTempo {
                bpm,
                at_beats: Some(beat),
                ramp: false,
            }
            .into(),
        );
        self.select_tempo_at(beat);
        result
    }

    /// Changes time signature `n`: its numerator by one, or its
    /// denominator to the next power of two.
    fn nudge_meter(&mut self, n: usize, numerator: bool, up: bool) -> Result<(), String> {
        let Some(&(bar, sig)) = self.meter_points().get(n) else {
            return Ok(());
        };
        let (num, den) = (sig.numerator, sig.denominator);
        let (num, den) = match (numerator, up) {
            (true, true) => (num.saturating_add(1).min(32), den),
            (true, false) => (num.saturating_sub(1).max(1), den),
            (false, true) => (num, (den * 2).min(32)),
            (false, false) => (num, (den / 2).max(1)),
        };
        if (num, den) == (sig.numerator, sig.denominator) {
            return Ok(());
        }
        self.execute(
            SetTimeSignature {
                numerator: num,
                denominator: den,
                at_bar: (n > 0).then_some(bar + 1),
            }
            .into(),
        )
    }

    fn remove_meter(&mut self, n: usize) -> Result<(), String> {
        let Some(&(bar, _)) = self.meter_points().get(n).filter(|_| n > 0) else {
            return Err(
                "the start time signature stays; select a time signature change to remove".into(),
            );
        };
        self.view.selected_marker = None;
        self.execute(RemoveTimeSignatureChange { at_bar: bar + 1 }.into())
    }

    /// Double-clicking the time signature lane adds a change at the bar
    /// line nearest the pointer, with the time signature already there.
    fn add_meter_change(&mut self, pos: Point) -> Result<(), String> {
        let meter = self.studio.session().project().meter_map();
        let beat = self.beat_at_x(pos.x);
        let bar = meter.bar_at(beat);
        let (start, next) = (meter.bar_start(bar), meter.bar_start(bar + 1));
        let bar = if beat - start > (next - start) / 2.0 {
            bar + 1
        } else {
            bar
        };
        if bar == 0 {
            return Err(
                "the first bar has the start time signature: change it in the header".into(),
            );
        }
        let sig = meter.meter_of_bar(bar);
        let result = self.execute(
            SetTimeSignature {
                numerator: sig.numerator,
                denominator: sig.denominator,
                at_bar: Some(bar + 1),
            }
            .into(),
        );
        self.select_meter_at(bar);
        result
    }
}

/// The `set_tempo_map` for these points (the first is the start).
fn tempo_map(points: &[TempoPoint]) -> SetTempoMap {
    SetTempoMap {
        bpm: points[0].1,
        changes: points[1..]
            .iter()
            .map(|&(beat, bpm, ramp)| TempoChange { beat, bpm, ramp })
            .collect(),
    }
}

/// The `set_time_signature_map` for these (bar from 0, time signature)
/// points (the first is the start).
fn meter_map(points: &[(u32, TimeSignature)]) -> SetTimeSignatureMap {
    let start = points[0].1;
    SetTimeSignatureMap {
        numerator: start.numerator,
        denominator: start.denominator,
        changes: points[1..]
            .iter()
            .map(|&(bar, sig)| MeterChange {
                bar: bar + 1,
                numerator: sig.numerator,
                denominator: sig.denominator,
            })
            .collect(),
    }
}
