// SPDX-License-Identifier: AGPL-3.0-or-later
//! Plugin hosting: finding CLAP, LV2 and VST3 plugins, describing them,
//! and running them.
//!
//! - **Scanning** reads what each plugin is (name, kind, ports, parameters)
//!   into a [`Catalog`]. Reading a CLAP or VST3 plugin runs its code, which
//!   could crash, so [`scan`] reads each `.clap` file and `.vst3` bundle in
//!   a subprocess (the `dawkey scan-plugin` command). LV2 plugins are
//!   described by Turtle files, read in-process without running plugin
//!   code.
//! - **Hosting**: [`Host::load`] instantiates and activates a plugin from
//!   its [`PluginInfo`], returning an [`Instance`] that stays on the
//!   control thread and a [`PluginProcessor`] for the engine's real-time
//!   thread. Hand the processor back to [`Instance::unload`] when the engine
//!   is done with it.
//!
//! The main audio ports carry the track's two channels; mono ports get
//! (and give) the average. A plugin's first other input is its sidechain;
//! its other outputs can each go to a bus (stereo, or mono to both
//! channels). Inputs beyond those get silence.

mod bridge;
mod clap;
mod expression;
mod gtk;
mod lv2;
mod lv2_ring;
mod lv2_ui;
mod lv2_worker;
mod ports;
mod qt;
mod run_loop;
mod scan;
mod vst3;

use std::any::Any;
use std::path::PathBuf;

use dawkey_engine::PluginProcessor;
use serde::{Deserialize, Serialize};

pub use bridge::{BridgeProgram, Sandbox, run_plugin_host};
pub use lv2::{StateProperty, StateValue, lv2_state_properties, lv2_state_set};
pub use scan::{
    Catalog, ScanFailure, SearchPath, clap_files, run_plugin_scanner, scan, scan_clap_file,
    scan_lv2, scan_plugin_file, scan_vst3_bundle, vst3_binary, vst3_bundles,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginFormat {
    Clap,
    Lv2,
    Vst3,
}

/// What a plugin is, as found by a scan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    /// `clap:<plugin id>`, `lv2:<plugin URI>` or `vst3:<class ID in hex>`;
    /// what projects store.
    pub id: String,
    pub format: PluginFormat,
    pub name: String,
    pub vendor: String,
    /// The `.clap` file, or the LV2 or VST3 bundle directory.
    pub path: PathBuf,
    /// Takes notes (an instrument, or a MIDI effect).
    pub instrument: bool,
    /// Sends MIDI (a MIDI effect: an arpeggiator, a chord maker, ...).
    pub midi_output: bool,
    /// Channels of the main audio input and output.
    pub audio_inputs: u32,
    pub audio_outputs: u32,
    /// Has a sidechain input (an audio input besides the main one).
    #[serde(default)]
    pub sidechain: bool,
    /// Audio outputs besides the main one (a drum instrument's separate
    /// outputs), in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<OutputInfo>,
    /// Parameters, in the order the engine indexes them.
    pub params: Vec<ParamInfo>,
    /// LV2: properties the plugin declares besides its parameters
    /// (`patch:writable`, `patch:readable`): a soundfont path, a string, a
    /// choice. Set with the plugin's saved state (see
    /// [`lv2_state_properties`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyInfo>,
}

/// A property an LV2 plugin declares (`patch:writable` or `patch:readable`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyInfo {
    /// The property's URI (its key in the plugin's saved state).
    pub uri: String,
    /// `rdfs:label`, or the URI's fragment.
    pub label: String,
    /// `rdfs:range`: the value's type (`atom:Path`, `atom:String`,
    /// `atom:Float`, `atom:Int`, `atom:Bool`, `atom:URI`, ...), if said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    /// The plugin takes changes to it (`patch:writable`); else it only
    /// reports it (`patch:readable`).
    pub writable: bool,
}

/// One of a plugin's other audio outputs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputInfo {
    pub name: String,
    pub channels: u32,
}

