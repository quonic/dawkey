// SPDX-License-Identifier: AGPL-3.0-or-later
//! The control-thread half of a VST3 plugin: its parameters' text, its
//! state, what its controller does (edits, restarts, its editor), and
//! keeping the controller in step with the values the processor plays.

use std::any::Any;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper};

use super::host::{Edit, PlugFrame};
use super::plugin::Plugin;
use super::processor::{CONTROLS, Links};
use super::{ok, read_wstring, write_wstring};
use crate::{GuiKind, ParamChange, ParamInfo, PluginError};

/// What a saved state starts with.
const STATE_MAGIC: &[u8; 4] = b"DKV3";

/// The component's and the controller's state in one blob: a magic, then
/// each with its length before it. `None` if both are empty.
pub(crate) fn state_blob(component: &[u8], controller: &[u8]) -> Option<Vec<u8>> {
    if component.is_empty() && controller.is_empty() {
        return None;
    }
    let mut blob = Vec::with_capacity(12 + component.len() + controller.len());
    blob.extend_from_slice(STATE_MAGIC);
    for part in [component, controller] {
        blob.extend_from_slice(&(part.len() as u32).to_le_bytes());
        blob.extend_from_slice(part);
    }
    Some(blob)
}

/// The two states in a blob [`state_blob`] made.
pub(crate) fn split_state(blob: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = blob.strip_prefix(STATE_MAGIC)?;
    let part = |bytes: &[u8]| -> Option<(usize, usize)> {
        let len = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
        (bytes.len() >= 4 + len).then_some((4, 4 + len))
    };
    let (a, b) = part(rest)?;
    let component = &rest[a..b];
    let rest = &rest[b..];
    let (c, d) = part(rest)?;
    Some((component, &rest[c..d]))
}

/// An open editor.
struct Gui {
    view: ComPtr<IPlugView>,
    frame: ComWrapper<PlugFrame>,
}

pub(crate) struct Vst3Instance {
    pub(crate) plugin: Arc<Plugin>,
    /// The catalog's parameters (by engine index): ID, and the catalog
    /// value of 1 normalized.
    params: Box<[(ParamID, f64)]>,
    /// Which of them are stepped (whole numbers in the catalog).
    stepped: Box<[bool]>,
    links: Links,
    /// Each parameter's value as its controller last said it is (catalog
    /// units).
    known: Vec<f64>,
    /// Each parameter's value the processor last applied, as bits (NaN:
    /// none yet): only a new one goes to the controller, so an edit the
    /// controller made isn't undone before the project plays it.
    last_applied: Vec<u64>,
    changes: Vec<ParamChange>,
    dirty: bool,
    gui: Option<Gui>,
    /// Its controller made no editor Dawkey can show.
    no_gui: bool,
}

impl Vst3Instance {
    pub(crate) fn new(
        plugin: Arc<Plugin>,
        params: Box<[(ParamID, f64)]>,
        stepped: Box<[bool]>,
        links: Links,
    ) -> Self {
        let mut instance = Self {
            known: vec![f64::NAN; params.len()],
            last_applied: vec![f64::NAN.to_bits(); params.len()],
            plugin,
            params,
            stepped,
            links,
            changes: Vec::new(),
            dirty: false,
            gui: None,
            no_gui: false,
        };
        instance.rebuild_midi_map();
        instance.read_known();
        instance
    }

    fn controller(&self) -> Option<&ComPtr<IEditController>> {
        self.plugin.controller.as_ref()
    }

    /// What the controller has for every parameter, as what's known.
    fn read_known(&mut self) {
        let Some(controller) = self.plugin.controller.clone() else {
            return;
        };
        for index in 0..self.params.len() {
            self.known[index] = self.read(&controller, index);
        }
    }

    /// Parameter `index` as the controller has it, in catalog units.
    fn read(&self, controller: &ComPtr<IEditController>, index: usize) -> f64 {
        let (id, max) = self.params[index];
        // SAFETY: a plain query.
        let value = catalog_value(unsafe { controller.getParamNormalized(id) }, max);
        if self.stepped[index] {
            value.round()
        } else {
            value
        }
    }

    /// Which parameter each MIDI controller changes, from the plugin.
    fn rebuild_midi_map(&mut self) {
        let map = &self.links.midi_map;
        let mapping = self.controller().and_then(|c| c.cast::<IMidiMapping>());
        for channel in 0..16 {
            for control in 0..CONTROLS {
                let target = mapping.as_ref().and_then(|mapping| {
                    let mut id: ParamID = 0;
                    // SAFETY: a plain query.
                    let found = ok(unsafe {
                        mapping.getMidiControllerAssignment(
                            0,
                            channel as i16,
                            control as i16,
                            &mut id,
                        )
                    });
                    // Only program changes need the step count (a walk
                    // over every parameter: JUCE plugins have thousands).
                    let program = control == ControllerNumbers_::kCtrlProgramChange as usize;
                    found.then(|| (id, if program { self.step_count(id) } else { 0 }))
                });
                map.set(channel, control, target);
            }
        }
    }

