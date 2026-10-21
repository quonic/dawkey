// SPDX-License-Identifier: AGPL-3.0-or-later
//! The plugin host process (`dawkey plugin-host`): loads plugins the way
//! Dawkey would in-process, then runs them, as many instances as Dawkey
//! asks for (of one plugin file, unless every instance has a process of its
//! own). Each instance's audio thread waits for blocks in its shared
//! memory; the main thread answers Dawkey on the socket (its stdin), does
//! the plugins' main-thread work, and runs their GUIs: windows of its own
//! (X11, through XWayland) that GUIs are embedded in, or the plugins' own
//! floating windows, with the timers and descriptors CLAP GUIs drive their
//! event loops with. It exits when Dawkey says so or goes away.

use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dk_engine::{AuxBuffer, MidiSink, PluginEventKind, PluginProcessor, ProcessContext};

use super::proto::{Envelope, FromHost, ToHost, read_frame, write_frame};
use super::realtime;
use super::shm::{MAX_AUX, MAX_EVENTS, Shm, futex_wait, recv_fd, signal};
use super::x11::{WindowEvent, X11};
use crate::{GuiKind, Host, Instance, PluginError, PluginInfo, SearchPath, StatePaths};

/// How often the main thread does the plugins' housekeeping (and their
/// GUIs', while one shows: about 60 times a second).
const IDLE_EVERY: Duration = Duration::from_millis(20);
const GUI_IDLE_EVERY: Duration = Duration::from_millis(16);

/// The parameters' current values, as the audio thread last sent them to
/// the plugin (for its GUI).
type Values = Arc<[AtomicU64]>;

/// A plugin's GUI, showing.
struct Gui {
    /// The window it's embedded in (embedded kinds).
    window: Option<u32>,
    resizable: bool,
    /// It said what size it is (or it isn't embedded). GUIs that don't
    /// are as big as the window they make in ours.
    sized: bool,
}

/// One instance the process hosts.
struct Slot {
    info: PluginInfo,
    instance: Instance,
    values: Values,
    gui_kind: Option<GuiKind>,
    gui: Option<Gui>,
    told_dirty: bool,
    shm: Arc<Shm>,
    audio: JoinHandle<Box<dyn PluginProcessor>>,
}

impl Slot {
    fn close_gui(&mut self, x11: Option<&X11>) {
        let Some(gui) = self.gui.take() else {
            return;
        };
        self.instance.gui_close();
        if let (Some(window), Some(x11)) = (gui.window, x11) {
            x11.destroy(window);
        }
    }

    /// Stops its audio thread and unloads the plugin (its GUI first).
    fn unload(mut self, x11: Option<&X11>) {
        self.close_gui(x11);
        let shm = &self.shm;
        shm.quit().store(1, Ordering::Release);
        signal(
            shm.request(),
            shm.request().load(Ordering::Acquire).wrapping_add(1),
        );
        match self.audio.join() {
            Ok(processor) => self.instance.unload(processor.into_any()),
            Err(_) => tracing::warn!("{}'s audio thread panicked", self.info.name),
        }
    }
}

/// Runs the plugin host process on this process's stdin (a socket from
/// Dawkey). Returns once every plugin is unloaded.
pub fn run_plugin_host() -> io::Result<()> {
    // SAFETY: Dawkey made stdin our end of a socket; nothing else in this
    // process uses descriptor 0.
    let socket = UnixStream::from(unsafe { OwnedFd::from_raw_fd(0) });
    let mut main = Main {
        socket,
        hosts: Vec::new(),
        slots: BTreeMap::new(),
        x11: None,
    };
    let result = main.run();
    for (_, slot) in std::mem::take(&mut main.slots) {
        slot.unload(main.x11.as_ref());
    }
    result
}

/// The host process's main thread.
struct Main {
    socket: UnixStream,
    /// Hosts plugins load with, by sample rate and search path (one LV2
    /// world for every instance).
    hosts: Vec<Host>,
    slots: BTreeMap<u32, Slot>,
    x11: Option<X11>,
}