/// A plugin parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    /// The CLAP or VST3 parameter ID (in decimal), or the LV2 port symbol.
    /// (VST3 values are the plugin's normalized ones: 0 to 1, or 0 to the
    /// step count for stepped parameters.)
    pub id: String,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    /// Only whole numbers (switches, choices).
    #[serde(default)]
    pub stepped: bool,
    /// The unit values are in, e.g. "dB" (LV2 plugins say; CLAP plugins
    /// format their values themselves).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Names of particular values (an LV2 port's scale points), e.g. the
    /// choices of a switch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<ValueLabel>,
}

/// A named parameter value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValueLabel {
    pub value: f64,
    pub label: String,
}

impl ParamInfo {
    /// A value as text from what the catalog knows: its label if it has
    /// one, otherwise the number with the unit.
    pub fn text(&self, value: f64) -> String {
        let close = |v: f64| (v - value).abs() <= 1e-6 * (self.max - self.min).abs().max(1.0);
        if let Some(label) = self.labels.iter().find(|l| close(l.value)) {
            return label.label.clone();
        }
        let number = if self.stepped || (value.fract() == 0.0 && value.abs() < 1e6) {
            format!("{value:.0}")
        } else if value.abs() >= 100.0 {
            format!("{value:.1}")
        } else {
            format!("{value:.2}")
        };
        match &self.unit {
            Some(unit) if unit == "%" => format!("{number}%"),
            Some(unit) => format!("{number} {unit}"),
            None => number,
        }
    }

    /// A value typed as text, read from what the catalog knows: a label
    /// (in any case), or a number, with or without the unit. Stepped
    /// parameters round to a whole number. `None` if it's neither; the
    /// value may be out of range.
    pub fn parse(&self, text: &str) -> Option<f64> {
        let text = text.trim();
        if let Some(label) = self
            .labels
            .iter()
            .find(|l| l.label.trim().eq_ignore_ascii_case(text))
        {
            return Some(label.value);
        }
        let mut number = text;
        if let Some(unit) = self.unit.as_deref().filter(|u| !u.is_empty()) {
            let lower = number.to_ascii_lowercase();
            if lower.ends_with(&unit.to_ascii_lowercase()) {
                number = number[..number.len() - unit.len()].trim_end();
            }
        }
        let value: f64 = number.parse().ok().filter(|v: &f64| v.is_finite())?;
        Some(if self.stepped { value.round() } else { value })
    }
}

impl PluginInfo {
    /// The index of parameter `id`.
    pub fn param_index(&self, id: &str) -> Option<usize> {
        self.params.iter().position(|p| p.id == id)
    }
}

/// A parameter the plugin changed itself: from its own GUI, or its own
/// logic. The host records it (as it would a change from its own controls),
/// so the project keeps the value. Knob drags come between a `Begin` and an
/// `End` (one undo step).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ParamChange {
    Begin { index: usize },
    Value { index: usize, value: f64 },
    End { index: usize },
}

/// Where files that plugins keep with their state go. LV2 plugins that
/// refer to files in their state (`state:mapPath`) keep paths under `base`
/// relative to it, so a project can move; ones that save files of their own
/// (`state:makePath`) make them in `files`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatePaths {
    /// The project's folder, once it has one.
    pub base: Option<PathBuf>,
    /// The slot's own folder (made when a plugin first puts a file in it).
    pub files: Option<PathBuf>,
    /// Where `files` is kept in the project, relative to `base`, when it's
    /// elsewhere while Dawkey runs (and copied there when saving): paths in
    /// it are saved as paths there.
    #[serde(default)]
    pub kept: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PluginError(pub String);

impl PluginError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Loads plugins for one sample rate. Lives on the control thread.
pub struct Host {
    sample_rate: u32,
    search: SearchPath,
    lv2: lv2::Lv2Host,
    /// Runs plugins in processes of their own, if set, and the processes
    /// running.
    bridge: Option<BridgeProgram>,
    processes: bridge::Processes,
    /// The engine's block size, the delay bridged plugins start with.
    bridge_block: u32,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("sample_rate", &self.sample_rate)
            .finish_non_exhaustive()
    }
}

