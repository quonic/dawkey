// SPDX-License-Identifier: AGPL-3.0-or-later
//! What clicks, drags, scrolling and keys do with plugins (the mixer's slots,
//! the picker, the editor) and automation lanes. Changes go through
//! commands, one undo step per gesture.

use dawkey_core::{
    AddAutomationPoint, AddPlugin, AutomationTarget, MASTER, PluginId, PointSpec, RemovePlugin,
    SetAutomation, SetPluginBypass, SetPluginOutput, SetPluginParam, SetPluginSidechain, TrackId,
    TrackKind,
};
use dawkey_ui::{Modifiers, Point};

use crate::App;
use crate::state::{AUTO_H, Gesture, PARAM_W, TextField};
use crate::view::plugins::{content_height, thumb};
use crate::view::{LaneKind, lane_label_kind};

/// An automation lane: its target, scale, and (time, value) points.
struct LaneData {
    target: AutomationTarget,
    kind: LaneKind,
    points: Vec<(f64, f64)>,
}

impl App {
    /// Clicks on plugin and automation widgets. `None` if `parts` isn't
    /// one of theirs.
    pub(crate) fn on_plugin_click(
        &mut self,
        parts: &[&str],
        pos: Point,
        count: u8,
        modifiers: Modifiers,
    ) -> Option<Result<(), String>> {
        Some(match parts {
            ["overlay", "backdrop"] | ["plugin_picker", "close"] | ["plugin_editor", "close"] => {
                self.close_overlays();
                Ok(())
            }
            ["track", track, "add_plugin"] => {
                let track = TrackId(track.parse().ok()?);
                self.close_overlays();
                self.view.plugin_picker = Some(track);
                Ok(())
            }
            ["plugin_picker", "filter", which] => {
                self.view.picker_filter = match *which {
                    "instruments" => Some(true),
                    "effects" => Some(false),
                    _ => None,
                };
                self.view.overlay_scroll = 0.0;
                self.view.picker_selected = 0;
                Ok(())
            }
            // It has the caret whenever the picker is open.
            ["plugin_picker", "search"] => Ok(()),
            [list @ ("plugin_picker" | "plugin_editor"), "scrollbar"] => {
                // Jumps there; dragging goes on from there.
                self.start_overlay_scroll(list, pos);
                Ok(())
            }
            ["plugin_picker", "rescan"] => {
                // On a worker thread in the window; `tick` says when it's
                // done.
                self.studio.rescan_plugins_in_background();
                if self.studio.scanning_plugins() {
                    self.message = Some("Scanning for plugins…".to_owned());
                } else {
                    let found = self.studio.cached_catalog().map_or(0, |c| c.plugins.len());
                    self.message = Some(format!("Found {found} plugins"));
                }
                Ok(())
            }
            ["plugin_picker", "item", n] => {
                let n: usize = n.parse().ok()?;
                self.add_picked(n)
            }
            ["plugin", id] => {
                let id = PluginId(id.parse().ok()?);
                self.close_overlays();
                self.view.plugin_editor = Some(id);
                Ok(())
            }
            ["plugin", id, "bypass"] => {
                let plugin = PluginId(id.parse().ok()?);
                let bypass = !self.slot(plugin)?.bypass;
                self.execute(SetPluginBypass { plugin, bypass }.into())
            }
            ["plugin", id, "sidechain"] => {
                let plugin = PluginId(id.parse().ok()?);
                self.cycle_sidechain(plugin)
            }
            ["plugin", id, "output", n] => {
                let plugin = PluginId(id.parse().ok()?);
                self.cycle_plugin_output(plugin, n.parse().ok()?)
            }
            ["plugin", id, "gui"] => {
                let plugin = PluginId(id.parse().ok()?);
                let visible = self
                    .studio
                    .plugin_details(plugin)
                    .is_some_and(|d| d.gui_visible);
                self.studio
                    .show_plugin_gui(plugin, !visible)
                    .map_err(|e| e.to_string())
            }
            ["plugin", id, "reload"] => {
                let plugin = PluginId(id.parse().ok()?);
                self.studio.reload_plugin(plugin).map_err(|e| e.to_string())
            }
            ["plugin", id, "remove"] => {
                let plugin = PluginId(id.parse().ok()?);
                self.close_overlays();
                self.execute(RemovePlugin { plugin }.into())
            }
            ["plugin", id, "param", param] if count == 2 => {
                // Double-click: type a value.
                let plugin = PluginId(id.parse().ok()?);
                self.start_text_edit(TextField::PluginParam {
                    plugin,
                    param: (*param).to_owned(),
                })
            }
            ["plugin", id, "param", param] if modifiers.ctrl => {
                // Ctrl-click: back to the default.
                let plugin = PluginId(id.parse().ok()?);
                let p = self.studio.plugin_param(plugin, param)?;
                self.execute(SetPluginParam::new(plugin, p.id.clone(), p.default).into())
            }
            ["plugin", id, "param", param, "automate"] => {
                let plugin = PluginId(id.parse().ok()?);
                self.automate_param(plugin, param)
            }
            ["track", track, "automation"] => {
                let track = TrackId(track.parse().ok()?);
                self.toggle_automation(track)
            }
            ["track", track, "auto", k, "lane"] => {
                let track = TrackId(track.parse().ok()?);
                let k: usize = k.parse().ok()?;
                self.view.selected_point = None;
                if count == 2 {
                    self.add_point_at(track, k, pos)
                } else {
                    self.locate_to(pos.x)
                }
            }
            ["auto", track, k, i] => {
                let track = TrackId(track.parse().ok()?);
                self.select_point(track, k.parse().ok()?, i.parse().ok()?);
                Ok(())
            }
            _ => return None,
        })
    }

