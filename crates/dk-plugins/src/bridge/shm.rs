// SPDX-License-Identifier: AGPL-3.0-or-later
//! The memory a plugin host process shares with Dawkey: one block's audio,
//! events and transport going in, and audio and MIDI coming out, with two
//! futex words to hand it back and forth. At most one block is ever in
//! flight, so the two sides never touch it at the same time: Dawkey writes
//! it and bumps `request` (waking the host process), the host process
//! processes it and sets `done` to `request`, and only then does Dawkey
//! read it (and write the next).
//!
//! Also the two system calls that need care: passing a file descriptor
//! over a Unix socket, and futex waits and wakes on shared memory.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use dk_engine::{MAX_BLOCK, PerNote, PluginEvent, PluginEventKind, Transport};

/// Other outputs a bridged plugin can send to buses.
pub(crate) const MAX_AUX: usize = 16;
/// Events each way per block.
pub(crate) const MAX_EVENTS: usize = dk_engine::MAX_PLUGIN_EVENTS;

const MAGIC: u64 = u64::from_le_bytes(*b"DKBRIDG1");

/// An event as the shared memory holds it.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct WireEvent {
    offset: u32,
    /// 1 note on, 2 note off, 3 MIDI, 4 per-note, 5 parameter.
    kind: u32,
    /// Channel and key; the message bytes (MIDI and per-note).
    data: [u8; 4],
    velocity: f32,
    /// A per-note controller's value, or a parameter's index.
    value: u32,
    param: f64,
}

impl WireEvent {
    pub(crate) fn from_event(event: &PluginEvent) -> Self {
        let mut wire = Self {
            offset: event.offset,
            ..Self::default()
        };
        match event.kind {
            PluginEventKind::NoteOn {
                channel,
                key,
                velocity,
            } => {
                wire.kind = 1;
                wire.data = [channel, key, 0, 0];
                wire.velocity = velocity;
            }
            PluginEventKind::NoteOff { channel, key } => {
                wire.kind = 2;
                wire.data = [channel, key, 0, 0];
            }
            PluginEventKind::Midi { data } => {
                wire.kind = 3;
                wire.data = [data[0], data[1], data[2], 0];
            }
            PluginEventKind::PerNote {
                channel,
                key,
                controller,
                value,
            } => {
                let [a, b, c] = controller.data(channel, key);
                wire.kind = 4;
                wire.data = [a, b, c, 0];
                wire.value = value;
            }
            PluginEventKind::Param { index, value } => {
                wire.kind = 5;
                wire.value = index;
                wire.param = value;
            }
        }
        wire
    }

    pub(crate) fn to_event(self) -> Option<PluginEvent> {
        let [a, b, c, _] = self.data;
        let kind = match self.kind {
            1 => PluginEventKind::NoteOn {
                channel: a,
                key: b,
                velocity: self.velocity,
            },
            2 => PluginEventKind::NoteOff { channel: a, key: b },
            3 => PluginEventKind::Midi { data: [a, b, c] },
            4 => PluginEventKind::PerNote {
                channel: a & 0x0f,
                key: b,
                controller: PerNote::from_data([a, b, c])?,
                value: self.value,
            },
            5 => PluginEventKind::Param {
                index: self.value,
                value: self.param,
            },
            _ => return None,
        };
        Some(PluginEvent {
            offset: self.offset,
            kind,
        })
    }
}

/// The transport as the shared memory holds it.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct WireTransport {
    /// 1 playing, 2 recording, 4 looping.
    flags: u32,
    bar: u32,
    numerator: u32,
    denominator: u32,
    frame: u64,
    seconds: f64,
    beats: f64,
    bpm: f64,
    bpm_per_frame: f64,
    bar_start_beats: f64,
    loop_beats: [f64; 2],
    loop_seconds: [f64; 2],
}

impl WireTransport {
    pub(crate) fn from_transport(t: &Transport) -> Self {
        let flags = u32::from(t.playing)
            | u32::from(t.recording) << 1
            | u32::from(t.loop_beats.is_some()) << 2;
        let (lb, ls) = (
            t.loop_beats.unwrap_or_default(),
            t.loop_seconds.unwrap_or_default(),
        );
        Self {
            flags,
            bar: t.bar,
            numerator: u32::from(t.numerator),
            denominator: u32::from(t.denominator),
            frame: t.frame,
            seconds: t.seconds,
            beats: t.beats,
            bpm: t.bpm,
            bpm_per_frame: t.bpm_per_frame,
            bar_start_beats: t.bar_start_beats,
            loop_beats: [lb.0, lb.1],
            loop_seconds: [ls.0, ls.1],
        }
    }

