// SPDX-License-Identifier: AGPL-3.0-or-later
//! VST3 hosting, through the `vst3` crate's bindings.
//!
//! A `.vst3` bundle's binary is loaded once per process ([`module`]) and
//! each plugin made from its factory ([`plugin`]): the component and its
//! edit controller, connected. The control thread keeps a
//! [`Vst3Instance`], the audio thread a [`Vst3Processor`]. Parameter values
//! in the catalog are VST3's normalized ones (0 to 1; 0 to the step count
//! for stepped parameters), so the plugin's own text is what users see.

mod host;
mod instance;
mod module;
mod plugin;
mod processor;

use std::ffi::c_char;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use vst3::ComPtr;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;

use crate::{OutputInfo, ParamInfo, PluginError, PluginFormat, PluginInfo};
use dawkey_engine::MAX_BLOCK;

pub(crate) use instance::Vst3Instance;
#[cfg(test)]
pub(crate) use instance::{split_state, state_blob};
pub(crate) use processor::Vst3Processor;

use plugin::Plugin;

/// A bundle's binary for this machine: `Contents/<arch>-linux/<name>.so`.
pub(crate) fn bundle_binary(bundle: &Path) -> PathBuf {
    let name = bundle
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    bundle
        .join("Contents")
        .join(format!("{}-linux", std::env::consts::ARCH))
        .join(format!("{name}.so"))
}

/// Whether a VST3 call succeeded (`kResultOk`, which is `kResultTrue`).
pub(crate) fn ok(result: tresult) -> bool {
    result == kResultOk
}

/// A UTF-16 string from a fixed-size VST3 field, up to its NUL.
pub(crate) fn read_wstring(s: &[TChar]) -> String {
    let len = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..len])
}

/// Writes `text` into a fixed-size VST3 field, cut to fit, NUL-terminated.
pub(crate) fn write_wstring(text: &str, s: &mut [TChar]) {
    let mut n = 0;
    for (unit, slot) in text.encode_utf16().zip(s.iter_mut()) {
        *slot = unit;
        n += 1;
    }
    let end = n.min(s.len().saturating_sub(1));
    if let Some(slot) = s.get_mut(end) {
        *slot = 0;
    }
}

/// A UTF-8 string from a fixed-size VST3 field, up to its NUL.
fn read_cstring(s: &[c_char]) -> String {
    let bytes: Vec<u8> = s
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

/// A class ID as Dawkey writes it: 32 uppercase hex digits, its bytes in
/// order.
pub(crate) fn cid_hex(cid: &TUID) -> String {
    cid.iter().map(|&b| format!("{:02X}", b as u8)).collect()
}

fn parse_cid(id: &str) -> Option<TUID> {
    let hex = id.strip_prefix("vst3:").unwrap_or(id);
    if hex.len() != 32 || !hex.is_ascii() {
        return None;
    }
    let mut cid: TUID = [0; 16];
    for (i, byte) in cid.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()? as c_char;
    }
    Some(cid)
}

/// Every parameter's ID, and the ones a catalog shows, described: those
/// that can be automated (or pick a program), and aren't hidden, read-only
/// or bypass. (That leaves out the stand-ins for MIDI controllers JUCE and
/// Dplug plugins have by the thousand: MIDI reaches them through
/// `IMidiMapping`.)
fn params(controller: Option<&ComPtr<IEditController>>) -> (Vec<ParamID>, Vec<ParamInfo>) {
    use ParameterInfo_::ParameterFlags_::*;
    let Some(controller) = controller else {
        return (Vec::new(), Vec::new());
    };
    let mut all = Vec::new();
    let mut shown = Vec::new();
    // SAFETY: plain queries.
    let count = unsafe { controller.getParameterCount() };
    for i in 0..count.max(0) {
        let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
        if !ok(unsafe { controller.getParameterInfo(i, &mut info) }) {
            continue;
        }
        all.push(info.id);
        if info.flags & (kIsHidden | kIsReadOnly | kIsBypass) != 0
            || info.flags & (kCanAutomate | kIsProgramChange) == 0
        {
            continue;
        }
        let steps = info.stepCount.max(0);
        let max = if steps > 0 { f64::from(steps) } else { 1.0 };
        let default = info.defaultNormalizedValue.clamp(0.0, 1.0) * max;
        let mut name = read_wstring(&info.title).trim().to_owned();
        if name.is_empty() {
            name = read_wstring(&info.shortTitle).trim().to_owned();
        }
        shown.push(ParamInfo {
            id: info.id.to_string(),
            name,
            min: 0.0,
            max,
            default: if steps > 0 { default.round() } else { default },
            stepped: steps > 0,
            unit: None,
            labels: Vec::new(),
        });
    }
    (all, shown)
}

