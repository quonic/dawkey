// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end UI tests over MCP, headless. Screenshots render on the software
//! Vulkan adapter (lavapipe), so they're comparable across machines.
//!
//! To regenerate the golden images after an intended visual change:
//! `DAWKEY_UPDATE_GOLDENS=1 cargo test --test mcp_ui`, then look at them.

use std::path::Path;

use base64::Engine as _;
use dk_testkit::Harness;
use serde_json::{Value, json};

const DAWKEY: &str = env!("CARGO_BIN_EXE_dawkey");

async fn start() -> Harness {
    Harness::headless(DAWKEY, std::iter::empty::<&str>())
        .await
        .expect("dawkey starts")
}

fn find<'a>(node: &'a Value, id: &str) -> Option<&'a Value> {
    if node["id"] == id {
        return Some(node);
    }
    node["children"]
        .as_array()?
        .iter()
        .find_map(|child| find(child, id))
}

async fn tree(dawkey: &Harness) -> Value {
    dawkey.call("ui_tree", json!({})).await.unwrap()
}

/// (x, y, w, h) of a widget.
async fn bounds(dawkey: &Harness, id: &str) -> (f64, f64, f64, f64) {
    let t = tree(dawkey).await;
    let b = &find(&t["root"], id).unwrap_or_else(|| panic!("no {id}"))["bounds"];
    let f = |k: &str| b[k].as_f64().unwrap();
    (f("x"), f("y"), f("w"), f("h"))
}

fn write_wav(dir: &Path, name: &str, samples: Vec<f32>) -> std::path::PathBuf {
    let path = dir.join(name);
    dk_media::write_wav(&path, &[samples], 48_000).unwrap();
    path
}

/// Exponentially decaying pulses every half second (no transcendental
/// functions, so identical everywhere).
fn decaying_pulses(frames: usize) -> Vec<f32> {
    let mut level = 0.0f32;
    (0..frames)
        .map(|i| {
            if i % 24_000 == 0 {
                level = 0.9;
            }
            level *= 0.9997;
            if (i / 40) % 2 == 0 { level } else { -level }
        })
        .collect()
}

/// A slow triangle-shaped swell of a fast square wave.
fn slow_triangle(frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| {
            let phase = (i % 96_000) as f32 / 96_000.0;
            let env = 0.2 + 0.5 * (1.0 - (2.0 * phase - 1.0).abs());
            if (i / 109) % 2 == 0 { env } else { -env }
        })
        .collect()
}

fn clip_start(state: &Value, clip: u64) -> f64 {
    state["project"]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|t| t["clips"].as_array().unwrap())
        .find(|c| c["id"] == clip)
        .map(|c| c["start"].as_f64().unwrap() / 48_000.0)
        .unwrap()
}

