// SPDX-License-Identifier: AGPL-3.0-or-later
//! A small polyphonic subtractive synth: one oscillator per voice (polyBLEP
//! saw and square, triangle, sine), an ADSR envelope, and a resonant
//! low-pass filter that the envelope opens. It responds to pitch bend
//! (±2 semitones), the mod wheel (vibrato), volume, expression and the
//! sustain pedal, and per key to MIDI 2.0 per-note pitch bend (±48
//! semitones), volume, expression and brightness.
//!
//! Voices are identified by a caller-chosen `tag` (the engine uses note IDs
//! for clip notes and channel/pitch for live input) and may carry a
//! `deadline`, a caller-defined time at which the note should end. The synth
//! doesn't interpret deadlines; it just reports the earliest one and releases
//! voices when told the time has come.

use std::f32::consts::{PI, TAU};

use dawkey_core::{SynthParams, Waveform};

/// Number of voices; a note beyond this steals one.
pub const VOICES: usize = 16;
/// Filter coefficients are recomputed every this many samples.
const CONTROL_RATE: u32 = 32;
/// Overall output level, so chords don't clip.
const OUTPUT_GAIN: f32 = 0.25;
/// A releasing voice quieter than this (-80 dB) is done.
const SILENT: f32 = 1e-4;
/// ln(1000): exponential stages fall by 60 dB over their time.
const LN_1000: f32 = 6.907_755;
/// Pitch bend range, in semitones each way.
pub const BEND_RANGE: f32 = 2.0;
/// Vibrato at full mod wheel: rate in Hz, depth in semitones.
const VIBRATO_HZ: f32 = 5.5;
const VIBRATO_DEPTH: f32 = 0.5;
/// Samples whose pitch ratios are computed at once while bending.
const CHUNK: usize = 64;
/// Per-note pitch bend range, in semitones each way.
pub const NOTE_BEND_RANGE: f32 = 48.0;
/// How far per-note brightness moves the filter cutoff, in octaves each
/// way.
const BRIGHTNESS_OCTAVES: f32 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Debug, Clone, Copy)]
struct Voice {
    stage: Stage,
    tag: u64,
    pitch: u8,
    deadline: u64,
    /// Note-off arrived while the sustain pedal was down.
    sustained: bool,
    /// Order of note-ons, for stealing the oldest voice.
    started: u64,
    /// Per-note controllers detached from the key (per-note management):
    /// the voice keeps these instead of following the key's.
    detached: Option<NoteControls>,
    phase: f32,
    inc: f32,
    amp: f32,
    /// Envelope level, 0 to 1.
    level: f32,
    /// Samples until the filter coefficients are next updated.
    countdown: u32,
    // Filter coefficients and state (a TPT state-variable filter).
    a1: f32,
    a2: f32,
    a3: f32,
    ic1: f32,
    ic2: f32,
}

impl Voice {
    const IDLE: Self = Self {
        stage: Stage::Idle,
        tag: 0,
        pitch: 0,
        deadline: u64::MAX,
        sustained: false,
        started: 0,
        detached: None,
        phase: 0.0,
        inc: 0.0,
        amp: 0.0,
        level: 0.0,
        countdown: 0,
        a1: 0.0,
        a2: 0.0,
        a3: 0.0,
        ic1: 0.0,
        ic2: 0.0,
    };

    /// Held: sounding and not yet released.
    fn held(&self) -> bool {
        matches!(self.stage, Stage::Attack | Stage::Decay | Stage::Sustain)
    }

    fn release(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
            self.sustained = false;
        }
    }
}

/// Per-sample constants derived from [`SynthParams`].
#[derive(Debug, Clone, Copy)]
struct Coeffs {
    waveform: Waveform,
    attack_step: f32,
    decay: f32,
    sustain: f32,
    release: f32,
    cutoff: f32,
    env_octaves: f32,
    /// Filter damping, from resonance.
    k: f32,
    nyquist_limit: f32,
}

