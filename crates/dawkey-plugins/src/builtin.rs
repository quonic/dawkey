// SPDX-License-Identifier: AGPL-3.0-or-later
//! Built-in DSP exposed through the plugin catalog and processor interface:
//! the studio guitar rack effects, and the synthesized drum kit instrument.

use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use dawkey_dsp::{
    BuiltinSpec, DrumEngine, DrumEvent, DrumEventKind, PAD_NAMES, RackEffect, RackProcessor,
    SampleKitData, builtin_specs,
};
use dawkey_engine::{
    MAX_BLOCK, MidiSink, PluginEvent, PluginEventKind, PluginProcessor, ProcessContext,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ParamInfo, PluginError, PluginFormat, PluginInfo, ValueLabel};

/// A real hand-clap (`assets/drums/shared/README.md`), shared by every
/// VCSL-sourced kit below instead of each embedding its own copy.
const VCSL_CLAP: &[u8] = include_bytes!("../assets/drums/shared/clap.flac");
/// `acoustic_vcsl`'s cymbals, reused by kits that only source different
/// drums (cymbals don't really have a genre or an era).
const VCSL_CLOSED_HAT: &[u8] = include_bytes!("../assets/drums/acoustic_vcsl/closed_hat.flac");
const VCSL_OPEN_HAT: &[u8] = include_bytes!("../assets/drums/acoustic_vcsl/open_hat.flac");
const VCSL_CRASH: &[u8] = include_bytes!("../assets/drums/acoustic_vcsl/crash.flac");
const VCSL_RIDE: &[u8] = include_bytes!("../assets/drums/acoustic_vcsl/ride.flac");

/// A bundled, real-recording drum kit `builtin:drums` can play its pads
/// back from instead of synthesizing them (a pad the kit has no sample for
/// keeps synthesizing). Set with [`SampleKit::id`] as the plugin's saved
/// state, the same way an LV2 plugin's soundfont path is a property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SampleKit {
    /// Kick, snare, hi-hats, tom and cymbals recorded for the Versilian
    /// Community Sample Library (CC0; see
    /// `crates/dawkey-plugins/assets/drums/acoustic_vcsl/README.md`), plus
    /// a real hand-clap from the same library. Its ride is a stick-struck
    /// suspended cymbal — VCSL has no dedicated ride.
    AcousticVcsl,
    /// A jazz kit played with sticks, from Versilian Studios' Virtuosity
    /// Drums (CC0; see
    /// `crates/dawkey-plugins/assets/drums/jazz_virtuosity/README.md`). It
    /// has every pad but Clap, including a real ride cymbal.
    JazzVirtuosity,
    /// The same Virtuosity Drums performance as `jazz_virtuosity`, miked at
    /// its deliberately degraded "lofi" position instead of "mid" (see
    /// `crates/dawkey-plugins/assets/drums/lofi_virtuosity/README.md`).
    LofiVirtuosity,
    /// Older, duller-sounding "Legacy" kick/snare/tom recordings from VCSL
    /// (see `crates/dawkey-plugins/assets/drums/vintage_vcsl/README.md`), a
    /// dusty 60s/70s character; cymbals and clap are shared with
    /// `acoustic_vcsl`.
    VintageVcsl,
    /// Hand percussion from VCSL — cajon, bongo, claves, cowbell, conga —
    /// mapped onto the pad layout the way a drum machine remaps a
    /// percussion ensemble onto a standard kit (see
    /// `crates/dawkey-plugins/assets/drums/latin_vcsl/README.md`). Cymbals
    /// and clap are shared with `acoustic_vcsl`.
    LatinVcsl,
}

impl SampleKit {
    pub const ALL: [SampleKit; 5] = [
        SampleKit::AcousticVcsl,
        SampleKit::JazzVirtuosity,
        SampleKit::LofiVirtuosity,
        SampleKit::VintageVcsl,
        SampleKit::LatinVcsl,
    ];

