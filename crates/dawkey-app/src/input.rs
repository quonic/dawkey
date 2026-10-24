// SPDX-License-Identifier: AGPL-3.0-or-later
//! What clicks, drags, scrolling and keys do. Everything that changes the
//! project goes through studio commands, so it's undoable like any MCP call.

use dawkey_core::{
    AddTrack, AutomationTarget, ClipId, Command, MoveClip, Project, RemoveClip, SetLoop, SetPunch,
    SetTrackArm, SetTrackGain, SetTrackMute, SetTrackOutput, SetTrackPan, SetTrackSolo, TrackId,
    TrackKind,
};
use dawkey_ui::{ChromeAction, DragPhase, Modifiers, MouseButton, Point, Rect, UiAction};

use crate::App;
use crate::state::{
    ClipEdge, FADER_H, Gesture, ImportRequest, MAX_ZOOM, MIN_ZOOM, PAN_W, Page, PendingAction,
    PlatformRequest, RangeKind, TextField, takes_shown, track_tops,
};
use crate::view::{db_from_position, position_from_db};

/// How much one pixel of scroll changes the zoom.
const ZOOM_PER_PX: f32 = 0.004;

impl App {
    pub(crate) fn apply(&mut self, actions: Vec<UiAction>) -> Vec<ChromeAction> {
        let mut chrome = Vec::new();
        for action in actions {
            let result = match action {
                UiAction::Click {
                    id,
                    pos,
                    button,
                    modifiers,
                    count,
                } => self.on_click(id.as_str(), pos, button, count, modifiers),
                UiAction::Drag {
                    id,
                    phase,
                    origin,
                    pos,
                    modifiers,
                } => self.on_drag(id.as_str(), phase, origin, pos, modifiers),
                UiAction::Scroll {
                    id,
                    pos,
                    delta,
                    modifiers,
                } => {
                    self.on_scroll(id.as_ref().map(|i| i.as_str()), pos, delta, modifiers);
                    Ok(())
                }
                UiAction::Key { key, modifiers } => self.on_key(&key, modifiers),
                // Closing asks first when there are unsaved changes.
                UiAction::Chrome(ChromeAction::Close) => {
                    self.request_action(PendingAction::Close);
                    Ok(())
                }
                UiAction::Chrome(action) => {
                    chrome.push(action);
                    Ok(())
                }
            };
            if let Err(e) = result {
                tracing::warn!("{e}");
                self.message = Some(e);
            }
        }
        if std::mem::take(&mut self.close_requested) {
            chrome.push(ChromeAction::Close);
        }
        chrome
    }