impl Coeffs {
    fn new(p: &SynthParams, rate: f32) -> Self {
        let samples = |ms: f32| ms.max(0.0) / 1000.0 * rate;
        let falloff = |ms: f32| {
            let n = samples(ms);
            if n < 1.0 { 0.0 } else { (-LN_1000 / n).exp() }
        };
        let attack = samples(p.attack_ms);
        Self {
            waveform: p.waveform,
            attack_step: if attack < 1.0 { 1.0 } else { 1.0 / attack },
            decay: falloff(p.decay_ms),
            sustain: p.sustain.clamp(0.0, 1.0),
            release: falloff(p.release_ms),
            cutoff: p.cutoff_hz.clamp(20.0, 20_000.0),
            env_octaves: p.filter_env_octaves.clamp(0.0, 8.0),
            k: 2.0 - 1.94 * p.resonance.clamp(0.0, 1.0),
            nyquist_limit: rate * 0.45,
        }
    }
}

/// A polyphonic synth. Real-time safe after [`new`](Self::new).
#[derive(Debug, Clone)]
pub struct PolySynth {
    rate: f32,
    coeffs: Coeffs,
    voices: [Voice; VOICES],
    notes_started: u64,
    pedal: bool,
    controls: Controls,
    /// Per-note controllers, by pitch.
    notes: [NoteControls; 128],
}

/// One key's MIDI 2.0 per-note controllers.
#[derive(Debug, Clone, Copy, PartialEq)]
struct NoteControls {
    /// Frequency ratio from per-note pitch bend.
    bend: f32,
    /// Gain from per-note volume and expression (unity at full).
    volume: f32,
    expression: f32,
    /// Filter cutoff shift, in octaves.
    brightness: f32,
}

impl NoteControls {
    const DEFAULT: Self = Self {
        bend: 1.0,
        volume: 1.0,
        expression: 1.0,
        brightness: 0.0,
    };
}

/// A 32-bit MIDI 2.0 value as -1 to 1 around its center (0x8000_0000).
fn centered(value: u32) -> f32 {
    ((f64::from(value) - 2_147_483_648.0) / 2_147_483_648.0) as f32
}

/// A 32-bit MIDI 2.0 value as 0 to 1.
fn unit(value: u32) -> f32 {
    (f64::from(value) / f64::from(u32::MAX)) as f32
}

/// Controller state: what bend, mod wheel, volume and expression are set to.
#[derive(Debug, Clone, Copy)]
struct Controls {
    /// Frequency ratio from pitch bend.
    bend: f32,
    /// Mod wheel, 0 to 1.
    modulation: f32,
    /// Vibrato phase, 0 to 1.
    lfo: f32,
    /// Gain from volume (CC 7, unity at 100) and expression (CC 11).
    volume: f32,
    expression: f32,
}

impl Controls {
    const DEFAULT: Self = Self {
        bend: 1.0,
        modulation: 0.0,
        lfo: 0.0,
        volume: 1.0,
        expression: 1.0,
    };

    /// True if notes play at their own pitch.
    fn steady(&self) -> bool {
        self.bend == 1.0 && self.modulation == 0.0
    }
}

impl PolySynth {
    pub fn new(sample_rate: u32, params: &SynthParams) -> Self {
        let rate = sample_rate as f32;
        Self {
            rate,
            coeffs: Coeffs::new(params, rate),
            voices: [Voice::IDLE; VOICES],
            notes_started: 0,
            pedal: false,
            controls: Controls::DEFAULT,
            notes: [NoteControls::DEFAULT; 128],
        }
    }

    /// Takes over another synth's sounding voices and pedal, so a rebuilt
    /// synth (new settings) continues seamlessly.
    pub fn continue_from(&mut self, other: &PolySynth) {
        self.voices = other.voices;
        self.notes_started = other.notes_started;
        self.pedal = other.pedal;
        self.controls = other.controls;
        self.notes = other.notes;
    }

