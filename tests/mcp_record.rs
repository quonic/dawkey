// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recording over MCP, with the virtual backend: a WAV file is fed to the
//! inputs (advance_clock's input_path), so takes can be checked sample for
//! sample. Nothing touches a real microphone.

use std::f32::consts::FRAC_1_SQRT_2;
use std::path::{Path, PathBuf};

use dk_testkit::Harness;
use serde_json::{Value, json};

const DAWKEY: &str = env!("CARGO_BIN_EXE_dawkey");
const RATE: usize = 48_000;

async fn start() -> Harness {
    Harness::headless(DAWKEY, std::iter::empty::<&str>())
        .await
        .expect("dawkey starts")
}

/// A ramp-like signal whose value identifies the frame: sample i is
/// (i % 1000) / 1000 - 0.5.
fn signal(frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| (i % 1000) as f32 / 1000.0 - 0.5)
        .collect()
}

fn write_input(dir: &Path, frames: usize) -> PathBuf {
    let path = dir.join("input.wav");
    dk_media::write_wav(&path, &[signal(frames)], RATE as u32).unwrap();
    path
}

/// Adds an audio track and arms it.
async fn armed_track(dawkey: &Harness, input: &str) -> u64 {
    let track = dawkey
        .call("add_track", json!({ "name": "Vox" }))
        .await
        .unwrap()["track"]
        .as_u64()
        .unwrap();
    dawkey
        .call("set_track_input", json!({ "track": track, "input": input }))
        .await
        .unwrap();
    dawkey
        .call("set_track_arm", json!({ "track": track, "armed": true }))
        .await
        .unwrap();
    track
}

/// Records `seconds` of `input` from the current position.
async fn record(dawkey: &Harness, input: &Path, seconds: f64) -> Value {
    dawkey.call("record", json!({})).await.unwrap();
    dawkey
        .call(
            "advance_clock",
            json!({ "seconds": seconds, "input_path": input }),
        )
        .await
        .unwrap();
    dawkey.call("stop", json!({})).await.unwrap()
}

fn clips(state: &Value, track: u64) -> Vec<Value> {
    state["project"]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == track)
        .unwrap()["clips"]
        .as_array()
        .unwrap()
        .clone()
}

fn source_path(state: &Value, source: &Value) -> PathBuf {
    let s = state["project"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == *source)
        .unwrap();
    PathBuf::from(s["path"].as_str().unwrap())
}

/// The left channel of a recorded file.
fn recorded(path: &Path) -> Vec<f32> {
    let decoded = dk_media::decode_file(path, RATE as u32).unwrap();
    decoded.audio.channel(0).to_vec()
}

