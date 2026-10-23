// SPDX-License-Identifier: AGPL-3.0-or-later
//! What Dawkey gives VST3 plugins: its host context (name, messages, the
//! run loop), a component handler for edits the plugin makes, streams for
//! state, and the frame its editor sits in.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use vst3::Steinberg::Linux::*;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface};

use super::write_wstring;
use crate::run_loop::Timers;

/// Interfaces Dawkey calls on plugins (for `IPlugInterfaceSupport`).
fn supported(iid: &[u8; 16]) -> bool {
    [
        IComponent::IID,
        IAudioProcessor::IID,
        IEditController::IID,
        IConnectionPoint::IID,
        IMidiMapping::IID,
        IProcessContextRequirements::IID,
    ]
    .contains(iid)
}

/// A pointer's identity, for telling handlers apart.
fn same<I: Interface>(a: &ComPtr<I>, b: *mut I) -> bool {
    std::ptr::eq(a.as_ptr(), b)
}

/// The timers and descriptors a plugin registered (`IRunLoop`), run on the
/// control thread.
#[derive(Default)]
pub(crate) struct RunLoop {
    timers: Mutex<Timers<TimerKey>>,
    fds: Mutex<Vec<(RawFd, ComPtr<IEventHandler>)>>,
}

/// A timer handler, compared by identity.
#[derive(Clone)]
pub(crate) struct TimerKey(ComPtr<ITimerHandler>);

// SAFETY: handlers are only called on the control thread (the run loop's);
// the lock only keeps the bookkeeping straight.
unsafe impl Send for RunLoop {}
unsafe impl Sync for RunLoop {}

impl RunLoop {
    /// Descriptors to watch, as `poll` events.
    pub(crate) fn poll_fds(&self) -> Vec<(RawFd, i16)> {
        self.fds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|&(fd, _)| (fd, libc::POLLIN))
            .collect()
    }

    /// Descriptor `fd` is ready.
    pub(crate) fn on_fd(&self, fd: RawFd) {
        // Called outside the lock: the handler may unregister itself.
        let handlers: Vec<ComPtr<IEventHandler>> = self
            .fds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(f, _)| *f == fd)
            .map(|(_, h)| h.clone())
            .collect();
        for handler in handlers {
            // SAFETY: a live handler the plugin registered.
            unsafe { handler.onFDIsSet(fd) };
        }
    }

    /// Runs the timers that are due; returns when the next one is.
    pub(crate) fn run_timers(&self, now: Instant) -> Option<Instant> {
        let due = self
            .timers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .due(now);
        for TimerKey(handler) in due {
            // SAFETY: a live handler the plugin registered.
            unsafe { handler.onTimer() };
        }
        self.timers.lock().unwrap_or_else(|e| e.into_inner()).next()
    }

    /// Services everything that's ready, without waiting (for instances
    /// nothing else polls for).
    pub(crate) fn run_now(&self) {
        let mut fds: Vec<libc::pollfd> = self
            .poll_fds()
            .into_iter()
            .map(|(fd, events)| libc::pollfd {
                fd,
                events,
                revents: 0,
            })
            .collect();
        if !fds.is_empty() {
            // SAFETY: `fds` is a valid array of pollfds for its length.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 0) };
            if ready > 0 {
                for fd in fds.iter().filter(|f| f.revents != 0) {
                    self.on_fd(fd.fd);
                }
            }
        }
        self.run_timers(Instant::now());
    }

    /// Forgets every handler (the plugin is going).
    pub(crate) fn clear(&self) {
        self.timers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.fds.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    fn register_handler(&self, handler: *mut IEventHandler, fd: RawFd) -> tresult {
        // SAFETY: the plugin passes a live handler; we keep a reference.
        let Some(handler) = (unsafe { ComRef::from_raw(handler) }) else {
            return kInvalidArgument;
        };
        self.fds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((fd, handler.to_com_ptr()));
        kResultOk
    }

    fn unregister_handler(&self, handler: *mut IEventHandler) -> tresult {
        self.fds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(_, h)| !same(h, handler));
        kResultOk
    }

    fn register_timer(&self, handler: *mut ITimerHandler, ms: TimerInterval) -> tresult {
        // SAFETY: as for handlers.
        let Some(handler) = (unsafe { ComRef::from_raw(handler) }) else {
            return kInvalidArgument;
        };
        self.timers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .add(TimerKey(handler.to_com_ptr()), ms);
        kResultOk
    }

    fn unregister_timer(&self, handler: *mut ITimerHandler) -> tresult {
        self.timers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(|TimerKey(h)| same(h, handler));
        kResultOk
    }
}