    /// MIDI 2.0 per-note pitch bend on one key, 32 bits (0x8000_0000 is
    /// none): ±48 semitones.
    pub fn set_note_bend(&mut self, pitch: u8, value: u32) {
        let semitones = centered(value) * NOTE_BEND_RANGE;
        self.notes[usize::from(pitch & 0x7f)].bend = (semitones / 12.0).exp2();
    }

    /// A MIDI 2.0 registered per-note controller on one key, 32 bits:
    /// volume (7), expression (11) and brightness (74, centered). Others
    /// are ignored.
    pub fn set_note_control(&mut self, pitch: u8, controller: u8, value: u32) {
        let note = &mut self.notes[usize::from(pitch & 0x7f)];
        match controller {
            7 => note.volume = unit(value).powi(2),
            11 => note.expression = unit(value).powi(2),
            74 => note.brightness = centered(value) * BRIGHTNESS_OCTAVES,
            _ => {}
        }
    }

    /// MIDI 2.0 per-note management on one key: with `detach`, its sounding
    /// voices keep the per-note controllers they have, and later ones only
    /// reach later notes; with `reset`, the key's per-note controllers go
    /// back to their defaults (after detaching).
    pub fn manage_note(&mut self, pitch: u8, detach: bool, reset: bool) {
        let key = usize::from(pitch & 0x7f);
        if detach {
            let current = self.notes[key];
            for v in &mut self.voices {
                if v.stage != Stage::Idle && usize::from(v.pitch) == key && v.detached.is_none() {
                    v.detached = Some(current);
                }
            }
        }
        if reset {
            self.notes[key] = NoteControls::DEFAULT;
        }
    }

    /// Pitch bend, 0 to 16383 (8192 is none): ±2 semitones.
    pub fn set_bend(&mut self, value: u16) {
        let v = f32::from(value.min(16_383)) - 8192.0;
        let semitones = v / if v > 0.0 { 8191.0 } else { 8192.0 } * BEND_RANGE;
        self.controls.bend = (semitones / 12.0).exp2();
    }

    /// A control change: mod wheel (1), volume (7), expression (11),
    /// sustain pedal (64), reset all controllers (121). Others are ignored.
    pub fn set_control(&mut self, controller: u8, value: u8) {
        let unit = f32::from(value.min(127)) / 127.0;
        match controller {
            1 => {
                if self.controls.modulation == 0.0 {
                    self.controls.lfo = 0.0;
                }
                self.controls.modulation = unit;
            }
            7 => self.controls.volume = (f32::from(value.min(127)) / 100.0).powi(2),
            11 => self.controls.expression = unit * unit,
            64 => self.set_pedal(value >= 64),
            121 => self.reset_controllers(),
            _ => {}
        }
    }

    /// Back to no bend, no modulation, full expression and the pedal up
    /// (volume stays, as MIDI's "reset all controllers" asks).
    pub fn reset_controllers(&mut self) {
        let volume = self.controls.volume;
        self.controls = Controls {
            volume,
            ..Controls::DEFAULT
        };
        self.notes = [NoteControls::DEFAULT; 128];
        self.set_pedal(false);
    }

    /// Starts a note. `amp` scales its level (1 is full velocity at unity
    /// gain, up to 4); `deadline` is when
    /// the caller wants it released (`u64::MAX` for never). A held voice with
    /// the same tag is released first.
    pub fn note_on(&mut self, tag: u64, pitch: u8, amp: f32, deadline: u64) {
        for v in &mut self.voices {
            if v.tag == tag && v.stage != Stage::Idle {
                v.release();
            }
        }
        let index = self.free_voice();
        let freq = 440.0 * ((f32::from(pitch) - 69.0) / 12.0).exp2();
        self.notes_started += 1;
        self.voices[index] = Voice {
            stage: Stage::Attack,
            tag,
            pitch: pitch & 0x7f,
            deadline,
            started: self.notes_started,
            inc: (freq / self.rate).min(0.49),
            amp: amp.clamp(0.0, 4.0),
            ..Voice::IDLE
        };
    }

