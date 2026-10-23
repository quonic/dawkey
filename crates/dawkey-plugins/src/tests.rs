// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hosts the plugins from `dawkey-test-plugins`. Processing runs under
//! `assert_no_alloc`, so the host side is checked for allocations (the
//! plugins' own code is allowed to allocate).

use std::sync::OnceLock;
use std::time::Duration;

use dawkey_engine::{
    AuxBuffer, MAX_PLUGIN_EVENTS, MidiSink, PluginEvent, PluginEventKind, PluginProcessor,
    ProcessContext, Transport,
};

use crate::{Catalog, Host, PluginFormat, PluginInfo, SearchPath, StatePaths, scan};

/// The test plugins, installed once for every test.
fn installed() -> &'static (tempfile::TempDir, SearchPath, Catalog) {
    static INSTALLED: OnceLock<(tempfile::TempDir, SearchPath, Catalog)> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let installed = dawkey_test_plugins::install(dir.path()).unwrap();
        let search = SearchPath {
            clap: vec![installed.clap_path],
            lv2: Some(vec![installed.lv2_path]),
            vst3: vec![installed.vst3_path],
        };
        let catalog = scan(&search, None, Duration::from_secs(10));
        (dir, search, catalog)
    })
}

fn catalog() -> &'static Catalog {
    &installed().2
}

fn host() -> Host {
    Host::new(48_000, installed().1.clone())
}

fn plugin(id: &str) -> &'static PluginInfo {
    catalog()
        .get(id)
        .unwrap_or_else(|| panic!("{id} not found in {:#?}", catalog()))
}

fn run(
    p: &mut Box<dyn PluginProcessor>,
    input: f32,
    frames: usize,
    events: &[PluginEvent],
) -> Vec<f32> {
    run_midi(p, input, frames, events).0
}

/// Like [`run`], also returning the MIDI the plugin sent.
fn run_midi(
    p: &mut Box<dyn PluginProcessor>,
    input: f32,
    frames: usize,
    events: &[PluginEvent],
) -> (Vec<f32>, Vec<PluginEvent>) {
    let (left, right, sent) = run_stereo(p, input, frames, events, Transport::default());
    assert_eq!(left, right);
    (left, sent)
}

/// One block with `transport`: both channels, and the MIDI sent.
fn run_stereo(
    p: &mut Box<dyn PluginProcessor>,
    input: f32,
    frames: usize,
    events: &[PluginEvent],
    transport: Transport,
) -> (Vec<f32>, Vec<f32>, Vec<PluginEvent>) {
    let mut left = vec![input; frames];
    let mut right = vec![input; frames];
    let mut sent = Vec::with_capacity(MAX_PLUGIN_EVENTS);
    assert_no_alloc::assert_no_alloc(|| {
        p.process(
            [&mut left, &mut right],
            ProcessContext::new(transport),
            events,
            MidiSink::new(&mut sent),
        )
    });
    (left, right, sent)
}

fn param(offset: u32, value: f64) -> PluginEvent {
    PluginEvent {
        offset,
        kind: PluginEventKind::Param { index: 0, value },
    }
}

fn note_on(offset: u32, key: u8) -> PluginEvent {
    PluginEvent {
        offset,
        kind: PluginEventKind::NoteOn {
            channel: 0,
            key,
            velocity: 1.0,
        },
    }
}

fn note_off(offset: u32, key: u8) -> PluginEvent {
    PluginEvent {
        offset,
        kind: PluginEventKind::NoteOff { channel: 0, key },
    }
}

/// The ID of the VST3 test plugin called `name`.
fn vst3(name: &str) -> String {
    dawkey_test_plugins::vst3_id(name).unwrap()
}

/// A gain in dB as plugin `id` takes it (VST3: normalized, 0 to 1 for -60
/// to +12 dB).
fn gain_value(id: &str, db: f64) -> f64 {
    if id.starts_with("vst3:") {
        (db + 60.0) / 72.0
    } else {
        db
    }
}

