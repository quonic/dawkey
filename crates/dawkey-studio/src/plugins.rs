// SPDX-License-Identifier: AGPL-3.0-or-later
//! The studio's plugins: the catalog of installed plugins, and the loaded
//! instances behind each plugin slot in the project.
//!
//! A slot's processor lives in the engine's graph; its control-thread half
//! (the [`Instance`]) stays here. When the engine lets go of a processor (its
//! slot was removed, or a graph never reached the audio thread), it comes
//! back through [`Plugins::take_back`] and is unloaded with its instance.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

use dawkey_core::{
    AutomationTarget, Command, CoreError, PluginId, PluginSlot, PluginState, Project, TrackId,
};
use dawkey_engine::{PluginProcessor, PluginProvider, ProvidedPlugin};
use dawkey_plugins::{
    BridgeProgram, Catalog, Host, Instance, ParamChange, PluginInfo, SearchPath, StatePaths,
};
use serde::{Deserialize, Serialize};

/// How long one `.clap` file or `.vst3` bundle may take to scan.
const SCAN_TIMEOUT: Duration = Duration::from_secs(20);

struct LiveSlot {
    key: u64,
    plugin: String,
    instance: Instance,
    latency: u32,
}

pub(crate) struct Plugins {
    search: SearchPath,
    /// The program that scans one CLAP file (`dawkey scan-plugin`); scans
    /// run in-process without one.
    scanner: Option<(PathBuf, Vec<String>)>,
    cache: Option<PathBuf>,
    catalog: Option<Catalog>,
    /// A scan running on a worker thread.
    scanning: Option<std::thread::JoinHandle<Catalog>>,
    /// Scan on a worker thread instead of waiting (the window does, so it
    /// keeps drawing); the slots load once the scan is done.
    background: bool,
    host: Host,
    /// Slots whose processor is in the engine.
    live: HashMap<PluginId, LiveSlot>,
    /// Instances whose slot is gone, waiting for their processor.
    retiring: HashMap<u64, Instance>,
    /// Instances behind offline renders, by render job.
    offline: HashMap<u64, Vec<(u64, Instance)>>,
    /// Why slots couldn't be loaded (not retried until a rescan).
    errors: BTreeMap<PluginId, (String, String)>,
    next_key: u64,
    /// The project's folder (once saved), and where slots' own files go
    /// while Dawkey runs (a folder for each, copied into the project's
    /// `plugins` folder when saving).
    state_base: Option<PathBuf>,
    state_files: PathBuf,
    /// Runs plugins in processes of their own, with this program.
    bridge: Option<BridgeProgram>,
    /// The engine's block size (what a bridged plugin's delay starts at).
    bridge_block: u32,
    /// The engine runs on a virtual clock, not in real time: bridged
    /// plugins are waited for each block (no added latency).
    bridge_waits: bool,
    /// Parameters plugins changed themselves, not yet taken.
    changes: Vec<(PluginId, ParamChange)>,
}

/// Bump when [`PluginInfo`] gains something scans find, or scans find
/// different plugins, so older caches are scanned again. (3: LV2 plugins
/// whose ports can't be read are left out; 4: LV2 plugins that require the
/// worker are in; 5: VST3 plugins.)
const CACHE_VERSION: u32 = 6;

/// The catalog as cached on disk, with what it was scanned from.
#[derive(Serialize, Deserialize)]
struct Cached {
    #[serde(default)]
    version: u32,
    fingerprint: u64,
    catalog: Catalog,
}

/// A slot that couldn't be loaded, as reported to clients.
#[derive(Debug, Clone, Serialize)]
pub struct PluginProblem {
    pub plugin: PluginId,
    pub id: String,
    pub error: String,
}

impl Plugins {
    pub(crate) fn new(sample_rate: u32) -> Self {
        let search = SearchPath::from_env();
        Self {
            host: Host::new(sample_rate, search.clone()),
            search,
            scanner: None,
            cache: default_cache(),
            catalog: None,
            scanning: None,
            background: false,
            live: HashMap::new(),
            retiring: HashMap::new(),
            offline: HashMap::new(),
            errors: BTreeMap::new(),
            next_key: 1,
            state_base: None,
            state_files: crate::cache::own_dir().join(dawkey_core::project_file::PLUGIN_DIR),
            bridge: None,
            bridge_block: 512,
            bridge_waits: false,
            changes: Vec::new(),
        }
    }