    pub(crate) fn to_transport(self) -> Transport {
        let looping = self.flags & 4 != 0;
        Transport {
            playing: self.flags & 1 != 0,
            recording: self.flags & 2 != 0,
            frame: self.frame,
            seconds: self.seconds,
            beats: self.beats,
            bpm: self.bpm,
            bpm_per_frame: self.bpm_per_frame,
            bar: self.bar,
            bar_start_beats: self.bar_start_beats,
            numerator: self.numerator as u8,
            denominator: self.denominator as u8,
            loop_beats: looping.then_some((self.loop_beats[0], self.loop_beats[1])),
            loop_seconds: looping.then_some((self.loop_seconds[0], self.loop_seconds[1])),
        }
    }
}

/// One block, in and out.
#[repr(C)]
pub(crate) struct Block {
    pub(crate) frames: u32,
    /// The sidechain holds audio.
    pub(crate) has_sidechain: u32,
    /// Other outputs wanted (at most [`MAX_AUX`]).
    pub(crate) aux_count: u32,
    pub(crate) events_in: u32,
    pub(crate) events_out: u32,
    pub(crate) transport: WireTransport,
    pub(crate) events: [WireEvent; MAX_EVENTS],
    pub(crate) midi_out: [WireEvent; MAX_EVENTS],
    /// The track's audio in, the plugin's out.
    pub(crate) audio: [[f32; MAX_BLOCK]; 2],
    pub(crate) sidechain: [[f32; MAX_BLOCK]; 2],
    pub(crate) aux: [[[f32; MAX_BLOCK]; 2]; MAX_AUX],
}

#[repr(C)]
struct Layout {
    magic: u64,
    /// Futex words: the block asked for, the block done, and a request to
    /// stop.
    request: AtomicU32,
    done: AtomicU32,
    quit: AtomicU32,
    _pad: u32,
    block: Block,
}

/// The shared memory, mapped.
pub(crate) struct Shm {
    layout: *mut Layout,
    fd: OwnedFd,
}

// The mapping lives as long as `Shm`; who touches the block when is the
// protocol above (the atomics are what both sides share at any time).
unsafe impl Send for Shm {}
unsafe impl Sync for Shm {}

impl std::fmt::Debug for Shm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shm").finish_non_exhaustive()
    }
}

impl Shm {
    const LEN: usize = std::mem::size_of::<Layout>();

    /// New shared memory (zeroed), to hand to a host process.
    pub(crate) fn create() -> io::Result<Self> {
        // SAFETY: a NUL-terminated name and valid flags.
        let raw = unsafe { libc::memfd_create(c"dawkey-plugin".as_ptr(), libc::MFD_CLOEXEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a new descriptor that nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: `fd` is valid.
        if unsafe { libc::ftruncate(fd.as_raw_fd(), Self::LEN as libc::off_t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let shm = Self::map(fd)?;
        // SAFETY: just mapped, zeroed by ftruncate; nobody else has it yet.
        unsafe { (*shm.layout).magic = MAGIC };
        Ok(shm)
    }

    /// The shared memory a host process was handed.
    pub(crate) fn open(fd: OwnedFd) -> io::Result<Self> {
        let shm = Self::map(fd)?;
        // SAFETY: mapped at full size; the creator wrote the magic before
        // sending the descriptor.
        if unsafe { (*shm.layout).magic } != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not Dawkey's plugin bridge memory",
            ));
        }
        Ok(shm)
    }

    fn map(fd: OwnedFd) -> io::Result<Self> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `fd` is valid; `stat` is written by fstat.
        if unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstat succeeded.
        if (unsafe { stat.assume_init() }.st_size as usize) < Self::LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the plugin bridge memory is too small",
            ));
        }
        // SAFETY: mapping a descriptor we own, readable and writable,
        // shared, at the size checked above.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                Self::LEN,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            layout: ptr.cast(),
            fd,
        })
    }

    pub(crate) fn fd(&self) -> BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.fd.as_fd()
    }

    fn word(&self, pick: fn(&Layout) -> &AtomicU32) -> &AtomicU32 {
        // SAFETY: the mapping lives as long as `self`; atomics may be
        // shared.
        pick(unsafe { &*self.layout })
    }

    pub(crate) fn request(&self) -> &AtomicU32 {
        self.word(|l| &l.request)
    }

    pub(crate) fn done(&self) -> &AtomicU32 {
        self.word(|l| &l.done)
    }

    pub(crate) fn quit(&self) -> &AtomicU32 {
        self.word(|l| &l.quit)
    }

    /// The block.
    ///
    /// # Safety
    ///
    /// Only the side whose turn it is (see the module docs) may use it, and
    /// only until it hands it over.
    #[expect(
        clippy::mut_from_ref,
        reason = "the protocol makes the access exclusive"
    )]
    pub(crate) unsafe fn block(&self) -> &mut Block {
        // SAFETY: as the caller promises.
        unsafe { &mut (*self.layout).block }
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        // SAFETY: mapped in `map` with this length; nothing refers to it
        // once `self` goes.
        unsafe { libc::munmap(self.layout.cast(), Self::LEN) };
    }
}