    /// Every tick: parameters plugins changed themselves (on their own
    /// GUIs) go into the project as the editor's sliders do: one undo step
    /// per gesture, and into the automation lane while writing it.
    pub(crate) fn apply_plugin_param_changes(&mut self) {
        use dawkey_studio::ParamChange;
        for (plugin, change) in self.studio.take_plugin_param_changes() {
            let (ParamChange::Begin { index }
            | ParamChange::Value { index, .. }
            | ParamChange::End { index }) = change;
            let Some(param) = self.studio.plugin_param_id(plugin, index) else {
                continue;
            };
            let project = self.studio.session().project();
            let Some((track, slot)) = project.plugin(plugin) else {
                continue;
            };
            let (track, current) = (track.id, slot.params.get(&param).copied());
            match change {
                ParamChange::Begin { .. } => {}
                ParamChange::Value { value, .. } => {
                    // The project has it already (the plugin echoing it).
                    if current == Some(value) {
                        continue;
                    }
                    let key = format!("plugin {plugin} param {param} on the plugin");
                    let target = AutomationTarget::Param {
                        plugin,
                        param: param.clone(),
                    };
                    let set = SetPluginParam::new(plugin, param, value);
                    if let Err(e) = self.control_moved(track, target, value, &key, set.into()) {
                        self.message = Some(e);
                    }
                }
                // The gesture's undo step ends (unless one of ours goes on).
                ParamChange::End { .. } => {
                    if self.view.gesture.is_none() {
                        self.release_automation();
                    }
                }
            }
        }
    }

    /// Feeds a plugin's sidechain from the next track that can: none, then
    /// each track and bus that doesn't take the plugin's signal.
    fn cycle_sidechain(&mut self, plugin: PluginId) -> Result<(), String> {
        let project = self.studio.session().project();
        let (track, slot) = project.plugin(plugin).ok_or("no such plugin")?;
        let mut choices = vec![None];
        choices.extend(
            project
                .tracks
                .iter()
                .filter(|t| t.id != track.id && !project.depends_on(t.id, track.id))
                .map(|t| Some(t.id)),
        );
        let at = choices
            .iter()
            .position(|&c| c == slot.sidechain)
            .unwrap_or(0);
        let source = choices[(at + 1) % choices.len()];
        self.execute(SetPluginSidechain { plugin, source }.into())
    }

    /// Sends a plugin's other output to the next bus that can take it, or
    /// nowhere after the last.
    fn cycle_plugin_output(&mut self, plugin: PluginId, output: u32) -> Result<(), String> {
        let project = self.studio.session().project();
        let (track, slot) = project.plugin(plugin).ok_or("no such plugin")?;
        let mut choices = vec![None];
        choices.extend(
            project
                .tracks
                .iter()
                .filter(|t| t.kind == TrackKind::Bus && t.id != MASTER)
                .filter(|t| !project.depends_on(track.id, t.id))
                .map(|t| Some(t.id)),
        );
        let current = slot.outputs.get(&output).copied();
        let at = choices.iter().position(|&c| c == current).unwrap_or(0);
        let bus = choices[(at + 1) % choices.len()];
        if bus == current {
            return Err("there's no bus to send it to (+ Bus makes one)".into());
        }
        self.execute(
            SetPluginOutput {
                plugin,
                output,
                bus,
            }
            .into(),
        )
    }

