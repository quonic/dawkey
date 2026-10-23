// SPDX-License-Identifier: AGPL-3.0-or-later
//! Plugins from `dawkey-test-plugins` run in processes of their own (the
//! `dawkey-plugin-host` binary), shared by the instances of one plugin file.
//! The processor that stands in for them runs under `assert_no_alloc`.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use dawkey_engine::{
    MAX_PLUGIN_EVENTS, MidiSink, PluginEvent, PluginEventKind, PluginProcessor, ProcessContext,
    Transport,
};
use dawkey_plugins::{
    BridgeProgram, Catalog, Host, Loaded, PluginInfo, Sandbox, SearchPath, StatePaths, scan,
};

#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

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

fn plugin(id: &str) -> &'static PluginInfo {
    installed().2.get(id).unwrap()
}

/// A host that runs plugins in sandboxed `dawkey-plugin-host` processes (one
/// per plugin file), blocks of `block` frames.
fn host(block: u32) -> Host {
    host_sharing(block, true)
}

/// Like [`host`], with a process per instance unless `share`.
fn host_sharing(block: u32, share: bool) -> Host {
    let mut host = Host::new(48_000, installed().1.clone());
    host.set_bridge(Some(BridgeProgram {
        program: PathBuf::from(env!("CARGO_BIN_EXE_dawkey-plugin-host")),
        args: Vec::new(),
        share,
        sandbox: Some(Sandbox::default()),
    }));
    host.set_bridge_block(block);
    host
}

/// Waits (up to 5 s) until `loaded`'s process is seen to have stopped.
fn until_crashed(loaded: &mut Loaded) -> String {
    let until = Instant::now() + Duration::from_secs(5);
    while loaded.instance.crashed().is_none() && Instant::now() < until {
        loaded.instance.idle();
        std::thread::sleep(Duration::from_millis(10));
    }
    loaded.instance.crashed().expect("it noticed")
}

fn kill(pid: u32) {
    // SIGKILL, as a crash would end it.
    std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
}

/// Whether process `pid` is gone (reaped, or a zombie no one waited for).
fn exited(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
        stat.rsplit_once(") ")
            .is_some_and(|(_, rest)| rest.starts_with('Z'))
    })
}

/// One block of `input` through `p`: the left channel and the MIDI sent.
fn run(
    p: &mut Box<dyn PluginProcessor>,
    input: &[f32],
    events: &[PluginEvent],
) -> (Vec<f32>, Vec<PluginEvent>) {
    let mut left = input.to_vec();
    let mut right = input.to_vec();
    let mut sent = Vec::with_capacity(MAX_PLUGIN_EVENTS);
    assert_no_alloc::assert_no_alloc(|| {
        p.process(
            [&mut left, &mut right],
            ProcessContext::new(Transport::default()),
            events,
            MidiSink::new(&mut sent),
        );
    });
    assert_eq!(left, right);
    (left, sent)
}

/// Like [`run`], then gives the plugin's process time to do the block (as
/// the next audio cycle would).
fn run_and_wait(
    p: &mut Box<dyn PluginProcessor>,
    input: &[f32],
    events: &[PluginEvent],
) -> (Vec<f32>, Vec<PluginEvent>) {
    let out = run(p, input, events);
    std::thread::sleep(Duration::from_millis(30));
    out
}

fn param(index: u32, value: f64) -> PluginEvent {
    PluginEvent {
        offset: 0,
        kind: PluginEventKind::Param { index, value },
    }
}

