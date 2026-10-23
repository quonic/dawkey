// SPDX-License-Identifier: AGPL-3.0-or-later
//! CLAP hosting through clack-host.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::ffi::CString;
use std::os::fd::RawFd;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfoBuffer, AudioPortRescanFlags, HostAudioPorts, HostAudioPortsImpl,
    PluginAudioPorts,
};
use clack_extensions::gui::{
    GuiApiType, GuiConfiguration, GuiSize, HostGui, HostGuiImpl, PluginGui, Window as GuiWindow,
};
use clack_extensions::latency::{HostLatency, HostLatencyImpl, PluginLatency};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::note_ports::{
    HostNotePorts, HostNotePortsImpl, NoteDialects, NotePortInfoBuffer, NotePortRescanFlags,
    PluginNotePorts,
};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamInfoBuffer,
    ParamInfoFlags, ParamRescanFlags, PluginParams,
};
use clack_extensions::posix_fd::{FdFlags, HostPosixFd, HostPosixFdImpl, PluginPosixFd};
use clack_extensions::state::{HostState, HostStateImpl, PluginState};
use clack_extensions::timer::{HostTimer, HostTimerImpl, PluginTimer, TimerId};
use clack_host::events::event_types::{
    MidiEvent, NoteExpressionEvent, NoteExpressionType, NoteOffEvent, NoteOnEvent, ParamValueEvent,
    TransportEvent, TransportFlags,
};
use clack_host::events::spaces::CoreEventSpace;
use clack_host::events::{EventFlags, EventHeader, Match};
use clack_host::prelude::*;
use clack_host::utils::{BeatTime, SecondsTime};
use dawkey_engine::{
    MAX_BLOCK, MAX_PLUGIN_EVENTS, MidiSink, PerNote, PluginEvent, PluginEventKind, PluginProcessor,
    ProcessContext, Transport,
};

use crate::expression::{self, DEFAULT_EXPRESSIONS, NoteExpression};
use crate::ports::{self, PortBuffer, Ports};
use crate::run_loop::Timers;
use crate::{OutputInfo, ParamChange, ParamInfo, PluginError, PluginFormat, PluginInfo};

/// Parameter changes from the plugin a block can hold for the control
/// thread.
const PARAM_CHANGES: usize = 1024;

struct DkHost;

struct Shared {
    /// The plugin asked for `on_main_thread`.
    callback: AtomicBool,
    /// What it asked of its GUI's window.
    gui: Mutex<GuiRequests>,
}

/// What a plugin asked of its GUI's window since the host last looked.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GuiRequests {
    /// A new size for the window.
    pub(crate) resize: Option<(u32, u32)>,
    /// Its GUI closed itself (or lost its connection).
    pub(crate) closed: bool,
}

/// How a plugin's GUI is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuiMode {
    /// In an X11 window the host makes.
    X11Embedded,
    /// In the plugin's own window.
    Floating(&'static std::ffi::CStr),
}

impl<'a> SharedHandler<'a> for Shared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {
        self.callback.store(true, Ordering::Release);
    }
}

struct MainThread {
    /// The plugin said its state changed.
    dirty: Cell<bool>,
    /// Timers and descriptors it wants run on its main thread (its GUI's
    /// event loop, on Linux).
    timers: RefCell<Timers<u32>>,
    next_timer: Cell<u32>,
    fds: RefCell<Vec<(RawFd, FdFlags)>>,
}

impl HostTimerImpl for MainThread {
    fn register_timer(&self, period_ms: u32) -> Result<TimerId, HostError> {
        let id = self.next_timer.get();
        self.next_timer.set(id.wrapping_add(1));
        self.timers.borrow_mut().add(id, u64::from(period_ms));
        Ok(TimerId(id))
    }

    fn unregister_timer(&self, timer_id: TimerId) -> Result<(), HostError> {
        self.timers.borrow_mut().remove(|&id| id == timer_id.0);
        Ok(())
    }
}

impl HostPosixFdImpl for MainThread {
    fn register_fd(&self, fd: RawFd, flags: FdFlags) -> Result<(), HostError> {
        let mut fds = self.fds.borrow_mut();
        fds.retain(|(f, _)| *f != fd);
        fds.push((fd, flags));
        Ok(())
    }

    fn modify_fd(&self, fd: RawFd, flags: FdFlags) -> Result<(), HostError> {
        match self.fds.borrow_mut().iter_mut().find(|(f, _)| *f == fd) {
            Some(entry) => {
                entry.1 = flags;
                Ok(())
            }
            None => Err(HostError::Message("that descriptor isn't registered")),
        }
    }

