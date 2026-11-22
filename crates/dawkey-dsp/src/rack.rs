// SPDX-License-Identifier: AGPL-3.0-or-later
//! Built-in 19-inch studio guitar effects rack DSP modules.
//!
//! All modules allocate their delay buffers once on construction and run
//! strictly lock-free and allocation-free inside [`RackProcessor::process`].

use std::f32::consts::{PI, TAU};

const MIN_FILTER_HZ: f32 = 10.0;
const ROOM_MAX_MS: f32 = 45.0;
const CHORUS_MAX_MS: f32 = 60.0;
const DELAY_MAX_MS: f32 = 2100.0;
const REVERB_PREDELAY_MAX_MS: f32 = 160.0;
/// Prevents subnormal floats inside feedback loops.
const ANTI_DENORMAL: f32 = 1e-24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RackEffect {
    Gate,
    Comp,
    Drive,
    Amp,
    Eq,
    Cab,
    Chorus,
    Delay,
    Reverb,
    AutoWah,
    Phaser,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BuiltinChoice {
    pub value: f64,
    pub label: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BuiltinParam {
    pub id: &'static str,
    pub name: &'static str,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub stepped: bool,
    pub unit: Option<&'static str>,
    pub choices: &'static [BuiltinChoice],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BuiltinSpec {
    pub effect: RackEffect,
    pub id: &'static str,
    pub name: &'static str,
    pub vendor: &'static str,
    pub sidechain: bool,
    pub params: &'static [BuiltinParam],
}

const TIGHT_CHOICES: &[BuiltinChoice] = &[
    BuiltinChoice {
        value: 0.0,
        label: "Normal",
    },
    BuiltinChoice {
        value: 1.0,
        label: "Tight",
    },
];

const AMP_CHANNEL_CHOICES: &[BuiltinChoice] = &[
    BuiltinChoice {
        value: 0.0,
        label: "Clean",
    },
    BuiltinChoice {
        value: 1.0,
        label: "Crunch",
    },
    BuiltinChoice {
        value: 2.0,
        label: "Lead",
    },
    BuiltinChoice {
        value: 3.0,
        label: "Modern",
    },
];

const CABINET_CHOICES: &[BuiltinChoice] = &[
    BuiltinChoice {
        value: 0.0,
        label: "1x12 Tweed",
    },
    BuiltinChoice {
        value: 1.0,
        label: "2x12 Brit",
    },
    BuiltinChoice {
        value: 2.0,
        label: "4x12 Vintage",
    },
    BuiltinChoice {
        value: 3.0,
        label: "4x12 Modern",
    },
    BuiltinChoice {
        value: 4.0,
        label: "4x10 Bass",
    },
    BuiltinChoice {
        value: 5.0,
        label: "1x15 Bass",
    },
];

const MIC_CHOICES: &[BuiltinChoice] = &[
    BuiltinChoice {
        value: 0.0,
        label: "Dyn 57",
    },
    BuiltinChoice {
        value: 1.0,
        label: "Ribbon 121",
    },
    BuiltinChoice {
        value: 2.0,
        label: "Cond 414",
    },
];

const DELAY_SYNC_CHOICES: &[BuiltinChoice] = &[
    BuiltinChoice {
        value: 0.0,
        label: "Free",
    },
    BuiltinChoice {
        value: 1.0,
        label: "1/16",
    },
    BuiltinChoice {
        value: 2.0,
        label: "1/8",
    },
    BuiltinChoice {
        value: 3.0,
        label: "1/8D",
    },
    BuiltinChoice {
        value: 4.0,
        label: "1/4T",
    },
    BuiltinChoice {
        value: 5.0,
        label: "1/4",
    },
    BuiltinChoice {
        value: 6.0,
        label: "1/2",
    },
];

const REVERB_MODE_CHOICES: &[BuiltinChoice] = &[
    BuiltinChoice {
        value: 0.0,
        label: "Spring",
    },
    BuiltinChoice {
        value: 1.0,
        label: "Plate",
    },
    BuiltinChoice {
        value: 2.0,
        label: "Hall",
    },
];

const GATE_PARAMS: &[BuiltinParam] = &[
    BuiltinParam {
        id: "threshold",
        name: "Threshold",
        min: -80.0,
        max: 0.0,
        default: -45.0,
        stepped: false,
        unit: Some("dB"),
        choices: &[],
    },
    BuiltinParam {
        id: "floor",
        name: "Floor",
        min: -80.0,
        max: 0.0,
        default: -72.0,
        stepped: false,
        unit: Some("dB"),
        choices: &[],
    },
    BuiltinParam {
        id: "hysteresis",
        name: "Hysteresis",
        min: 0.0,
        max: 12.0,
        default: 4.0,
        stepped: false,
        unit: Some("dB"),
        choices: &[],
    },
    BuiltinParam {
        id: "attack",
        name: "Attack",
        min: 0.1,
        max: 50.0,
        default: 1.0,
        stepped: false,
        unit: Some("ms"),
        choices: &[],
    },
    BuiltinParam {
        id: "hold",
        name: "Hold",
        min: 1.0,
        max: 500.0,
        default: 40.0,
        stepped: false,
        unit: Some("ms"),
        choices: &[],
    },
    BuiltinParam {
        id: "release",
        name: "Release",
        min: 5.0,
        max: 1000.0,
        default: 120.0,
        stepped: false,
        unit: Some("ms"),
        choices: &[],
    },
];

const COMP_PARAMS: &[BuiltinParam] = &[
    BuiltinParam {
        id: "threshold",
        name: "Threshold",
        min: -48.0,
        max: 0.0,
        default: -18.0,
        stepped: false,
        unit: Some("dB"),
        choices: &[],
    },
    BuiltinParam {
        id: "ratio",
        name: "Ratio",
        min: 1.0,
        max: 20.0,
        default: 4.0,
        stepped: false,
        unit: Some(":1"),
        choices: &[],
    },
    BuiltinParam {
        id: "attack",
        name: "Attack",
        min: 0.1,
        max: 100.0,
        default: 5.0,
        stepped: false,
        unit: Some("ms"),
        choices: &[],
    },
    BuiltinParam {
        id: "release",
        name: "Release",
        min: 10.0,
        max: 1500.0,
        default: 150.0,
        stepped: false,
        unit: Some("ms"),
        choices: &[],
    },
    BuiltinParam {
        id: "makeup",
        name: "Makeup",
        min: -12.0,
        max: 24.0,
        default: 3.0,
        stepped: false,
        unit: Some("dB"),
        choices: &[],
    },
    BuiltinParam {
        id: "mix",
        name: "Mix",
        min: 0.0,
        max: 100.0,
        default: 100.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
];

const DRIVE_PARAMS: &[BuiltinParam] = &[
    BuiltinParam {
        id: "drive",
        name: "Drive",
        min: 0.0,
        max: 10.0,
        default: 5.0,
        stepped: false,
        unit: None,
        choices: &[],
    },
    BuiltinParam {
        id: "tone",
        name: "Tone",
        min: 0.0,
        max: 10.0,
        default: 5.5,
        stepped: false,
        unit: None,
        choices: &[],
    },
    BuiltinParam {
        id: "level",
        name: "Level",
        min: -24.0,
        max: 12.0,
        default: 0.0,
        stepped: false,
        unit: Some("dB"),
        choices: &[],
    },
    BuiltinParam {
        id: "tight",
        name: "Voicing",
        min: 0.0,
        max: 1.0,
        default: 1.0,
        stepped: true,
        unit: None,
        choices: TIGHT_CHOICES,
    },
    BuiltinParam {
        id: "mix",
        name: "Mix",
        min: 0.0,
        max: 100.0,
        default: 100.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
];

const AMP_PARAMS: &[BuiltinParam] = &[
    BuiltinParam {
        id: "channel",
        name: "Channel",
        min: 0.0,
        max: 3.0,
        default: 2.0,
        stepped: true,
        unit: None,
        choices: AMP_CHANNEL_CHOICES,
    },
    BuiltinParam {
        id: "gain",
        name: "Gain",
        min: 0.0,
        max: 10.0,
        default: 6.0,
        stepped: false,
        unit: None,
        choices: &[],
    },
    BuiltinParam {
        id: "bass",
        name: "Bass",
        min: 0.0,
        max: 10.0,
        default: 5.0,
        stepped: false,
        unit: None,
        choices: &[],
    },
    BuiltinParam {
        id: "mid",
        name: "Mid",
        min: 0.0,
        max: 10.0,
        default: 6.0,
        stepped: false,
        unit: None,
        choices: &[],
    },
    BuiltinParam {
        id: "treble",
        name: "Treble",
        min: 0.0,
        max: 10.0,
        default: 6.0,
        stepped: false,
        unit: None,
        choices: &[],
    },
    BuiltinParam {
        id: "presence",
        name: "Presence",
        min: 0.0,
        max: 10.0,
        default: 5.0,
        stepped: false,
        unit: None,
        choices: &[],
    },
    BuiltinParam {
        id: "depth",
        name: "Depth",
        min: 0.0,
        max: 10.0,
        default: 5.0,
        stepped: false,
        unit: None,
        choices: &[],
    },
    BuiltinParam {
        id: "sag",
        name: "Sag",
        min: 0.0,
        max: 10.0,
        default: 3.0,
        stepped: false,
        unit: None,
        choices: &[],
    },
    BuiltinParam {
        id: "master",
        name: "Master",
        min: -36.0,
        max: 12.0,
        default: 0.0,
        stepped: false,
        unit: Some("dB"),
        choices: &[],
    },
];

const EQ_PARAMS: &[BuiltinParam] = &[
    BuiltinParam {
        id: "low_cut",
        name: "Low Cut",
        min: 20.0,
        max: 400.0,
        default: 60.0,
        stepped: false,
        unit: Some("Hz"),
        choices: &[],
    },
    BuiltinParam {
        id: "low_gain",
        name: "100 Hz",
        min: -15.0,
        max: 15.0,
        default: 0.0,
        stepped: false,
        unit: Some("dB"),
        choices: &[],
    },
    BuiltinParam {
        id: "low_mid_gain",
        name: "280 Hz",
        min: -15.0,
        max: 15.0,
        default: 0.0,
        stepped: false,
        unit: Some("dB"),
        choices: &[],
    },
    BuiltinParam {
        id: "mid_freq",
        name: "Mid Freq",
        min: 300.0,
        max: 2500.0,
        default: 850.0,
        stepped: false,
        unit: Some("Hz"),
        choices: &[],
    },
    BuiltinParam {
        id: "mid_gain",
        name: "Mid Gain",
        min: -15.0,
        max: 15.0,
        default: 0.0,
        stepped: false,
        unit: Some("dB"),
        choices: &[],
    },
    BuiltinParam {
        id: "mid_q",
        name: "Mid Q",
        min: 0.4,
        max: 8.0,
        default: 1.2,
        stepped: false,
        unit: None,
        choices: &[],
    },
    BuiltinParam {
        id: "high_mid_gain",
        name: "2.4 kHz",
        min: -15.0,
        max: 15.0,
        default: 0.0,
        stepped: false,
        unit: Some("dB"),
        choices: &[],
    },
    BuiltinParam {
        id: "high_gain",
        name: "6 kHz",
        min: -15.0,
        max: 15.0,
        default: 0.0,
        stepped: false,
        unit: Some("dB"),
        choices: &[],
    },
    BuiltinParam {
        id: "output",
        name: "Trim",
        min: -12.0,
        max: 12.0,
        default: 0.0,
        stepped: false,
        unit: Some("dB"),
        choices: &[],
    },
];

const CAB_PARAMS: &[BuiltinParam] = &[
    BuiltinParam {
        id: "cabinet",
        name: "Cabinet",
        min: 0.0,
        max: 5.0,
        default: 2.0,
        stepped: true,
        unit: None,
        choices: CABINET_CHOICES,
    },
    BuiltinParam {
        id: "mic",
        name: "Mic",
        min: 0.0,
        max: 2.0,
        default: 0.0,
        stepped: true,
        unit: None,
        choices: MIC_CHOICES,
    },
    BuiltinParam {
        id: "distance",
        name: "Distance",
        min: 0.0,
        max: 100.0,
        default: 25.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "low_cut",
        name: "Low Cut",
        min: 20.0,
        max: 300.0,
        default: 75.0,
        stepped: false,
        unit: Some("Hz"),
        choices: &[],
    },
    BuiltinParam {
        id: "high_cut",
        name: "High Cut",
        min: 2000.0,
        max: 16000.0,
        default: 6500.0,
        stepped: false,
        unit: Some("Hz"),
        choices: &[],
    },
    BuiltinParam {
        id: "room",
        name: "Room",
        min: 0.0,
        max: 100.0,
        default: 15.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "output",
        name: "Output",
        min: -24.0,
        max: 12.0,
        default: 0.0,
        stepped: false,
        unit: Some("dB"),
        choices: &[],
    },
];

const CHORUS_PARAMS: &[BuiltinParam] = &[
    BuiltinParam {
        id: "rate",
        name: "Rate",
        min: 0.05,
        max: 8.0,
        default: 0.8,
        stepped: false,
        unit: Some("Hz"),
        choices: &[],
    },
    BuiltinParam {
        id: "depth",
        name: "Depth",
        min: 0.0,
        max: 100.0,
        default: 45.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "delay",
        name: "Delay",
        min: 2.0,
        max: 25.0,
        default: 7.5,
        stepped: false,
        unit: Some("ms"),
        choices: &[],
    },
    BuiltinParam {
        id: "warmth",
        name: "Warmth",
        min: 0.0,
        max: 100.0,
        default: 40.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "width",
        name: "Width",
        min: 0.0,
        max: 100.0,
        default: 100.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "mix",
        name: "Mix",
        min: 0.0,
        max: 100.0,
        default: 35.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
];

const DELAY_PARAMS: &[BuiltinParam] = &[
    BuiltinParam {
        id: "time",
        name: "Time",
        min: 10.0,
        max: 2000.0,
        default: 375.0,
        stepped: false,
        unit: Some("ms"),
        choices: &[],
    },
    BuiltinParam {
        id: "sync",
        name: "Sync",
        min: 0.0,
        max: 6.0,
        default: 0.0,
        stepped: true,
        unit: None,
        choices: DELAY_SYNC_CHOICES,
    },
    BuiltinParam {
        id: "feedback",
        name: "Feedback",
        min: 0.0,
        max: 95.0,
        default: 35.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "ping_pong",
        name: "Ping-Pong",
        min: 0.0,
        max: 100.0,
        default: 50.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "tone",
        name: "Tone",
        min: 0.0,
        max: 100.0,
        default: 60.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "mod_depth",
        name: "Mod",
        min: 0.0,
        max: 100.0,
        default: 15.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "duck",
        name: "Duck",
        min: 0.0,
        max: 100.0,
        default: 25.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "mix",
        name: "Mix",
        min: 0.0,
        max: 100.0,
        default: 25.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
];

const REVERB_PARAMS: &[BuiltinParam] = &[
    BuiltinParam {
        id: "mode",
        name: "Mode",
        min: 0.0,
        max: 2.0,
        default: 1.0,
        stepped: true,
        unit: None,
        choices: REVERB_MODE_CHOICES,
    },
    BuiltinParam {
        id: "predelay",
        name: "Pre-Delay",
        min: 0.0,
        max: 150.0,
        default: 20.0,
        stepped: false,
        unit: Some("ms"),
        choices: &[],
    },
    BuiltinParam {
        id: "decay",
        name: "Decay",
        min: 0.2,
        max: 12.0,
        default: 2.2,
        stepped: false,
        unit: Some("s"),
        choices: &[],
    },
    BuiltinParam {
        id: "damping",
        name: "Damping",
        min: 0.0,
        max: 100.0,
        default: 45.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "dispersion",
        name: "Spread",
        min: 0.0,
        max: 100.0,
        default: 60.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "low_cut",
        name: "Low Cut",
        min: 20.0,
        max: 500.0,
        default: 100.0,
        stepped: false,
        unit: Some("Hz"),
        choices: &[],
    },
    BuiltinParam {
        id: "mix",
        name: "Mix",
        min: 0.0,
        max: 100.0,
        default: 22.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
];

const AUTOWAH_DIRECTION_CHOICES: &[BuiltinChoice] = &[
    BuiltinChoice {
        value: 0.0,
        label: "Up",
    },
    BuiltinChoice {
        value: 1.0,
        label: "Down",
    },
];

const AUTOWAH_PARAMS: &[BuiltinParam] = &[
    BuiltinParam {
        id: "sensitivity",
        name: "Sensitivity",
        min: 0.0,
        max: 100.0,
        default: 60.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "freq_low",
        name: "Freq Low",
        min: 80.0,
        max: 1500.0,
        default: 300.0,
        stepped: false,
        unit: Some("Hz"),
        choices: &[],
    },
    BuiltinParam {
        id: "freq_high",
        name: "Freq High",
        min: 400.0,
        max: 6000.0,
        default: 2200.0,
        stepped: false,
        unit: Some("Hz"),
        choices: &[],
    },
    BuiltinParam {
        id: "q",
        name: "Resonance",
        min: 0.3,
        max: 10.0,
        default: 2.0,
        stepped: false,
        unit: None,
        choices: &[],
    },
    BuiltinParam {
        id: "attack",
        name: "Attack",
        min: 0.5,
        max: 50.0,
        default: 4.0,
        stepped: false,
        unit: Some("ms"),
        choices: &[],
    },
    BuiltinParam {
        id: "release",
        name: "Release",
        min: 10.0,
        max: 500.0,
        default: 120.0,
        stepped: false,
        unit: Some("ms"),
        choices: &[],
    },
    BuiltinParam {
        id: "direction",
        name: "Direction",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        stepped: true,
        unit: None,
        choices: AUTOWAH_DIRECTION_CHOICES,
    },
    BuiltinParam {
        id: "mix",
        name: "Mix",
        min: 0.0,
        max: 100.0,
        default: 100.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
];

const PHASER_STAGE_CHOICES: &[BuiltinChoice] = &[
    BuiltinChoice {
        value: 0.0,
        label: "4",
    },
    BuiltinChoice {
        value: 1.0,
        label: "6",
    },
    BuiltinChoice {
        value: 2.0,
        label: "8",
    },
];

const PHASER_PARAMS: &[BuiltinParam] = &[
    BuiltinParam {
        id: "rate",
        name: "Rate",
        min: 0.02,
        max: 8.0,
        default: 0.5,
        stepped: false,
        unit: Some("Hz"),
        choices: &[],
    },
    BuiltinParam {
        id: "depth",
        name: "Depth",
        min: 0.0,
        max: 100.0,
        default: 80.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "feedback",
        name: "Feedback",
        min: 0.0,
        max: 95.0,
        default: 40.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "stages",
        name: "Stages",
        min: 0.0,
        max: 2.0,
        default: 0.0,
        stepped: true,
        unit: None,
        choices: PHASER_STAGE_CHOICES,
    },
    BuiltinParam {
        id: "center",
        name: "Center",
        min: 200.0,
        max: 4000.0,
        default: 800.0,
        stepped: false,
        unit: Some("Hz"),
        choices: &[],
    },
    BuiltinParam {
        id: "width",
        name: "Width",
        min: 0.0,
        max: 100.0,
        default: 70.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
    BuiltinParam {
        id: "mix",
        name: "Mix",
        min: 0.0,
        max: 100.0,
        default: 50.0,
        stepped: false,
        unit: Some("%"),
        choices: &[],
    },
];

static SPECS: &[BuiltinSpec] = &[
    BuiltinSpec {
        effect: RackEffect::Gate,
        id: "builtin:gate",
        name: "GATE-100 Studio Silencer",
        vendor: "Dawkey Studio",
        sidechain: true,
        params: GATE_PARAMS,
    },
    BuiltinSpec {
        effect: RackEffect::Comp,
        id: "builtin:comp",
        name: "COMP-76 FET Studio Limiter",
        vendor: "Dawkey Studio",
        sidechain: true,
        params: COMP_PARAMS,
    },
    BuiltinSpec {
        effect: RackEffect::Drive,
        id: "builtin:drive",
        name: "OD-808 Tube Overdrive",
        vendor: "Dawkey Studio",
        sidechain: false,
        params: DRIVE_PARAMS,
    },
    BuiltinSpec {
        effect: RackEffect::Amp,
        id: "builtin:amp",
        name: "JMP-2000 Dual Tube Preamp",
        vendor: "Dawkey Studio",
        sidechain: false,
        params: AMP_PARAMS,
    },
    BuiltinSpec {
        effect: RackEffect::Eq,
        id: "builtin:eq",
        name: "PEQ-500 5-Band Parametric EQ",
        vendor: "Dawkey Studio",
        sidechain: false,
        params: EQ_PARAMS,
    },
    BuiltinSpec {
        effect: RackEffect::Cab,
        id: "builtin:cab",
        name: "CAB-IR Studio Cabinet & Mic",
        vendor: "Dawkey Studio",
        sidechain: false,
        params: CAB_PARAMS,
    },
    BuiltinSpec {
        effect: RackEffect::Chorus,
        id: "builtin:chorus",
        name: "DIM-C Tri-Stereo Chorus",
        vendor: "Dawkey Studio",
        sidechain: false,
        params: CHORUS_PARAMS,
    },
    BuiltinSpec {
        effect: RackEffect::Delay,
        id: "builtin:delay",
        name: "DDL-2290 Studio Digital Delay",
        vendor: "Dawkey Studio",
        sidechain: false,
        params: DELAY_PARAMS,
    },
    BuiltinSpec {
        effect: RackEffect::Reverb,
        id: "builtin:reverb",
        name: "VERB-480 Studio Reverberator",
        vendor: "Dawkey Studio",
        sidechain: false,
        params: REVERB_PARAMS,
    },
    BuiltinSpec {
        effect: RackEffect::AutoWah,
        id: "builtin:autowah",
        name: "AW-909 Envelope Filter",
        vendor: "Dawkey Studio",
        sidechain: false,
        params: AUTOWAH_PARAMS,
    },
    BuiltinSpec {
        effect: RackEffect::Phaser,
        id: "builtin:phaser",
        name: "PH-90 Analog Phaser",
        vendor: "Dawkey Studio",
        sidechain: false,
        params: PHASER_PARAMS,
    },
];

pub fn builtin_specs() -> &'static [BuiltinSpec] {
    SPECS
}

impl RackEffect {
    pub fn id(self) -> &'static str {
        self.spec().id
    }

    pub fn from_id(id: &str) -> Option<Self> {
        SPECS.iter().find(|s| s.id == id).map(|s| s.effect)
    }

    pub fn spec(self) -> &'static BuiltinSpec {
        &SPECS[self as usize]
    }
}

#[inline]
fn db_to_lin(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

#[inline]
fn lin_to_db(lin: f32) -> f32 {
    20.0 * lin.max(1e-6).log10()
}

#[inline]
fn one_pole_coeff(time_ms: f32, sample_rate: f32) -> f32 {
    let samples = (time_ms.max(0.01) * 0.001 * sample_rate).max(1.0);
    (-1.0 / samples).exp()
}

#[inline]
fn lp_coeff(freq_hz: f32, sample_rate: f32) -> f32 {
    let w = (TAU * freq_hz.clamp(MIN_FILTER_HZ, sample_rate * 0.45) / sample_rate).min(PI);
    (-w).exp()
}

/// Stereo biquad filter (Direct Form II Transposed) with exact magnitude query.
#[derive(Debug, Clone, Copy)]
struct StereoBiquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: [f32; 2],
    z2: [f32; 2],
}

impl StereoBiquad {
    fn bypass() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: [0.0; 2],
            z2: [0.0; 2],
        }
    }

    fn set_coeffs(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        let inv = 1.0 / a0;
        self.b0 = b0 * inv;
        self.b1 = b1 * inv;
        self.b2 = b2 * inv;
        self.a1 = a1 * inv;
        self.a2 = a2 * inv;
    }

    fn set_highpass(&mut self, freq_hz: f32, q: f32, sr: f32) {
        let f = freq_hz.clamp(MIN_FILTER_HZ, sr * 0.45);
        let w = TAU * f / sr;
        let cos = w.cos();
        let alpha = w.sin() / (2.0 * q.max(0.1));
        self.set_coeffs(
            (1.0 + cos) * 0.5,
            -(1.0 + cos),
            (1.0 + cos) * 0.5,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        );
    }

    fn set_lowpass(&mut self, freq_hz: f32, q: f32, sr: f32) {
        let f = freq_hz.clamp(MIN_FILTER_HZ, sr * 0.45);
        let w = TAU * f / sr;
        let cos = w.cos();
        let alpha = w.sin() / (2.0 * q.max(0.1));
        self.set_coeffs(
            (1.0 - cos) * 0.5,
            1.0 - cos,
            (1.0 - cos) * 0.5,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        );
    }

    fn set_peaking(&mut self, freq_hz: f32, q: f32, gain_db: f32, sr: f32) {
        if gain_db.abs() < 1e-3 {
            self.b0 = 1.0;
            self.b1 = 0.0;
            self.b2 = 0.0;
            self.a1 = 0.0;
            self.a2 = 0.0;
            return;
        }
        let f = freq_hz.clamp(MIN_FILTER_HZ, sr * 0.45);
        let a = 10.0_f32.powf(gain_db / 40.0);
        let w = TAU * f / sr;
        let cos = w.cos();
        let alpha = w.sin() / (2.0 * q.max(0.1));
        self.set_coeffs(
            1.0 + alpha * a,
            -2.0 * cos,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos,
            1.0 - alpha / a,
        );
    }

    fn set_low_shelf(&mut self, freq_hz: f32, slope: f32, gain_db: f32, sr: f32) {
        if gain_db.abs() < 1e-3 {
            self.b0 = 1.0;
            self.b1 = 0.0;
            self.b2 = 0.0;
            self.a1 = 0.0;
            self.a2 = 0.0;
            return;
        }
        let f = freq_hz.clamp(MIN_FILTER_HZ, sr * 0.45);
        let a = 10.0_f32.powf(gain_db / 40.0);
        let w = TAU * f / sr;
        let cos = w.cos();
        let sin = w.sin();
        let alpha = sin
            * 0.5
            * ((a + 1.0 / a) * (1.0 / slope.max(0.1) - 1.0) + 2.0)
                .max(0.0)
                .sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        self.set_coeffs(
            a * ((a + 1.0) - (a - 1.0) * cos + two_sqrt_a_alpha),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
            a * ((a + 1.0) - (a - 1.0) * cos - two_sqrt_a_alpha),
            (a + 1.0) + (a - 1.0) * cos + two_sqrt_a_alpha,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos),
            (a + 1.0) + (a - 1.0) * cos - two_sqrt_a_alpha,
        );
    }

    fn set_high_shelf(&mut self, freq_hz: f32, slope: f32, gain_db: f32, sr: f32) {
        if gain_db.abs() < 1e-3 {
            self.b0 = 1.0;
            self.b1 = 0.0;
            self.b2 = 0.0;
            self.a1 = 0.0;
            self.a2 = 0.0;
            return;
        }
        let f = freq_hz.clamp(MIN_FILTER_HZ, sr * 0.45);
        let a = 10.0_f32.powf(gain_db / 40.0);
        let w = TAU * f / sr;
        let cos = w.cos();
        let sin = w.sin();
        let alpha = sin
            * 0.5
            * ((a + 1.0 / a) * (1.0 / slope.max(0.1) - 1.0) + 2.0)
                .max(0.0)
                .sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        self.set_coeffs(
            a * ((a + 1.0) + (a - 1.0) * cos + two_sqrt_a_alpha),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
            a * ((a + 1.0) + (a - 1.0) * cos - two_sqrt_a_alpha),
            (a + 1.0) - (a - 1.0) * cos + two_sqrt_a_alpha,
            2.0 * ((a - 1.0) - (a + 1.0) * cos),
            (a + 1.0) - (a - 1.0) * cos - two_sqrt_a_alpha,
        );
    }

    fn set_notch(&mut self, freq_hz: f32, q: f32, sr: f32) {
        let f = freq_hz.clamp(MIN_FILTER_HZ, sr * 0.45);
        let w = TAU * f / sr;
        let cos = w.cos();
        let alpha = w.sin() / (2.0 * q.max(0.1));
        self.set_coeffs(1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha);
    }

    /// Constant skirt gain, 0 dB peak bandpass: the resonant sweep an
    /// envelope filter rides its detected level across.
    fn set_bandpass(&mut self, freq_hz: f32, q: f32, sr: f32) {
        let f = freq_hz.clamp(MIN_FILTER_HZ, sr * 0.45);
        let w = TAU * f / sr;
        let cos = w.cos();
        let alpha = w.sin() / (2.0 * q.max(0.1));
        self.set_coeffs(alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha);
    }

    #[inline]
    fn process_channel(&mut self, ch: usize, x: f32) -> f32 {
        let y = self.b0 * x + self.z1[ch];
        self.z1[ch] = self.b1 * x - self.a1 * y + self.z2[ch] + ANTI_DENORMAL;
        self.z2[ch] = self.b2 * x - self.a2 * y;
        y
    }

    fn magnitude_db(&self, freq_hz: f32, sr: f32) -> f32 {
        let w = TAU * freq_hz.clamp(MIN_FILTER_HZ, sr * 0.49) / sr;
        let c1 = w.cos();
        let s1 = w.sin();
        let c2 = (2.0 * w).cos();
        let s2 = (2.0 * w).sin();
        let num_re = self.b0 + self.b1 * c1 + self.b2 * c2;
        let num_im = -self.b1 * s1 - self.b2 * s2;
        let den_re = 1.0 + self.a1 * c1 + self.a2 * c2;
        let den_im = -self.a1 * s1 - self.a2 * s2;
        let mag_sq =
            (num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im).max(1e-12);
        10.0 * mag_sq.max(1e-12).log10()
    }
}

/// The coefficient of a first-order allpass with break frequency `freq_hz`:
/// a phaser stage, swept by its LFO.
#[inline]
fn allpass_coeff(freq_hz: f32, sr: f32) -> f32 {
    let t = (PI * freq_hz.clamp(MIN_FILTER_HZ, sr * 0.45) / sr).tan();
    (t - 1.0) / (t + 1.0)
}

#[inline]
fn read_frac(buf: &[f32], write: usize, delay_samples: f32) -> f32 {
    let len = buf.len();
    let d = delay_samples.clamp(0.0, (len.saturating_sub(2)) as f32);
    let i0 = d.floor() as usize;
    let frac = d - i0 as f32;
    let idx0 = (write + len - i0) % len;
    let idx1 = (idx0 + len - 1) % len;
    buf[idx0] + (buf[idx1] - buf[idx0]) * frac
}

#[inline]
fn read_int(buf: &[f32], write: usize, delay_samples: usize) -> f32 {
    let len = buf.len();
    let d = delay_samples.min(len.saturating_sub(1));
    buf[(write + len - d) % len]
}

#[derive(Debug)]
struct GateState {
    env: f32,
    gain: f32,
    open: bool,
    hold_left: u32,
}

impl GateState {
    fn new() -> Self {
        Self {
            env: 0.0,
            gain: 1.0,
            open: false,
            hold_left: 0,
        }
    }

    fn process(
        &mut self,
        params: &[f64],
        left: &mut [f32],
        right: &mut [f32],
        sidechain: Option<[&[f32]; 2]>,
        sr: f32,
    ) {
        let thresh_db = params[0] as f32;
        let floor_db = params[1] as f32;
        let hyst_db = params[2] as f32;
        let attack_ms = params[3] as f32;
        let hold_ms = params[4] as f32;
        let release_ms = params[5] as f32;

        let open_lin = db_to_lin(thresh_db);
        let close_lin = db_to_lin(thresh_db - hyst_db);
        let floor_lin = db_to_lin(floor_db);
        let hold_samples = ((hold_ms * 0.001 * sr).round() as u32).max(1);

        let det_att = one_pole_coeff(1.5, sr);
        let det_rel = one_pole_coeff(25.0, sr);
        let g_att = one_pole_coeff(attack_ms, sr);
        let g_rel = one_pole_coeff(release_ms, sr);

        for i in 0..left.len() {
            let key = match sidechain {
                Some([sc_l, sc_r]) => sc_l[i].abs().max(sc_r[i].abs()),
                None => left[i].abs().max(right[i].abs()),
            };
            let det_c = if key > self.env { det_att } else { det_rel };
            self.env = det_c * self.env + (1.0 - det_c) * key + ANTI_DENORMAL;

            if self.env >= open_lin {
                self.open = true;
                self.hold_left = hold_samples;
            } else if self.open {
                if self.env >= close_lin {
                    self.hold_left = hold_samples;
                } else if self.hold_left > 0 {
                    self.hold_left -= 1;
                } else {
                    self.open = false;
                }
            }

            let target = if self.open { 1.0 } else { floor_lin };
            let gc = if target > self.gain { g_att } else { g_rel };
            self.gain = gc * self.gain + (1.0 - gc) * target;
            left[i] *= self.gain;
            right[i] *= self.gain;
        }
    }
}

#[derive(Debug)]
struct CompState {
    sc_hpf_l: f32,
    sc_hpf_r: f32,
    sc_prev_l: f32,
    sc_prev_r: f32,
    env_db: f32,
}

impl CompState {
    fn new() -> Self {
        Self {
            sc_hpf_l: 0.0,
            sc_hpf_r: 0.0,
            sc_prev_l: 0.0,
            sc_prev_r: 0.0,
            // A reduction amount (dB), never negative (see `process`): 0.0
            // is "no reduction yet", its only sane resting value. Starting
            // this at a large negative number (as if it were an audio-level
            // envelope) made `wet_gain`'s `db_to_lin(-env_db)` spike to a
            // huge boost for the first several attack time constants,
            // whatever hit the compressor first.
            env_db: 0.0,
        }
    }

    fn process(
        &mut self,
        params: &[f64],
        left: &mut [f32],
        right: &mut [f32],
        sidechain: Option<[&[f32]; 2]>,
        sr: f32,
    ) {
        let thresh_db = params[0] as f32;
        let ratio = (params[1] as f32).max(1.0);
        let attack_ms = params[2] as f32;
        let release_ms = params[3] as f32;
        let makeup_lin = db_to_lin(params[4] as f32);
        let mix = (params[5] as f32 * 0.01).clamp(0.0, 1.0);

        let hpf_c = lp_coeff(90.0, sr);
        let att_c = one_pole_coeff(attack_ms, sr);
        let rel_c = one_pole_coeff(release_ms, sr);
        let knee = 4.0_f32;
        let half_knee = knee * 0.5;
        let slope = 1.0 - 1.0 / ratio;

        for i in 0..left.len() {
            let (kl, kr) = match sidechain {
                Some([sc_l, sc_r]) => (sc_l[i], sc_r[i]),
                None => (left[i], right[i]),
            };
            self.sc_hpf_l = hpf_c * (self.sc_hpf_l + kl - self.sc_prev_l) + ANTI_DENORMAL;
            self.sc_prev_l = kl;
            self.sc_hpf_r = hpf_c * (self.sc_hpf_r + kr - self.sc_prev_r) + ANTI_DENORMAL;
            self.sc_prev_r = kr;

            let level = self.sc_hpf_l.abs().max(self.sc_hpf_r.abs());
            let in_db = lin_to_db(level).max(-96.0);
            let over = in_db - thresh_db;
            let target_gr_db = if over <= -half_knee {
                0.0
            } else if over < half_knee {
                let d = over + half_knee;
                slope * (d * d) / (2.0 * knee)
            } else {
                slope * over
            };

            let c = if target_gr_db > self.env_db {
                att_c
            } else {
                rel_c
            };
            self.env_db = c * self.env_db + (1.0 - c) * target_gr_db;
            let wet_gain = db_to_lin(-self.env_db) * makeup_lin;

            let dl = left[i];
            let dr = right[i];
            left[i] = dl * (1.0 - mix) + dl * wet_gain * mix;
            right[i] = dr * (1.0 - mix) + dr * wet_gain * mix;
        }
    }
}

#[derive(Debug)]
struct DriveState {
    hpf_z: [f32; 2],
    hpf_prev: [f32; 2],
    mid_hump: StereoBiquad,
    tone_z: [f32; 2],
    dc_z: [f32; 2],
    dc_prev: [f32; 2],
    os_prev: [f32; 2],
    last_tight: bool,
}

impl DriveState {
    fn new(sr: f32) -> Self {
        let mut mid_hump = StereoBiquad::bypass();
        mid_hump.set_peaking(720.0, 0.85, 3.5, sr);
        Self {
            hpf_z: [0.0; 2],
            hpf_prev: [0.0; 2],
            mid_hump,
            tone_z: [0.0; 2],
            dc_z: [0.0; 2],
            dc_prev: [0.0; 2],
            os_prev: [0.0; 2],
            last_tight: true,
        }
    }

    fn process(&mut self, params: &[f64], left: &mut [f32], right: &mut [f32], sr: f32) {
        let drive = params[0] as f32;
        let tone = params[1] as f32;
        let level_lin = db_to_lin(params[2] as f32);
        let tight = params[3] >= 0.5;
        let mix = (params[4] as f32 * 0.01).clamp(0.0, 1.0);

        if tight != self.last_tight {
            self.last_tight = tight;
            self.mid_hump
                .set_peaking(if tight { 820.0 } else { 700.0 }, 0.85, 3.5, sr);
        }

        let hpf_c = lp_coeff(if tight { 240.0 } else { 110.0 }, sr);
        let tone_hz = 850.0 * (7.5_f32).powf(tone / 10.0);
        let tone_c = lp_coeff(tone_hz, sr);
        let dc_c = lp_coeff(18.0, sr);
        let pre_gain = 1.5 + drive * 2.4;
        let asym = 0.14_f32;
        let asym_dc = asym.tanh();

        for i in 0..left.len() {
            let dry = [left[i], right[i]];
            let mut out = [0.0_f32; 2];
            for ch in 0..2 {
                let x = dry[ch];
                self.hpf_z[ch] = hpf_c * (self.hpf_z[ch] + x - self.hpf_prev[ch]) + ANTI_DENORMAL;
                self.hpf_prev[ch] = x;
                let humped = self.mid_hump.process_channel(ch, self.hpf_z[ch]);

                // 2x oversampled asymmetrical soft clipping.
                let mid = 0.5 * (self.os_prev[ch] + humped);
                self.os_prev[ch] = humped;
                let s0 = ((mid * pre_gain + asym).tanh() - asym_dc) * 0.7;
                let s1 = ((humped * pre_gain + asym).tanh() - asym_dc) * 0.7;
                let clipped = 0.5 * (s0 + s1);

                self.tone_z[ch] = tone_c * self.tone_z[ch] + (1.0 - tone_c) * clipped;
                let toned = self.tone_z[ch];
                self.dc_z[ch] = dc_c * (self.dc_z[ch] + toned - self.dc_prev[ch]);
                self.dc_prev[ch] = toned;
                out[ch] = dry[ch] * (1.0 - mix) + self.dc_z[ch] * level_lin * mix;
            }
            left[i] = out[0];
            right[i] = out[1];
        }
    }
}

#[derive(Debug)]
struct AmpState {
    in_hpf: StereoBiquad,
    miller_lpf: StereoBiquad,
    bass_eq: StereoBiquad,
    mid_eq: StereoBiquad,
    treble_eq: StereoBiquad,
    presence_eq: StereoBiquad,
    depth_eq: StereoBiquad,
    dc_z: [f32; 2],
    dc_prev: [f32; 2],
    os_prev: [f32; 2],
    sag_env: f32,
    cached: [f64; 9],
}

fn defaults<const N: usize>(spec: &[BuiltinParam]) -> [f64; N] {
    let mut out = [0.0; N];
    for (dst, p) in out.iter_mut().zip(spec.iter()) {
        *dst = p.default;
    }
    out
}

impl AmpState {
    fn new(sr: f32) -> Self {
        let cached = defaults::<9>(AMP_PARAMS);
        let mut s = Self {
            in_hpf: StereoBiquad::bypass(),
            miller_lpf: StereoBiquad::bypass(),
            bass_eq: StereoBiquad::bypass(),
            mid_eq: StereoBiquad::bypass(),
            treble_eq: StereoBiquad::bypass(),
            presence_eq: StereoBiquad::bypass(),
            depth_eq: StereoBiquad::bypass(),
            dc_z: [0.0; 2],
            dc_prev: [0.0; 2],
            os_prev: [0.0; 2],
            sag_env: 0.0,
            cached,
        };
        s.update_filters(&cached, sr);
        s
    }

    fn update_filters(&mut self, params: &[f64], sr: f32) {
        let ch = (params[0].round() as usize).min(3);
        let in_cut = match ch {
            0 => 70.0,
            1 => 90.0,
            2 => 115.0,
            _ => 155.0,
        };
        let miller_hz = match ch {
            0 => 9500.0,
            1 => 7500.0,
            2 => 6200.0,
            _ => 5600.0,
        };
        self.in_hpf.set_highpass(in_cut, 0.707, sr);
        self.miller_lpf.set_lowpass(miller_hz, 0.707, sr);
        self.bass_eq
            .set_low_shelf(115.0, 0.85, (params[2] as f32 - 5.0) * 2.0, sr);
        self.mid_eq
            .set_peaking(650.0, 0.9, (params[3] as f32 - 5.0) * 2.2, sr);
        self.treble_eq
            .set_high_shelf(2600.0, 0.85, (params[4] as f32 - 5.0) * 2.0, sr);
        self.presence_eq
            .set_high_shelf(4500.0, 0.9, (params[5] as f32 - 5.0) * 1.6, sr);
        self.depth_eq
            .set_peaking(85.0, 1.1, (params[6] as f32 - 5.0) * 1.6, sr);
    }

    fn process(&mut self, params: &[f64], left: &mut [f32], right: &mut [f32], sr: f32) {
        if self.cached[..].iter().zip(params).any(|(a, b)| *a != *b) {
            for (dst, src) in self.cached.iter_mut().zip(params.iter()) {
                *dst = *src;
            }
            self.update_filters(params, sr);
        }

        let channel = (params[0].round() as usize).min(3);
        let gain = params[1] as f32;
        let sag_amt = params[7] as f32 * 0.1;
        let master_lin = db_to_lin(params[8] as f32);

        let (stages, stage_drive, makeup) = match channel {
            0 => (2usize, 0.8 + gain * 0.25, 0.85),
            1 => (3, 1.4 + gain * 0.55, 0.65),
            2 => (4, 2.0 + gain * 0.95, 0.52),
            _ => (4, 2.6 + gain * 1.25, 0.48),
        };

        let sag_att = one_pole_coeff(12.0, sr);
        let sag_rel = one_pole_coeff(140.0, sr);
        let dc_c = lp_coeff(16.0, sr);

        for i in 0..left.len() {
            let level = left[i].abs().max(right[i].abs());
            let sc = if level > self.sag_env {
                sag_att
            } else {
                sag_rel
            };
            self.sag_env = sc * self.sag_env + (1.0 - sc) * level + ANTI_DENORMAL;
            let rail = 1.0 / (1.0 + sag_amt * 1.8 * self.sag_env);

            let dry = [left[i], right[i]];
            let mut out = [0.0_f32; 2];
            for ch in 0..2 {
                let x = self.in_hpf.process_channel(ch, dry[ch]);
                let mid = 0.5 * (self.os_prev[ch] + x);
                self.os_prev[ch] = x;

                let run_tube = |mut s: f32| -> f32 {
                    for st in 0..stages {
                        let bias = if st % 2 == 0 { 0.08 } else { -0.05 };
                        let driven = s * stage_drive * rail + bias;
                        s = (driven / (1.0 + 0.35 * driven.abs())).tanh() - bias * 0.4;
                    }
                    s
                };
                let pre = 0.5 * (run_tube(mid) + run_tube(x)) * makeup;
                let mil = self.miller_lpf.process_channel(ch, pre);
                let t1 = self.bass_eq.process_channel(ch, mil);
                let t2 = self.mid_eq.process_channel(ch, t1);
                let t3 = self.treble_eq.process_channel(ch, t2);
                let t4 = self.presence_eq.process_channel(ch, t3);
                let t5 = self.depth_eq.process_channel(ch, t4);

                self.dc_z[ch] = dc_c * (self.dc_z[ch] + t5 - self.dc_prev[ch]);
                self.dc_prev[ch] = t5;
                out[ch] = self.dc_z[ch] * master_lin;
            }
            left[i] = out[0];
            right[i] = out[1];
        }
    }
}

#[derive(Debug)]
struct EqState {
    hpf: StereoBiquad,
    low: StereoBiquad,
    low_mid: StereoBiquad,
    mid: StereoBiquad,
    high_mid: StereoBiquad,
    high: StereoBiquad,
    cached: [f64; 9],
}

impl EqState {
    fn new(sr: f32) -> Self {
        let cached = defaults::<9>(EQ_PARAMS);
        let mut s = Self {
            hpf: StereoBiquad::bypass(),
            low: StereoBiquad::bypass(),
            low_mid: StereoBiquad::bypass(),
            mid: StereoBiquad::bypass(),
            high_mid: StereoBiquad::bypass(),
            high: StereoBiquad::bypass(),
            cached,
        };
        s.update(&cached, sr);
        s
    }

    fn update(&mut self, params: &[f64], sr: f32) {
        self.hpf.set_highpass(params[0] as f32, 0.707, sr);
        self.low.set_low_shelf(100.0, 0.85, params[1] as f32, sr);
        self.low_mid.set_peaking(280.0, 1.0, params[2] as f32, sr);
        self.mid
            .set_peaking(params[3] as f32, params[5] as f32, params[4] as f32, sr);
        self.high_mid.set_peaking(2400.0, 1.1, params[6] as f32, sr);
        self.high.set_high_shelf(6000.0, 0.85, params[7] as f32, sr);
    }

    fn process(&mut self, params: &[f64], left: &mut [f32], right: &mut [f32], sr: f32) {
        if self.cached[..].iter().zip(params).any(|(a, b)| *a != *b) {
            for (dst, src) in self.cached.iter_mut().zip(params.iter()) {
                *dst = *src;
            }
            self.update(params, sr);
        }
        let out_gain = db_to_lin(params[8] as f32);
        for i in 0..left.len() {
            let mut l = self.hpf.process_channel(0, left[i]);
            l = self.low.process_channel(0, l);
            l = self.low_mid.process_channel(0, l);
            l = self.mid.process_channel(0, l);
            l = self.high_mid.process_channel(0, l);
            l = self.high.process_channel(0, l);
            left[i] = l * out_gain;

            let mut r = self.hpf.process_channel(1, right[i]);
            r = self.low.process_channel(1, r);
            r = self.low_mid.process_channel(1, r);
            r = self.mid.process_channel(1, r);
            r = self.high_mid.process_channel(1, r);
            r = self.high.process_channel(1, r);
            right[i] = r * out_gain;
        }
    }
}

#[derive(Debug)]
struct CabState {
    low_cut: StereoBiquad,
    cab_res: StereoBiquad,
    cab_scoop: StereoBiquad,
    cab_presence: StereoBiquad,
    cab_notch: StereoBiquad,
    mic_voice: StereoBiquad,
    high_cut: StereoBiquad,
    room_l: Vec<f32>,
    room_r: Vec<f32>,
    write: usize,
    cached: [f64; 7],
}

impl CabState {
    fn new(sr: f32) -> Self {
        let room_len = ((ROOM_MAX_MS * 0.001 * sr).ceil() as usize).max(64);
        let cached = defaults::<7>(CAB_PARAMS);
        let mut s = Self {
            low_cut: StereoBiquad::bypass(),
            cab_res: StereoBiquad::bypass(),
            cab_scoop: StereoBiquad::bypass(),
            cab_presence: StereoBiquad::bypass(),
            cab_notch: StereoBiquad::bypass(),
            mic_voice: StereoBiquad::bypass(),
            high_cut: StereoBiquad::bypass(),
            room_l: vec![0.0; room_len],
            room_r: vec![0.0; room_len],
            write: 0,
            cached,
        };
        s.update(&cached, sr);
        s
    }

    fn update(&mut self, params: &[f64], sr: f32) {
        let cab = (params[0].round() as usize).min(5);
        let mic = (params[1].round() as usize).min(2);
        let dist = (params[2] as f32 * 0.01).clamp(0.0, 1.0);
        let low_cut = params[3] as f32;
        let high_cut = params[4] as f32;

        let (res_hz, res_db, scoop_hz, scoop_db, pres_hz, pres_db, notch_hz) = match cab {
            0 => (135.0, 4.0, 480.0, -3.5, 2100.0, 4.5, 4400.0),
            1 => (115.0, 5.0, 420.0, -4.0, 2600.0, 5.0, 4100.0),
            2 => (100.0, 6.0, 380.0, -4.5, 2300.0, 4.0, 3900.0),
            3 => (85.0, 6.5, 450.0, -6.0, 2900.0, 5.5, 4300.0),
            // Bass cabinets: the resonance peak sits an octave or more
            // lower (larger drivers, bigger enclosures), with a gentler
            // low-mid scoop and the presence bump moved down so it adds
            // string "growl" rather than guitar-style top-end sparkle.
            4 => (90.0, 5.0, 550.0, -2.0, 1800.0, 3.0, 4600.0),
            _ => (70.0, 6.0, 500.0, -2.5, 1600.0, 2.5, 4200.0),
        };
        let (mic_hz, mic_q, mic_db, mic_hf_scale) = match mic {
            0 => (2800.0, 1.2, 3.0, 0.92),
            1 => (220.0, 0.9, 3.5, 0.68),
            _ => (5200.0, 1.0, 2.5, 1.05),
        };

        let eff_hc = (high_cut * mic_hf_scale * (1.0 - dist * 0.35)).clamp(1200.0, 16000.0);
        self.low_cut.set_highpass(low_cut, 0.85, sr);
        self.cab_res
            .set_peaking(res_hz, 1.6, res_db * (1.0 - dist * 0.25), sr);
        self.cab_scoop.set_peaking(scoop_hz, 1.1, scoop_db, sr);
        self.cab_presence.set_peaking(pres_hz, 1.4, pres_db, sr);
        self.cab_notch.set_notch(notch_hz, 3.0, sr);
        self.mic_voice.set_peaking(mic_hz, mic_q, mic_db, sr);
        self.high_cut.set_lowpass(eff_hc, 0.85, sr);
    }

    fn process(&mut self, params: &[f64], left: &mut [f32], right: &mut [f32], sr: f32) {
        if self.cached[..].iter().zip(params).any(|(a, b)| *a != *b) {
            for (dst, src) in self.cached.iter_mut().zip(params.iter()) {
                *dst = *src;
            }
            self.update(params, sr);
        }
        let room = (params[5] as f32 * 0.01).clamp(0.0, 1.0) * 0.35;
        let out_lin = db_to_lin(params[6] as f32);
        let t1 = (0.007 * sr) as usize;
        let t2 = (0.013 * sr) as usize;
        let t3 = (0.021 * sr) as usize;
        let t4 = (0.031 * sr) as usize;
        let len = self.room_l.len();

        for i in 0..left.len() {
            let mut l = self.low_cut.process_channel(0, left[i]);
            l = self.cab_res.process_channel(0, l);
            l = self.cab_scoop.process_channel(0, l);
            l = self.cab_presence.process_channel(0, l);
            l = self.cab_notch.process_channel(0, l);
            l = self.mic_voice.process_channel(0, l);
            l = self.high_cut.process_channel(0, l);

            let mut r = self.low_cut.process_channel(1, right[i]);
            r = self.cab_res.process_channel(1, r);
            r = self.cab_scoop.process_channel(1, r);
            r = self.cab_presence.process_channel(1, r);
            r = self.cab_notch.process_channel(1, r);
            r = self.mic_voice.process_channel(1, r);
            r = self.high_cut.process_channel(1, r);

            self.room_l[self.write] = l;
            self.room_r[self.write] = r;
            let refl_l = 0.45 * read_int(&self.room_r, self.write, t1)
                + 0.30 * read_int(&self.room_l, self.write, t2)
                - 0.25 * read_int(&self.room_r, self.write, t3)
                + 0.15 * read_int(&self.room_l, self.write, t4);
            let refl_r = 0.45 * read_int(&self.room_l, self.write, t1)
                + 0.30 * read_int(&self.room_r, self.write, t2)
                - 0.25 * read_int(&self.room_l, self.write, t3)
                + 0.15 * read_int(&self.room_r, self.write, t4);
            self.write = (self.write + 1) % len;

            left[i] = (l + refl_l * room) * out_lin;
            right[i] = (r + refl_r * room) * out_lin;
        }
    }
}

#[derive(Debug)]
struct ChorusState {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write: usize,
    phase: f32,
    lp_l: f32,
    lp_r: f32,
}

impl ChorusState {
    fn new(sr: f32) -> Self {
        let len = ((CHORUS_MAX_MS * 0.001 * sr).ceil() as usize).max(64);
        Self {
            buf_l: vec![0.0; len],
            buf_r: vec![0.0; len],
            write: 0,
            phase: 0.0,
            lp_l: 0.0,
            lp_r: 0.0,
        }
    }

    fn process(&mut self, params: &[f64], left: &mut [f32], right: &mut [f32], sr: f32) {
        let rate = params[0] as f32;
        let depth_ms = (params[1] as f32 * 0.01) * 6.0;
        let base_ms = params[2] as f32;
        let warmth = (params[3] as f32 * 0.01).clamp(0.0, 1.0);
        let width = (params[4] as f32 * 0.01).clamp(0.0, 1.0);
        let mix = (params[5] as f32 * 0.01).clamp(0.0, 1.0);

        let inc = TAU * rate / sr;
        let lp_hz = 12000.0 * (0.18_f32).powf(warmth);
        let lp_c = lp_coeff(lp_hz, sr);
        let len = self.buf_l.len();

        for i in 0..left.len() {
            let dl = left[i];
            let dr = right[i];
            self.buf_l[self.write] = dl;
            self.buf_r[self.write] = dr;

            let lfo0 = (self.phase).sin();
            let lfo1 = (self.phase + TAU / 3.0).sin();
            let lfo2 = (self.phase + 2.0 * TAU / 3.0).sin();
            self.phase += inc;
            if self.phase >= TAU {
                self.phase -= TAU;
            }

            let d0 = ((base_ms + lfo0 * depth_ms) * 0.001 * sr).max(1.0);
            let d1 = ((base_ms + 1.5 + lfo1 * depth_ms) * 0.001 * sr).max(1.0);
            let d2 = ((base_ms + 3.0 + lfo2 * depth_ms) * 0.001 * sr).max(1.0);

            let v0 = read_frac(&self.buf_l, self.write, d0);
            let v1 = 0.5
                * (read_frac(&self.buf_l, self.write, d1) + read_frac(&self.buf_r, self.write, d1));
            let v2 = read_frac(&self.buf_r, self.write, d2);

            let wet_raw_l = (0.7 * v0 + 0.5 * v1 + 0.2 * v2) * 0.65;
            let wet_raw_r = (0.2 * v0 + 0.5 * v1 + 0.7 * v2) * 0.65;
            let mid = 0.5 * (wet_raw_l + wet_raw_r);
            let side = 0.5 * (wet_raw_l - wet_raw_r) * width;

            self.lp_l = lp_c * self.lp_l + (1.0 - lp_c) * (mid + side) + ANTI_DENORMAL;
            self.lp_r = lp_c * self.lp_r + (1.0 - lp_c) * (mid - side) + ANTI_DENORMAL;
            self.write = (self.write + 1) % len;

            left[i] = dl * (1.0 - mix) + self.lp_l * mix;
            right[i] = dr * (1.0 - mix) + self.lp_r * mix;
        }
    }
}

#[derive(Debug)]
struct DelayState {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write: usize,
    smooth_samples: f32,
    lfo_phase: f32,
    fb_lp_l: f32,
    fb_lp_r: f32,
    duck_env: f32,
}

impl DelayState {
    fn new(sr: f32) -> Self {
        let len = ((DELAY_MAX_MS * 0.001 * sr).ceil() as usize).max(128);
        Self {
            buf_l: vec![0.0; len],
            buf_r: vec![0.0; len],
            write: 0,
            smooth_samples: 0.375 * sr,
            lfo_phase: 0.0,
            fb_lp_l: 0.0,
            fb_lp_r: 0.0,
            duck_env: 0.0,
        }
    }

    fn process(&mut self, params: &[f64], left: &mut [f32], right: &mut [f32], sr: f32, bpm: f64) {
        let time_ms = params[0] as f32;
        let sync = (params[1].round() as usize).min(6);
        let fb = (params[2] as f32 * 0.01).clamp(0.0, 0.95);
        let pp = (params[3] as f32 * 0.01).clamp(0.0, 1.0);
        let tone = (params[4] as f32 * 0.01).clamp(0.0, 1.0);
        let mod_depth = (params[5] as f32 * 0.01).clamp(0.0, 1.0);
        let duck = (params[6] as f32 * 0.01).clamp(0.0, 1.0);
        let mix = (params[7] as f32 * 0.01).clamp(0.0, 1.0);

        let eff_ms = if sync == 0 {
            time_ms
        } else {
            let beat_mult = match sync {
                1 => 0.25,
                2 => 0.5,
                3 => 0.75,
                4 => 1.0 / 3.0,
                5 => 1.0,
                _ => 2.0,
            };
            let beat_ms = 60_000.0 / (bpm as f32).clamp(20.0, 999.0);
            (beat_ms * beat_mult).clamp(10.0, 2000.0)
        };
        let target_samples = eff_ms * 0.001 * sr;
        let slew_c = one_pole_coeff(25.0, sr);
        let lfo_inc = TAU * 0.75 / sr;
        let mod_samples = mod_depth * 0.0025 * sr;
        let tone_hz = 1000.0 * (12.0_f32).powf(tone);
        let tone_c = lp_coeff(tone_hz, sr);
        let duck_att = one_pole_coeff(8.0, sr);
        let duck_rel = one_pole_coeff(180.0, sr);
        let len = self.buf_l.len();

        for i in 0..left.len() {
            let dl = left[i];
            let dr = right[i];
            let lvl = dl.abs().max(dr.abs());
            let dc = if lvl > self.duck_env {
                duck_att
            } else {
                duck_rel
            };
            self.duck_env = dc * self.duck_env + (1.0 - dc) * lvl + ANTI_DENORMAL;
            let duck_gain = 1.0 / (1.0 + duck * 6.0 * self.duck_env);

            self.smooth_samples = slew_c * self.smooth_samples + (1.0 - slew_c) * target_samples;
            let lfo = self.lfo_phase.sin();
            self.lfo_phase += lfo_inc;
            if self.lfo_phase >= TAU {
                self.lfo_phase -= TAU;
            }
            let d = (self.smooth_samples + lfo * mod_samples).clamp(1.0, (len - 3) as f32);
            let tap_l = read_frac(&self.buf_l, self.write, d);
            let tap_r = read_frac(&self.buf_r, self.write, d);

            self.fb_lp_l = tone_c * self.fb_lp_l + (1.0 - tone_c) * tap_l + ANTI_DENORMAL;
            self.fb_lp_r = tone_c * self.fb_lp_r + (1.0 - tone_c) * tap_r + ANTI_DENORMAL;

            let cross_l = self.fb_lp_l * (1.0 - pp) + self.fb_lp_r * pp;
            let cross_r = self.fb_lp_r * (1.0 - pp) + self.fb_lp_l * pp;
            // On ping-pong, inject dry sum more heavily into left so repeats bounce L <-> R.
            let in_l = dl * (1.0 - 0.35 * pp) + dr * (0.35 * pp);
            let in_r = dr * (1.0 - 0.65 * pp);
            self.buf_l[self.write] = in_l + cross_l * fb;
            self.buf_r[self.write] = in_r + cross_r * fb;
            self.write = (self.write + 1) % len;

            left[i] = dl * (1.0 - mix) + tap_l * duck_gain * mix;
            right[i] = dr * (1.0 - mix) + tap_r * duck_gain * mix;
        }
    }
}

#[derive(Debug)]
struct Comb {
    buf: Vec<f32>,
    idx: usize,
    lp: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(8)],
            idx: 0,
            lp: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let out = self.buf[self.idx];
        self.lp = out * (1.0 - damp) + self.lp * damp + ANTI_DENORMAL;
        self.buf[self.idx] = input + self.lp * feedback;
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        out
    }
}

#[derive(Debug)]
struct AllPass {
    buf: Vec<f32>,
    idx: usize,
}

impl AllPass {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(4)],
            idx: 0,
        }
    }

    #[inline]
    fn process(&mut self, input: f32, coeff: f32) -> f32 {
        let buf_out = self.buf[self.idx];
        let out = -input + buf_out;
        self.buf[self.idx] = input + buf_out * coeff + ANTI_DENORMAL;
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        out
    }
}

