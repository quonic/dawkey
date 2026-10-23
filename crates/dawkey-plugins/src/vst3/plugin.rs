// SPDX-License-Identifier: AGPL-3.0-or-later
//! One VST3 plugin: its component (the processor) and its edit controller,
//! made, connected and set up, and shut down when the last of its halves
//! goes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper, Interface};

use super::host::{ComponentHandler, HandlerState, HostContext, MemoryStream, RunLoop};
use super::module::Module;
use super::{ok, read_wstring};
use crate::PluginError;
use crate::ports::Ports;

/// A plugin, made and initialized. Shared by its control-thread half (the
/// instance) and its processor: whichever goes last shuts it down.
pub(crate) struct Plugin {
    // Released in this order (fields drop in order): what was asked of the
    // component before the component itself, which some plugins (DPF's)
    // want last.
    /// The two halves' connection points, connected to each other.
    connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    /// The edit controller, if it has one (the component itself, or an
    /// object of its own).
    pub(crate) controller: Option<ComPtr<IEditController>>,
    pub(crate) processor: ComPtr<IAudioProcessor>,
    pub(crate) component: ComPtr<IComponent>,
    /// The controller is an object of its own (initialized and terminated
    /// separately).
    separate: bool,
    pub(crate) active: AtomicBool,
    pub(crate) processing: AtomicBool,
    pub(crate) handler: Arc<HandlerState>,
    pub(crate) run_loop: Arc<RunLoop>,
    _handler: ComWrapper<ComponentHandler>,
    _context: ComWrapper<HostContext>,
    _module: Arc<Module>,
}

// SAFETY: VST3 splits a plugin's calls by thread: the processor calls
// `IAudioProcessor::process` and `setProcessing` on the audio thread, and
// everything else happens on the control thread that made it. `Plugin` is
// shared between those two halves only to shut it down after both are done
// (`Drop`), which happens on the control thread (the engine hands
// processors back to it).
unsafe impl Send for Plugin {}
unsafe impl Sync for Plugin {}

impl Plugin {
    /// Makes class `cid` of `module`, initializes it and its controller,
    /// and connects them.
    pub(crate) fn create(module: Arc<Module>, cid: &TUID) -> Result<Self, PluginError> {
        let run_loop = Arc::new(RunLoop::default());
        let context = ComWrapper::new(HostContext::new(Some(Arc::clone(&run_loop))));
        let unknown = context.to_com_ptr::<FUnknown>().expect("it's an FUnknown");
        let factory = &module.factory;
        let component: ComPtr<IComponent> = create_instance(factory, cid)
            .ok_or_else(|| PluginError::new("the plugin couldn't be made"))?;
        // SAFETY: the context outlives the plugin (kept in it).
        if !ok(unsafe { component.initialize(unknown.as_ptr()) }) {
            return Err(PluginError::new("the plugin didn't initialize"));
        }
        let processor = match component.cast::<IAudioProcessor>() {
            Some(processor) => processor,
            None => {
                unsafe { component.terminate() };
                return Err(PluginError::new("it isn't an audio processor"));
            }
        };
        let (controller, separate) = match component.cast::<IEditController>() {
            Some(controller) => (Some(controller), false),
            None => {
                let mut id: TUID = [0; 16];
                // SAFETY: room for a class ID.
                let controller = if ok(unsafe { component.getControllerClassId(&mut id) }) {
                    create_instance::<IEditController>(factory, &id)
                        .filter(|c| ok(unsafe { c.initialize(unknown.as_ptr()) }))
                } else {
                    None
                };
                let separate = controller.is_some();
                (controller, separate)
            }
        };
        let connection = if separate {
            let controller = controller.as_ref().expect("separate means there's one");
            match (
                component.cast::<IConnectionPoint>(),
                controller.cast::<IConnectionPoint>(),
            ) {
                (Some(a), Some(b)) => {
                    // SAFETY: both live; each keeps the other until
                    // disconnected (in `Drop`).
                    unsafe {
                        a.connect(b.as_ptr());
                        b.connect(a.as_ptr());
                    }
                    Some((a, b))
                }
                _ => None,
            }
        } else {
            None
        };
        let handler = Arc::new(HandlerState::default());
        let handler_object = ComWrapper::new(ComponentHandler {
            state: Arc::clone(&handler),
        });
        if let Some(controller) = &controller {
            let handler = handler_object
                .to_com_ptr::<IComponentHandler>()
                .expect("it's a component handler");
            // SAFETY: the handler outlives the controller's use of it (it's
            // taken away in `Drop` first).
            unsafe { controller.setComponentHandler(handler.as_ptr()) };
        }
        Ok(Self {
            component,
            processor,
            controller,
            separate,
            connection,
            active: AtomicBool::new(false),
            processing: AtomicBool::new(false),
            handler,
            run_loop,
            _handler: handler_object,
            _context: context,
            _module: module,
        })
    }

