// SPDX-License-Identifier: AGPL-3.0-or-later
//! Links Dawkey's ports to the default devices.
//!
//! WirePlumber's stream policy only links nodes that have an audio adapter,
//! and a `pw_filter` doesn't, so (like JACK clients) Dawkey makes its own
//! links. WirePlumber still owns the policy decision: the targets are what it
//! publishes as `default.audio.sink` and `default.audio.source` in the
//! `default` metadata, and the links follow them when they change.
//!
//! - Playback: Dawkey's outputs → the default sink, always.
//! - Capture: the default source → Dawkey's inputs, only while capture is
//!   on (an audio track is armed), so the microphone isn't held open
//!   otherwise.
//! - MIDI in: the MIDI devices that armed MIDI tracks listen to (all of
//!   them, unless every armed track picked one) → a Dawkey input port per
//!   device (`midi_in_N`), only while a MIDI track is armed. A device is a
//!   port of WirePlumber's MIDI bridge nodes, except the ALSA "Midi
//!   Through" loopback. MIDI ports of other apps (JACK clients, other
//!   PipeWire filters) can be picked too, named `node:port`, but "every
//!   device" leaves them out.
//! - MIDI out: a Dawkey output port per device (or app) that tracks send
//!   to (`midi_out_N`) → that device.
//!
//! A device keeps its port while it's wanted; ports of devices no longer
//! wanted are reused once all are taken.
//!
//! The router also watches the latency MIDI ports report (for ALSA's MIDI
//! bridge, a cycle): what a device's input port takes, so MIDI sent to it
//! can go out that much earlier, and how late what comes out of a device's
//! output port is, so recorded MIDI can be placed that much earlier.
//!
//! The router runs its own PipeWire connection on its own thread, using
//! pipewire-rs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use pipewire as pw;
use pw::types::ObjectType;

use crate::{MIDI_DEVICES, MidiCaptureRoute, MidiPorts, MidiRoutes, Status};

pub(crate) enum RouterMessage {
    /// Dawkey's own node ID, once the filter is registered.
    OwnNode(u32),
    /// How to assign the filter's MIDI device ports.
    Ports(MidiPorts),
    /// Connect (or disconnect) the default source to the inputs.
    Capture(bool),
    /// The MIDI devices to connect.
    Midi(MidiRoutes),
    Quit,
}

pub(crate) struct Router {
    sender: pw::channel::Sender<RouterMessage>,
    thread: Option<JoinHandle<()>>,
}

impl Router {
    pub(crate) fn start(status: Arc<Status>) -> Self {
        let (sender, receiver) = pw::channel::channel();
        let thread = std::thread::Builder::new()
            .name("dawkey-pw-router".into())
            .spawn(move || {
                if let Err(e) = run(receiver, &status) {
                    tracing::warn!("PipeWire router stopped: {e}");
                    if let Ok(mut slot) = status.router_error.lock() {
                        *slot = Some(e.to_string());
                    }
                }
            })
            .expect("failed to spawn PipeWire router thread");
        Self {
            sender,
            thread: Some(thread),
        }
    }

    pub(crate) fn sender(&self) -> pw::channel::Sender<RouterMessage> {
        self.sender.clone()
    }
}

impl Drop for Router {
    fn drop(&mut self) {
        let _ = self.sender.send(RouterMessage::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Debug)]
struct NodeInfo {
    name: String,
    media_class: String,
    /// `application.name`.
    app: String,
}

#[derive(Debug)]
struct PortInfo {
    node: u32,
    output: bool,
    channel: String,
    monitor: bool,
    /// Carries MIDI (or UMP) rather than audio.
    midi: bool,
    /// `port.alias`, or `port.name`.
    name: String,
    /// `port.name`.
    port_name: String,
}

/// The latency a port reports (PipeWire's Latency param): for an input
/// port, how long what goes into it takes to come out of the device (the
/// input direction); for an output port, how long ago what comes out of it
/// went into the device (the output direction). Cycles, frames and
/// nanoseconds, added up.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct PortLatency {
    pub(crate) quantum: f32,
    pub(crate) rate: u32,
    pub(crate) ns: u64,
}