/// Waits until `word` isn't `expected` (or `timeout` passes, or a spurious
/// wake). Works across processes.
pub(crate) fn futex_wait(word: &AtomicU32, expected: u32, timeout: Duration) {
    let ts = libc::timespec {
        tv_sec: timeout.as_secs() as libc::time_t,
        tv_nsec: timeout.subsec_nanos().into(),
    };
    // SAFETY: `word` is a valid, aligned u32; FUTEX_WAIT (not private, so
    // it works on shared memory) reads it and sleeps.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            word.as_ptr(),
            libc::FUTEX_WAIT,
            expected,
            &raw const ts,
        );
    }
}

/// Wakes whoever waits on `word`. Doesn't block (fine on the audio thread).
pub(crate) fn futex_wake(word: &AtomicU32) {
    // SAFETY: as above; FUTEX_WAKE only wakes.
    unsafe {
        libc::syscall(libc::SYS_futex, word.as_ptr(), libc::FUTEX_WAKE, i32::MAX);
    }
}

/// Bumps `word` and wakes its waiter.
pub(crate) fn signal(word: &AtomicU32, value: u32) {
    word.store(value, Ordering::Release);
    futex_wake(word);
}

/// Sends `fd` over `socket` (with a byte of data, as sendmsg needs).
pub(crate) fn send_fd(socket: &UnixStream, fd: BorrowedFd<'_>) -> io::Result<()> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    // Room for one descriptor.
    let mut control = [0u8; 64];
    // SAFETY: a zeroed msghdr is valid; the pointers set below outlive the
    // call; CMSG_* compute within `control`, which is big enough for one fd.
    unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &raw mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) as usize;
        let cmsg = libc::CMSG_FIRSTHDR(&raw const msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as u32) as usize;
        std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<i32>(), fd.as_raw_fd());
        if libc::sendmsg(socket.as_raw_fd(), &raw const msg, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Receives a descriptor [`send_fd`] sent.
pub(crate) fn recv_fd(socket: &UnixStream) -> io::Result<OwnedFd> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0u8; 64];
    // SAFETY: as in `send_fd`; the descriptor received is new and ours.
    unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &raw mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = control.len();
        if libc::recvmsg(socket.as_raw_fd(), &raw mut msg, libc::MSG_CMSG_CLOEXEC) <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "no descriptor from Dawkey",
            ));
        }
        let cmsg = libc::CMSG_FIRSTHDR(&raw const msg);
        if cmsg.is_null()
            || (*cmsg).cmsg_level != libc::SOL_SOCKET
            || (*cmsg).cmsg_type != libc::SCM_RIGHTS
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "no descriptor from Dawkey",
            ));
        }
        let fd = std::ptr::read_unaligned(libc::CMSG_DATA(cmsg).cast::<i32>());
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_and_the_transport_survive_the_trip() {
        let events = [
            PluginEventKind::NoteOn {
                channel: 2,
                key: 60,
                velocity: 0.5,
            },
            PluginEventKind::NoteOff {
                channel: 2,
                key: 60,
            },
            PluginEventKind::Midi {
                data: [0xb0, 7, 100],
            },
            PluginEventKind::PerNote {
                channel: 3,
                key: 64,
                controller: PerNote::Registered(74),
                value: 0x8000_0000,
            },
            PluginEventKind::Param {
                index: 3,
                value: -6.5,
            },
        ];
        for (offset, kind) in events.into_iter().enumerate() {
            let event = PluginEvent {
                offset: offset as u32,
                kind,
            };
            assert_eq!(WireEvent::from_event(&event).to_event(), Some(event));
        }
        let transport = Transport {
            playing: true,
            frame: 48_000,
            bpm: 120.0,
            numerator: 3,
            denominator: 4,
            loop_beats: Some((4.0, 8.0)),
            loop_seconds: Some((2.0, 4.0)),
            ..Transport::default()
        };
        assert_eq!(
            WireTransport::from_transport(&transport).to_transport(),
            transport
        );
    }

    #[test]
    fn shared_memory_and_descriptors_cross_over() {
        let shm = Shm::create().unwrap();
        let (a, b) = UnixStream::pair().unwrap();
        send_fd(&a, shm.fd()).unwrap();
        let other = Shm::open(recv_fd(&b).unwrap()).unwrap();
        // SAFETY: one thread, one side at a time.
        unsafe { shm.block().audio[0][7] = 0.25 };
        signal(shm.request(), 1);
        assert_eq!(other.request().load(Ordering::Acquire), 1);
        // SAFETY: as above.
        assert_eq!(unsafe { other.block().audio[0][7] }, 0.25);
        // A wait on a word that already changed returns at once.
        futex_wait(other.request(), 0, Duration::from_secs(5));
    }
}
