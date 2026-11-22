// SPDX-License-Identifier: AGPL-3.0-or-later
//! The drum machine over MCP: the one-click track, kit/pattern presets, and
//! switching a kit's pads between synthesis and the bundled sample kit.

use dawkey_testkit::Harness;
use serde_json::json;

const DAWKEY: &str = env!("CARGO_BIN_EXE_dawkey");

async fn start() -> Harness {
    Harness::headless(DAWKEY, std::iter::empty::<&str>())
        .await
        .expect("dawkey starts")
}

async fn render(dawkey: &Harness, start_s: f64, end_s: f64) -> serde_json::Value {
    dawkey
        .call(
            "render_range",
            json!({ "start_s": start_s, "end_s": end_s }),
        )
        .await
        .unwrap()["analysis"]
        .clone()
}

#[tokio::test]
async fn set_drum_sample_kit_switches_pads_to_real_recordings() {
    let dawkey = start().await;
    let added = dawkey.call("add_drum_track", json!({})).await.unwrap();
    let track = added["track"].as_u64().unwrap();
    let clip = added["clip"].as_u64().unwrap();
    let plugin = dawkey
        .call("get_state", json!({ "summary": true }))
        .await
        .unwrap()["project"]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"].as_u64() == Some(track))
        .unwrap()["plugins"][0]["id"]
        .as_u64()
        .unwrap();

    dawkey
        .call(
            "add_midi_notes",
            json!({
                "clip": clip,
                "notes": [{ "pitch": 36, "start_beats": 0.0, "length_beats": 0.25 }],
            }),
        )
        .await
        .unwrap();

    let synth = render(&dawkey, 0.0, 1.0).await;
    assert!(!synth["silent"].as_bool().unwrap(), "{synth}");

    let details = dawkey
        .call(
            "set_drum_sample_kit",
            json!({ "plugin": plugin, "kit": "acoustic_vcsl" }),
        )
        .await
        .unwrap();
    assert_eq!(details["sample_kit"], "acoustic_vcsl");

    let sampled = render(&dawkey, 0.0, 1.0).await;
    assert!(!sampled["silent"].as_bool().unwrap(), "{sampled}");
    assert_ne!(
        sampled["hash"], synth["hash"],
        "a real recording should sound different from the synthesized kick"
    );

    // Reverting to synthesis is byte-identical to never having switched.
    dawkey
        .call(
            "set_drum_sample_kit",
            json!({ "plugin": plugin, "kit": null }),
        )
        .await
        .unwrap();
    let back = render(&dawkey, 0.0, 1.0).await;
    assert_eq!(back["hash"], synth["hash"]);

    let get_plugin = dawkey
        .call("get_plugin", json!({ "plugin": plugin }))
        .await
        .unwrap();
    assert!(get_plugin.get("sample_kit").is_none());
}

#[tokio::test]
async fn the_jazz_kit_sounds_different_from_both_synth_and_the_acoustic_kit() {
    let dawkey = start().await;
    let added = dawkey.call("add_drum_track", json!({})).await.unwrap();
    let track = added["track"].as_u64().unwrap();
    let clip = added["clip"].as_u64().unwrap();
    let plugin = dawkey
        .call("get_state", json!({ "summary": true }))
        .await
        .unwrap()["project"]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"].as_u64() == Some(track))
        .unwrap()["plugins"][0]["id"]
        .as_u64()
        .unwrap();
    dawkey
        .call(
            "add_midi_notes",
            json!({
                "clip": clip,
                "notes": [
                    { "pitch": 36, "start_beats": 0.0, "length_beats": 0.25 },
                    { "pitch": 51, "start_beats": 0.5, "length_beats": 0.25 },
                ],
            }),
        )
        .await
        .unwrap();
    let synth = render(&dawkey, 0.0, 1.0).await;

    dawkey
        .call(
            "set_drum_sample_kit",
            json!({ "plugin": plugin, "kit": "acoustic_vcsl" }),
        )
        .await
        .unwrap();
    let acoustic = render(&dawkey, 0.0, 1.0).await;

    dawkey
        .call(
            "set_drum_sample_kit",
            json!({ "plugin": plugin, "kit": "jazz_virtuosity" }),
        )
        .await
        .unwrap();
    let jazz = render(&dawkey, 0.0, 1.0).await;

    assert!(!jazz["silent"].as_bool().unwrap(), "{jazz}");
    assert_ne!(jazz["hash"], synth["hash"]);
    assert_ne!(jazz["hash"], acoustic["hash"]);
}