#[derive(Debug)]
struct ReverbState {
    predelay_l: Vec<f32>,
    predelay_r: Vec<f32>,
    pd_write: usize,
    combs_l: [Comb; 4],
    combs_r: [Comb; 4],
    ap_l: [AllPass; 3],
    ap_r: [AllPass; 3],
    hpf_z: [f32; 2],
    hpf_prev: [f32; 2],
}

impl ReverbState {
    fn new(sr: f32) -> Self {
        let scale = sr / 44_100.0;
        let scaled = |n: usize| ((n as f32 * scale).round() as usize).max(4);
        let pd_len = ((REVERB_PREDELAY_MAX_MS * 0.001 * sr).ceil() as usize).max(32);
        Self {
            predelay_l: vec![0.0; pd_len],
            predelay_r: vec![0.0; pd_len],
            pd_write: 0,
            combs_l: [
                Comb::new(scaled(1116)),
                Comb::new(scaled(1188)),
                Comb::new(scaled(1277)),
                Comb::new(scaled(1356)),
            ],
            combs_r: [
                Comb::new(scaled(1116 + 23)),
                Comb::new(scaled(1188 + 23)),
                Comb::new(scaled(1277 + 23)),
                Comb::new(scaled(1356 + 23)),
            ],
            ap_l: [
                AllPass::new(scaled(556)),
                AllPass::new(scaled(441)),
                AllPass::new(scaled(341)),
            ],
            ap_r: [
                AllPass::new(scaled(556 + 23)),
                AllPass::new(scaled(441 + 23)),
                AllPass::new(scaled(341 + 23)),
            ],
            hpf_z: [0.0; 2],
            hpf_prev: [0.0; 2],
        }
    }