    pub fn id(self) -> &'static str {
        match self {
            SampleKit::AcousticVcsl => "acoustic_vcsl",
            SampleKit::JazzVirtuosity => "jazz_virtuosity",
            SampleKit::LofiVirtuosity => "lofi_virtuosity",
            SampleKit::VintageVcsl => "vintage_vcsl",
            SampleKit::LatinVcsl => "latin_vcsl",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == id)
    }

    /// One embedded sample per pad, in `Pad::ALL`/`PAD_NAMES` order (Kick,
    /// Snare, Closed Hat, Open Hat, Tom, Clap, Crash, Ride); `None` for a
    /// pad this kit leaves synthesized.
    fn assets(self) -> [Option<&'static [u8]>; 8] {
        match self {
            SampleKit::AcousticVcsl => [
                Some(include_bytes!("../assets/drums/acoustic_vcsl/kick.flac").as_slice()),
                Some(include_bytes!("../assets/drums/acoustic_vcsl/snare.flac").as_slice()),
                Some(VCSL_CLOSED_HAT),
                Some(VCSL_OPEN_HAT),
                Some(include_bytes!("../assets/drums/acoustic_vcsl/tom.flac").as_slice()),
                Some(VCSL_CLAP),
                Some(VCSL_CRASH),
                Some(VCSL_RIDE),
            ],
            SampleKit::JazzVirtuosity => [
                Some(include_bytes!("../assets/drums/jazz_virtuosity/kick.flac").as_slice()),
                Some(include_bytes!("../assets/drums/jazz_virtuosity/snare.flac").as_slice()),
                Some(include_bytes!("../assets/drums/jazz_virtuosity/closed_hat.flac").as_slice()),
                Some(include_bytes!("../assets/drums/jazz_virtuosity/open_hat.flac").as_slice()),
                Some(include_bytes!("../assets/drums/jazz_virtuosity/tom.flac").as_slice()),
                None,
                Some(include_bytes!("../assets/drums/jazz_virtuosity/crash.flac").as_slice()),
                Some(include_bytes!("../assets/drums/jazz_virtuosity/ride.flac").as_slice()),
            ],
            SampleKit::LofiVirtuosity => [
                Some(include_bytes!("../assets/drums/lofi_virtuosity/kick.flac").as_slice()),
                Some(include_bytes!("../assets/drums/lofi_virtuosity/snare.flac").as_slice()),
                Some(include_bytes!("../assets/drums/lofi_virtuosity/closed_hat.flac").as_slice()),
                Some(include_bytes!("../assets/drums/lofi_virtuosity/open_hat.flac").as_slice()),
                Some(include_bytes!("../assets/drums/lofi_virtuosity/tom.flac").as_slice()),
                None,
                Some(include_bytes!("../assets/drums/lofi_virtuosity/crash.flac").as_slice()),
                Some(include_bytes!("../assets/drums/lofi_virtuosity/ride.flac").as_slice()),
            ],
            SampleKit::VintageVcsl => [
                Some(include_bytes!("../assets/drums/vintage_vcsl/kick.flac").as_slice()),
                Some(include_bytes!("../assets/drums/vintage_vcsl/snare.flac").as_slice()),
                Some(VCSL_CLOSED_HAT),
                Some(VCSL_OPEN_HAT),
                Some(include_bytes!("../assets/drums/vintage_vcsl/tom.flac").as_slice()),
                Some(VCSL_CLAP),
                Some(VCSL_CRASH),
                Some(VCSL_RIDE),
            ],
            SampleKit::LatinVcsl => [
                Some(include_bytes!("../assets/drums/latin_vcsl/kick.flac").as_slice()),
                Some(include_bytes!("../assets/drums/latin_vcsl/snare.flac").as_slice()),
                Some(include_bytes!("../assets/drums/latin_vcsl/closed_hat.flac").as_slice()),
                Some(include_bytes!("../assets/drums/latin_vcsl/open_hat.flac").as_slice()),
                Some(include_bytes!("../assets/drums/latin_vcsl/tom.flac").as_slice()),
                Some(VCSL_CLAP),
                Some(VCSL_CRASH),
                Some(VCSL_RIDE),
            ],
        }
    }
}