/// Dawkey as a plugin sees it: `IHostApplication` (its name, and messages
/// for a plugin's two halves to talk with), `IPlugInterfaceSupport`, and
/// the run loop (when it's an instance's).
pub(crate) struct HostContext {
    run_loop: Option<Arc<RunLoop>>,
}

impl HostContext {
    pub(crate) fn new(run_loop: Option<Arc<RunLoop>>) -> Self {
        Self { run_loop }
    }
}

impl Class for HostContext {
    type Interfaces = (IHostApplication, IPlugInterfaceSupport, IRunLoop);
}

impl IHostApplicationTrait for HostContext {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        // SAFETY: the plugin passes room for a String128.
        write_wstring("Dawkey", unsafe { &mut *name });
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the plugin passes 16-byte IDs and somewhere to put the
        // object.
        let (cid, iid) = unsafe { (&*(cid as *const [u8; 16]), &*(iid as *const [u8; 16])) };
        let object = if *cid == IMessage::IID {
            ComWrapper::new(Message::default()).to_com_ptr::<FUnknown>()
        } else if *cid == IAttributeList::IID {
            ComWrapper::new(AttributeList::default()).to_com_ptr::<FUnknown>()
        } else {
            None
        };
        let Some(object) = object else {
            unsafe { *obj = std::ptr::null_mut() };
            return kNoInterface;
        };
        let ptr = object.as_ptr();
        // SAFETY: a live object of ours; queryInterface adds the reference
        // the caller owns.
        unsafe { ((*(*ptr).vtbl).queryInterface)(ptr, iid as *const _ as *const TUID, obj) }
    }
}

impl IPlugInterfaceSupportTrait for HostContext {
    unsafe fn isPlugInterfaceSupported(&self, iid: *const TUID) -> tresult {
        if iid.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: a 16-byte ID.
        let iid = unsafe { &*(iid as *const [u8; 16]) };
        if supported(iid) {
            kResultTrue
        } else {
            kResultFalse
        }
    }
}

impl IRunLoopTrait for HostContext {
    unsafe fn registerEventHandler(
        &self,
        handler: *mut IEventHandler,
        fd: FileDescriptor,
    ) -> tresult {
        match &self.run_loop {
            Some(run_loop) => run_loop.register_handler(handler, fd),
            None => kNotImplemented,
        }
    }

    unsafe fn unregisterEventHandler(&self, handler: *mut IEventHandler) -> tresult {
        match &self.run_loop {
            Some(run_loop) => run_loop.unregister_handler(handler),
            None => kNotImplemented,
        }
    }

    unsafe fn registerTimer(&self, handler: *mut ITimerHandler, ms: TimerInterval) -> tresult {
        match &self.run_loop {
            Some(run_loop) => run_loop.register_timer(handler, ms),
            None => kNotImplemented,
        }
    }

    unsafe fn unregisterTimer(&self, handler: *mut ITimerHandler) -> tresult {
        match &self.run_loop {
            Some(run_loop) => run_loop.unregister_timer(handler),
            None => kNotImplemented,
        }
    }
}

/// An edit the plugin made through its component handler.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Edit {
    Begin(ParamID),
    Perform(ParamID, f64),
    End(ParamID),
}

/// What the plugin told its component handler, for the control thread.
#[derive(Default)]
pub(crate) struct HandlerState {
    /// Edits, in order. Plugins call from whatever thread (JUCE from its
    /// message thread, some from the audio thread): a value meets a busy
    /// lock by being dropped rather than waiting (the next one comes), the
    /// start and end of a gesture wait (or the gesture would never end).
    pub(crate) edits: Mutex<Vec<Edit>>,
    /// `restartComponent` flags since the last look.
    pub(crate) restart: AtomicI32,
    /// `setDirty(true)` since the last look.
    pub(crate) dirty: AtomicBool,
}

