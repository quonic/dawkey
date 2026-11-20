// SPDX-License-Identifier: AGPL-3.0-or-later
//! Built-in studio guitar rack DSP effects exposed through the plugin catalog
//! and processor interface.

use std::any::Any;
use std::path::PathBuf;
use std::sync::LazyLock;

use dawkey_dsp::{BuiltinSpec, RackEffect, RackProcessor, builtin_specs};
use dawkey_engine::{
    MAX_BLOCK, MidiSink, PluginEvent, PluginEventKind, PluginProcessor, ProcessContext,
};

use crate::{ParamInfo, PluginError, PluginFormat, PluginInfo, ValueLabel};

static BUILTIN_INFOS: LazyLock<Vec<PluginInfo>> =
    LazyLock::new(|| builtin_specs().iter().map(to_plugin_info).collect());

fn to_plugin_info(spec: &BuiltinSpec) -> PluginInfo {
    PluginInfo {
        id: spec.id.to_owned(),
        format: PluginFormat::Builtin,
        name: spec.name.to_owned(),
        vendor: spec.vendor.to_owned(),
        path: PathBuf::new(),
        instrument: false,
        midi_output: false,
        audio_inputs: 2,
        audio_outputs: 2,
        sidechain: spec.sidechain,
        outputs: Vec::new(),
        params: spec
            .params
            .iter()
            .map(|p| ParamInfo {
                id: p.id.to_owned(),
                name: p.name.to_owned(),
                min: p.min,
                max: p.max,
                default: p.default,
                stepped: p.stepped,
                unit: p.unit.map(str::to_owned),
                labels: p
                    .choices
                    .iter()
                    .map(|c| ValueLabel {
                        value: c.value,
                        label: c.label.to_owned(),
                    })
                    .collect(),
            })
            .collect(),
        properties: Vec::new(),
    }
}

/// Every built-in rack DSP unit, in signal-chain order.
pub fn builtin_plugins() -> &'static [PluginInfo] {
    &BUILTIN_INFOS
}

/// Looks up a built-in rack DSP unit by ID (`"builtin:..."`).
pub fn builtin_plugin(id: &str) -> Option<&'static PluginInfo> {
    BUILTIN_INFOS.iter().find(|p| p.id == id)
}

#[derive(Debug)]
pub(crate) struct BuiltinProcessor {
    dsp: RackProcessor,
}

pub(crate) fn load(info: &PluginInfo, sample_rate: u32) -> Result<BuiltinProcessor, PluginError> {
    let effect = RackEffect::from_id(&info.id)
        .ok_or_else(|| PluginError::new(format!("unknown built-in plugin {:?}", info.id)))?;
    Ok(BuiltinProcessor {
        dsp: RackProcessor::new(effect, sample_rate),
    })
}

impl PluginProcessor for BuiltinProcessor {
    fn process(
        &mut self,
        audio: [&mut [f32]; 2],
        context: ProcessContext,
        events: &[PluginEvent],
        _midi_out: MidiSink,
    ) {
        let frames = audio[0].len().min(audio[1].len()).min(MAX_BLOCK);
        let [left, right] = audio;
        let bpm = if context.transport.bpm > 0.0 {
            context.transport.bpm
        } else {
            120.0
        };
        let mut params = 0usize;
        let mut from = 0usize;
        while from < frames {
            while let Some(event) = events.get(params).filter(|e| e.offset as usize <= from) {
                if let PluginEventKind::Param { index, value } = event.kind {
                    self.dsp.set_param(index as usize, value);
                }
                params += 1;
            }
            let to = events[params..]
                .iter()
                .find(|e| matches!(e.kind, PluginEventKind::Param { .. }))
                .map_or(frames, |e| (e.offset as usize).clamp(from + 1, frames));
            let sc = context
                .sidechain
                .map(|[sl, sr]| [&sl[from..to], &sr[from..to]]);
            self.dsp
                .process(&mut left[from..to], &mut right[from..to], sc, bpm);
            from = to;
        }
    }

    fn stop(&mut self) {}

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}