#[test]
fn scans_both_formats() {
    let gain = plugin("clap:dawkey.test.gain");
    assert_eq!(gain.format, PluginFormat::Clap);
    assert_eq!(
        (gain.name.as_str(), gain.vendor.as_str()),
        ("Test Gain", "Dawkey")
    );
    assert_eq!(
        (gain.audio_inputs, gain.audio_outputs, gain.instrument),
        (2, 2, false)
    );
    let p = &gain.params[0];
    assert_eq!(
        (p.id.as_str(), p.name.as_str(), p.min, p.max, p.default),
        ("1", "Gain", -60.0, 12.0, 0.0)
    );
    assert!(plugin("clap:dawkey.test.beep").instrument);
    assert!(!plugin("clap:dawkey.test.beep").midi_output);
    for id in [
        "clap:dawkey.test.octave",
        "lv2:urn:dawkey:test:octave",
        &vst3("octave"),
    ] {
        let octave = plugin(id);
        assert_eq!(
            (
                octave.instrument,
                octave.midi_output,
                octave.audio_inputs,
                octave.audio_outputs
            ),
            (true, true, 0, 0),
            "{id}"
        );
    }
    assert!(plugin("clap:dawkey.test.delay").params.is_empty());

    let lv2 = plugin("lv2:urn:dawkey:test:gain");
    assert_eq!(lv2.format, PluginFormat::Lv2);
    assert_eq!(lv2.name, "Test Gain (LV2)");
    assert_eq!(
        (lv2.audio_inputs, lv2.audio_outputs, lv2.instrument),
        (2, 2, false)
    );
    assert_eq!(lv2.params[0].id, "gain");
    let beep = plugin("lv2:urn:dawkey:test:beep");
    assert_eq!(
        (beep.instrument, beep.audio_inputs, beep.audio_outputs),
        (true, 0, 2)
    );

    let vst3_gain = plugin(&vst3("gain"));
    assert_eq!(vst3_gain.format, PluginFormat::Vst3);
    assert_eq!(
        (vst3_gain.name.as_str(), vst3_gain.vendor.as_str()),
        ("Test Gain (VST3)", "Dawkey")
    );
    assert!(vst3_gain.path.ends_with("dawkey-test.vst3"));
    assert_eq!(
        (
            vst3_gain.audio_inputs,
            vst3_gain.audio_outputs,
            vst3_gain.instrument
        ),
        (2, 2, false)
    );
    let p = &vst3_gain.params[0];
    assert_eq!(
        (p.id.as_str(), p.name.as_str(), p.min, p.max, p.stepped),
        ("1", "Gain", 0.0, 1.0, false)
    );
    assert!((p.default - 60.0 / 72.0).abs() < 1e-9);
    // Hidden and read-only parameters (the beep's mapped CC 7 and its
    // trigger) aren't in the catalog.
    let beep = plugin(&vst3("beep"));
    assert!(beep.instrument && !beep.midi_output);
    assert_eq!((beep.audio_inputs, beep.audio_outputs), (0, 2));
    assert_eq!(beep.params.len(), 1);
    assert_eq!(beep.params[0].name, "Level");
    assert!(plugin(&vst3("delay")).params.is_empty());
    // The memo's controller is a class of its own, not a plugin.
    assert!(
        catalog()
            .plugins
            .iter()
            .filter(|p| p.format == PluginFormat::Vst3)
            .all(|p| p.name.ends_with("(VST3)")),
    );
    assert_eq!(
        catalog()
            .plugins
            .iter()
            .filter(|p| p.format == PluginFormat::Vst3)
            .count(),
        8
    );
    assert!(catalog().failures.is_empty(), "{:?}", catalog().failures);
}

/// -6.0206 dB halves the level.
const HALF_DB: f64 = -6.020_599_913_279_624;

#[test]
fn hosts_gain_effects_with_sample_accurate_params() {
    let mut host = host();
    for id in [
        "clap:dawkey.test.gain",
        "lv2:urn:dawkey:test:gain",
        &vst3("gain"),
    ] {
        let mut loaded = host.load(plugin(id)).unwrap();
        assert_eq!(loaded.latency, 0);
        let out = run(&mut loaded.processor, 1.0, 64, &[]);
        assert!(out.iter().all(|&s| (s - 1.0).abs() < 1e-6), "{id}: {out:?}");
        let out = run(
            &mut loaded.processor,
            1.0,
            64,
            &[param(32, gain_value(id, HALF_DB))],
        );
        assert!(
            out[..32].iter().all(|&s| (s - 1.0).abs() < 1e-6),
            "{id}: {out:?}"
        );
        assert!(
            out[32..].iter().all(|&s| (s - 0.5).abs() < 1e-6),
            "{id}: {out:?}"
        );
        loaded.processor.stop();
        loaded.instance.unload(loaded.processor.into_any());
    }
}

#[test]
fn shows_parameter_values_as_the_plugin_does() {
    let mut host = host();
    let clap = plugin("clap:dawkey.test.gain");
    let mut loaded = host.load(clap).unwrap();
    assert_eq!(
        loaded.instance.param_text(clap, 0, -6.0).as_deref(),
        Some("-6.0 dB")
    );
    loaded.instance.unload(loaded.processor.into_any());
    let lv2 = plugin("lv2:urn:dawkey:test:gain");
    assert_eq!(lv2.params[0].unit.as_deref(), Some("dB"));
    let mut loaded = host.load(lv2).unwrap();
    assert_eq!(
        loaded.instance.param_text(lv2, 0, -6.5).as_deref(),
        Some("-6.50 dB")
    );
    loaded.instance.unload(loaded.processor.into_any());
    // VST3 plugins too, from their normalized values.
    let vst3 = plugin(&vst3("gain"));
    let mut loaded = host.load(vst3).unwrap();
    assert_eq!(
        loaded
            .instance
            .param_text(vst3, 0, gain_value(&vst3.id, -6.0))
            .as_deref(),
        Some("-6.0 dB")
    );
    loaded.instance.unload(loaded.processor.into_any());
    // Scale points name values.
    let level = &plugin("lv2:urn:dawkey:test:beep").params[0];
    assert_eq!(
        (level.text(0.0), level.text(1.0), level.text(0.25)),
        ("Off".to_owned(), "Full".to_owned(), "0.25".to_owned())
    );
}