    fn step_count(&self, id: ParamID) -> u32 {
        let Some(controller) = self.controller() else {
            return 0;
        };
        // SAFETY: plain queries.
        let count = unsafe { controller.getParameterCount() };
        (0..count)
            .find_map(|i| {
                let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
                (ok(unsafe { controller.getParameterInfo(i, &mut info) }) && info.id == id)
                    .then_some(info.stepCount.max(0) as u32)
            })
            .unwrap_or(0)
    }

    fn index_of(&self, id: ParamID) -> Option<usize> {
        self.params.iter().position(|&(p, _)| p == id)
    }

    /// Catches up with the processor and the controller: values the
    /// processor played go to the controller, and what the controller did
    /// comes back (edits, restarts).
    fn sync(&mut self) {
        let Some(controller) = self.plugin.controller.clone() else {
            return;
        };
        for i in 0..self.params.len() {
            let bits = self.links.applied[i].load(Ordering::Relaxed);
            if bits == self.last_applied[i] {
                continue;
            }
            self.last_applied[i] = bits;
            let applied = f64::from_bits(bits);
            if !applied.is_nan() {
                let (id, max) = self.params[i];
                // SAFETY: plain calls on the control thread.
                unsafe { controller.setParamNormalized(id, applied / max) };
                // As it keeps it (float, for many plugins).
                self.known[i] = self.read(&controller, i);
            }
        }
        while let Ok((id, value)) = self.links.sent.pop() {
            unsafe { controller.setParamNormalized(id, value) };
        }
        for edit in self.plugin.handler.take_edits() {
            let id = match edit {
                Edit::Begin(id) | Edit::Perform(id, _) | Edit::End(id) => id,
            };
            let Some(index) = self.index_of(id) else {
                // A parameter the project doesn't keep: straight to the
                // processor.
                if let Edit::Perform(id, value) = edit {
                    let _ = self.links.edits.push((id, value));
                }
                continue;
            };
            let max = self.params[index].1;
            self.changes.push(match edit {
                Edit::Begin(_) => ParamChange::Begin { index },
                Edit::Perform(_, value) => {
                    let value = catalog_value(value, max);
                    let value = if self.stepped[index] {
                        value.round()
                    } else {
                        value
                    };
                    self.known[index] = self.read(&controller, index);
                    ParamChange::Value { index, value }
                }
                Edit::End(_) => ParamChange::End { index },
            });
        }
        let restart = self.plugin.handler.take_restart();
        if restart & RestartFlags_::kParamValuesChanged != 0 {
            // A preset loaded, say: the values that changed reach the
            // project like edits.
            for index in 0..self.params.len() {
                let value = self.read(&controller, index);
                // Plugins keep floats: what's new is more than their
                // rounding.
                let tolerance = 1e-6 * self.params[index].1;
                if (value - self.known[index]).abs() > tolerance {
                    self.known[index] = value;
                    self.changes.extend([
                        ParamChange::Begin { index },
                        ParamChange::Value { index, value },
                        ParamChange::End { index },
                    ]);
                }
            }
            self.dirty = true;
        }
        if restart & RestartFlags_::kMidiCCAssignmentChanged != 0 {
            self.rebuild_midi_map();
        }
        if restart & RestartFlags_::kLatencyChanged != 0 {
            tracing::debug!("a VST3 plugin's latency changed; it counts when it's next loaded");
        }
        if restart & (RestartFlags_::kReloadComponent | RestartFlags_::kIoChanged) != 0 {
            tracing::warn!(
                "a VST3 plugin asked to be reloaded (its buses changed), which Dawkey doesn't do yet"
            );
        }
        if self.plugin.handler.dirty.swap(false, Ordering::AcqRel) {
            self.dirty = true;
        }
    }

    /// Parameter `index` as its controller has it (catalog units).
    #[cfg(test)]
    pub(crate) fn controller_value(&self, index: usize) -> Option<f64> {
        Some(self.read(self.controller()?, index))
    }

    pub(crate) fn param_text(&mut self, param: &ParamInfo, value: f64) -> Option<String> {
        let id: ParamID = param.id.parse().ok()?;
        let controller = self.controller()?;
        let mut text: String128 = [0; 128];
        // SAFETY: room for a String128.
        if !ok(unsafe { controller.getParamStringByValue(id, value / param.max, &mut text) }) {
            return None;
        }
        let text = read_wstring(&text).trim().to_owned();
        (!text.is_empty()).then_some(text)
    }

    pub(crate) fn param_value(&mut self, param: &ParamInfo, text: &str) -> Option<f64> {
        let id: ParamID = param.id.parse().ok()?;
        let controller = self.controller()?;
        let mut wide: String128 = [0; 128];
        write_wstring(text.trim(), &mut wide);
        let mut value = 0.0;
        // SAFETY: a NUL-terminated UTF-16 string, and somewhere for the
        // value.
        if !ok(unsafe { controller.getParamValueByString(id, wide.as_mut_ptr(), &mut value) }) {
            return None;
        }
        let value = catalog_value(value, param.max);
        Some(if param.stepped { value.round() } else { value })
    }