#[tokio::test]
async fn tree_and_clicks() {
    let dawkey = start().await;
    let t = tree(&dawkey).await;
    assert_eq!(t["platform"], "headless");
    assert_eq!(t["window"]["csd"], true);
    let root = &t["root"];
    assert!(find(root, "tracks/empty").is_some());
    let play = find(root, "transport/play").unwrap();
    assert_eq!(play["role"], "toggle");
    assert_eq!(play["checked"], false);

    let track = dawkey
        .call("add_track", json!({ "name": "Lead" }))
        .await
        .unwrap()["track"]
        .clone();
    let mute = format!("track/{track}/mute");
    dawkey
        .call("ui_click", json!({ "id": mute }))
        .await
        .unwrap();
    let t = tree(&dawkey).await;
    assert_eq!(find(&t["root"], &mute).unwrap()["checked"], true);
    // A UI click is an ordinary command: it shows up in the project and undo.
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tracks"][0]["mute"], true);
    assert_eq!(state["undo"], "set_track_mute");

    dawkey
        .call("ui_click", json!({ "id": "transport/play" }))
        .await
        .unwrap();
    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    assert_eq!(status["transport"]["playing"], true);

    // There are unsaved changes, so closing asks first.
    let close = dawkey
        .call("ui_click", json!({ "id": "window/close" }))
        .await
        .unwrap();
    assert_eq!(close["window_actions"], json!([]));
    assert_eq!(close["view"]["confirm"], "close");
    let close = dawkey
        .call("ui_click", json!({ "id": "confirm/discard" }))
        .await
        .unwrap();
    assert_eq!(close["window_actions"], json!(["close"]));

    let err = dawkey
        .call_err("ui_click", json!({ "id": "no/such/widget" }))
        .await
        .unwrap();
    assert!(err.contains("no widget"), "{err}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn resize_relayouts() {
    let dawkey = start().await;
    dawkey
        .call("ui_resize", json!({ "width": 800, "height": 500 }))
        .await
        .unwrap();
    let t = tree(&dawkey).await;
    let close = find(&t["root"], "window/close").unwrap();
    // Right edge of the window, inside the 16 px shadow margin.
    let right = close["bounds"]["x"].as_f64().unwrap() + close["bounds"]["w"].as_f64().unwrap();
    assert_eq!(right, 784.0);
    assert!(
        dawkey
            .call_err("ui_resize", json!({ "width": 10, "height": 10 }))
            .await
            .unwrap()
            .contains("out of range")
    );
    dawkey.shutdown().await.unwrap();
}

async fn screenshot(dawkey: &Harness, scale: f64) -> png::Reader<std::io::Cursor<Vec<u8>>> {
    let result = dawkey
        .call_raw("ui_screenshot", json!({ "scale": scale }))
        .await
        .unwrap();
    let image = result
        .content
        .iter()
        .find_map(|c| c.as_image())
        .expect("image content");
    assert_eq!(image.mime_type, "image/png");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&image.data)
        .unwrap();
    png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap()
}

fn pixels(mut reader: png::Reader<std::io::Cursor<Vec<u8>>>) -> (u32, u32, Vec<u8>) {
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    buf.truncate(info.buffer_size());
    (info.width, info.height, buf)
}

#[tokio::test]
async fn screenshot_matches_golden() {
    let dir = tempfile::tempdir().unwrap();
    let dawkey = start().await;
    dawkey
        .call("rename_project", json!({ "name": "Golden" }))
        .await
        .unwrap();
    // Two deterministic signals, dropped onto the timeline.
    let kick = write_wav(dir.path(), "kick.wav", decaying_pulses(96_000));
    let pad = write_wav(dir.path(), "pad.wav", slow_triangle(144_000));
    let lanes = bounds(&dawkey, "timeline/lanes").await;
    dawkey
        .call(
            "ui_drop_files",
            json!({ "paths": [kick, pad], "x": lanes.0 + 40.0, "y": lanes.1 + 10.0 }),
        )
        .await
        .unwrap();
    dawkey
        .call("add_track", json!({ "name": "Vocals" }))
        .await
        .unwrap();
    dawkey
        .call("set_track_gain", json!({ "track": 2, "gain_db": -4.5 }))
        .await
        .unwrap();
    dawkey
        .call("ui_click", json!({ "id": "track/2/mute" }))
        .await
        .unwrap();
    dawkey
        .call("ui_click", json!({ "id": "track/3/solo" }))
        .await
        .unwrap();
    dawkey
        .call("ui_click", json!({ "id": "clip/1" }))
        .await
        .unwrap();
    dawkey
        .call("locate", json!({ "position_s": 1.25 }))
        .await
        .unwrap();
    dawkey
        .call("ui_resize", json!({ "width": 900, "height": 460 }))
        .await
        .unwrap();

    assert_golden(&dawkey, "main_window", (900, 460)).await;

    // HiDPI renders scale up.
    let (w2, h2, _) = pixels(screenshot(&dawkey, 2.0).await);
    assert_eq!((w2, h2), (1800, 920));
    dawkey.shutdown().await.unwrap();
}