    /// Releases the held voices with this tag, or marks them sustained while
    /// the pedal is down.
    pub fn note_off(&mut self, tag: u64) {
        for v in &mut self.voices {
            if v.tag == tag && v.held() {
                if self.pedal {
                    v.sustained = true;
                } else {
                    v.release();
                }
            }
        }
    }

    /// Sustain pedal. Lifting it releases the notes it was holding.
    pub fn set_pedal(&mut self, down: bool) {
        self.pedal = down;
        if !down {
            for v in &mut self.voices {
                if v.sustained {
                    v.release();
                }
            }
        }
    }

    /// Releases every voice whose tag matches, ignoring the pedal.
    pub fn release_where(&mut self, matches: impl Fn(u64) -> bool) {
        for v in &mut self.voices {
            if matches(v.tag) {
                v.release();
            }
        }
    }

    /// Silences every voice at once, without a release tail.
    pub fn reset(&mut self) {
        self.voices = [Voice::IDLE; VOICES];
        self.pedal = false;
    }

    /// The earliest deadline of a held voice, if any.
    pub fn next_deadline(&self) -> Option<u64> {
        self.voices
            .iter()
            .filter(|v| v.held() && v.deadline != u64::MAX)
            .map(|v| v.deadline)
            .min()
    }

    /// Releases held voices whose deadline is at or before `now`, or marks
    /// them sustained while the pedal is down.
    pub fn release_due(&mut self, now: u64) {
        for v in &mut self.voices {
            if v.held() && v.deadline <= now {
                if self.pedal {
                    v.sustained = true;
                    v.deadline = u64::MAX;
                } else {
                    v.release();
                }
            }
        }
    }

    /// Updates the deadline of every held voice: `f(tag, deadline)` returns
    /// the new deadline, or `None` to release the voice.
    pub fn retarget(&mut self, mut f: impl FnMut(u64, u64) -> Option<u64>) {
        for v in &mut self.voices {
            if v.held() {
                match f(v.tag, v.deadline) {
                    Some(deadline) => v.deadline = deadline,
                    None => v.release(),
                }
            }
        }
    }

    /// True if no voice is sounding.
    pub fn is_silent(&self) -> bool {
        self.voices.iter().all(|v| v.stage == Stage::Idle)
    }

    /// Number of voices sounding (held or releasing).
    pub fn active_voices(&self) -> usize {
        self.voices
            .iter()
            .filter(|v| v.stage != Stage::Idle)
            .count()
    }

    /// Adds the synth's output for `out.len()` samples to `out` (mono).
    /// The result doesn't depend on how a stretch of time is split into
    /// calls.
    pub fn render(&mut self, out: &mut [f32]) {
        let c = self.coeffs;
        let rate = self.rate;
        let gain = OUTPUT_GAIN * self.controls.volume * self.controls.expression;
        let notes = &self.notes;
        if self.controls.steady() {
            for v in &mut self.voices {
                let note = v.detached.unwrap_or(notes[usize::from(v.pitch)]);
                render_voice(v, &c, rate, gain, &note, out, None);
            }
            return;
        }
        // Bent or vibrato: each sample's frequency ratio, a chunk at a time.
        let lfo_inc = VIBRATO_HZ / rate;
        let depth = VIBRATO_DEPTH * self.controls.modulation / 12.0;
        for chunk in out.chunks_mut(CHUNK) {
            let mut ratios = [0.0; CHUNK];
            for ratio in &mut ratios[..chunk.len()] {
                let lfo = &mut self.controls.lfo;
                *ratio = if depth == 0.0 {
                    self.controls.bend
                } else {
                    self.controls.bend * (depth * (TAU * *lfo).sin()).exp2()
                };
                *lfo += lfo_inc;
                if *lfo >= 1.0 {
                    *lfo -= 1.0;
                }
            }
            for v in &mut self.voices {
                let note = v.detached.unwrap_or(notes[usize::from(v.pitch)]);
                let ratios = Some(&ratios[..chunk.len()]);
                render_voice(v, &c, rate, gain, &note, chunk, ratios);
            }
        }
    }