/// Describes every plugin in a `.vst3` bundle. Runs plugin code.
pub(crate) fn scan_bundle(path: &Path) -> Result<Vec<PluginInfo>, PluginError> {
    let module = module::load(path)?;
    let factory = &module.factory;
    let mut factory_info: PFactoryInfo = unsafe { std::mem::zeroed() };
    // SAFETY: plain queries into structs of ours.
    let factory_vendor = if ok(unsafe { factory.getFactoryInfo(&mut factory_info) }) {
        read_cstring(&factory_info.vendor)
    } else {
        String::new()
    };
    let factory2 = factory.cast::<IPluginFactory2>();
    let mut found = Vec::new();
    for i in 0..unsafe { factory.countClasses() }.max(0) {
        let mut class: PClassInfo = unsafe { std::mem::zeroed() };
        if !ok(unsafe { factory.getClassInfo(i, &mut class) }) {
            continue;
        }
        if read_cstring(&class.category) != "Audio Module Class" {
            continue;
        }
        let mut vendor = factory_vendor.clone();
        if let Some(factory2) = &factory2 {
            let mut class2: PClassInfo2 = unsafe { std::mem::zeroed() };
            if ok(unsafe { factory2.getClassInfo2(i, &mut class2) }) {
                let v = read_cstring(&class2.vendor);
                if !v.is_empty() {
                    vendor = v;
                }
            }
        }
        let name = read_cstring(&class.name);
        let plugin = match Plugin::create(Arc::clone(&module), &class.cid) {
            Ok(plugin) => plugin,
            Err(e) => {
                tracing::warn!("{} ({name}): {e}", path.display());
                continue;
            }
        };
        plugin.arrange_buses();
        let (ins, events_in) = plugin.buses(BusDirections_::kInput as i32);
        let (outs, events_out) = plugin.buses(BusDirections_::kOutput as i32);
        let (_, params) = params(plugin.controller.as_ref());
        let id = cid_hex(&class.cid);
        found.push(PluginInfo {
            id: format!("vst3:{id}"),
            format: PluginFormat::Vst3,
            name: if name.is_empty() { id } else { name },
            vendor,
            path: path.to_owned(),
            instrument: events_in,
            midi_output: events_out,
            audio_inputs: ins.main_channels(),
            audio_outputs: outs.main_channels(),
            sidechain: ins.others().next().is_some(),
            outputs: outs
                .others()
                .map(|i| OutputInfo {
                    name: ins_name(&outs.names[i], i),
                    channels: outs.channels[i],
                })
                .collect(),
            params,
        });
    }
    Ok(found)
}

fn ins_name(name: &str, index: usize) -> String {
    if name.is_empty() {
        format!("Output {}", index + 1)
    } else {
        name.to_owned()
    }
}

/// Instantiates and activates `info`'s plugin, restoring `state` first.
pub(crate) fn load(
    info: &PluginInfo,
    sample_rate: u32,
    state: Option<&[u8]>,
    offline: bool,
) -> Result<(Vst3Instance, Vst3Processor, u32), PluginError> {
    let error = |what: String| PluginError::new(format!("{}: {what}", info.id));
    let cid = parse_cid(&info.id).ok_or_else(|| error("not a VST3 class ID".to_owned()))?;
    let module = module::load(&info.path)?;
    let plugin = Plugin::create(module, &cid).map_err(|e| error(e.0))?;
    plugin.arrange_buses();
    match state.map(|s| instance::split_state(s).ok_or("it isn't a VST3 state")) {
        Some(Ok((component, controller))) => {
            if let Err(e) = plugin.restore_state(component, controller) {
                // The plugin still works, as it starts.
                tracing::warn!("{}: restoring its state failed: {e}", info.id);
                plugin.sync_controller();
            }
        }
        Some(Err(e)) => {
            tracing::warn!("{}: restoring its state failed: {e}", info.id);
            plugin.sync_controller();
        }
        None => plugin.sync_controller(),
    }
    let (ins, _) = plugin.buses(BusDirections_::kInput as i32);
    let (outs, _) = plugin.buses(BusDirections_::kOutput as i32);
    // SAFETY: plain calls, in the order VST3 sets plugins up.
    unsafe {
        if !ok(plugin
            .processor
            .canProcessSampleSize(SymbolicSampleSizes_::kSample32 as i32))
        {
            return Err(error("it doesn't process 32-bit samples".to_owned()));
        }
        let mut setup = ProcessSetup {
            processMode: if offline {
                ProcessModes_::kOffline as i32
            } else {
                ProcessModes_::kRealtime as i32
            },
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            maxSamplesPerBlock: MAX_BLOCK as i32,
            sampleRate: f64::from(sample_rate),
        };
        if !ok(plugin.processor.setupProcessing(&mut setup)) {
            return Err(error("setting up processing failed".to_owned()));
        }
    }
    plugin.activate_buses();
    let (all, _) = params(plugin.controller.as_ref());
    let catalog = info
        .params
        .iter()
        .map(|p| p.id.parse::<ParamID>().map(|id| (id, p.max.max(1.0))))
        .collect::<Result<Box<[_]>, _>>()
        .map_err(|_| error("bad parameter id in the catalog".to_owned()))?;
    plugin.start().map_err(|e| error(e.0))?;
    // SAFETY: a plain query, once active.
    let latency = unsafe { plugin.processor.getLatencySamples() };
    // What it said while loading (JUCE restarts as it takes its state)
    // isn't news.
    plugin.handler.clear();
    let plugin = Arc::new(plugin);
    let (processor, links) = Vst3Processor::new(
        Arc::clone(&plugin),
        &ins,
        &outs,
        catalog.clone(),
        &all,
        offline,
        sample_rate,
    );
    let stepped = info.params.iter().map(|p| p.stepped).collect();
    let instance = Vst3Instance::new(plugin, catalog, stepped, links);
    Ok((instance, processor, latency))
}