impl HandlerState {
    fn edit(&self, edit: Edit) -> tresult {
        /// Edits kept until the control thread looks (it looks often).
        const MOST: usize = 4096;
        let edits = match edit {
            Edit::Perform(..) => self.edits.try_lock().ok(),
            _ => Some(self.edits.lock().unwrap_or_else(|e| e.into_inner())),
        };
        if let Some(mut edits) = edits
            && (edits.len() < MOST || !matches!(edit, Edit::Perform(..)))
        {
            edits.push(edit);
        }
        kResultOk
    }

    pub(crate) fn take_edits(&self) -> Vec<Edit> {
        std::mem::take(&mut *self.edits.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub(crate) fn take_restart(&self) -> i32 {
        self.restart.swap(0, Ordering::AcqRel)
    }

    /// Forgets what the plugin said so far (while loading).
    pub(crate) fn clear(&self) {
        self.take_edits();
        self.take_restart();
        self.dirty.store(false, Ordering::Release);
    }
}

/// The component handler a plugin's controller reports edits to.
pub(crate) struct ComponentHandler {
    pub(crate) state: Arc<HandlerState>,
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler, IComponentHandler2);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, id: ParamID) -> tresult {
        self.state.edit(Edit::Begin(id))
    }

    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        self.state.edit(Edit::Perform(id, value))
    }

    unsafe fn endEdit(&self, id: ParamID) -> tresult {
        self.state.edit(Edit::End(id))
    }

    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        self.state.restart.fetch_or(flags, Ordering::AcqRel);
        kResultOk
    }
}

impl IComponentHandler2Trait for ComponentHandler {
    unsafe fn setDirty(&self, state: TBool) -> tresult {
        if state != 0 {
            self.state.dirty.store(true, Ordering::Release);
        }
        kResultOk
    }

    unsafe fn requestOpenEditor(&self, _name: FIDString) -> tresult {
        kNotImplemented
    }

    unsafe fn startGroupEdit(&self) -> tresult {
        kResultOk
    }

    unsafe fn finishGroupEdit(&self) -> tresult {
        kResultOk
    }
}

/// A message between a plugin's component and its controller. (Behind
/// locks: a plugin may keep one and read it on another thread.)
#[derive(Default)]
pub(crate) struct Message {
    id: Mutex<Option<std::ffi::CString>>,
    attributes: Mutex<Option<ComWrapper<AttributeList>>>,
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        self.id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(std::ptr::null(), |id| id.as_ptr())
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        let id = (!id.is_null()).then(|| {
            // SAFETY: a NUL-terminated string from the plugin.
            unsafe { CStr::from_ptr(id) }.to_owned()
        });
        *self.id.lock().unwrap_or_else(|e| e.into_inner()) = id;
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        let mut attributes = self.attributes.lock().unwrap_or_else(|e| e.into_inner());
        let attributes =
            attributes.get_or_insert_with(|| ComWrapper::new(AttributeList::default()));
        // Not an added reference: the message owns its list.
        attributes
            .as_com_ref::<IAttributeList>()
            .map_or(std::ptr::null_mut(), |a| a.as_ptr())
    }
}

enum Attribute {
    Int(i64),
    Float(f64),
    String(Vec<u16>),
    Binary(Vec<u8>),
}

/// Attributes of a message.
#[derive(Default)]
pub(crate) struct AttributeList {
    attributes: Mutex<HashMap<Vec<u8>, Attribute>>,
}

impl Class for AttributeList {
    type Interfaces = (IAttributeList,);
}

/// An attribute ID's bytes.
fn attr_id(id: *const c_char) -> Option<Vec<u8>> {
    // SAFETY: a NUL-terminated string from the plugin.
    (!id.is_null()).then(|| unsafe { CStr::from_ptr(id) }.to_bytes().to_owned())
}