impl PortLatency {
    /// In milliseconds, with `quantum` frames per cycle at `rate` Hz (the
    /// frame parts count as 0 while those aren't known).
    pub(crate) fn ms(self, quantum: u32, rate: u32) -> f64 {
        let frames = f64::from(self.quantum) * f64::from(quantum) + f64::from(self.rate);
        let frames_ms = if rate == 0 {
            0.0
        } else {
            frames * 1000.0 / f64::from(rate)
        };
        frames_ms + self.ns as f64 / 1e6
    }

    /// The latency in a Latency param, if it's for the direction of a
    /// port that's an output (`output`) or an input.
    fn parse(pod: &pw::spa::pod::Pod, output: bool) -> Option<Self> {
        use libspa_sys as spa;
        use pw::spa::utils::Id;
        let object = pod.as_object().ok()?;
        let prop = |key: u32| object.find_prop(Id(key)).map(|p| p.value());
        let direction = prop(spa::SPA_PARAM_LATENCY_direction)?.get_id().ok()?;
        let wanted = if output {
            spa::SPA_DIRECTION_OUTPUT
        } else {
            spa::SPA_DIRECTION_INPUT
        };
        if direction.0 != wanted {
            return None;
        }
        Some(Self {
            quantum: prop(spa::SPA_PARAM_LATENCY_maxQuantum)?.get_float().ok()?,
            rate: prop(spa::SPA_PARAM_LATENCY_maxRate)?.get_int().ok()?.max(0) as u32,
            ns: prop(spa::SPA_PARAM_LATENCY_maxNs)?.get_long().ok()?.max(0) as u64,
        })
    }
}

/// Links between Dawkey and one device node.
#[derive(Default)]
struct Connection {
    /// The device node the links go to.
    peer: Option<u32>,
    links: Vec<pw::link::Link>,
}

#[derive(Default)]
struct RouterState {
    nodes: HashMap<u32, NodeInfo>,
    ports: HashMap<u32, PortInfo>,
    own_node: Option<u32>,
    default_sink: Option<String>,
    default_source: Option<String>,
    capture: bool,
    midi: MidiRoutes,
    playback: Connection,
    input: Connection,
    /// MIDI links, by (output port, input port).
    midi_links: HashMap<(u32, u32), pw::link::Link>,
    midi_ports: Option<MidiPorts>,
    /// The device each MIDI device port is assigned to: inputs, outputs.
    slots: [[Option<String>; MIDI_DEVICES]; 2],
    metadata: Option<(pw::metadata::Metadata, pw::metadata::MetadataListener)>,
    /// MIDI ports being watched for their latency, and what they report.
    watched: HashMap<u32, (pw::port::Port, pw::port::PortListener)>,
    latency: HashMap<u32, PortLatency>,
}