    fn process(&mut self, params: &[f64], left: &mut [f32], right: &mut [f32], sr: f32) {
        let mode = (params[0].round() as usize).min(2);
        let predelay_ms = params[1] as f32;
        let decay_s = (params[2] as f32).clamp(0.2, 12.0);
        let damping = (params[3] as f32 * 0.01).clamp(0.0, 1.0);
        let dispersion = (params[4] as f32 * 0.01).clamp(0.0, 1.0);
        let low_cut = params[5] as f32;
        let mix = (params[6] as f32 * 0.01).clamp(0.0, 1.0);

        let (fb_scale, ap_coeff, damp_scale) = match mode {
            0 => (0.86, 0.35 + dispersion * 0.25, 0.35), // Spring: brighter chirp
            1 => (0.94, 0.45 + dispersion * 0.20, 0.55), // Plate: dense smooth
            _ => (0.98, 0.50 + dispersion * 0.22, 0.70), // Hall: spacious tail
        };
        let rt60_samples = decay_s * sr;
        let mean_comb = 1240.0 * (sr / 44_100.0);
        let fb = (10.0_f32.powf(-3.0 * mean_comb / rt60_samples) * fb_scale).clamp(0.2, 0.96);
        let damp = (damping * damp_scale).clamp(0.02, 0.85);
        let pd_samples =
            ((predelay_ms * 0.001 * sr).round() as usize).min(self.predelay_l.len() - 1);
        let hpf_c = lp_coeff(low_cut, sr);
        let pd_len = self.predelay_l.len();

        for i in 0..left.len() {
            let dl = left[i];
            let dr = right[i];
            self.hpf_z[0] = hpf_c * (self.hpf_z[0] + dl - self.hpf_prev[0]) + ANTI_DENORMAL;
            self.hpf_prev[0] = dl;
            self.hpf_z[1] = hpf_c * (self.hpf_z[1] + dr - self.hpf_prev[1]) + ANTI_DENORMAL;
            self.hpf_prev[1] = dr;

            self.predelay_l[self.pd_write] = self.hpf_z[0];
            self.predelay_r[self.pd_write] = self.hpf_z[1];
            let pd_l = read_int(&self.predelay_l, self.pd_write, pd_samples);
            let pd_r = read_int(&self.predelay_r, self.pd_write, pd_samples);
            self.pd_write = (self.pd_write + 1) % pd_len;

            let mono_in = 0.25 * (pd_l + pd_r);
            let mut sum_l = 0.0_f32;
            let mut sum_r = 0.0_f32;
            for c in &mut self.combs_l {
                sum_l += c.process(mono_in, fb, damp);
            }
            for c in &mut self.combs_r {
                sum_r += c.process(mono_in, fb, damp);
            }
            for ap in &mut self.ap_l {
                sum_l = ap.process(sum_l, ap_coeff);
            }
            for ap in &mut self.ap_r {
                sum_r = ap.process(sum_r, ap_coeff);
            }
            let wet_l = sum_l * 0.35;
            let wet_r = sum_r * 0.35;
            left[i] = dl * (1.0 - mix) + wet_l * mix;
            right[i] = dr * (1.0 - mix) + wet_r * mix;
        }
    }
}