/// A loaded plugin: the control-thread half, the real-time half, and what
/// the engine needs to know about it.
pub struct Loaded {
    pub instance: Instance,
    pub processor: Box<dyn PluginProcessor>,
    /// Latency the plugin adds, in frames.
    pub latency: u32,
}

impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loaded")
            .field("instance", &self.instance)
            .field("latency", &self.latency)
            .finish_non_exhaustive()
    }
}

/// How a plugin's GUI is shown, in the plugin host process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuiKind {
    Clap(clap::GuiMode),
    /// An LV2 `ui:X11UI`, embedded.
    Lv2X11,
    /// An LV2 Gtk or Qt UI (`ui:GtkUI`, `ui:Gtk3UI`, `ui:Qt5UI`,
    /// `ui:Qt6UI`), in a window of its own.
    Lv2Own,
    /// A VST3 editor, embedded (`X11EmbedWindowID`).
    Vst3,
}

impl GuiKind {
    /// Whether it goes in a window the host makes.
    pub(crate) fn embedded(self) -> bool {
        matches!(
            self,
            Self::Lv2X11 | Self::Vst3 | Self::Clap(clap::GuiMode::X11Embedded)
        )
    }
}

/// The control-thread half of a loaded plugin.
pub struct Instance(InstanceKind);

enum InstanceKind {
    Clap(Box<clap::ClapInstance>),
    Lv2(Box<lv2::Lv2Instance>),
    Vst3(Box<vst3::Vst3Instance>),
    /// In a process of its own.
    Bridged(Box<bridge::BridgedInstance>),
}

impl std::fmt::Debug for Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.0 {
            InstanceKind::Clap(_) => "Instance(clap)",
            InstanceKind::Lv2(_) => "Instance(lv2)",
            InstanceKind::Vst3(_) => "Instance(vst3)",
            InstanceKind::Bridged(_) => "Instance(bridged)",
        })
    }
}

impl Host {
    /// A host for plugins found on `search` (LV2 plugins must be there; CLAP
    /// plugins load from the path in their [`PluginInfo`]).
    pub fn new(sample_rate: u32, search: SearchPath) -> Self {
        Self {
            sample_rate,
            lv2: lv2::Lv2Host::new(search.lv2.clone()),
            search,
            bridge: None,
            processes: bridge::Processes::default(),
            bridge_block: 512,
        }
    }

    /// Runs plugins loaded from now on in processes of their own, started
    /// with `program` (`None`: in this process).
    pub fn set_bridge(&mut self, program: Option<BridgeProgram>) {
        self.bridge = program;
    }

    pub fn bridged(&self) -> bool {
        self.bridge.is_some()
    }