fn run(
    receiver: pw::channel::Receiver<RouterMessage>,
    status: &Arc<Status>,
) -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;
    let state = Rc::new(RefCell::new(RouterState::default()));

    let _receiver = receiver.attach(mainloop.loop_(), {
        let (state, core, status, mainloop) = (
            Rc::clone(&state),
            core.clone(),
            Arc::clone(status),
            mainloop.clone(),
        );
        move |message| match message {
            RouterMessage::OwnNode(id) => {
                state.borrow_mut().own_node = Some(id);
                reconcile(&state, &core, &status);
            }
            RouterMessage::Capture(on) => {
                state.borrow_mut().capture = on;
                reconcile(&state, &core, &status);
            }
            RouterMessage::Midi(routes) => {
                state.borrow_mut().midi = routes;
                reconcile(&state, &core, &status);
            }
            RouterMessage::Ports(maker) => {
                state.borrow_mut().midi_ports = Some(maker);
                reconcile(&state, &core, &status);
            }
            RouterMessage::Quit => mainloop.quit(),
        }
    });

    let _registry_listener = registry
        .add_listener_local()
        .global({
            let (state, core, status, registry) = (
                Rc::clone(&state),
                core.clone(),
                Arc::clone(status),
                registry.clone(),
            );
            move |global| {
                let Some(props) = global.props else {
                    return;
                };
                let get = |key| props.get(key).unwrap_or_default().to_owned();
                match global.type_ {
                    ObjectType::Node => {
                        state.borrow_mut().nodes.insert(
                            global.id,
                            NodeInfo {
                                name: get("node.name"),
                                media_class: get("media.class"),
                                app: get("application.name"),
                            },
                        );
                    }
                    ObjectType::Port => {
                        let Ok(node) = get("node.id").parse() else {
                            return;
                        };
                        let format = get("format.dsp");
                        let alias = get("port.alias");
                        let port_name = get("port.name");
                        let info = PortInfo {
                            node,
                            output: get("port.direction") == "out",
                            channel: get("audio.channel"),
                            monitor: get("port.monitor") == "true",
                            midi: format.contains("midi") || format.contains("UMP"),
                            name: if alias.is_empty() {
                                port_name.clone()
                            } else {
                                alias
                            },
                            port_name,
                        };
                        let (midi, output) = (info.midi, info.output);
                        state.borrow_mut().ports.insert(global.id, info);
                        if midi {
                            watch_latency(&state, &core, &status, &registry, global, output);
                        }
                    }
                    ObjectType::Metadata if get("metadata.name") == "default" => {
                        let Ok(metadata) = registry.bind::<pw::metadata::Metadata, _>(global)
                        else {
                            return;
                        };
                        let listener = metadata
                            .add_listener_local()
                            .property({
                                let (state, core, status) =
                                    (Rc::clone(&state), core.clone(), Arc::clone(&status));
                                move |_subject, key, _type, value| {
                                    let name = value.and_then(name_from_json);
                                    match key {
                                        Some("default.audio.sink") => {
                                            state.borrow_mut().default_sink = name;
                                        }
                                        Some("default.audio.source") => {
                                            state.borrow_mut().default_source = name;
                                        }
                                        _ => return 0,
                                    }
                                    reconcile(&state, &core, &status);
                                    0
                                }
                            })
                            .register();
                        state.borrow_mut().metadata = Some((metadata, listener));
                        return;
                    }
                    _ => return,
                }
                reconcile(&state, &core, &status);
            }
        })
        .global_remove({
            let (state, core, status) = (Rc::clone(&state), core.clone(), Arc::clone(status));
            move |id| {
                {
                    let s = &mut *state.borrow_mut();
                    s.nodes.remove(&id);
                    s.ports.remove(&id);
                    s.watched.remove(&id);
                    s.latency.remove(&id);
                    // A port went away, and its links with it.
                    s.midi_links.retain(|&(from, to), _| from != id && to != id);
                    for connection in [&mut s.playback, &mut s.input] {
                        if connection.peer == Some(id) {
                            // The device went away, and its links with it.
                            connection.peer = None;
                            connection.links.clear();
                        }
                    }
                }
                reconcile(&state, &core, &status);
            }
        })
        .register();

    mainloop.run();

    // Leaving: drop our links explicitly rather than waiting for the
    // connection to close.
    let s = &mut *state.borrow_mut();
    let links: Vec<_> = s
        .playback
        .links
        .drain(..)
        .chain(s.input.links.drain(..))
        .chain(s.midi_links.drain().map(|(_, link)| link))
        .collect();
    for link in links {
        let _ = core.destroy_object(link);
    }
    Ok(())
}

