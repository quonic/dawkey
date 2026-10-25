// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dawkey's side of bridged plugins: the processes that run them, and the
//! processor that stands in for each instance on the audio thread.
//!
//! Instances of the same plugin file share a process (unless the program
//! says not to): fewer processes, less memory, faster loads, and fewer
//! real-time processes for rtkit to grant. A crash takes down the
//! instances in that process, all of one plugin. Each instance has shared
//! memory and an audio thread of its own there, so they still run in
//! parallel.
//!
//! The audio thread never waits for the plugin: each block it takes back
//! what the host process made of the previous one, and hands it this one.
//! The output comes out of a FIFO that starts with a block of silence, so
//! the plugin is late by exactly that much (reported as latency, which
//! delay compensation lines up), and so is the MIDI it sends. A block
//! longer than that grows the delay once. If the host process isn't done
//! in time, the block is silence (its events go with the next block it
//! gets); if it dies, the track's audio passes through. Offline renders
//! wait for each block instead, so they add no latency.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::{Duration, Instant};

use dawkey_engine::{
    MAX_BLOCK, MidiSink, PluginEvent, PluginEventKind, PluginProcessor, ProcessContext,
};

use super::proto::{Envelope, FromHost, ToHost, read_frame, write_frame};
use super::sandbox::{Enforced, Entered, Sandbox};
use super::shm::{MAX_AUX, MAX_EVENTS, Shm, WireEvent, WireTransport, futex_wait, send_fd, signal};
use crate::{ParamChange, PluginError, PluginInfo, SearchPath, StatePaths};

/// How long a plugin may take to load (and a process to enter its
/// sandbox): one that takes longer is taken to hang, and its process goes.
const LOAD_TIMEOUT: Duration = Duration::from_secs(60);
/// How long Dawkey waits for an answer to a question, to save a state (a
/// big one takes a while) and to show a GUI. One that takes longer is
/// busy, not hung: a GUI's dialog (a file chooser, a message) holds up the
/// process's main thread until it's answered, and the answer comes late.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
const SAVE_TIMEOUT: Duration = Duration::from_secs(20);
const GUI_TIMEOUT: Duration = Duration::from_secs(10);
/// How long unloading an instance, or a process exiting, may take.
const UNLOAD_TIMEOUT: Duration = Duration::from_secs(2);
/// How long an offline render waits for one block.
const BLOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// The program that hosts plugins in processes of their own.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BridgeProgram {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Instances of the same plugin file share a process; otherwise each
    /// has one of its own.
    pub share: bool,
    /// The sandbox its processes enter before loading plugins (`None`:
    /// none).
    pub sandbox: Option<Sandbox>,
}

/// What an instance and its processor share.
#[derive(Debug, Default)]
struct Status {
    /// The host process is gone (it crashed, or stopped answering).
    dead: AtomicBool,
    /// The plugin said its state changed.
    dirty: AtomicBool,
    /// The processor's delay, in frames.
    delay: AtomicU32,
    /// Parameters the plugin changed itself, not yet taken.
    changes: Mutex<Vec<ParamChange>>,
    /// Its GUI is showing.
    gui_visible: AtomicBool,
}

/// A reply from a host process (to the request it names), and its blob.
type Reply = (u64, FromHost, Vec<u8>);

/// Replies to an instance, and what notices about it set.
#[derive(Debug)]
struct Route {
    replies: mpsc::Sender<Reply>,
    status: Arc<Status>,
}

type Routes = Arc<Mutex<HashMap<u32, Route>>>;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Every instance in the process is gone with it.
fn all_dead(dead: &AtomicBool, routes: &Mutex<HashMap<u32, Route>>) {
    dead.store(true, Ordering::Release);
    // Dropping the senders ends any wait for a reply.
    for (_, route) in lock(routes).drain() {
        route.status.dead.store(true, Ordering::Release);
    }
}