    fn unregister_fd(&self, fd: RawFd) -> Result<(), HostError> {
        self.fds.borrow_mut().retain(|(f, _)| *f != fd);
        Ok(())
    }
}

impl HostGuiImpl for Shared {
    fn resize_hints_changed(&self) {}

    fn request_resize(&self, size: GuiSize) -> Result<(), HostError> {
        self.gui.lock().unwrap_or_else(|e| e.into_inner()).resize = Some((size.width, size.height));
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn request_hide(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn closed(&self, _was_destroyed: bool) {
        self.gui.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
    }
}

impl<'a> MainThreadHandler<'a> for MainThread {}

impl HostStateImpl for MainThread {
    fn mark_dirty(&self) {
        self.dirty.set(true);
    }
}

impl HostHandlers for DkHost {
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder
            .register::<HostLog>()
            .register::<HostParams>()
            .register::<HostLatency>()
            .register::<HostAudioPorts>()
            .register::<HostNotePorts>()
            .register::<HostState>()
            .register::<HostGui>()
            .register::<HostTimer>()
            .register::<HostPosixFd>();
    }
}

impl HostLogImpl for Shared {
    fn log(&self, severity: LogSeverity, message: &str) {
        match severity {
            LogSeverity::Debug => tracing::debug!("plugin: {message}"),
            LogSeverity::Info => tracing::info!("plugin: {message}"),
            LogSeverity::Warning => tracing::warn!("plugin: {message}"),
            _ => tracing::error!("plugin: {message}"),
        }
    }
}

impl HostParamsImplShared for Shared {
    fn request_flush(&self) {
        // The engine processes continuously; changes reach the plugin there.
    }
}

impl HostParamsImplMainThread for MainThread {
    fn rescan(&self, _flags: ParamRescanFlags) {}
    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

impl HostLatencyImpl for MainThread {
    fn changed(&self) {
        // Read again only when the plugin is next activated.
    }
}

impl HostAudioPortsImpl for MainThread {
    fn is_rescan_flag_supported(&self, _flag: AudioPortRescanFlags) -> bool {
        false
    }
    fn rescan(&self, _flags: AudioPortRescanFlags) {}
}

impl HostNotePortsImpl for MainThread {
    fn supported_dialects(&self) -> NoteDialects {
        NoteDialects::CLAP | NoteDialects::MIDI
    }
    fn rescan(&self, _flags: NotePortRescanFlags) {}
}

fn host_info() -> HostInfo {
    HostInfo::new(
        "Dawkey",
        "Dawkey",
        "https://github.com/quonic/dawkey",
        env!("CARGO_PKG_VERSION"),
    )
    .expect("host info has no NUL bytes")
}

fn load_entry(path: &Path) -> Result<PluginEntry, PluginError> {
    // SAFETY: loading a plugin library runs its initialization code; that's
    // the risk every plugin host takes (and why scans run in a subprocess).
    unsafe { PluginEntry::load(path) }
        .map_err(|e| PluginError::new(format!("{}: {e}", path.display())))
}

fn new_instance(entry: &PluginEntry, id: &str) -> Result<PluginInstance<DkHost>, PluginError> {
    let id_c = CString::new(id).map_err(|_| PluginError::new("bad plugin id"))?;
    PluginInstance::<DkHost>::new(
        |_| Shared {
            callback: AtomicBool::new(false),
            gui: Mutex::new(GuiRequests::default()),
        },
        |_| MainThread {
            dirty: Cell::new(false),
            timers: RefCell::new(Timers::default()),
            next_timer: Cell::new(1),
            fds: RefCell::new(Vec::new()),
        },
        entry,
        &id_c,
        &host_info(),
    )
    .map_err(|e| PluginError::new(format!("{id}: {e}")))
}

fn audio_ports(instance: &mut PluginInstance<DkHost>, input: bool) -> Ports {
    let handle = instance.plugin_handle();
    let Some(ext) = handle.get_extension::<PluginAudioPorts>() else {
        // No extension: the CLAP default is one stereo port each way.
        return Ports {
            channels: vec![2],
            names: vec!["main".to_owned()],
            main: Some(0),
        };
    };
    let mut buffer = AudioPortInfoBuffer::new();
    let mut ports = Ports::default();
    for i in 0..ext.count(&handle, input) {
        let Some(info) = ext.get(&handle, i, input, &mut buffer) else {
            continue;
        };
        if info.flags.contains(AudioPortFlags::IS_MAIN) && ports.main.is_none() {
            ports.main = Some(ports.channels.len());
        }
        ports.channels.push(info.channel_count);
        ports
            .names
            .push(String::from_utf8_lossy(info.name).trim().to_owned());
    }
    if ports.main.is_none() && !ports.channels.is_empty() {
        ports.main = Some(0);
    }
    ports
}

/// Whether the plugin has a note output port (it sends MIDI).
fn sends_notes(instance: &mut PluginInstance<DkHost>) -> bool {
    let handle = instance.plugin_handle();
    let Some(ext) = handle.get_extension::<PluginNotePorts>() else {
        return false;
    };
    let mut buffer = NotePortInfoBuffer::new();
    (0..ext.count(&handle, false)).any(|i| {
        ext.get(&handle, i, false, &mut buffer).is_some_and(|info| {
            info.supported_dialects
                .intersects(NoteDialects::CLAP | NoteDialects::MIDI)
        })
    })
}

/// The first note input port, and whether it only speaks MIDI.
fn note_port(instance: &mut PluginInstance<DkHost>) -> Option<(u16, bool)> {
    let handle = instance.plugin_handle();
    let ext = handle.get_extension::<PluginNotePorts>()?;
    let mut buffer = NotePortInfoBuffer::new();
    (0..ext.count(&handle, true).min(u32::from(u16::MAX))).find_map(|i| {
        let info = ext.get(&handle, i, true, &mut buffer)?;
        let dialects = info.supported_dialects;
        dialects
            .intersects(NoteDialects::CLAP | NoteDialects::MIDI)
            .then(|| (i as u16, !dialects.intersects(NoteDialects::CLAP)))
    })
}

fn params(instance: &mut PluginInstance<DkHost>) -> Vec<ParamInfo> {
    let handle = instance.plugin_handle();
    let Some(ext) = handle.get_extension::<PluginParams>() else {
        return Vec::new();
    };
    let mut buffer = ParamInfoBuffer::new();
    (0..ext.count(&handle))
        .filter_map(|i| {
            let info = ext.get_info(&handle, i, &mut buffer)?;
            if info
                .flags
                .intersects(ParamInfoFlags::IS_HIDDEN | ParamInfoFlags::IS_READONLY)
            {
                return None;
            }
            Some(ParamInfo {
                id: u32::from(info.id).to_string(),
                name: String::from_utf8_lossy(info.name).trim().to_owned(),
                min: info.min_value,
                max: info.max_value,
                default: info.default_value,
                stepped: info.flags.contains(ParamInfoFlags::IS_STEPPED),
                unit: None,
                labels: Vec::new(),
            })
        })
        .collect()
}

/// Describes every plugin in a `.clap` file. Runs plugin code.
pub(crate) fn scan_file(path: &Path) -> Result<Vec<PluginInfo>, PluginError> {
    let entry = load_entry(path)?;
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| PluginError::new("no plugin factory"))?;
    let descriptors: Vec<(String, String, String)> = factory
        .plugin_descriptors()
        .filter_map(|d| {
            let text = |s: Option<&std::ffi::CStr>| {
                s.map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };
            let id = d.id()?.to_str().ok()?.to_owned();
            Some((id, text(d.name()), text(d.vendor())))
        })
        .collect();
    let mut found = Vec::new();
    for (id, name, vendor) in descriptors {
        let mut instance = match new_instance(&entry, &id) {
            Ok(instance) => instance,
            Err(e) => {
                tracing::warn!("{}: {e}", path.display());
                continue;
            }
        };
        let inputs = audio_ports(&mut instance, true);
        let outputs = audio_ports(&mut instance, false);
        found.push(PluginInfo {
            id: format!("clap:{id}"),
            format: PluginFormat::Clap,
            name: if name.is_empty() { id.clone() } else { name },
            vendor,
            path: path.to_owned(),
            instrument: note_port(&mut instance).is_some(),
            midi_output: sends_notes(&mut instance),
            audio_inputs: inputs.main_channels(),
            audio_outputs: outputs.main_channels(),
            sidechain: inputs.others().next().is_some(),
            outputs: outputs
                .others()
                .map(|i| OutputInfo {
                    name: outputs.names[i].clone(),
                    channels: outputs.channels[i],
                })
                .collect(),
            params: params(&mut instance),
        });
    }
    Ok(found)
}

pub(crate) struct ClapInstance {
    instance: PluginInstance<DkHost>,
    /// Parameters the plugin changed itself, from the processor.
    changes: rtrb::Consumer<ParamChange>,
}

impl ClapInstance {
    /// The plugin's text for parameter `id` (a CLAP ID in decimal) at
    /// `value`.
    pub(crate) fn param_text(&mut self, id: &str, value: f64) -> Option<String> {
        let id = ClapId::new(id.parse().ok()?);
        let handle = self.instance.plugin_handle();
        let ext = handle.get_extension::<PluginParams>()?;
        let mut buffer = [0u8; 128];
        let text = ext.value_to_text(&handle, id, value, &mut buffer).ok()?;
        let text = String::from_utf8_lossy(text).trim().to_owned();
        (!text.is_empty()).then_some(text)
    }

