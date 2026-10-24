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
