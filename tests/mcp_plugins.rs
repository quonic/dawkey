// SPDX-License-Identifier: AGPL-3.0-or-later
//! Plugins over MCP, with the tiny plugins from `dawkey-test-plugins` (CLAP and
//! LV2), scanned by `dawkey scan-plugin` subprocesses. Nothing on the
//! system's plugin paths is used.

use std::path::{Path, PathBuf};

use dawkey_testkit::Harness;
use serde_json::{Value, json};

const DAWKEY: &str = env!("CARGO_BIN_EXE_dawkey");

/// -6.0206 dB halves the level.
const HALF_DB: f64 = -6.020_599_913_279_624;

struct Setup {
    _dir: tempfile::TempDir,
    clap: PathBuf,
    lv2: PathBuf,
    vst3: PathBuf,
    wav: PathBuf,
}

/// Installs the test plugins, and writes a second of constant 0.5 (mono).
fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let installed = dawkey_test_plugins::install(&dir.path().join("plugins")).unwrap();
    let wav = dir.path().join("half.wav");
    dawkey_media::write_wav(&wav, &[vec![0.5; 48_000]], 48_000).unwrap();
    Setup {
        clap: installed.clap_path,
        lv2: installed.lv2_path,
        vst3: installed.vst3_path,
        wav,
        _dir: dir,
    }
}

async fn start(setup: &Setup) -> Harness {
    start_with(&setup.clap, &setup.lv2, &setup.vst3).await
}

async fn start_with(clap: &Path, lv2: &Path, vst3: &Path) -> Harness {
    Harness::headless_with_env(
        DAWKEY,
        std::iter::empty::<&str>(),
        [("CLAP_PATH", clap), ("LV2_PATH", lv2), ("VST3_PATH", vst3)],
    )
    .await
    .expect("dawkey starts")
}

async fn peak(dawkey: &Harness, start_s: f64, end_s: f64) -> Option<f64> {
    dawkey
        .call(
            "render_range",
            json!({ "start_s": start_s, "end_s": end_s }),
        )
        .await
        .unwrap()["analysis"]["peak_dbfs"][0]
        .as_f64()
}

/// The ID of the VST3 test plugin called `name`.
fn vst3(name: &str) -> String {
    dawkey_test_plugins::vst3_id(name).unwrap()
}

fn approx(actual: Option<f64>, expected: f64) -> bool {
    actual.is_some_and(|a| (a - expected).abs() < 0.01)
}

/// dB of 0.5 panned to center with equal power.
const HALF_CENTER_DB: f64 = -9.0309;

/// Imports the test signal on a new track. Returns the track.
async fn signal_track(dawkey: &Harness, wav: &Path) -> Value {
    dawkey
        .call("import_audio", json!({ "path": wav }))
        .await
        .unwrap()["track"]
        .clone()
}

async fn add_plugin(dawkey: &Harness, track: &Value, plugin: &str) -> Value {
    dawkey
        .call("add_plugin", json!({ "track": track, "plugin": plugin }))
        .await
        .unwrap()["plugin"]
        .clone()
}