/// Compares a screenshot with `tests/golden/<name>.png` (or writes it, with
/// `DAWKEY_UPDATE_GOLDENS` set).
async fn assert_golden(dawkey: &Harness, name: &str, size: (u32, u32)) {
    let (w, h, actual) = pixels(screenshot(dawkey, 1.0).await);
    assert_eq!((w, h), size);
    let golden_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.png"));
    if std::env::var_os("DAWKEY_UPDATE_GOLDENS").is_some() {
        let result = dawkey
            .call_raw("ui_screenshot", json!({ "scale": 1.0 }))
            .await
            .unwrap();
        let image = result.content.iter().find_map(|c| c.as_image()).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&image.data)
            .unwrap();
        std::fs::create_dir_all(golden_path.parent().unwrap()).unwrap();
        std::fs::write(&golden_path, bytes).unwrap();
    }
    let golden = std::fs::read(&golden_path).unwrap_or_else(|_| {
        panic!(
            "{} is missing; run with DAWKEY_UPDATE_GOLDENS=1 to create it",
            golden_path.display()
        )
    });
    let decoder = png::Decoder::new(std::io::Cursor::new(golden));
    let (gw, gh, expected) = pixels(decoder.read_info().unwrap());
    assert_eq!((gw, gh), (w, h));

    // Allow tiny rasterization differences between lavapipe versions.
    let differing = actual
        .as_chunks::<4>()
        .0
        .iter()
        .zip(expected.as_chunks::<4>().0)
        .filter(|(a, e)| a.iter().zip(*e).any(|(x, y)| x.abs_diff(*y) > 8))
        .count();
    let total = (w * h) as usize;
    assert!(
        differing * 1000 <= total,
        "{differing} of {total} pixels differ from {}",
        golden_path.display()
    );
}