    /// Runs slots loaded from now on in processes of their own (`None`: in
    /// this one).
    pub(crate) fn set_bridge(&mut self, bridge: Option<BridgeProgram>) {
        self.bridge = bridge;
        self.host.set_bridge(self.bridge_program());
    }

    /// The bridge, its sandbox letting plugins write where their own files
    /// go.
    fn bridge_program(&self) -> Option<BridgeProgram> {
        let mut program = self.bridge.clone()?;
        if let Some(sandbox) = &mut program.sandbox {
            sandbox.writable.push(self.state_files.clone());
        }
        Some(program)
    }

    /// Whether the engine runs in real time (`false`: a virtual clock,
    /// whose bridged plugins are waited for each block).
    pub(crate) fn set_real_time(&mut self, real_time: bool) {
        self.bridge_waits = !real_time;
    }

    /// The engine's block size changed: bridged plugins loaded from now on
    /// start that late.
    pub(crate) fn set_bridge_block(&mut self, frames: u32) {
        self.bridge_block = frames;
        self.host.set_bridge_block(frames);
    }

    /// A new host for `search` and the sample rate, keeping the bridge.
    fn new_host(&mut self, sample_rate: u32, search: SearchPath) {
        self.host = Host::new(sample_rate, search);
        self.host.set_bridge(self.bridge_program());
        self.host.set_bridge_block(self.bridge_block);
    }

    /// Where plugins keep files with their state: `base` is the project's
    /// folder (`None` before it's saved), `files` the folder of each slot's
    /// folder while Dawkey runs.
    pub(crate) fn set_state_dirs(&mut self, base: Option<PathBuf>, files: PathBuf) {
        self.state_base = base;
        if files != self.state_files {
            self.state_files = files;
            // Plugin processes started from now on may write there.
            self.host.set_bridge(self.bridge_program());
        }
    }

    /// Where slot `slot` keeps files with its state: a folder of its own
    /// while Dawkey runs, kept in the project's `plugins/<slot>`.
    fn state_paths(&self, slot: PluginId) -> StatePaths {
        let folder = slot.0.to_string();
        StatePaths {
            base: self.state_base.clone(),
            files: Some(self.state_files.join(&folder)),
            kept: self
                .state_base
                .as_ref()
                .map(|_| Path::new(dawkey_core::project_file::PLUGIN_DIR).join(&folder)),
        }
    }

    pub(crate) fn set_search(&mut self, search: SearchPath, cache: Option<PathBuf>) {
        // A scan of the old path is of no use.
        if let Some(scan) = self.scanning.take() {
            let _ = scan.join();
        }
        self.new_host(self.host.sample_rate(), search.clone());
        self.search = search;
        self.cache = cache;
        self.catalog = None;
        self.errors.clear();
    }

    /// Like [`set_search`](Self::set_search), keeping the cache.
    pub(crate) fn change_search(&mut self, search: SearchPath) {
        let cache = self.cache.clone();
        self.set_search(search, cache);
    }

    pub(crate) fn set_scanner(&mut self, scanner: Option<(PathBuf, Vec<String>)>) {
        self.scanner = scanner;
    }

    pub(crate) fn set_background(&mut self, background: bool) {
        self.background = background;
    }

    /// The installed plugins: from the cache if nothing changed since it
    /// was written, otherwise scanned now (waiting for a scan already
    /// running).
    pub(crate) fn catalog(&mut self) -> &Catalog {
        if self.catalog.is_none() {
            match self.scanning.take() {
                Some(scan) => {
                    let catalog = scan.join().unwrap_or_default();
                    self.install(catalog);
                }
                None => {
                    let catalog = self.read_cache().unwrap_or_else(|| self.scan().run());
                    self.catalog = Some(catalog);
                }
            }
        }
        self.catalog.as_ref().expect("set above")
    }