    fn on_click(
        &mut self,
        id: &str,
        pos: Point,
        button: MouseButton,
        count: u8,
        modifiers: Modifiers,
    ) -> Result<(), String> {
        if button != MouseButton::Left {
            return Ok(());
        }
        self.leave_text_edit(id)?;
        let parts: Vec<&str> = id.split('/').collect();
        if let Some(result) = self.on_recovery_click(&parts) {
            return result;
        }
        if let Some(result) = self.on_confirm_click(&parts) {
            return result;
        }
        if let Some(result) = self.on_prefs_click(&parts) {
            return result;
        }
        if let Some(result) = self.on_export_click(&parts) {
            return result;
        }
        if let Some(result) = self.on_plugin_click(&parts, pos, count, modifiers) {
            return result;
        }
        if let Some(result) = self.on_roll_click(&parts, pos, count, modifiers) {
            return result;
        }
        if let Some(result) = self.on_tempo_click(&parts, pos, count) {
            return result;
        }
        if let Some(result) = self.on_take_click(&parts) {
            return result;
        }
        let studio = &mut self.studio;
        match parts[..] {
            ["transport", "play"] => if studio.transport().playing {
                studio.stop()
            } else {
                studio.play()
            }
            .map_err(|e| e.to_string()),
            ["transport", "stop"] => {
                // Stop; a second stop returns to the start.
                if studio.transport().playing {
                    studio.stop()
                } else {
                    studio.locate(0.0)
                }
                .map_err(|e| e.to_string())
            }
            ["view", "arrange"] => {
                self.view.page = Page::Arrange;
                Ok(())
            }
            ["view", "mixer"] => {
                self.view.page = Page::Mixer;
                Ok(())
            }
            ["view", "piano_roll"] => match self.editable_clip() {
                Some(clip) => self.open_piano_roll(clip),
                None => Err("select a MIDI clip to edit".into()),
            },
            ["toolbar", "snap"] => {
                self.view.snap = !self.view.snap;
                Ok(())
            }
            ["toolbar", "split"] => self.split_selected(),
            ["toolbar", "import"] => {
                let position_s = self.studio.transport().position_s;
                self.platform_requests
                    .push(PlatformRequest::ChooseAudioFiles {
                        track: None,
                        position_s,
                    });
                Ok(())
            }
            ["arrange", "add_track"] => self.execute(AddTrack::default().into()),
            ["arrange", "add_midi_track"] => self.execute(
                AddTrack {
                    kind: TrackKind::Midi,
                    ..Default::default()
                }
                .into(),
            ),
            ["arrange", "add_bus"] => self.execute(
                AddTrack {
                    kind: TrackKind::Bus,
                    ..Default::default()
                }
                .into(),
            ),
            ["track", track, "output"] => {
                let track = parse_track(track)?;
                self.cycle_output(track)
            }
            ["transport", "record"] => self.toggle_record(),
            ["transport", "loop"] => self.toggle_region(RangeKind::Loop),
            ["transport", "punch"] => self.toggle_region(RangeKind::Punch),
            ["transport", "automation"] => {
                self.view.automation_write = !self.view.automation_write;
                Ok(())
            }
            ["transport", "automation_mode"] => {
                self.cycle_automation_mode();
                Ok(())
            }
            ["track", track, toggle @ ("mute" | "solo" | "arm")] => {
                let track = parse_track(track)?;
                let current = self
                    .studio
                    .session()
                    .project()
                    .track(track)
                    .ok_or_else(|| format!("track {track} does not exist"))?;
                let command: Command = match toggle {
                    "mute" => SetTrackMute {
                        track,
                        mute: !current.mute,
                    }
                    .into(),
                    "solo" => SetTrackSolo {
                        track,
                        solo: !current.solo,
                    }
                    .into(),
                    _ => SetTrackArm {
                        track,
                        armed: !current.armed,
                    }
                    .into(),
                };
                self.execute(command)
            }
            // Double-clicking a track's name types a new one.
            ["track", track, "name"] if count == 2 => {
                let field = TextField::TrackName(parse_track(track)?);
                match self.editing(&field) {
                    true => Ok(()),
                    false => self.start_text_edit(field),
                }
            }
            // An edge handle (or the name) clicked without dragging is the
            // clip. Double-clicking the name types a new one.
            ["clip", clip] | ["clip", clip, "start" | "end" | "name"] => {
                let clip = ClipId(parse_id(clip)?);
                let name = parts.get(2) == Some(&"name");
                if name && self.editing(&TextField::ClipName(clip)) {
                    return Ok(());
                }
                self.view.selected_clip = Some(clip);
                self.view.selected_marker = None;
                if count == 2 && name {
                    return self.start_text_edit(TextField::ClipName(clip));
                }
                let midi = self.clip_kind(clip) == Some(TrackKind::Midi);
                if count == 2 && midi {
                    self.open_piano_roll(clip)?;
                }
                Ok(())
            }
            ["timeline", "ruler"] => self.locate_at(pos.x),
            ["timeline", "lanes"] => {
                self.view.selected_clip = None;
                self.view.selected_marker = None;
                // Double-clicking an empty MIDI lane makes a clip there.
                if count == 2
                    && let Some(track) = self.track_at(pos.y)
                    && !self.is_audio_track(track)
                {
                    return self.create_midi_clip(track, pos.x);
                }
                self.locate_at(pos.x)
            }
            _ => Ok(()),
        }
    }