#[test]
fn reads_typed_parameter_values_as_the_plugin_does() {
    let mut host = host();
    // CLAP plugins read the text themselves.
    let clap = plugin("clap:dawkey.test.gain");
    let mut loaded = host.load(clap).unwrap();
    let mut read = |text: &str| loaded.instance.param_value(clap, 0, text);
    assert_eq!(read("-12 dB"), Some(-12.0));
    assert_eq!(read(" -3.5 "), Some(-3.5));
    assert_eq!(read("loud"), None);
    loaded.instance.unload(loaded.processor.into_any());
    let beep = plugin("clap:dawkey.test.beep");
    let mut loaded = host.load(beep).unwrap();
    assert_eq!(loaded.instance.param_value(beep, 0, "50 %"), Some(0.5));
    loaded.instance.unload(loaded.processor.into_any());
    // VST3 plugins too, into their normalized values.
    let gain_info = plugin(&vst3("gain"));
    let mut loaded = host.load(gain_info).unwrap();
    let value = loaded.instance.param_value(gain_info, 0, "-12 dB").unwrap();
    assert!(
        (value - gain_value(&gain_info.id, -12.0)).abs() < 1e-9,
        "{value}"
    );
    assert_eq!(loaded.instance.param_value(gain_info, 0, "loud"), None);
    loaded.instance.unload(loaded.processor.into_any());
    let beep = plugin(&vst3("beep"));
    let mut loaded = host.load(beep).unwrap();
    assert_eq!(loaded.instance.param_value(beep, 0, "50 %"), Some(0.5));
    loaded.instance.unload(loaded.processor.into_any());
    // LV2: a number, with or without its unit, or a label in any case.
    let lv2 = plugin("lv2:urn:dawkey:test:gain");
    let mut loaded = host.load(lv2).unwrap();
    for (text, value) in [
        ("-6 dB", Some(-6.0)),
        ("-6db", Some(-6.0)),
        ("2", Some(2.0)),
    ] {
        assert_eq!(loaded.instance.param_value(lv2, 0, text), value, "{text}");
    }
    assert_eq!(loaded.instance.param_value(lv2, 0, "-6 Hz"), None);
    loaded.instance.unload(loaded.processor.into_any());
    let level = &plugin("lv2:urn:dawkey:test:beep").params[0];
    assert_eq!(
        (level.parse("off"), level.parse("Full"), level.parse("0.3")),
        (Some(0.0), Some(1.0), Some(0.3))
    );
}

#[test]
fn saves_and_restores_plugin_state() {
    let mut host = host();
    for id in [
        "clap:dawkey.test.memo",
        "lv2:urn:dawkey:test:memo",
        &vst3("memo"),
    ] {
        let info = plugin(id);
        assert!(info.instrument, "{id} takes notes");
        let mut loaded = host.load(info).unwrap();
        let out = run(&mut loaded.processor, 0.25, 32, &[]);
        assert!(out.iter().all(|&s| s == 0.25), "{id}: {out:?}");
        // A note at full velocity: it remembers 1.0.
        let out = run(&mut loaded.processor, 0.25, 32, &[note_on(0, 60)]);
        assert!(out.iter().all(|&s| s == 1.25), "{id}: {out:?}");
        if !id.starts_with("lv2:") {
            // It asks for the main thread to say its state changed (the
            // VST3 memo tells its controller, which says so).
            loaded.instance.idle();
            assert!(loaded.instance.state_dirty(), "{id}");
        }
        let state = loaded
            .instance
            .save_state(&StatePaths::default())
            .unwrap()
            .expect("it saves state");
        assert!(!loaded.instance.state_dirty());
        loaded.processor.stop();
        loaded.instance.unload(loaded.processor.into_any());

        // A fresh instance starts from it.
        let mut restored = host
            .load_with_state(info, Some(&state), &StatePaths::default())
            .unwrap();
        let out = run(&mut restored.processor, 0.0, 32, &[]);
        assert!(out.iter().all(|&s| s == 1.0), "{id}: {out:?}");
        restored.instance.unload(restored.processor.into_any());
        // A state it can't read is dropped: it starts as new.
        let mut fresh = host
            .load_with_state(info, Some(b"bad"), &StatePaths::default())
            .unwrap();
        let out = run(&mut fresh.processor, 0.0, 32, &[]);
        assert!(out.iter().all(|&s| s == 0.0), "{id}: {out:?}");
        fresh.instance.unload(fresh.processor.into_any());
    }
    // Plugins without state save nothing.
    for id in ["lv2:urn:dawkey:test:gain", &vst3("gain")] {
        let mut gain = host.load(plugin(id)).unwrap();
        assert_eq!(
            gain.instance.save_state(&StatePaths::default()).unwrap(),
            None,
            "{id}"
        );
        gain.instance.unload(gain.processor.into_any());
    }
}