/// An envelope follower driving a resonant bandpass filter's center
/// frequency: a touch-wah pedal (louder input sweeps the filter up or down
/// instead of a pedal's rocker).
#[derive(Debug)]
struct AutoWahState {
    env: f32,
    filter: StereoBiquad,
}

impl AutoWahState {
    fn new(sr: f32) -> Self {
        let mut filter = StereoBiquad::bypass();
        filter.set_bandpass(500.0, 2.0, sr);
        Self { env: 0.0, filter }
    }

    fn process(&mut self, params: &[f64], left: &mut [f32], right: &mut [f32], sr: f32) {
        let sensitivity = (params[0] as f32 * 0.01).clamp(0.0, 1.0);
        let freq_low = (params[1] as f32).max(MIN_FILTER_HZ);
        let freq_high = (params[2] as f32).max(freq_low + 1.0);
        let q = (params[3] as f32).max(0.1);
        let attack_ms = params[4] as f32;
        let release_ms = params[5] as f32;
        let down = params[6] >= 0.5;
        let mix = (params[7] as f32 * 0.01).clamp(0.0, 1.0);

        let att_c = one_pole_coeff(attack_ms, sr);
        let rel_c = one_pole_coeff(release_ms, sr);
        let log_lo = freq_low.ln();
        let log_hi = freq_high.ln();
        // Higher sensitivity reaches the top of the sweep from a quieter
        // input, like opening a wah pedal further for the same pick attack.
        let gain = 1.0 + sensitivity * 9.0;

        for i in 0..left.len() {
            let level = left[i].abs().max(right[i].abs());
            let c = if level > self.env { att_c } else { rel_c };
            self.env = c * self.env + (1.0 - c) * level + ANTI_DENORMAL;

            let drive = (self.env * gain).clamp(0.0, 1.0);
            let t = if down { 1.0 - drive } else { drive };
            let freq = (log_lo + (log_hi - log_lo) * t).exp();
            self.filter.set_bandpass(freq, q, sr);

            let dl = left[i];
            let dr = right[i];
            let wl = self.filter.process_channel(0, dl);
            let wr = self.filter.process_channel(1, dr);
            left[i] = dl * (1.0 - mix) + wl * mix;
            right[i] = dr * (1.0 - mix) + wr * mix;
        }
    }
}