    fn on_drag(
        &mut self,
        id: &str,
        phase: DragPhase,
        origin: Point,
        pos: Point,
        modifiers: Modifiers,
    ) -> Result<(), String> {
        if phase == DragPhase::Start {
            self.leave_text_edit(id)?;
            self.view.gesture = self.start_gesture(id, origin, modifiers)?;
            if let Some(gesture) = self.view.gesture.clone() {
                self.touch(&gesture)?;
            }
            return Ok(());
        }
        let Some(gesture) = self.view.gesture.clone() else {
            return Ok(());
        };
        let end = phase == DragPhase::End;
        if let Some(result) = self.on_roll_drag(&gesture, origin, pos, end, modifiers) {
            return result;
        }
        if let Some(result) = self.on_plugin_drag(&gesture, origin, pos, end, modifiers) {
            return result;
        }
        if let Some(result) = self.on_tempo_drag(&gesture, origin, pos, end, modifiers) {
            return result;
        }
        if let Gesture::Comp { .. } = gesture {
            return self.on_comp_drag(gesture, pos, end, modifiers);
        }
        match gesture {
            Gesture::Clip {
                clip,
                grab_s,
                preview_track,
                ..
            } => {
                let mut start = (self.time_at(pos.x) - grab_s).max(0.0);
                if self.view.snap && !modifiers.shift {
                    start = snap(self.studio.session().project(), start);
                }
                // Clips only move onto tracks of their own kind.
                let kind = self.clip_kind(clip);
                let track = self
                    .track_at(pos.y)
                    .filter(|t| self.studio.session().project().track(*t).map(|t| t.kind) == kind)
                    .unwrap_or(preview_track);
                // Ctrl leaves the clip where it is and drops a copy.
                let copy = modifiers.ctrl;
                let start = self.free_start_s(clip, track, start, !copy);
                if end {
                    self.view.gesture = None;
                    if copy {
                        return self.duplicate_clip(clip, track, start);
                    }
                    let project = self.studio.session().project();
                    let (current_track, current) = project
                        .clip(clip)
                        .map(|(t, c)| (t.id, project.frames_to_seconds(c.start)))
                        .ok_or_else(|| format!("clip {clip} no longer exists"))?;
                    if track == current_track && (start - current).abs() < 1e-9 {
                        return Ok(());
                    }
                    return self.execute(
                        MoveClip {
                            clip,
                            position_s: Some(start),
                            track: (track != current_track).then_some(track),
                        }
                        .into(),
                    );
                }
                self.view.gesture = Some(Gesture::Clip {
                    clip,
                    grab_s,
                    preview_start_s: start,
                    preview_track: track,
                    copy,
                });
                Ok(())
            }
            Gesture::TrimClip { .. } => self.on_trim_drag(gesture, pos, end, modifiers),
            Gesture::Repeat { .. } => self.on_repeat_drag(gesture, pos, end, modifiers),
            Gesture::Fader {
                track,
                start_db,
                merge_key,
            } => {
                let p = position_from_db(start_db) - (pos.y - origin.y) / FADER_H;
                let gain_db = db_from_position(p);
                let result = self.control_moved(
                    track,
                    AutomationTarget::Gain,
                    f64::from(gain_db),
                    &merge_key,
                    SetTrackGain { track, gain_db }.into(),
                );
                if end {
                    self.end_gesture();
                }
                result
            }
            Gesture::Pan {
                track,
                start_pan,
                merge_key,
            } => {
                let pan = (start_pan + (pos.x - origin.x) / (PAN_W / 2.0)).clamp(-1.0, 1.0);
                let pan = (pan * 100.0).round() / 100.0;
                let result = self.control_moved(
                    track,
                    AutomationTarget::Pan,
                    f64::from(pan),
                    &merge_key,
                    SetTrackPan { track, pan }.into(),
                );
                if end {
                    self.end_gesture();
                }
                result
            }
            Gesture::Scrub => {
                if end {
                    self.view.gesture = None;
                }
                self.locate_at(pos.x)
            }
            Gesture::MoveNotes { .. }
            | Gesture::ResizeNote { .. }
            | Gesture::Marquee { .. }
            | Gesture::Velocity { .. }
            | Gesture::ControlPoint { .. }
            | Gesture::Paint { .. }
            | Gesture::SynthParam { .. }
            | Gesture::DeviceLatency { .. }
            | Gesture::TempoPoint { .. }
            | Gesture::MeterChange { .. }
            | Gesture::PluginParam { .. }
            | Gesture::OverlayScroll { .. }
            | Gesture::AutoPoint { .. }
            | Gesture::Comp { .. } => {
                unreachable!(
                    "handled by on_roll_drag, on_plugin_drag, on_tempo_drag and on_comp_drag"
                )
            }
            Gesture::Range { kind, anchor_s, .. } => {
                let current_s = self.snap_to_beat(self.time_at(pos.x).max(0.0));
                if !end {
                    self.view.gesture = Some(Gesture::Range {
                        kind,
                        anchor_s,
                        current_s,
                    });
                    return Ok(());
                }
                self.view.gesture = None;
                let (start, end) = (anchor_s.min(current_s), anchor_s.max(current_s));
                if end - start < 1e-3 {
                    return Ok(());
                }
                let (start_s, end_s, enabled) = (Some(start), Some(end), Some(true));
                self.execute(match kind {
                    RangeKind::Loop => SetLoop {
                        start_s,
                        end_s,
                        enabled,
                    }
                    .into(),
                    RangeKind::Punch => SetPunch {
                        start_s,
                        end_s,
                        enabled,
                    }
                    .into(),
                })
            }
        }
    }