type SampleKitCache = HashMap<(SampleKit, u32), Arc<SampleKitData>>;

/// Decoded, resampled sample kits, memoized by `(kit, sample_rate)` since
/// the embedded bytes never change. Building an entry decodes and allocates,
/// so [`load_sample_kit`] must only run on the control thread (it's only
/// ever called from [`load`], during graph compilation).
static SAMPLE_KIT_CACHE: LazyLock<Mutex<SampleKitCache>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn load_sample_kit(kit: SampleKit, sample_rate: u32) -> Arc<SampleKitData> {
    let mut cache = SAMPLE_KIT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache
        .entry((kit, sample_rate))
        .or_insert_with(|| {
            let mut data: SampleKitData = Default::default();
            let label = Path::new(kit.id());
            for (i, asset) in kit.assets().into_iter().enumerate() {
                let Some(bytes) = asset else { continue };
                if let Ok(decoded) = dawkey_media::decode_bytes(bytes, "flac", sample_rate, label) {
                    data[i] = Some(decoded.audio.channel(0).to_vec());
                }
            }
            Arc::new(data)
        })
        .clone()
}

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

pub(crate) fn load(
    info: &PluginInfo,
    sample_rate: u32,
    state: Option<&[u8]>,
) -> Result<BuiltinProcessor, PluginError> {
    if info.id == DRUMS_ID {
        let mut engine = DrumEngine::new(sample_rate);
        if let Some(kit) = state
            .and_then(|s| std::str::from_utf8(s).ok())
            .and_then(SampleKit::from_id)
        {
            engine.load_samples(Some(load_sample_kit(kit, sample_rate)));
        }
        return Ok(BuiltinProcessor::Drums(Box::new(engine)));
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

#[cfg(test)]
mod tests {
    use dawkey_dsp::Pad;

    use super::*;

    fn drum_info() -> PluginInfo {
        builtin_plugin(DRUMS_ID).cloned().expect("registered")
    }

    #[test]
    fn with_no_state_it_synthesizes_like_before() {
        let processor = load(&drum_info(), 48_000, None).unwrap();
        assert!(matches!(processor, BuiltinProcessor::Drums(_)));
    }

    #[test]
    fn an_unknown_kit_id_falls_back_to_synthesis_instead_of_erroring() {
        load(&drum_info(), 48_000, Some(b"not_a_real_kit")).unwrap();
    }

    #[test]
    fn a_known_kit_loads_real_samples_for_the_pads_it_covers() {
        let kit = load_sample_kit(SampleKit::AcousticVcsl, 48_000);
        // Every pad has a real sample now, including Clap (a shared VCSL
        // hand-clap) and Ride (a stand-in suspended cymbal, since VCSL has
        // no dedicated ride).
        for pad in Pad::ALL {
            assert!(
                kit[pad.index()].as_ref().is_some_and(|b| !b.is_empty()),
                "{pad:?} should have a sample"
            );
        }
    }

    #[test]
    fn every_kit_covers_every_pad_except_where_it_documents_a_gap() {
        // Only the two Virtuosity-sourced kits lack a Clap sample (that
        // library has no hand-clap); every VCSL-sourced kit shares one.
        let no_clap = [SampleKit::JazzVirtuosity, SampleKit::LofiVirtuosity];
        for kit_id in SampleKit::ALL {
            let kit = load_sample_kit(kit_id, 48_000);
            for pad in Pad::ALL {
                let has_sample = kit[pad.index()].as_ref().is_some_and(|b| !b.is_empty());
                if pad == Pad::Clap && no_clap.contains(&kit_id) {
                    assert!(!has_sample, "{kit_id:?} {pad:?} should synthesize");
                } else {
                    assert!(has_sample, "{kit_id:?} {pad:?} should have a sample");
                }
            }
        }
    }

    #[test]
    fn loading_the_same_kit_twice_reuses_the_cached_buffers() {
        let a = load_sample_kit(SampleKit::AcousticVcsl, 44_100);
        let b = load_sample_kit(SampleKit::AcousticVcsl, 44_100);
        assert!(Arc::ptr_eq(&a, &b));
    }
}