/// A plugin host process, and the instances it hosts. It exits when the
/// last of them goes.
#[derive(Debug)]
pub(crate) struct Process {
    child: Mutex<Child>,
    pid: u32,
    /// Where messages go, a frame (and a descriptor) at a time.
    socket: Mutex<UnixStream>,
    routes: Routes,
    dead: Arc<AtomicBool>,
    next_instance: AtomicU32,
    /// What its sandbox is, as the kernel enforces it.
    sandbox: Option<Entered>,
}

impl Process {
    fn start(program: &BridgeProgram) -> Result<Self, PluginError> {
        let failed = |e: io::Error| PluginError::new(format!("starting its process failed: {e}"));
        let (ours, theirs) = UnixStream::pair().map_err(failed)?;
        let mut child = Command::new(&program.program)
            .args(&program.args)
            .stdin(Stdio::from(OwnedFd::from(theirs)))
            .stdout(Stdio::null())
            .spawn()
            .map_err(failed)?;
        let sandbox = match &program.sandbox {
            Some(sandbox) => match enter_sandbox(&ours, sandbox) {
                Ok(entered) => Some(entered),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(PluginError::new(format!(
                        "its sandbox: {e} (the preferences can run plugins without one)"
                    )));
                }
            },
            None => None,
        };
        let routes = Routes::default();
        let dead = Arc::new(AtomicBool::new(false));
        listen(
            ours.try_clone().map_err(failed)?,
            Arc::clone(&routes),
            Arc::clone(&dead),
        );
        Ok(Self {
            pid: child.id(),
            child: Mutex::new(child),
            socket: Mutex::new(ours),
            routes,
            dead,
            next_instance: AtomicU32::new(1),
            sandbox,
        })
    }

    fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Acquire) || self.exited()
    }

    /// A new instance: its number, where its replies come, and its status.
    fn register(&self) -> (u32, mpsc::Receiver<Reply>, Arc<Status>) {
        let id = self.next_instance.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Status::default());
        if self.dead.load(Ordering::Acquire) {
            status.dead.store(true, Ordering::Release);
        } else {
            let route = Route {
                replies: tx,
                status: Arc::clone(&status),
            };
            lock(&self.routes).insert(id, route);
        }
        (id, rx, status)
    }

    fn forget(&self, instance: u32) {
        lock(&self.routes).remove(&instance);
    }

    fn send(&self, instance: u32, request: u64, message: ToHost, blob: &[u8]) -> io::Result<()> {
        let envelope = Envelope {
            instance,
            request,
            message,
        };
        write_frame(&mut &*lock(&self.socket), &envelope, blob)
    }

    /// Sends `load` for `instance`, then its shared memory.
    fn send_load(
        &self,
        instance: u32,
        request: u64,
        load: ToHost,
        state: &[u8],
        shm: &Shm,
    ) -> io::Result<()> {
        let socket = lock(&self.socket);
        let envelope = Envelope {
            instance,
            request,
            message: load,
        };
        write_frame(&mut &*socket, &envelope, state)?;
        send_fd(&socket, shm.fd())
    }

    /// Whether it exited (then its instances are gone).
    fn exited(&self) -> bool {
        let exited = matches!(lock(&self.child).try_wait(), Ok(Some(_)));
        if exited && !self.dead.load(Ordering::Acquire) {
            all_dead(&self.dead, &self.routes);
        }
        exited
    }

    /// Hung, or of no more use: it goes, with every instance in it.
    fn kill(&self) {
        all_dead(&self.dead, &self.routes);
        let mut child = lock(&self.child);
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // The last instance went: it exits (or is killed).
        if self.exited() {
            return;
        }
        let _ = self.send(0, 0, ToHost::Quit, &[]);
        let until = Instant::now() + UNLOAD_TIMEOUT;
        while Instant::now() < until {
            if self.exited() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        tracing::warn!("plugin process {} didn't exit; killing it", self.pid);
        self.kill();
    }
}