    /// Sends a track to the next place it can go: the master, then each
    /// bus in turn that doesn't feed it.
    fn cycle_output(&mut self, track: TrackId) -> Result<(), String> {
        let project = self.studio.session().project();
        let current = project
            .track(track)
            .ok_or_else(|| format!("track {track} does not exist"))?
            .output;
        let mut choices: Vec<Option<TrackId>> = vec![None];
        choices.extend(
            project
                .tracks
                .iter()
                .filter(|t| t.kind == TrackKind::Bus && !project.depends_on(track, t.id))
                .map(|t| Some(t.id)),
        );
        let at = choices.iter().position(|&c| c == current).unwrap_or(0);
        let bus = choices[(at + 1) % choices.len()];
        if bus == current {
            return Err("there's no bus to send it to (+ Bus makes one)".into());
        }
        self.execute(SetTrackOutput { track, bus }.into())
    }

    fn toggle_record(&mut self) -> Result<(), String> {
        let result = if self.studio.transport().recording {
            self.studio.stop()
        } else {
            self.studio.record()
        };
        result.map_err(|e| e.to_string())
    }

    /// Switches the loop or punch range on or off.
    fn toggle_region(&mut self, kind: RangeKind) -> Result<(), String> {
        let project = self.studio.session().project();
        let (region, what) = match kind {
            RangeKind::Loop => (project.loop_region, "loop range: Shift+drag"),
            RangeKind::Punch => (project.punch, "punch range: Ctrl+drag"),
        };
        if region.range.is_none() {
            return Err(format!("set a {what} on the ruler first"));
        }
        let enabled = Some(!region.enabled);
        self.execute(match kind {
            RangeKind::Loop => SetLoop {
                enabled,
                ..Default::default()
            }
            .into(),
            RangeKind::Punch => SetPunch {
                enabled,
                ..Default::default()
            }
            .into(),
        })
    }

    /// Snaps to the nearest beat when snapping is on. (Range drags use
    /// Shift and Ctrl themselves, so unlike clip drags, Shift doesn't turn
    /// snapping off for them.)
    pub(crate) fn snap_to_beat(&self, t: f64) -> f64 {
        if !self.view.snap {
            return t;
        }
        snap(self.studio.session().project(), t)
    }