    /// The component's state, then the controller's.
    pub(crate) fn save_state(&self) -> (Vec<u8>, Vec<u8>) {
        let stream = ComWrapper::new(MemoryStream::default());
        let ptr = stream.to_com_ptr::<IBStream>().expect("it's a stream");
        // SAFETY: a live stream for the call.
        unsafe { self.component.getState(ptr.as_ptr()) };
        let component = stream.take();
        let controller = match self.own_controller() {
            Some(controller) => {
                unsafe { controller.getState(ptr.as_ptr()) };
                stream.take()
            }
            None => Vec::new(),
        };
        (component, controller)
    }

    /// Restores a state [`save_state`](Self::save_state) saved.
    pub(crate) fn restore_state(&self, component: &[u8], controller: &[u8]) -> Result<(), String> {
        let stream = ComWrapper::new(MemoryStream::new(component.to_owned()));
        let ptr = stream.to_com_ptr::<IBStream>().expect("it's a stream");
        // SAFETY: a live stream for the call.
        if !ok(unsafe { self.component.setState(ptr.as_ptr()) }) {
            return Err("it didn't take its state".to_owned());
        }
        if let Some(c) = self.own_controller() {
            stream.rewind();
            unsafe { c.setComponentState(ptr.as_ptr()) };
            if !controller.is_empty() {
                let stream = ComWrapper::new(MemoryStream::new(controller.to_owned()));
                let ptr = stream.to_com_ptr::<IBStream>().expect("it's a stream");
                unsafe { c.setState(ptr.as_ptr()) };
            }
        }
        Ok(())
    }

    /// The controller, when it's an object of its own. (One object that's
    /// both has one state: its `getState` and `setState` are the same.)
    fn own_controller(&self) -> Option<&ComPtr<IEditController>> {
        self.controller.as_ref().filter(|_| self.separate)
    }

    /// Tells the controller what the component starts with.
    pub(crate) fn sync_controller(&self) {
        let Some(controller) = self.own_controller() else {
            return;
        };
        let stream = ComWrapper::new(MemoryStream::default());
        let ptr = stream.to_com_ptr::<IBStream>().expect("it's a stream");
        // SAFETY: a live stream for both calls.
        unsafe {
            if ok(self.component.getState(ptr.as_ptr())) {
                stream.rewind();
                controller.setComponentState(ptr.as_ptr());
            }
        }
    }

    /// The audio buses one way, and whether there are event buses.
    pub(crate) fn buses(&self, direction: BusDirection) -> (Ports, bool) {
        let component = &self.component;
        let mut ports = Ports::default();
        // SAFETY: plain queries.
        let count = unsafe { component.getBusCount(MediaTypes_::kAudio as i32, direction) };
        for i in 0..count.max(0) {
            let mut bus: BusInfo = unsafe { std::mem::zeroed() };
            if !ok(unsafe {
                component.getBusInfo(MediaTypes_::kAudio as i32, direction, i, &mut bus)
            }) {
                continue;
            }
            if bus.busType == BusTypes_::kMain as i32 && ports.main.is_none() {
                ports.main = Some(ports.channels.len());
            }
            ports.channels.push(bus.channelCount.max(0) as u32);
            ports.names.push(read_wstring(&bus.name));
        }
        if ports.main.is_none() && !ports.channels.is_empty() {
            ports.main = Some(0);
        }
        let events = unsafe { component.getBusCount(MediaTypes_::kEvent as i32, direction) } > 0;
        (ports, events)
    }