    /// Starts dragging a plugin parameter or an automation point.
    pub(crate) fn start_plugin_gesture(
        &mut self,
        parts: &[&str],
        merge_key: String,
    ) -> Option<Result<Option<Gesture>, String>> {
        Some(Ok(match parts {
            ["plugin", id, "param", param] => {
                let plugin = PluginId(id.parse().ok()?);
                let p = self.studio.plugin_param(plugin, param)?;
                Some(Gesture::PluginParam {
                    plugin,
                    param: p.id.clone(),
                    start_value: p.value,
                    min: p.min,
                    max: p.max,
                    stepped: p.stepped,
                    merge_key,
                })
            }
            ["auto", track, k, i] => {
                let track = TrackId(track.parse().ok()?);
                let (lane, index): (usize, usize) = (k.parse().ok()?, i.parse().ok()?);
                let project = self.studio.session().project();
                let point = project
                    .track(track)?
                    .automation
                    .get(lane)?
                    .points
                    .get(index)?;
                let (time_s, value) = (project.frames_to_seconds(point.frame), point.value);
                self.select_point(track, lane, index);
                Some(Gesture::AutoPoint {
                    track,
                    lane,
                    index,
                    time_s,
                    value,
                })
            }
            _ => return None,
        }))
    }

    /// Continues or ends a plugin or automation gesture. `None` for others.
    pub(crate) fn on_plugin_drag(
        &mut self,
        gesture: &Gesture,
        origin: Point,
        pos: Point,
        end: bool,
        modifiers: Modifiers,
    ) -> Option<Result<(), String>> {
        Some(match gesture.clone() {
            Gesture::OverlayScroll {
                start_scroll,
                per_px,
                max,
            } => {
                self.view.overlay_scroll =
                    (start_scroll + (pos.y - origin.y) * per_px).clamp(0.0, max);
                if end {
                    self.view.gesture = None;
                }
                Ok(())
            }
            Gesture::PluginParam {
                plugin,
                param,
                start_value,
                min,
                max,
                stepped,
                merge_key,
            } => {
                let mut value = (start_value
                    + f64::from((pos.x - origin.x) / PARAM_W) * (max - min))
                    .clamp(min, max);
                if stepped {
                    value = value.round();
                }
                let track = self
                    .studio
                    .session()
                    .project()
                    .plugin(plugin)
                    .map(|(t, _)| t.id)
                    .ok_or_else(|| format!("plugin {plugin} does not exist"));
                let result = track.and_then(|track| {
                    let target = AutomationTarget::Param {
                        plugin,
                        param: param.clone(),
                    };
                    let set = SetPluginParam::new(plugin, param, value);
                    self.control_moved(track, target, value, &merge_key, set.into())
                });
                if end {
                    self.view.gesture = None;
                    self.release_automation();
                }
                result
            }
            Gesture::AutoPoint {
                track, lane, index, ..
            } => {
                let kind = self.lane(track, lane)?.kind;
                let mut time_s = self.time_at(pos.x).max(0.0);
                if !modifiers.shift {
                    time_s = self.snap_to_beat(time_s);
                }
                let value = self.lane_value_at(track, lane, kind, pos.y)?;
                if !end {
                    self.view.gesture = Some(Gesture::AutoPoint {
                        track,
                        lane,
                        index,
                        time_s,
                        value,
                    });
                    return Some(Ok(()));
                }
                self.view.gesture = None;
                let LaneData {
                    target, mut points, ..
                } = self.lane(track, lane)?;
                if index >= points.len() {
                    return Some(Ok(()));
                }
                points[index] = (time_s, value);
                self.view.selected_point = None;
                self.set_lane(track, target, points)
            }
            _ => return None,
        })
    }

    /// Scrolls the picker or editor list. Returns false if `id` is neither.
    pub(crate) fn on_overlay_scroll(&mut self, id: Option<&str>, delta: Point) -> bool {
        let list = match id {
            Some("plugin_picker/list" | "plugin_picker/scrollbar") => "plugin_picker",
            Some("plugin_editor/params" | "plugin_editor/scrollbar") => "plugin_editor",
            _ => return false,
        };
        let (_, max) = self.overlay_extent(list);
        self.view.overlay_scroll = (self.view.overlay_scroll + delta.y).clamp(0.0, max);
        true
    }