    pub(crate) fn param_value(&mut self, id: &str, text: &str) -> Option<f64> {
        let id = ClapId::new(id.parse().ok()?);
        let text = std::ffi::CString::new(text.trim()).ok()?;
        let handle = self.instance.plugin_handle();
        let ext = handle.get_extension::<PluginParams>()?;
        ext.text_to_value(&handle, id, &text)
    }

    /// What the plugin saves of itself, or `None` if it saves nothing.
    pub(crate) fn save_state(&mut self) -> Result<Option<Vec<u8>>, PluginError> {
        self.instance.access_handler(|h| h.dirty.set(false));
        let handle = self.instance.plugin_handle();
        let Some(ext) = handle.get_extension::<PluginState>() else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        ext.save(&handle, &mut bytes)
            .map_err(|e| PluginError::new(format!("saving its state failed: {e}")))?;
        Ok(Some(bytes))
    }

    /// Whether the plugin said its state changed since it was last saved.
    pub(crate) fn state_dirty(&self) -> bool {
        self.instance.access_handler(|h| h.dirty.get())
    }

    pub(crate) fn take_param_changes(&mut self) -> Vec<ParamChange> {
        let n = self.changes.slots();
        let Ok(chunk) = self.changes.read_chunk(n) else {
            return Vec::new();
        };
        chunk.into_iter().collect()
    }

