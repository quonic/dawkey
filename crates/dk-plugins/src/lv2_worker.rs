// SPDX-License-Identifier: AGPL-3.0-or-later
//! The LV2 worker (`work:schedule`, `work:interface`): work a plugin asks
//! for from its audio thread (loading a sample, say) done on a thread of
//! its own, the results handed back on the audio thread after `run`. For
//! offline renders and the virtual clock the work is done right away
//! instead, inside the plugin's call, so what they render doesn't depend
//! on how fast the worker was.

use std::cell::UnsafeCell;
use std::ffi::{CStr, c_void};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{JoinHandle, Thread};
use std::time::Duration;

use lv2_raw::core::LV2Feature;

use crate::lv2_ring::{MessageReader, MessageWriter, gather, ring};

pub(crate) const SCHEDULE: &CStr = c"http://lv2plug.in/ns/ext/worker#schedule";
pub(crate) const INTERFACE: &CStr = c"http://lv2plug.in/ns/ext/worker#interface";

/// Bytes of requests (or responses) waiting at once.
const RING: usize = 256 * 1024;

const SUCCESS: u32 = 0;
const ERR_NO_SPACE: u32 = 2;

type RespondFn = unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> u32;

/// `LV2_Worker_Interface`.
#[repr(C)]
pub(crate) struct WorkerInterface {
    work: Option<
        unsafe extern "C" fn(*mut c_void, RespondFn, *mut c_void, u32, *const c_void) -> u32,
    >,
    work_response: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> u32>,
    end_run: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
}

/// `LV2_Worker_Schedule`.
#[repr(C)]
struct Schedule {
    handle: *mut c_void,
    schedule_work: unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> u32,
}

/// What `schedule_work` needs, behind the feature's handle.
struct Shared {
    /// The plugin and its interface, once it's made.
    plugin: AtomicPtr<c_void>,
    interface: AtomicPtr<WorkerInterface>,
    mode: Mode,
    quit: AtomicBool,
}

enum Mode {
    /// Requests go to the worker thread.
    Threaded {
        requests: UnsafeCell<MessageWriter>,
        thread: OnceLock<Thread>,
    },
    /// The work is done in `schedule_work`; its responses wait here.
    Now {
        responses: UnsafeCell<MessageWriter>,
    },
}

// The writers are only used by the thread the plugin calls
// `schedule_work` on, which LV2 makes one at a time (`run`, `work_response`
// and `restore` never overlap); the rest is atomics.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

unsafe extern "C" fn schedule_work(handle: *mut c_void, size: u32, data: *const c_void) -> u32 {
    // SAFETY: `handle` is the `Shared` the feature was made with, which
    // outlives the plugin; `data` holds `size` bytes.
    unsafe {
        let shared = &*handle.cast::<Shared>();
        let bytes = if size == 0 || data.is_null() {
            &[][..]
        } else {
            std::slice::from_raw_parts(data.cast::<u8>(), size as usize)
        };
        match &shared.mode {
            Mode::Threaded { requests, thread } => {
                if !(*requests.get()).write(0, &[bytes]) {
                    return ERR_NO_SPACE;
                }
                if let Some(thread) = thread.get() {
                    thread.unpark();
                }
                SUCCESS
            }
            Mode::Now { responses } => {
                let (plugin, interface) = (
                    shared.plugin.load(Ordering::Acquire),
                    shared.interface.load(Ordering::Acquire),
                );
                match interface.as_ref().and_then(|i| i.work) {
                    Some(work) if !plugin.is_null() => work(
                        plugin,
                        respond,
                        responses.get().cast(),
                        size,
                        bytes.as_ptr().cast(),
                    ),
                    _ => 1,
                }
            }
        }
    }
}

unsafe extern "C" fn respond(handle: *mut c_void, size: u32, data: *const c_void) -> u32 {
    // SAFETY: `handle` is the `MessageWriter` for responses, used only by
    // whoever is calling `work`; `data` holds `size` bytes.
    unsafe {
        let responses = &mut *handle.cast::<MessageWriter>();
        let bytes = if size == 0 || data.is_null() {
            &[][..]
        } else {
            std::slice::from_raw_parts(data.cast::<u8>(), size as usize)
        };
        if responses.write(0, &[bytes]) {
            SUCCESS
        } else {
            ERR_NO_SPACE
        }
    }
}

/// A plugin's worker. Made before the plugin (its [`feature`](Self::feature)
/// goes to `instantiate`), [`start`](Self::start)ed once it exists, and
/// dropped only after the plugin is gone.
pub(crate) struct Worker {
    shared: Arc<Shared>,
    schedule: Box<Schedule>,
    /// Responses for the audio thread.
    responses: MessageReader,
    /// Where a response is copied to be handed over whole (preallocated).
    scratch: Vec<u64>,
    /// The worker thread's ends, until it starts.
    pending: Option<(MessageReader, MessageWriter)>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker").finish_non_exhaustive()
    }
}