    fn start_gesture(
        &mut self,
        id: &str,
        origin: Point,
        modifiers: Modifiers,
    ) -> Result<Option<Gesture>, String> {
        let parts: Vec<&str> = id.split('/').collect();
        self.gesture_serial += 1;
        let merge_key = format!("{id} #{}", self.gesture_serial);
        if let [list @ ("plugin_picker" | "plugin_editor"), "scrollbar"] = parts[..] {
            return Ok(self.start_overlay_scroll(list, origin));
        }
        if let Some(gesture) = self.start_roll_gesture(&parts, origin, modifiers, merge_key.clone())
        {
            return gesture;
        }
        if let Some(gesture) = self.start_plugin_gesture(&parts, merge_key.clone()) {
            return gesture;
        }
        if let Some(gesture) = self.start_tempo_gesture(&parts, origin) {
            self.view.selected_clip = None;
            return gesture;
        }
        if let ["take", clip] = parts[..] {
            return self.start_comp(ClipId(parse_id(clip)?), origin, modifiers);
        }
        let project = self.studio.session().project();
        Ok(match parts[..] {
            ["clip", clip, "end"] if modifiers.ctrl => {
                Some(self.start_repeat(ClipId(parse_id(clip)?), origin)?)
            }
            ["clip", clip, edge @ ("start" | "end")] => {
                let clip = ClipId(parse_id(clip)?);
                let edge = if edge == "start" {
                    ClipEdge::Start
                } else {
                    ClipEdge::End
                };
                Some(self.start_trim(clip, edge, origin)?)
            }
            // Dragging a clip by its name moves it, unless the name is
            // being typed.
            ["clip", clip] | ["clip", clip, "name"] => {
                let clip = ClipId(parse_id(clip)?);
                if self.editing(&TextField::ClipName(clip)) {
                    return Ok(None);
                }
                let (track, c) = project
                    .clip(clip)
                    .ok_or_else(|| format!("clip {clip} does not exist"))?;
                let start = project.frames_to_seconds(c.start);
                self.view.selected_clip = Some(clip);
                self.view.selected_marker = None;
                Some(Gesture::Clip {
                    clip,
                    grab_s: self.time_at(origin.x) - start,
                    preview_start_s: start,
                    preview_track: track.id,
                    copy: modifiers.ctrl,
                })
            }
            ["track", track, "fader"] => {
                let track = parse_track(track)?;
                let fixed = project
                    .track(track)
                    .ok_or_else(|| format!("track {track} does not exist"))?
                    .gain_db;
                // From where the fader is now (its automation's value).
                let start_db =
                    self.value_now(track, &AutomationTarget::Gain, f64::from(fixed)) as f32;
                Some(Gesture::Fader {
                    track,
                    start_db,
                    merge_key,
                })
            }
            ["track", track, "pan"] => {
                let track = parse_track(track)?;
                let fixed = project
                    .track(track)
                    .ok_or_else(|| format!("track {track} does not exist"))?
                    .pan;
                let start_pan =
                    self.value_now(track, &AutomationTarget::Pan, f64::from(fixed)) as f32;
                Some(Gesture::Pan {
                    track,
                    start_pan,
                    merge_key,
                })
            }
            ["timeline", "ruler"] if modifiers.shift || modifiers.ctrl => {
                let kind = if modifiers.shift {
                    RangeKind::Loop
                } else {
                    RangeKind::Punch
                };
                let anchor_s = self.snap_to_beat(self.time_at(origin.x).max(0.0));
                Some(Gesture::Range {
                    kind,
                    anchor_s,
                    current_s: anchor_s,
                })
            }
            ["timeline", "ruler"] => Some(Gesture::Scrub),
            _ => None,
        })
    }

    fn end_gesture(&mut self) {
        self.view.gesture = None;
        self.release_automation();
    }

    fn on_scroll(&mut self, id: Option<&str>, pos: Point, delta: Point, modifiers: Modifiers) {
        if self.on_overlay_scroll(id, delta) {
            return;
        }
        if matches!(
            id,
            Some("piano_roll/grid" | "piano_roll/keys" | "piano_roll/ruler")
        ) {
            self.on_roll_scroll(pos, delta, modifiers);
            return;
        }
        if !matches!(
            id,
            Some(
                "timeline/lanes"
                    | "timeline/ruler"
                    | "arrange/headers"
                    | "tempo/lane"
                    | "meter/lane"
            )
        ) {
            return;
        }
        if modifiers.ctrl {
            // Zoom around the pointer.
            let anchor = self.time_at(pos.x);
            let pps =
                (self.view.px_per_s * (-delta.y * ZOOM_PER_PX).exp()).clamp(MIN_ZOOM, MAX_ZOOM);
            self.view.px_per_s = pps;
            self.view.scroll_s =
                (anchor - f64::from(pos.x - self.lanes_rect().x) / f64::from(pps)).max(0.0);
            return;
        }
        let (dx, dy) = if modifiers.shift {
            (delta.y, 0.0)
        } else {
            (delta.x, delta.y)
        };
        self.view.scroll_s = (self.view.scroll_s + f64::from(dx / self.view.px_per_s)).max(0.0);
        let (_, content) = self.track_rows();
        let max = (content - self.lanes_rect().h).max(0.0);
        self.view.scroll_y = (self.view.scroll_y + dy).clamp(0.0, max);
    }