    /// How its GUI can be shown, if it has one: embedded in an X11 window
    /// if it can be, otherwise in its own (X11, then Wayland).
    pub(crate) fn gui_mode(&mut self) -> Option<GuiMode> {
        let handle = self.instance.plugin_handle();
        let gui = handle.get_extension::<PluginGui>()?;
        let supports = |api: GuiApiType<'static>, is_floating| {
            gui.is_api_supported(
                &handle,
                GuiConfiguration {
                    api_type: api,
                    is_floating,
                },
            )
        };
        if supports(GuiApiType::X11, false) {
            Some(GuiMode::X11Embedded)
        } else if supports(GuiApiType::X11, true) {
            Some(GuiMode::Floating(c"x11"))
        } else if supports(GuiApiType::WAYLAND, true) {
            Some(GuiMode::Floating(c"wayland"))
        } else {
            None
        }
    }

    /// Opens its GUI: embedded in X11 window `parent`, or floating (titled
    /// `title`). Returns the size it wants (embedded).
    pub(crate) fn gui_open(
        &mut self,
        mode: GuiMode,
        parent: Option<u32>,
        title: &str,
    ) -> Result<Option<(u32, u32)>, PluginError> {
        let handle = self.instance.plugin_handle();
        let gui = handle
            .get_extension::<PluginGui>()
            .ok_or_else(|| PluginError::new("it has no GUI"))?;
        let (api, is_floating) = match mode {
            GuiMode::X11Embedded => (GuiApiType::X11, false),
            GuiMode::Floating(api) => (GuiApiType(api), true),
        };
        gui.create(
            &handle,
            GuiConfiguration {
                api_type: api,
                is_floating,
            },
        )
        .map_err(|e| PluginError::new(format!("opening its GUI failed: {e}")))?;
        let size = if let Some(parent) = parent {
            let size = gui.get_size(&handle).map(|s| (s.width, s.height));
            // SAFETY: `parent` is a live X11 window the host process made
            // for this GUI, and outlives it (destroyed after `gui_close`).
            let set = unsafe { gui.set_parent(&handle, GuiWindow::from_x11_handle(parent.into())) };
            if let Err(e) = set {
                gui.destroy(&handle);
                return Err(PluginError::new(format!("embedding its GUI failed: {e}")));
            }
            size
        } else {
            if let Ok(title) = CString::new(title) {
                gui.suggest_title(&handle, &title);
            }
            None
        };
        if let Err(e) = gui.show(&handle) {
            gui.destroy(&handle);
            return Err(PluginError::new(format!("showing its GUI failed: {e}")));
        }
        Ok(size)
    }