fn unload(loaded: Loaded) {
    loaded.instance.unload(loaded.processor.into_any());
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
fn a_bridged_plugin_sounds_the_same_one_block_late() {
    let mut host = host(64);
    for id in [
        "clap:dawkey.test.gain",
        "lv2:urn:dawkey:test:gain",
        &vst3("gain"),
    ] {
        let info = plugin(id);
        let mut loaded = host.load(info).unwrap();
        assert_eq!(loaded.latency, 64, "{id}");
        assert!(loaded.instance.process_id().is_some());
        assert!(loaded.instance.sandboxed(), "{id}");
        let ramp: Vec<f32> = (0..64).map(|i| i as f32 / 64.0).collect();
        // The first block comes out as silence; then each block, a block
        // late, at -6 dB.
        let (first, _) = run_and_wait(
            &mut loaded.processor,
            &ramp,
            &[param(0, gain_value(id, -6.0))],
        );
        assert!(first.iter().all(|&s| s == 0.0), "{id}: {first:?}");
        let (second, _) = run_and_wait(&mut loaded.processor, &[0.5; 64], &[]);
        let gain = 10f32.powf(-6.0 / 20.0);
        for (out, input) in second.iter().zip(&ramp) {
            assert!((out - input * gain).abs() < 1e-6, "{id}: {out} vs {input}");
        }
        // Asked of the plugin in its process.
        let text = loaded
            .instance
            .param_text(info, 0, gain_value(id, -6.0))
            .unwrap();
        assert!(
            text.starts_with("-6") && text.ends_with("dB"),
            "{id}: {text}"
        );
        let value = loaded.instance.param_value(info, 0, "-12 dB").unwrap();
        assert!(
            (value - gain_value(id, -12.0)).abs() < 1e-9,
            "{id}: {value}"
        );
        unload(loaded);
    }
}

#[test]
fn offline_renders_wait_for_each_block() {
    let mut host = host(64);
    let info = plugin("clap:dawkey.test.gain");
    let mut loaded = host
        .load_for(info, None, &StatePaths::default(), true)
        .unwrap();
    assert_eq!(loaded.latency, 0);
    let (out, _) = run(&mut loaded.processor, &[0.5; 64], &[param(0, -6.0)]);
    let expected = 0.5 * 10f32.powf(-6.0 / 20.0);
    assert!(out.iter().all(|&s| (s - expected).abs() < 1e-6), "{out:?}");
    unload(loaded);
}

#[test]
fn offline_lv2_workers_work_as_asked_in_the_plugin_process() {
    let mut host = host(64);
    let info = plugin("lv2:urn:dawkey:test:worker");
    let mut loaded = host
        .load_for(info, None, &StatePaths::default(), true)
        .unwrap();
    let note = PluginEvent {
        offset: 0,
        kind: PluginEventKind::NoteOn {
            channel: 0,
            key: 60,
            velocity: 1.0,
        },
    };
    // The note schedules work, whose response sets the level for the next
    // block: exactly, since the plugin process works as it's asked.
    let (out, _) = run(&mut loaded.processor, &[0.0; 64], &[note]);
    assert_eq!(out, [0.0; 64]);
    let (out, _) = run(&mut loaded.processor, &[0.0; 64], &[]);
    assert_eq!(out, [1.0; 64]);
    // And it said its state changed.
    let started = Instant::now();
    while !loaded.instance.state_dirty() && started.elapsed() < Duration::from_secs(5) {
        loaded.instance.idle();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(loaded.instance.state_dirty());
    unload(loaded);
}

#[test]
fn notes_go_in_and_midi_comes_out_a_block_late() {
    let mut host = host(64);
    let info = plugin("clap:dawkey.test.octave");
    let mut loaded = host.load(info).unwrap();
    let note = PluginEvent {
        offset: 5,
        kind: PluginEventKind::NoteOn {
            channel: 0,
            key: 60,
            velocity: 1.0,
        },
    };
    let (_, first) = run_and_wait(&mut loaded.processor, &[0.0; 64], &[note]);
    assert!(first.is_empty());
    let (_, second) = run_and_wait(&mut loaded.processor, &[0.0; 64], &[]);
    assert_eq!(
        second,
        [PluginEvent {
            offset: 5,
            kind: PluginEventKind::NoteOn {
                channel: 0,
                key: 72,
                velocity: 1.0
            }
        }]
    );
    unload(loaded);
}

#[test]
fn state_goes_through_the_bridge() {
    let mut host = host(64);
    for id in [
        "clap:dawkey.test.memo",
        "lv2:urn:dawkey:test:memo",
        &vst3("memo"),
    ] {
        let info = plugin(id);
        let mut loaded = host.load(info).unwrap();
        let note = PluginEvent {
            offset: 0,
            kind: PluginEventKind::NoteOn {
                channel: 0,
                key: 60,
                velocity: 1.0,
            },
        };
        run_and_wait(&mut loaded.processor, &[0.0; 64], &[note]);
        if !id.starts_with("lv2:") {
            // It says its state changed (a notice from its process).
            let until = Instant::now() + Duration::from_secs(5);
            while !loaded.instance.state_dirty() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(loaded.instance.state_dirty(), "{id}");
        }
        let state = loaded
            .instance
            .save_state(&StatePaths::default())
            .unwrap()
            .expect("it saves state");
        assert!(!loaded.instance.state_dirty());
        unload(loaded);

        let mut restored = host
            .load_with_state(info, Some(&state), &StatePaths::default())
            .unwrap();
        run_and_wait(&mut restored.processor, &[0.0; 64], &[]);
        let (out, _) = run_and_wait(&mut restored.processor, &[0.0; 64], &[]);
        assert!(out.iter().all(|&s| s == 1.0), "{id}: {out:?}");
        unload(restored);
    }
}

#[test]
fn a_crash_takes_only_the_plugin_down() {
    let mut host = host(64);
    let info = plugin("clap:dawkey.test.gain");
    let mut loaded = host.load(info).unwrap();
    assert_eq!(loaded.instance.crashed(), None);
    kill(loaded.instance.process_id().unwrap());
    let why = until_crashed(&mut loaded);
    assert!(why.contains("crashed"), "{why}");
    // The track's audio passes through.
    let (out, _) = run(&mut loaded.processor, &[0.25; 64], &[param(0, -60.0)]);
    assert!(out.iter().all(|&s| s == 0.25), "{out:?}");
    assert!(
        loaded.instance.param_text(info, 0, 0.0).is_some(),
        "falls back"
    );
    unload(loaded);
}

#[test]
fn instances_of_one_plugin_file_share_a_process() {
    let mut host = host(64);
    // The CLAP test plugins are one file; the LV2 ones another.
    let mut gain = host.load(plugin("clap:dawkey.test.gain")).unwrap();
    let mut gain2 = host.load(plugin("clap:dawkey.test.gain")).unwrap();
    let mut octave = host.load(plugin("clap:dawkey.test.octave")).unwrap();
    let mut lv2 = host.load(plugin("lv2:urn:dawkey:test:gain")).unwrap();
    let pid = gain.instance.process_id().unwrap();
    assert_eq!(gain2.instance.process_id(), Some(pid));
    assert_eq!(octave.instance.process_id(), Some(pid));
    let lv2_pid = lv2.instance.process_id().unwrap();
    assert_ne!(lv2_pid, pid);

    // Each instance runs on its own: its own parameters, its own audio.
    run(&mut gain.processor, &[0.5; 64], &[param(0, -6.0)]);
    run(&mut gain2.processor, &[0.5; 64], &[param(0, -12.0)]);
    std::thread::sleep(Duration::from_millis(30));
    let (out, _) = run_and_wait(&mut gain.processor, &[0.0; 64], &[]);
    let (out2, _) = run_and_wait(&mut gain2.processor, &[0.0; 64], &[]);
    let db = |s: f32| 20.0 * (s / 0.5).log10();
    assert!((db(out[0]) + 6.0).abs() < 1e-3, "{}", db(out[0]));
    assert!((db(out2[0]) + 12.0).abs() < 1e-3, "{}", db(out2[0]));
    // Asked of the right instance.
    assert_eq!(
        gain2
            .instance
            .param_value(plugin("clap:dawkey.test.gain"), 0, "-3 dB"),
        Some(-3.0)
    );

    // One unloaded, the others run on in the process.
    unload(gain2);
    run_and_wait(&mut gain.processor, &[0.5; 64], &[]);
    let (out, _) = run_and_wait(&mut gain.processor, &[0.0; 64], &[]);
    assert!((db(out[0]) + 6.0).abs() < 1e-3, "{}", db(out[0]));
    assert_eq!(gain.instance.crashed(), None);

    // A crash takes down every instance in the process, and only those.
    kill(pid);
    until_crashed(&mut gain);
    until_crashed(&mut octave);
    lv2.instance.idle();
    assert_eq!(lv2.instance.crashed(), None);
    let (out, _) = run_and_wait(&mut lv2.processor, &[0.5; 64], &[param(0, 0.0)]);
    assert_eq!(out.len(), 64);
    // A new instance of the plugin gets a new process.
    let again = host.load(plugin("clap:dawkey.test.gain")).unwrap();
    assert_ne!(again.instance.process_id(), Some(pid));
    unload(again);
    unload(gain);
    unload(octave);

    // The process exits with its last instance.
    unload(lv2);
    assert!(exited(lv2_pid), "process {lv2_pid} is still running");
}

#[test]
fn instances_of_one_vst3_bundle_share_a_process() {
    let mut host = host(64);
    let gain = host.load(plugin(&vst3("gain"))).unwrap();
    let memo = host.load(plugin(&vst3("memo"))).unwrap();
    let clap = host.load(plugin("clap:dawkey.test.gain")).unwrap();
    let pid = gain.instance.process_id().unwrap();
    assert_eq!(memo.instance.process_id(), Some(pid));
    // The same library, but another plugin file: a process of its own.
    assert_ne!(clap.instance.process_id(), Some(pid));
    assert!(gain.instance.sandboxed());
    // A crash takes down the bundle's instances.
    let (mut gain, mut memo) = (gain, memo);
    kill(pid);
    until_crashed(&mut gain);
    until_crashed(&mut memo);
    let (out, _) = run(&mut gain.processor, &[0.25; 64], &[]);
    assert!(out.iter().all(|&s| s == 0.25), "{out:?}");
    unload(gain);
    unload(memo);
    unload(clap);
}

#[test]
fn each_instance_can_have_a_process_of_its_own() {
    let mut host = host_sharing(64, false);
    let one = host.load(plugin("clap:dawkey.test.gain")).unwrap();
    let two = host.load(plugin("clap:dawkey.test.gain")).unwrap();
    assert_ne!(one.instance.process_id(), two.instance.process_id());
    unload(one);
    unload(two);
}

#[test]
fn a_busy_plugin_is_waited_for_not_killed() {
    let mut host = host(64);
    let info = plugin("clap:dawkey.test.gain");
    let mut loaded = host.load(info).unwrap();
    // Its main thread is held up (as by a GUI's dialog): the question gets
    // Dawkey's own answer after a short wait, and the plugin lives on.
    let started = Instant::now();
    let text = loaded
        .instance
        .param_text(info, 0, dawkey_test_plugins::STALL_DB);
    assert_eq!(text, Some(info.params[0].text(dawkey_test_plugins::STALL_DB)));
    let waited = started.elapsed();
    assert!(waited < Duration::from_secs(3), "{waited:?}");
    assert_eq!(loaded.instance.crashed(), None);
    // While it's busy, it isn't asked (no wait), and its audio goes on.
    let started = Instant::now();
    assert_eq!(loaded.instance.param_value(info, 0, "-6"), Some(-6.0));
    assert!(started.elapsed() < Duration::from_millis(500));
    run_and_wait(&mut loaded.processor, &[0.5; 64], &[param(0, -6.0)]);
    let (out, _) = run_and_wait(&mut loaded.processor, &[0.5; 64], &[]);
    assert!(
        (out[0] - 0.5 * 10f32.powf(-6.0 / 20.0)).abs() < 1e-6,
        "{}",
        out[0]
    );
    // Once it's done, the late answer is passed over and it answers again.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(loaded.instance.param_value(info, 0, "-12 dB"), Some(-12.0));
    let text = loaded.instance.param_text(info, 0, -3.0).unwrap();
    assert_eq!(text, "-3.0 dB");
    unload(loaded);
}

#[test]
fn a_longer_block_grows_the_delay() {
    let mut host = host(64);
    let mut loaded = host.load(plugin("clap:dawkey.test.gain")).unwrap();
    assert_eq!(loaded.instance.latency(), Some(64));
    run_and_wait(&mut loaded.processor, &[0.5; 128], &[]);
    assert_eq!(loaded.instance.latency(), Some(128));
    // Its output: what came in, now 128 frames late.
    let (out, _) = run_and_wait(&mut loaded.processor, &[0.25; 128], &[]);
    assert!(out.iter().all(|&s| s == 0.5), "{out:?}");
    unload(loaded);
}

#[test]
fn plugins_that_fail_to_load_say_why() {
    let mut host = host(64);
    let mut info = plugin("clap:dawkey.test.gain").clone();
    info.path = PathBuf::from("/nonexistent/plugin.clap");
    let err = host.load(&info).unwrap_err();
    assert!(!err.0.is_empty());
}

#[test]
fn parameters_the_plugin_changes_come_back_through_the_bridge() {
    for id in ["clap:dawkey.test.beep", &vst3("beep")] {
        changes_come_back(id);
    }
}

fn changes_come_back(id: &str) {
    use dawkey_plugins::ParamChange;
    let mut host = host(64);
    let mut loaded = host.load(plugin(id)).unwrap();
    let note = PluginEvent {
        offset: 0,
        kind: PluginEventKind::NoteOn {
            channel: 0,
            key: 127,
            velocity: 1.0,
        },
    };
    run_and_wait(&mut loaded.processor, &[0.0; 64], &[note]);
    let until = Instant::now() + Duration::from_secs(5);
    let mut changes = Vec::new();
    while changes.len() < 3 && Instant::now() < until {
        changes.extend(loaded.instance.take_param_changes());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        changes,
        [
            ParamChange::Begin { index: 0 },
            ParamChange::Value {
                index: 0,
                value: 0.5
            },
            ParamChange::End { index: 0 },
        ]
    );
    unload(loaded);
}

#[test]
fn plugins_without_a_gui_say_so() {
    let mut host = host(64);
    let mut loaded = host.load(plugin("clap:dawkey.test.gain")).unwrap();
    assert!(!loaded.instance.has_gui());
    let err = loaded.instance.show_gui(true).unwrap_err();
    assert!(err.0.contains("no GUI"), "{}", err.0);
    unload(loaded);
    // In-process plugins don't show GUIs at all.
    let mut in_process = Host::new(48_000, installed().1.clone());
    let mut loaded = in_process.load(plugin("clap:dawkey.test.gain")).unwrap();
    assert!(!loaded.instance.has_gui());
    let err = loaded.instance.show_gui(true).unwrap_err();
    assert!(err.0.contains("processes of their own"), "{}", err.0);
    unload(loaded);
}

#[test]
fn a_qt_ui_that_cant_open_says_why_and_the_plugin_runs_on() {
    let mut host = host(64);
    let info = plugin("lv2:urn:dawkey:test:octave");
    let mut loaded = host
        .load_for(info, None, &StatePaths::default(), true)
        .unwrap();
    // It says it has a Qt 5 UI, which its library doesn't have (and which
    // needs a display besides): that's said, and nothing else happens.
    assert!(loaded.instance.has_gui());
    let err = loaded.instance.show_gui(true).unwrap_err();
    assert!(err.0.contains("opening its UI failed"), "{}", err.0);
    assert!(!loaded.instance.gui_visible());
    assert_eq!(loaded.instance.crashed(), None);
    unload(loaded);
}

#[test]
fn a_gtk_ui_that_cant_open_says_why_and_the_plugin_runs_on() {
    let mut host = host(64);
    let mut loaded = host
        .load_for(
            plugin("lv2:urn:dawkey:test:memo"),
            None,
            &StatePaths::default(),
            true,
        )
        .unwrap();
    // It says it has a Gtk UI, which its library doesn't have (and which
    // needs a Gtk and a display besides).
    assert!(loaded.instance.has_gui());
    let err = loaded.instance.show_gui(true).unwrap_err();
    assert!(err.0.contains("opening its UI failed"), "{}", err.0);
    assert!(!loaded.instance.gui_visible());
    assert_eq!(loaded.instance.crashed(), None);
    let (out, _) = run(&mut loaded.processor, &[0.25; 64], &[]);
    assert_eq!(out, [0.25; 64]);
    unload(loaded);
}