#[test]
fn lv2_state_keeps_files_with_the_project() {
    let mut host = host();
    let info = plugin("lv2:urn:dawkey:test:memo");
    let dir = tempfile::tempdir().unwrap();
    let paths = |base: &std::path::Path| StatePaths {
        base: Some(base.to_owned()),
        files: Some(base.join("plugins").join("3")),
        kept: None,
    };
    let project = dir.path().join("Song.dawkey");
    let mut loaded = host.load(info).unwrap();
    run(&mut loaded.processor, 0.0, 32, &[note_on(0, 60)]);
    // Given somewhere to make files, the memo keeps its memo in one, and
    // says where it is relative to the project.
    let state = loaded
        .instance
        .save_state(&paths(&project))
        .unwrap()
        .unwrap();
    loaded.instance.unload(loaded.processor.into_any());
    let file = project.join("plugins/3/memo.txt");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "1");
    let has = |needle: &[u8]| state.windows(needle.len()).any(|w| w == needle);
    assert!(has(b"plugins/3/memo.txt\0"));
    assert!(!has(project.as_os_str().as_encoded_bytes()));

    // The project moves: the state finds the file where it went.
    let moved = dir.path().join("Moved.dawkey");
    std::fs::rename(&project, &moved).unwrap();
    let mut restored = host
        .load_with_state(info, Some(&state), &paths(&moved))
        .unwrap();
    let out = run(&mut restored.processor, 0.0, 32, &[]);
    assert!(out.iter().all(|&s| s == 1.0), "{out:?}");
    restored.instance.unload(restored.processor.into_any());
}

#[test]
fn lv2_files_made_elsewhere_are_saved_as_kept_in_the_project() {
    let mut host = host();
    let info = plugin("lv2:urn:dawkey:test:memo");
    let dir = tempfile::tempdir().unwrap();
    let (project, staging) = (
        dir.path().join("Song.dawkey"),
        dir.path().join("cache/plugins/3"),
    );
    let paths = StatePaths {
        base: Some(project.clone()),
        files: Some(staging.clone()),
        kept: Some("plugins/3".into()),
    };
    let mut loaded = host.load(info).unwrap();
    run(&mut loaded.processor, 0.0, 32, &[note_on(0, 60)]);
    // The file goes where Dawkey keeps it while it runs; the state says
    // where it will be in the project.
    let state = loaded.instance.save_state(&paths).unwrap().unwrap();
    loaded.instance.unload(loaded.processor.into_any());
    assert_eq!(
        std::fs::read_to_string(staging.join("memo.txt")).unwrap(),
        "1"
    );
    let has = |needle: &[u8]| state.windows(needle.len()).any(|w| w == needle);
    assert!(has(b"plugins/3/memo.txt\0"));
    assert!(!has(b"cache"));

    // Copied there (as saving does), the state finds it.
    std::fs::create_dir_all(project.join("plugins")).unwrap();
    std::fs::rename(
        dir.path().join("cache/plugins/3"),
        project.join("plugins/3"),
    )
    .unwrap();
    let mut restored = host.load_with_state(info, Some(&state), &paths).unwrap();
    let out = run(&mut restored.processor, 0.0, 32, &[]);
    assert!(out.iter().all(|&s| s == 1.0), "{out:?}");
    restored.instance.unload(restored.processor.into_any());
}

#[test]
fn plugins_follow_the_transport() {
    let mut host = host();
    for id in [
        "clap:dawkey.test.tempo",
        "lv2:urn:dawkey:test:tempo",
        &vst3("tempo"),
    ] {
        let mut loaded = host.load(plugin(id)).unwrap();
        let rolling = Transport {
            playing: true,
            frame: 96_000,
            seconds: 2.0,
            beats: 4.0,
            bpm: 120.0,
            bar: 1,
            bar_start_beats: 4.0,
            numerator: 4,
            denominator: 4,
            ..Transport::default()
        };
        let (left, right, _) = run_stereo(&mut loaded.processor, 0.0, 64, &[], rolling);
        assert_eq!((left[63], right[63]), (0.12, 0.02), "{id}");
        // The next block, where the last one left off: LV2 plugins aren't
        // told again, and keep what they know.
        let next = Transport {
            frame: 96_064,
            ..rolling
        };
        let (left, right, _) = run_stereo(&mut loaded.processor, 0.0, 64, &[], next);
        assert_eq!((left[0], right[0]), (0.12, 0.02), "{id}");
        // Stopped at a new tempo.
        let stopped = Transport {
            playing: false,
            bpm: 90.0,
            ..next
        };
        let (left, right, _) = run_stereo(&mut loaded.processor, 0.0, 64, &[], stopped);
        assert_eq!((left[0], right[0]), (0.09, 0.0), "{id}");
        loaded.instance.unload(loaded.processor.into_any());
    }
}