    pub(crate) fn gui_close(&mut self) {
        let handle = self.instance.plugin_handle();
        if let Some(gui) = handle.get_extension::<PluginGui>() {
            let _ = gui.hide(&handle);
            gui.destroy(&handle);
        }
    }

    pub(crate) fn gui_can_resize(&mut self) -> bool {
        let handle = self.instance.plugin_handle();
        handle
            .get_extension::<PluginGui>()
            .is_some_and(|gui| gui.can_resize(&handle))
    }

    /// The window was resized: the GUI takes the nearest size it can.
    /// Returns that size.
    pub(crate) fn gui_resized(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let handle = self.instance.plugin_handle();
        let gui = handle.get_extension::<PluginGui>()?;
        if !gui.can_resize(&handle) {
            return None;
        }
        let wanted = GuiSize { width, height };
        let size = gui.adjust_size(&handle, wanted).unwrap_or(wanted);
        gui.set_size(&handle, size).ok()?;
        Some((size.width, size.height))
    }

    /// What it asked of its window since the last call.
    pub(crate) fn gui_requests(&mut self) -> GuiRequests {
        self.instance.access_shared_handler(|s| {
            std::mem::take(&mut *s.gui.lock().unwrap_or_else(|e| e.into_inner()))
        })
    }

    /// The descriptors it wants watched, as `poll` events.
    pub(crate) fn poll_fds(&self) -> Vec<(RawFd, i16)> {
        self.instance.access_handler(|h| {
            h.fds
                .borrow()
                .iter()
                .map(|&(fd, flags)| {
                    let mut events = 0;
                    if flags.contains(FdFlags::READ) {
                        events |= libc::POLLIN;
                    }
                    if flags.contains(FdFlags::WRITE) {
                        events |= libc::POLLOUT;
                    }
                    (fd, events)
                })
                .collect()
        })
    }

    /// Descriptor `fd` is ready (`revents` from `poll`).
    pub(crate) fn on_fd(&mut self, fd: RawFd, revents: i16) {
        let mut flags = FdFlags::empty();
        flags.set(FdFlags::READ, revents & libc::POLLIN != 0);
        flags.set(FdFlags::WRITE, revents & libc::POLLOUT != 0);
        flags.set(
            FdFlags::ERROR,
            revents & (libc::POLLERR | libc::POLLHUP) != 0,
        );
        let handle = self.instance.plugin_handle();
        if let Some(ext) = handle.get_extension::<PluginPosixFd>() {
            ext.on_fd(&handle, fd, flags);
        }
    }

    /// Runs the timers that are due. Returns when the next one is.
    pub(crate) fn run_timers(&mut self, now: Instant) -> Option<Instant> {
        let due = self
            .instance
            .access_handler(|h| h.timers.borrow_mut().due(now));
        if !due.is_empty() {
            let handle = self.instance.plugin_handle();
            if let Some(ext) = handle.get_extension::<PluginTimer>() {
                for id in due {
                    ext.on_timer(&handle, TimerId(id));
                }
            }
        }
        self.instance.access_handler(|h| h.timers.borrow().next())
    }

    pub(crate) fn idle(&mut self) {
        let asked = self
            .instance
            .access_shared_handler(|s| s.callback.swap(false, Ordering::AcqRel));
        if asked {
            self.instance.call_on_main_thread_callback();
        }
    }

    pub(crate) fn unload(mut self, processor: Box<dyn Any + Send>) {
        match processor.downcast::<ClapProcessor>() {
            Ok(processor) => {
                let stopped = processor.processor.into_stopped();
                self.instance.deactivate(stopped);
            }
            Err(_) => tracing::error!("a CLAP instance was given the wrong processor"),
        }
    }
}

pub(crate) struct ClapProcessor {
    processor: clack_host::process::PluginAudioProcessor<DkHost>,
    input_ports: AudioPorts,
    output_ports: AudioPorts,
    inputs: Vec<PortBuffer>,
    outputs: Vec<PortBuffer>,
    main_in: Option<usize>,
    main_out: Option<usize>,
    /// The sidechain input port (the first other input).
    side_in: Option<usize>,
    /// The other output ports, in order.
    other_out: Box<[usize]>,
    events: EventBuffer,
    /// What the plugin sends.
    out_events: EventBuffer,
    /// CLAP IDs of the parameters, by engine index.
    param_ids: Box<[ClapId]>,
    /// Parameters the plugin changed itself, for the control thread.
    changes: rtrb::Producer<ParamChange>,
    note_port: Option<(u16, bool)>,
    steady: u64,
}