impl IAttributeListTrait for AttributeList {
    unsafe fn setInt(&self, id: IAttributeList_::AttrID, value: int64) -> tresult {
        let Some(id) = attr_id(id) else {
            return kInvalidArgument;
        };
        self.attributes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Attribute::Int(value));
        kResultOk
    }

    unsafe fn getInt(&self, id: IAttributeList_::AttrID, value: *mut int64) -> tresult {
        match attr_id(id).and_then(|id| {
            match self
                .attributes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&id)
            {
                Some(Attribute::Int(v)) => Some(*v),
                _ => None,
            }
        }) {
            Some(v) if !value.is_null() => {
                unsafe { *value = v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: IAttributeList_::AttrID, value: f64) -> tresult {
        let Some(id) = attr_id(id) else {
            return kInvalidArgument;
        };
        self.attributes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Attribute::Float(value));
        kResultOk
    }

    unsafe fn getFloat(&self, id: IAttributeList_::AttrID, value: *mut f64) -> tresult {
        match attr_id(id).and_then(|id| {
            match self
                .attributes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&id)
            {
                Some(Attribute::Float(v)) => Some(*v),
                _ => None,
            }
        }) {
            Some(v) if !value.is_null() => {
                unsafe { *value = v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: IAttributeList_::AttrID, string: *const TChar) -> tresult {
        let Some(id) = attr_id(id) else {
            return kInvalidArgument;
        };
        if string.is_null() {
            return kInvalidArgument;
        }
        let mut text = Vec::new();
        let mut at = string;
        // SAFETY: a NUL-terminated UTF-16 string from the plugin.
        unsafe {
            while *at != 0 {
                text.push(*at);
                at = at.add(1);
            }
        }
        text.push(0);
        self.attributes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Attribute::String(text));
        kResultOk
    }

    unsafe fn getString(
        &self,
        id: IAttributeList_::AttrID,
        string: *mut TChar,
        size_in_bytes: uint32,
    ) -> tresult {
        let attributes = self.attributes.lock().unwrap_or_else(|e| e.into_inner());
        let Some(Attribute::String(text)) = attr_id(id).and_then(|id| attributes.get(&id)) else {
            return kResultFalse;
        };
        let room = size_in_bytes as usize / 2;
        if string.is_null() || room == 0 {
            return kInvalidArgument;
        }
        let n = text.len().min(room);
        // SAFETY: the plugin gave `size_in_bytes` of room.
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr(), string, n);
            *string.add(n - 1) = 0;
        }
        kResultOk
    }

    unsafe fn setBinary(
        &self,
        id: IAttributeList_::AttrID,
        data: *const c_void,
        size_in_bytes: uint32,
    ) -> tresult {
        let Some(id) = attr_id(id) else {
            return kInvalidArgument;
        };
        let bytes = if data.is_null() || size_in_bytes == 0 {
            Vec::new()
        } else {
            // SAFETY: the plugin passes that many bytes.
            unsafe { std::slice::from_raw_parts(data as *const u8, size_in_bytes as usize) }
                .to_owned()
        };
        self.attributes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Attribute::Binary(bytes));
        kResultOk
    }

    unsafe fn getBinary(
        &self,
        id: IAttributeList_::AttrID,
        data: *mut *const c_void,
        size_in_bytes: *mut uint32,
    ) -> tresult {
        let attributes = self.attributes.lock().unwrap_or_else(|e| e.into_inner());
        let Some(Attribute::Binary(bytes)) = attr_id(id).and_then(|id| attributes.get(&id)) else {
            return kResultFalse;
        };
        if data.is_null() || size_in_bytes.is_null() {
            return kInvalidArgument;
        }
        // The bytes stay where they are while the list lives unchanged.
        unsafe {
            *data = bytes.as_ptr() as *const c_void;
            *size_in_bytes = bytes.len() as u32;
        }
        kResultOk
    }
}

/// A stream in memory, for plugin state.
#[derive(Default)]
pub(crate) struct MemoryStream {
    data: RefCell<Vec<u8>>,
    pos: Cell<usize>,
}

impl MemoryStream {
    pub(crate) fn new(data: Vec<u8>) -> Self {
        Self {
            data: RefCell::new(data),
            pos: Cell::new(0),
        }
    }

    pub(crate) fn take(&self) -> Vec<u8> {
        self.pos.set(0);
        std::mem::take(&mut *self.data.borrow_mut())
    }

    pub(crate) fn rewind(&self) {
        self.pos.set(0);
    }
}