    /// Applies new settings to sounding and future notes.
    pub fn set_params(&mut self, params: &SynthParams) {
        self.coeffs = Coeffs::new(params, self.rate);
    }

    /// Picks a voice for a new note: an idle one, else the quietest
    /// releasing one, else the oldest.
    fn free_voice(&self) -> usize {
        let indexed = || self.voices.iter().enumerate();
        if let Some((i, _)) = indexed().find(|(_, v)| v.stage == Stage::Idle) {
            return i;
        }
        if let Some((i, _)) = indexed()
            .filter(|(_, v)| v.stage == Stage::Release)
            .min_by(|(_, a), (_, b)| a.level.total_cmp(&b.level))
        {
            return i;
        }
        indexed()
            .min_by_key(|(_, v)| v.started)
            .map_or(0, |(i, _)| i)
    }
}

/// Adds one voice to `out`. `ratios` scale its frequency per sample
/// (bend, vibrato), and `note` holds its key's per-note controllers. (At
/// their defaults they multiply by 1 and add 0, so the output is the same
/// bit for bit as without them.)
fn render_voice(
    v: &mut Voice,
    c: &Coeffs,
    rate: f32,
    gain: f32,
    note: &NoteControls,
    out: &mut [f32],
    ratios: Option<&[f32]>,
) {
    if v.stage == Stage::Idle {
        return;
    }
    let gain = gain * note.volume * note.expression;
    for (i, sample) in out.iter_mut().enumerate() {
        if v.countdown == 0 {
            update_filter(v, c, rate, note.brightness);
            v.countdown = CONTROL_RATE;
        }
        v.countdown -= 1;
        let inc = match ratios {
            Some(r) => (v.inc * r[i] * note.bend).min(0.49),
            None if note.bend == 1.0 => v.inc,
            None => (v.inc * note.bend).min(0.49),
        };
        let x = oscillator(c.waveform, v.phase, inc);
        v.phase += inc;
        if v.phase >= 1.0 {
            v.phase -= 1.0;
        }
        // TPT state-variable low-pass.
        let v3 = x - v.ic2;
        let v1 = v.a1 * v.ic1 + v.a2 * v3;
        let v2 = v.ic2 + v.a2 * v.ic1 + v.a3 * v3;
        v.ic1 = 2.0 * v1 - v.ic1;
        v.ic2 = 2.0 * v2 - v.ic2;
        *sample += v2 * v.level * v.amp * gain;
        if !advance_envelope(v, c) {
            *v = Voice::IDLE;
            break;
        }
    }
}

fn update_filter(v: &mut Voice, c: &Coeffs, rate: f32, brightness: f32) {
    let octaves = c.env_octaves * v.level + brightness;
    let cutoff = (c.cutoff * octaves.exp2()).min(c.nyquist_limit);
    let g = (PI * cutoff / rate).tan();
    v.a1 = 1.0 / (1.0 + g * (g + c.k));
    v.a2 = g * v.a1;
    v.a3 = g * v.a2;
}