impl Main {
    /// Until Dawkey says to stop (or goes away).
    fn run(&mut self) -> io::Result<()> {
        let mut next_timer: Option<Instant> = None;
        loop {
            let idle = if self.slots.values().any(|s| s.gui.is_some()) {
                GUI_IDLE_EVERY
            } else {
                IDLE_EVERY
            };
            let timeout = next_timer.map_or(idle, |due| {
                due.saturating_duration_since(Instant::now()).min(idle)
            });
            // The socket, the X server, and what the plugins watch.
            let mut fds = vec![(self.socket.as_raw_fd(), libc::POLLIN)];
            if let Some(x11) = &self.x11 {
                fds.push((x11.fd(), libc::POLLIN));
            }
            let mut owners = Vec::new();
            for (&id, slot) in &self.slots {
                for (fd, events) in slot.instance.poll_fds() {
                    fds.push((fd, events));
                    owners.push((fd, id));
                }
            }
            for (fd, revents) in poll(&fds, timeout)? {
                if fd == self.socket.as_raw_fd() {
                    if !self.message()? {
                        return Ok(());
                    }
                } else if self.x11.as_ref().is_none_or(|x| x.fd() != fd) {
                    for &(_, id) in owners.iter().filter(|(owned, _)| *owned == fd) {
                        if let Some(slot) = self.slots.get_mut(&id) {
                            slot.instance.on_fd(fd, revents);
                        }
                    }
                }
            }
            self.window_events()?;
            let now = Instant::now();
            next_timer = None;
            for slot in self.slots.values_mut() {
                if let Some(due) = slot.instance.run_timers(now) {
                    next_timer = Some(next_timer.map_or(due, |next| next.min(due)));
                }
                slot.instance.idle();
            }
            let ids: Vec<u32> = self.slots.keys().copied().collect();
            for id in ids {
                self.gui_idle(id)?;
                let Some(slot) = self.slots.get_mut(&id) else {
                    continue;
                };
                let changes = slot.instance.take_param_changes();
                let dirty = slot.instance.state_dirty() && !slot.told_dirty;
                slot.told_dirty |= dirty;
                if !changes.is_empty() {
                    self.send(id, FromHost::ParamsChanged(changes), &[])?;
                }
                if dirty {
                    self.send(id, FromHost::StateDirty, &[])?;
                }
            }
        }
    }

    /// Sends a notice about `instance`.
    fn send(&self, instance: u32, message: FromHost, blob: &[u8]) -> io::Result<()> {
        self.reply(instance, 0, message, blob)
    }

    /// Answers `request` about `instance`.
    fn reply(&self, instance: u32, request: u64, message: FromHost, blob: &[u8]) -> io::Result<()> {
        let envelope = Envelope {
            instance,
            request,
            message,
        };
        write_frame(&mut &self.socket, &envelope, blob)
    }

    /// Answers Dawkey's next message. False once it says to stop (or went
    /// away).
    fn message(&mut self) -> io::Result<bool> {
        let Ok((
            Envelope {
                instance,
                request,
                message,
            },
            blob,
        )) = read_frame::<Envelope<ToHost>>(&mut &self.socket)
        else {
            return Ok(false);
        };
        let (reply, blob) = match message {
            ToHost::Quit => return Ok(false),
            ToHost::Sandbox(sandbox) => {
                let entered = if self.slots.is_empty() && self.hosts.is_empty() {
                    super::sandbox::enter(&sandbox)
                } else {
                    Err("plugins are loaded already".to_owned())
                };
                let reply = match entered {
                    Ok(entered) => FromHost::Sandboxed(entered),
                    Err(error) => FromHost::Failed { error },
                };
                (reply, Vec::new())
            }
            ToHost::Load {
                info,
                sample_rate,
                search,
                has_state,
                paths,
                offline,
            } => {
                // Its shared memory follows.
                let shm = recv_fd(&self.socket)?;
                let state = has_state.then_some(&blob[..]);
                let reply = self.load(
                    instance,
                    *info,
                    sample_rate,
                    search,
                    state,
                    &paths,
                    offline,
                    shm,
                );
                (reply, Vec::new())
            }
            ToHost::Unload => {
                if let Some(slot) = self.slots.remove(&instance) {
                    slot.unload(self.x11.as_ref());
                }
                (FromHost::Unloaded, Vec::new())
            }
            message => self.answer(instance, message),
        };
        self.reply(instance, request, reply, &blob)?;
        Ok(true)
    }