/// One stage of a phaser's swept allpass chain, direct form I: `prev_x`/
/// `prev_y` hold the last input/output per channel.
#[derive(Debug, Clone, Copy)]
struct PhaserAllpass {
    a: f32,
    prev_x: [f32; 2],
    prev_y: [f32; 2],
}

impl PhaserAllpass {
    fn new() -> Self {
        Self {
            a: 0.0,
            prev_x: [0.0; 2],
            prev_y: [0.0; 2],
        }
    }

    #[inline]
    fn process_channel(&mut self, ch: usize, x: f32) -> f32 {
        let y = -self.a * x + self.prev_x[ch] + self.a * self.prev_y[ch];
        self.prev_x[ch] = x;
        self.prev_y[ch] = y + ANTI_DENORMAL;
        y
    }
}

const PHASER_MAX_STAGES: usize = 8;

/// A chain of LFO-swept allpass stages (a classic analog phaser): each
/// stage's break frequency sweeps together, carving moving notches into
/// the spectrum. Feedback around the chain deepens and narrows them.
#[derive(Debug)]
struct PhaserState {
    stages: [PhaserAllpass; PHASER_MAX_STAGES],
    phase: f32,
    fb_l: f32,
    fb_r: f32,
}

impl PhaserState {
    fn new() -> Self {
        Self {
            stages: [PhaserAllpass::new(); PHASER_MAX_STAGES],
            phase: 0.0,
            fb_l: 0.0,
            fb_r: 0.0,
        }
    }