/// Steps the envelope by one sample. Returns false once the voice is done.
fn advance_envelope(v: &mut Voice, c: &Coeffs) -> bool {
    match v.stage {
        Stage::Idle => return false,
        Stage::Attack => {
            v.level += c.attack_step;
            if v.level >= 1.0 {
                v.level = 1.0;
                v.stage = Stage::Decay;
            }
        }
        Stage::Decay => {
            v.level = c.sustain + (v.level - c.sustain) * c.decay;
            if v.level - c.sustain < SILENT {
                v.level = c.sustain;
                v.stage = Stage::Sustain;
            }
        }
        Stage::Sustain => {
            v.level = c.sustain;
            if c.sustain < SILENT {
                return false;
            }
        }
        Stage::Release => {
            v.level *= c.release;
            if v.level < SILENT {
                return false;
            }
        }
    }
    true
}

fn oscillator(waveform: Waveform, phase: f32, inc: f32) -> f32 {
    match waveform {
        Waveform::Saw => 2.0 * phase - 1.0 - poly_blep(phase, inc),
        Waveform::Square => {
            let naive = if phase < 0.5 { 1.0 } else { -1.0 };
            let half = if phase < 0.5 {
                phase + 0.5
            } else {
                phase - 0.5
            };
            naive + poly_blep(phase, inc) - poly_blep(half, inc)
        }
        Waveform::Triangle => 4.0 * (phase - 0.5).abs() - 1.0,
        Waveform::Sine => (TAU * phase).sin(),
    }
}