impl Worker {
    /// A worker with a thread of its own, or (`now`) one that works inside
    /// `schedule_work`.
    pub(crate) fn new(now: bool) -> Self {
        let (responses_in, responses) = ring(RING);
        let (mode, pending) = if now {
            (
                Mode::Now {
                    responses: UnsafeCell::new(responses_in),
                },
                None,
            )
        } else {
            let (requests, requests_out) = ring(RING);
            (
                Mode::Threaded {
                    requests: UnsafeCell::new(requests),
                    thread: OnceLock::new(),
                },
                Some((requests_out, responses_in)),
            )
        };
        let shared = Arc::new(Shared {
            plugin: AtomicPtr::new(std::ptr::null_mut()),
            interface: AtomicPtr::new(std::ptr::null_mut()),
            mode,
            quit: AtomicBool::new(false),
        });
        let schedule = Box::new(Schedule {
            handle: Arc::as_ptr(&shared).cast_mut().cast(),
            schedule_work,
        });
        Self {
            shared,
            schedule,
            responses,
            scratch: vec![0; RING / 8],
            pending,
            thread: None,
        }
    }

    /// The `work:schedule` feature.
    pub(crate) fn feature(&self) -> LV2Feature {
        LV2Feature {
            uri: SCHEDULE.as_ptr(),
            data: std::ptr::from_ref(&*self.schedule).cast_mut().cast(),
        }
    }

    /// The plugin exists: `plugin` is its handle, `interface` its
    /// `work:interface` (without one, what it schedules is never done).
    ///
    /// # Safety
    ///
    /// `plugin` and `interface` stay valid until this is dropped.
    pub(crate) unsafe fn start(
        &mut self,
        plugin: *mut c_void,
        interface: Option<std::ptr::NonNull<WorkerInterface>>,
    ) {
        let interface = interface.map_or(std::ptr::null_mut(), |i| i.as_ptr());
        self.shared.plugin.store(plugin, Ordering::Release);
        self.shared.interface.store(interface, Ordering::Release);
        let Some((requests, responses)) = self.pending.take() else {
            return;
        };
        if interface.is_null() {
            return;
        }
        let shared = Arc::clone(&self.shared);
        let thread = std::thread::Builder::new()
            .name("dawkey-lv2-worker".into())
            .spawn(move || work_thread(&shared, requests, responses));
        match thread {
            Ok(thread) => {
                if let Mode::Threaded { thread: slot, .. } = &self.shared.mode {
                    let _ = slot.set(thread.thread().clone());
                }
                self.thread = Some(thread);
            }
            Err(e) => tracing::warn!("no LV2 worker thread: {e}"),
        }
    }

    /// Hands the plugin the responses that are ready, then tells it the run
    /// is over. Real-time safe; called on the audio thread after `run`.
    pub(crate) fn end_run(&mut self) {
        let plugin = self.shared.plugin.load(Ordering::Acquire);
        // SAFETY: the interface is the plugin's, valid while it is.
        let Some(interface) = (unsafe { self.shared.interface.load(Ordering::Acquire).as_ref() })
        else {
            return;
        };
        let scratch = &mut self.scratch;
        if let Some(work_response) = interface.work_response {
            self.responses.read(|_, parts| {
                // Responses are smaller than the ring, which `scratch`
                // holds: this never allocates.
                let bytes = gather(scratch, parts);
                // SAFETY: the plugin's own function, on its audio thread.
                unsafe { work_response(plugin, bytes.len() as u32, bytes.as_ptr().cast()) };
            });
        }
        if let Some(end_run) = interface.end_run {
            // SAFETY: as above.
            unsafe { end_run(plugin) };
        }
    }
}

impl Worker {
    /// Stops the worker thread (once the work it's doing is done).
    pub(crate) fn stop(&mut self) {
        self.shared.quit.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Does each request as it comes, until the worker is dropped.
fn work_thread(shared: &Shared, mut requests: MessageReader, mut responses: MessageWriter) {
    let mut scratch = Vec::new();
    let plugin = shared.plugin.load(Ordering::Acquire);
    // SAFETY: set before the thread started, valid until it's joined.
    let Some(work) =
        (unsafe { shared.interface.load(Ordering::Acquire).as_ref() }).and_then(|i| i.work)
    else {
        return;
    };
    while !shared.quit.load(Ordering::Acquire) {
        requests.read(|_, parts| {
            let bytes = gather(&mut scratch, parts);
            // SAFETY: the plugin's own function, called from one thread at
            // a time; `responses` is this thread's.
            unsafe {
                work(
                    plugin,
                    respond,
                    std::ptr::from_mut(&mut responses).cast(),
                    bytes.len() as u32,
                    bytes.as_ptr().cast(),
                )
            };
        });
        std::thread::park_timeout(Duration::from_millis(100));
    }
}