    fn process(&mut self, params: &[f64], left: &mut [f32], right: &mut [f32], sr: f32) {
        let rate = params[0] as f32;
        let depth = (params[1] as f32 * 0.01).clamp(0.0, 1.0);
        let feedback = (params[2] as f32 * 0.01).clamp(0.0, 0.95);
        let stage_count = (4 + params[3].round() as usize * 2).min(PHASER_MAX_STAGES);
        let center = (params[4] as f32).max(MIN_FILTER_HZ);
        let width = (params[5] as f32 * 0.01).clamp(0.0, 1.0);
        let mix = (params[6] as f32 * 0.01).clamp(0.0, 1.0);

        let inc = TAU * rate / sr;
        let log_c = center.ln();
        // No depth means no sweep: a static phaser, notching at `center`.
        let half_range = (1.0 + depth * 3.0).ln();
        let r_offset = width * PI;

        for i in 0..left.len() {
            let dl = left[i];
            let dr = right[i];

            let lfo_l = self.phase.sin();
            let lfo_r = (self.phase + r_offset).sin();
            self.phase += inc;
            if self.phase >= TAU {
                self.phase -= TAU;
            }

            let freq_l = (log_c + lfo_l * half_range).exp();
            let freq_r = (log_c + lfo_r * half_range).exp();
            let a_l = allpass_coeff(freq_l, sr);
            let a_r = allpass_coeff(freq_r, sr);

            let mut yl = dl + self.fb_l * feedback;
            for stage in self.stages.iter_mut().take(stage_count) {
                stage.a = a_l;
                yl = stage.process_channel(0, yl);
            }
            let mut yr = dr + self.fb_r * feedback;
            for stage in self.stages.iter_mut().take(stage_count) {
                stage.a = a_r;
                yr = stage.process_channel(1, yr);
            }
            self.fb_l = yl;
            self.fb_r = yr;

            left[i] = dl * (1.0 - mix) + yl * mix;
            right[i] = dr * (1.0 - mix) + yr * mix;
        }
    }
}

