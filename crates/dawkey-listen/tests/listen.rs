// SPDX-License-Identifier: AGPL-3.0-or-later
//! Describing and picturing a small arrangement made here: a kick on every
//! beat, a bass line and a chord pad, at 120 BPM in 4/4.
//!
//! `DAWKEY_LISTEN_PICTURES=<folder>` writes the pictures there to look at.

use std::f64::consts::TAU;

use dawkey_listen::{
    Clip, DescribeOptions, Panel, PictureKind, PictureOptions, Timing, describe, picture,
};

const RATE: u32 = 48_000;
const BPM: f64 = 120.0;

/// 120 BPM in 4/4 from the project's start.
struct Steady;

impl Timing for Steady {
    fn beat_at(&self, seconds: f64) -> f64 {
        seconds * BPM / 60.0
    }
    fn seconds_at(&self, beat: f64) -> f64 {
        beat * 60.0 / BPM
    }
    fn bar_of_beat(&self, beat: f64) -> f64 {
        beat / 4.0 + 1.0
    }
    fn bar_start(&self, bar: u32) -> f64 {
        f64::from(bar.saturating_sub(1)) * 4.0
    }
}

fn hz(midi: f64) -> f64 {
    440.0 * 2f64.powf((midi - 69.0) / 12.0)
}

fn seconds(beats: f64) -> f64 {
    beats * 60.0 / BPM
}

/// A sine-ish kick (a falling pitch with a fast decay) on every beat,
/// `late_ms` after it.
fn kick(beats: usize, late_ms: f64) -> Vec<f32> {
    let mut x = vec![0.0f32; (seconds(beats as f64) * f64::from(RATE)) as usize];
    for b in 0..beats {
        let at = ((seconds(b as f64) + late_ms / 1000.0) * f64::from(RATE)) as usize;
        let mut phase = 0.0;
        for i in 0..(RATE as usize / 4) {
            let t = i as f64 / f64::from(RATE);
            let f = 50.0 + 120.0 * (-t * 30.0).exp();
            phase += TAU * f / f64::from(RATE);
            if let Some(s) = x.get_mut(at + i) {
                *s += (0.8 * phase.sin() * (-t * 12.0).exp()) as f32;
            }
        }
    }
    x
}

/// A band-limited sawtooth line: (MIDI note, start beat, length in beats).
fn line(notes: &[(f64, f64, f64)], total_beats: f64, amp: f64) -> Vec<f32> {
    let mut x = vec![0.0f32; (seconds(total_beats) * f64::from(RATE)) as usize];
    for &(midi, start, length) in notes {
        let f = hz(midi);
        let a = (seconds(start) * f64::from(RATE)) as usize;
        let n = (seconds(length) * f64::from(RATE)) as usize;
        let harmonics = ((8000.0 / f) as usize).clamp(1, 40);
        for i in 0..n {
            let t = i as f64 / f64::from(RATE);
            // 5 ms attack and release.
            let env = (t / 0.005)
                .min(1.0)
                .min((n - i) as f64 / (0.005 * f64::from(RATE)));
            let mut s = 0.0;
            for h in 1..=harmonics {
                s += (TAU * f * h as f64 * t).sin() / h as f64;
            }
            if let Some(o) = x.get_mut(a + i) {
                *o += (amp * env * s) as f32;
            }
        }
    }
    x
}

/// Am | F | C | G, one bar each, as a bass line and a pad.
fn song() -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let bars = 4.0;
    let bass = line(
        &[
            (45.0, 0.0, 4.0),
            (41.0, 4.0, 4.0),
            (48.0, 8.0, 4.0),
            (43.0, 12.0, 4.0),
        ],
        bars * 4.0,
        0.25,
    );
    let chords: [[f64; 3]; 4] = [
        [57.0, 60.0, 64.0],
        [53.0, 57.0, 60.0],
        [60.0, 64.0, 67.0],
        [55.0, 59.0, 62.0],
    ];
    let notes: Vec<(f64, f64, f64)> = chords
        .iter()
        .enumerate()
        .flat_map(|(bar, c)| c.iter().map(move |&m| (m, bar as f64 * 4.0, 4.0)))
        .collect();
    let pad = line(&notes, bars * 4.0, 0.08);
    (kick(16, 0.0), bass, pad)
}

