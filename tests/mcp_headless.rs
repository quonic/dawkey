// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end: drive `dawkey mcp --headless` over stdio like an MCP client.

use dawkey_testkit::Harness;
use serde_json::{Value, json};

const DAWKEY: &str = env!("CARGO_BIN_EXE_dawkey");

async fn start() -> Harness {
    Harness::headless(DAWKEY, std::iter::empty::<&str>())
        .await
        .expect("dawkey starts")
}

fn track_names(state: &Value) -> Vec<&str> {
    state["project"]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn lists_generated_tools() {
    let dawkey = start().await;
    assert_eq!(dawkey.server_name().as_deref(), Some("dawkey"));

    let tools = dawkey.tools().await.unwrap();
    let add = tools.iter().find(|t| t.name == "add_track").unwrap();
    assert!(
        add.description
            .as_deref()
            .unwrap()
            .starts_with("Add a new track")
    );
    assert_eq!(add.input_schema["type"], "object");
    assert_eq!(
        add.input_schema["properties"]["kind"]["enum"],
        json!(["audio", "midi", "bus"])
    );
    let annotations = add.annotations.as_ref().unwrap();
    assert_eq!(annotations.destructive_hint, Some(false));

    let remove = tools.iter().find(|t| t.name == "remove_track").unwrap();
    assert_eq!(
        remove.annotations.as_ref().unwrap().destructive_hint,
        Some(true)
    );

    let state = tools.iter().find(|t| t.name == "get_state").unwrap();
    assert_eq!(
        state.annotations.as_ref().unwrap().read_only_hint,
        Some(true)
    );

    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn edit_undo_redo() {
    let dawkey = start().await;

    let added = dawkey
        .call("add_track", json!({ "name": "Drums" }))
        .await
        .unwrap();
    let drums = added["track"].as_u64().unwrap();
    dawkey
        .call("add_track", json!({ "kind": "midi" }))
        .await
        .unwrap();
    dawkey
        .call("set_track_gain", json!({ "track": drums, "gain_db": -6.0 }))
        .await
        .unwrap();

    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(track_names(&state), ["Drums", "MIDI 1"]);
    assert_eq!(state["project"]["tracks"][0]["gain_db"], -6.0);
    assert_eq!(state["dirty"], true);
    assert_eq!(state["undo"], "set_track_gain");

    // One tool call is one undo step.
    let undone = dawkey.call("undo", json!({})).await.unwrap();
    assert_eq!(undone["undone"], "set_track_gain");
    dawkey.call("undo", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(track_names(&state), ["Drums"]);
    assert_eq!(state["project"]["tracks"][0]["gain_db"], 0.0);

    let redone = dawkey.call("redo", json!({})).await.unwrap();
    assert_eq!(redone["redone"], "add_track");
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(track_names(&state), ["Drums", "MIDI 1"]);
    assert_eq!(state["project"]["tracks"][0]["id"], drums);

    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn rename_clips() {
    let dawkey = start().await;
    let added = dawkey
        .call(
            "add_midi_clip",
            json!({ "notes": [{ "pitch": 60, "start_beats": 0, "length_beats": 1 }] }),
        )
        .await
        .unwrap();
    let clip = added["clip"].as_u64().unwrap();
    let name = |state: &Value| state["project"]["tracks"][0]["clips"][0]["name"].clone();

    // Unnamed clips leave `name` out.
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(name(&state), Value::Null);
    dawkey
        .call("rename_clip", json!({ "clip": clip, "name": "Hook" }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(name(&state), "Hook");
    assert_eq!(state["undo"], "rename_clip");

    let err = dawkey
        .call_err("rename_clip", json!({ "clip": 999, "name": "x" }))
        .await
        .unwrap();
    assert!(err.contains("999"), "{err}");
    let err = dawkey
        .call_err("rename_clip", json!({ "clip": clip, "name": "a\u{7}b" }))
        .await
        .unwrap();
    assert!(err.contains("control characters"), "{err}");

    // An empty name goes back to the default.
    dawkey
        .call("rename_clip", json!({ "clip": clip, "name": "" }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(name(&state), Value::Null);
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn bad_input_is_a_readable_tool_error() {
    let dawkey = start().await;
    let drums = dawkey.call("add_track", json!({})).await.unwrap()["track"].clone();

    let err = dawkey
        .call_err("set_track_gain", json!({ "track": drums, "gain_db": 40.0 }))
        .await
        .unwrap();
    assert!(err.contains("gain_db must be between"), "{err}");

    let err = dawkey
        .call_err("remove_track", json!({ "track": 999 }))
        .await
        .unwrap();
    assert!(err.contains("track 999 does not exist"), "{err}");

    let err = dawkey
        .call_err("add_track", json!({ "colour": "red" }))
        .await
        .unwrap();
    assert!(err.contains("unknown field"), "{err}");

    // Failed calls don't create undo steps.
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["undo"], "add_track");
    assert_eq!(state["revision"], 1);

    assert!(dawkey.call_raw("no_such_tool", json!({})).await.is_err());
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn save_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Song");

    let dawkey = start().await;
    dawkey
        .call("rename_project", json!({ "name": "Song" }))
        .await
        .unwrap();
    dawkey
        .call("add_track", json!({ "name": "Vox" }))
        .await
        .unwrap();
    dawkey
        .call("set_tempo", json!({ "bpm": 87.5 }))
        .await
        .unwrap();

    // Unsaved changes are protected.
    let err = dawkey.call_err("new_project", json!({})).await.unwrap();
    assert!(err.contains("unsaved changes"), "{err}");

    let saved = dawkey
        .call("save_project", json!({ "path": path }))
        .await
        .unwrap();
    let bundle = saved["path"].as_str().unwrap().to_owned();
    assert!(bundle.ends_with("Song.dawkey"));

    dawkey.call("new_project", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert!(track_names(&state).is_empty());

    let opened = dawkey
        .call("open_project", json!({ "path": bundle }))
        .await
        .unwrap();
    assert_eq!(opened["project"]["name"], "Song");
    assert_eq!(opened["project"]["tempo_bpm"], 87.5);
    assert_eq!(track_names(&opened), ["Vox"]);
    assert_eq!(opened["dirty"], false);
    dawkey.shutdown().await.unwrap();

    // The project can also be opened from the command line.
    let dawkey = Harness::headless(DAWKEY, [&bundle]).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(track_names(&state), ["Vox"]);
    dawkey.shutdown().await.unwrap();
}