    /// Asks for stereo main buses (the track's two channels), keeping the
    /// others as they are. Plugins that won't stay as they were.
    pub(crate) fn arrange_buses(&self) {
        let arrangement = |direction: BusDirection| -> Vec<SpeakerArrangement> {
            let (ports, _) = self.buses(direction);
            (0..ports.channels.len())
                .map(|i| {
                    if Some(i) == ports.main {
                        return SpeakerArr::kStereo;
                    }
                    let mut current: SpeakerArrangement = 0;
                    // SAFETY: a plain query.
                    let ok = ok(unsafe {
                        self.processor
                            .getBusArrangement(direction, i as i32, &mut current)
                    });
                    if ok { current } else { SpeakerArr::kStereo }
                })
                .collect()
        };
        let mut inputs = arrangement(BusDirections_::kInput as i32);
        let mut outputs = arrangement(BusDirections_::kOutput as i32);
        // SAFETY: arrays of the lengths given.
        unsafe {
            self.processor.setBusArrangements(
                inputs.as_mut_ptr(),
                inputs.len() as i32,
                outputs.as_mut_ptr(),
                outputs.len() as i32,
            );
        }
    }

    /// Activates every bus (unused inputs get silence).
    pub(crate) fn activate_buses(&self) {
        for media in [MediaTypes_::kAudio, MediaTypes_::kEvent] {
            for direction in [BusDirections_::kInput, BusDirections_::kOutput] {
                // SAFETY: plain calls.
                unsafe {
                    let count = self.component.getBusCount(media as i32, direction as i32);
                    for i in 0..count.max(0) {
                        self.component
                            .activateBus(media as i32, direction as i32, i, 1);
                    }
                }
            }
        }
    }

    /// `setActive(true)`, then `setProcessing(true)`.
    pub(crate) fn start(&self) -> Result<(), PluginError> {
        // SAFETY: called after setupProcessing, on the control thread.
        if !ok(unsafe { self.component.setActive(1) }) {
            return Err(PluginError::new("activating failed"));
        }
        self.active.store(true, Ordering::Release);
        // Some plugins return kNotImplemented here; that's fine.
        unsafe { self.processor.setProcessing(1) };
        self.processing.store(true, Ordering::Release);
        Ok(())
    }

    /// Stops processing (on the audio thread, as it leaves it).
    pub(crate) fn stop_processing(&self) {
        if self.processing.swap(false, Ordering::AcqRel) {
            // SAFETY: setProcessing may be called on the processing thread.
            // It's plugin code there (JUCE resets, which may allocate).
            assert_no_alloc::permit_alloc(|| unsafe { self.processor.setProcessing(0) });
        }
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        self.run_loop.clear();
        // SAFETY: the last half is gone (or going) on the control thread;
        // the plugin is shut down in the order VST3 asks.
        unsafe {
            if self.processing.swap(false, Ordering::AcqRel) {
                self.processor.setProcessing(0);
            }
            if self.active.swap(false, Ordering::AcqRel) {
                self.component.setActive(0);
            }
            if let Some((a, b)) = &self.connection {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if let Some(controller) = &self.controller {
                controller.setComponentHandler(std::ptr::null_mut());
                if self.separate {
                    controller.terminate();
                }
            }
            self.component.terminate();
        }
    }
}

/// An instance of class `cid` as interface `I`.
pub(crate) fn create_instance<I: Interface>(
    factory: &ComPtr<IPluginFactory>,
    cid: &TUID,
) -> Option<ComPtr<I>> {
    let mut object = std::ptr::null_mut();
    let iid = I::IID;
    // SAFETY: 16-byte IDs; the factory gives an owned reference (or null).
    let result = unsafe {
        factory.createInstance(
            cid.as_ptr(),
            iid.as_ptr() as *const std::ffi::c_char,
            &mut object,
        )
    };
    if !ok(result) {
        return None;
    }
    unsafe { ComPtr::from_raw(object as *mut I) }
}