#[tokio::test]
async fn piano_roll_over_mcp() {
    let dawkey = start().await;
    dawkey
        .call("rename_project", json!({ "name": "Notes" }))
        .await
        .unwrap();
    dawkey
        .call("add_track", json!({ "kind": "midi", "name": "Lead" }))
        .await
        .unwrap();
    // Double-clicking the empty lane makes a one-bar clip at the beat under
    // the pointer: 0.5 s is 40 px in.
    let lanes = bounds(&dawkey, "timeline/lanes").await;
    dawkey
        .call(
            "ui_click",
            json!({ "x": lanes.0 + 41.0, "y": lanes.1 + 20.0, "double": true }),
        )
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let clip = &state["project"]["tracks"][0]["clips"][0];
    assert_eq!(
        (clip["start"].as_u64(), clip["length"].as_u64()),
        (Some(24_000), Some(96_000))
    );
    let clip = clip["id"].as_u64().unwrap();
    dawkey
        .call(
            "add_midi_notes",
            json!({ "clip": clip, "notes": [
                { "pitch": 60, "start_beats": 0, "length_beats": 1 },
                { "pitch": 64, "start_beats": 1, "length_beats": 1, "velocity": 64 },
                { "pitch": 67, "start_beats": 2, "length_beats": 2 },
            ] }),
        )
        .await
        .unwrap();

    // Double-click the clip to open it.
    let opened = dawkey
        .call(
            "ui_click",
            json!({ "id": format!("clip/{clip}"), "double": true }),
        )
        .await
        .unwrap();
    assert_eq!(opened["view"]["page"], "piano_roll");
    assert_eq!(opened["view"]["editing_clip"], clip);
    let t = tree(&dawkey).await;
    for id in [
        "piano_roll/grid",
        "piano_roll/keys",
        "piano_roll/close",
        "note/1",
        "note/1/end",
        "track/1/synth/cutoff",
    ] {
        assert!(find(&t["root"], id).is_some(), "missing {id}");
    }

    // Drag the first note up an octave (12 rows of 12 px).
    dawkey
        .call("ui_drag", json!({ "id": "note/1", "dx": 0, "dy": -144 }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let notes = &state["project"]["tracks"][0]["clips"][0]["notes"];
    assert_eq!(notes[0]["pitch"], 72);
    dawkey
        .call("ui_click", json!({ "id": "note/2" }))
        .await
        .unwrap();
    assert_golden(&dawkey, "piano_roll", (1280, 800)).await;

    dawkey
        .call("ui_key", json!({ "key": "delete" }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(
        state["project"]["tracks"][0]["clips"][0]["notes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let closed = dawkey
        .call("ui_click", json!({ "id": "piano_roll/close" }))
        .await
        .unwrap();
    assert_eq!(closed["view"]["page"], "arrange");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn editing_with_drags_keys_and_scroll() {
    let dir = tempfile::tempdir().unwrap();
    let dawkey = start().await;
    let wav = write_wav(dir.path(), "a.wav", decaying_pulses(48_000));
    let lanes = bounds(&dawkey, "timeline/lanes").await;
    let dropped = dawkey
        .call(
            "ui_drop_files",
            json!({ "paths": [wav], "x": lanes.0 + 80.0, "y": lanes.1 + 10.0 }),
        )
        .await
        .unwrap();
    let clip = dropped["imported"][0]["placed"]["clip"].as_u64().unwrap();
    let track = dropped["imported"][0]["placed"]["track"].as_u64().unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(clip_start(&state, clip), 1.0, "dropped at 80 px = 1 s");

    // Drag right by 2 s (160 px at the default zoom); one undo step.
    dawkey
        .call(
            "ui_drag",
            json!({ "id": format!("clip/{clip}"), "dx": 160 }),
        )
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(clip_start(&state, clip), 3.0);
    assert_eq!(state["undo"], "move_clip");
    dawkey
        .call("ui_key", json!({ "key": "ctrl+z" }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(clip_start(&state, clip), 1.0);
    dawkey
        .call("ui_key", json!({ "key": "ctrl+shift+z" }))
        .await
        .unwrap();

    // Dragging the clip's end half a second (40 px) left trims it.
    dawkey
        .call(
            "ui_drag",
            json!({ "id": format!("clip/{clip}/end"), "dx": -40 }),
        )
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["undo"], "trim_clip");
    assert_eq!(state["project"]["tracks"][0]["clips"][0]["length"], 24_000);
    dawkey
        .call("ui_key", json!({ "key": "ctrl+z" }))
        .await
        .unwrap();

    // Ctrl+scroll zooms; the time under the pointer stays put.
    let zoomed = dawkey
        .call(
            "ui_scroll",
            json!({ "x": lanes.0 + 160.0, "y": lanes.1 + 10.0, "dy": -150, "modifiers": { "ctrl": true } }),
        )
        .await
        .unwrap();
    let pps = zoomed["view"]["px_per_s"].as_f64().unwrap();
    let scroll = zoomed["view"]["scroll_s"].as_f64().unwrap();
    assert!(pps > 80.0);
    assert!((scroll + 160.0 / pps - 2.0).abs() < 1e-3);

    // Clicking the ruler moves the playhead there.
    let ruler = bounds(&dawkey, "timeline/ruler").await;
    dawkey
        .call(
            "ui_click",
            json!({ "x": ruler.0 + 100.0, "y": ruler.1 + 5.0 }),
        )
        .await
        .unwrap();
    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    let position = status["transport"]["position_s"].as_f64().unwrap();
    assert!(
        (position - (scroll + 100.0 / pps)).abs() < 1e-3,
        "{position}"
    );

    // Space plays, delete removes the selected clip.
    dawkey
        .call("ui_key", json!({ "key": "space" }))
        .await
        .unwrap();
    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    assert_eq!(status["transport"]["playing"], true);
    dawkey
        .call("ui_click", json!({ "id": format!("clip/{clip}") }))
        .await
        .unwrap();
    dawkey
        .call("ui_key", json!({ "key": "delete" }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tracks"][0]["clips"], json!([]));

    // The mixer: drag a fader up to +12 dB.
    dawkey
        .call("ui_click", json!({ "id": "view/mixer" }))
        .await
        .unwrap();
    dawkey
        .call(
            "ui_drag",
            json!({ "id": format!("track/{track}/fader"), "dy": -60 }),
        )
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tracks"][0]["gain_db"], 12.0);
    assert_eq!(state["undo"], "set_track_gain");

    // Headless can't show dialogs; it says which one was asked for.
    let import = dawkey
        .call("ui_click", json!({ "id": "toolbar/import" }))
        .await
        .unwrap();
    assert_eq!(import["dialogs"][0]["request"], "choose_audio_files");

    // Opening another project over unsaved changes asks first; saving a
    // project that has no file yet asks where, then carries on opening.
    let asked = dawkey
        .call("ui_key", json!({ "key": "ctrl+o" }))
        .await
        .unwrap();
    assert_eq!(asked["view"]["confirm"], "open_project");
    assert_eq!(asked["dialogs"], json!([]));
    let saved = dawkey
        .call("ui_click", json!({ "id": "confirm/save" }))
        .await
        .unwrap();
    assert_eq!(
        saved["dialogs"],
        json!([{ "request": "choose_save_location", "then": "open_project" }])
    );
    assert_eq!(saved["view"]["confirm"], Value::Null);
    let bad = dawkey
        .call_err("ui_key", json!({ "key": "hyper+x" }))
        .await
        .unwrap();
    assert!(bad.contains("unknown modifier"), "{bad}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn automation_lanes_screenshot() {
    let dir = tempfile::tempdir().unwrap();
    let installed = dk_test_plugins::install(&dir.path().join("plugins")).unwrap();
    let dawkey = Harness::headless_with_env(
        DAWKEY,
        std::iter::empty::<&str>(),
        [
            ("CLAP_PATH", installed.clap_path.as_os_str()),
            ("LV2_PATH", installed.lv2_path.as_os_str()),
            ("VST3_PATH", installed.vst3_path.as_os_str()),
        ],
    )
    .await
    .unwrap();
    dawkey
        .call("rename_project", json!({ "name": "Lanes" }))
        .await
        .unwrap();
    let pad = write_wav(dir.path(), "pad.wav", slow_triangle(144_000));
    let track = dawkey
        .call("import_audio", json!({ "path": pad }))
        .await
        .unwrap()["track"]
        .clone();
    let slot = dawkey
        .call(
            "add_plugin",
            json!({ "track": track, "plugin": "clap:dawkey.test.gain" }),
        )
        .await
        .unwrap()["plugin"]
        .clone();
    dawkey
        .call(
            "set_automation",
            json!({
                "track": track,
                "target": { "kind": "param", "plugin": slot, "param": "1" },
                "points": [
                    { "time_s": 0.0, "value": -24.0 },
                    { "time_s": 1.5, "value": 6.0 },
                    { "time_s": 3.0, "value": -12.0 },
                ],
            }),
        )
        .await
        .unwrap();
    dawkey
        .call(
            "set_automation",
            json!({
                "track": track,
                "target": { "kind": "pan" },
                "points": [{ "time_s": 0.5, "value": -1.0 }, { "time_s": 2.5, "value": 1.0 }],
            }),
        )
        .await
        .unwrap();
    dawkey
        .call(
            "ui_click",
            json!({ "id": format!("track/{track}/automation") }),
        )
        .await
        .unwrap();
    dawkey
        .call("ui_click", json!({ "id": format!("auto/{track}/0/1") }))
        .await
        .unwrap();
    dawkey
        .call("locate", json!({ "position_s": 1.0 }))
        .await
        .unwrap();
    let t = tree(&dawkey).await;
    let header = find(&t["root"], &format!("track/{track}/auto/0")).unwrap();
    assert_eq!(header["name"], "Test Gain: Gain");
    assert_eq!(header["value"], "-4");
    dawkey
        .call("ui_resize", json!({ "width": 900, "height": 460 }))
        .await
        .unwrap();
    assert_golden(&dawkey, "automation", (900, 460)).await;
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn tempo_lanes_over_mcp() {
    let dawkey = start().await;
    dawkey
        .call("rename_project", json!({ "name": "Tempo" }))
        .await
        .unwrap();
    dawkey
        .call(
            "add_midi_clip",
            json!({ "notes": [{ "pitch": 60, "start_beats": 0, "length_beats": 2 },
                              { "pitch": 67, "start_beats": 4, "length_beats": 2 }] }),
        )
        .await
        .unwrap();
    dawkey
        .call(
            "set_tempo_map",
            json!({ "bpm": 100, "changes": [{ "beat": 8, "bpm": 140 },
                                             { "beat": 16, "bpm": 90, "ramp": true }] }),
        )
        .await
        .unwrap();
    dawkey
        .call(
            "set_time_signature",
            json!({ "numerator": 7, "denominator": 8, "at_bar": 3 }),
        )
        .await
        .unwrap();
    dawkey
        .call("ui_click", json!({ "id": "transport/tempo" }))
        .await
        .unwrap();
    // Dragging the 140 BPM change up 10 px: 145 BPM, one undo step.
    dawkey
        .call("ui_drag", json!({ "id": "tempo/1", "dx": 0, "dy": -10 }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tempo_changes"][0]["bpm"], 145.0);
    dawkey.call("undo", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tempo_changes"][0]["bpm"], 140.0);
    // Ramping the change, from its header button.
    dawkey
        .call("ui_click", json!({ "id": "tempo/1" }))
        .await
        .unwrap();
    dawkey
        .call("ui_click", json!({ "id": "tempo/ramp" }))
        .await
        .unwrap();
    let t = tree(&dawkey).await;
    let point = find(&t["root"], "tempo/1").unwrap();
    assert_eq!(
        point["value"],
        "140 BPM at beat 8, ramping from the previous"
    );
    let header = find(&t["root"], "meter/header").unwrap();
    assert_eq!(header["value"], "4/4 from bar 1");
    dawkey
        .call("ui_click", json!({ "id": "meter/1" }))
        .await
        .unwrap();
    dawkey
        .call("ui_resize", json!({ "width": 900, "height": 460 }))
        .await
        .unwrap();
    assert_golden(&dawkey, "tempo_lanes", (900, 460)).await;

    // Typing a tempo: the selected change's BPM, set on Return.
    dawkey
        .call("ui_click", json!({ "id": "tempo/1" }))
        .await
        .unwrap();
    dawkey
        .call("ui_click", json!({ "id": "tempo/value" }))
        .await
        .unwrap();
    dawkey
        .call("ui_type", json!({ "text": "132.5" }))
        .await
        .unwrap();
    let t = tree(&dawkey).await;
    assert_eq!(find(&t["root"], "tempo/value").unwrap()["value"], "132.5");
    dawkey
        .call("ui_key", json!({ "key": "return" }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tempo_changes"][0]["bpm"], 132.5);
    assert_eq!(state["undo"], "set_tempo");

    // The time signature: the numerator, Tab, the denominator.
    dawkey
        .call("ui_click", json!({ "id": "meter/numerator/value" }))
        .await
        .unwrap();
    dawkey
        .call("ui_type", json!({ "text": "6" }))
        .await
        .unwrap();
    dawkey
        .call("ui_key", json!({ "key": "tab" }))
        .await
        .unwrap();
    dawkey
        .call("ui_type", json!({ "text": "8" }))
        .await
        .unwrap();
    dawkey
        .call("ui_key", json!({ "key": "return" }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let sig = &state["project"]["time_signature"];
    assert_eq!(
        (&sig["numerator"], &sig["denominator"]),
        (&json!(6), &json!(8))
    );
    dawkey.shutdown().await.unwrap();
}

/// (start, length) in seconds of the clips on track `track`, by start.
fn clips_on(state: &Value, track: u64) -> Vec<(f64, f64)> {
    let mut clips: Vec<_> = state["project"]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == track)
        .unwrap()["clips"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["start"].as_f64().unwrap() / 48_000.0,
                c["length"].as_f64().unwrap() / 48_000.0,
            )
        })
        .collect();
    clips.sort_by(|a, b| a.0.total_cmp(&b.0));
    clips
}

#[tokio::test]
async fn copying_and_repeating_clips_with_keys_and_drags() {
    let dir = tempfile::tempdir().unwrap();
    let dawkey = start().await;
    let wav = write_wav(dir.path(), "a.wav", decaying_pulses(48_000));
    let lanes = bounds(&dawkey, "timeline/lanes").await;
    let dropped = dawkey
        .call(
            "ui_drop_files",
            json!({ "paths": [wav], "x": lanes.0 + 80.0, "y": lanes.1 + 10.0 }),
        )
        .await
        .unwrap();
    let clip = dropped["imported"][0]["placed"]["clip"].as_u64().unwrap();
    let track = dropped["imported"][0]["placed"]["track"].as_u64().unwrap();
    let other = dawkey.call("add_track", json!({})).await.unwrap()["track"]
        .as_u64()
        .unwrap();

    // Copied, and pasted over the other track at 3 s (240 px).
    dawkey
        .call("ui_click", json!({ "id": format!("clip/{clip}") }))
        .await
        .unwrap();
    let copied = dawkey
        .call("ui_key", json!({ "key": "ctrl+c" }))
        .await
        .unwrap();
    assert_eq!(copied["view"]["clipboard"], clip);
    let lane = bounds(&dawkey, &format!("track/{other}/lane")).await;
    dawkey
        .call(
            "ui_hover",
            json!({ "x": lane.0 + 240.0, "y": lane.1 + lane.3 / 2.0 }),
        )
        .await
        .unwrap();
    dawkey
        .call("ui_key", json!({ "key": "ctrl+v" }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["undo"], "duplicate_clip");
    assert_eq!(clips_on(&state, other), vec![(3.0, 1.0)]);

    // Ctrl-dragged half a second on: the copy can't go over the clip, so
    // it goes right after it.
    let dragged = dawkey
        .call(
            "ui_drag",
            json!({ "id": format!("clip/{clip}"), "dx": 40, "modifiers": { "ctrl": true } }),
        )
        .await
        .unwrap();
    let copy = dragged["view"]["selected_clip"].as_u64().unwrap();
    assert_ne!(copy, clip);
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(clips_on(&state, track), vec![(1.0, 1.0), (2.0, 1.0)]);

    // Ctrl-dragging the copy's end 2 s on repeats it twice.
    dawkey
        .call(
            "ui_drag",
            json!({ "id": format!("clip/{copy}/end"), "dx": 160, "modifiers": { "ctrl": true } }),
        )
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["undo"], "repeat_clip");
    assert_eq!(
        clips_on(&state, track),
        vec![(1.0, 1.0), (2.0, 1.0), (3.0, 1.0), (4.0, 1.0)]
    );
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn take_lanes_over_mcp() {
    let dir = tempfile::tempdir().unwrap();
    let input = write_wav(dir.path(), "input.wav", slow_triangle(6 * 48_000));
    let dawkey = start().await;
    dawkey
        .call("rename_project", json!({ "name": "Takes" }))
        .await
        .unwrap();
    let track = dawkey
        .call("add_track", json!({ "name": "Vox" }))
        .await
        .unwrap()["track"]
        .clone();
    dawkey
        .call("ui_click", json!({ "id": format!("track/{track}/arm") }))
        .await
        .unwrap();
    dawkey
        .call(
            "ui_click",
            json!({ "id": format!("track/{track}/monitor") }),
        )
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tracks"][0]["monitor"], true);

    // Two and a half passes over a 2 s loop: three takes.
    dawkey
        .call(
            "set_loop",
            json!({ "start_s": 0.0, "end_s": 2.0, "enabled": true }),
        )
        .await
        .unwrap();
    dawkey.call("record", json!({})).await.unwrap();
    dawkey
        .call(
            "advance_clock",
            json!({ "seconds": 5.0, "input_path": input }),
        )
        .await
        .unwrap();
    dawkey.call("stop", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let clips = state["project"]["tracks"][0]["clips"]
        .as_array()
        .unwrap()
        .clone();
    let lanes: Vec<_> = clips
        .iter()
        .map(|c| c["lane"].as_u64().unwrap_or(0))
        .collect();
    assert_eq!(lanes, [0, 1, 2]);
    let ids: Vec<u64> = clips.iter().map(|c| c["id"].as_u64().unwrap()).collect();

    // The takes toggle shows the lanes under the track.
    let t = tree(&dawkey).await;
    assert!(find(&t["root"], &format!("track/{track}/take/0")).is_none());
    dawkey
        .call("ui_click", json!({ "id": format!("track/{track}/takes") }))
        .await
        .unwrap();
    let t = tree(&dawkey).await;
    for k in 0..3 {
        assert!(find(&t["root"], &format!("track/{track}/take/{k}/lane")).is_some());
    }
    let header = find(&t["root"], &format!("track/{track}/take/2")).unwrap();
    assert_eq!(header["value"], "playing");
    assert!(find(&t["root"], &format!("take/{}", ids[0])).is_some());
    // The track's lane shows only what plays.
    assert!(find(&t["root"], &format!("clip/{}", ids[0])).is_none());
    assert!(find(&t["root"], &format!("clip/{}", ids[2])).is_some());

    // Clicking a take plays it, instead of what it overlaps.
    dawkey
        .call("ui_click", json!({ "id": format!("take/{}", ids[0]) }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["undo"], "comp_take");
    let muted = |state: &Value| -> Vec<(u64, u64, bool)> {
        let mut v: Vec<_> = state["project"]["tracks"][0]["clips"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["lane"].as_u64().unwrap_or(0),
                    c["start"].as_u64().unwrap(),
                    c["muted"].as_bool().unwrap(),
                )
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(muted(&state), [(0, 0, false), (1, 0, true), (2, 0, true)]);
    dawkey.call("undo", json!({})).await.unwrap();

    // Swiping the middle take from 1 s to 1.5 s plays it there.
    let (x, _, w, _) = bounds(&dawkey, &format!("take/{}", ids[1])).await;
    assert!((w - 160.0).abs() < 1.0, "2 s at 80 px/s: {w}");
    let lane = bounds(&dawkey, &format!("track/{track}/take/1/lane")).await;
    dawkey
        .call(
            "ui_drag",
            json!({ "x": x + 80.0, "y": lane.1 + 20.0, "dx": 40.0, "dy": 0.0 }),
        )
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["undo"], "comp_take");
    assert_eq!(
        muted(&state),
        [
            (0, 0, true),
            (1, 0, true),
            (1, 48_000, false),
            (1, 72_000, true),
            (2, 0, false)
        ]
    );
    // The header of take 2 says it partly plays.
    let t = tree(&dawkey).await;
    let header = find(&t["root"], &format!("track/{track}/take/1")).unwrap();
    assert_eq!(header["value"], "partly playing");

    dawkey
        .call("locate", json!({ "position_s": 1.25 }))
        .await
        .unwrap();
    dawkey
        .call("ui_resize", json!({ "width": 900, "height": 460 }))
        .await
        .unwrap();
    assert_golden(&dawkey, "take_lanes", (900, 460)).await;

    // Clicking take 1's header plays it throughout, and nothing else.
    dawkey
        .call("ui_click", json!({ "id": format!("track/{track}/take/0") }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let playing: Vec<_> = muted(&state).into_iter().filter(|c| !c.2).collect();
    assert_eq!(playing, [(0, 0, false)]);
    dawkey.shutdown().await.unwrap();
}