#[tokio::test]
async fn an_unknown_sample_kit_is_a_readable_tool_error() {
    let dawkey = start().await;
    let added = dawkey.call("add_drum_track", json!({})).await.unwrap();
    let track = added["track"].as_u64().unwrap();
    let plugin = dawkey
        .call("get_state", json!({ "summary": true }))
        .await
        .unwrap()["project"]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"].as_u64() == Some(track))
        .unwrap()["plugins"][0]["id"]
        .as_u64()
        .unwrap();

    let err = dawkey
        .call_err(
            "set_drum_sample_kit",
            json!({ "plugin": plugin, "kit": "not_a_real_kit" }),
        )
        .await
        .unwrap();
    assert!(
        err.contains("not_a_real_kit") || err.contains("kit"),
        "{err}"
    );
}

#[tokio::test]
async fn set_drum_sample_kit_refuses_a_non_drum_plugin() {
    let dawkey = start().await;
    let added = dawkey
        .call("add_track", json!({ "kind": "midi" }))
        .await
        .unwrap();
    let track = added["track"].as_u64().unwrap();
    let added = dawkey
        .call(
            "add_plugin",
            json!({ "track": track, "plugin": "builtin:comp" }),
        )
        .await
        .unwrap();
    let plugin = added["plugin"].as_u64().unwrap();

    let err = dawkey
        .call_err(
            "set_drum_sample_kit",
            json!({ "plugin": plugin, "kit": "acoustic_vcsl" }),
        )
        .await
        .unwrap();
    assert!(err.contains("drum"), "{err}");
}

#[tokio::test]
async fn patching_one_pad_leaves_a_lower_indexed_unpatched_pad_on_master() {
    // A regression test for the "gap": routing only the Snare (output 1)
    // still allocates an aux buffer for the Kick (output 0) to size the
    // array, which must NOT make the Kick's audio go there too (silently
    // dropped) instead of staying on the track's own output to Master.
    let dawkey = start().await;
    let added = dawkey.call("add_drum_track", json!({})).await.unwrap();
    let track = added["track"].as_u64().unwrap();
    let clip = added["clip"].as_u64().unwrap();
    let plugin = dawkey
        .call("get_state", json!({ "summary": true }))
        .await
        .unwrap()["project"]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"].as_u64() == Some(track))
        .unwrap()["plugins"][0]["id"]
        .as_u64()
        .unwrap();

    // Create the bus *before* the reference render, so the graph's
    // topology (and so its compensation delays) stay identical across
    // every render in this test — only the output routing changes. A
    // render taken before vs. after adding an unrelated track isn't
    // guaranteed bit-identical even for unrelated audio, since adding a
    // track can shift cross-track latency compensation.
    let bus = dawkey
        .call("add_track", json!({ "kind": "bus", "name": "Snare Bus" }))
        .await
        .unwrap()["track"]
        .as_u64()
        .unwrap();

    // The Kick alone, rendered on the track directly: the reference tone.
    dawkey
        .call(
            "add_midi_notes",
            json!({
                "clip": clip,
                "notes": [{ "pitch": 36, "start_beats": 0.0, "length_beats": 0.25 }],
            }),
        )
        .await
        .unwrap();
    let kick_alone = dawkey
        .call(
            "render_range",
            json!({ "start_s": 0.0, "end_s": 1.0, "tracks": [track] }),
        )
        .await
        .unwrap()["analysis"]
        .clone();
    assert!(!kick_alone["silent"].as_bool().unwrap());

    // Add the Snare and patch only it (output 1) to its own bus.
    dawkey
        .call(
            "add_midi_notes",
            json!({
                "clip": clip,
                "notes": [{ "pitch": 38, "start_beats": 0.0, "length_beats": 0.25 }],
            }),
        )
        .await
        .unwrap();
    dawkey
        .call(
            "set_plugin_output",
            json!({ "plugin": plugin, "output": 1, "bus": bus }),
        )
        .await
        .unwrap();

    // The track's own output must still sound like the Kick alone: the
    // Snare left, and the Kick (output 0, below the Snare's index) must
    // not have been swept away with it. A fresh aux route can shift
    // overall graph latency compensation by a sample or two (unrelated to
    // this feature), so compare levels with a tolerance rather than
    // requiring a bit-exact hash.
    let track_after = dawkey
        .call(
            "render_range",
            json!({ "start_s": 0.0, "end_s": 1.0, "tracks": [track] }),
        )
        .await
        .unwrap()["analysis"]
        .clone();
    assert!(!track_after["silent"].as_bool().unwrap(), "{track_after}");
    let before = kick_alone["peak_dbfs"][0].as_f64().unwrap();
    let after = track_after["peak_dbfs"][0].as_f64().unwrap();
    assert!(
        (before - after).abs() < 0.1,
        "the Kick's level should be unaffected by patching the Snare away: {before} vs {after}"
    );

    // And the Snare actually arrived at its bus.
    let bus_out = dawkey
        .call(
            "render_range",
            json!({ "start_s": 0.0, "end_s": 1.0, "tracks": [bus] }),
        )
        .await
        .unwrap()["analysis"]
        .clone();
    assert!(!bus_out["silent"].as_bool().unwrap(), "{bus_out}");
}