#[test]
fn sidechains_and_other_outputs() {
    let mut host = host();
    for id in [
        "clap:dawkey.test.subtract",
        "lv2:urn:dawkey:test:subtract",
        &vst3("subtract"),
    ] {
        let info = plugin(id);
        assert!(info.sidechain, "{id}");
        assert_eq!((info.audio_inputs, info.audio_outputs), (2, 2), "{id}");
        let mut loaded = host.load(info).unwrap();
        let (mut left, mut right) = (vec![1.0; 32], vec![1.0; 32]);
        let side = [vec![0.25; 32], vec![0.5; 32]];
        let mut sent = Vec::with_capacity(MAX_PLUGIN_EVENTS);
        assert_no_alloc::assert_no_alloc(|| {
            let context = ProcessContext {
                sidechain: Some([&side[0], &side[1]]),
                ..ProcessContext::new(Transport::default())
            };
            loaded.processor.process(
                [&mut left, &mut right],
                context,
                &[],
                MidiSink::new(&mut sent),
            )
        });
        assert_eq!((left[0], right[0]), (0.75, 0.5), "{id}");
        // Without one, the sidechain is silent.
        let out = run(&mut loaded.processor, 1.0, 32, &[]);
        assert_eq!(out[0], 1.0, "{id}");
        loaded.instance.unload(loaded.processor.into_any());
    }
    for id in [
        "clap:dawkey.test.split",
        "lv2:urn:dawkey:test:split",
        &vst3("split"),
    ] {
        let info = plugin(id);
        assert!(!info.sidechain);
        assert_eq!(info.outputs.len(), 1, "{id}");
        assert_eq!(
            (info.outputs[0].name.as_str(), info.outputs[0].channels),
            ("High", 2),
            "{id}"
        );
        let mut loaded = host.load(info).unwrap();
        let (mut left, mut right) = (vec![0.0; 32], vec![0.0; 32]);
        let mut aux = [AuxBuffer::new()];
        let mut sent = Vec::with_capacity(MAX_PLUGIN_EVENTS);
        let notes = [note_on(0, 40), note_on(0, 72)];
        let mut notes = notes.to_vec();
        if let PluginEventKind::NoteOn { velocity, .. } = &mut notes[1].kind {
            *velocity = 0.5;
        }
        assert_no_alloc::assert_no_alloc(|| {
            let context = ProcessContext {
                aux: &mut aux,
                ..ProcessContext::new(Transport::default())
            };
            loaded.processor.process(
                [&mut left, &mut right],
                context,
                &notes,
                MidiSink::new(&mut sent),
            )
        });
        assert_eq!(left[31], 1.0, "{id}: the low key on the main output");
        let high = &aux[0].channels;
        assert!((high[0][31] - 0.5).abs() < 0.01, "{id}: {}", high[0][31]);
        assert_eq!(high[0][31], high[1][31]);
        loaded.instance.unload(loaded.processor.into_any());
    }
}

#[test]
fn reports_and_applies_latency() {
    let mut host = host();
    for id in ["clap:dawkey.test.delay", &vst3("delay")] {
        let mut loaded = host.load(plugin(id)).unwrap();
        assert_eq!(loaded.latency, dawkey_test_plugins::DELAY_FRAMES, "{id}");
        let first = run(&mut loaded.processor, 1.0, 48, &[]);
        assert!(first.iter().all(|&s| s == 0.0), "{id}");
        let second = run(&mut loaded.processor, 1.0, 48, &[]);
        // 64 frames of delay: the input shows up 16 frames into the second
        // block.
        assert!(second[..16].iter().all(|&s| s == 0.0), "{id}");
        assert!(second[16..].iter().all(|&s| s == 1.0), "{id}");
        loaded.processor.stop();
        loaded.instance.unload(loaded.processor.into_any());
    }
}

#[test]
fn hosts_instruments() {
    let mut host = host();
    for id in [
        "clap:dawkey.test.beep",
        "lv2:urn:dawkey:test:beep",
        &vst3("beep"),
    ] {
        let mut loaded = host.load(plugin(id)).unwrap();
        // The track's audio is replaced by the instrument's.
        let out = run(
            &mut loaded.processor,
            0.25,
            64,
            &[note_on(10, 60), note_off(20, 60)],
        );
        assert!(out[..10].iter().all(|&s| s == 0.0), "{id}: {out:?}");
        assert!(
            out[10..20].iter().all(|&s| (s - 1.0).abs() < 1e-6),
            "{id}: {out:?}"
        );
        assert!(out[20..].iter().all(|&s| s == 0.0), "{id}: {out:?}");
        // A parameter change and a note at the same frame both land.
        let out = run(
            &mut loaded.processor,
            0.0,
            64,
            &[param(8, 0.5), note_on(8, 62)],
        );
        assert!(out[..8].iter().all(|&s| s == 0.0), "{id}: {out:?}");
        assert!(
            out[8..].iter().all(|&s| (s - 0.5).abs() < 1e-6),
            "{id}: {out:?}"
        );
        loaded.processor.stop();
        loaded.instance.unload(loaded.processor.into_any());
    }
}

#[test]
fn per_note_controllers_reach_clap_and_vst3_as_note_expressions() {
    let mut host = host();
    for id in ["clap:dawkey.test.beep", &vst3("beep")] {
        per_note_volume_reaches(&mut host, id);
    }
    let volume = |offset, key, value| PluginEvent {
        offset,
        kind: PluginEventKind::PerNote {
            channel: 0,
            key,
            controller: dawkey_engine::PerNote::Registered(7),
            value,
        },
    };
    // The LV2 beep (MIDI 1.0) doesn't get them.
    let mut lv2 = host.load(plugin("lv2:urn:dawkey:test:beep")).unwrap();
    let out = run(
        &mut lv2.processor,
        0.0,
        16,
        &[param(0, 1.0), volume(0, 60, 0), note_on(0, 60)],
    );
    assert!(out.iter().all(|&s| (s - 1.0).abs() < 1e-6), "{out:?}");
    let mut processor = lv2.processor;
    processor.stop();
    lv2.instance.unload(processor.into_any());
}