    /// The catalog if there is one, without waiting: from the cache, or
    /// once a scan finishes. In the background mode a scan starts (on a
    /// worker thread) if there's no catalog yet; otherwise this scans now.
    pub(crate) fn catalog_now(&mut self) -> Option<&Catalog> {
        self.poll();
        if self.catalog.is_none() && self.scanning.is_none() {
            if let Some(cached) = self.read_cache() {
                self.catalog = Some(cached);
            } else if self.background {
                self.start_scan();
            } else {
                self.catalog();
            }
        }
        self.catalog.as_ref()
    }

    pub(crate) fn cached(&self) -> Option<&Catalog> {
        self.catalog.as_ref()
    }

    /// Whether a scan is running on a worker thread.
    pub(crate) fn scanning(&self) -> bool {
        self.scanning.is_some()
    }

    /// Takes the result of a background scan that has finished. Returns
    /// true if there was one (slots may load now).
    pub(crate) fn poll(&mut self) -> bool {
        if !self.scanning.as_ref().is_some_and(|s| s.is_finished()) {
            return false;
        }
        let catalog = self
            .scanning
            .take()
            .expect("checked above")
            .join()
            .unwrap_or_default();
        self.install(catalog);
        true
    }

    /// Scans again, whatever the cache says, and waits for it.
    pub(crate) fn rescan(&mut self) -> &Catalog {
        let catalog = match self.scanning.take() {
            // Started since the cache was read: as fresh as a new one.
            Some(scan) => scan.join().unwrap_or_default(),
            None => self.scan().run(),
        };
        self.install(catalog);
        self.catalog.as_ref().expect("set above")
    }

    /// Starts scanning again on a worker thread; the current catalog stays
    /// until it's done. Outside the background mode, scans now.
    pub(crate) fn rescan_in_background(&mut self) {
        if !self.background {
            self.rescan();
        } else if self.scanning.is_none() {
            self.start_scan();
        }
    }

    fn start_scan(&mut self) {
        let scan = self.scan();
        let thread = std::thread::Builder::new()
            .name("dawkey-scan".into())
            .spawn(move || scan.run());
        match thread {
            Ok(thread) => self.scanning = Some(thread),
            Err(e) => {
                tracing::warn!("starting a plugin scan: {e}");
                let catalog = self.scan().run();
                self.install(catalog);
            }
        }
    }

    /// A new catalog: plugins that failed to load may load now.
    fn install(&mut self, catalog: Catalog) {
        self.catalog = Some(catalog);
        self.host.refresh();
        self.errors.clear();
    }

    fn read_cache(&self) -> Option<Catalog> {
        let fingerprint = fingerprint(&self.search);
        let bytes = std::fs::read(self.cache.as_ref()?).ok()?;
        serde_json::from_slice::<Cached>(&bytes)
            .ok()
            .filter(|c| c.fingerprint == fingerprint && c.version == CACHE_VERSION)
            .map(|c| c.catalog)
    }

    /// Everything a scan needs, to run here or on another thread.
    fn scan(&self) -> Scan {
        Scan {
            search: self.search.clone(),
            scanner: self.scanner.clone(),
            cache: self.cache.clone(),
        }
    }

    /// The catalog entry for a slot's plugin. Waits for the catalog, except
    /// in the background mode, where it's `None` until the scan is done.
    pub(crate) fn info(&mut self, plugin: &str) -> Option<&PluginInfo> {
        if self.background {
            self.catalog_now()?.get(plugin)
        } else {
            self.catalog().get(plugin)
        }
    }

    pub(crate) fn provider<'a>(&'a mut self, states: &'a States) -> LiveProvider<'a> {
        LiveProvider {
            plugins: self,
            states,
        }
    }

