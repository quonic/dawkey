// SPDX-License-Identifier: AGPL-3.0-or-later
//! Built-in synthesized drum kit: eight percussion voices, each its own
//! small subtractive/noise synthesis algorithm, triggered by MIDI note
//! number like a sampler would be but with nothing to load or allocate.
//!
//! Voices are one-shots: a `NoteOn` (re)triggers a pad's envelope; `NoteOff`
//! is ignored, same as a drum machine's own pads. Allocation-free after
//! construction, like [`crate::rack`] and [`crate::synth`].

use std::f32::consts::TAU;
use std::sync::Arc;

/// The General MIDI drum-map note each pad answers to.
pub const PAD_NOTES: [u8; 8] = [36, 38, 42, 46, 45, 39, 49, 51];
pub const PAD_NAMES: [&str; 8] = [
    "Kick",
    "Snare",
    "Closed Hat",
    "Open Hat",
    "Tom",
    "Clap",
    "Crash",
    "Ride",
];
/// Per-pad parameter defaults: `(tone, decay_ms, level_db)`.
const PAD_DEFAULTS: [(f32, f32, f32); 8] = [
    (0.5, 320.0, 0.0),   // Kick
    (0.5, 180.0, 0.0),   // Snare
    (0.5, 70.0, -3.0),   // Closed Hat
    (0.5, 320.0, -5.0),  // Open Hat
    (0.5, 260.0, -2.0),  // Tom
    (0.5, 130.0, -2.0),  // Clap
    (0.5, 1400.0, -6.0), // Crash
    (0.5, 900.0, -7.0),  // Ride
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pad {
    Kick,
    Snare,
    ClosedHat,
    OpenHat,
    Tom,
    Clap,
    Crash,
    Ride,
}

impl Pad {
    pub const ALL: [Pad; 8] = [
        Pad::Kick,
        Pad::Snare,
        Pad::ClosedHat,
        Pad::OpenHat,
        Pad::Tom,
        Pad::Clap,
        Pad::Crash,
        Pad::Ride,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn note(self) -> u8 {
        PAD_NOTES[self.index()]
    }

    pub fn name(self) -> &'static str {
        PAD_NAMES[self.index()]
    }

    pub fn from_note(note: u8) -> Option<Pad> {
        PAD_NOTES
            .iter()
            .position(|&n| n == note)
            .map(|i| Self::ALL[i])
    }
}

/// An event for [`DrumEngine::process`], `offset` frames into the block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrumEvent {
    pub offset: u32,
    pub kind: DrumEventKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DrumEventKind {
    NoteOn {
        key: u8,
        velocity: f32,
    },
    NoteOff {
        key: u8,
    },
    /// Parameter `index` (`pad * 3 + field`, field 0 tone, 1 decay_ms, 2
    /// level_db) is now `value`.
    Param {
        index: u32,
        value: f64,
    },
}

#[inline]
fn db_to_lin(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

#[inline]
fn decay_coeff(ms: f32, sample_rate: f32) -> f32 {
    let samples = (ms.max(1.0) * 0.001 * sample_rate).max(1.0);
    (-1.0_f32 / samples).exp()
}

#[inline]
fn hp_coeff(hz: f32, sample_rate: f32) -> f32 {
    let w = (TAU * hz.clamp(10.0, sample_rate * 0.49) / sample_rate).min(std::f32::consts::PI);
    (-w).exp()
}

/// A tiny deterministic PRNG (xorshift32): drum noise never comes from a
/// real entropy source, so the engine's output stays reproducible run to
/// run, which the golden audio hash tests require.
#[derive(Debug, Clone, Copy)]
struct Noise(u32);

impl Noise {
    fn new(seed: u32) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Voice {
    active: bool,
    env: f32,
    env_coeff: f32,
    click: f32,
    phase: f32,
    phase2: f32,
    age: f32,
    vel: f32,
    hp1_in: f32,
    hp1_out: f32,
    hp2_out: f32,
    /// Fractional read position into a sample-kit pad's buffer, in frames.
    /// Unused (and harmless) for pads that are still synthesized.
    pos: f64,
}

/// One pad's recorded one-shot, mono, already resampled to the engine's
/// sample rate; `None` for a pad that keeps synthesizing. Loading this is a
/// control-thread-only operation (decoding and resampling allocate) — see
/// [`DrumEngine::load_samples`].
pub type SampleKitData = [Option<Vec<f32>>; 8];

/// Eight percussion voices, parameterized per pad: synthesized unless a
/// sample kit is loaded, in which case pads it covers play that recording
/// back instead (pads it doesn't cover keep synthesizing).
#[derive(Debug, Clone)]
pub struct DrumEngine {
    sample_rate: f32,
    params: [(f32, f32, f32); 8],
    voices: [Voice; 8],
    noise: [Noise; 8],
    samples: Option<Arc<SampleKitData>>,
}

impl DrumEngine {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.max(8_000) as f32;
        let mut noise = [Noise::new(0); 8];
        for (i, n) in noise.iter_mut().enumerate() {
            // Distinct, fixed seeds so each pad's noise is independent but
            // always the same from run to run.
            *n = Noise::new(0x9E37_79B9_u32.wrapping_mul(i as u32 + 1));
        }
        Self {
            sample_rate: sr,
            params: PAD_DEFAULTS,
            voices: [Voice::default(); 8],
            noise,
            samples: None,
        }
    }

    /// Loads a sample kit (`None` reverts fully to synthesis). The kit
    /// itself must already be decoded and resampled to this engine's rate —
    /// building it allocates, so do that on the control thread, not here.
    pub fn load_samples(&mut self, kit: Option<Arc<SampleKitData>>) {
        self.samples = kit;
    }

    /// Sets a flattened param (`pad * 3 + field`); out-of-range indices and
    /// non-finite values are ignored.
    pub fn set_param(&mut self, index: usize, value: f64) {
        if !value.is_finite() {
            return;
        }
        let pad = index / 3;
        let field = index % 3;
        let Some(p) = self.params.get_mut(pad) else {
            return;
        };
        match field {
            0 => p.0 = (value as f32).clamp(0.0, 1.0),
            1 => p.1 = (value as f32).clamp(5.0, 4000.0),
            2 => p.2 = (value as f32).clamp(-36.0, 6.0),
            _ => {}
        }
    }

    fn note_on(&mut self, key: u8, velocity: f32) {
        let Some(pad) = Pad::from_note(key) else {
            return;
        };
        let i = pad.index();
        let (_, decay_ms, _) = self.params[i];
        let v = &mut self.voices[i];
        v.active = true;
        v.env = 1.0;
        v.env_coeff = decay_coeff(decay_ms, self.sample_rate);
        v.click = 1.0;
        v.phase = 0.0;
        v.phase2 = 0.0;
        v.age = 0.0;
        v.vel = velocity.clamp(0.0, 1.0);
        v.pos = 0.0;
    }

    /// Renders one block, applying `events` (sorted by offset) as it goes.
    /// `left`/`right` carry the mix on the way out; the caller is expected
    /// to have cleared or not care about their input (the kit is an
    /// instrument: it replaces, not adds to, its input).
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], events: &[DrumEvent]) {
        let frames = left.len().min(right.len());
        let click_coeff = decay_coeff(4.0, self.sample_rate);
        let mut ei = 0usize;
        for n in 0..frames {
            while let Some(e) = events.get(ei).filter(|e| e.offset as usize <= n) {
                match e.kind {
                    DrumEventKind::NoteOn { key, velocity } => self.note_on(key, velocity),
                    DrumEventKind::NoteOff { .. } => {}
                    DrumEventKind::Param { index, value } => self.set_param(index as usize, value),
                }
                ei += 1;
            }
            let mut out = 0.0f32;
            for pad in Pad::ALL {
                let i = pad.index();
                if !self.voices[i].active {
                    continue;
                }
                let (tone, _, level_db) = self.params[i];
                // Disjoint field borrows: voices/noise/samples are
                // independent fields, so this doesn't need `&mut self`.
                let v = &mut self.voices[i];
                let n = &mut self.noise[i];
                let sample = self.samples.as_deref().and_then(|k| k[i].as_deref());
                out += render_pad(
                    pad,
                    v,
                    n,
                    sample,
                    self.sample_rate,
                    tone,
                    level_db,
                    click_coeff,
                );
            }
            left[n] = out;
            right[n] = out;
        }
    }
}