    /// Loads `info` as instance `id`, its audio thread waiting for blocks
    /// in `shm`.
    #[expect(clippy::too_many_arguments, reason = "everything a load needs")]
    fn load(
        &mut self,
        id: u32,
        info: PluginInfo,
        sample_rate: u32,
        search: SearchPath,
        state: Option<&[u8]>,
        paths: &StatePaths,
        offline: bool,
        shm: OwnedFd,
    ) -> FromHost {
        let shm = match Shm::open(shm) {
            Ok(shm) => Arc::new(shm),
            Err(e) => {
                return FromHost::Failed {
                    error: format!("opening its shared memory: {e}"),
                };
            }
        };
        let host = match self
            .hosts
            .iter()
            .position(|h| h.sample_rate() == sample_rate && *h.search() == search)
        {
            Some(i) => &mut self.hosts[i],
            None => {
                self.hosts.push(Host::new(sample_rate, search));
                self.hosts.last_mut().expect("just pushed")
            }
        };
        let loaded = match host.load_for(&info, state, paths, offline) {
            Ok(loaded) => loaded,
            Err(PluginError(error)) => return FromHost::Failed { error },
        };
        let mut instance = loaded.instance;
        let values: Values = info
            .params
            .iter()
            .map(|p| AtomicU64::new(p.default.to_bits()))
            .collect();
        let audio = {
            let (shm, values) = (Arc::clone(&shm), Arc::clone(&values));
            let processor = loaded.processor;
            std::thread::Builder::new()
                .name("dawkey-plugin-audio".into())
                .spawn(move || audio_thread(&shm, &values, processor))
        };
        let audio = match audio {
            Ok(audio) => audio,
            Err(e) => {
                return FromHost::Failed {
                    error: format!("starting its audio thread: {e}"),
                };
            }
        };
        let gui_kind = instance.gui_kind();
        self.slots.insert(
            id,
            Slot {
                info,
                instance,
                values,
                gui_kind,
                gui: None,
                told_dirty: false,
                shm,
                audio,
            },
        );
        FromHost::Loaded {
            latency: loaded.latency,
            gui: gui_kind.is_some(),
        }
    }

    /// The reply to a message about instance `id`, and its blob.
    fn answer(&mut self, id: u32, message: ToHost) -> (FromHost, Vec<u8>) {
        let Some(slot) = self.slots.get_mut(&id) else {
            let error = format!("there's no instance {id} here");
            return (FromHost::Failed { error }, Vec::new());
        };
        let info = &slot.info;
        match message {
            ToHost::ParamText { index, value } => (
                FromHost::Text(slot.instance.param_text(info, index, value)),
                Vec::new(),
            ),
            ToHost::ParamValue { index, text } => (
                FromHost::Value(slot.instance.param_value(info, index, &text)),
                Vec::new(),
            ),
            ToHost::SaveState { paths } => {
                slot.told_dirty = false;
                match slot.instance.save_state(&paths) {
                    Ok(Some(bytes)) => (FromHost::State { saves: true }, bytes),
                    Ok(None) => (FromHost::State { saves: false }, Vec::new()),
                    Err(PluginError(error)) => (FromHost::StateFailed { error }, Vec::new()),
                }
            }
            ToHost::ShowGui { title } => match self.open_gui(id, &title) {
                Ok(()) => (FromHost::Gui { visible: true }, Vec::new()),
                Err(error) => (FromHost::GuiFailed { error }, Vec::new()),
            },
            ToHost::HideGui => {
                slot.close_gui(self.x11.as_ref());
                (FromHost::Gui { visible: false }, Vec::new())
            }
            ToHost::Load { .. } | ToHost::Unload | ToHost::Quit | ToHost::Sandbox(_) => {
                unreachable!("handled by `message`")
            }
        }
    }