    /// The picker's or editor's list: how high it shows, and how far it
    /// scrolls.
    fn overlay_extent(&mut self, list: &str) -> (f32, f32) {
        let rows = match list {
            "plugin_picker" => self.picker_ids().len(),
            _ => self
                .view
                .plugin_editor
                .and_then(|id| self.studio.plugin_details_window(id, 0, 0))
                .map_or(0, |d| d.param_count),
        };
        let view_id = match list {
            "plugin_picker" => "plugin_picker/list",
            _ => "plugin_editor/params",
        };
        let visible = self.ui.find(view_id).map_or(400.0, |n| n.rect().h);
        (visible, (content_height(rows) - visible).max(0.0))
    }

    /// Pressing the list's scroll bar (`<list>/scrollbar`) at `pos`: off
    /// the thumb, the list jumps so the thumb's middle is there. Returns
    /// the drag that goes on from there.
    pub(crate) fn start_overlay_scroll(&mut self, list: &str, pos: Point) -> Option<Gesture> {
        let bar = self.ui.find(&format!("{list}/scrollbar"))?.rect();
        let (visible, max) = self.overlay_extent(list);
        let content = max + visible;
        let (y, h) = thumb(self.view.overlay_scroll, content, visible, bar.h)?;
        let at = pos.y - bar.y;
        let travel = (bar.h - h).max(1.0);
        if at < y || at > y + h {
            let top = (at - h / 2.0).clamp(0.0, travel);
            self.view.overlay_scroll = top / travel * max;
        }
        Some(Gesture::OverlayScroll {
            start_scroll: self.view.overlay_scroll,
            per_px: max / travel,
            max,
        })
    }

    /// Keys for automation points and overlays. `None` for others.
    pub(crate) fn on_plugin_key(&mut self, key: &str, m: Modifiers) -> Option<Result<(), String>> {
        if let Some(result) = self.on_picker_key(key, m) {
            return Some(result);
        }
        match (key, m.ctrl) {
            ("escape", false)
                if self.view.plugin_picker.is_some() || self.view.plugin_editor.is_some() =>
            {
                self.close_overlays();
                Some(Ok(()))
            }
            ("delete" | "backspace", false) => {
                let (track, lane, index) = self.view.selected_point.take()?;
                let LaneData {
                    target, mut points, ..
                } = self.lane(track, lane)?;
                if index < points.len() {
                    points.remove(index);
                }
                Some(self.set_lane(track, target, points))
            }
            _ => None,
        }
    }

    /// Selects point `index` of the track's automation lane `lane`.
    fn select_point(&mut self, track: TrackId, lane: usize, index: usize) {
        let project = self.studio.session().project();
        self.view.point_anchor = project
            .track(track)
            .and_then(|t| t.automation.get(lane))
            .and_then(|l| Some((l.target.clone(), l.points.get(index)?.frame)));
        self.view.selected_point = Some((track, lane, index));
    }

    /// After the project changed (an undo, say), finds the selected
    /// automation point again by its lane's target and frame, as its
    /// indices may now be another's; or lets it go if it's gone.
    pub(crate) fn refind_point(&mut self) {
        let Some((track, ..)) = self.view.selected_point else {
            return;
        };
        let project = self.studio.session().project();
        self.view.selected_point = self.view.point_anchor.as_ref().and_then(|(target, frame)| {
            let automation = &project.track(track)?.automation;
            let lane = automation.iter().position(|l| l.target == *target)?;
            let index = automation[lane]
                .points
                .iter()
                .position(|p| p.frame == *frame)?;
            Some((track, lane, index))
        });
    }

    pub(crate) fn close_overlays(&mut self) {
        self.view.export_dialog = false;
        self.view.prefs_open = false;
        self.view.plugin_picker = None;
        self.view.picker_query.clear();
        self.view.picker_selected = 0;
        self.view.plugin_editor = None;
        self.view.overlay_scroll = 0.0;
    }

    /// Adds the `n`th plugin the picker shows to its track and opens it.
    pub(crate) fn add_picked(&mut self, n: usize) -> Result<(), String> {
        let Some(track) = self.view.plugin_picker else {
            return Ok(());
        };
        let id = self
            .picker_ids()
            .into_iter()
            .nth(n)
            .ok_or("no such plugin")?;
        // Instruments go first on a track, so they play its notes.
        let instrument = self
            .studio
            .plugin_catalog()
            .get(&id)
            .is_some_and(|p| p.instrument);
        let outcome = self
            .studio
            .execute(
                AddPlugin {
                    track,
                    plugin: id,
                    index: instrument.then_some(0),
                }
                .into(),
            )
            .map_err(|e| e.to_string())?;
        self.close_overlays();
        self.view.plugin_editor = outcome.plugin;
        Ok(())
    }