fn mix(parts: &[&[f32]]) -> Vec<f32> {
    let n = parts.iter().map(|p| p.len()).max().unwrap_or(0);
    (0..n)
        .map(|i| parts.iter().map(|p| p.get(i).copied().unwrap_or(0.0)).sum())
        .collect()
}

fn stereo(x: &[f32]) -> Vec<Vec<f32>> {
    vec![x.to_vec(), x.to_vec()]
}

fn clip(channels: &[Vec<f32>]) -> Clip<'_> {
    Clip {
        channels,
        sample_rate: RATE,
        start_s: 0.0,
        timing: Some(&Steady),
    }
}

fn bars(n: usize) -> Vec<(f64, f64)> {
    (0..n)
        .map(|b| (seconds(b as f64 * 4.0), seconds((b + 1) as f64 * 4.0)))
        .collect()
}

#[test]
fn describes_the_bass_line() {
    let (_, bass, _) = song();
    let channels = vec![bass];
    let options = DescribeOptions {
        segments: bars(4),
        ..DescribeOptions::default()
    };
    let d = describe(&clip(&channels), &options);
    let notes = d.notes.as_ref().expect("notes");
    let pitches: Vec<&str> = notes.list.iter().map(|n| n.pitch.as_str()).collect();
    assert_eq!(pitches, ["A2", "F2", "C3", "G2"], "{notes:#?}");
    assert!(notes.voiced > 0.9, "{notes:#?}");
    for n in &notes.list {
        assert!(n.cents.abs() <= 5.0, "{n:?}");
        assert!((n.length_beats.unwrap() - 4.0).abs() < 0.1, "{n:?}");
    }
    // Within a 10 ms pitch frame of the bar line (a 120 BPM bar is 2 s).
    assert!(
        (notes.list[1].bar.unwrap() - 2.0).abs() < 0.01,
        "{notes:#?}"
    );
    let timeline: Vec<&str> = d.timeline.iter().map(|m| m.notes.as_str()).collect();
    assert_eq!(timeline, ["A2", "F2", "C3", "G2"]);
    assert!(
        d.observations
            .iter()
            .any(|o| o.contains("A single line: 4 notes")),
        "{:#?}",
        d.observations
    );
}

#[test]
fn describes_the_harmony() {
    let (_, bass, pad) = song();
    let channels = stereo(&mix(&[&bass, &pad]));
    let options = DescribeOptions {
        segments: bars(4),
        ..DescribeOptions::default()
    };
    let d = describe(&clip(&channels), &options);
    let chords: Vec<Option<&str>> = d.timeline.iter().map(|m| m.chord.as_deref()).collect();
    assert_eq!(
        chords,
        [Some("Am"), Some("F"), Some("C"), Some("G")],
        "{d:#?}"
    );
    let tonality = d.tonality.as_ref().expect("tonality");
    // A minor and C major share their notes; either is a fair answer.
    assert!(
        ["A minor", "C major"].contains(&tonality.key.as_deref().unwrap()),
        "{tonality:#?}"
    );
    assert!(
        d.observations.iter().any(|o| o.contains("Am → F → C → G")),
        "{:#?}",
        d.observations
    );
    assert!(
        d.observations.iter().any(|o| o.contains("it's mono")),
        "{:#?}",
        d.observations
    );
}

