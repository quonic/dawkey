// SPDX-License-Identifier: AGPL-3.0-or-later
//! Timeline positions in beats and bars over MCP: every position in seconds
//! can be given as `*_beats` or `*_bar`, and get_state reports beats.

use dawkey_testkit::Harness;
use serde_json::json;

const DAWKEY: &str = env!("CARGO_BIN_EXE_dawkey");

async fn start() -> Harness {
    Harness::headless(DAWKEY, std::iter::empty::<&str>())
        .await
        .expect("dawkey starts")
}

#[tokio::test]
async fn positions_in_beats_and_bars() {
    let dawkey = start().await;
    // 120 bpm, 3/4: a beat is 0.5 s and a bar 1.5 s.
    dawkey
        .call("set_tempo", json!({ "bpm": 120 }))
        .await
        .unwrap();
    dawkey
        .call(
            "set_time_signature",
            json!({ "numerator": 3, "denominator": 4 }),
        )
        .await
        .unwrap();

    // The tool list offers the alternatives.
    let tools = dawkey.tools().await.unwrap();
    let set_loop = tools.iter().find(|t| t.name == "set_loop").unwrap();
    let props = set_loop.input_schema["properties"].as_object().unwrap();
    for key in [
        "start_s",
        "start_beats",
        "start_bar",
        "end_beats",
        "end_bar",
    ] {
        assert!(props.contains_key(key), "{key}");
    }
    let render = tools.iter().find(|t| t.name == "render_range").unwrap();
    assert!(render.input_schema["properties"]["end_bar"].is_object());

    // A clip from bar 2 for two beats.
    let added = dawkey
        .call(
            "add_midi_clip",
            json!({
                "position_bar": 2,
                "length_beats": 2,
                "notes": [{ "pitch": 60, "start_beats": 1, "length_beats": 0.5 }],
            }),
        )
        .await
        .unwrap();
    let clip = added["clip"].as_u64().unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let clip_state = &state["project"]["tracks"][0]["clips"][0];
    assert_eq!(clip_state["start"], 72_000); // 1.5 s at 48 kHz
    assert_eq!(clip_state["length"], 48_000);
    assert_eq!(clip_state["start_beats"], 3.0);
    assert_eq!(clip_state["end_beats"], 5.0);
    assert_eq!(clip_state["start_bar"], 2.0);
    let note = &clip_state["notes"][0];
    assert_eq!(note["start"], 24_000);
    assert_eq!(note["start_beats"], 1.0);
    assert_eq!(note["length_beats"], 0.5);

    // Halfway through bar 3 is beat 7.5: 3.75 s.
    dawkey
        .call("move_clip", json!({ "clip": clip, "position_bar": 3.5 }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tracks"][0]["clips"][0]["start"], 180_000);

    // The loop in beats, reported in beats too.
    dawkey
        .call(
            "set_loop",
            json!({ "start_beats": 3, "end_bar": 4, "enabled": true }),
        )
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let range = &state["project"]["loop"]["range"];
    assert_eq!(range["start"], 72_000);
    assert_eq!(range["end"], 216_000);
    assert_eq!(range["start_beats"], 3.0);
    assert_eq!(range["end_beats"], 9.0);

    // Exactly two bars rendered: bar 2 to the start of bar 4.
    let rendered = dawkey
        .call("render_range", json!({ "start_bar": 2, "end_bar": 4 }))
        .await
        .unwrap();
    assert_eq!(rendered["start_s"], 1.5);
    assert_eq!(rendered["end_s"], 4.5);
    assert_eq!(rendered["analysis"]["frames"], 144_000);

    // A tempo change later on counts: at 60 bpm from beat 6, beat 8 is a
    // second later than it would be.
    dawkey
        .call("set_tempo", json!({ "bpm": 60, "at_beats": 6 }))
        .await
        .unwrap();
    let rendered = dawkey
        .call("render_range", json!({ "start_beats": 6, "end_beats": 8 }))
        .await
        .unwrap();
    assert_eq!(rendered["start_s"], 3.0);
    assert_eq!(rendered["end_s"], 5.0);

    // Bad input is a tool error that names the argument.
    for (tool, args, expected) in [
        (
            "set_loop",
            json!({ "start_s": 1, "start_beats": 2, "end_s": 3 }),
            "not both",
        ),
        (
            "move_clip",
            json!({ "clip": clip, "position_bar": 0 }),
            "at least 1",
        ),
        (
            "move_clip",
            json!({ "clip": clip, "position_beats": -1 }),
            "at least 0",
        ),
        (
            "move_clip",
            json!({ "clip": clip, "position_beats": "x" }),
            "number",
        ),
    ] {
        let err = dawkey.call_err(tool, args).await.unwrap();
        assert!(err.contains(expected), "{tool}: {err}");
    }
    dawkey.shutdown().await.unwrap();
}

/// get_state can leave out notes, controls, automation points and tracks,
/// and open_project returns a summary.
#[tokio::test]
async fn smaller_states() {
    let dir = tempfile::tempdir().unwrap();
    let dawkey = start().await;
    let added = dawkey
        .call(
            "add_midi_clip",
            json!({
                "notes": [
                    { "pitch": 60, "start_beats": 0, "length_beats": 1 },
                    { "pitch": 72, "start_beats": 1, "length_beats": 1 },
                ],
                "controls": [{ "controller": { "kind": "pitch_bend" }, "time_beats": 0.5, "value": 8192 }],
            }),
        )
        .await
        .unwrap();
    let midi = added["track"].as_u64().unwrap();
    let other = dawkey
        .call("add_track", json!({ "name": "Other" }))
        .await
        .unwrap();
    let other = other["track"].as_u64().unwrap();
    dawkey
        .call(
            "set_automation",
            json!({ "track": midi, "target": { "kind": "gain" }, "points": [
                { "time_s": 0, "value": 0 }, { "time_s": 1, "value": -6 }
            ] }),
        )
        .await
        .unwrap();

    let full = dawkey.call("get_state", json!({})).await.unwrap();
    assert!(full.get("summary").is_none());
    let clip = &full["project"]["tracks"][0]["clips"][0];
    assert_eq!(clip["notes"].as_array().unwrap().len(), 2);
    assert_eq!(full["project"]["tracks"].as_array().unwrap().len(), 2);

    let summary = dawkey
        .call("get_state", json!({ "summary": true, "tracks": [midi] }))
        .await
        .unwrap();
    let tracks = summary["project"]["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 1, "{other} left out");
    let clip = &tracks[0]["clips"][0];
    assert!(clip.get("notes").is_none());
    assert_eq!(clip["note_count"], 2);
    assert_eq!(clip["pitch_range"], json!([60, 72]));
    assert_eq!(clip["control_count"], 1);
    assert!(clip.get("controls").is_none());
    assert_eq!(tracks[0]["automation"][0]["point_count"], 2);
    assert!(tracks[0]["automation"][0].get("points").is_none());
    assert_eq!(
        summary["summary"]["left_out"],
        json!(["notes", "controls", "automation_points", "other_tracks"])
    );
    assert_eq!(summary["project"]["master"]["id"], 0);

    // Just the notes left out: controls stay.
    let no_notes = dawkey
        .call("get_state", json!({ "notes": false }))
        .await
        .unwrap();
    let clip = &no_notes["project"]["tracks"][0]["clips"][0];
    assert_eq!(clip["note_count"], 2);
    assert_eq!(clip["controls"].as_array().unwrap().len(), 1);
    assert_eq!(no_notes["summary"]["left_out"], json!(["notes"]));

    // open_project and new_project return summaries.
    let saved = dawkey
        .call("save_project", json!({ "path": dir.path().join("Small") }))
        .await
        .unwrap();
    let fresh = dawkey.call("new_project", json!({})).await.unwrap();
    assert_eq!(fresh["summary"]["left_out"].as_array().unwrap().len(), 3);
    let opened = dawkey
        .call("open_project", json!({ "path": saved["path"] }))
        .await
        .unwrap();
    assert_eq!(opened["project"]["tracks"][0]["clips"][0]["note_count"], 2);
    dawkey.shutdown().await.unwrap();
}

/// set_clip_length's `length_beats` is at the tempo where the clip starts,
/// not at the project's start, also in a batch.
#[tokio::test]
async fn clip_length_in_beats_at_the_clips_tempo() {
    const RATE: u64 = 48_000;
    let dawkey = start().await;
    // 120 bpm, then 60 bpm from beat 8 (4 s).
    dawkey
        .call("set_tempo", json!({ "bpm": 120 }))
        .await
        .unwrap();
    dawkey
        .call("set_tempo", json!({ "bpm": 60, "at_beats": 8 }))
        .await
        .unwrap();
    let added = dawkey
        .call(
            "add_midi_clip",
            json!({
                "position_beats": 8,
                "length_beats": 4,
                "notes": [{ "pitch": 60, "start_beats": 0, "length_beats": 1 }],
            }),
        )
        .await
        .unwrap();
    let clip = added["clip"].as_u64().unwrap();
    let length =
        |state: &serde_json::Value| state["project"]["tracks"][0]["clips"][0]["length"].clone();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(length(&state), 4 * RATE);

    // 2 beats at 60 bpm are 2 s.
    dawkey
        .call(
            "set_clip_length",
            json!({ "clip": clip, "length_beats": 2 }),
        )
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(length(&state), 2 * RATE, "{state}");

    dawkey
        .call(
            "batch",
            json!({ "operations": [
                { "tool": "set_clip_length", "args": { "clip": clip, "length_beats": 3 } },
            ] }),
        )
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(length(&state), 3 * RATE, "{state}");
    dawkey.shutdown().await.unwrap();
}