/// Instantiates and activates `info`'s plugin.
pub(crate) fn load(
    info: &PluginInfo,
    sample_rate: u32,
    state: Option<&[u8]>,
) -> Result<(ClapInstance, ClapProcessor, u32), PluginError> {
    let entry = load_entry(&info.path)?;
    let id = info.id.strip_prefix("clap:").unwrap_or(&info.id);
    let mut instance = new_instance(&entry, id)?;
    if let Some(state) = state {
        let handle = instance.plugin_handle();
        let restored = match handle.get_extension::<PluginState>() {
            Some(ext) => ext
                .load(&handle, &mut &state[..])
                .map_err(|e| e.to_string()),
            None => Err("it doesn't save state".to_owned()),
        };
        // The plugin still works, as it starts.
        if let Err(e) = restored {
            tracing::warn!("{id}: restoring its state failed: {e}");
        }
    }
    let ins = audio_ports(&mut instance, true);
    let outs = audio_ports(&mut instance, false);
    let note_port = note_port(&mut instance);
    let config = PluginAudioConfiguration {
        sample_rate: f64::from(sample_rate),
        min_frames_count: 1,
        max_frames_count: MAX_BLOCK as u32,
    };
    let stopped = instance
        .activate(|_, _| (), config)
        .map_err(|e| PluginError::new(format!("{id}: activating failed: {e}")))?;
    let latency = {
        let handle = instance.plugin_handle();
        handle
            .get_extension::<PluginLatency>()
            .map_or(0, |ext| ext.get(&handle))
    };
    let param_ids = info
        .params
        .iter()
        .map(|p| p.id.parse::<u32>().map(ClapId::new))
        .collect::<Result<Box<[_]>, _>>()
        .map_err(|_| PluginError::new(format!("{id}: bad parameter id in the catalog")))?;
    let (changes, changes_out) = rtrb::RingBuffer::new(PARAM_CHANGES);
    let processor = ClapProcessor {
        processor: stopped.into(),
        input_ports: AudioPorts::with_capacity(ins.total(), ins.channels.len()),
        output_ports: AudioPorts::with_capacity(outs.total(), outs.channels.len()),
        inputs: ins.buffers(),
        outputs: outs.buffers(),
        main_in: ins.main,
        main_out: outs.main,
        side_in: ins.others().next(),
        other_out: outs.others().collect(),
        events: EventBuffer::with_capacity(MAX_PLUGIN_EVENTS),
        out_events: EventBuffer::with_capacity(MAX_PLUGIN_EVENTS),
        param_ids,
        changes,
        note_port,
        steady: 0,
    };
    Ok((
        ClapInstance {
            instance,
            changes: changes_out,
        },
        processor,
        latency,
    ))
}

/// The CLAP note expression for a MIDI 2.0 per-note controller (32-bit
/// value), if there is one.
fn note_expression(controller: PerNote, value: u32) -> Option<(NoteExpressionType, f64)> {
    expression::from_per_note(controller, value).map(|(e, value)| (clap_expression(e), value))
}

fn clap_expression(expression: NoteExpression) -> NoteExpressionType {
    match expression {
        NoteExpression::Tuning => NoteExpressionType::Tuning,
        NoteExpression::Volume => NoteExpressionType::Volume,
        NoteExpression::Pan => NoteExpressionType::Pan,
        NoteExpression::Expression => NoteExpressionType::Expression,
        NoteExpression::Brightness => NoteExpressionType::Brightness,
        NoteExpression::Vibrato => NoteExpressionType::Vibrato,
        NoteExpression::Pressure => NoteExpressionType::Pressure,
    }
}

/// The note expression with this raw CLAP ID. (clack 0.2's own
/// `NoteExpressionType::from_raw` leaves out "expression".)
fn expression_from_raw(raw: i32) -> Option<NoteExpression> {
    [
        NoteExpression::Volume,
        NoteExpression::Pan,
        NoteExpression::Tuning,
        NoteExpression::Vibrato,
        NoteExpression::Expression,
        NoteExpression::Brightness,
        NoteExpression::Pressure,
    ]
    .into_iter()
    .find(|&e| clap_expression(e).into_raw() == raw)
}