#[tokio::test]
async fn records_input_sample_for_sample() {
    let dir = tempfile::tempdir().unwrap();
    let input = write_input(dir.path(), RATE);
    let dawkey = start().await;
    let track = armed_track(&dawkey, "left").await;
    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    assert_eq!(status["capturing"], true);

    let stopped = record(&dawkey, &input, 1.0).await;
    assert_eq!(stopped["recorded"]["dropped_frames"], 0);
    assert_eq!(stopped["recording"], false);

    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["undo"], "record");
    let clips = clips(&state, track);
    assert_eq!(clips.len(), 1);
    assert_eq!(
        (clips[0]["start"].as_u64(), clips[0]["length"].as_u64()),
        (Some(0), Some(48_000))
    );
    assert_eq!(clips[0]["muted"], false);
    let path = source_path(&state, &clips[0]["source"]);
    assert_eq!(path.file_name().unwrap(), "Vox take 1.wav");
    assert_eq!(
        recorded(&path),
        signal(RATE),
        "the take is the input, exactly"
    );

    // One undo removes the take.
    dawkey.call("undo", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert!(self::clips(&state, track).is_empty());
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn punch_records_only_inside_the_range() {
    let dir = tempfile::tempdir().unwrap();
    let input = write_input(dir.path(), RATE);
    let dawkey = start().await;
    let track = armed_track(&dawkey, "stereo").await;
    dawkey
        .call(
            "set_punch",
            json!({ "start_s": 0.25, "end_s": 0.5, "enabled": true }),
        )
        .await
        .unwrap();
    record(&dawkey, &input, 1.0).await;

    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let clips = clips(&state, track);
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0]["start"], 12_000);
    assert_eq!(clips[0]["length"], 12_000);
    let path = source_path(&state, &clips[0]["source"]);
    let decoded = dk_media::decode_file(&path, RATE as u32).unwrap();
    assert_eq!(decoded.audio.channel_count(), 2);
    assert_eq!(decoded.audio.channel(0), &signal(RATE)[12_000..24_000]);
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn loop_recording_makes_takes() {
    let dir = tempfile::tempdir().unwrap();
    let input = write_input(dir.path(), 2 * RATE);
    let dawkey = start().await;
    let track = armed_track(&dawkey, "left").await;
    dawkey
        .call(
            "set_loop",
            json!({ "start_s": 0.0, "end_s": 0.5, "enabled": true }),
        )
        .await
        .unwrap();
    let stopped = record(&dawkey, &input, 1.25).await;
    assert_eq!(stopped["recorded"]["clips"].as_array().unwrap().len(), 3);

    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let clips = clips(&state, track);
    let summary: Vec<(u64, u64, bool)> = clips
        .iter()
        .map(|c| {
            (
                c["start"].as_u64().unwrap(),
                c["length"].as_u64().unwrap(),
                c["muted"].as_bool().unwrap(),
            )
        })
        .collect();
    // Three passes over 0–0.5 s; the earlier two are muted.
    assert_eq!(
        summary,
        [(0, 24_000, true), (0, 24_000, true), (0, 12_000, false)]
    );
    let second = recorded(&source_path(&state, &clips[1]["source"]));
    assert_eq!(second, &signal(2 * RATE)[24_000..48_000]);

    // Only the last take is heard.
    let render = dawkey
        .call("render_range", json!({ "start_s": 0.3, "end_s": 0.5 }))
        .await
        .unwrap();
    assert_eq!(render["analysis"]["silent"], true);
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn latency_offset_shifts_takes_earlier() {
    let dir = tempfile::tempdir().unwrap();
    let input = write_input(dir.path(), RATE);
    let dawkey = start().await;
    let track = armed_track(&dawkey, "left").await;
    let offset = dawkey
        .call("set_record_offset", json!({ "ms": 10 }))
        .await
        .unwrap();
    assert_eq!(offset["record_latency_ms"], 10.0);
    dawkey
        .call("locate", json!({ "position_s": 0.5 }))
        .await
        .unwrap();
    record(&dawkey, &input, 0.5).await;
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let clips = clips(&state, track);
    assert_eq!(clips[0]["start"], 24_000 - 480);
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn recording_needs_an_armed_track() {
    let dawkey = start().await;
    let err = dawkey.call_err("record", json!({})).await.unwrap();
    assert!(err.contains("no track is armed"), "{err}");
    // An armed MIDI track connects MIDI, not the audio source.
    let midi = dawkey
        .call("add_track", json!({ "kind": "midi" }))
        .await
        .unwrap()["track"]
        .clone();
    dawkey
        .call("set_track_arm", json!({ "track": midi, "armed": true }))
        .await
        .unwrap();
    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    assert_eq!(status["capturing"], false);
    assert_eq!(status["midi_capturing"], true);
    let err = dawkey
        .call_err("set_track_input", json!({ "track": midi, "input": "left" }))
        .await
        .unwrap();
    assert!(err.contains("MIDI track"), "{err}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn saved_projects_record_into_the_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let input = write_input(dir.path(), RATE);
    let dawkey = start().await;
    let track = armed_track(&dawkey, "left").await;
    let saved = dawkey
        .call("save_project", json!({ "path": dir.path().join("Song") }))
        .await
        .unwrap();
    let bundle = PathBuf::from(saved["path"].as_str().unwrap());
    record(&dawkey, &input, 0.25).await;
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let path = source_path(&state, &clips(&state, track)[0]["source"]);
    assert!(path.starts_with(bundle.join("audio")), "{}", path.display());
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn armed_tracks_meter_and_monitor_their_input() {
    let dir = tempfile::tempdir().unwrap();
    let input = write_input(dir.path(), RATE);
    let dawkey = start().await;
    let track = armed_track(&dawkey, "left").await;
    // The input peaks at 0.5.
    let peak = 20.0 * 0.5f64.log10();
    let advance = || async {
        dawkey
            .call(
                "advance_clock",
                json!({ "seconds": 0.1, "input_path": input }),
            )
            .await
            .unwrap()
    };

    // Armed: the meter shows the input, but nothing plays.
    dawkey.call("get_meters", json!({})).await.unwrap();
    let out = advance().await;
    assert_eq!(out["output"]["silent"], true);
    let meters = dawkey.call("get_meters", json!({})).await.unwrap();
    let level = meters["tracks"][0]["peak_dbfs"][0].as_f64().unwrap();
    assert!((level - peak).abs() < 0.01, "{meters}");

    // Monitoring: the input plays, even stopped. The left input is mono,
    // panned with equal power.
    dawkey
        .call(
            "set_track_monitor",
            json!({ "track": track, "monitor": true }),
        )
        .await
        .unwrap();
    let out = advance().await;
    assert_eq!(out["transport"]["playing"], false);
    let level = out["output"]["peak_dbfs"][1].as_f64().unwrap();
    assert!((level - (peak - 3.0103)).abs() < 0.01, "{out}");

    // It's saved with the track, and one undo step.
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tracks"][0]["monitor"], true);
    dawkey.call("undo", json!({})).await.unwrap();
    assert_eq!(advance().await["output"]["silent"], true);

    // Only audio tracks have it.
    let midi = dawkey
        .call("add_track", json!({ "kind": "midi" }))
        .await
        .unwrap()["track"]
        .clone();
    let err = dawkey
        .call_err(
            "set_track_monitor",
            json!({ "track": midi, "monitor": true }),
        )
        .await
        .unwrap();
    assert!(err.contains("MIDI track"), "{err}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn recording_over_a_take_stacks_and_comps() {
    let dir = tempfile::tempdir().unwrap();
    let input = write_input(dir.path(), RATE);
    let dawkey = start().await;
    let track = armed_track(&dawkey, "left").await;
    record(&dawkey, &input, 1.0).await;
    // A second take over 0.25–0.5 s, punched in.
    dawkey
        .call(
            "set_punch",
            json!({ "start_s": 0.25, "end_s": 0.5, "enabled": true }),
        )
        .await
        .unwrap();
    dawkey
        .call("locate", json!({ "position_s": 0.0 }))
        .await
        .unwrap();
    record(&dawkey, &input, 1.0).await;

    // (start, length, muted, lane), by start then lane.
    let summary = |state: &Value| {
        let mut v: Vec<(u64, u64, bool, u64)> = clips(state, track)
            .iter()
            .map(|c| {
                (
                    c["start"].as_u64().unwrap(),
                    c["length"].as_u64().unwrap(),
                    c["muted"].as_bool().unwrap(),
                    c["lane"].as_u64().unwrap_or(0),
                )
            })
            .collect();
        v.sort_by_key(|c| (c.0, c.3));
        v
    };
    // The first take goes silent where the second plays, and stays whole
    // around it.
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["undo"], "record");
    assert_eq!(
        summary(&state),
        [
            (0, 12_000, false, 0),
            (12_000, 12_000, true, 0),
            (12_000, 12_000, false, 1),
            (24_000, 24_000, false, 0),
        ]
    );
    // What plays is the first take with the second punched in: the input,
    // since both recorded the same signal at the same times.
    let render = dawkey
        .call("render_range", json!({ "start_s": 0.0, "end_s": 1.0 }))
        .await
        .unwrap();
    let rendered = dawkey
        .call(
            "render_range",
            json!({ "start_s": 0.0, "end_s": 1.0, "output_path": dir.path().join("out.wav") }),
        )
        .await
        .unwrap();
    assert_eq!(render["analysis"]["hash"], rendered["analysis"]["hash"]);
    let left = recorded(&dir.path().join("out.wav"));
    let expected: Vec<f32> = signal(RATE).iter().map(|s| s * FRAC_1_SQRT_2).collect();
    assert!(
        left.iter()
            .zip(&expected)
            .all(|(a, b)| (a - b).abs() < 1e-6),
        "the comp plays the input"
    );

    // Comping the first take back over the punch: one undo step.
    let first_middle = clips(&state, track)
        .into_iter()
        .find(|c| c["start"] == 12_000 && c["lane"].as_u64().unwrap_or(0) == 0)
        .unwrap()["id"]
        .clone();
    let comped = dawkey
        .call("comp_take", json!({ "clip": first_middle }))
        .await
        .unwrap();
    assert_eq!(comped["clip"], first_middle);
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["undo"], "comp_take");
    let muted: Vec<_> = summary(&state).into_iter().filter(|c| c.2).collect();
    assert_eq!(muted, [(12_000, 12_000, true, 1)]);
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn takes_of_unsaved_projects_are_deleted_with_them() {
    let dir = tempfile::tempdir().unwrap();
    let input = write_input(dir.path(), RATE);
    let dawkey = start().await;
    let track = armed_track(&dawkey, "left").await;
    record(&dawkey, &input, 0.25).await;
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let take = source_path(&state, &clips(&state, track)[0]["source"]);
    assert!(take.exists());
    assert!(
        take.components().any(|c| c.as_os_str() == "recordings"),
        "{}",
        take.display()
    );

    dawkey
        .call("new_project", json!({ "discard_changes": true }))
        .await
        .unwrap();
    assert!(!take.exists(), "{} is still there", take.display());

    // Saving copies takes into the project and deletes the cached ones.
    let track = armed_track(&dawkey, "left").await;
    record(&dawkey, &input, 0.25).await;
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let take = source_path(&state, &clips(&state, track)[0]["source"]);
    let saved = dawkey
        .call("save_project", json!({ "path": dir.path().join("Song") }))
        .await
        .unwrap();
    assert!(!take.exists());
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let bundle = PathBuf::from(saved["path"].as_str().unwrap());
    let copy = bundle.join(source_path(&state, &clips(&state, track)[0]["source"]));
    assert!(copy.exists(), "{}", copy.display());

    // And quitting deletes the takes of a project that was never saved.
    dawkey
        .call("new_project", json!({ "discard_changes": true }))
        .await
        .unwrap();
    let track = armed_track(&dawkey, "left").await;
    record(&dawkey, &input, 0.25).await;
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let take = source_path(&state, &clips(&state, track)[0]["source"]);
    assert!(take.exists());
    dawkey.shutdown().await.unwrap();
    assert!(!take.exists());
}