    fn open_gui(&mut self, id: u32, title: &str) -> Result<(), String> {
        let slot = self.slots.get_mut(&id).ok_or("no such instance")?;
        if slot.gui.is_some() {
            return Ok(());
        }
        let kind = slot
            .gui_kind
            .ok_or_else(|| "the plugin has no GUI Dawkey can show".to_owned())?;
        if !kind.embedded() {
            slot.instance.gui_open(kind, None, title).map_err(|e| e.0)?;
            slot.gui = Some(Gui {
                window: None,
                resizable: false,
                sized: true,
            });
            return Ok(());
        }
        if self.x11.is_none() {
            self.x11 = Some(X11::connect()?);
        }
        let x11 = self.x11.as_ref().expect("connected above");
        let resizable = slot.instance.gui_can_resize();
        // A placeholder size until the GUI says.
        let window = x11.create_window(title, 400, 300, true)?;
        match slot.instance.gui_open(kind, Some(window), title) {
            Ok(size) => {
                let (width, height) = size.unwrap_or((400, 300));
                x11.resize(window, width, height, resizable)?;
                slot.gui = Some(Gui {
                    window: Some(window),
                    resizable,
                    sized: size.is_some(),
                });
                Ok(())
            }
            Err(e) => {
                x11.destroy(window);
                Err(e.0)
            }
        }
    }

    /// Windows that were closed or resized.
    fn window_events(&mut self) -> io::Result<()> {
        let Some(x11) = &self.x11 else {
            return Ok(());
        };
        let events = x11.events().map_err(io::Error::other)?;
        for (window, event) in events {
            let Some((&id, slot)) = self
                .slots
                .iter_mut()
                .find(|(_, s)| s.gui.as_ref().is_some_and(|g| g.window == Some(window)))
            else {
                continue;
            };
            match event {
                WindowEvent::Closed => {
                    slot.close_gui(self.x11.as_ref());
                    self.send(id, FromHost::GuiClosed, &[])?;
                }
                WindowEvent::Resized(width, height) => {
                    let resizable = slot.gui.as_ref().is_some_and(|g| g.resizable);
                    if resizable
                        && let Some((w, h)) = slot.instance.gui_resized(width, height)
                        && (w, h) != (width, height)
                        && let Some(x11) = &self.x11
                    {
                        let _ = x11.resize(window, w, h, true);
                    }
                }
            }
        }
        Ok(())
    }

    /// A GUI's own work: the size it asks for, closing itself.
    fn gui_idle(&mut self, id: u32) -> io::Result<()> {
        let Some(slot) = self.slots.get_mut(&id) else {
            return Ok(());
        };
        let Some(gui) = &mut slot.gui else {
            return Ok(());
        };
        // A GUI that didn't say its size: once it has made its window (in
        // ours), ours takes its size.
        if !gui.sized
            && let (Some(window), Some(x11)) = (gui.window, &self.x11)
            && let Some((width, height)) = x11.child_size(window)
        {
            gui.sized = true;
            let _ = x11.resize(window, width, height, gui.resizable);
        }
        let (window, resizable) = (gui.window, gui.resizable);
        let values: Vec<f64> = slot
            .values
            .iter()
            .map(|v| f64::from_bits(v.load(Ordering::Relaxed)))
            .collect();
        let (resize, closed) = slot.instance.gui_idle(&values);
        if closed {
            slot.close_gui(self.x11.as_ref());
            return self.send(id, FromHost::GuiClosed, &[]);
        }
        if let (Some((width, height)), Some(window), Some(x11)) = (resize, window, &self.x11) {
            let _ = x11.resize(window, width, height, resizable);
        }
        Ok(())
    }
}