    /// What the running plugins save of themselves now: every one, or with
    /// `dirty_only` the ones that said their state changed. Plugins that
    /// save nothing are left out.
    pub(crate) fn capture(&mut self, dirty_only: bool) -> Vec<(PluginId, PluginState)> {
        let mut states = Vec::new();
        let ids: Vec<PluginId> = self.live.keys().copied().collect();
        for id in ids {
            let paths = self.state_paths(id);
            let slot = self.live.get_mut(&id).expect("listed above");
            if dirty_only && !slot.instance.state_dirty() {
                continue;
            }
            match slot.instance.save_state(&paths) {
                Ok(Some(bytes)) => states.push((id, PluginState::new(bytes))),
                Ok(None) => {}
                Err(e) => tracing::warn!("plugin {id} ({}): {e}", slot.plugin),
            }
        }
        states
    }

    /// What slot `slot`'s running plugin saves of itself now, if it runs
    /// and saves anything.
    pub(crate) fn capture_one(&mut self, slot: PluginId) -> Option<PluginState> {
        let paths = self.state_paths(slot);
        let live = self.live.get_mut(&slot)?;
        match live.instance.save_state(&paths) {
            Ok(Some(bytes)) => Some(PluginState::new(bytes)),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!("plugin {slot} ({}): {e}", live.plugin);
                None
            }
        }
    }

    /// After a compile: slots that left the project wait for their
    /// processors to come back. Returns what they saved of themselves on
    /// the way out, so undo can bring them back as they were.
    pub(crate) fn forget_missing(&mut self, project: &Project) -> Vec<(PluginId, PluginState)> {
        let present: std::collections::HashSet<PluginId> = project
            .all_tracks()
            .flat_map(|t| t.plugins.iter().map(|p| p.id))
            .collect();
        let gone: Vec<PluginId> = self
            .live
            .keys()
            .filter(|id| !present.contains(id))
            .copied()
            .collect();
        let mut states = Vec::new();
        for id in gone {
            let mut slot = self.live.remove(&id).expect("listed above");
            match slot.instance.save_state(&self.state_paths(id)) {
                Ok(Some(bytes)) => states.push((id, PluginState::new(bytes))),
                Ok(None) => {}
                Err(e) => tracing::warn!("plugin {id} ({}): {e}", slot.plugin),
            }
            self.retiring.insert(slot.key, slot.instance);
        }
        self.errors.retain(|id, _| present.contains(id));
        states
    }

    /// Unloads processors the engine let go of. Returns true if one of them
    /// belonged to a slot still in the project (its graph never played), so
    /// the graph must be compiled again.
    pub(crate) fn take_back(&mut self, retired: Vec<(u64, Box<dyn PluginProcessor>)>) -> bool {
        let mut stale = false;
        for (key, processor) in retired {
            if let Some(instance) = self.retiring.remove(&key) {
                instance.unload(processor.into_any());
            } else if let Some(id) = self
                .live
                .iter()
                .find(|(_, s)| s.key == key)
                .map(|(id, _)| *id)
            {
                let slot = self.live.remove(&id).expect("found above");
                slot.instance.unload(processor.into_any());
                stale = true;
            }
        }
        stale
    }

    /// Parameter `index` of slot `slot` (plugin `info`) at `value`, as the
    /// plugin shows it (asking the running instance, if there is one).
    pub(crate) fn param_text(
        &mut self,
        slot: PluginId,
        info: &PluginInfo,
        index: usize,
        value: f64,
    ) -> String {
        let live = self.live.get_mut(&slot).filter(|s| s.plugin == info.id);
        live.and_then(|s| s.instance.param_text(info, index, value))
            .or_else(|| info.params.get(index).map(|p| p.text(value)))
            .unwrap_or_default()
    }

    /// Lets every loaded plugin do its control-thread work.
    /// Lets every loaded plugin do its control-thread work. Returns true
    /// if a plugin's latency changed (a bridged one's grew with the
    /// engine's blocks), so the graph must be compiled again.
    pub(crate) fn idle(&mut self) -> bool {
        let mut changed = false;
        for (&id, slot) in &mut self.live {
            slot.instance.idle();
            self.changes.extend(
                slot.instance
                    .take_param_changes()
                    .into_iter()
                    .map(|change| (id, change)),
            );
            if let Some(latency) = slot.instance.latency()
                && latency != slot.latency
            {
                slot.latency = latency;
                changed = true;
            }
        }
        changed
    }

    /// Unloads slot `slot`'s plugin (once the engine lets go of it) and
    /// forgets why it failed, so the next compile loads it again.
    pub(crate) fn reload(&mut self, slot: PluginId) {
        self.errors.remove(&slot);
        if let Some(live) = self.live.remove(&slot) {
            self.retiring.insert(live.key, live.instance);
        }
    }

    /// Parameters plugins changed themselves since the last call.
    pub(crate) fn take_param_changes(&mut self) -> Vec<(PluginId, ParamChange)> {
        std::mem::take(&mut self.changes)
    }

    /// Whether slot `slot`'s plugin has a GUI to show, and whether it's
    /// showing.
    pub(crate) fn gui(&self, slot: PluginId) -> (bool, bool) {
        self.live.get(&slot).map_or((false, false), |live| {
            (live.instance.has_gui(), live.instance.gui_visible())
        })
    }

    /// Shows or hides slot `slot`'s plugin's GUI.
    pub(crate) fn show_gui(&mut self, slot: PluginId, show: bool) -> Result<(), CoreError> {
        let live = self
            .live
            .get_mut(&slot)
            .ok_or_else(|| CoreError::InvalidArgument(format!("plugin {slot} isn't running")))?;
        live.instance
            .show_gui(show)
            .map_err(|e| CoreError::InvalidArgument(e.0))
    }

    /// The process a slot's plugin runs in, if it's bridged.
    pub(crate) fn process_id(&self, slot: PluginId) -> Option<u32> {
        self.live.get(&slot)?.instance.process_id()
    }

    /// Whether the process a slot's plugin runs in is sandboxed.
    pub(crate) fn sandboxed(&self, slot: PluginId) -> bool {
        self.live.get(&slot).is_some_and(|s| s.instance.sandboxed())
    }

    /// For a new project: every loaded slot is on its way out. A new sample
    /// rate means a new engine, whose old graph (and processors) is dropped
    /// whole, so their instances go now; it also needs a new host.
    pub(crate) fn reset(&mut self, sample_rate: u32) {
        self.errors.clear();
        if sample_rate != self.host.sample_rate() {
            self.live.clear();
            self.retiring.clear();
            self.new_host(sample_rate, self.search.clone());
            return;
        }
        for (_, slot) in self.live.drain() {
            self.retiring.insert(slot.key, slot.instance);
        }
    }

    pub(crate) fn problems(&self) -> Vec<PluginProblem> {
        let crashed = self.live.iter().filter_map(|(&plugin, slot)| {
            Some(PluginProblem {
                plugin,
                id: slot.plugin.clone(),
                error: slot.instance.crashed()?,
            })
        });
        let mut problems: Vec<PluginProblem> = self
            .errors
            .iter()
            .map(|(&plugin, (id, error))| PluginProblem {
                plugin,
                id: id.clone(),
                error: error.clone(),
            })
            .chain(crashed)
            .collect();
        problems.sort_by_key(|p| p.plugin);
        problems
    }

    /// Fresh instances for an offline render (job `job`).
    pub(crate) fn offline_provider<'a>(
        &'a mut self,
        job: u64,
        states: &'a States,
    ) -> OfflineProvider<'a> {
        OfflineProvider {
            plugins: self,
            states,
            job,
        }
    }

    /// Unloads an offline render's plugins.
    pub(crate) fn finish_offline(
        &mut self,
        job: u64,
        processors: Vec<(u64, Box<dyn PluginProcessor>)>,
    ) {
        let mut instances: HashMap<u64, Instance> = self
            .offline
            .remove(&job)
            .unwrap_or_default()
            .into_iter()
            .collect();
        for (key, processor) in processors {
            if let Some(instance) = instances.remove(&key) {
                instance.unload(processor.into_any());
            }
        }
    }

    /// Checks a command against the installed plugins: plugins exist, and
    /// parameter values are in range.
    pub(crate) fn check(&mut self, project: &Project, command: &Command) -> Result<(), CoreError> {
        let invalid = |m: String| Err(CoreError::InvalidArgument(m));
        match command {
            Command::AddPlugin(c) => {
                if self.info(&c.plugin).is_none() && self.catalog.is_none() {
                    return invalid(
                        "Dawkey is still scanning for plugins; try again in a moment".to_owned(),
                    );
                }
                if self.info(&c.plugin).is_none() {
                    return invalid(format!(
                        "plugin {} is not installed (list_plugins shows the installed ones)",
                        c.plugin
                    ));
                }
            }
            Command::SetPluginParam(c) => {
                let values: Vec<f64> = c.value.into_iter().collect();
                self.check_value(project, c.plugin, &c.param, &values)?;
            }
            Command::SetPluginSidechain(c) if c.source.is_some() => {
                if let Some(info) = self.slot_info(project, c.plugin)?
                    && !info.sidechain
                {
                    return invalid(format!("{} has no sidechain input", info.name));
                }
            }
            Command::SetPluginOutput(c) if c.bus.is_some() => {
                if let Some(info) = self.slot_info(project, c.plugin)?
                    && c.output as usize >= info.outputs.len()
                {
                    let outputs: Vec<String> = info
                        .outputs
                        .iter()
                        .enumerate()
                        .map(|(i, o)| format!("{i} ({})", o.name))
                        .collect();
                    return invalid(if outputs.is_empty() {
                        format!("{} has no other outputs", info.name)
                    } else {
                        format!(
                            "{} has no output {}; its other outputs are {}",
                            info.name,
                            c.output,
                            outputs.join(", ")
                        )
                    });
                }
            }
            Command::SetAutomation(c) => {
                if let AutomationTarget::Param { plugin, param } = &c.target {
                    let values: Vec<f64> = c.points.iter().map(|p| p.value).collect();
                    self.check_value(project, *plugin, param, &values)?;
                }
            }
            Command::AddAutomationPoint(c) => {
                if let AutomationTarget::Param { plugin, param } = &c.target {
                    self.check_value(project, *plugin, param, &[c.value])?;
                }
            }
            Command::WriteAutomation(c) => {
                if let AutomationTarget::Param { plugin, param } = &c.target {
                    let values: Vec<f64> = c.points.iter().map(|p| p.value).collect();
                    self.check_value(project, *plugin, param, &values)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// A `set_plugin_param` with text gets the value the plugin reads in
    /// it (asking the running instance, if there is one).
    pub(crate) fn resolve(
        &mut self,
        project: &Project,
        command: Command,
    ) -> Result<Command, CoreError> {
        let Command::SetPluginParam(mut c) = command else {
            return Ok(command);
        };
        let Some(text) = c.text.as_deref().filter(|_| c.value.is_none()) else {
            return Ok(Command::SetPluginParam(c));
        };
        // Reports a missing plugin or parameter.
        self.check_value(project, c.plugin, &c.param, &[])?;
        let Some(info) = self.slot_info(project, c.plugin)? else {
            return Ok(Command::SetPluginParam(c));
        };
        let index = info.param_index(&c.param).expect("checked above");
        let live = self.live.get_mut(&c.plugin).filter(|s| s.plugin == info.id);
        let value = match live {
            Some(slot) => slot.instance.param_value(&info, index, text),
            None => info.params[index].parse(text),
        };
        let Some(value) = value else {
            let p = &info.params[index];
            let example = self.param_text(c.plugin, &info, index, p.default);
            return Err(CoreError::InvalidArgument(format!(
                "{} ({}) can't read {text:?}; it shows values like {example:?}",
                p.name, p.id
            )));
        };
        c.value = Some(value);
        c.text = None;
        Ok(Command::SetPluginParam(c))
    }

    /// The catalog entry of slot `slot`'s plugin; `None` if the slot
    /// doesn't exist (the command reports that).
    fn slot_info(
        &mut self,
        project: &Project,
        slot: PluginId,
    ) -> Result<Option<PluginInfo>, CoreError> {
        let Some((_, slot)) = project.plugin(slot) else {
            return Ok(None);
        };
        match self.info(&slot.plugin) {
            Some(info) => Ok(Some(info.clone())),
            None => Err(CoreError::InvalidArgument(format!(
                "plugin {} is not installed",
                slot.plugin
            ))),
        }
    }

    fn check_value(
        &mut self,
        project: &Project,
        slot: PluginId,
        param: &str,
        values: &[f64],
    ) -> Result<(), CoreError> {
        let invalid = |m: String| Err(CoreError::InvalidArgument(m));
        let Some((_, slot_data)) = project.plugin(slot) else {
            // The command itself reports this.
            return Ok(());
        };
        let Some(info) = self.info(&slot_data.plugin) else {
            return invalid(format!("plugin {} is not installed", slot_data.plugin));
        };
        let Some(p) = info.params.iter().find(|p| p.id == param) else {
            let ids: Vec<String> = info
                .params
                .iter()
                .take(12)
                .map(|p| format!("{} ({})", p.id, p.name))
                .collect();
            return invalid(format!(
                "{} has no parameter {param:?}; its parameters include {}",
                info.name,
                ids.join(", ")
            ));
        };
        if let Some(bad) = values
            .iter()
            .find(|&&v| !(p.min - 1e-9..=p.max + 1e-9).contains(&v))
        {
            return invalid(format!(
                "{} ({}) goes from {} to {}; got {bad}",
                p.name, p.id, p.min, p.max
            ));
        }
        Ok(())
    }
}

impl Drop for Plugins {
    fn drop(&mut self) {
        // Instances whose processors never came back go now; clack and the
        // LV2 processor clean up whichever half is dropped last.
        self.live.clear();
        self.retiring.clear();
    }
}

/// What plugins saved of themselves, by slot.
pub(crate) type States = BTreeMap<PluginId, PluginState>;

/// Supplies the live engine: reuses the processor already running for a
/// slot, and loads the others (from their saved state).
pub(crate) struct LiveProvider<'a> {
    plugins: &'a mut Plugins,
    states: &'a States,
}

impl PluginProvider for LiveProvider<'_> {
    fn provide(&mut self, _track: TrackId, slot: &PluginSlot) -> Option<ProvidedPlugin> {
        let plugins = &mut *self.plugins;
        if plugins
            .errors
            .get(&slot.id)
            .is_some_and(|(id, _)| *id == slot.plugin)
        {
            return None;
        }
        let Some(info) = plugins.info(&slot.plugin).cloned() else {
            // No catalog: still scanning, and loaded once the scan is done.
            plugins.catalog.as_ref()?;
            plugins
                .errors
                .insert(slot.id, (slot.plugin.clone(), "not installed".to_owned()));
            return None;
        };
        let params: Vec<(String, f64)> = info
            .params
            .iter()
            .map(|p| (p.id.clone(), p.default))
            .collect();
        if let Some(live) = plugins.live.get(&slot.id)
            && live.plugin == slot.plugin
        {
            return Some(ProvidedPlugin {
                key: live.key,
                processor: None,
                params,
                latency: live.latency,
                instrument: info.instrument,
                midi_output: info.midi_output,
                sidechain: info.sidechain,
                outputs: info.outputs.len() as u32,
            });
        }
        let state = self.states.get(&slot.id).map(PluginState::bytes);
        let paths = plugins.state_paths(slot.id);
        let waits = plugins.bridge_waits;
        match plugins.host.load_for(&info, state, &paths, waits) {
            Ok(loaded) => {
                let key = plugins.next_key;
                plugins.next_key += 1;
                if let Some(old) = plugins.live.insert(
                    slot.id,
                    LiveSlot {
                        key,
                        plugin: slot.plugin.clone(),
                        instance: loaded.instance,
                        latency: loaded.latency,
                    },
                ) {
                    plugins.retiring.insert(old.key, old.instance);
                }
                Some(ProvidedPlugin {
                    key,
                    processor: Some(loaded.processor),
                    params,
                    latency: loaded.latency,
                    instrument: info.instrument,
                    midi_output: info.midi_output,
                    sidechain: info.sidechain,
                    outputs: info.outputs.len() as u32,
                })
            }
            Err(e) => {
                tracing::warn!("plugin {} ({}): {e}", slot.id, slot.plugin);
                plugins.errors.insert(slot.id, (slot.plugin.clone(), e.0));
                None
            }
        }
    }
}

/// Supplies an offline render with fresh instances, from saved states.
pub(crate) struct OfflineProvider<'a> {
    plugins: &'a mut Plugins,
    states: &'a States,
    job: u64,
}