fn per_note_volume_reaches(host: &mut Host, id: &str) {
    let mut loaded = host.load(plugin(id)).unwrap();
    // Per-note volume at half (0.5² = 0.25 as linear gain) on key 60 only;
    // the level parameter is 1.
    let volume = |offset, key, value| PluginEvent {
        offset,
        kind: PluginEventKind::PerNote {
            channel: 0,
            key,
            controller: dawkey_engine::PerNote::Registered(7),
            value,
        },
    };
    let half = u32::MAX / 2;
    let out = run(
        &mut loaded.processor,
        0.0,
        64,
        &[
            param(0, 1.0),
            volume(0, 60, half),
            note_on(0, 60),
            note_on(32, 62),
        ],
    );
    assert!(
        out[..32].iter().all(|&s| (s - 0.25).abs() < 1e-6),
        "{out:?}"
    );
    assert!(
        out[32..].iter().all(|&s| (s - 1.25).abs() < 1e-6),
        "{id}: {out:?}"
    );
    let mut processor = loaded.processor;
    processor.stop();
    loaded.instance.unload(processor.into_any());
}

#[test]
fn hosts_midi_effects() {
    let mut host = host();
    for id in [
        "clap:dawkey.test.octave",
        "lv2:urn:dawkey:test:octave",
        &vst3("octave"),
    ] {
        let mut loaded = host.load(plugin(id)).unwrap();
        let bend = PluginEvent {
            offset: 12,
            kind: PluginEventKind::Midi {
                data: [0xe0, 0, 64],
            },
        };
        let (out, sent) = run_midi(
            &mut loaded.processor,
            0.25,
            64,
            &[note_on(10, 60), bend, note_off(20, 60)],
        );
        // No audio ports: the track's audio passes by.
        assert!(out.iter().all(|&s| s == 0.25), "{id}: {out:?}");
        let sent: Vec<(u32, PluginEventKind)> = sent.iter().map(|e| (e.offset, e.kind)).collect();
        let expected = vec![
            (
                10,
                PluginEventKind::NoteOn {
                    channel: 0,
                    key: 72,
                    velocity: 1.0,
                },
            ),
            (12, bend.kind),
            (
                20,
                PluginEventKind::NoteOff {
                    channel: 0,
                    key: 72,
                },
            ),
        ];
        assert_eq!(sent, expected, "{id}");
        loaded.processor.stop();
        loaded.instance.unload(loaded.processor.into_any());
    }
}

#[test]
fn note_expressions_a_plugin_sends_become_per_note_controllers() {
    let mut host = host();
    for id in ["clap:dawkey.test.octave", &vst3("octave")] {
        expressions_come_back(&mut host, id);
    }
}