/// `poll`s `fds` (descriptor and events) for up to `timeout`; returns the
/// ready ones with what happened.
fn poll(fds: &[(RawFd, i16)], timeout: Duration) -> io::Result<Vec<(RawFd, i16)>> {
    let mut pollfds: Vec<libc::pollfd> = fds
        .iter()
        .map(|&(fd, events)| libc::pollfd {
            fd,
            events,
            revents: 0,
        })
        .collect();
    // SAFETY: valid pollfds, as many as said.
    let n = unsafe {
        libc::poll(
            pollfds.as_mut_ptr(),
            pollfds.len() as libc::nfds_t,
            timeout.as_millis() as i32,
        )
    };
    if n < 0 {
        let e = io::Error::last_os_error();
        return if e.kind() == io::ErrorKind::Interrupted {
            Ok(Vec::new())
        } else {
            Err(e)
        };
    }
    Ok(pollfds
        .iter()
        .filter(|p| p.revents != 0)
        .map(|p| (p.fd, p.revents))
        .collect())
}

/// Asks for real-time scheduling, one priority below Dawkey's audio thread
/// (see [`realtime`](super::realtime)); carries on without it if Dawkey's
/// audio isn't real-time or the system says no.
fn try_realtime() {
    let dawkey = std::os::unix::process::parent_id();
    let Some(priority) = realtime::priority_below(dawkey) else {
        tracing::debug!("Dawkey's audio isn't real-time, so the plugin's isn't either");
        return;
    };
    realtime::make_realtime(priority);
}

/// Processes each block Dawkey asks for until it says to stop; returns the
/// processor. Keeps `values` up to date with the parameters sent.
fn audio_thread(
    shm: &Shm,
    values: &Values,
    mut processor: Box<dyn PluginProcessor>,
) -> Box<dyn PluginProcessor> {
    try_realtime();
    let mut events = Vec::with_capacity(MAX_EVENTS);
    let mut midi = Vec::with_capacity(MAX_EVENTS);
    let mut aux: Vec<AuxBuffer> = (0..MAX_AUX).map(|_| AuxBuffer::new()).collect();
    // Requests count from 1 in fresh memory; one may already be waiting.
    let mut last = 0;
    loop {
        if shm.quit().load(Ordering::Acquire) != 0 {
            break;
        }
        let request = shm.request().load(Ordering::Acquire);
        if request == last {
            futex_wait(shm.request(), last, Duration::from_millis(200));
            continue;
        }
        last = request;
        // SAFETY: Dawkey asked for this block and waits for `done`: it's
        // ours until then.
        let block = unsafe { shm.block() };
        let frames = (block.frames as usize).min(dk_engine::MAX_BLOCK);
        let aux_count = (block.aux_count as usize).min(MAX_AUX);
        events.clear();
        events.extend(
            block.events[..(block.events_in as usize).min(MAX_EVENTS)]
                .iter()
                .filter_map(|e| e.to_event()),
        );
        for event in &events {
            if let PluginEventKind::Param { index, value } = event.kind
                && let Some(slot) = values.get(index as usize)
            {
                slot.store(value.to_bits(), Ordering::Relaxed);
            }
        }
        midi.clear();
        let [left, right] = &mut block.audio;
        let side = &block.sidechain;
        let context = ProcessContext {
            transport: block.transport.to_transport(),
            sidechain: (block.has_sidechain != 0).then(|| [&side[0][..frames], &side[1][..frames]]),
            aux: &mut aux[..aux_count],
        };
        processor.process(
            [&mut left[..frames], &mut right[..frames]],
            context,
            &events,
            MidiSink::new(&mut midi),
        );
        for (out, buffer) in block.aux.iter_mut().zip(&aux[..aux_count]) {
            for (channel, data) in out.iter_mut().zip(&buffer.channels) {
                channel[..frames].copy_from_slice(&data[..frames]);
            }
        }
        for (wire, event) in block.midi_out.iter_mut().zip(&midi) {
            *wire = super::shm::WireEvent::from_event(event);
        }
        block.events_out = midi.len().min(MAX_EVENTS) as u32;
        signal(shm.done(), request);
    }
    processor.stop();
    processor
}