/// Follows the latency a MIDI port (an `output` one, or an input) reports.
fn watch_latency(
    state: &Rc<RefCell<RouterState>>,
    core: &pw::core::CoreRc,
    status: &Arc<Status>,
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
    output: bool,
) {
    let Ok(port) = registry.bind::<pw::port::Port, _>(global) else {
        return;
    };
    let id = global.id;
    let listener = port
        .add_listener_local()
        .param({
            let (state, core, status) = (Rc::clone(state), core.clone(), Arc::clone(status));
            move |_seq, kind, _index, _next, pod| {
                if kind != pw::spa::param::ParamType::Latency {
                    return;
                }
                let Some(latency) = pod.and_then(|pod| PortLatency::parse(pod, output)) else {
                    return;
                };
                let old = state.borrow_mut().latency.insert(id, latency);
                if old != Some(latency) {
                    reconcile(&state, &core, &status);
                }
            }
        })
        .register();
    port.subscribe_params(&[pw::spa::param::ParamType::Latency]);
    state.borrow_mut().watched.insert(id, (port, listener));
}

#[derive(Clone, Copy)]
enum Direction {
    /// Dawkey's outputs → the peer's inputs.
    ToPeer,
    /// The peer's outputs → Dawkey's inputs.
    FromPeer,
}

/// Makes our links match the current defaults and capture setting.
fn reconcile(state: &Rc<RefCell<RouterState>>, core: &pw::core::CoreRc, status: &Status) {
    let mut s = state.borrow_mut();
    let Some(own) = s.own_node else {
        return;
    };
    let find = |s: &RouterState, name: &Option<String>, class: &str| {
        name.as_ref().and_then(|name| {
            s.nodes
                .iter()
                .find(|(_, n)| &n.name == name && n.media_class.starts_with(class))
                .map(|(&id, _)| id)
        })
    };
    let sink = find(&s, &s.default_sink, "Audio/Sink");
    let source = if s.capture {
        find(&s, &s.default_source, "Audio/Source")
    } else {
        None
    };

    let RouterState {
        ports,
        nodes,
        playback,
        input,
        ..
    } = &mut *s;
    let name = |id: Option<u32>| id.and_then(|id| nodes.get(&id)).map(|n| n.name.clone());
    if connect(core, ports, own, playback, sink, Direction::ToPeer) {
        set_name(&status.linked_to, name(playback.peer));
    }
    if connect(core, ports, own, input, source, Direction::FromPeer) {
        set_name(&status.capture_from, name(input.peer));
    }
    reconcile_midi(&mut s, core, own, status);
}