#[derive(Debug)]
enum ProcessorKind {
    Gate(GateState),
    Comp(CompState),
    Drive(DriveState),
    Amp(AmpState),
    Eq(EqState),
    Cab(CabState),
    Chorus(ChorusState),
    Delay(DelayState),
    Reverb(Box<ReverbState>),
    AutoWah(AutoWahState),
    Phaser(PhaserState),
}

#[derive(Debug)]
pub struct RackProcessor {
    effect: RackEffect,
    sample_rate: f32,
    params: [f64; 10],
    state: ProcessorKind,
}

impl RackProcessor {
    pub fn new(effect: RackEffect, sample_rate: u32) -> Self {
        let sr = (sample_rate.max(8_000)) as f32;
        let spec = effect.spec();
        let mut params = [0.0; 10];
        for (i, p) in spec.params.iter().enumerate() {
            params[i] = p.default;
        }
        let state = match effect {
            RackEffect::Gate => ProcessorKind::Gate(GateState::new()),
            RackEffect::Comp => ProcessorKind::Comp(CompState::new()),
            RackEffect::Drive => ProcessorKind::Drive(DriveState::new(sr)),
            RackEffect::Amp => ProcessorKind::Amp(AmpState::new(sr)),
            RackEffect::Eq => ProcessorKind::Eq(EqState::new(sr)),
            RackEffect::Cab => ProcessorKind::Cab(CabState::new(sr)),
            RackEffect::Chorus => ProcessorKind::Chorus(ChorusState::new(sr)),
            RackEffect::Delay => ProcessorKind::Delay(DelayState::new(sr)),
            RackEffect::Reverb => ProcessorKind::Reverb(Box::new(ReverbState::new(sr))),
            RackEffect::AutoWah => ProcessorKind::AutoWah(AutoWahState::new(sr)),
            RackEffect::Phaser => ProcessorKind::Phaser(PhaserState::new()),
        };
        Self {
            effect,
            sample_rate: sr,
            params,
            state,
        }
    }

    pub fn set_param(&mut self, index: usize, value: f64) {
        if let Some(spec) = self.effect.spec().params.get(index)
            && value.is_finite()
        {
            let clamped = value.clamp(spec.min, spec.max);
            self.params[index] = if spec.stepped {
                clamped.round()
            } else {
                clamped
            };
        }
    }

    pub fn process(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        sidechain: Option<[&[f32]; 2]>,
        bpm: f64,
    ) {
        let n = self.effect.spec().params.len();
        let p = &self.params[..n];
        let sr = self.sample_rate;
        match &mut self.state {
            ProcessorKind::Gate(s) => s.process(p, left, right, sidechain, sr),
            ProcessorKind::Comp(s) => s.process(p, left, right, sidechain, sr),
            ProcessorKind::Drive(s) => s.process(p, left, right, sr),
            ProcessorKind::Amp(s) => s.process(p, left, right, sr),
            ProcessorKind::Eq(s) => s.process(p, left, right, sr),
            ProcessorKind::Cab(s) => s.process(p, left, right, sr),
            ProcessorKind::Chorus(s) => s.process(p, left, right, sr),
            ProcessorKind::Delay(s) => s.process(p, left, right, sr, bpm),
            ProcessorKind::Reverb(s) => s.process(p, left, right, sr),
            ProcessorKind::AutoWah(s) => s.process(p, left, right, sr),
            ProcessorKind::Phaser(s) => s.process(p, left, right, sr),
        }
    }
}

/// Evaluates the PEQ-500 frequency response in dB at `freq_hz` (48 kHz reference).
#[allow(clippy::too_many_arguments)]
pub fn eq_response_db(
    low_cut: f32,
    low_gain: f32,
    low_mid_gain: f32,
    mid_freq: f32,
    mid_gain: f32,
    mid_q: f32,
    high_mid_gain: f32,
    high_gain: f32,
    output: f32,
    freq_hz: f32,
) -> f32 {
    let sr = 48_000.0;
    let mut eq = EqState::new(sr);
    let params = [
        f64::from(low_cut),
        f64::from(low_gain),
        f64::from(low_mid_gain),
        f64::from(mid_freq),
        f64::from(mid_gain),
        f64::from(mid_q),
        f64::from(high_mid_gain),
        f64::from(high_gain),
        f64::from(output),
    ];
    eq.update(&params, sr);
    eq.hpf.magnitude_db(freq_hz, sr)
        + eq.low.magnitude_db(freq_hz, sr)
        + eq.low_mid.magnitude_db(freq_hz, sr)
        + eq.mid.magnitude_db(freq_hz, sr)
        + eq.high_mid.magnitude_db(freq_hz, sr)
        + eq.high.magnitude_db(freq_hz, sr)
        + output
}

/// Evaluates the CAB-IR frequency response in dB at `freq_hz` (48 kHz reference).
pub fn cab_response_db(
    cabinet: usize,
    mic: usize,
    distance: f32,
    low_cut: f32,
    high_cut: f32,
    freq_hz: f32,
) -> f32 {
    let sr = 48_000.0;
    let mut cab = CabState {
        low_cut: StereoBiquad::bypass(),
        cab_res: StereoBiquad::bypass(),
        cab_scoop: StereoBiquad::bypass(),
        cab_presence: StereoBiquad::bypass(),
        cab_notch: StereoBiquad::bypass(),
        mic_voice: StereoBiquad::bypass(),
        high_cut: StereoBiquad::bypass(),
        room_l: Vec::new(),
        room_r: Vec::new(),
        write: 0,
        cached: [f64::NAN; 7],
    };
    let params = [
        cabinet as f64,
        mic as f64,
        f64::from(distance),
        f64::from(low_cut),
        f64::from(high_cut),
        0.0,
        0.0,
    ];
    cab.update(&params, sr);
    cab.low_cut.magnitude_db(freq_hz, sr)
        + cab.cab_res.magnitude_db(freq_hz, sr)
        + cab.cab_scoop.magnitude_db(freq_hz, sr)
        + cab.cab_presence.magnitude_db(freq_hz, sr)
        + cab.cab_notch.magnitude_db(freq_hz, sr)
        + cab.mic_voice.magnitude_db(freq_hz, sr)
        + cab.high_cut.magnitude_db(freq_hz, sr)
}

#[cfg(test)]
mod tests {
    use assert_no_alloc::permit_alloc;

    use super::*;

    fn test_tone(len: usize, amp: f32) -> (Vec<f32>, Vec<f32>) {
        let mut l = Vec::with_capacity(len);
        let mut r = Vec::with_capacity(len);
        for i in 0..len {
            let s = amp * (TAU * 220.0 * i as f32 / 48_000.0).sin();
            l.push(s);
            r.push(s);
        }
        (l, r)
    }