/// Has a new process enter `sandbox`, before anything else.
fn enter_sandbox(socket: &UnixStream, sandbox: &Sandbox) -> Result<Entered, String> {
    let message = Envelope {
        instance: 0,
        request: 0,
        message: ToHost::Sandbox(sandbox.clone()),
    };
    write_frame(&mut &*socket, &message, &[]).map_err(|e| e.to_string())?;
    socket
        .set_read_timeout(Some(LOAD_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let reply = read_frame::<Envelope<FromHost>>(&mut &*socket);
    let _ = socket.set_read_timeout(None);
    match reply.map_err(|e| e.to_string())?.0.message {
        FromHost::Sandboxed(entered) => {
            if entered.landlock != Enforced::Fully {
                tracing::warn!("this kernel enforces some of the plugin sandbox: {entered:?}");
            }
            Ok(entered)
        }
        FromHost::Failed { error } => Err(error),
        other => Err(format!("its process said {other:?}")),
    }
}

/// Reads what a host process sends on a thread of its own: replies go to
/// their instance's channel, notices set its flags, and the end of the
/// stream (or garbage) means the process is gone.
fn listen(socket: UnixStream, routes: Routes, dead: Arc<AtomicBool>) {
    let spawned = std::thread::Builder::new()
        .name("dawkey-plugin-bridge".into())
        .spawn(move || {
            let mut socket = socket;
            loop {
                let Ok((
                    Envelope {
                        instance,
                        request,
                        message,
                    },
                    blob,
                )) = read_frame::<Envelope<FromHost>>(&mut socket)
                else {
                    all_dead(&dead, &routes);
                    break;
                };
                let routes = lock(&routes);
                // An instance that went already.
                let Some(route) = routes.get(&instance) else {
                    continue;
                };
                match message {
                    FromHost::StateDirty => route.status.dirty.store(true, Ordering::Relaxed),
                    FromHost::GuiClosed => route.status.gui_visible.store(false, Ordering::Relaxed),
                    FromHost::ParamsChanged(changes) => lock(&route.status.changes).extend(changes),
                    message => {
                        // A GUI that opened (or closed) late, after a dialog
                        // was answered, still counts.
                        if let FromHost::Gui { visible } = message {
                            route.status.gui_visible.store(visible, Ordering::Relaxed);
                        }
                        let _ = route.replies.send((request, message, blob));
                    }
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("listening to a plugin process: {e}");
    }
}

/// The plugin processes running, by the plugin file they host.
#[derive(Debug, Default)]
pub(crate) struct Processes {
    by_file: HashMap<(PathBuf, BridgeProgram), Weak<Process>>,
}

impl Processes {
    /// The process to load `info` in: the one its file's other instances
    /// run in, or a new one.
    fn for_plugin(
        &mut self,
        program: &BridgeProgram,
        info: &PluginInfo,
    ) -> Result<Arc<Process>, PluginError> {
        if !program.share {
            return Process::start(program).map(Arc::new);
        }
        self.by_file.retain(|_, process| process.strong_count() > 0);
        let key = (info.path.clone(), program.clone());
        if let Some(process) = self.by_file.get(&key).and_then(Weak::upgrade)
            && !process.is_dead()
        {
            return Ok(process);
        }
        let process = Arc::new(Process::start(program)?);
        self.by_file.insert(key, Arc::downgrade(&process));
        Ok(process)
    }
}

/// What a plugin whose process stopped says.
const CRASHED: &str = "its process stopped (the plugin crashed)";
/// What a plugin whose process is busy says.
const BUSY: &str = "it's busy: its GUI may be waiting for you to answer a dialog";

/// Why a reply didn't come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// Not in time.
    Late,
    /// The process is gone.
    Gone,
}

/// The control-thread side of a bridged plugin.
pub(crate) struct BridgedInstance {
    process: Arc<Process>,
    id: u32,
    replies: mpsc::Receiver<Reply>,
    status: Arc<Status>,
    /// The plugin's own latency.
    plugin_latency: u32,
    /// It has a GUI, and what to call its window.
    gui: bool,
    name: String,
    /// The process has no such instance (any more).
    gone: bool,
    /// The number of the next request, and of one not answered in time.
    next_request: u64,
    unanswered: Option<u64>,
}

impl std::fmt::Debug for BridgedInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgedInstance")
            .field("pid", &self.process.pid)
            .field("instance", &self.id)
            .finish_non_exhaustive()
    }
}

/// Loads `info` in a process `program` started (or one already running its
/// plugin file, see [`Processes`]). `block` is the engine's block size (the
/// delay to start with); `offline` processors wait for each block instead.
/// Returns the instance, the processor and its latency.
#[expect(clippy::too_many_arguments, reason = "everything a load needs")]
pub(crate) fn load(
    processes: &mut Processes,
    program: &BridgeProgram,
    info: &PluginInfo,
    sample_rate: u32,
    search: &SearchPath,
    state: Option<&[u8]>,
    paths: &StatePaths,
    block: u32,
    offline: bool,
) -> Result<(BridgedInstance, BridgedProcessor, u32), PluginError> {
    let failed = |e: io::Error| PluginError::new(format!("starting its process failed: {e}"));
    let process = processes.for_plugin(program, info)?;
    let shm = Arc::new(Shm::create().map_err(failed)?);
    let (id, replies, status) = process.register();
    let mut instance = BridgedInstance {
        process,
        id,
        replies,
        status: Arc::clone(&status),
        plugin_latency: 0,
        gui: false,
        name: info.name.clone(),
        gone: false,
        next_request: 1,
        unanswered: None,
    };
    let load = ToHost::Load {
        info: Box::new(info.clone()),
        sample_rate,
        search: search.clone(),
        has_state: state.is_some(),
        paths: paths.clone(),
        offline,
    };
    let request = instance.request();
    if let Err(e) = instance
        .process
        .send_load(id, request, load, state.unwrap_or(&[]), &shm)
    {
        instance.process.kill();
        return Err(PluginError::new(format!("talking to its process: {e}")));
    }
    let loaded = match instance.wait(request, LOAD_TIMEOUT) {
        Ok(loaded) => loaded,
        Err(Wait::Late) => {
            // Hung: as good as crashed, and so is what shares its process.
            instance.process.kill();
            return Err(PluginError::new("it stopped answering"));
        }
        Err(Wait::Gone) => return Err(PluginError::new(CRASHED)),
    };
    let latency = match loaded {
        (FromHost::Loaded { latency, gui }, _) => {
            instance.gui = gui;
            latency
        }
        (FromHost::Failed { error }, _) => {
            instance.gone = true;
            return Err(PluginError::new(error));
        }
        (other, _) => {
            instance.gone = true;
            return Err(PluginError::new(format!("its process said {other:?}")));
        }
    };
    instance.plugin_latency = latency;
    let delay = if offline {
        0
    } else {
        (block as usize).clamp(1, MAX_BLOCK)
    };
    status.delay.store(delay as u32, Ordering::Relaxed);
    let aux = info.outputs.len().min(MAX_AUX);
    let processor = BridgedProcessor::new(shm, status, delay, aux, offline);
    Ok((instance, processor, latency + delay as u32))
}

impl BridgedInstance {
    pub(crate) fn pid(&self) -> u32 {
        self.process.pid
    }

    /// Its process is in a sandbox the kernel enforces (some of).
    pub(crate) fn sandboxed(&self) -> bool {
        self.process
            .sandbox
            .as_ref()
            .is_some_and(|s| s.landlock != Enforced::Not || s.seccomp)
    }

    /// Why the plugin can't be used, if it can't.
    pub(crate) fn crashed(&self) -> Option<String> {
        self.status
            .dead
            .load(Ordering::Acquire)
            .then(|| format!("{CRASHED}; reload it to try again"))
    }

    /// The latency it adds now: its own, and the bridge's delay.
    pub(crate) fn latency(&self) -> u32 {
        self.plugin_latency + self.status.delay.load(Ordering::Relaxed)
    }

    /// A number for a new request.
    fn request(&mut self) -> u64 {
        let request = self.next_request;
        self.next_request += 1;
        request
    }

    /// Waits up to `timeout` for the reply to `request`, passing over late
    /// ones (and noting the one not answered in time, if it's there).
    fn wait(&mut self, request: u64, timeout: Duration) -> Result<(FromHost, Vec<u8>), Wait> {
        let until = Instant::now() + timeout;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.replies.recv_timeout(left) {
                Ok((answered, message, blob)) if answered == request => {
                    return Ok((message, blob));
                }
                Ok((answered, ..)) => {
                    if self.unanswered == Some(answered) {
                        self.unanswered = None;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return Err(Wait::Late),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(Wait::Gone),
            }
        }
    }

    /// Sends `message` and waits up to `timeout` for the reply. A process
    /// that's still busy with an earlier request (its GUI's dialog, most
    /// likely) isn't asked: the answer would only come late.
    fn ask(
        &mut self,
        message: ToHost,
        timeout: Duration,
    ) -> Result<(FromHost, Vec<u8>), PluginError> {
        if let Some(e) = self.crashed() {
            return Err(PluginError::new(e));
        }
        // Late replies that came meanwhile.
        while let Ok((answered, ..)) = self.replies.try_recv() {
            if self.unanswered == Some(answered) {
                self.unanswered = None;
            }
        }
        if self.unanswered.is_some() {
            return Err(PluginError::new(BUSY));
        }
        let request = self.request();
        self.process
            .send(self.id, request, message, &[])
            .map_err(|e| PluginError::new(format!("talking to its process: {e}")))?;
        match self.wait(request, timeout) {
            Ok(reply) => Ok(reply),
            Err(Wait::Late) => {
                self.unanswered = Some(request);
                Err(PluginError::new(BUSY))
            }
            Err(Wait::Gone) => Err(PluginError::new(CRASHED)),
        }
    }

    pub(crate) fn param_text(&mut self, index: usize, value: f64) -> Option<String> {
        match self.ask(ToHost::ParamText { index, value }, REPLY_TIMEOUT) {
            Ok((FromHost::Text(text), _)) => text,
            _ => None,
        }
    }

    pub(crate) fn param_value(&mut self, index: usize, text: &str) -> Option<f64> {
        let text = text.to_owned();
        match self.ask(ToHost::ParamValue { index, text }, REPLY_TIMEOUT) {
            Ok((FromHost::Value(value), _)) => value,
            _ => None,
        }
    }

    pub(crate) fn save_state(
        &mut self,
        paths: &StatePaths,
    ) -> Result<Option<Vec<u8>>, PluginError> {
        let paths = paths.clone();
        self.status.dirty.store(false, Ordering::Relaxed);
        match self.ask(ToHost::SaveState { paths }, SAVE_TIMEOUT)? {
            (FromHost::State { saves: true }, bytes) => Ok(Some(bytes)),
            (FromHost::State { saves: false }, _) => Ok(None),
            (FromHost::StateFailed { error }, _) => Err(PluginError::new(error)),
            (other, _) => Err(PluginError::new(format!("its process said {other:?}"))),
        }
    }

    pub(crate) fn state_dirty(&self) -> bool {
        self.status.dirty.load(Ordering::Relaxed)
    }

    pub(crate) fn has_gui(&self) -> bool {
        self.gui
    }

    pub(crate) fn gui_visible(&self) -> bool {
        self.status.gui_visible.load(Ordering::Relaxed)
    }

    pub(crate) fn show_gui(&mut self, show: bool) -> Result<(), PluginError> {
        if !self.gui {
            return Err(PluginError::new("it has no GUI Dawkey can show"));
        }
        let message = if show {
            ToHost::ShowGui {
                title: format!("{} — Dawkey", self.name),
            }
        } else {
            ToHost::HideGui
        };
        match self.ask(message, GUI_TIMEOUT)? {
            (FromHost::Gui { visible }, _) => {
                self.status.gui_visible.store(visible, Ordering::Relaxed);
                Ok(())
            }
            (FromHost::GuiFailed { error }, _) => Err(PluginError::new(error)),
            (other, _) => Err(PluginError::new(format!("its process said {other:?}"))),
        }
    }

    pub(crate) fn take_param_changes(&mut self) -> Vec<ParamChange> {
        std::mem::take(&mut *lock(&self.status.changes))
    }

    /// Notices a process that exited without a word.
    pub(crate) fn idle(&mut self) {
        if !self.status.dead.load(Ordering::Relaxed) {
            self.process.exited();
        }
    }

    /// Unloads the plugin. The processor (the engine is done with it) goes
    /// too. The process exits with its last instance (or is killed then,
    /// if it doesn't).
    pub(crate) fn unload(mut self) {
        if self.gone || self.status.dead.load(Ordering::Acquire) {
            return;
        }
        self.gone = true;
        let request = self.request();
        if self
            .process
            .send(self.id, request, ToHost::Unload, &[])
            .is_err()
        {
            return;
        }
        // Busy (a GUI's dialog): the process unloads it when it gets to it.
        if let Err(Wait::Late) = self.wait(request, UNLOAD_TIMEOUT) {
            tracing::warn!(
                "plugin process {} is busy; it unloads {} when it can",
                self.process.pid,
                self.name
            );
        }
    }
}

impl Drop for BridgedInstance {
    fn drop(&mut self) {
        // Dropped without unloading: the process unloads it anyway (and
        // exits with its last instance, see `Process`'s drop).
        if !self.gone && !self.status.dead.load(Ordering::Acquire) {
            let _ = self.process.send(self.id, 0, ToHost::Unload, &[]);
        }
        self.process.forget(self.id);
    }
}

/// Mono FIFO of up to two blocks and a delay's worth. Real-time safe.
#[derive(Debug)]
struct Fifo {
    data: Box<[f32]>,
    start: usize,
    len: usize,
}

impl Fifo {
    fn new() -> Self {
        Self {
            data: vec![0.0; 3 * MAX_BLOCK].into_boxed_slice(),
            start: 0,
            len: 0,
        }
    }

    fn push(&mut self, samples: &[f32]) {
        let cap = self.data.len();
        for &s in samples.iter().take(cap - self.len) {
            self.data[(self.start + self.len) % cap] = s;
            self.len += 1;
        }
    }

    fn push_silence(&mut self, frames: usize) {
        let cap = self.data.len();
        for _ in 0..frames.min(cap - self.len) {
            self.data[(self.start + self.len) % cap] = 0.0;
            self.len += 1;
        }
    }

    /// Fills `out`, with silence past what it has.
    fn pop(&mut self, out: &mut [f32]) {
        let cap = self.data.len();
        let n = out.len().min(self.len);
        for (i, o) in out.iter_mut().enumerate() {
            *o = if i < n {
                self.data[(self.start + i) % cap]
            } else {
                0.0
            };
        }
        self.start = (self.start + n) % cap;
        self.len -= n;
    }
}

/// Stands in for a bridged plugin on the audio thread. Real-time safe: it
/// copies to and from the shared memory, and wakes the host process.
pub(crate) struct BridgedProcessor {
    shm: Arc<Shm>,
    status: Arc<Status>,
    offline: bool,
    /// The block with the host process (its length, other outputs, and the
    /// frame the MIDI it sends is due from), if one is.
    pending: Option<(usize, usize, u64)>,
    /// Requests sent so far.
    sent: u32,
    delay: usize,
    /// Frames processed so far: the clock MIDI is due by.
    clock: u64,
    main: [Fifo; 2],
    aux: Box<[[Fifo; 2]]>,
    /// MIDI the plugin sent, and the frame each is due to pass on (as late
    /// as its audio, `delay` frames after the frame it was sent for).
    midi: VecDeque<(u64, PluginEventKind)>,
    /// What still matters of the events of blocks the host process missed
    /// (it was busy), to go with the next block it gets.
    held: Vec<PluginEvent>,
    /// Blocks the host process wasn't done with in time.
    late: u64,
}

impl std::fmt::Debug for BridgedProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgedProcessor")
            .field("delay", &self.delay)
            .field("late", &self.late)
            .finish_non_exhaustive()
    }
}

impl BridgedProcessor {
    fn new(shm: Arc<Shm>, status: Arc<Status>, delay: usize, aux: usize, offline: bool) -> Self {
        let mut processor = Self {
            shm,
            status,
            offline,
            pending: None,
            sent: 0,
            delay,
            clock: 0,
            main: [Fifo::new(), Fifo::new()],
            aux: (0..aux).map(|_| [Fifo::new(), Fifo::new()]).collect(),
            // Room for the MIDI of a few blocks in flight.
            midi: VecDeque::with_capacity(4 * MAX_EVENTS),
            held: Vec::with_capacity(MAX_EVENTS),
            late: 0,
        };
        for fifo in processor.fifos() {
            fifo.push_silence(delay);
        }
        processor
    }

    fn fifos(&mut self) -> impl Iterator<Item = &mut Fifo> {
        self.main.iter_mut().chain(self.aux.iter_mut().flatten())
    }

    /// Puts this block in the shared memory and wakes the host process.
    fn send(&mut self, audio: &[&mut [f32]; 2], context: &ProcessContext, events: &[PluginEvent]) {
        let frames = audio[0].len();
        // SAFETY: no block is with the host process (`pending` is none, or
        // it's done): the memory is ours.
        let block = unsafe { self.shm.block() };
        block.frames = frames as u32;
        for (to, from) in block.audio.iter_mut().zip(audio) {
            to[..frames].copy_from_slice(from);
        }
        block.has_sidechain = u32::from(context.sidechain.is_some());
        if let Some(side) = context.sidechain {
            for (to, from) in block.sidechain.iter_mut().zip(side) {
                to[..frames].copy_from_slice(&from[..frames]);
            }
        }
        let aux = context.aux.len().min(MAX_AUX);
        block.aux_count = aux as u32;
        block.transport = WireTransport::from_transport(&context.transport);
        // What it missed first (at its start), then this block's events.
        let n = (self.held.len() + events.len()).min(MAX_EVENTS);
        for (wire, event) in block.events.iter_mut().zip(self.held.iter().chain(events)) {
            *wire = WireEvent::from_event(event);
        }
        block.events_in = n as u32;
        block.events_out = 0;
        self.held.clear();
        self.sent = self.sent.wrapping_add(1);
        signal(self.shm.request(), self.sent);
        self.pending = Some((frames, aux, self.clock + self.delay as u64));
    }

    /// Keeps what matters of the events of a block the host process misses,
    /// for the next block it gets: each parameter's last value, note-offs
    /// and other MIDI. Note-ons are dropped (too late to play).
    fn hold(&mut self, events: &[PluginEvent]) {
        for event in events {
            let kind = event.kind;
            match kind {
                PluginEventKind::NoteOn { .. } => continue,
                PluginEventKind::Param { index, .. } => {
                    let earlier = self.held.iter_mut().find(
                        |e| matches!(e.kind, PluginEventKind::Param { index: i, .. } if i == index),
                    );
                    if let Some(earlier) = earlier {
                        earlier.kind = kind;
                        continue;
                    }
                }
                _ => {}
            }
            if self.held.len() < self.held.capacity() {
                self.held.push(PluginEvent { offset: 0, kind });
            }
        }
    }

    /// Whether the host process is done with the block sent.
    fn done(&self) -> bool {
        self.shm.done().load(Ordering::Acquire) == self.sent
    }

    /// Takes the block the host process did: its audio into the FIFOs, its
    /// MIDI to pass on from frame `due`.
    fn collect(&mut self, frames: usize, aux: usize, due: u64) {
        // SAFETY: the host process is done with it: the memory is ours.
        let block = unsafe { self.shm.block() };
        for (fifo, data) in self.main.iter_mut().zip(&block.audio) {
            fifo.push(&data[..frames]);
        }
        for (i, fifos) in self.aux.iter_mut().enumerate() {
            for (fifo, data) in fifos.iter_mut().zip(&block.aux[i.min(MAX_AUX - 1)]) {
                if i < aux {
                    fifo.push(&data[..frames]);
                } else {
                    fifo.push_silence(frames);
                }
            }
        }
        let n = (block.events_out as usize).min(MAX_EVENTS);
        for event in block.midi_out[..n].iter().filter_map(|e| e.to_event()) {
            if self.midi.len() < self.midi.capacity() {
                self.midi
                    .push_back((due + u64::from(event.offset), event.kind));
            }
        }
        self.pending = None;
    }

    /// Offline: the block, processed while we wait.
    fn process_waiting(
        &mut self,
        audio: [&mut [f32]; 2],
        context: ProcessContext,
        events: &[PluginEvent],
        mut midi_out: MidiSink,
    ) {
        self.send(&audio, &context, events);
        let until = Instant::now() + BLOCK_TIMEOUT;
        loop {
            let done = self.shm.done().load(Ordering::Acquire);
            if done == self.sent {
                break;
            }
            if Instant::now() > until || self.status.dead.load(Ordering::Acquire) {
                self.status.dead.store(true, Ordering::Release);
                self.pending = None;
                return;
            }
            futex_wait(self.shm.done(), done, Duration::from_millis(50));
        }
        let frames = audio[0].len();
        // SAFETY: done: the memory is ours.
        let block = unsafe { self.shm.block() };
        let [left, right] = audio;
        left.copy_from_slice(&block.audio[0][..frames]);
        right.copy_from_slice(&block.audio[1][..frames]);
        for (out, data) in context.aux.iter_mut().zip(&block.aux) {
            for (channel, data) in out.channels.iter_mut().zip(data) {
                channel[..frames].copy_from_slice(&data[..frames]);
            }
        }
        let n = (block.events_out as usize).min(MAX_EVENTS);
        for event in block.midi_out[..n].iter().filter_map(|e| e.to_event()) {
            midi_out.push(event.offset, event.kind);
        }
        self.pending = None;
    }
}

impl PluginProcessor for BridgedProcessor {
    fn process(
        &mut self,
        audio: [&mut [f32]; 2],
        context: ProcessContext,
        events: &[PluginEvent],
        mut midi_out: MidiSink,
    ) {
        let frames = audio[0].len();
        if self.status.dead.load(Ordering::Acquire) {
            // The track's audio passes; nothing comes out of the others.
            for out in context.aux.iter_mut() {
                out.channels.iter_mut().for_each(|c| c[..frames].fill(0.0));
            }
            return;
        }
        if self.offline {
            return self.process_waiting(audio, context, events, midi_out);
        }
        // The block it did since the last call, if it's done.
        if let Some((done_frames, aux, due)) = self.pending {
            if self.done() {
                self.collect(done_frames, aux, due);
            } else {
                self.late += 1;
            }
        }
        // A longer block than the delay: the delay grows (once).
        if frames > self.delay {
            let more = frames - self.delay;
            for fifo in self.fifos() {
                fifo.push_silence(more);
            }
            self.delay = frames;
            self.status.delay.store(frames as u32, Ordering::Relaxed);
        }
        // This block goes to it, unless it's still busy (then the plugin
        // misses it, but not what its events change).
        if self.pending.is_none() {
            self.send(&audio, &context, events);
        } else {
            self.hold(events);
        }
        let [left, right] = audio;
        self.main[0].pop(left);
        self.main[1].pop(right);
        for (out, fifos) in context.aux.iter_mut().zip(self.aux.iter_mut()) {
            for (channel, fifo) in out.channels.iter_mut().zip(fifos.iter_mut()) {
                fifo.pop(&mut channel[..frames]);
            }
        }
        // The MIDI due in this block (or before: from a late block).
        let end = self.clock + frames as u64;
        while let Some(&(due, kind)) = self.midi.front()
            && due < end
        {
            self.midi.pop_front();
            midi_out.push(due.saturating_sub(self.clock) as u32, kind);
        }
        self.clock = end;
    }

    fn stop(&mut self) {}

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any + Send> {
        self
    }
}
