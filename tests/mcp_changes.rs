// SPDX-License-Identifier: AGPL-3.0-or-later
//! Telling what changed the project outside a client's own calls: the
//! instance ID in results, get_changes, and the project resource's updates.

use dawkey_testkit::Harness;
use serde_json::json;

const DAWKEY: &str = env!("CARGO_BIN_EXE_dawkey");
const TOKEN: &str = "changes-test-token";

#[tokio::test]
async fn instance_and_changes() {
    let dawkey = Harness::headless(DAWKEY, std::iter::empty::<&str>())
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let instance = state["instance"].as_str().unwrap().to_owned();
    assert!(!instance.is_empty());
    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    assert_eq!(status["instance"], instance);
    assert_eq!(status["revision"], 0);

    // Nothing has happened yet.
    let none = dawkey
        .call("get_changes", json!({ "since_revision": 0 }))
        .await
        .unwrap();
    assert_eq!(none["changes"], json!([]));
    assert_eq!(none["complete"], true);
    assert_eq!(none["instance"], instance);

    let added = dawkey
        .call(
            "add_midi_clip",
            json!({ "notes": [{ "pitch": 60, "start_beats": 0, "length_beats": 1 }] }),
        )
        .await
        .unwrap();
    assert_eq!(added["instance"], instance);
    assert_eq!(added["revision"], 1);
    let (track, clip) = (
        added["track"].as_u64().unwrap(),
        added["clip"].as_u64().unwrap(),
    );
    let note = added["notes"][0].as_u64().unwrap();
    dawkey
        .call("set_track_gain", json!({ "track": track, "gain_db": -3 }))
        .await
        .unwrap();
    let edited = dawkey
        .call(
            "edit_midi_notes",
            json!({ "clip": clip, "notes": [{ "id": note, "pitch": 62 }] }),
        )
        .await
        .unwrap();
    assert_eq!(edited["revision"], 3);
    dawkey
        .call("set_tempo", json!({ "bpm": 100 }))
        .await
        .unwrap();
    let undone = dawkey.call("undo", json!({})).await.unwrap();
    assert_eq!(undone["revision"], 5);
    assert_eq!(undone["instance"], instance);

    let changes = dawkey
        .call("get_changes", json!({ "since_revision": 1 }))
        .await
        .unwrap();
    assert_eq!(changes["since_revision"], 1);
    assert_eq!(changes["revision"], 5);
    assert_eq!(changes["complete"], true);
    assert_eq!(
        changes["changes"],
        json!([
            { "revision": 2, "kind": "command", "label": "set_track_gain", "tracks": [track] },
            { "revision": 3, "kind": "command", "label": "edit_midi_notes", "tracks": [track], "clips": [clip], "notes": [note] },
            // A tempo change re-times the MIDI clip, so it touches its note.
            { "revision": 4, "kind": "command", "label": "set_tempo", "also": ["tempo"],
              "tracks": [track], "clips": [clip], "notes": [note] },
            { "revision": 5, "kind": "undo", "label": "set_tempo", "also": ["tempo"],
              "tracks": [track], "clips": [clip], "notes": [note] },
        ])
    );
    let first = dawkey
        .call("get_changes", json!({ "since_revision": 0 }))
        .await
        .unwrap();
    assert_eq!(first["changes"][0]["label"], "add_midi_clip");
    assert_eq!(first["changes"][0]["notes"], json!([note]));
    assert_eq!(first["changes"][0]["clips"], json!([clip]));
    assert_eq!(first["changes"].as_array().unwrap().len(), 5);

    // A revision from another life of the project isn't covered.
    let stale = dawkey
        .call("get_changes", json!({ "since_revision": 50 }))
        .await
        .unwrap();
    assert_eq!(stale["complete"], false);
    assert_eq!(stale["changes"], json!([]));
    dawkey
        .call("new_project", json!({ "discard_changes": true }))
        .await
        .unwrap();
    // Another project carries the revisions on, so the old project's
    // revisions aren't covered, however many changes the new one makes.
    let fresh = dawkey
        .call("get_changes", json!({ "since_revision": 5 }))
        .await
        .unwrap();
    assert_eq!(fresh["revision"], 6);
    assert_eq!(fresh["complete"], false);
    for n in 0..5 {
        dawkey
            .call("add_track", json!({ "name": format!("New {n}") }))
            .await
            .unwrap();
    }
    let since_old = dawkey
        .call("get_changes", json!({ "since_revision": 5 }))
        .await
        .unwrap();
    assert_eq!(since_old["changes"].as_array().unwrap().len(), 5);
    assert_eq!(since_old["complete"], false, "{since_old}");
    let since_new = dawkey
        .call("get_changes", json!({ "since_revision": 6 }))
        .await
        .unwrap();
    assert_eq!(since_new["complete"], true);
    dawkey.shutdown().await.unwrap();
}

/// A client that subscribes to the project resource is told when another
/// client changes the project, but not of its own changes.
#[tokio::test]
async fn project_resource_updates() {
    let dawkey = Harness::headless_http(DAWKEY, std::iter::empty::<&str>(), TOKEN)
        .await
        .unwrap();
    let url = dawkey.url().unwrap().to_owned();
    let watcher = Harness::connect_http(&url, TOKEN).await.unwrap();

    let resources = watcher.resources().await.unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].uri, "dawkey://project");
    let project = watcher.read_resource("dawkey://project").await.unwrap();
    assert_eq!(project["project"]["name"], "Untitled");
    assert_eq!(project["revision"], 0);
    assert!(project["instance"].is_string());

    watcher.subscribe("dawkey://project").await.unwrap();
    // It must have seen a revision to be told of a later one.
    watcher
        .call("get_state", json!({ "summary": true }))
        .await
        .unwrap();
    // Its own change: no notice.
    watcher
        .call("add_track", json!({ "name": "Mine" }))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    assert_eq!(watcher.resource_updates(), Vec::<String>::new());

    // Another client's change: one notice, however many calls.
    dawkey
        .call("add_track", json!({ "name": "Theirs" }))
        .await
        .unwrap();
    dawkey
        .call("add_track", json!({ "name": "Theirs too" }))
        .await
        .unwrap();
    let mut updates = Vec::new();
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        updates.extend(watcher.resource_updates());
        if !updates.is_empty() {
            break;
        }
    }
    assert_eq!(updates, ["dawkey://project"]);
    // Nothing more until it changes again.
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    assert_eq!(watcher.resource_updates(), Vec::<String>::new());
    let changes = watcher
        .call("get_changes", json!({ "since_revision": 1 }))
        .await
        .unwrap();
    assert_eq!(changes["changes"].as_array().unwrap().len(), 2);
    assert_eq!(changes["changes"][1]["label"], "add_track");

    // The one that didn't subscribe is never told.
    assert_eq!(dawkey.resource_updates(), Vec::<String>::new());
    watcher.shutdown().await.unwrap();
    dawkey.shutdown().await.unwrap();
}