impl PluginProvider for OfflineProvider<'_> {
    fn provide(&mut self, _track: TrackId, slot: &PluginSlot) -> Option<ProvidedPlugin> {
        let plugins = &mut *self.plugins;
        let info = plugins.info(&slot.plugin).cloned()?;
        let state = self.states.get(&slot.id).map(PluginState::bytes);
        let paths = plugins.state_paths(slot.id);
        // Bridged plugins are waited for each block: no added latency.
        let loaded = plugins.host.load_for(&info, state, &paths, true).ok()?;
        let key = plugins.next_key;
        plugins.next_key += 1;
        plugins
            .offline
            .entry(self.job)
            .or_default()
            .push((key, loaded.instance));
        Some(ProvidedPlugin {
            key,
            processor: Some(loaded.processor),
            params: info
                .params
                .iter()
                .map(|p| (p.id.clone(), p.default))
                .collect(),
            latency: loaded.latency,
            instrument: info.instrument,
            midi_output: info.midi_output,
            sidechain: info.sidechain,
            outputs: info.outputs.len() as u32,
        })
    }
}

/// A scan of every plugin, written to the cache.
struct Scan {
    search: SearchPath,
    scanner: Option<(PathBuf, Vec<String>)>,
    cache: Option<PathBuf>,
}

