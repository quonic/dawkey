// SPDX-License-Identifier: AGPL-3.0-or-later
//! Built-in DSP exposed through the plugin catalog and processor interface:
//! the studio guitar rack effects, and the synthesized drum kit instrument.

use std::any::Any;
use std::path::PathBuf;
use std::sync::LazyLock;

use dawkey_dsp::{
    BuiltinSpec, DrumEngine, DrumEvent, DrumEventKind, PAD_NAMES, RackEffect, RackProcessor,
    builtin_specs,
};
use dawkey_engine::{
    MAX_BLOCK, MidiSink, PluginEvent, PluginEventKind, PluginProcessor, ProcessContext,
};

use crate::{ParamInfo, PluginError, PluginFormat, PluginInfo, ValueLabel};

/// `builtin:drums`' plugin ID, for anything (e.g. `AddDrumTrack`) that needs
/// to name it without hardcoding the string.
pub const DRUMS_ID: &str = "builtin:drums";

static BUILTIN_INFOS: LazyLock<Vec<PluginInfo>> = LazyLock::new(|| {
    builtin_specs()
        .iter()
        .map(to_plugin_info)
        .chain(std::iter::once(drum_plugin_info()))
        .collect()
});

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

/// Default per-pad `(tone, decay_ms, level_db)`, matching
/// [`DrumEngine::new`]'s own defaults.
const PAD_PARAM_DEFAULTS: [(f64, f64, f64); 8] = [
    (0.5, 320.0, 0.0),
    (0.5, 180.0, 0.0),
    (0.5, 70.0, -3.0),
    (0.5, 320.0, -5.0),
    (0.5, 260.0, -2.0),
    (0.5, 130.0, -2.0),
    (0.5, 1400.0, -6.0),
    (0.5, 900.0, -7.0),
];

fn drum_plugin_info() -> PluginInfo {
    let mut params = Vec::with_capacity(24);
    for (i, name) in PAD_NAMES.iter().enumerate() {
        let slug = name.to_ascii_lowercase().replace(' ', "_");
        let (tone, decay_ms, level_db) = PAD_PARAM_DEFAULTS[i];
        params.push(ParamInfo {
            id: format!("{slug}_tone"),
            name: format!("{name} Tone"),
            min: 0.0,
            max: 1.0,
            default: tone,
            stepped: false,
            unit: None,
            labels: Vec::new(),
        });
        params.push(ParamInfo {
            id: format!("{slug}_decay"),
            name: format!("{name} Decay"),
            min: 5.0,
            max: 4000.0,
            default: decay_ms,
            stepped: false,
            unit: Some("ms".to_owned()),
            labels: Vec::new(),
        });
        params.push(ParamInfo {
            id: format!("{slug}_level"),
            name: format!("{name} Level"),
            min: -36.0,
            max: 6.0,
            default: level_db,
            stepped: false,
            unit: Some("dB".to_owned()),
            labels: Vec::new(),
        });
    }
    PluginInfo {
        id: DRUMS_ID.to_owned(),
        format: PluginFormat::Builtin,
        name: "DK-8 Synth Drum Kit".to_owned(),
        vendor: "Dawkey Studio".to_owned(),
        path: PathBuf::new(),
        instrument: true,
        midi_output: false,
        audio_inputs: 0,
        audio_outputs: 2,
        sidechain: false,
        outputs: Vec::new(),
        params,
        properties: Vec::new(),
    }
}

/// Every built-in DSP unit (rack effects and the drum kit instrument).
pub fn builtin_plugins() -> &'static [PluginInfo] {
    &BUILTIN_INFOS
}

/// Looks up a built-in DSP unit by ID (`"builtin:..."`).
pub fn builtin_plugin(id: &str) -> Option<&'static PluginInfo> {
    BUILTIN_INFOS.iter().find(|p| p.id == id)
}

#[derive(Debug)]
pub(crate) enum BuiltinProcessor {
    Rack(Box<RackProcessor>),
    Drums(Box<DrumEngine>),
}

pub(crate) fn load(info: &PluginInfo, sample_rate: u32) -> Result<BuiltinProcessor, PluginError> {
    if info.id == DRUMS_ID {
        return Ok(BuiltinProcessor::Drums(Box::new(DrumEngine::new(
            sample_rate,
        ))));
    }
    let effect = RackEffect::from_id(&info.id)
        .ok_or_else(|| PluginError::new(format!("unknown built-in plugin {:?}", info.id)))?;
    Ok(BuiltinProcessor::Rack(Box::new(RackProcessor::new(
        effect,
        sample_rate,
    ))))
}

fn to_drum_event(e: &PluginEvent) -> Option<DrumEvent> {
    let kind = match e.kind {
        PluginEventKind::NoteOn { key, velocity, .. } => DrumEventKind::NoteOn { key, velocity },
        PluginEventKind::NoteOff { key, .. } => DrumEventKind::NoteOff { key },
        PluginEventKind::Param { index, value } => DrumEventKind::Param { index, value },
        PluginEventKind::Midi { .. } | PluginEventKind::PerNote { .. } => return None,
    };
    Some(DrumEvent {
        offset: e.offset,
        kind,
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
        match self {
            BuiltinProcessor::Rack(dsp) => {
                let bpm = if context.transport.bpm > 0.0 {
                    context.transport.bpm
                } else {
                    120.0
                };
                let mut params = 0usize;
                let mut from = 0usize;
                while from < frames {
                    while let Some(event) = events.get(params).filter(|e| e.offset as usize <= from)
                    {
                        if let PluginEventKind::Param { index, value } = event.kind {
                            dsp.set_param(index as usize, value);
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
                    dsp.process(&mut left[from..to], &mut right[from..to], sc, bpm);
                    from = to;
                }
            }
            BuiltinProcessor::Drums(dsp) => {
                const CAP: usize = 256;
                let mut buf = [DrumEvent {
                    offset: 0,
                    kind: DrumEventKind::NoteOff { key: 0 },
                }; CAP];
                let mut n = 0usize;
                for e in events {
                    if n >= CAP {
                        break;
                    }
                    if let Some(d) = to_drum_event(e) {
                        buf[n] = d;
                        n += 1;
                    }
                }
                dsp.process(&mut left[..frames], &mut right[..frames], &buf[..n]);
            }
        }
    }

    fn stop(&mut self) {}

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}