    /// The engine's block size: bridged plugins loaded from now on start
    /// that late (see [`Instance::latency`]).
    pub fn set_bridge_block(&mut self, frames: u32) {
        self.bridge_block = frames;
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn search(&self) -> &SearchPath {
        &self.search
    }

    /// Re-reads the LV2 world (after a rescan finds new bundles).
    pub fn refresh(&mut self) {
        self.lv2 = lv2::Lv2Host::new(self.search.lv2.clone());
    }

    /// Instantiates and activates a plugin.
    pub fn load(&mut self, info: &PluginInfo) -> Result<Loaded, PluginError> {
        self.load_with_state(info, None, &StatePaths::default())
    }

    /// Like [`load`](Self::load), restoring what
    /// [`Instance::save_state`] saved first, with the files it refers to
    /// under `paths`. A state the plugin doesn't take is logged, and the
    /// plugin starts as new.
    pub fn load_with_state(
        &mut self,
        info: &PluginInfo,
        state: Option<&[u8]>,
        paths: &StatePaths,
    ) -> Result<Loaded, PluginError> {
        self.load_for(info, state, paths, false)
    }

    /// Like [`load_with_state`](Self::load_with_state). A bridged plugin
    /// loaded for an `offline` render is waited for each block, so it adds
    /// no latency, and an LV2 plugin's worker does its work as it's asked
    /// for (not on a thread of its own), so the render doesn't depend on
    /// how long it takes.
    pub fn load_for(
        &mut self,
        info: &PluginInfo,
        state: Option<&[u8]>,
        paths: &StatePaths,
        offline: bool,
    ) -> Result<Loaded, PluginError> {
        if let Some(program) = &self.bridge {
            let (instance, processor, latency) = bridge::load(
                &mut self.processes,
                program,
                info,
                self.sample_rate,
                &self.search,
                state,
                paths,
                self.bridge_block,
                offline,
            )?;
            return Ok(Loaded {
                instance: Instance(InstanceKind::Bridged(Box::new(instance))),
                processor: Box::new(processor),
                latency,
            });
        }
        match info.format {
            PluginFormat::Clap => {
                let (instance, processor, latency) = clap::load(info, self.sample_rate, state)?;
                Ok(Loaded {
                    instance: Instance(InstanceKind::Clap(Box::new(instance))),
                    processor: Box::new(processor),
                    latency,
                })
            }
            PluginFormat::Lv2 => {
                let (instance, processor, latency) =
                    self.lv2
                        .load(info, self.sample_rate, state, paths, offline)?;
                Ok(Loaded {
                    instance: Instance(InstanceKind::Lv2(Box::new(instance))),
                    processor: Box::new(processor),
                    latency,
                })
            }
            PluginFormat::Vst3 => {
                let (instance, processor, latency) =
                    vst3::load(info, self.sample_rate, state, offline)?;
                Ok(Loaded {
                    instance: Instance(InstanceKind::Vst3(Box::new(instance))),
                    processor: Box::new(processor),
                    latency,
                })
            }
        }
    }
}

impl Instance {
    /// Parameter `index` of `info` (this instance's plugin) at `value`, as
    /// the plugin shows it: CLAP and VST3 plugins format it themselves,
    /// otherwise [`ParamInfo::text`].
    pub fn param_text(&mut self, info: &PluginInfo, index: usize, value: f64) -> Option<String> {
        let param = info.params.get(index)?;
        let own = match &mut self.0 {
            InstanceKind::Clap(clap) => clap.param_text(&param.id, value),
            InstanceKind::Lv2(_) => None,
            InstanceKind::Vst3(vst3) => vst3.param_text(param, value),
            InstanceKind::Bridged(bridged) => bridged.param_text(index, value),
        };
        Some(own.unwrap_or_else(|| param.text(value)))
    }

    /// Parameter `index` of `info` typed as `text`, read the way the plugin
    /// shows values: CLAP and VST3 plugins read it themselves, otherwise
    /// [`ParamInfo::parse`]. `None` if it isn't a value.
    pub fn param_value(&mut self, info: &PluginInfo, index: usize, text: &str) -> Option<f64> {
        let param = info.params.get(index)?;
        let own = match &mut self.0 {
            InstanceKind::Clap(clap) => clap.param_value(&param.id, text),
            InstanceKind::Lv2(_) => None,
            InstanceKind::Vst3(vst3) => vst3.param_value(param, text),
            InstanceKind::Bridged(bridged) => bridged.param_value(index, text),
        };
        own.filter(|v| v.is_finite()).or_else(|| param.parse(text))
    }

    /// What the plugin saves of itself beyond its parameters, or `None` if
    /// it saves nothing; files it saves or refers to go by `paths`. Can be
    /// called while the processor runs.
    pub fn save_state(&mut self, paths: &StatePaths) -> Result<Option<Vec<u8>>, PluginError> {
        match &mut self.0 {
            InstanceKind::Clap(clap) => clap.save_state(),
            InstanceKind::Lv2(lv2) => lv2.save_state(paths),
            InstanceKind::Vst3(vst3) => vst3.save_state(),
            InstanceKind::Bridged(bridged) => bridged.save_state(paths),
        }
    }

    /// Whether the plugin said its state changed since
    /// [`save_state`](Self::save_state).
    pub fn state_dirty(&self) -> bool {
        match &self.0 {
            InstanceKind::Clap(clap) => clap.state_dirty(),
            InstanceKind::Lv2(lv2) => lv2.state_dirty(),
            InstanceKind::Vst3(vst3) => vst3.state_dirty(),
            InstanceKind::Bridged(bridged) => bridged.state_dirty(),
        }
    }