impl Class for MemoryStream {
    type Interfaces = (IBStream, ISizeableStream);
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(&self, buffer: *mut c_void, num_bytes: int32, num_read: *mut int32) -> tresult {
        let data = self.data.borrow();
        let pos = self.pos.get().min(data.len());
        let n = (num_bytes.max(0) as usize).min(data.len() - pos);
        if n > 0 {
            if buffer.is_null() {
                return kInvalidArgument;
            }
            // SAFETY: the plugin gave room for `num_bytes`.
            unsafe { std::ptr::copy_nonoverlapping(data[pos..].as_ptr(), buffer as *mut u8, n) };
        }
        self.pos.set(pos + n);
        if !num_read.is_null() {
            unsafe { *num_read = n as int32 };
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_written: *mut int32,
    ) -> tresult {
        let n = num_bytes.max(0) as usize;
        if n > 0 && buffer.is_null() {
            return kInvalidArgument;
        }
        let mut data = self.data.borrow_mut();
        let pos = self.pos.get();
        if data.len() < pos + n {
            data.resize(pos + n, 0);
        }
        if n > 0 {
            // SAFETY: the plugin passes `num_bytes` bytes.
            let bytes = unsafe { std::slice::from_raw_parts(buffer as *const u8, n) };
            data[pos..pos + n].copy_from_slice(bytes);
        }
        self.pos.set(pos + n);
        if !num_written.is_null() {
            unsafe { *num_written = n as int32 };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let len = self.data.borrow().len() as i64;
        let base = match mode as u32 {
            IBStream_::IStreamSeekMode_::kIBSeekSet => 0,
            IBStream_::IStreamSeekMode_::kIBSeekCur => self.pos.get() as i64,
            IBStream_::IStreamSeekMode_::kIBSeekEnd => len,
            _ => return kInvalidArgument,
        };
        let to = base.saturating_add(pos);
        if to < 0 {
            return kInvalidArgument;
        }
        self.pos.set(to as usize);
        if !result.is_null() {
            unsafe { *result = to };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        unsafe { *pos = self.pos.get() as int64 };
        kResultOk
    }
}

impl ISizeableStreamTrait for MemoryStream {
    unsafe fn getStreamSize(&self, size: *mut int64) -> tresult {
        if size.is_null() {
            return kInvalidArgument;
        }
        unsafe { *size = self.data.borrow().len() as int64 };
        kResultOk
    }

    unsafe fn setStreamSize(&self, size: int64) -> tresult {
        if size < 0 {
            return kInvalidArgument;
        }
        self.data.borrow_mut().resize(size as usize, 0);
        kResultOk
    }
}

/// The frame a plugin's editor sits in: it asks it for a new size, and
/// gets the run loop from it.
pub(crate) struct PlugFrame {
    run_loop: Arc<RunLoop>,
    /// The size the editor asked for, for the window.
    pub(crate) resize: Mutex<Option<(u32, u32)>>,
}

impl PlugFrame {
    pub(crate) fn new(run_loop: Arc<RunLoop>) -> Self {
        Self {
            run_loop,
            resize: Mutex::new(None),
        }
    }
}

impl Class for PlugFrame {
    type Interfaces = (IPlugFrame, IRunLoop);
}

impl IPlugFrameTrait for PlugFrame {
    unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        if view.is_null() || new_size.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the editor passes itself and a rectangle.
        let (view, rect) = unsafe { (ComRef::from_raw_unchecked(view), &mut *new_size) };
        let width = (rect.right - rect.left).max(1) as u32;
        let height = (rect.bottom - rect.top).max(1) as u32;
        *self.resize.lock().unwrap_or_else(|e| e.into_inner()) = Some((width, height));
        // The editor takes its new size in this call (VST3 wants it so);
        // the window follows when the host next looks.
        let mut rect = *rect;
        unsafe { view.onSize(&mut rect) };
        kResultTrue
    }
}

impl IRunLoopTrait for PlugFrame {
    unsafe fn registerEventHandler(
        &self,
        handler: *mut IEventHandler,
        fd: FileDescriptor,
    ) -> tresult {
        self.run_loop.register_handler(handler, fd)
    }

    unsafe fn unregisterEventHandler(&self, handler: *mut IEventHandler) -> tresult {
        self.run_loop.unregister_handler(handler)
    }

    unsafe fn registerTimer(&self, handler: *mut ITimerHandler, ms: TimerInterval) -> tresult {
        self.run_loop.register_timer(handler, ms)
    }

    unsafe fn unregisterTimer(&self, handler: *mut ITimerHandler) -> tresult {
        self.run_loop.unregister_timer(handler)
    }
}