#[test]
fn hears_the_kick_on_the_grid_and_when_it_drags() {
    let on_time = vec![kick(16, 0.0)];
    let d = describe(&clip(&on_time), &DescribeOptions::default());
    assert_eq!(d.rhythm.onsets, 16, "{:#?}", d.rhythm);
    let tempo = d.rhythm.tempo.as_ref().expect("tempo");
    assert!((tempo.bpm - BPM).abs() < 1.0, "{tempo:?}");
    assert_eq!(tempo.project_bpm, Some(BPM));
    let timing = d.rhythm.timing.as_ref().expect("timing");
    assert!(timing.median_offset_ms.abs() < 3.0, "{timing:?}");
    assert!(timing.within_10ms > 0.9, "{timing:?}");

    let late = vec![kick(16, 15.0)];
    let d = describe(&clip(&late), &DescribeOptions::default());
    let timing = d.rhythm.timing.as_ref().expect("timing");
    assert!((timing.median_offset_ms - 15.0).abs() < 3.0, "{timing:?}");
    assert!(
        d.observations.iter().any(|o| o.contains("ms late")),
        "{:#?}",
        d.observations
    );
}

#[test]
fn flags_clipping_phase_and_silence() {
    let loud: Vec<f32> = (0..RATE)
        .map(|i| (1.5 * (TAU * 220.0 * f64::from(i) / f64::from(RATE)).sin()) as f32)
        .collect();
    let inverted: Vec<f32> = loud.iter().map(|s| -s * 0.5).collect();
    let half: Vec<f32> = loud.iter().map(|s| s * 0.5).collect();
    let channels = vec![half, inverted];
    let d = describe(&clip(&channels), &DescribeOptions::default());
    assert!(d.levels.clipped_samples == 0);
    assert!(d.stereo.as_ref().unwrap().correlation < -0.9);
    assert!(
        d.observations.iter().any(|o| o.contains("out of phase")),
        "{:#?}",
        d.observations
    );

    let channels = vec![loud];
    let d = describe(&clip(&channels), &DescribeOptions::default());
    assert!(d.levels.clipped_samples > 0);
    assert!(
        d.observations.iter().any(|o| o.contains("full scale")),
        "{:#?}",
        d.observations
    );
    let peak = &d.spectrum.as_ref().unwrap().peaks[0];
    assert_eq!(peak.note, "A3", "{peak:?}");

    let silence = vec![vec![0.0f32; RATE as usize]];
    let d = describe(&clip(&silence), &DescribeOptions::default());
    assert_eq!(d.observations, ["Silent: every sample is zero."]);
    assert!(d.spectrum.is_none());
    // The report serializes (it's what MCP returns).
    serde_json::to_string(&d).unwrap();
}

#[test]
fn draws_pictures() {
    let (kick, bass, pad) = song();
    let stems = [
        ("Kick".to_owned(), stereo(&kick)),
        ("Bass".to_owned(), stereo(&bass)),
        ("Pad".to_owned(), stereo(&pad)),
    ];
    let full = stereo(&mix(&[&kick, &bass, &pad]));
    let out = std::env::var_os("DAWKEY_LISTEN_PICTURES").map(std::path::PathBuf::from);
    for kind in [
        PictureKind::Spectrogram,
        PictureKind::Spectrum,
        PictureKind::Chroma,
    ] {
        let mut options = PictureOptions::new(kind);
        options.title = format!("{kind:?}, bars 1 to 5");
        let mut panels = vec![Panel::prepare("Mix", &clip(&full), &options)];
        for (label, channels) in &stems {
            panels.push(Panel::prepare(label.clone(), &clip(channels), &options));
        }
        let p = picture(&panels, &options);
        assert_eq!(p.width, 1200);
        assert_eq!(p.rgb.len(), (p.width * p.height * 3) as usize);
        assert!(!p.caption.is_empty());
        let png = p.to_png();
        assert_eq!(&png[1..4], b"PNG");
        if let Some(dir) = &out {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(format!("{kind:?}.png").to_lowercase()), png).unwrap();
        }
        // The same audio draws the same pixels.
        let again = picture(&panels, &options);
        assert_eq!(again.rgb, p.rgb);
    }
}