fn expressions_come_back(host: &mut Host, id: &str) {
    let mut loaded = host.load(plugin(id)).unwrap();
    let per_note = |key, controller, value| PluginEvent {
        offset: 4,
        kind: PluginEventKind::PerNote {
            channel: 1,
            key,
            controller,
            value,
        },
    };
    let (_, sent) = run_midi(
        &mut loaded.processor,
        0.0,
        16,
        &[
            per_note(60, dawkey_engine::PerNote::Registered(7), 0x4000_0000),
            per_note(61, dawkey_engine::PerNote::PitchBend, 0xc000_0000),
        ],
    );
    // Octave sends them on an octave up, as note expressions, and they
    // come back as the same per-note controllers.
    let sent: Vec<_> = sent
        .iter()
        .map(|e| match e.kind {
            PluginEventKind::PerNote {
                channel,
                key,
                controller,
                value,
            } => (e.offset, channel, key, controller, value),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        sent,
        [
            (4, 1, 72, dawkey_engine::PerNote::Registered(7), 0x4000_0000),
            (4, 1, 73, dawkey_engine::PerNote::PitchBend, 0xc000_0000),
        ],
        "{id}"
    );

    // A per-note reset reaches it as every expression at its default,
    // and comes back as those values (pressure as poly pressure).
    let reset = dawkey_engine::PerNote::Management {
        detach: false,
        reset: true,
    };
    let (_, sent) = run_midi(&mut loaded.processor, 0.0, 16, &[per_note(60, reset, 0)]);
    let kinds: Vec<_> = sent.iter().map(|e| e.kind).collect();
    assert_eq!(kinds.len(), 7, "{id}: {kinds:?}");
    assert!(kinds.contains(&PluginEventKind::PerNote {
        channel: 1,
        key: 72,
        controller: dawkey_engine::PerNote::Registered(7),
        value: u32::MAX,
    }));
    assert!(kinds.contains(&PluginEventKind::Midi {
        data: [0xa1, 72, 0]
    }));
    loaded.processor.stop();
    loaded.instance.unload(loaded.processor.into_any());
}

#[test]
fn missing_plugins_fail_to_load() {
    let mut host = host();
    let mut gone = plugin("lv2:urn:dawkey:test:gain").clone();
    gone.id = "lv2:urn:dawkey:test:nope".into();
    let err = host.load(&gone).unwrap_err();
    assert!(err.0.contains("not installed"), "{err}");
}

#[test]
fn lv2_plugins_whose_ports_cant_be_read_are_refused() {
    // Its port is described nowhere lilv can read (like ll-plugins' math
    // constants, whose shared blank-node port lilv loses): it would seem to
    // have none, and write to a port nobody connected.
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("broken.lv2");
    std::fs::create_dir(&bundle).unwrap();
    std::fs::write(
        bundle.join("manifest.ttl"),
        "@prefix lv2: <http://lv2plug.in/ns/lv2core#> .\n\
         @prefix doap: <http://usefulinc.com/ns/doap#> .\n\
         <urn:dawkey:test:broken> a lv2:Plugin ;\n\
             lv2:binary <broken.so> ;\n\
             doap:name \"Broken\" .\n",
    )
    .unwrap();
    let search = SearchPath {
        clap: Vec::new(),
        lv2: Some(vec![dir.path().to_owned()]),
        vst3: Vec::new(),
    };
    let catalog = scan(&search, None, Duration::from_secs(10));
    assert!(catalog.plugins.is_empty(), "{:?}", catalog.plugins);
    assert!(
        catalog.failures[0].error.contains("ports can't be read"),
        "{:?}",
        catalog.failures
    );
}

#[test]
fn parameters_the_plugin_changes_itself_reach_the_host() {
    use crate::ParamChange;
    let mut host = host();
    // The VST3 beep's processor tells its controller, which edits Level
    // through the component handler.
    for id in ["clap:dawkey.test.beep", &vst3("beep")] {
        let info = plugin(id);
        let mut loaded = host.load(info).unwrap();
        assert_eq!(loaded.instance.take_param_changes(), [], "{id}");
        // Key 127: the plugin turns its Level to half, as its GUI would.
        run(&mut loaded.processor, 0.0, 32, &[note_on(0, 127)]);
        assert_eq!(
            loaded.instance.take_param_changes(),
            [
                ParamChange::Begin { index: 0 },
                ParamChange::Value {
                    index: 0,
                    value: 0.5
                },
                ParamChange::End { index: 0 },
            ],
            "{id}"
        );
        assert_eq!(loaded.instance.take_param_changes(), [], "{id}");
        loaded.instance.unload(loaded.processor.into_any());
    }
}

#[test]
fn vst3_midi_controllers_reach_the_parameters_they_map_to() {
    let mut host = host();
    let mut loaded = host.load(plugin(&vst3("beep"))).unwrap();
    // CC 7 is mapped to a hidden parameter that scales the beep.
    let cc7 = PluginEvent {
        offset: 16,
        kind: PluginEventKind::Midi {
            data: [0xb0, 7, 64],
        },
    };
    let out = run(&mut loaded.processor, 0.0, 32, &[note_on(0, 60), cc7]);
    assert!(out[..16].iter().all(|&s| (s - 1.0).abs() < 1e-6), "{out:?}");
    let scaled = 64.0 / 127.0;
    assert!(
        out[16..].iter().all(|&s| (s - scaled).abs() < 1e-6),
        "{out:?}"
    );
    // Controllers it doesn't map do nothing.
    let cc1 = PluginEvent {
        offset: 0,
        kind: PluginEventKind::Midi { data: [0xb0, 1, 0] },
    };
    let out = run(&mut loaded.processor, 0.0, 8, &[cc1]);
    assert!(out.iter().all(|&s| (s - scaled).abs() < 1e-6), "{out:?}");
    loaded.processor.stop();
    loaded.instance.unload(loaded.processor.into_any());
}

#[test]
fn vst3_editors_run_on_the_hosts_run_loop() {
    use crate::ParamChange;
    let mut host = host();
    let info = plugin(&vst3("gain"));
    let mut loaded = host.load(info).unwrap();
    // The project plays it at -24 dB: its controller hears so.
    run(&mut loaded.processor, 0.0, 16, &[param(0, 0.5)]);
    let instance = &mut loaded.instance;
    instance.idle();
    assert_eq!(instance.vst3_controller_value(0), Some(0.5));
    assert_eq!(instance.gui_kind(), Some(crate::GuiKind::Vst3));
    // Its editor ignores the window it's given (there's no X server here).
    let size = instance
        .gui_open(crate::GuiKind::Vst3, Some(1), "Test Gain")
        .unwrap();
    assert_eq!(size, Some((200, 100)));
    assert!(instance.gui_can_resize());
    // It registered a timer: on its first tick it moves its knob (an edit
    // through the host) and asks for more room, which it takes at once.
    let next = instance.run_timers(std::time::Instant::now());
    assert!(next.is_some());
    instance.run_timers(std::time::Instant::now() + Duration::from_secs(1));
    assert_eq!(instance.gui_idle(&[]), (Some((300, 150)), false));
    assert_eq!(instance.gui_idle(&[]), (None, false));
    assert_eq!(
        instance.take_param_changes(),
        [
            ParamChange::Begin { index: 0 },
            ParamChange::Value {
                index: 0,
                value: 0.25
            },
            ParamChange::End { index: 0 },
        ]
    );
    // Until the project plays the edit, the controller keeps it (what the
    // processor plays now isn't news).
    instance.idle();
    instance.idle();
    assert_eq!(instance.vst3_controller_value(0), Some(0.25));
    // Resizing the window: it keeps to at least 100 × 50.
    assert_eq!(instance.gui_resized(400, 10), Some((400, 50)));
    instance.gui_close();
    // Closed, its timer is gone.
    assert_eq!(instance.run_timers(std::time::Instant::now()), None);
    loaded.instance.unload(loaded.processor.into_any());

    // The delay has a controller but no editor: opening one says so, and
    // then it says it has none.
    let mut delay = host.load(plugin(&vst3("delay"))).unwrap();
    assert_eq!(delay.instance.gui_kind(), Some(crate::GuiKind::Vst3));
    let err = delay
        .instance
        .gui_open(crate::GuiKind::Vst3, Some(1), "Test Delay")
        .unwrap_err();
    assert!(err.0.contains("no GUI"), "{}", err.0);
    assert_eq!(delay.instance.gui_kind(), None);
    delay.instance.unload(delay.processor.into_any());
}

#[test]
fn vst3_states_keep_both_halves() {
    use crate::vst3::{split_state, state_blob};
    assert_eq!(state_blob(&[], &[]), None);
    let blob = state_blob(b"component", b"ctl").unwrap();
    assert_eq!(&blob[..4], b"DKV3");
    assert_eq!(split_state(&blob), Some((&b"component"[..], &b"ctl"[..])));
    let only = state_blob(&[], b"ctl").unwrap();
    assert_eq!(split_state(&only), Some((&b""[..], &b"ctl"[..])));
    assert_eq!(split_state(b"DKV3\x09\0\0\0abc"), None);
    assert_eq!(split_state(b"bad"), None);
}

#[test]
fn lv2_uis_get_meters_and_atoms_and_message_the_plugin() {
    use crate::{GuiKind, ParamChange};
    let mut host = host();
    let info = plugin("lv2:urn:dawkey:test:worker");
    // Offline, the worker works as it's asked: what follows is exact.
    let mut loaded = host
        .load_for(info, None, &StatePaths::default(), true)
        .unwrap();
    assert_eq!(loaded.instance.gui_kind(), Some(GuiKind::Lv2X11));
    assert!(!loaded.instance.state_dirty());
    // The test UI draws nothing, so it needs no window.
    loaded
        .instance
        .gui_open(GuiKind::Lv2X11, Some(0), "Test Worker")
        .unwrap();
    // Opening, it sent the plugin a level (a patch:Set), which changes the
    // plugin's state.
    assert!(loaded.instance.state_dirty());
    // The message arrives with the next block, and the worker's response
    // after it; the level plays from the block after.
    assert_eq!(run(&mut loaded.processor, 0.0, 32, &[]), [0.0; 32]);
    assert_eq!(run(&mut loaded.processor, 0.0, 32, &[]), [0.5; 32]);
    // The UI gets the meter (the plugin's output port), then the level the
    // plugin sent on its notify port (which only fits in the buffer it
    // asked for), and reports both through `echo`.
    assert_eq!(loaded.instance.gui_idle(&[0.0]), (None, false));
    assert_eq!(
        loaded.instance.take_param_changes(),
        [
            ParamChange::Value {
                index: 0,
                value: 0.5
            },
            ParamChange::Value {
                index: 0,
                value: 5.0
            },
        ]
    );
    loaded.instance.gui_close();
    assert!(loaded.instance.state_dirty());
    let state = loaded
        .instance
        .save_state(&StatePaths::default())
        .unwrap()
        .expect("it saves state");
    assert!(!loaded.instance.state_dirty());

    // Notes go through the worker too, and the plugin then says its state
    // changed (state:StateChanged), UI or not.
    run(&mut loaded.processor, 0.0, 32, &[note_on(0, 60)]);
    assert_eq!(run(&mut loaded.processor, 0.0, 32, &[]), [1.0; 32]);
    assert!(loaded.instance.state_dirty());
    loaded.instance.unload(loaded.processor.into_any());

    // Restoring schedules work as well, done before the first block.
    let mut restored = host
        .load_for(info, Some(&state), &StatePaths::default(), true)
        .unwrap();
    assert_eq!(run(&mut restored.processor, 0.0, 32, &[]), [0.5; 32]);
    restored.instance.unload(restored.processor.into_any());
}

#[test]
fn lv2_workers_run_on_a_thread_of_their_own() {
    let mut host = host();
    let info = plugin("lv2:urn:dawkey:test:worker");
    let mut loaded = host.load(info).unwrap();
    let mut out = run(&mut loaded.processor, 0.0, 32, &[note_on(0, 60)]);
    let started = std::time::Instant::now();
    while out != [1.0; 32] && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(1));
        out = run(&mut loaded.processor, 0.0, 32, &[]);
    }
    assert_eq!(out, [1.0; 32]);
    loaded.instance.unload(loaded.processor.into_any());
}

#[test]
fn lv2_uis_are_picked_x11_first_then_gtk_then_qt() {
    use crate::GuiKind;
    let mut host = host();
    for (id, kind) in [
        // An X11 UI, a Gtk 3 one and a Qt 6 one: the X11 one, embedded.
        ("lv2:urn:dawkey:test:worker", Some(GuiKind::Lv2X11)),
        // Only a Gtk one, or only a Qt one: in a window of its own.
        ("lv2:urn:dawkey:test:memo", Some(GuiKind::Lv2Own)),
        ("lv2:urn:dawkey:test:octave", Some(GuiKind::Lv2Own)),
        ("lv2:urn:dawkey:test:gain", None),
    ] {
        let mut loaded = host.load(plugin(id)).unwrap();
        assert_eq!(loaded.instance.gui_kind(), kind, "{id}");
        loaded.instance.unload(loaded.processor.into_any());
    }
}