impl Scan {
    fn run(self) -> Catalog {
        let fingerprint = fingerprint(&self.search);
        let args: Vec<&str> = self
            .scanner
            .as_ref()
            .map(|(_, a)| a.iter().map(String::as_str).collect())
            .unwrap_or_default();
        let scanner = self.scanner.as_ref().map(|(p, _)| (p.as_path(), &args[..]));
        let catalog = dawkey_plugins::scan(&self.search, scanner, SCAN_TIMEOUT);
        if let Some(path) = &self.cache {
            let cached = Cached {
                version: CACHE_VERSION,
                fingerprint,
                catalog: catalog.clone(),
            };
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| {
                    std::fs::write(path, serde_json::to_vec(&cached).expect("serializes"))
                });
            if let Err(e) = written {
                tracing::warn!("caching the plugin list in {}: {e}", path.display());
            }
        }
        catalog
    }
}

/// `$XDG_CACHE_HOME/dawkey/plugins.json`.
/// `$XDG_CACHE_HOME/dawkey/plugins.json`, where the plugin list is cached.
pub fn default_cache() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))?;
    Some(cache.join("dawkey").join("plugins.json"))
}

/// Changes when plugins are installed, removed or updated: the CLAP files,
/// VST3 binaries and LV2 manifests with their sizes and times.
fn fingerprint(search: &SearchPath) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut stamp = |path: &Path| {
        path.hash(&mut hasher);
        if let Ok(meta) = std::fs::metadata(path) {
            meta.len().hash(&mut hasher);
            meta.modified().ok().hash(&mut hasher);
        }
    };
    for file in dawkey_plugins::clap_files(search) {
        stamp(&file);
    }
    for bundle in dawkey_plugins::vst3_bundles(search) {
        stamp(&dawkey_plugins::vst3_binary(&bundle));
    }
    let lv2_dirs: Vec<PathBuf> = match &search.lv2 {
        Some(dirs) => dirs.clone(),
        None => {
            let mut dirs = Vec::new();
            if let Some(home) = std::env::var_os("HOME") {
                dirs.push(Path::new(&home).join(".lv2"));
            }
            dirs.extend(
                ["/usr/lib64/lv2", "/usr/lib/lv2", "/usr/local/lib/lv2"].map(PathBuf::from),
            );
            dirs
        }
    };
    for dir in lv2_dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut bundles: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        bundles.sort();
        for bundle in bundles {
            stamp(&bundle.join("manifest.ttl"));
        }
    }
    hasher.finish()
}