    #[test]
    fn all_rack_effects_process_without_allocating() {
        for spec in builtin_specs() {
            let mut proc = RackProcessor::new(spec.effect, 48_000);
            let (mut l, mut r) = test_tone(512, 0.25);
            let (sc_l, sc_r) = test_tone(512, 0.5);
            assert_no_alloc::assert_no_alloc(|| {
                for _ in 0..8 {
                    proc.set_param(0, spec.params[0].default);
                    proc.process(&mut l, &mut r, Some([&sc_l, &sc_r]), 120.0);
                }
            });
            assert!(l.iter().all(|s| s.is_finite()), "{}", spec.id);
            assert!(r.iter().all(|s| s.is_finite()), "{}", spec.id);
            let energy: f32 = l.iter().map(|s| s * s).sum();
            assert!(energy > 1e-4, "{} produced silence", spec.id);
        }
    }

    #[test]
    fn comp_does_not_spike_on_a_cold_start_hit() {
        let mut comp = RackProcessor::new(RackEffect::Comp, 48_000);
        let (mut l, mut r) = test_tone(512, 0.8); // a loud hit right from sample 0
        assert_no_alloc::assert_no_alloc(|| comp.process(&mut l, &mut r, None, 120.0));
        let peak = l.iter().map(|s| s.abs()).fold(0.0_f32, f32::max);
        assert!(
            peak < 2.0,
            "a fresh compressor shouldn't boost its first input: peak={peak}"
        );
    }

    #[test]
    fn gate_attenuates_quiet_signal_and_opens_above_threshold() {
        let mut gate = RackProcessor::new(RackEffect::Gate, 48_000);
        gate.set_param(0, -20.0); // threshold
        gate.set_param(1, -60.0); // floor
        gate.set_param(4, 1.0); // hold 1 ms
        gate.set_param(5, 5.0); // fast release
        let (mut l, mut r) = test_tone(4096, 0.01); // -40 dBFS
        assert_no_alloc::assert_no_alloc(|| {
            gate.process(&mut l, &mut r, None, 120.0);
        });
        let quiet_peak = l[3000..].iter().map(|s| s.abs()).fold(0.0_f32, f32::max);
        assert!(quiet_peak < 0.0005, "gate should attenuate: {quiet_peak}");

        let (mut l2, mut r2) = test_tone(4096, 0.5); // -6 dBFS
        gate.process(&mut l2, &mut r2, None, 120.0);
        let loud_peak = l2[2000..].iter().map(|s| s.abs()).fold(0.0_f32, f32::max);
        assert!(loud_peak > 0.4, "gate should open: {loud_peak}");
    }

    #[test]
    fn delay_echoes_impulse_at_expected_time() {
        let mut delay = RackProcessor::new(RackEffect::Delay, 48_000);
        delay.set_param(0, 100.0); // 100 ms = 4800 samples
        delay.set_param(1, 0.0); // Free
        delay.set_param(2, 0.0); // 0% feedback
        delay.set_param(3, 0.0); // mono
        delay.set_param(5, 0.0); // no modulation
        delay.set_param(6, 0.0); // no duck
        delay.set_param(7, 50.0); // 50% mix
        let mut l = vec![0.0_f32; 6000];
        let mut r = vec![0.0_f32; 6000];
        l[0] = 1.0;
        r[0] = 1.0;
        // Let delay time smoother settle first on silence.
        let mut warm_l = vec![0.0_f32; 24_000];
        let mut warm_r = vec![0.0_f32; 24_000];
        delay.process(&mut warm_l, &mut warm_r, None, 120.0);
        assert_no_alloc::assert_no_alloc(|| {
            delay.process(&mut l, &mut r, None, 120.0);
        });
        assert!(
            l[4800].abs() > 0.3,
            "expected echo at 4800, got {}",
            l[4800]
        );
    }

    #[test]
    fn autowah_sweeps_filter_frequency_with_envelope() {
        let sr = 48_000.0_f32;
        let tone_hz = 300.0_f32;
        let make_tone = |len: usize, amp: f32| -> (Vec<f32>, Vec<f32>) {
            let mut l = Vec::with_capacity(len);
            let mut r = Vec::with_capacity(len);
            for i in 0..len {
                let s = amp * (TAU * tone_hz * i as f32 / sr).sin();
                l.push(s);
                r.push(s);
            }
            (l, r)
        };
        let gain_at = |amp: f32, direction: f64| -> f32 {
            let mut proc = RackProcessor::new(RackEffect::AutoWah, 48_000);
            proc.set_param(1, f64::from(tone_hz)); // freq_low == the test tone
            proc.set_param(2, 2200.0); // freq_high, far from the test tone
            proc.set_param(4, 1.0); // fast attack
            proc.set_param(5, 5.0); // fast release, settles within the block
            proc.set_param(6, direction);
            let (mut l, mut r) = make_tone(4096, amp);
            assert_no_alloc::assert_no_alloc(|| proc.process(&mut l, &mut r, None, 120.0));
            l[3000..].iter().map(|s| s.abs()).fold(0.0_f32, f32::max) / amp
        };

        // Up (default): a quiet input keeps the filter near freq_low, right
        // on the test tone, so it passes through; a loud one sweeps the
        // filter up toward freq_high, away from the tone, attenuating it.
        let quiet_gain = gain_at(0.02, 0.0);
        let loud_gain = gain_at(0.9, 0.0);
        assert!(
            quiet_gain > loud_gain * 1.5,
            "quiet (filter near tone freq) should pass more than loud (swept away): quiet={quiet_gain} loud={loud_gain}"
        );

        // Down reverses it: a loud input now settles the filter near
        // freq_low too, so it should pass about as well as the quiet
        // Up case, not like loud Up's attenuated case.
        let down_gain = gain_at(0.9, 1.0);
        assert!(
            down_gain > loud_gain * 1.5,
            "loud Down should settle near freq_low like quiet Up, not attenuate like loud Up: down={down_gain} loud={loud_gain}"
        );
    }

    #[test]
    fn phaser_mix_zero_is_bypass() {
        let mut proc = RackProcessor::new(RackEffect::Phaser, 48_000);
        proc.set_param(6, 0.0); // mix: 0% = pure dry
        let (mut l, mut r) = test_tone(2048, 0.4);
        let dry = l.clone();
        assert_no_alloc::assert_no_alloc(|| proc.process(&mut l, &mut r, None, 120.0));
        for (wet, dry) in l.iter().zip(dry.iter()) {
            assert!((wet - dry).abs() < 1e-5, "wet={wet} dry={dry}");
        }
    }

    #[test]
    fn phaser_sweeps_without_blowing_up() {
        let mut proc = RackProcessor::new(RackEffect::Phaser, 48_000);
        proc.set_param(2, 90.0); // high feedback
        proc.set_param(6, 60.0); // mix
        let (mut l, mut r) = test_tone(48_000, 0.4);
        assert_no_alloc::assert_no_alloc(|| proc.process(&mut l, &mut r, None, 120.0));
        // Resonant feedback peaks can exceed unity gain at some frequencies;
        // this only guards against genuine blow-up (NaN/inf or runaway gain).
        assert!(
            l.iter().all(|s| s.is_finite() && s.abs() < 10.0),
            "phaser output should stay bounded with high feedback"
        );
    }

    #[test]
    fn eq_and_cab_curves_respond_to_parameters() {
        let flat = eq_response_db(20.0, 0.0, 0.0, 1000.0, 0.0, 1.2, 0.0, 0.0, 0.0, 1000.0);
        let boosted = eq_response_db(20.0, 0.0, 0.0, 1000.0, 10.0, 1.2, 0.0, 0.0, 0.0, 1000.0);
        assert!(flat.abs() < 0.1, "{flat}");
        assert!((boosted - 10.0).abs() < 0.2, "{boosted}");

        let cab_low = cab_response_db(2, 0, 25.0, 80.0, 6500.0, 1000.0);
        let cab_sub = cab_response_db(2, 0, 25.0, 80.0, 6500.0, 25.0);
        let cab_fizz = cab_response_db(2, 0, 25.0, 80.0, 6500.0, 15000.0);
        assert!(cab_low > cab_sub + 10.0, "{cab_low} vs {cab_sub}");
        assert!(cab_low > cab_fizz + 10.0, "{cab_low} vs {cab_fizz}");

        // The bass cabinets (4, 5) resonate an octave or so lower than any
        // guitar cabinet (90 and 70 Hz, vs 85-135 Hz): right at the lowest
        // guitar cabinet's own resonance, "1x15 Bass" still has headroom to
        // spare while the guitar cabinets are already past their peak.
        let hz = 70.0;
        let guitar = (0..4)
            .map(|c| cab_response_db(c, 1, 25.0, 20.0, 5000.0, hz))
            .fold(f32::MIN, f32::max);
        let bass = cab_response_db(5, 1, 25.0, 20.0, 5000.0, hz);
        assert!(bass > guitar + 1.0, "bass {bass} vs guitar's best {guitar}");
        permit_alloc(|| ());
    }

    /// A tiny deterministic noise generator (xorshift32), so the test needs
    /// no RNG dependency and is reproducible.
    fn broadband(len: usize, seed: &mut u32) -> Vec<f32> {
        (0..len)
            .map(|_| {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 17;
                *seed ^= *seed << 5;
                (*seed as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect()
    }

    /// `dawkey_plugins::Host::load_for` unconditionally tells the host every
    /// built-in rack unit has zero latency (`PluginFormat::Builtin` always
    /// gets `latency: 0`), unlike CLAP/LV2/VST3, which are asked. That's
    /// only honest if no unit secretly buffers its input before responding.
    /// This pins it at the DSP level, independent of the host's claim:
    /// every unit must respond to a loud, broadband signal (on its main
    /// input and its sidechain, where it takes one) within a tolerance wide
    /// enough for ordinary envelope/filter settling, but far short of what
    /// real lookahead or an oversampling filter's group delay would cost.
    #[test]
    fn builtin_effects_respond_without_hidden_latency() {
        const SILENCE: usize = 512;
        const SIGNAL: usize = 4096;
        // ~5 ms at 48 kHz: generous for attack/filter settling, nowhere
        // near what unreported lookahead or an oversampling filter costs.
        const TOLERANCE: usize = 240;
        const THRESHOLD: f32 = 0.02;

        for spec in builtin_specs() {
            let mut proc = RackProcessor::new(spec.effect, 48_000);
            let mut seed_l = 0x1234_5678_u32;
            let mut seed_r = 0x8765_4321_u32;
            let mut l = vec![0.0_f32; SILENCE];
            l.extend(broadband(SIGNAL, &mut seed_l).iter().map(|s| s * 0.8));
            let mut r = vec![0.0_f32; SILENCE];
            r.extend(broadband(SIGNAL, &mut seed_r).iter().map(|s| s * 0.8));
            let (sc_l, sc_r) = (l.clone(), r.clone());

            assert_no_alloc::assert_no_alloc(|| {
                proc.process(&mut l, &mut r, Some([&sc_l, &sc_r]), 120.0);
            });

            let onset = l[SILENCE..]
                .iter()
                .zip(&r[SILENCE..])
                .position(|(a, b)| a.abs() > THRESHOLD || b.abs() > THRESHOLD)
                .unwrap_or_else(|| panic!("{}: no response to a loud signal", spec.id));
            assert!(
                onset <= TOLERANCE,
                "{}: output didn't respond until {onset} samples ({:.1} ms) after \
                 input started — that's real latency load_for doesn't report",
                spec.id,
                onset as f32 / 48.0,
            );
        }
    }
}