/// Band-limits a unit step at phase 0 (polyBLEP).
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn params() -> SynthParams {
        SynthParams {
            attack_ms: 1.0,
            decay_ms: 10.0,
            sustain: 0.5,
            release_ms: 10.0,
            ..SynthParams::default()
        }
    }

    fn render(synth: &mut PolySynth, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames];
        assert_no_alloc::assert_no_alloc(|| synth.render(&mut out));
        out
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    #[test]
    fn note_sounds_then_releases_to_silence() {
        let mut synth = PolySynth::new(RATE, &params());
        assert!(synth.is_silent());
        synth.note_on(1, 69, 1.0, u64::MAX);
        let held = render(&mut synth, 4800);
        assert!(peak(&held) > 0.05, "{}", peak(&held));
        synth.note_off(1);
        // A 10 ms release is well under 100 ms.
        let tail = render(&mut synth, 4800);
        assert!(synth.is_silent());
        assert!(peak(&tail[4000..]) == 0.0);
    }

    #[test]
    fn pitch_sets_the_frequency() {
        // A sine at A4 crosses zero upwards 440 times a second.
        let p = SynthParams {
            waveform: Waveform::Sine,
            cutoff_hz: 20_000.0,
            ..params()
        };
        let mut synth = PolySynth::new(RATE, &p);
        synth.note_on(1, 69, 1.0, u64::MAX);
        let out = render(&mut synth, RATE as usize);
        let rising = out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((439..=441).contains(&rising), "{rising}");
    }

    #[test]
    fn output_is_independent_of_buffer_splits() {
        let mut a = PolySynth::new(RATE, &params());
        let mut b = a.clone();
        a.note_on(1, 60, 0.8, u64::MAX);
        b.note_on(1, 60, 0.8, u64::MAX);
        let whole = render(&mut a, 1000);
        let mut parts = Vec::new();
        for len in [1, 31, 33, 64, 200, 671] {
            parts.extend(render(&mut b, len));
        }
        assert_eq!(whole, parts);
    }

    #[test]
    fn steals_the_oldest_voice() {
        let mut synth = PolySynth::new(RATE, &params());
        for tag in 0..VOICES as u64 + 1 {
            synth.note_on(tag, 60 + tag as u8, 1.0, u64::MAX);
        }
        assert_eq!(synth.active_voices(), VOICES);
        // The first note was stolen: releasing it changes nothing.
        synth.note_off(0);
        assert!(synth.voices.iter().all(|v| v.held()));
    }

    #[test]
    fn pedal_holds_released_notes() {
        let mut synth = PolySynth::new(RATE, &params());
        synth.note_on(7, 60, 1.0, u64::MAX);
        synth.set_pedal(true);
        synth.note_off(7);
        render(&mut synth, 4800);
        assert_eq!(synth.active_voices(), 1);
        synth.set_pedal(false);
        render(&mut synth, 4800);
        assert!(synth.is_silent());
    }

    #[test]
    fn deadlines() {
        let mut synth = PolySynth::new(RATE, &params());
        synth.note_on(1, 60, 1.0, 100);
        synth.note_on(2, 64, 1.0, 50);
        synth.note_on(3, 67, 1.0, u64::MAX);
        assert_eq!(synth.next_deadline(), Some(50));
        synth.release_due(50);
        assert_eq!(synth.next_deadline(), Some(100));
        // Tag 1 moves; anything else is released.
        synth.retarget(|tag, _| (tag == 1).then_some(200));
        assert_eq!(synth.next_deadline(), Some(200));
        let held: Vec<_> = synth
            .voices
            .iter()
            .filter(|v| v.held())
            .map(|v| v.tag)
            .collect();
        assert_eq!(held, [1]);
    }

    #[test]
    fn pitch_bend_and_vibrato() {
        let p = SynthParams {
            waveform: Waveform::Sine,
            cutoff_hz: 20_000.0,
            ..params()
        };
        let crossings = |out: &[f32]| out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        let mut synth = PolySynth::new(RATE, &p);
        // Two semitones up from A4: 493.9 Hz.
        synth.set_bend(16_383);
        synth.note_on(1, 69, 1.0, u64::MAX);
        let up = crossings(&render(&mut synth, RATE as usize));
        assert!((492..=495).contains(&up), "{up}");
        synth.set_bend(8192);
        let level = peak(&render(&mut synth, 4800));
        // Vibrato wobbles around the pitch: about 440 crossings a second.
        synth.set_control(1, 127);
        let wobbly = render(&mut synth, RATE as usize);
        assert!((436..=444).contains(&crossings(&wobbly)));
        // Expression scales the level.
        synth.set_control(1, 0);
        synth.set_control(11, 64);
        let quiet = peak(&render(&mut synth, 4800));
        assert!((quiet / level - 0.254).abs() < 0.01, "{quiet} vs {level}");
        synth.reset_controllers();
        assert!((peak(&render(&mut synth, 4800)) - level).abs() < 1e-3);
    }

    #[test]
    fn per_note_controllers_act_on_one_key() {
        let p = SynthParams {
            waveform: Waveform::Sine,
            cutoff_hz: 20_000.0,
            ..params()
        };
        let crossings = |out: &[f32]| out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        // Per-note bend of a quarter of its range up (12 semitones): A5.
        let mut synth = PolySynth::new(RATE, &p);
        synth.set_note_bend(69, 0xa000_0000);
        synth.set_note_bend(60, 0x2000_0000);
        synth.note_on(1, 69, 1.0, u64::MAX);
        let up = crossings(&render(&mut synth, RATE as usize));
        assert!((879..=881).contains(&up), "{up}");
        // Per-note volume 0 silences that key only.
        let mut synth = PolySynth::new(RATE, &p);
        synth.set_note_control(64, 7, 0);
        synth.note_on(1, 64, 1.0, u64::MAX);
        assert_eq!(peak(&render(&mut synth, 4800)), 0.0);
        synth.note_on(2, 65, 1.0, u64::MAX);
        assert!(peak(&render(&mut synth, 4800)) > 0.05);
        // Reset all controllers brings the key back.
        synth.reset_controllers();
        synth.note_on(3, 64, 1.0, u64::MAX);
        synth.note_off(2);
        render(&mut synth, 4800);
        assert!(peak(&render(&mut synth, 4800)) > 0.05);
    }

    #[test]
    fn per_note_management_detaches_and_resets() {
        let p = SynthParams {
            waveform: Waveform::Sine,
            cutoff_hz: 20_000.0,
            ..params()
        };
        let level = |synth: &mut PolySynth| {
            render(synth, 4800);
            peak(&render(synth, 4800))
        };
        let mut synth = PolySynth::new(RATE, &p);
        synth.note_on(1, 64, 1.0, u64::MAX);
        let full = level(&mut synth);
        // Half volume (a quarter of the gain), then detach: the sounding
        // note keeps it, though the key's volume goes to 0.
        synth.set_note_control(64, 7, 0x8000_0000);
        let half = level(&mut synth);
        assert!((half / full - 0.25).abs() < 0.01, "{half} vs {full}");
        synth.manage_note(64, true, false);
        synth.set_note_control(64, 7, 0);
        assert!((level(&mut synth) - half).abs() < 1e-3);
        // A new note on the key gets the key's volume: silent.
        synth.note_off(1);
        let mut silent = PolySynth::new(RATE, &p);
        silent.continue_from(&synth);
        render(&mut silent, RATE as usize);
        silent.note_on(2, 64, 1.0, u64::MAX);
        assert_eq!(level(&mut silent), 0.0);
        // Reset: the key is back at full volume.
        silent.manage_note(64, false, true);
        assert!((level(&mut silent) - full).abs() < 1e-3);
    }

    #[test]
    fn default_per_note_values_change_nothing() {
        let mut a = PolySynth::new(RATE, &params());
        let mut b = a.clone();
        b.set_note_bend(60, 0x8000_0000);
        b.set_note_control(60, 7, u32::MAX);
        b.set_note_control(60, 11, u32::MAX);
        b.set_note_control(60, 74, 0x8000_0000);
        for synth in [&mut a, &mut b] {
            synth.set_bend(10_000);
            synth.note_on(1, 60, 0.8, u64::MAX);
        }
        assert_eq!(render(&mut a, 2000), render(&mut b, 2000));
        // Brightness opens the filter: a brighter saw is louder.
        let mut c = PolySynth::new(RATE, &params());
        c.set_note_control(60, 74, u32::MAX);
        c.note_on(1, 60, 0.8, u64::MAX);
        let mut d = PolySynth::new(RATE, &params());
        d.note_on(1, 60, 0.8, u64::MAX);
        let energy = |s: &[f32]| s.iter().map(|x| x * x).sum::<f32>();
        assert!(energy(&render(&mut c, 4800)) > energy(&render(&mut d, 4800)));
    }

    #[test]
    fn bent_output_is_independent_of_buffer_splits() {
        let mut a = PolySynth::new(RATE, &params());
        a.set_bend(12_000);
        a.set_control(1, 100);
        let mut b = a.clone();
        a.note_on(1, 60, 0.8, u64::MAX);
        b.note_on(1, 60, 0.8, u64::MAX);
        let whole = render(&mut a, 1000);
        let mut parts = Vec::new();
        for len in [1, 31, 33, 64, 200, 671] {
            parts.extend(render(&mut b, len));
        }
        assert_eq!(whole, parts);
    }

    #[test]
    fn pedal_holds_notes_past_their_deadline() {
        let mut synth = PolySynth::new(RATE, &params());
        synth.set_control(64, 127);
        synth.note_on(1, 60, 1.0, 10);
        synth.release_due(10);
        assert_eq!(synth.next_deadline(), None);
        render(&mut synth, 4800);
        assert_eq!(synth.active_voices(), 1);
        synth.set_control(64, 0);
        render(&mut synth, 4800);
        assert!(synth.is_silent());
    }

    #[test]
    fn rebuilt_synth_continues_voices() {
        let mut old = PolySynth::new(RATE, &params());
        old.note_on(1, 60, 1.0, u64::MAX);
        render(&mut old, 100);
        let mut new = PolySynth::new(
            RATE,
            &SynthParams {
                waveform: Waveform::Square,
                ..params()
            },
        );
        new.continue_from(&old);
        assert_eq!(new.active_voices(), 1);
        assert!(peak(&render(&mut new, 100)) > 0.0);
    }
}