    /// Control-thread housekeeping the plugin asked for. Call it regularly.
    pub fn idle(&mut self) {
        match &mut self.0 {
            InstanceKind::Clap(clap) => clap.idle(),
            InstanceKind::Lv2(_) => {}
            InstanceKind::Vst3(vst3) => vst3.idle(),
            InstanceKind::Bridged(bridged) => bridged.idle(),
        }
    }

    /// Parameters the plugin changed itself since the last call, in order.
    pub fn take_param_changes(&mut self) -> Vec<ParamChange> {
        match &mut self.0 {
            InstanceKind::Clap(clap) => clap.take_param_changes(),
            InstanceKind::Lv2(lv2) => lv2.take_param_changes(),
            InstanceKind::Vst3(vst3) => vst3.take_param_changes(),
            InstanceKind::Bridged(bridged) => bridged.take_param_changes(),
        }
    }

    /// Whether the plugin has a GUI Dawkey can show (only plugins in
    /// processes of their own: GUIs may need X11).
    pub fn has_gui(&self) -> bool {
        match &self.0 {
            InstanceKind::Bridged(bridged) => bridged.has_gui(),
            _ => false,
        }
    }

    /// Whether its GUI is showing.
    pub fn gui_visible(&self) -> bool {
        match &self.0 {
            InstanceKind::Bridged(bridged) => bridged.gui_visible(),
            _ => false,
        }
    }

    /// Shows the plugin's GUI in a window of its own, or hides it.
    pub fn show_gui(&mut self, show: bool) -> Result<(), PluginError> {
        match &mut self.0 {
            InstanceKind::Bridged(bridged) => bridged.show_gui(show),
            _ => Err(PluginError::new(
                "plugin GUIs show only for plugins that run in processes of their own",
            )),
        }
    }

    /// How the GUI of a plugin in this process can be shown (in the plugin
    /// host process).
    pub(crate) fn gui_kind(&mut self) -> Option<GuiKind> {
        match &mut self.0 {
            InstanceKind::Clap(clap) => clap.gui_mode().map(GuiKind::Clap),
            InstanceKind::Lv2(lv2) => lv2.gui_kind(),
            InstanceKind::Vst3(vst3) => vst3.gui_kind(),
            InstanceKind::Bridged(_) => None,
        }
    }

    /// Opens the GUI: embedded in X11 window `parent` (for kinds that
    /// embed), or floating titled `title`. Returns the size it wants.
    pub(crate) fn gui_open(
        &mut self,
        kind: GuiKind,
        parent: Option<u32>,
        title: &str,
    ) -> Result<Option<(u32, u32)>, PluginError> {
        match (&mut self.0, kind, parent) {
            (InstanceKind::Clap(clap), GuiKind::Clap(mode), parent) => {
                clap.gui_open(mode, parent, title)
            }
            (InstanceKind::Lv2(lv2), GuiKind::Lv2X11, Some(parent)) => {
                lv2.gui_open(Some(parent), title)
            }
            (InstanceKind::Lv2(lv2), GuiKind::Lv2Own, None) => lv2.gui_open(None, title),
            (InstanceKind::Vst3(vst3), GuiKind::Vst3, Some(parent)) => vst3.gui_open(parent),
            _ => Err(PluginError::new("it can't show that GUI")),
        }
    }

    pub(crate) fn gui_close(&mut self) {
        match &mut self.0 {
            InstanceKind::Clap(clap) => clap.gui_close(),
            InstanceKind::Lv2(lv2) => lv2.gui_close(),
            InstanceKind::Vst3(vst3) => vst3.gui_close(),
            InstanceKind::Bridged(_) => {}
        }
    }

    pub(crate) fn gui_can_resize(&mut self) -> bool {
        match &mut self.0 {
            InstanceKind::Clap(clap) => clap.gui_can_resize(),
            InstanceKind::Vst3(vst3) => vst3.gui_can_resize(),
            _ => true,
        }
    }