/// The transport as CLAP has it.
fn transport_event(t: &Transport) -> TransportEvent {
    let mut flags = TransportFlags::HAS_TEMPO
        | TransportFlags::HAS_BEATS_TIMELINE
        | TransportFlags::HAS_SECONDS_TIMELINE
        | TransportFlags::HAS_TIME_SIGNATURE;
    flags.set(TransportFlags::IS_PLAYING, t.playing);
    flags.set(TransportFlags::IS_RECORDING, t.recording);
    flags.set(TransportFlags::IS_LOOP_ACTIVE, t.loop_beats.is_some());
    let (loop_start, loop_end) = t.loop_beats.unwrap_or_default();
    let (loop_start_s, loop_end_s) = t.loop_seconds.unwrap_or_default();
    TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags,
        song_pos_beats: BeatTime::from_float(t.beats),
        song_pos_seconds: SecondsTime::from_float(t.seconds),
        tempo: t.bpm,
        tempo_inc: t.bpm_per_frame,
        loop_start_beats: BeatTime::from_float(loop_start),
        loop_end_beats: BeatTime::from_float(loop_end),
        loop_start_seconds: SecondsTime::from_float(loop_start_s),
        loop_end_seconds: SecondsTime::from_float(loop_end_s),
        bar_start: BeatTime::from_float(t.bar_start_beats),
        bar_number: i32::try_from(t.bar).unwrap_or(i32::MAX),
        time_signature_numerator: u16::from(t.numerator),
        time_signature_denominator: u16::from(t.denominator),
    }
}

impl ClapProcessor {
    fn push_events(&mut self, events: &[PluginEvent]) {
        self.events.clear();
        let port = self.note_port.map_or(0, |(p, _)| p);
        let midi_only = self.note_port.is_some_and(|(_, m)| m);
        for event in events.iter().take(MAX_PLUGIN_EVENTS) {
            let time = event.offset;
            match event.kind {
                PluginEventKind::NoteOn {
                    channel,
                    key,
                    velocity,
                } if !midi_only => self.events.push(&NoteOnEvent::new(
                    time,
                    Pckn::new(port, channel, key, Match::All),
                    f64::from(velocity),
                )),
                PluginEventKind::NoteOff { channel, key } if !midi_only => self.events.push(
                    &NoteOffEvent::new(time, Pckn::new(port, channel, key, Match::All), 0.0),
                ),
                PluginEventKind::NoteOn {
                    channel,
                    key,
                    velocity,
                } => self.events.push(&MidiEvent::new(
                    time,
                    port,
                    [
                        0x90 | channel,
                        key,
                        (velocity * 127.0).round().clamp(1.0, 127.0) as u8,
                    ],
                )),
                PluginEventKind::NoteOff { channel, key } => {
                    self.events
                        .push(&MidiEvent::new(time, port, [0x80 | channel, key, 0]))
                }
                PluginEventKind::Midi { data } => {
                    self.events.push(&MidiEvent::new(time, port, data))
                }
                // A reset sets every expression of the key back; there's
                // nothing to detach (expressions follow the key's notes).
                PluginEventKind::PerNote {
                    channel,
                    key,
                    controller: PerNote::Management { reset, .. },
                    ..
                } if !midi_only => {
                    if reset {
                        for (expression, value) in DEFAULT_EXPRESSIONS {
                            self.events.push(&NoteExpressionEvent::new(
                                time,
                                Pckn::new(port, channel, key, Match::All),
                                clap_expression(expression),
                                value,
                            ));
                        }
                    }
                }
                PluginEventKind::PerNote {
                    channel,
                    key,
                    controller,
                    value,
                } if !midi_only => {
                    if let Some((expression, value)) = note_expression(controller, value) {
                        self.events.push(&NoteExpressionEvent::new(
                            time,
                            Pckn::new(port, channel, key, Match::All),
                            expression,
                            value,
                        ));
                    }
                }
                // Plugins that only take MIDI 1.0 have no per-note controllers.
                PluginEventKind::PerNote { .. } => {}
                PluginEventKind::Param { index, value } => {
                    if let Some(&id) = self.param_ids.get(index as usize) {
                        self.events
                            .push(&ParamValueEvent::new(time, id, Pckn::match_all(), value));
                    }
                }
            }
        }
    }
}