/// Makes the MIDI links match the routes: wanted devices to (or from)
/// their own Dawkey ports, adding those ports first if needed.
fn reconcile_midi(s: &mut RouterState, core: &pw::core::CoreRc, own: u32, status: &Status) {
    // (port, name, is an app).
    let devices = |input: bool| -> Vec<(u32, String, bool)> {
        let mut list: Vec<(u32, String, bool)> = s
            .ports
            .iter()
            .filter(|(_, p)| p.output != input)
            .filter_map(|(&id, p)| {
                if is_midi_device(p, &s.nodes) {
                    Some((id, p.name.clone(), false))
                } else {
                    midi_app_port(p, &s.nodes, own).map(|name| (id, name, true))
                }
            })
            .collect();
        list.sort();
        list
    };
    // Devices we can take MIDI from have output ports, and the other way.
    let (sources, sinks) = (devices(false), devices(true));
    // Which devices go through which port. A wanted device keeps its slot;
    // a new one takes a free slot, or one whose device isn't wanted now.
    let wanted_in: Vec<String> = sources
        .iter()
        .filter(|(_, d, app)| match &s.midi.capture {
            MidiCaptureRoute::Off => false,
            MidiCaptureRoute::All => !app,
            MidiCaptureRoute::Devices(names) => names.contains(d),
        })
        .map(|(_, d, _)| d.clone())
        .collect();
    let wanted_out: Vec<String> = sinks
        .iter()
        .map(|(_, d, _)| d.clone())
        .filter(|d| s.midi.outputs.contains(d))
        .collect();
    for (output, devices) in [(false, &wanted_in), (true, &wanted_out)] {
        let slots = &mut s.slots[usize::from(output)];
        for device in devices {
            if slots.iter().any(|d| d.as_ref() == Some(device)) {
                continue;
            }
            let free = slots.iter().position(Option::is_none).or_else(|| {
                slots
                    .iter()
                    .position(|d| d.as_ref().is_some_and(|d| !devices.contains(d)))
            });
            let Some(slot) = free else {
                tracing::warn!("no MIDI port left for {device}");
                continue;
            };
            slots[slot] = Some(device.clone());
            if let Some(ports) = &s.midi_ports {
                ports.assign(slot, output, device);
            }
        }
    }
    let own_port = |device: &str, output: bool| {
        let slot = s.slots[usize::from(output)]
            .iter()
            .position(|d| d.as_deref() == Some(device))?;
        let name = MidiPorts::port_name(slot, output);
        s.ports
            .iter()
            .find(|(_, p)| p.node == own && p.output == output && p.midi && p.port_name == name)
            .map(|(&id, _)| id)
    };
    let mut wanted = Vec::new();
    for (port, device, _) in &sources {
        if wanted_in.contains(device)
            && let Some(ours) = own_port(device, false)
        {
            wanted.push((*port, ours));
        }
    }
    for (port, device, _) in &sinks {
        if wanted_out.contains(device)
            && let Some(ours) = own_port(device, true)
        {
            wanted.push((ours, *port));
        }
    }
    let stale: Vec<(u32, u32)> = s
        .midi_links
        .keys()
        .copied()
        .filter(|pair| !wanted.contains(pair))
        .collect();
    for pair in stale {
        if let Some(link) = s.midi_links.remove(&pair) {
            let _ = core.destroy_object(link);
        }
    }
    for (from, to) in wanted {
        if s.midi_links.contains_key(&(from, to)) {
            continue;
        }
        let props = pw::properties::properties! {
            "link.output.node" => s.ports[&from].node.to_string(),
            "link.output.port" => from.to_string(),
            "link.input.node" => s.ports[&to].node.to_string(),
            "link.input.port" => to.to_string(),
            "object.linger" => "false",
        };
        match core.create_object::<pw::link::Link>("link-factory", &props) {
            Ok(link) => {
                s.midi_links.insert((from, to), link);
            }
            Err(e) => tracing::warn!("linking MIDI port {from} → {to} failed: {e}"),
        }
    }
    let name_of = |port: &u32| {
        sources
            .iter()
            .chain(&sinks)
            .find(|(id, ..)| id == port)
            .map_or_else(|| s.ports[port].name.clone(), |(_, n, _)| n.clone())
    };
    let names = |ids: Vec<u32>| {
        let mut names: Vec<String> = ids.iter().map(name_of).collect();
        names.sort();
        names.dedup();
        names
    };
    let linked_from = names(
        s.midi_links
            .keys()
            .map(|&(from, _)| from)
            .filter(|p| s.ports[p].node != own)
            .collect(),
    );
    let linked_to = names(
        s.midi_links
            .keys()
            .map(|&(_, to)| to)
            .filter(|p| s.ports[p].node != own)
            .collect(),
    );
    let available = |list: &[(u32, String, bool)]| {
        let mut names: Vec<String> = list.iter().map(|(_, n, _)| n.clone()).collect();
        names.dedup();
        names
    };
    let latency = |list: &[(u32, String, bool)]| -> Vec<(String, PortLatency)> {
        list.iter()
            .filter_map(|(id, name, _)| s.latency.get(id).map(|l| (name.clone(), *l)))
            .collect()
    };
    for (slot, value) in [
        (&status.midi_input_latency, latency(&sources)),
        (&status.midi_output_latency, latency(&sinks)),
    ] {
        if let Ok(mut slot) = slot.lock() {
            *slot = value;
        }
    }
    for (slot, value) in [
        (&status.midi_from, linked_from),
        (&status.midi_to, linked_to),
        (&status.midi_inputs, available(&sources)),
        (&status.midi_outputs, available(&sinks)),
    ] {
        if let Ok(mut slot) = slot.lock() {
            *slot = value;
        }
    }
}