    /// The window was resized; returns the size the GUI took.
    pub(crate) fn gui_resized(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        match &mut self.0 {
            InstanceKind::Clap(clap) => clap.gui_resized(width, height),
            InstanceKind::Vst3(vst3) => vst3.gui_resized(width, height),
            _ => None,
        }
    }

    /// The open GUI's main-thread work, given the parameters' current
    /// values: the size it asked for, and whether it closed itself.
    pub(crate) fn gui_idle(&mut self, values: &[f64]) -> (Option<(u32, u32)>, bool) {
        match &mut self.0 {
            InstanceKind::Clap(clap) => {
                let requests = clap.gui_requests();
                (requests.resize, requests.closed)
            }
            InstanceKind::Lv2(lv2) => lv2.gui_idle(values),
            InstanceKind::Vst3(vst3) => vst3.gui_requests(),
            InstanceKind::Bridged(_) => (None, false),
        }
    }

    /// Descriptors the plugin wants watched, as `poll` events.
    pub(crate) fn poll_fds(&self) -> Vec<(std::os::fd::RawFd, i16)> {
        match &self.0 {
            InstanceKind::Clap(clap) => clap.poll_fds(),
            InstanceKind::Vst3(vst3) => vst3.poll_fds(),
            _ => Vec::new(),
        }
    }

    pub(crate) fn on_fd(&mut self, fd: std::os::fd::RawFd, revents: i16) {
        match &mut self.0 {
            InstanceKind::Clap(clap) => clap.on_fd(fd, revents),
            InstanceKind::Vst3(vst3) => vst3.on_fd(fd, revents),
            _ => {}
        }
    }

    /// Runs the plugin's timers that are due; returns when the next is.
    pub(crate) fn run_timers(&mut self, now: std::time::Instant) -> Option<std::time::Instant> {
        match &mut self.0 {
            InstanceKind::Clap(clap) => clap.run_timers(now),
            InstanceKind::Vst3(vst3) => vst3.run_timers(now),
            _ => None,
        }
    }

    /// Why the plugin stopped working, if it did (a bridged plugin whose
    /// process crashed). Its processor then passes the audio through.
    pub fn crashed(&self) -> Option<String> {
        match &self.0 {
            InstanceKind::Bridged(bridged) => bridged.crashed(),
            _ => None,
        }
    }

    /// The latency the plugin adds now, if it can change after loading (a
    /// bridged plugin's grows when the engine's blocks do).
    pub fn latency(&self) -> Option<u32> {
        match &self.0 {
            InstanceKind::Bridged(bridged) => Some(bridged.latency()),
            _ => None,
        }
    }

    /// It runs in a sandbox (see [`Sandbox`]) the kernel enforces, at
    /// least in part.
    pub fn sandboxed(&self) -> bool {
        match &self.0 {
            InstanceKind::Bridged(bridged) => bridged.sandboxed(),
            _ => false,
        }
    }

    /// The process a bridged plugin runs in.
    pub fn process_id(&self) -> Option<u32> {
        match &self.0 {
            InstanceKind::Bridged(bridged) => Some(bridged.pid()),
            _ => None,
        }
    }

    /// A VST3 plugin's controller's value of parameter `index`.
    #[cfg(test)]
    pub(crate) fn vst3_controller_value(&self, index: usize) -> Option<f64> {
        match &self.0 {
            InstanceKind::Vst3(vst3) => vst3.controller_value(index),
            _ => None,
        }
    }

    /// Shuts the plugin down, given back its processor (which the engine
    /// has stopped). Dropping an instance without this leaks nothing, but
    /// skips the plugin's deactivation.
    pub fn unload(self, processor: Box<dyn Any + Send>) {
        match self.0 {
            InstanceKind::Clap(clap) => clap.unload(processor),
            // The processor owns the LV2 instance and deactivates it when
            // dropped.
            InstanceKind::Lv2(_) => drop(processor),
            InstanceKind::Vst3(vst3) => vst3.unload(processor),
            InstanceKind::Bridged(bridged) => {
                drop(processor);
                bridged.unload();
            }
        }
    }
}

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[cfg(test)]
mod tests;