#[tokio::test]
async fn lists_the_installed_plugins() {
    let setup = setup();
    let dawkey = start(&setup).await;
    let listed = dawkey.call("list_plugins", json!({})).await.unwrap();
    let ids: Vec<&str> = listed["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "clap:dawkey.test.beep",
            "lv2:urn:dawkey:test:beep",
            &vst3("beep"),
            "clap:dawkey.test.delay",
            &vst3("delay"),
            "clap:dawkey.test.gain",
            "lv2:urn:dawkey:test:gain",
            &vst3("gain"),
            "clap:dawkey.test.memo",
            "lv2:urn:dawkey:test:memo",
            &vst3("memo"),
            "clap:dawkey.test.octave",
            "lv2:urn:dawkey:test:octave",
            &vst3("octave"),
            "clap:dawkey.test.split",
            "lv2:urn:dawkey:test:split",
            &vst3("split"),
            "clap:dawkey.test.subtract",
            "lv2:urn:dawkey:test:subtract",
            &vst3("subtract"),
            "clap:dawkey.test.tempo",
            "lv2:urn:dawkey:test:tempo",
            &vst3("tempo"),
            "lv2:urn:dawkey:test:worker",
        ],
        "sorted by name"
    );
    assert_eq!(listed["scan_failures"], 0);
    assert_eq!(listed["total"], 24);
    // Compact entries by default: no ports or parameter counts.
    assert_eq!(listed["plugins"][0]["kind"], "instrument");
    assert!(listed["plugins"][0].get("audio_inputs").is_none());
    assert!(listed.get("next").is_none());
    // Paged.
    let page = dawkey
        .call("list_plugins", json!({ "first": 20, "count": 3 }))
        .await
        .unwrap();
    assert_eq!(page["plugins"].as_array().unwrap().len(), 3);
    assert_eq!(page["plugins"][0]["id"], "clap:dawkey.test.tempo");
    assert_eq!(page["total"], 24);
    assert_eq!(page["next"], 23);
    let detailed = dawkey
        .call(
            "list_plugins",
            json!({ "query": "clap:dawkey.test.gain", "detail": true }),
        )
        .await
        .unwrap();
    assert_eq!(detailed["plugins"][0]["audio_inputs"], 2);
    assert_eq!(detailed["plugins"][0]["param_count"], 1);

    let instruments = dawkey
        .call(
            "list_plugins",
            json!({ "kind": "instrument", "params": true }),
        )
        .await
        .unwrap();
    let first = &instruments["plugins"][0];
    assert_eq!(first["instrument"], true);
    assert_eq!(first["params"][0]["name"], "Level");
    // The beeps, the splits, and the memos and the worker (effects that
    // take notes).
    assert_eq!(instruments["plugins"].as_array().unwrap().len(), 10);
    let effects = dawkey
        .call("list_plugins", json!({ "kind": "midi_effect" }))
        .await
        .unwrap();
    let effects = effects["plugins"].as_array().unwrap();
    assert_eq!(effects.len(), 3);
    assert!(effects.iter().all(|p| p["kind"] == "midi_effect"));
    let found = dawkey
        .call("list_plugins", json!({ "query": "LV2" }))
        .await
        .unwrap();
    assert_eq!(found["plugins"].as_array().unwrap().len(), 8);

    let found = dawkey
        .call("list_plugins", json!({ "query": "VST3" }))
        .await
        .unwrap();
    assert_eq!(found["plugins"].as_array().unwrap().len(), 8);
    assert_eq!(found["plugins"][0]["format"], "vst3");

    let rescanned = dawkey.call("rescan_plugins", json!({})).await.unwrap();
    assert_eq!(rescanned["found"], 24);
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_crashing_plugin_only_fails_its_own_scan() {
    let setup = setup();
    let dawkey = Harness::headless_with_env(
        DAWKEY,
        std::iter::empty::<&str>(),
        [
            ("CLAP_PATH", setup.clap.as_os_str()),
            ("LV2_PATH", setup.lv2.as_os_str()),
            ("VST3_PATH", setup.vst3.as_os_str()),
            ("dawkey_TEST_PLUGIN_CRASH", "1".as_ref()),
        ],
    )
    .await
    .unwrap();
    // The CLAP and VST3 scans crashed in their subprocesses; the LV2
    // plugins are listed.
    let listed = dawkey.call("list_plugins", json!({})).await.unwrap();
    assert_eq!(listed["scan_failures"], 2);
    let rescanned = dawkey.call("rescan_plugins", json!({})).await.unwrap();
    for failure in rescanned["failures"].as_array().unwrap() {
        let error = failure["error"].as_str().unwrap();
        assert!(error.contains("crashed"), "{error}");
    }
    assert_eq!(rescanned["found"], 8);
    // Still alive.
    dawkey.call("get_state", json!({})).await.unwrap();
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn plugins_that_print_while_scanned_are_found() {
    let setup = setup();
    let dawkey = Harness::headless_with_env(
        DAWKEY,
        std::iter::empty::<&str>(),
        [
            ("CLAP_PATH", setup.clap.as_os_str()),
            ("LV2_PATH", setup.lv2.as_os_str()),
            ("VST3_PATH", setup.vst3.as_os_str()),
            ("dawkey_TEST_PLUGIN_CHATTY", "1".as_ref()),
        ],
    )
    .await
    .unwrap();
    // They print on stdout as they're loaded and as the scanner exits,
    // which the scan reads its JSON from.
    let rescanned = dawkey.call("rescan_plugins", json!({})).await.unwrap();
    assert_eq!(rescanned["failures"], json!([]));
    assert_eq!(rescanned["found"], 24);
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn effects_process_tracks_and_take_parameters() {
    let setup = setup();
    let dawkey = start(&setup).await;
    for plugin in ["clap:dawkey.test.gain", "lv2:urn:dawkey:test:gain"] {
        dawkey
            .call("new_project", json!({ "discard_changes": true }))
            .await
            .unwrap();
        let track = signal_track(&dawkey, &setup.wav).await;
        let slot = add_plugin(&dawkey, &track, plugin).await;
        // At its default (0 dB) the gain changes nothing.
        assert!(
            approx(peak(&dawkey, 0.0, 1.0).await, HALF_CENTER_DB),
            "{plugin}"
        );

        let param = if plugin.starts_with("clap") {
            "1"
        } else {
            "gain"
        };
        dawkey
            .call(
                "set_plugin_param",
                json!({ "plugin": slot, "param": param, "value": HALF_DB }),
            )
            .await
            .unwrap();
        assert!(
            approx(peak(&dawkey, 0.0, 1.0).await, HALF_CENTER_DB + HALF_DB),
            "{plugin}"
        );
        let details = dawkey
            .call("get_plugin", json!({ "plugin": slot }))
            .await
            .unwrap();
        assert_eq!(details["params"][0]["value"], HALF_DB);
        // As the plugin shows it: CLAP plugins format their values, LV2
        // ports give a unit.
        let text = if plugin.starts_with("clap:") {
            "-6.0 dB"
        } else {
            "-6.02 dB"
        };
        assert_eq!(details["params"][0]["text"], text, "{plugin}");
        assert_eq!(details["params"][0]["min"], -60.0);
        assert_eq!(details["param_count"], 1);
        let paged = dawkey
            .call("get_plugin", json!({ "plugin": slot, "first": 1 }))
            .await
            .unwrap();
        assert_eq!(paged["first_param"], 1);
        assert_eq!(paged["params"], json!([]));
        assert!(details.get("error").is_none(), "{details}");

        // Bypass passes the signal through; undo brings the gain back.
        dawkey
            .call(
                "set_plugin_bypass",
                json!({ "plugin": slot, "bypass": true }),
            )
            .await
            .unwrap();
        assert!(
            approx(peak(&dawkey, 0.0, 1.0).await, HALF_CENTER_DB),
            "{plugin}"
        );
        dawkey.call("undo", json!({})).await.unwrap();
        assert!(approx(
            peak(&dawkey, 0.0, 1.0).await,
            HALF_CENTER_DB + HALF_DB
        ));

        // Bad input is a readable tool error.
        let err = dawkey
            .call_err(
                "set_plugin_param",
                json!({ "plugin": slot, "param": param, "value": 99.0 }),
            )
            .await
            .unwrap();
        assert!(err.contains("goes from -60 to 12"), "{err}");
        let err = dawkey
            .call_err(
                "set_plugin_param",
                json!({ "plugin": slot, "param": "nope", "value": 0.0 }),
            )
            .await
            .unwrap();
        assert!(err.contains("no parameter"), "{err}");

        // A value as text, read the way the plugin shows its values.
        dawkey
            .call(
                "set_plugin_param",
                json!({ "plugin": slot, "param": param, "text": "-12 dB" }),
            )
            .await
            .unwrap();
        let details = dawkey
            .call("get_plugin", json!({ "plugin": slot }))
            .await
            .unwrap();
        assert_eq!(details["params"][0]["value"], -12.0, "{plugin}");
        let err = dawkey
            .call_err(
                "set_plugin_param",
                json!({ "plugin": slot, "param": param, "text": "loud" }),
            )
            .await
            .unwrap();
        assert!(err.contains("can't read \"loud\""), "{err}");
        let err = dawkey
            .call_err(
                "set_plugin_param",
                json!({ "plugin": slot, "param": param, "text": "99 dB" }),
            )
            .await
            .unwrap();
        assert!(err.contains("goes from -60 to 12"), "{err}");
        dawkey.call("undo", json!({})).await.unwrap();

        // Removing it (one undo step) leaves the dry signal.
        dawkey
            .call("remove_plugin", json!({ "plugin": slot }))
            .await
            .unwrap();
        assert!(approx(peak(&dawkey, 0.0, 1.0).await, HALF_CENTER_DB));
        dawkey.call("undo", json!({})).await.unwrap();
        assert!(approx(
            peak(&dawkey, 0.0, 1.0).await,
            HALF_CENTER_DB + HALF_DB
        ));
    }
    let track = signal_track(&dawkey, &setup.wav).await;
    let err = dawkey
        .call_err(
            "add_plugin",
            json!({ "track": track, "plugin": "clap:not.there" }),
        )
        .await
        .unwrap();
    assert!(err.contains("not installed"), "{err}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn vst3_effects_process_tracks_and_take_parameters() {
    let setup = setup();
    let dawkey = start(&setup).await;
    let plugin = vst3("gain");
    let track = signal_track(&dawkey, &setup.wav).await;
    let slot = add_plugin(&dawkey, &track, &plugin).await;
    assert!(approx(peak(&dawkey, 0.0, 1.0).await, HALF_CENTER_DB));
    // VST3 parameters are normalized: this gain's 0 to 1 is -60 to +12 dB.
    let half = (HALF_DB + 60.0) / 72.0;
    dawkey
        .call(
            "set_plugin_param",
            json!({ "plugin": slot, "param": "1", "value": half }),
        )
        .await
        .unwrap();
    assert!(approx(
        peak(&dawkey, 0.0, 1.0).await,
        HALF_CENTER_DB + HALF_DB
    ));
    let details = dawkey
        .call("get_plugin", json!({ "plugin": slot }))
        .await
        .unwrap();
    assert_eq!(details["format"], "vst3");
    assert_eq!(details["params"][0]["value"], half);
    assert_eq!(details["params"][0]["text"], "-6.0 dB");
    assert_eq!(
        (&details["params"][0]["min"], &details["params"][0]["max"]),
        (&json!(0.0), &json!(1.0))
    );
    // Values as text are read by the plugin.
    dawkey
        .call(
            "set_plugin_param",
            json!({ "plugin": slot, "param": "1", "text": "-12 dB" }),
        )
        .await
        .unwrap();
    assert!(approx(peak(&dawkey, 0.0, 1.0).await, HALF_CENTER_DB - 12.0));
    let err = dawkey
        .call_err(
            "set_plugin_param",
            json!({ "plugin": slot, "param": "1", "value": 2.0 }),
        )
        .await
        .unwrap();
    assert!(err.contains("goes from 0 to 1"), "{err}");
    // Automation, like any parameter's.
    dawkey
        .call(
            "set_automation",
            json!({
                "track": track,
                "target": { "kind": "param", "plugin": slot, "param": "1" },
                "points": [{ "time_s": 0.5, "value": 60.0 / 72.0 }, { "time_s": 0.5001, "value": half }],
            }),
        )
        .await
        .unwrap();
    assert!(approx(peak(&dawkey, 0.0, 0.49).await, HALF_CENTER_DB));
    assert!(approx(
        peak(&dawkey, 0.51, 1.0).await,
        HALF_CENTER_DB + HALF_DB
    ));
    // Latency counts as for any plugin.
    let other = signal_track(&dawkey, &setup.wav).await;
    add_plugin(&dawkey, &other, &vst3("delay")).await;
    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    assert_eq!(status["plugin_latency_frames"], 64);
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn instrument_plugins_play_midi_tracks() {
    let setup = setup();
    let dawkey = start(&setup).await;
    for plugin in [
        "clap:dawkey.test.beep",
        "lv2:urn:dawkey:test:beep",
        &vst3("beep"),
    ] {
        dawkey
            .call("new_project", json!({ "discard_changes": true }))
            .await
            .unwrap();
        let clip = dawkey
            .call(
                "add_midi_clip",
                json!({ "notes": [{ "pitch": 60, "start_beats": 1, "length_beats": 1, "velocity": 127 }] }),
            )
            .await
            .unwrap();
        add_plugin(&dawkey, &clip["track"], plugin).await;
        // Beat 1 is 0.5 s to 1 s. The beep outputs full velocity (DC 1.0),
        // through a stereo (balance) pan: 0 dBFS.
        assert_eq!(peak(&dawkey, 0.0, 0.5).await, None, "{plugin}");
        assert!(approx(peak(&dawkey, 0.5, 1.0).await, 0.0), "{plugin}");
        assert_eq!(peak(&dawkey, 1.0, 1.5).await, None, "{plugin}");
    }
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn plugins_keep_their_own_state_in_the_project() {
    let setup = setup();
    let dawkey = start(&setup).await;
    for plugin in [
        "clap:dawkey.test.memo",
        "lv2:urn:dawkey:test:memo",
        &vst3("memo"),
    ] {
        dawkey
            .call("new_project", json!({ "discard_changes": true }))
            .await
            .unwrap();
        let clip = dawkey
            .call(
                "add_midi_clip",
                json!({ "notes": [{ "pitch": 60, "start_beats": 0, "length_beats": 1, "velocity": 127 }] }),
            )
            .await
            .unwrap();
        let slot = add_plugin(&dawkey, &clip["track"], plugin).await;
        // Playing the note: the memo remembers full velocity, only in its
        // own state (it has no parameters).
        dawkey.call("play", json!({})).await.unwrap();
        dawkey
            .call("advance_clock", json!({ "seconds": 0.25 }))
            .await
            .unwrap();
        dawkey.call("stop", json!({})).await.unwrap();
        dawkey
            .call("remove_clip", json!({ "clip": clip["clip"] }))
            .await
            .unwrap();
        // Renders (fresh instances) start where the running one is: DC 1.0.
        assert!(approx(peak(&dawkey, 0.0, 0.1).await, 0.0), "{plugin}");
        let details = dawkey
            .call("get_plugin", json!({ "plugin": slot }))
            .await
            .unwrap();
        assert!(details["state_bytes"].as_u64().unwrap() > 0, "{details}");
        let state = dawkey.call("get_state", json!({})).await.unwrap();
        assert!(state["project"].get("plugin_states").is_none());
        assert!(
            state["plugin_state_bytes"][slot.to_string()]
                .as_u64()
                .unwrap()
                > 0
        );

        // Undoing a removal brings the plugin back as it was.
        dawkey
            .call("remove_plugin", json!({ "plugin": slot }))
            .await
            .unwrap();
        assert_eq!(peak(&dawkey, 0.0, 0.1).await, None, "{plugin}");
        dawkey.call("undo", json!({})).await.unwrap();
        assert!(approx(peak(&dawkey, 0.0, 0.1).await, 0.0), "{plugin}");

        // And the project file keeps it.
        let path = setup.wav.with_file_name("Memo");
        let saved = dawkey
            .call("save_project", json!({ "path": path }))
            .await
            .unwrap();
        dawkey
            .call("new_project", json!({ "discard_changes": true }))
            .await
            .unwrap();
        let opened = dawkey
            .call("open_project", json!({ "path": saved["path"] }))
            .await
            .unwrap();
        assert_eq!(opened["dirty"], false);
        assert!(approx(peak(&dawkey, 0.0, 0.1).await, 0.0), "{plugin}");
        if plugin.starts_with("lv2:") {
            // The LV2 memo keeps its memo in a file in the project, which
            // goes where the project goes.
            let bundle = std::path::PathBuf::from(saved["path"].as_str().unwrap());
            let file = bundle.join(format!("plugins/{slot}/memo.txt"));
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "1");
            let moved = bundle.with_file_name("Moved.dawkey");
            dawkey
                .call("new_project", json!({ "discard_changes": true }))
                .await
                .unwrap();
            std::fs::rename(&bundle, &moved).unwrap();
            dawkey
                .call("open_project", json!({ "path": moved }))
                .await
                .unwrap();
            assert!(approx(peak(&dawkey, 0.0, 0.1).await, 0.0), "{plugin}");
        }
    }
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn sandboxed_plugins_keep_their_files_in_the_project() {
    let setup = setup();
    // The project goes where a sandboxed plugin can't write (not in /tmp or
    // the cache): its files are copied there when saving.
    let projects = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    for (flag, sandboxed) in [(None, true), (Some("--no-sandbox"), false)] {
        let args = ["--bridge-plugins"].into_iter().chain(flag);
        let dawkey = Harness::headless_with_env(
            DAWKEY,
            args,
            [
                ("CLAP_PATH", &setup.clap),
                ("LV2_PATH", &setup.lv2),
                ("VST3_PATH", &setup.vst3),
            ],
        )
        .await
        .expect("dawkey starts");
        let clip = dawkey
            .call(
                "add_midi_clip",
                json!({ "notes": [{ "pitch": 60, "start_beats": 0, "length_beats": 1, "velocity": 127 }] }),
            )
            .await
            .unwrap();
        let slot = add_plugin(&dawkey, &clip["track"], "lv2:urn:dawkey:test:memo").await;
        let details = dawkey
            .call("get_plugin", json!({ "plugin": slot }))
            .await
            .unwrap();
        assert_eq!(
            details["sandboxed"].as_bool().unwrap_or(false),
            sandboxed,
            "{details}"
        );
        dawkey.call("play", json!({})).await.unwrap();
        dawkey
            .call("advance_clock", json!({ "seconds": 0.25 }))
            .await
            .unwrap();
        dawkey.call("stop", json!({})).await.unwrap();
        dawkey
            .call("remove_clip", json!({ "clip": clip["clip"] }))
            .await
            .unwrap();

        let path = projects.path().join(format!("Memo {sandboxed}"));
        let saved = dawkey
            .call("save_project", json!({ "path": path }))
            .await
            .unwrap();
        let bundle = std::path::PathBuf::from(saved["path"].as_str().unwrap());
        let file = bundle.join(format!("plugins/{slot}/memo.txt"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "1", "{flag:?}");
        // Opened again, the plugin finds its file in the project.
        dawkey
            .call("new_project", json!({ "discard_changes": true }))
            .await
            .unwrap();
        dawkey
            .call("open_project", json!({ "path": bundle }))
            .await
            .unwrap();
        assert!(approx(peak(&dawkey, 0.0, 0.1).await, 0.0), "{flag:?}");
        dawkey.shutdown().await.unwrap();
    }
}

/// Peaks of both channels of a render, in dBFS.
async fn peaks(dawkey: &Harness, start_s: f64, end_s: f64) -> [Option<f64>; 2] {
    let rendered = dawkey
        .call(
            "render_range",
            json!({ "start_s": start_s, "end_s": end_s }),
        )
        .await
        .unwrap();
    [0, 1].map(|c| rendered["analysis"]["peak_dbfs"][c].as_f64())
}

#[tokio::test]
async fn plugins_follow_the_transport() {
    let setup = setup();
    let dawkey = start(&setup).await;
    // dB of a value through a mono track's equal-power center pan.
    let db = |v: f64| 20.0 * (v * std::f64::consts::FRAC_1_SQRT_2).log10();
    for plugin in [
        "clap:dawkey.test.tempo",
        "lv2:urn:dawkey:test:tempo",
        &vst3("tempo"),
    ] {
        dawkey
            .call("new_project", json!({ "discard_changes": true }))
            .await
            .unwrap();
        let track = dawkey.call("add_track", json!({})).await.unwrap()["track"].clone();
        add_plugin(&dawkey, &track, plugin).await;
        // 90 BPM in 3/4: a bar is 2 s. From bar 5 (beat 12, 8 s) on, 150 BPM.
        for (tool, args) in [
            ("set_tempo", json!({ "bpm": 90 })),
            (
                "set_time_signature",
                json!({ "numerator": 3, "denominator": 4 }),
            ),
            ("set_tempo", json!({ "bpm": 150, "at_beats": 12 })),
        ] {
            dawkey.call(tool, args).await.unwrap();
        }
        // The plugin shows tempo / 1000 on the left, (bar + 1) / 100 on the
        // right: the third bar (index 2) is 4 s to 6 s.
        let [left, right] = peaks(&dawkey, 4.1, 4.2).await;
        assert!(approx(left, db(0.09)), "{plugin}: {left:?}");
        assert!(approx(right, db(0.03)), "{plugin}: {right:?}");
        // At 150 BPM a 3/4 bar is 1.2 s: bar index 4 is 8 s to 9.2 s, bar 5
        // 9.2 s to 10.4 s.
        let [left, right] = peaks(&dawkey, 9.3, 9.4).await;
        assert!(approx(left, db(0.15)), "{plugin}: {left:?}");
        assert!(approx(right, db(0.06)), "{plugin}: {right:?}");
    }
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn buses_and_the_master_take_plugins() {
    let setup = setup();
    let dawkey = start(&setup).await;
    let track = signal_track(&dawkey, &setup.wav).await;
    let bus = dawkey
        .call("add_track", json!({ "kind": "bus" }))
        .await
        .unwrap()["track"]
        .clone();
    dawkey
        .call("set_track_output", json!({ "track": track, "bus": bus }))
        .await
        .unwrap();
    // Through the bus (stereo: no pan law of its own), unchanged.
    assert!(approx(peak(&dawkey, 0.0, 0.5).await, HALF_CENTER_DB));
    for (on, plugin) in [
        (bus.clone(), "clap:dawkey.test.gain"),
        (json!(0), "lv2:urn:dawkey:test:gain"),
    ] {
        let slot = add_plugin(&dawkey, &on, plugin).await;
        let param = if plugin.starts_with("clap:") {
            "1"
        } else {
            "gain"
        };
        dawkey
            .call(
                "set_plugin_param",
                json!({ "plugin": slot, "param": param, "value": HALF_DB }),
            )
            .await
            .unwrap();
    }
    // Halved on the bus, and again on the master.
    assert!(approx(
        peak(&dawkey, 0.0, 0.5).await,
        HALF_CENTER_DB + 2.0 * HALF_DB
    ));
    // The master's fader.
    dawkey
        .call("set_track_gain", json!({ "track": 0, "gain_db": 6.0 }))
        .await
        .unwrap();
    assert!(approx(
        peak(&dawkey, 0.0, 0.5).await,
        HALF_CENTER_DB + 2.0 * HALF_DB + 6.0
    ));
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["master"]["id"], 0);
    assert_eq!(
        state["project"]["master"]["plugins"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(state["project"]["tracks"][0]["output"], bus);
    let err = dawkey
        .call_err("set_track_output", json!({ "track": bus, "bus": bus }))
        .await
        .unwrap();
    assert!(err.contains("loop"), "{err}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn sidechains_take_another_tracks_signal() {
    let setup = setup();
    let dawkey = start(&setup).await;
    let db = |v: f64| 20.0 * v.log10();
    for plugin in [
        "clap:dawkey.test.subtract",
        "lv2:urn:dawkey:test:subtract",
        &vst3("subtract"),
    ] {
        dawkey
            .call("new_project", json!({ "discard_changes": true }))
            .await
            .unwrap();
        let kick = signal_track(&dawkey, &setup.wav).await;
        let bass = signal_track(&dawkey, &setup.wav).await;
        let slot = add_plugin(&dawkey, &bass, plugin).await;
        dawkey
            .call(
                "set_plugin_sidechain",
                json!({ "plugin": slot, "source": kick }),
            )
            .await
            .unwrap();
        // The bass less the kick after its fader and (equal-power) pan.
        let half = std::f64::consts::FRAC_1_SQRT_2;
        let kick_out = 0.5 * half;
        let bass_out = (0.5 - kick_out) * half;
        assert!(
            approx(peak(&dawkey, 0.0, 0.5).await, db(kick_out + bass_out)),
            "{plugin}"
        );
        // A muted kick still feeds it.
        dawkey
            .call("set_track_mute", json!({ "track": kick, "mute": true }))
            .await
            .unwrap();
        assert!(
            approx(peak(&dawkey, 0.0, 0.5).await, db(bass_out)),
            "{plugin}"
        );
        let details = dawkey
            .call("get_plugin", json!({ "plugin": slot }))
            .await
            .unwrap();
        assert_eq!(details["has_sidechain"], true);
        assert_eq!(details["sidechain"], kick);
        // No loops: the kick can't take the bass's plugin's signal back.
        let gain = add_plugin(&dawkey, &kick, "clap:dawkey.test.subtract").await;
        let err = dawkey
            .call_err(
                "set_plugin_sidechain",
                json!({ "plugin": gain, "source": bass }),
            )
            .await
            .unwrap();
        assert!(err.contains("loop"), "{err}");
        // Plugins without a sidechain input say so.
        let other = add_plugin(&dawkey, &kick, "clap:dawkey.test.gain").await;
        let err = dawkey
            .call_err(
                "set_plugin_sidechain",
                json!({ "plugin": other, "source": bass }),
            )
            .await
            .unwrap();
        assert!(err.contains("no sidechain"), "{err}");
    }
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn multi_out_instruments_feed_buses() {
    let setup = setup();
    let dawkey = start(&setup).await;
    for plugin in [
        "clap:dawkey.test.split",
        "lv2:urn:dawkey:test:split",
        &vst3("split"),
    ] {
        dawkey
            .call("new_project", json!({ "discard_changes": true }))
            .await
            .unwrap();
        let clip = dawkey
            .call(
                "add_midi_clip",
                json!({ "notes": [
                    { "pitch": 40, "start_beats": 0, "length_beats": 2, "velocity": 127 },
                    { "pitch": 72, "start_beats": 0, "length_beats": 2, "velocity": 127 },
                ] }),
            )
            .await
            .unwrap();
        let slot = add_plugin(&dawkey, &clip["track"], plugin).await;
        // Only the low key comes out of the main output: DC 1.0.
        assert!(approx(peak(&dawkey, 0.1, 0.5).await, 0.0), "{plugin}");
        let details = dawkey
            .call("get_plugin", json!({ "plugin": slot }))
            .await
            .unwrap();
        assert_eq!(details["outputs"][0]["name"], "High", "{details}");
        let bus = dawkey
            .call("add_track", json!({ "kind": "bus" }))
            .await
            .unwrap()["track"]
            .clone();
        dawkey
            .call(
                "set_plugin_output",
                json!({ "plugin": slot, "output": 0, "bus": bus }),
            )
            .await
            .unwrap();
        // The high key through the bus too: 2.0, +6 dB.
        assert!(approx(peak(&dawkey, 0.1, 0.5).await, 6.0206), "{plugin}");
        dawkey
            .call("set_track_mute", json!({ "track": bus, "mute": true }))
            .await
            .unwrap();
        assert!(approx(peak(&dawkey, 0.1, 0.5).await, 0.0), "{plugin}");
        let err = dawkey
            .call_err(
                "set_plugin_output",
                json!({ "plugin": slot, "output": 1, "bus": bus }),
            )
            .await
            .unwrap();
        assert!(err.contains("no output 1"), "{err}");
    }
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn midi_effects_feed_instruments_and_devices() {
    let setup = setup();
    let dawkey = start(&setup).await;
    for octave in [
        "clap:dawkey.test.octave",
        "lv2:urn:dawkey:test:octave",
        &vst3("octave"),
    ] {
        dawkey
            .call("new_project", json!({ "discard_changes": true }))
            .await
            .unwrap();
        let clip = dawkey
            .call(
                "add_midi_clip",
                json!({ "notes": [{ "pitch": 60, "start_beats": 1, "length_beats": 1, "velocity": 127 }] }),
            )
            .await
            .unwrap();
        let track = &clip["track"];
        // The effect's notes play the instrument after it.
        add_plugin(&dawkey, track, octave).await;
        add_plugin(&dawkey, track, "clap:dawkey.test.beep").await;
        assert_eq!(peak(&dawkey, 0.0, 0.5).await, None, "{octave}");
        assert!(approx(peak(&dawkey, 0.5, 1.0).await, 0.0), "{octave}");
        assert_eq!(peak(&dawkey, 1.0, 1.5).await, None, "{octave}");

        // With a device, the notes the effect sends go there instead of to
        // the instrument.
        dawkey
            .call(
                "set_track_midi_output",
                json!({ "track": track, "device": "Synth" }),
            )
            .await
            .unwrap();
        dawkey.call("play", json!({})).await.unwrap();
        let out = dawkey
            .call("advance_clock", json!({ "seconds": 1.5 }))
            .await
            .unwrap();
        dawkey.call("stop", json!({})).await.unwrap();
        assert_eq!(out["output"]["silent"], true, "{octave}: {out}");
        assert_eq!(
            out["midi_output"],
            json!([
                { "at_s": 0.5, "device": "Synth", "data": [0x90, 72, 127] },
                { "at_s": 1.0, "device": "Synth", "data": [0x80, 72, 0] },
            ]),
            "{octave}"
        );
    }
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn note_expressions_pass_down_the_chain() {
    let setup = setup();
    let dawkey = start(&setup).await;
    // Key 60 at half per-note volume (a quarter of the gain).
    let clip = dawkey
        .call(
            "add_midi_clip",
            json!({
                "notes": [{ "pitch": 60, "start_beats": 1, "length_beats": 1, "velocity": 127 }],
                "controls": [{
                    "controller": { "kind": "note_controller", "pitch": 60, "number": 7 },
                    "time_beats": 0, "value": 32768,
                }],
            }),
        )
        .await
        .unwrap();
    let track = &clip["track"];
    // The octave effect sends the note and its volume an octave up, so the
    // instrument plays key 72 at a quarter of the level.
    add_plugin(&dawkey, track, "clap:dawkey.test.octave").await;
    add_plugin(&dawkey, track, "clap:dawkey.test.beep").await;
    assert!(approx(peak(&dawkey, 0.5, 1.0).await, -12.0412));
    // VST3 plugins pass them on the same way.
    let vst3_track = dawkey
        .call("add_track", json!({ "kind": "midi" }))
        .await
        .unwrap()["track"]
        .clone();
    let vst3_clip = dawkey
        .call(
            "add_midi_clip",
            json!({
                "track": vst3_track,
                "notes": [{ "pitch": 60, "start_beats": 4, "length_beats": 1, "velocity": 127 }],
                "controls": [{
                    "controller": { "kind": "note_controller", "pitch": 60, "number": 7 },
                    "time_beats": 0, "value": 32768,
                }],
            }),
        )
        .await
        .unwrap();
    assert_eq!(vst3_clip["track"], vst3_track);
    add_plugin(&dawkey, &vst3_track, &vst3("octave")).await;
    add_plugin(&dawkey, &vst3_track, &vst3("beep")).await;
    assert!(approx(peak(&dawkey, 2.0, 2.5).await, -12.0412));

    // To a device, as a MIDI 2.0 per-note controller before the note.
    dawkey
        .call(
            "set_track_midi_output",
            json!({ "track": track, "device": "Synth" }),
        )
        .await
        .unwrap();
    dawkey.call("play", json!({})).await.unwrap();
    let out = dawkey
        .call("advance_clock", json!({ "seconds": 0.75 }))
        .await
        .unwrap();
    dawkey.call("stop", json!({})).await.unwrap();
    let sent = out["midi_output"].as_array().unwrap();
    // (Playback starts by chasing the default, then the clip's value.)
    let volume = sent
        .iter()
        .rfind(|m| m["data"] == json!([0x00, 72, 7]))
        .unwrap_or_else(|| panic!("{out}"));
    assert_eq!(volume["ump"], json!([0x4000_4807u32, 0x8000_0000u32]));
    assert_eq!(sent.last().unwrap()["data"], json!([0x90, 72, 127]));
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn automation_and_latency() {
    let setup = setup();
    let dawkey = start(&setup).await;
    let track = signal_track(&dawkey, &setup.wav).await;
    let slot = add_plugin(&dawkey, &track, "clap:dawkey.test.gain").await;
    dawkey
        .call(
            "set_automation",
            json!({
                "track": track,
                "target": { "kind": "param", "plugin": slot, "param": "1" },
                "points": [{ "time_s": 0.5, "value": 0.0 }, { "time_s": 0.5001, "value": HALF_DB }],
            }),
        )
        .await
        .unwrap();
    assert!(approx(peak(&dawkey, 0.0, 0.49).await, HALF_CENTER_DB));
    assert!(approx(
        peak(&dawkey, 0.51, 1.0).await,
        HALF_CENTER_DB + HALF_DB
    ));
    let details = dawkey
        .call("get_plugin", json!({ "plugin": slot }))
        .await
        .unwrap();
    assert_eq!(details["params"][0]["automated"], true);
    // Out-of-range automation is refused.
    let err = dawkey
        .call_err(
            "add_automation_point",
            json!({
                "track": track,
                "target": { "kind": "param", "plugin": slot, "param": "1" },
                "time_s": 0.2, "value": 50.0,
            }),
        )
        .await
        .unwrap();
    assert!(err.contains("goes from"), "{err}");

    // A plugin with latency delays every track to match, and renders are
    // compensated.
    let other = signal_track(&dawkey, &setup.wav).await;
    add_plugin(&dawkey, &other, "clap:dawkey.test.delay").await;
    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    assert_eq!(status["plugin_latency_frames"], 64);
    let render = dawkey
        .call("render_range", json!({ "start_s": 0.0, "end_s": 0.01 }))
        .await
        .unwrap();
    // Both tracks sound from the very first frame (no 64-frame gap).
    assert_eq!(render["analysis"]["silent"], false);
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_plugins_are_reported_and_pass_audio() {
    let setup = setup();
    let dir = tempfile::tempdir().unwrap();
    let dawkey = start(&setup).await;
    let track = signal_track(&dawkey, &setup.wav).await;
    let slot = add_plugin(&dawkey, &track, "clap:dawkey.test.gain").await;
    dawkey
        .call(
            "set_plugin_param",
            json!({ "plugin": slot, "param": "1", "value": HALF_DB }),
        )
        .await
        .unwrap();
    let saved = dawkey
        .call("save_project", json!({ "path": dir.path().join("Song") }))
        .await
        .unwrap();
    dawkey.shutdown().await.unwrap();

    // Reopened where the plugin isn't installed.
    let empty = tempfile::tempdir().unwrap();
    let dawkey = start_with(empty.path(), empty.path(), empty.path()).await;
    dawkey
        .call("open_project", json!({ "path": saved["path"] }))
        .await
        .unwrap();
    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    let problems = status["plugin_problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{status}");
    assert_eq!(problems[0]["id"], "clap:dawkey.test.gain");
    assert!(approx(peak(&dawkey, 0.0, 1.0).await, HALF_CENTER_DB));
    // The slot and its setting are kept for when it's installed again.
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(
        state["project"]["tracks"][0]["plugins"][0]["params"]["1"],
        HALF_DB
    );
    let details = dawkey
        .call("get_plugin", json!({ "plugin": slot }))
        .await
        .unwrap();
    assert_eq!(details["error"], "not installed");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_plugin_lists_a_choices_options() {
    let setup = setup();
    for args in [&[][..], &["--bridge-plugins"][..]] {
        let dawkey = Harness::headless_with_env(
            DAWKEY,
            args.iter().copied(),
            [
                ("CLAP_PATH", &setup.clap),
                ("LV2_PATH", &setup.lv2),
                ("VST3_PATH", &setup.vst3),
            ],
        )
        .await
        .expect("dawkey starts");
        let track = dawkey.call("add_track", json!({})).await.unwrap()["track"].clone();
        let gain = add_plugin(&dawkey, &track, "clap:dawkey.test.gain").await;

        // A choice lists its options, named as the plugin shows them, and
        // takes one by name: an LV2 enumeration's scale points, and a CLAP
        // parameter whose steps lie between 0 and 1 (as LSP's do), each at
        // the value the plugin reads its name as, though none of the
        // samples that find them lands on a third.
        let cases = [
            (
                "lv2:urn:dawkey:test:tempo",
                json!([{ "value": 0.0, "text": "Zero" }, { "value": 1.0, "text": "One" }]),
                ("zero", 0.0),
            ),
            (
                "clap:dawkey.test.tempo",
                json!([
                    { "value": 0.0, "text": "Zero" },
                    { "value": 1.0 / 3.0, "text": "One" },
                    { "value": 2.0 / 3.0, "text": "Two" },
                    { "value": 1.0, "text": "Three" },
                ]),
                ("Two", 2.0 / 3.0),
            ),
        ];
        for (plugin, choices, (text, value)) in cases {
            let slot = add_plugin(&dawkey, &track, plugin).await;
            let details = dawkey
                .call("get_plugin", json!({ "plugin": slot }))
                .await
                .unwrap();
            let param = details["params"][0].clone();
            assert_eq!(param["text"], "One", "{plugin} {args:?}");
            assert_eq!(param["choices"], choices, "{plugin} {args:?}");
            dawkey
                .call(
                    "set_plugin_param",
                    json!({ "plugin": slot, "param": param["id"], "text": text }),
                )
                .await
                .unwrap();
            let details = dawkey
                .call("get_plugin", json!({ "plugin": slot }))
                .await
                .unwrap();
            assert_eq!(details["params"][0]["value"], value, "{plugin} {args:?}");
        }

        // A continuous parameter has none.
        let details = dawkey
            .call("get_plugin", json!({ "plugin": gain }))
            .await
            .unwrap();
        assert!(details["params"][0].get("choices").is_none(), "{args:?}");
        dawkey.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn lv2_plugins_get_the_worker() {
    let setup = setup();
    for args in [&[][..], &["--bridge-plugins"][..]] {
        let dawkey = Harness::headless_with_env(
            DAWKEY,
            args.iter().copied(),
            [
                ("CLAP_PATH", &setup.clap),
                ("LV2_PATH", &setup.lv2),
                ("VST3_PATH", &setup.vst3),
            ],
        )
        .await
        .expect("dawkey starts");
        let clip = dawkey
            .call(
                "add_midi_clip",
                json!({ "notes": [{ "pitch": 60, "start_beats": 0, "length_beats": 1, "velocity": 127 }] }),
            )
            .await
            .unwrap();
        // It requires `work:schedule`: its level (DC) is set through it.
        let slot = add_plugin(&dawkey, &clip["track"], "lv2:urn:dawkey:test:worker").await;
        dawkey.call("play", json!({})).await.unwrap();
        let played = dawkey
            .call("advance_clock", json!({ "seconds": 0.25 }))
            .await
            .unwrap();
        assert!(
            approx(played["output"]["peak_dbfs"][0].as_f64(), 0.0),
            "{args:?}: {played}"
        );
        dawkey.call("stop", json!({})).await.unwrap();
        dawkey
            .call("remove_clip", json!({ "clip": clip["clip"] }))
            .await
            .unwrap();
        // It said its state changed, so the project takes it (when the
        // studio next looks), and renders (fresh instances) restore it
        // through the worker before they start.
        let mut state = Value::Null;
        for _ in 0..100 {
            state = dawkey.call("get_state", json!({})).await.unwrap();
            if state["plugin_state_bytes"][slot.to_string()].is_u64() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            state["plugin_state_bytes"][slot.to_string()]
                .as_u64()
                .is_some_and(|n| n > 0),
            "{args:?}: {state}"
        );
        assert!(approx(peak(&dawkey, 0.0, 0.1).await, 0.0), "{args:?}");
        dawkey.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn instances_of_a_plugin_share_its_process_unless_asked_not_to() {
    let setup = setup();
    for (flag, shared) in [
        ("--bridge-plugins", true),
        ("--bridge-plugins=separate", false),
    ] {
        let dawkey = Harness::headless_with_env(
            DAWKEY,
            [flag],
            [
                ("CLAP_PATH", &setup.clap),
                ("LV2_PATH", &setup.lv2),
                ("VST3_PATH", &setup.vst3),
            ],
        )
        .await
        .expect("dawkey starts");
        let track = signal_track(&dawkey, &setup.wav).await;
        let mut pids = Vec::new();
        for plugin in [
            "clap:dawkey.test.gain",
            "clap:dawkey.test.gain",
            "clap:dawkey.test.delay",
            "lv2:urn:dawkey:test:gain",
            &vst3("gain"),
            &vst3("delay"),
        ] {
            let slot = add_plugin(&dawkey, &track, plugin).await;
            let details = dawkey
                .call("get_plugin", json!({ "plugin": slot }))
                .await
                .unwrap();
            pids.push(details["process_id"].as_u64().unwrap());
        }
        if shared {
            // The CLAP test plugins are one file, the LV2 ones a bundle, the
            // VST3 ones another.
            assert_eq!((pids[0], pids[0]), (pids[1], pids[2]), "{flag}");
            assert_ne!(pids[0], pids[3], "{flag}");
            assert_eq!(pids[4], pids[5], "{flag}");
            assert!(!pids[..4].contains(&pids[4]), "{flag}: {pids:?}");
        } else {
            let mut distinct = pids.clone();
            distinct.sort_unstable();
            distinct.dedup();
            assert_eq!(distinct.len(), 6, "{flag}: {pids:?}");
        }
        dawkey.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn bridged_plugins_run_in_processes_of_their_own() {
    let setup = setup();
    let dawkey = Harness::headless_with_env(
        DAWKEY,
        ["--bridge-plugins"],
        [
            ("CLAP_PATH", &setup.clap),
            ("LV2_PATH", &setup.lv2),
            ("VST3_PATH", &setup.vst3),
        ],
    )
    .await
    .expect("dawkey starts");
    let track = signal_track(&dawkey, &setup.wav).await;
    let mut pids = Vec::new();
    for (plugin, param) in [
        ("clap:dawkey.test.gain", "1"),
        ("lv2:urn:dawkey:test:gain", "gain"),
    ] {
        let slot = add_plugin(&dawkey, &track, plugin).await;
        dawkey
            .call(
                "set_plugin_param",
                json!({ "plugin": slot, "param": param, "value": HALF_DB }),
            )
            .await
            .unwrap();
        let details = dawkey
            .call("get_plugin", json!({ "plugin": slot }))
            .await
            .unwrap();
        pids.push(details["process_id"].as_u64().expect("its own process"));
        // Asked of the plugin in its process.
        assert!(
            details["params"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("-6.0")
        );
    }
    assert_ne!(pids[0], pids[1]);
    // Both halve the signal, in renders and as it plays.
    assert!(approx(
        peak(&dawkey, 0.0, 1.0).await,
        HALF_CENTER_DB + 2.0 * HALF_DB
    ));
    dawkey.call("play", json!({})).await.unwrap();
    dawkey
        .call("locate", json!({ "position_s": 0.0 }))
        .await
        .unwrap();
    let played = dawkey
        .call("advance_clock", json!({ "seconds": 0.5 }))
        .await
        .unwrap();
    assert!(
        approx(
            played["output"]["peak_dbfs"][0].as_f64(),
            HALF_CENTER_DB + 2.0 * HALF_DB
        ),
        "{played}"
    );

    // The first one crashes: only it stops, and the signal passes it.
    let slots: Vec<Value> = dawkey.call("get_state", json!({})).await.unwrap()["project"]["tracks"]
        [0]["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].clone())
        .collect();
    std::process::Command::new("kill")
        .args(["-9", &pids[0].to_string()])
        .status()
        .unwrap();
    let mut error = Value::Null;
    for _ in 0..100 {
        let details = dawkey
            .call("get_plugin", json!({ "plugin": slots[0] }))
            .await
            .unwrap();
        error = details["error"].clone();
        if !error.is_null() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(error.as_str().unwrap().contains("crashed"), "{error}");
    dawkey
        .call("locate", json!({ "position_s": 0.0 }))
        .await
        .unwrap();
    let played = dawkey
        .call("advance_clock", json!({ "seconds": 0.5 }))
        .await
        .unwrap();
    assert!(
        approx(
            played["output"]["peak_dbfs"][0].as_f64(),
            HALF_CENTER_DB + HALF_DB
        ),
        "{played}"
    );
    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    assert_eq!(status["plugin_problems"][0]["plugin"], slots[0]);

    // Reloading it brings it back, in a new process.
    let reloaded = dawkey
        .call("reload_plugin", json!({ "plugin": slots[0] }))
        .await
        .unwrap();
    assert!(reloaded.get("error").is_none(), "{reloaded}");
    assert_ne!(reloaded["process_id"].as_u64(), Some(pids[0]));
    dawkey
        .call("locate", json!({ "position_s": 0.0 }))
        .await
        .unwrap();
    let played = dawkey
        .call("advance_clock", json!({ "seconds": 0.5 }))
        .await
        .unwrap();
    assert!(
        approx(
            played["output"]["peak_dbfs"][0].as_f64(),
            HALF_CENTER_DB + 2.0 * HALF_DB
        ),
        "{played}"
    );
    dawkey.shutdown().await.unwrap();
    // No plugin process is left behind.
    for pid in pids {
        assert!(!Path::new("/proc").join(pid.to_string()).exists(), "{pid}");
    }
}

/// A batch applies several commands as one call and one undo step, where
/// later ones refer to what earlier ones made; a failure applies nothing.
#[tokio::test]
async fn batches_commands() {
    let setup = setup();
    let dawkey = start(&setup).await;
    dawkey
        .call("set_tempo", json!({ "bpm": 120 }))
        .await
        .unwrap();
    let result = dawkey
        .call(
            "batch",
            json!({
                "label": "set up the mix",
                "operations": [
                    { "tool": "add_track", "args": { "name": "Verb", "kind": "bus" } },
                    { "tool": "add_midi_clip", "args": {
                        "track_name": "Keys", "position_bar": 2,
                        "notes": [{ "pitch": 60, "start_beats": 0, "length_beats": 1 }],
                    } },
                    { "tool": "set_track_output", "args": { "track": "$1.track", "bus": "$0.track" } },
                    { "tool": "add_plugin", "args": { "track": "$1.track", "plugin": "clap:dawkey.test.gain" } },
                    { "tool": "set_plugin_param", "args": { "plugin": "$3.plugin", "param": "1", "value": -6 } },
                    { "tool": "set_track_pan", "args": { "track": "$1.track", "pan": -0.5 } },
                    { "tool": "edit_midi_notes", "args": { "clip": "$1.clip", "notes": [{ "id": "$1.notes[0]", "pitch": 67 }] } },
                ],
            }),
        )
        .await
        .unwrap();
    let outcomes = result["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 7);
    let bus = outcomes[0]["track"].as_u64().unwrap();
    let keys = outcomes[1]["track"].as_u64().unwrap();
    assert_eq!(result["revision"], 2, "one undo step");

    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["undo"], "set up the mix");
    let track = state["project"]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == keys)
        .unwrap();
    assert_eq!(track["output"], bus);
    assert_eq!(track["pan"], -0.5);
    assert_eq!(track["plugins"][0]["plugin"], "clap:dawkey.test.gain");
    assert_eq!(track["clips"][0]["start"], 96_000); // bar 2 at 120 bpm
    assert_eq!(track["clips"][0]["notes"][0]["pitch"], 67);
    let slot = outcomes[3]["plugin"].clone();
    let plugin = dawkey
        .call("get_plugin", json!({ "plugin": slot }))
        .await
        .unwrap();
    assert_eq!(plugin["params"][0]["value"], -6.0);

    // One undo reverts it all.
    dawkey.call("undo", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tracks"], json!([]));

    // A failure in the middle applies nothing, and says where.
    let err = dawkey
        .call_err(
            "batch",
            json!({ "operations": [
                { "tool": "add_track", "args": { "name": "A" } },
                { "tool": "add_plugin", "args": { "track": "$0.track", "plugin": "clap:not.there" } },
            ] }),
        )
        .await
        .unwrap();
    assert!(err.contains("operation 1: add_plugin:"), "{err}");
    assert!(err.contains("clap:not.there"), "{err}");
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tracks"], json!([]));
    assert_eq!(state["revision"], 3);

    // Bad batches.
    for (ops, expected) in [
        (json!([]), "at least one"),
        (json!([{ "tool": "get_state" }]), "can't be in a batch"),
        (json!([{ "tool": "nope" }]), "unknown tool"),
        (
            json!([{ "tool": "set_track_gain", "args": { "track": "$0.track", "gain_db": 0 } }]),
            "hasn't run yet",
        ),
        (
            json!([{ "tool": "add_track", "args": { "name": "A" } },
                   { "tool": "set_track_gain", "args": { "track": "$0.clip", "gain_db": 0 } }]),
            "returned no \"clip\"",
        ),
        (
            json!([{ "tool": "add_track", "args": { "name": "A", "bogus": 1 } }]),
            "add_track: invalid arguments: unknown field `bogus`",
        ),
    ] {
        let err = dawkey
            .call_err("batch", json!({ "operations": ops }))
            .await
            .unwrap();
        assert!(err.contains(expected), "{err}");
    }

    // Strings that start with `$` but aren't references are taken as they
    // are, and `$$` is a `$`.
    dawkey
        .call(
            "batch",
            json!({ "operations": [
                { "tool": "add_track", "args": { "name": "$5 Synth" } },
                { "tool": "add_track", "args": { "name": "$$0.track" } },
            ] }),
        )
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let names: Vec<&str> = state["project"]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["$5 Synth", "$0.track"]);
    dawkey.shutdown().await.unwrap();
}

/// Aux sends: a copy of a track's signal to a bus at its own level, post or
/// pre fader, automatable, and never a loop.
#[tokio::test]
async fn aux_sends() {
    let setup = setup();
    let dawkey = start(&setup).await;
    let track = signal_track(&dawkey, &setup.wav).await;
    let verb = dawkey
        .call("add_track", json!({ "name": "Verb", "kind": "bus" }))
        .await
        .unwrap()["track"]
        .as_u64()
        .unwrap();
    // The bus halves what reaches it, so its share can be told apart.
    let slot = add_plugin(&dawkey, &json!(verb), "clap:dawkey.test.gain").await;
    dawkey
        .call(
            "set_plugin_param",
            json!({ "plugin": slot, "param": "1", "value": HALF_DB }),
        )
        .await
        .unwrap();
    let direct = peak(&dawkey, 0.0, 0.5).await.unwrap();
    assert!(approx(Some(direct), HALF_CENTER_DB));

    // A send at 0 dB adds a halved copy: 1.5 times the direct signal.
    let set = dawkey
        .call("set_track_send", json!({ "track": track, "bus": verb }))
        .await
        .unwrap();
    assert!(set["revision"].is_u64());
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(
        state["project"]["tracks"][0]["sends"],
        json!([{ "bus": verb, "gain_db": 0.0 }])
    );
    let with_send = peak(&dawkey, 0.0, 0.5).await.unwrap();
    assert!(
        approx(Some(with_send), direct + 20.0 * 1.5f64.log10()),
        "{with_send}"
    );

    // At -6.02 dB the copy is a quarter: 1.25 times.
    dawkey
        .call(
            "set_track_send",
            json!({ "track": track, "bus": verb, "gain_db": HALF_DB }),
        )
        .await
        .unwrap();
    let quieter = peak(&dawkey, 0.0, 0.5).await.unwrap();
    assert!(
        approx(Some(quieter), direct + 20.0 * 1.25f64.log10()),
        "{quieter}"
    );

    // Post-fader: pulling the fader down takes the send with it. Pre-fader:
    // the send stays.
    dawkey
        .call(
            "set_track_gain",
            json!({ "track": track, "gain_db": -144.0 }),
        )
        .await
        .unwrap();
    let faded = peak(&dawkey, 0.0, 0.5).await.unwrap_or(-200.0);
    assert!(faded < -140.0, "faded out, send and all: {faded}");
    dawkey
        .call(
            "set_track_send",
            json!({ "track": track, "bus": verb, "pre_fader": true }),
        )
        .await
        .unwrap();
    let pre = peak(&dawkey, 0.0, 0.5).await.unwrap();
    // Before the fader and pan law: the half-scale clip, the send's -6 dB,
    // and the bus's plugin halving it.
    assert!(approx(Some(pre), 3.0 * HALF_DB), "{pre}");
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tracks"][0]["sends"][0]["pre_fader"], true);
    dawkey
        .call("set_track_gain", json!({ "track": track, "gain_db": 0.0 }))
        .await
        .unwrap();

    // The send's level automates, in dB.
    dawkey
        .call(
            "set_automation",
            json!({ "track": track, "target": { "kind": "send", "bus": verb }, "points": [
                { "time_s": 0.0, "value": -144.0 }, { "time_s": 0.0001, "value": -144.0 },
                { "time_s": 0.5, "value": -144.0 }, { "time_s": 0.5001, "value": 0.0 },
            ] }),
        )
        .await
        .unwrap();
    let before = peak(&dawkey, 0.1, 0.4).await.unwrap();
    assert!(
        approx(Some(before), HALF_CENTER_DB),
        "no send yet: {before}"
    );
    let after = peak(&dawkey, 0.6, 0.9).await.unwrap();
    assert!(after > before + 1.0, "{after} vs {before}");

    // Removing the send takes its lane; the bus going takes the send.
    dawkey
        .call("remove_track_send", json!({ "track": track, "bus": verb }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert!(state["project"]["tracks"][0].get("sends").is_none());
    assert!(state["project"]["tracks"][0].get("automation").is_none());
    dawkey
        .call("set_track_send", json!({ "track": track, "bus": verb }))
        .await
        .unwrap();
    dawkey
        .call("remove_track", json!({ "track": verb }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert!(state["project"]["tracks"][0].get("sends").is_none());
    dawkey.call("undo", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tracks"][0]["sends"][0]["bus"], verb);

    // No loops, no sends to tracks, none from the master.
    let other = dawkey
        .call("add_track", json!({ "name": "Other", "kind": "bus" }))
        .await
        .unwrap()["track"]
        .as_u64()
        .unwrap();
    dawkey
        .call("set_track_output", json!({ "track": verb, "bus": other }))
        .await
        .unwrap();
    for (args, expected) in [
        (json!({ "track": other, "bus": verb }), "loop"),
        (json!({ "track": verb, "bus": verb }), "loop"),
        (json!({ "track": track, "bus": track }), "not a bus"),
        (json!({ "track": 0, "bus": verb }), "master"),
        (json!({ "track": track, "bus": 99 }), "99"),
        (
            json!({ "track": track, "bus": other, "gain_db": 50 }),
            "gain_db",
        ),
    ] {
        let err = dawkey.call_err("set_track_send", args).await.unwrap();
        assert!(err.contains(expected), "{err}");
    }
    let err = dawkey
        .call_err("remove_track_send", json!({ "track": track, "bus": other }))
        .await
        .unwrap();
    assert!(err.contains("no send"), "{err}");
    dawkey.shutdown().await.unwrap();
}

/// An LV2 plugin's properties beyond its parameters: listed from its TTL
/// and its saved state, and set by rewriting the state and reloading.
#[tokio::test]
async fn plugin_properties() {
    let setup = setup();
    let dawkey = start(&setup).await;
    let track = signal_track(&dawkey, &setup.wav).await;
    let slot = add_plugin(&dawkey, &track, "lv2:urn:dawkey:test:memo").await;
    let details = dawkey
        .call("get_plugin", json!({ "plugin": slot }))
        .await
        .unwrap();
    assert_eq!(
        details["properties"],
        json!([{
            "uri": "urn:dawkey:test:memo#memo",
            "label": "Memo",
            "range": "http://lv2plug.in/ns/ext/atom#Float",
            "writable": true,
        }])
    );
    assert!(details.get("state").is_none(), "nothing saved yet");
    // The memo adds nothing yet.
    assert!(approx(peak(&dawkey, 0.0, 0.5).await, HALF_CENTER_DB));

    // Set it: the plugin starts over with 0.25 added to its audio.
    let set = dawkey
        .call(
            "set_plugin_property",
            json!({ "plugin": slot, "key": "urn:dawkey:test:memo#memo", "value": 0.25, "kind": "Float" }),
        )
        .await
        .unwrap();
    assert_eq!(
        set["state"],
        json!([{
            "key": "urn:dawkey:test:memo#memo",
            "kind": "http://lv2plug.in/ns/ext/atom#Float",
            "value": 0.25,
        }])
    );
    assert!(set["revision"].is_u64());
    let louder = peak(&dawkey, 0.0, 0.5).await.unwrap();
    // 0.5 + 0.25 at center pan.
    assert!(
        approx(Some(louder), HALF_CENTER_DB + 20.0 * 1.5f64.log10()),
        "{louder}"
    );

    // Once it has run, the plugin saves the memo as a file of its own (its
    // state says where, as a path), so a change says its type.
    let state = dawkey
        .call("get_plugin", json!({ "plugin": slot }))
        .await
        .unwrap()["state"]
        .clone();
    assert_eq!(state[0]["kind"], "http://lv2plug.in/ns/ext/atom#Path");
    assert!(state[0]["value"].is_string());
    dawkey
        .call(
            "set_plugin_property",
            json!({ "plugin": slot, "key": "urn:dawkey:test:memo#memo", "value": 0.5, "kind": "Float" }),
        )
        .await
        .unwrap();
    let full = peak(&dawkey, 0.0, 0.5).await.unwrap();
    assert!(
        approx(Some(full), HALF_CENTER_DB + 20.0 * 2.0f64.log10()),
        "{full}"
    );

    // It's saved with the project, and survives reopening it.
    let dir = tempfile::tempdir().unwrap();
    let saved = dawkey
        .call("save_project", json!({ "path": dir.path().join("Memo") }))
        .await
        .unwrap();
    dawkey.call("new_project", json!({})).await.unwrap();
    dawkey
        .call("open_project", json!({ "path": saved["path"] }))
        .await
        .unwrap();
    let reopened = peak(&dawkey, 0.0, 0.5).await.unwrap();
    assert!(approx(Some(reopened), full), "{reopened}");

    // Other kinds, and removing a key.
    dawkey
        .call(
            "set_plugin_property",
            json!({ "plugin": slot, "key": "urn:example:note", "value": "/tmp/x.sf2" }),
        )
        .await
        .unwrap();
    let state = dawkey
        .call("get_plugin", json!({ "plugin": slot }))
        .await
        .unwrap()["state"]
        .clone();
    let note = state
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["key"] == "urn:example:note")
        .unwrap();
    assert_eq!(note["kind"], "http://lv2plug.in/ns/ext/atom#Path");
    assert_eq!(note["value"], "/tmp/x.sf2");
    dawkey
        .call(
            "set_plugin_property",
            json!({ "plugin": slot, "key": "urn:example:note", "value": null }),
        )
        .await
        .unwrap();
    let state = dawkey
        .call("get_plugin", json!({ "plugin": slot }))
        .await
        .unwrap()["state"]
        .clone();
    assert!(
        state
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["key"] != "urn:example:note")
    );

    // Bad input.
    let clap = add_plugin(&dawkey, &track, "clap:dawkey.test.gain").await;
    for (args, expected) in [
        (
            json!({ "plugin": clap, "key": "urn:x", "value": 1 }),
            "not an LV2 plugin",
        ),
        (
            json!({ "plugin": slot, "key": "urn:x", "value": 1, "kind": "Chunk" }),
            "can't be written",
        ),
        (
            json!({ "plugin": slot, "key": "urn:x", "value": "text", "kind": "Int" }),
            "whole number",
        ),
        (
            json!({ "plugin": slot, "key": " ", "value": 1 }),
            "needs a key",
        ),
        (json!({ "plugin": 999, "key": "urn:x", "value": 1 }), "999"),
    ] {
        let err = dawkey.call_err("set_plugin_property", args).await.unwrap();
        assert!(err.contains(expected), "{err}");
    }
    dawkey.shutdown().await.unwrap();
}