/// A MIDI port of a device: on a MIDI bridge node, and not the ALSA "Midi
/// Through" loopback (which would echo our own output back).
fn is_midi_device(port: &PortInfo, nodes: &HashMap<u32, NodeInfo>) -> bool {
    port.midi
        && !port.name.starts_with("Midi Through")
        && nodes
            .get(&port.node)
            .is_some_and(|n| n.media_class.starts_with("Midi/Bridge"))
}

/// The name of another app's MIDI port, as tracks pick it: `node:port`
/// (or the port's alias, if it has one with a `:`). Not our own ports, not
/// devices, and not the per-device ports of other Dawkey instances.
fn midi_app_port(port: &PortInfo, nodes: &HashMap<u32, NodeInfo>, own: u32) -> Option<String> {
    let node = nodes.get(&port.node)?;
    if !port.midi
        || port.monitor
        || port.node == own
        || node.media_class.starts_with("Midi/Bridge")
        || (node.app == "Dawkey" && is_pool_port(&port.port_name))
    {
        return None;
    }
    Some(if port.name.contains(':') {
        port.name.clone()
    } else {
        format!("{}:{}", node.name, port.port_name)
    })
}

/// A Dawkey device port: `midi_in_N` or `midi_out_N`.
fn is_pool_port(name: &str) -> bool {
    ["midi_in_", "midi_out_"].iter().any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Points `connection` at `target`, relinking if it changed. Returns true
/// if the connection changed.
fn connect(
    core: &pw::core::CoreRc,
    ports: &HashMap<u32, PortInfo>,
    own: u32,
    connection: &mut Connection,
    target: Option<u32>,
    direction: Direction,
) -> bool {
    if target == connection.peer && (target.is_none() || !connection.links.is_empty()) {
        return false;
    }
    for link in connection.links.drain(..) {
        let _ = core.destroy_object(link);
    }
    connection.peer = None;
    let Some(peer) = target else {
        return true;
    };
    let (from_node, to_node) = match direction {
        Direction::ToPeer => (own, peer),
        Direction::FromPeer => (peer, own),
    };
    let pairs = port_pairs(ports, from_node, to_node);
    if pairs.is_empty() {
        // Ports not announced yet; a later registry event retries.
        return true;
    }
    for (out_port, in_port) in pairs {
        let props = pw::properties::properties! {
            "link.output.node" => from_node.to_string(),
            "link.output.port" => out_port.to_string(),
            "link.input.node" => to_node.to_string(),
            "link.input.port" => in_port.to_string(),
            "object.linger" => "false",
        };
        match core.create_object::<pw::link::Link>("link-factory", &props) {
            Ok(link) => connection.links.push(link),
            Err(e) => tracing::warn!("linking {from_node} → {to_node} failed: {e}"),
        }
    }
    connection.peer = Some(peer);
    tracing::debug!("linked {from_node} → {to_node}");
    true
}

/// Pairs `from`'s output ports with `to`'s input ports: by channel name
/// when every input finds one, otherwise in port order, repeating the
/// smaller side (a mono device feeds or takes both channels).
fn port_pairs(ports: &HashMap<u32, PortInfo>, from: u32, to: u32) -> Vec<(u32, u32)> {
    let sorted = |node: u32, output: bool| {
        let mut list: Vec<(u32, &PortInfo)> = ports
            .iter()
            .filter(|(_, p)| p.node == node && p.output == output && !p.monitor && !p.midi)
            .map(|(&id, p)| (id, p))
            .collect();
        list.sort_by_key(|&(id, _)| id);
        list
    };
    let outs = sorted(from, true);
    let ins = sorted(to, false);
    if outs.is_empty() || ins.is_empty() {
        return Vec::new();
    }
    let by_channel: Vec<(u32, u32)> = ins
        .iter()
        .filter_map(|(i, q)| {
            outs.iter()
                .find(|(_, p)| p.channel == q.channel)
                .map(|(o, _)| (*o, *i))
        })
        .collect();
    if by_channel.len() == ins.len() && by_channel.len() >= outs.len() {
        return by_channel;
    }
    let n = outs.len().max(ins.len());
    (0..n)
        .map(|k| (outs[k % outs.len()].0, ins[k % ins.len()].0))
        .collect()
}

fn set_name(slot: &Mutex<Option<String>>, name: Option<String>) {
    if let Ok(mut slot) = slot.lock() {
        *slot = name;
    }
}

/// Parses WirePlumber's `{"name": "..."}` metadata value.
fn name_from_json(value: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(value).ok()?;
    parsed.get("name")?.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(node: u32, output: bool, channel: &str) -> PortInfo {
        PortInfo {
            node,
            output,
            channel: channel.into(),
            monitor: false,
            midi: false,
            name: String::new(),
            port_name: String::new(),
        }
    }

    fn sorted(mut v: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
        v.sort();
        v
    }

    #[test]
    fn pairs_ports_by_channel_then_order() {
        let mut ports = HashMap::from([
            (10, port(1, true, "FL")),
            (11, port(1, true, "FR")),
            (20, port(2, false, "FR")),
            (21, port(2, false, "FL")),
            (
                22,
                PortInfo {
                    monitor: true,
                    ..port(2, true, "FL")
                },
            ),
        ]);
        assert_eq!(sorted(port_pairs(&ports, 1, 2)), [(10, 21), (11, 20)]);

        // MIDI ports are not audio.
        ports.insert(
            23,
            PortInfo {
                midi: true,
                ..port(2, false, "")
            },
        );
        assert_eq!(sorted(port_pairs(&ports, 1, 2)), [(10, 21), (11, 20)]);
        ports.remove(&23);

        // A mono sink gets both channels.
        ports.retain(|&id, _| id < 20);
        ports.insert(30, port(3, false, "MONO"));
        assert_eq!(sorted(port_pairs(&ports, 1, 3)), [(10, 30), (11, 30)]);

        // A mono source feeds both inputs.
        ports.insert(40, port(4, true, "MONO"));
        ports.insert(50, port(5, false, "FL"));
        ports.insert(51, port(5, false, "FR"));
        assert_eq!(sorted(port_pairs(&ports, 4, 5)), [(40, 50), (40, 51)]);
    }

    #[test]
    fn picks_midi_devices() {
        let nodes = HashMap::from([
            (
                1,
                NodeInfo {
                    name: "Midi-Bridge".into(),
                    media_class: "Midi/Bridge".into(),
                    app: String::new(),
                },
            ),
            (
                2,
                NodeInfo {
                    name: "some-app".into(),
                    media_class: "Stream/Output/Audio".into(),
                    app: "Some App".into(),
                },
            ),
            (
                3,
                NodeInfo {
                    name: "dawkey".into(),
                    media_class: "Stream/Output/Audio".into(),
                    app: "Dawkey".into(),
                },
            ),
        ]);
        let midi = |node, output, name: &str| PortInfo {
            midi: true,
            name: name.into(),
            ..port(node, output, "")
        };
        assert!(is_midi_device(
            &midi(1, true, "Keystation:Keystation MIDI 1"),
            &nodes
        ));
        assert!(is_midi_device(&midi(1, false, "Keystation"), &nodes));
        assert!(!is_midi_device(
            &midi(1, true, "Midi Through:Midi Through Port-0"),
            &nodes
        ));
        assert!(!is_midi_device(&midi(2, true, "app out"), &nodes));
        assert!(!is_midi_device(&port(1, true, "FL"), &nodes));

        // Other apps' MIDI ports, named node:port.
        let named = |node, output, name: &str| PortInfo {
            port_name: name.into(),
            ..midi(node, output, name)
        };
        assert_eq!(
            midi_app_port(&named(2, false, "midi_in"), &nodes, 99).as_deref(),
            Some("some-app:midi_in")
        );
        assert_eq!(
            midi_app_port(&named(2, true, "synth:out"), &nodes, 99).as_deref(),
            Some("synth:out")
        );
        // Not devices, not our own node, not audio, not another Dawkey's
        // device ports (its generic input is fine).
        assert_eq!(midi_app_port(&named(1, true, "Keys"), &nodes, 99), None);
        assert_eq!(midi_app_port(&named(2, false, "midi_in"), &nodes, 2), None);
        assert_eq!(midi_app_port(&port(2, false, "FL"), &nodes, 99), None);
        assert_eq!(
            midi_app_port(&named(3, false, "midi_in_3"), &nodes, 99),
            None
        );
        assert_eq!(
            midi_app_port(&named(3, false, "midi_in"), &nodes, 99).as_deref(),
            Some("dawkey:midi_in")
        );
    }

    /// A Latency param as PipeWire sends it.
    fn latency_pod(direction: u32, quantum: f32, rate: i32, ns: i64) -> Vec<u8> {
        use libspa_sys as spa;
        use pw::spa::pod::{Object, Property, Value, serialize::PodSerializer};
        use pw::spa::utils::Id;
        let object = Object {
            type_: spa::SPA_TYPE_OBJECT_ParamLatency,
            id: spa::SPA_PARAM_Latency,
            properties: vec![
                Property::new(spa::SPA_PARAM_LATENCY_direction, Value::Id(Id(direction))),
                Property::new(spa::SPA_PARAM_LATENCY_minQuantum, Value::Float(quantum)),
                Property::new(spa::SPA_PARAM_LATENCY_maxQuantum, Value::Float(quantum)),
                Property::new(spa::SPA_PARAM_LATENCY_minRate, Value::Int(rate)),
                Property::new(spa::SPA_PARAM_LATENCY_maxRate, Value::Int(rate)),
                Property::new(spa::SPA_PARAM_LATENCY_minNs, Value::Long(ns)),
                Property::new(spa::SPA_PARAM_LATENCY_maxNs, Value::Long(ns)),
            ],
        };
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(object))
            .unwrap()
            .0
            .into_inner()
    }

    #[test]
    fn reads_the_latency_of_ports() {
        let input = latency_pod(libspa_sys::SPA_DIRECTION_INPUT, 1.0, 32, 500_000);
        let pod = pw::spa::pod::Pod::from_bytes(&input).unwrap();
        let latency = PortLatency::parse(pod, false).unwrap();
        assert_eq!(
            latency,
            PortLatency {
                quantum: 1.0,
                rate: 32,
                ns: 500_000
            }
        );
        // A 256-frame cycle and 32 frames at 48 kHz, and half a millisecond.
        assert!((latency.ms(256, 48_000) - 6.5).abs() < 1e-9);
        // Before the rate is known, only the time part counts.
        assert!((latency.ms(0, 0) - 0.5).abs() < 1e-9);
        // The output direction is for MIDI coming out of a device's output
        // ports, and the input direction doesn't count there.
        let output = latency_pod(libspa_sys::SPA_DIRECTION_OUTPUT, 2.0, 0, 0);
        let pod = pw::spa::pod::Pod::from_bytes(&output).unwrap();
        assert_eq!(PortLatency::parse(pod, false), None);
        assert_eq!(PortLatency::parse(pod, true).unwrap().quantum, 2.0);
        let pod = pw::spa::pod::Pod::from_bytes(&input).unwrap();
        assert_eq!(PortLatency::parse(pod, true), None);
    }

    #[test]
    fn parses_default_device_metadata() {
        assert_eq!(
            name_from_json(r#"{ "name": "alsa_output.usb" }"#).as_deref(),
            Some("alsa_output.usb")
        );
        assert_eq!(name_from_json("garbage"), None);
    }
}