/// Plays back one frame of a sample-kit pad's recording, linearly
/// interpolated, and advances its read position; deactivates the voice at
/// the buffer's end.
fn render_sample_pad(v: &mut Voice, buf: &[f32], level_db: f32) -> f32 {
    let level = db_to_lin(level_db) * v.vel;
    let idx = v.pos as usize;
    let sample = match (buf.get(idx), buf.get(idx + 1)) {
        (Some(&a), Some(&b)) => {
            let frac = (v.pos - idx as f64) as f32;
            a + (b - a) * frac
        }
        (Some(&a), None) => a,
        _ => {
            v.active = false;
            0.0
        }
    };
    v.pos += 1.0;
    if v.pos as usize >= buf.len() {
        v.active = false;
    }
    sample * level
}

#[allow(clippy::too_many_arguments)]
fn render_pad(
    pad: Pad,
    v: &mut Voice,
    n: &mut Noise,
    sample: Option<&[f32]>,
    sr: f32,
    tone: f32,
    level_db: f32,
    click_coeff: f32,
) -> f32 {
    if let Some(buf) = sample {
        return render_sample_pad(v, buf, level_db);
    }
    let level = db_to_lin(level_db) * v.vel;
    let sample = match pad {
        Pad::Kick => {
            let freq = 42.0 + 170.0 * v.env.powf(0.4);
            v.phase += TAU * freq / sr;
            let tone_sig = v.phase.sin() * v.env;
            let click_sig = n.next() * v.click * 0.3 * (0.3 + tone);
            tone_sig + click_sig
        }
        Pad::Snare => {
            let freq = 170.0 + 60.0 * tone;
            v.phase += TAU * freq / sr;
            let tone_sig = v.phase.sin() * v.env;
            let cutoff = 1200.0 + 2500.0 * tone;
            let a = hp_coeff(cutoff, sr);
            let x = n.next();
            let y = a * (v.hp1_out + x - v.hp1_in);
            v.hp1_in = x;
            v.hp1_out = y;
            tone_sig * 0.35 + y * v.env * 0.85
        }
        Pad::ClosedHat | Pad::OpenHat => {
            let cutoff = 5500.0 + 6000.0 * tone;
            let a = hp_coeff(cutoff, sr);
            let x = n.next();
            let y = a * (v.hp1_out + x - v.hp1_in);
            v.hp1_in = x;
            v.hp1_out = y;
            y * v.env
        }
        Pad::Tom => {
            let base = 90.0 + 160.0 * tone;
            let freq = base * (0.8 + 0.2 * v.env);
            v.phase += TAU * freq / sr;
            v.phase.sin() * v.env
        }
        Pad::Clap => {
            let cutoff = 900.0 + 2200.0 * tone;
            let a = hp_coeff(cutoff, sr);
            let x = n.next();
            let y = a * (v.hp1_out + x - v.hp1_in);
            v.hp1_in = x;
            v.hp1_out = y;
            // Low-pass the highpassed noise to band it; a cheap
            // "flutter" (several quick bursts) stands in for a real
            // clap's multiple hand-claps.
            let lp_a = hp_coeff(3500.0, sr);
            v.hp2_out = v.hp2_out + (1.0 - lp_a) * (y - v.hp2_out);
            let flutter = (0.55 + 0.45 * (TAU * 22.0 * v.age).sin().abs()).min(1.0);
            v.hp2_out * v.env * flutter
        }
        Pad::Crash | Pad::Ride => {
            let cutoff = 3200.0 + 4500.0 * tone;
            let a = hp_coeff(cutoff, sr);
            let x = n.next();
            let y = a * (v.hp1_out + x - v.hp1_in);
            v.hp1_in = x;
            v.hp1_out = y;
            let base = 320.0 + 500.0 * tone;
            v.phase += TAU * base / sr;
            v.phase2 += TAU * base * 1.6 / sr;
            let metallic = v.phase.sin() * 0.3 + v.phase2.sin() * 0.2;
            let noise_mix = if matches!(pad, Pad::Ride) { 0.5 } else { 0.85 };
            (y * noise_mix + metallic * (1.0 - noise_mix)) * v.env
        }
    };
    v.env *= v.env_coeff;
    v.click *= click_coeff;
    v.age += 1.0 / sr;
    if v.env < 1e-4 {
        v.active = false;
    }
    sample * level
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_maps_round_trip() {
        for pad in Pad::ALL {
            assert_eq!(Pad::from_note(pad.note()), Some(pad));
        }
    }

    #[test]
    fn a_struck_pad_produces_decaying_nonzero_audio_and_settles() {
        let mut engine = DrumEngine::new(48_000);
        let events = [DrumEvent {
            offset: 0,
            kind: DrumEventKind::NoteOn {
                key: Pad::Kick.note(),
                velocity: 1.0,
            },
        }];
        let mut left = vec![0.0; 48_000];
        let mut right = vec![0.0; 48_000];
        engine.process(&mut left, &mut right, &events);
        assert!(left.iter().any(|&s| s.abs() > 0.01));
        // A one-shot's amplitude decays well below its initial peak.
        let peak = left[..200].iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        let tail = left[40_000..].iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(tail < peak * 0.1, "peak={peak} tail={tail}");
        assert_eq!(left, right);
    }

    #[test]
    fn different_pads_sound_different() {
        let mut kick = DrumEngine::new(48_000);
        let mut hat = DrumEngine::new(48_000);
        let kick_on = [DrumEvent {
            offset: 0,
            kind: DrumEventKind::NoteOn {
                key: Pad::Kick.note(),
                velocity: 1.0,
            },
        }];
        let hat_on = [DrumEvent {
            offset: 0,
            kind: DrumEventKind::NoteOn {
                key: Pad::ClosedHat.note(),
                velocity: 1.0,
            },
        }];
        let mut kl = vec![0.0; 2048];
        let mut kr = vec![0.0; 2048];
        let mut hl = vec![0.0; 2048];
        let mut hr = vec![0.0; 2048];
        kick.process(&mut kl, &mut kr, &kick_on);
        hat.process(&mut hl, &mut hr, &hat_on);
        assert_ne!(kl, hl);
    }

    #[test]
    fn params_are_deterministic_across_fresh_instances() {
        let mut a = DrumEngine::new(44_100);
        let mut b = DrumEngine::new(44_100);
        let events = [DrumEvent {
            offset: 10,
            kind: DrumEventKind::NoteOn {
                key: Pad::Snare.note(),
                velocity: 0.8,
            },
        }];
        let mut al = vec![0.0; 4096];
        let mut ar = vec![0.0; 4096];
        let mut bl = vec![0.0; 4096];
        let mut br = vec![0.0; 4096];
        a.process(&mut al, &mut ar, &events);
        b.process(&mut bl, &mut br, &events);
        assert_eq!(al, bl);
    }

    fn sample_kit_with(pad: Pad, buf: Vec<f32>) -> Arc<SampleKitData> {
        let mut kit: SampleKitData = Default::default();
        kit[pad.index()] = Some(buf);
        Arc::new(kit)
    }

    #[test]
    fn a_sample_backed_pad_plays_back_the_buffer_then_deactivates() {
        let mut engine = DrumEngine::new(48_000);
        let buf = vec![1.0, 0.5, -0.5, -1.0];
        engine.load_samples(Some(sample_kit_with(Pad::Kick, buf.clone())));
        let events = [DrumEvent {
            offset: 0,
            kind: DrumEventKind::NoteOn {
                key: Pad::Kick.note(),
                velocity: 1.0,
            },
        }];
        let mut left = vec![0.0; 8];
        let mut right = vec![0.0; 8];
        engine.process(&mut left, &mut right, &events);
        assert_eq!(&left[..buf.len()], buf.as_slice());
        // Past the buffer's end, the voice is silent (not still synthesizing).
        assert_eq!(&left[buf.len()..], &[0.0; 4]);
        assert_eq!(left, right);
    }

    #[test]
    fn a_pad_without_a_sample_still_synthesizes_with_a_kit_loaded() {
        let mut with_kit = DrumEngine::new(48_000);
        with_kit.load_samples(Some(sample_kit_with(Pad::Kick, vec![0.0; 4])));
        let mut without_kit = DrumEngine::new(48_000);
        let events = [DrumEvent {
            offset: 0,
            kind: DrumEventKind::NoteOn {
                key: Pad::Snare.note(),
                velocity: 1.0,
            },
        }];
        let mut al = vec![0.0; 2048];
        let mut ar = vec![0.0; 2048];
        let mut bl = vec![0.0; 2048];
        let mut br = vec![0.0; 2048];
        with_kit.process(&mut al, &mut ar, &events);
        without_kit.process(&mut bl, &mut br, &events);
        assert_eq!(
            al, bl,
            "snare has no sample, so it should synthesize unchanged"
        );
    }
}