    /// Shows the lane for a parameter, adding one (with the current value
    /// at the playhead) if there isn't one.
    fn automate_param(&mut self, plugin: PluginId, param: &str) -> Result<(), String> {
        let (track, _) = self
            .studio
            .session()
            .project()
            .plugin(plugin)
            .ok_or("no such plugin")?;
        let track = track.id;
        let p = self
            .studio
            .plugin_param(plugin, param)
            .ok_or("no such parameter")?;
        if !p.automated {
            let time_s = self.studio.transport().position_s;
            self.execute(
                AddAutomationPoint {
                    track,
                    target: AutomationTarget::Param {
                        plugin,
                        param: p.id.clone(),
                    },
                    time_s,
                    value: p.value,
                }
                .into(),
            )?;
        }
        if !self.view.automation_shown.contains(&track) {
            self.view.automation_shown.push(track);
        }
        self.message = Some(format!(
            "{} is automated; edit it in the arrange view",
            p.name
        ));
        Ok(())
    }

    /// Shows or hides a track's automation. A track without lanes gets a
    /// gain lane to start with.
    fn toggle_automation(&mut self, track: TrackId) -> Result<(), String> {
        if let Some(i) = self.view.automation_shown.iter().position(|t| *t == track) {
            self.view.automation_shown.remove(i);
            return Ok(());
        }
        let current = self
            .studio
            .session()
            .project()
            .track(track)
            .ok_or_else(|| format!("track {track} does not exist"))?;
        if current.automation.is_empty() {
            let value = f64::from(current.gain_db);
            self.execute(
                AddAutomationPoint {
                    track,
                    target: AutomationTarget::Gain,
                    time_s: 0.0,
                    value,
                }
                .into(),
            )?;
        }
        self.view.automation_shown.push(track);
        Ok(())
    }

    /// Double-click on a lane: a point there.
    fn add_point_at(&mut self, track: TrackId, lane: usize, pos: Point) -> Result<(), String> {
        let Some(LaneData { target, kind, .. }) = self.lane(track, lane) else {
            return Ok(());
        };
        let time_s = self.snap_to_beat(self.time_at(pos.x).max(0.0));
        let Some(value) = self.lane_value_at(track, lane, kind, pos.y) else {
            return Ok(());
        };
        self.execute(
            AddAutomationPoint {
                track,
                target,
                time_s,
                value,
            }
            .into(),
        )
    }

    fn locate_to(&mut self, x: f32) -> Result<(), String> {
        let t = self.time_at(x).max(0.0);
        self.studio.locate(t).map_err(|e| e.to_string())
    }

    /// Lane `lane` of `track`.
    fn lane(&mut self, track: TrackId, lane: usize) -> Option<LaneData> {
        let project = self.studio.session().project();
        let t = project.track(track)?;
        let l = t.automation.get(lane)?;
        let points = l
            .points
            .iter()
            .map(|p| (project.frames_to_seconds(p.frame), p.value))
            .collect();
        let (_, kind) = lane_label_kind(t, &l.target, self.studio.cached_catalog());
        Some(LaneData {
            target: l.target.clone(),
            kind,
            points,
        })
    }

    /// The lane value at window y.
    fn lane_value_at(&self, track: TrackId, lane: usize, kind: LaneKind, y: f32) -> Option<f64> {
        let rect = self
            .ui
            .find(&format!("track/{track}/auto/{lane}/lane"))?
            .rect();
        let h = AUTO_H - 1.0;
        let unit = 1.0 - (y - rect.y - 4.0) / (h - 8.0);
        Some(kind.value(unit))
    }

    fn set_lane(
        &mut self,
        track: TrackId,
        target: AutomationTarget,
        points: Vec<(f64, f64)>,
    ) -> Result<(), String> {
        self.execute(
            SetAutomation {
                track,
                target,
                points: points
                    .into_iter()
                    .map(|(time_s, value)| PointSpec { time_s, value })
                    .collect(),
            }
            .into(),
        )
    }

    fn slot(&self, id: PluginId) -> Option<&dawkey_core::PluginSlot> {
        self.studio.session().project().plugin(id).map(|(_, s)| s)
    }
}