    fn on_key(&mut self, key: &str, m: Modifiers) -> Result<(), String> {
        if let Some(result) = self.on_recovery_key(key) {
            return result;
        }
        if let Some(result) = self.on_confirm_key(key) {
            return result;
        }
        if let Some(result) = self.on_text_key(key, m) {
            return result;
        }
        if let Some(result) = self.on_export_key(key) {
            return result;
        }
        if let Some(result) = self.on_prefs_key(key) {
            return result;
        }
        if let Some(result) = self.on_plugin_key(key, m) {
            return result;
        }
        if self.view.page == Page::PianoRoll
            && let Some(result) = self.on_roll_key(key, m)
        {
            return result;
        }
        if let Some(result) = self.on_tempo_key(key, m) {
            return result;
        }
        match (key, m.ctrl, m.shift) {
            ("return", false, _) if self.view.page == Page::Arrange => match self.editable_clip() {
                Some(clip) => self.open_piano_roll(clip),
                None => Ok(()),
            },
            ("f2", false, _) if self.view.page == Page::Arrange => match self.view.selected_clip {
                Some(clip) => self.start_text_edit(TextField::ClipName(clip)),
                None => Ok(()),
            },
            ("space", false, _) => if self.studio.transport().playing {
                self.studio.stop()
            } else {
                self.studio.play()
            }
            .map_err(|e| e.to_string()),
            ("home", ..) => self.studio.locate(0.0).map_err(|e| e.to_string()),
            ("r", false, false) => self.toggle_record(),
            ("s", false, false) if self.view.page == Page::Arrange => self.split_selected(),
            ("l", false, false) => self.toggle_region(RangeKind::Loop),
            ("z", true, false) => self.studio.undo().map(|_| ()).map_err(|e| e.to_string()),
            ("z", true, true) | ("y", true, _) => {
                self.studio.redo().map(|_| ()).map_err(|e| e.to_string())
            }
            ("s", true, false) => {
                if self.studio.session().path().is_some() {
                    self.studio
                        .save(None)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                } else {
                    self.platform_requests
                        .push(PlatformRequest::ChooseSaveLocation { then: None });
                    Ok(())
                }
            }
            ("s", true, true) => {
                self.platform_requests
                    .push(PlatformRequest::ChooseSaveLocation { then: None });
                Ok(())
            }
            ("o", true, _) => {
                self.request_action(PendingAction::OpenProject);
                Ok(())
            }
            ("e", true, _) => {
                self.open_export_dialog();
                Ok(())
            }
            (",", true, _) => {
                self.open_preferences();
                Ok(())
            }
            ("i", true, _) => {
                let position_s = self.studio.transport().position_s;
                self.platform_requests
                    .push(PlatformRequest::ChooseAudioFiles {
                        track: None,
                        position_s,
                    });
                Ok(())
            }
            ("c", true, false) if self.view.page == Page::Arrange => {
                let clip = self.view.selected_clip.ok_or("select a clip to copy")?;
                self.view.clipboard = Some(clip);
                Ok(())
            }
            ("v", true, false) if self.view.page == Page::Arrange => self.paste_clip(),
            ("delete" | "backspace", false, _) => match self.view.selected_clip.take() {
                Some(clip) => self.execute(RemoveClip { clip }.into()),
                None => Ok(()),
            },
            ("tab", false, _) => {
                self.view.page = match self.view.page {
                    Page::Arrange => Page::Mixer,
                    Page::Mixer | Page::PianoRoll => Page::Arrange,
                };
                Ok(())
            }
            ("+" | "=", ..) => {
                self.zoom_by(1.5);
                Ok(())
            }
            ("-", ..) => {
                self.zoom_by(1.0 / 1.5);
                Ok(())
            }
            ("escape", ..) => {
                self.view.selected_clip = None;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Queues files dropped at `pos` for import: the first onto the audio
    /// track under the pointer (or a new track), the rest onto new tracks.
    pub fn drop_files(&mut self, paths: Vec<std::path::PathBuf>, pos: Point) {
        let lanes = self.lanes_rect();
        let in_timeline = self.view.page == Page::Arrange && lanes.contains(pos);
        let (track, position_s) = if in_timeline {
            let mut t = self.time_at(pos.x).max(0.0);
            if self.view.snap {
                t = snap(self.studio.session().project(), t);
            }
            (self.track_at(pos.y).filter(|t| self.is_audio_track(*t)), t)
        } else {
            (None, self.studio.transport().position_s)
        };
        for (i, path) in paths.into_iter().enumerate() {
            self.imports.push(ImportRequest {
                path,
                track: if i == 0 { track } else { None },
                position_s,
            });
        }
    }

    fn zoom_by(&mut self, factor: f32) {
        let lanes = self.lanes_rect();
        let center = lanes.x + lanes.w / 2.0;
        let anchor = self.time_at(center);
        self.view.px_per_s = (self.view.px_per_s * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.view.scroll_s =
            (anchor - f64::from(lanes.w / 2.0) / f64::from(self.view.px_per_s)).max(0.0);
    }

    pub(crate) fn execute(&mut self, command: Command) -> Result<(), String> {
        self.studio
            .execute(command)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub(crate) fn execute_merging(&mut self, command: Command, key: &str) -> Result<(), String> {
        self.studio
            .execute_merging(command, key)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn locate_at(&mut self, x: f32) -> Result<(), String> {
        let t = self.time_at(x).max(0.0);
        self.studio.locate(t).map_err(|e| e.to_string())
    }

    /// The lanes area from the last layout.
    pub(crate) fn lanes_rect(&self) -> Rect {
        self.ui
            .find("timeline/lanes")
            .map(|n| n.rect())
            .unwrap_or_default()
    }

    /// Timeline time at window x.
    pub(crate) fn time_at(&self, x: f32) -> f64 {
        self.view.scroll_s + f64::from(x - self.lanes_rect().x) / f64::from(self.view.px_per_s)
    }

    /// The top of each track's row (before scrolling), and their height.
    fn track_rows(&self) -> (Vec<f32>, f32) {
        let shown = &self.view.automation_shown;
        track_tops(self.studio.session().project().tracks.iter().map(|t| {
            let lanes = t
                .clips
                .iter()
                .map(|c| c.lane as usize + 1)
                .max()
                .unwrap_or(1);
            let autos = if shown.contains(&t.id) {
                t.automation.len()
            } else {
                0
            };
            (takes_shown(&self.view, t.id, lanes), autos)
        }))
    }

    /// The track whose row (lane or automation lanes) is at window y.
    pub(crate) fn track_at(&self, y: f32) -> Option<TrackId> {
        let lanes = self.lanes_rect();
        if y < lanes.y {
            return None;
        }
        let at = y - lanes.y + self.view.scroll_y;
        let (tops, total) = self.track_rows();
        if at >= total {
            return None;
        }
        let index = tops.partition_point(|&top| top <= at).checked_sub(1)?;
        self.studio
            .session()
            .project()
            .tracks
            .get(index)
            .map(|t| t.id)
    }

    fn is_audio_track(&self, track: TrackId) -> bool {
        self.studio
            .session()
            .project()
            .track(track)
            .is_some_and(|t| t.kind == TrackKind::Audio)
    }

    pub(crate) fn clip_kind(&self, clip: ClipId) -> Option<TrackKind> {
        self.studio
            .session()
            .project()
            .clip(clip)
            .map(|(_, c)| c.kind())
    }

    /// The clip the piano roll would open: the one being edited, else the
    /// selected clip if it's a MIDI clip.
    fn editable_clip(&self) -> Option<ClipId> {
        self.view
            .editing_clip
            .filter(|c| self.clip_kind(*c) == Some(TrackKind::Midi))
            .or(self
                .view
                .selected_clip
                .filter(|c| self.clip_kind(*c) == Some(TrackKind::Midi)))
    }
}

/// `t` (seconds) moved to the nearest beat (the time signature's
/// denominator note, counted from the bar line) under the tempo map.
pub(crate) fn snap(project: &Project, t: f64) -> f64 {
    let map = project.tempo_map();
    map.seconds_at(snap_beat(project, map.beat_at_seconds(t)))
}

/// `beat` moved to the nearest beat of the time signature (its
/// denominator note, counted from the bar line), at least 0.
pub(crate) fn snap_beat(project: &Project, beat: f64) -> f64 {
    let meter = project.meter_map();
    let bar = meter.bar_start(meter.bar_at(beat));
    let unit = meter.meter_at(beat).beat_unit();
    (bar + ((beat - bar) / unit).round() * unit).max(0.0)
}

pub(crate) fn parse_id(s: &str) -> Result<u64, String> {
    s.parse().map_err(|_| format!("bad id {s:?}"))
}

pub(crate) fn parse_track(s: &str) -> Result<TrackId, String> {
    parse_id(s).map(TrackId)
}