    pub(crate) fn save_state(&mut self) -> Result<Option<Vec<u8>>, PluginError> {
        self.sync();
        self.dirty = false;
        let (component, controller) = self.plugin.save_state();
        Ok(state_blob(&component, &controller))
    }

    pub(crate) fn state_dirty(&self) -> bool {
        self.dirty || self.plugin.handler.dirty.load(Ordering::Acquire)
    }

    pub(crate) fn idle(&mut self) {
        self.plugin.run_loop.run_now();
        self.sync();
    }

    pub(crate) fn take_param_changes(&mut self) -> Vec<ParamChange> {
        self.sync();
        std::mem::take(&mut self.changes)
    }

    pub(crate) fn gui_kind(&mut self) -> Option<GuiKind> {
        (self.controller().is_some() && !self.no_gui).then_some(GuiKind::Vst3)
    }

    /// Opens its editor in X11 window `parent`; returns the size it wants.
    pub(crate) fn gui_open(&mut self, parent: u32) -> Result<Option<(u32, u32)>, PluginError> {
        let controller = self
            .controller()
            .ok_or_else(|| PluginError::new("it has no GUI"))?
            .clone();
        // SAFETY: a NUL-terminated name; the view comes back owned.
        let view = unsafe { ComPtr::from_raw(controller.createView(c"editor".as_ptr())) };
        let Some(view) = view else {
            self.no_gui = true;
            return Err(PluginError::new("it has no GUI"));
        };
        if !ok(unsafe { view.isPlatformTypeSupported(kPlatformTypeX11EmbedWindowID) }) {
            self.no_gui = true;
            return Err(PluginError::new("it has no GUI Dawkey can show (not X11)"));
        }
        let frame = ComWrapper::new(PlugFrame::new(Arc::clone(&self.plugin.run_loop)));
        let frame_ptr = frame.to_com_ptr::<IPlugFrame>().expect("it's a frame");
        // SAFETY: the frame outlives the view's use of it (taken away
        // before it goes); `parent` is a live X11 window the host process
        // made for this editor, destroyed after `gui_close`.
        unsafe { view.setFrame(frame_ptr.as_ptr()) };
        let attached = unsafe {
            view.attached(
                parent as usize as *mut std::ffi::c_void,
                kPlatformTypeX11EmbedWindowID,
            )
        };
        if !ok(attached) {
            unsafe { view.setFrame(std::ptr::null_mut()) };
            return Err(PluginError::new("embedding its GUI failed"));
        }
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let size = ok(unsafe { view.getSize(&mut rect) }).then(|| {
            (
                (rect.right - rect.left).max(1) as u32,
                (rect.bottom - rect.top).max(1) as u32,
            )
        });
        self.gui = Some(Gui { view, frame });
        Ok(size)
    }

    pub(crate) fn gui_close(&mut self) {
        if let Some(gui) = self.gui.take() {
            // SAFETY: the view is attached; it's detached, then let go.
            unsafe {
                gui.view.removed();
                gui.view.setFrame(std::ptr::null_mut());
            }
        }
    }

    pub(crate) fn gui_can_resize(&mut self) -> bool {
        self.gui
            .as_ref()
            .is_some_and(|gui| ok(unsafe { gui.view.canResize() }))
    }

    /// The window was resized: the editor takes the nearest size it can.
    pub(crate) fn gui_resized(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let gui = self.gui.as_ref()?;
        if !ok(unsafe { gui.view.canResize() }) {
            return None;
        }
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        // SAFETY: plain calls with a rectangle.
        unsafe {
            gui.view.checkSizeConstraint(&mut rect);
            gui.view.onSize(&mut rect);
        }
        Some((
            (rect.right - rect.left).max(1) as u32,
            (rect.bottom - rect.top).max(1) as u32,
        ))
    }

    /// The size the editor asked for since the last call (VST3 editors
    /// don't close themselves).
    pub(crate) fn gui_requests(&mut self) -> (Option<(u32, u32)>, bool) {
        let resize = self.gui.as_ref().and_then(|gui| {
            gui.frame
                .resize
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
        });
        (resize, false)
    }

    pub(crate) fn poll_fds(&self) -> Vec<(RawFd, i16)> {
        self.plugin.run_loop.poll_fds()
    }

    pub(crate) fn on_fd(&mut self, fd: RawFd, _revents: i16) {
        self.plugin.run_loop.on_fd(fd);
    }

    pub(crate) fn run_timers(&mut self, now: Instant) -> Option<Instant> {
        self.plugin.run_loop.run_timers(now)
    }

    /// Shuts it down: the plugin goes with the last of its halves.
    pub(crate) fn unload(mut self, processor: Box<dyn Any + Send>) {
        self.gui_close();
        drop(processor);
    }
}

impl Drop for Vst3Instance {
    fn drop(&mut self) {
        self.gui_close();
    }
}

/// A normalized value in catalog units (the step count is the top).
fn catalog_value(normalized: f64, max: f64) -> f64 {
    normalized.clamp(0.0, 1.0) * max
}