impl PluginProcessor for ClapProcessor {
    fn process(
        &mut self,
        audio: [&mut [f32]; 2],
        context: ProcessContext,
        events: &[PluginEvent],
        mut midi_out: MidiSink,
    ) {
        let frames = audio[0].len().min(MAX_BLOCK);
        let [left, right] = audio;
        let ProcessContext {
            transport,
            sidechain,
            aux,
        } = context;
        ports::fill_inputs(
            &mut self.inputs,
            self.main_in,
            self.side_in,
            [&*left, &*right],
            sidechain,
            frames,
        );
        self.push_events(events);
        let Self {
            processor,
            input_ports,
            output_ports,
            inputs,
            outputs,
            events,
            out_events,
            steady,
            main_out,
            other_out,
            param_ids,
            changes,
            ..
        } = self;
        let Ok(started) = processor.ensure_processing_started() else {
            return;
        };
        let ins = input_ports.with_input_buffers(inputs.iter_mut().map(|port| {
            AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_input_only(
                    port.data
                        .as_chunks_mut::<MAX_BLOCK>()
                        .0
                        .iter_mut()
                        .map(|c| InputChannel::variable(&mut c[..frames])),
                ),
            }
        }));
        let mut outs = output_ports.with_output_buffers(outputs.iter_mut().map(|port| {
            AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_output_only(
                    port.data
                        .as_chunks_mut::<MAX_BLOCK>()
                        .0
                        .iter_mut()
                        .map(|c| &mut c[..frames]),
                ),
            }
        }));
        let input_events = events.as_input();
        out_events.clear();
        let transport = transport_event(&transport);
        let result = assert_no_alloc::permit_alloc(|| {
            started.process(
                &ins,
                &mut outs,
                &input_events,
                &mut out_events.as_output(),
                Some(*steady),
                Some(&transport),
            )
        });
        *steady += frames as u64;
        for event in out_events.iter() {
            let time = event.header().time();
            // Parameters it changed itself go to the control thread.
            let index = |id: Option<ClapId>| param_ids.iter().position(|&p| Some(p) == id);
            let change = match event.as_core_event() {
                Some(CoreEventSpace::ParamValue(e)) => {
                    index(e.param_id()).map(|index| ParamChange::Value {
                        index,
                        value: e.value(),
                    })
                }
                Some(CoreEventSpace::ParamGestureBegin(e)) => {
                    index(e.param_id()).map(|index| ParamChange::Begin { index })
                }
                Some(CoreEventSpace::ParamGestureEnd(e)) => {
                    index(e.param_id()).map(|index| ParamChange::End { index })
                }
                _ => None,
            };
            if let Some(change) = change {
                // A full ring drops the change (the control thread is busy).
                let _ = changes.push(change);
                continue;
            }
            let kind = match event.as_core_event() {
                Some(CoreEventSpace::NoteOn(e)) => {
                    let pckn = e.pckn();
                    let (Ok(channel), Ok(key)) = (
                        u8::try_from(pckn.raw_channel()),
                        u8::try_from(pckn.raw_key()),
                    ) else {
                        continue;
                    };
                    PluginEventKind::NoteOn {
                        channel: channel & 0x0f,
                        key: key & 0x7f,
                        velocity: e.velocity().clamp(0.0, 1.0) as f32,
                    }
                }
                Some(CoreEventSpace::NoteOff(e)) => {
                    let pckn = e.pckn();
                    let (Ok(channel), Ok(key)) = (
                        u8::try_from(pckn.raw_channel()),
                        u8::try_from(pckn.raw_key()),
                    ) else {
                        continue;
                    };
                    PluginEventKind::NoteOff {
                        channel: channel & 0x0f,
                        key: key & 0x7f,
                    }
                }
                Some(CoreEventSpace::Midi(e)) if (0x80..0xf0).contains(&e.data()[0]) => {
                    PluginEventKind::from_midi(e.data())
                }
                // Expressions on one key (not a note ID alone) go on as
                // per-note controllers.
                Some(CoreEventSpace::NoteExpression(e)) => {
                    let pckn = e.pckn();
                    let (Ok(channel), Ok(key), Some(expression)) = (
                        u8::try_from(pckn.raw_channel()),
                        u8::try_from(pckn.raw_key()),
                        expression_from_raw(e.as_raw().expression_id),
                    ) else {
                        continue;
                    };
                    expression::to_per_note(expression, channel & 0x0f, key & 0x7f, e.value())
                }
                _ => continue,
            };
            midi_out.push(time, kind);
        }
        if result.is_err() {
            // Can't log here (that allocates); the track goes silent.
            ports::silence([left, right], aux, frames);
            return;
        }
        ports::copy_outputs(outputs, *main_out, other_out, [left, right], aux, frames);
    }

    fn stop(&mut self) {
        if self.processor.is_started() {
            // Plugin code on the audio thread, which may allocate.
            assert_no_alloc::permit_alloc(|| {
                let _ = self.processor.stop_processing();
            });
        }
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}
