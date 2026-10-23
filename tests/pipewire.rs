// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests against a real PipeWire daemon. Ignored by default because CI has no
//! daemon; run them on a desktop session with
//! `cargo test --test pipewire -- --ignored --test-threads 1`.
//!
//! The transport is never started and MIDI tracks are muted before they're
//! armed, so nothing audible is played.

use std::process::Command;
use std::time::{Duration, Instant};

use dawkey_testkit::Harness;
use serde_json::{Value, json};

const DAWKEY: &str = env!("CARGO_BIN_EXE_dawkey");

async fn wait_for_state(dawkey: &Harness, wanted: &[&str]) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
        let state = status["pipewire"]["state"].as_str().unwrap_or_default();
        if wanted.contains(&state) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "PipeWire node never reached {wanted:?}: {status}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Finds Dawkey's node and its ports in `pw-dump` output.
fn pw_dump_node(node_id: u64) -> (Value, Vec<Value>) {
    let out = Command::new("pw-dump").output().expect("pw-dump runs");
    let objects: Vec<Value> = serde_json::from_slice(&out.stdout).expect("pw-dump JSON");
    let node = objects
        .iter()
        .find(|o| o["id"] == node_id)
        .cloned()
        .expect("node in pw-dump");
    let ports = objects
        .into_iter()
        .filter(|o| {
            o["type"] == "PipeWire:Interface:Port" && o["info"]["props"]["node.id"] == node_id
        })
        .collect();
    (node, ports)
}

#[tokio::test]
#[ignore = "needs a running PipeWire daemon"]
async fn registers_a_node_with_its_ports() {
    // Without autoconnect nothing links the node, so it stays paused.
    let dawkey = Harness::headless(DAWKEY, ["--audio", "pipewire", "--no-autoconnect"])
        .await
        .unwrap();
    let status = wait_for_state(&dawkey, &["paused"]).await;
    assert_eq!(status["backend"], "pipewire");
    assert_eq!(status["pipewire"]["linked_to"], Value::Null);
    let node_id = status["pipewire"]["node_id"].as_u64().expect("node id");

    let (node, ports) = pw_dump_node(node_id);
    let props = &node["info"]["props"];
    assert_eq!(props["node.name"], "dawkey");
    assert_eq!(props["media.class"], "Stream/Output/Audio");
    assert_eq!(props["media.role"], "Production");
    let mut names: Vec<&str> = ports
        .iter()
        .map(|p| p["info"]["props"]["port.name"].as_str().unwrap())
        .collect();
    names.sort();
    let mut expected = vec![
        "input_FL".to_owned(),
        "input_FR".into(),
        "midi_in".into(),
        "output_FL".into(),
        "output_FR".into(),
    ];
    // MIDI device ports, assigned to devices as needed.
    for n in 1..=8 {
        expected.push(format!("midi_in_{n}"));
        expected.push(format!("midi_out_{n}"));
    }
    expected.sort();
    assert_eq!(names, expected);
    let midi = ports
        .iter()
        .find(|p| p["info"]["props"]["port.name"] == "midi_in")
        .unwrap();
    assert_eq!(midi["info"]["props"]["format.dsp"], "8 bit raw midi");
    assert_eq!(midi["info"]["direction"], "input");
    // Nothing is armed, so nothing is captured.
    assert_eq!(status["capturing"], false);
    assert_eq!(status["midi_capturing"], false);
    assert_eq!(status["pipewire"]["capture_from"], Value::Null);
    assert_eq!(status["pipewire"]["midi_from"], json!([]));

    // The paused node never runs the engine. Edits must keep working anyway
    // (graph updates are coalesced, not queued without bound).
    for _ in 0..150 {
        dawkey.call("add_track", json!({})).await.unwrap();
    }
    dawkey
        .call("new_project", json!({ "discard_changes": true }))
        .await
        .unwrap();

    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "needs a running PipeWire daemon; briefly links a silent stream to the default sink"]
async fn links_to_the_default_sink_and_runs() {
    let dawkey = Harness::headless(DAWKEY, ["--audio", "pipewire"])
        .await
        .unwrap();
    let status = wait_for_state(&dawkey, &["streaming"]).await;
    let sink = status["pipewire"]["linked_to"]
        .as_str()
        .expect("linked to a sink")
        .to_owned();
    let default = Command::new("pw-metadata")
        .args(["0", "default.audio.sink"])
        .output()
        .expect("pw-metadata runs");
    assert!(
        String::from_utf8_lossy(&default.stdout).contains(&sink),
        "linked to {sink}, which is not the default sink"
    );

    // The data thread is calling the engine.
    let first = dawkey.call("get_engine_status", json!({})).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let second = dawkey.call("get_engine_status", json!({})).await.unwrap();
    assert!(
        second["cycles"].as_u64() > first["cycles"].as_u64(),
        "{first} -> {second}"
    );
    let pw = &second["pipewire"];
    assert!(pw["quantum"].as_u64().unwrap() > 0, "{pw}");
    assert!(pw["graph_rate"].as_u64().unwrap() > 0, "{pw}");
    assert_eq!(second["transport"]["playing"], false);

    dawkey.shutdown().await.unwrap();
}

/// MIDI output ports of MIDI bridge devices, other than "Midi Through",
/// from `pw-dump`.
fn midi_device_ports() -> Vec<u64> {
    let out = Command::new("pw-dump").output().expect("pw-dump runs");
    let objects: Vec<Value> = serde_json::from_slice(&out.stdout).expect("pw-dump JSON");
    let bridges: Vec<u64> = objects
        .iter()
        .filter(|o| {
            o["info"]["props"]["media.class"]
                .as_str()
                .is_some_and(|c| c.starts_with("Midi/Bridge"))
        })
        .filter_map(|o| o["id"].as_u64())
        .collect();
    objects
        .iter()
        .filter(|o| o["type"] == "PipeWire:Interface:Port")
        .filter(|o| {
            let props = &o["info"]["props"];
            props["node.id"]
                .as_u64()
                .is_some_and(|n| bridges.contains(&n))
                && props["port.direction"] == "out"
                && props["format.dsp"]
                    .as_str()
                    .is_some_and(|f| f.contains("midi"))
                && !props["port.alias"]
                    .as_str()
                    .unwrap_or_default()
                    .starts_with("Midi Through")
        })
        .filter_map(|o| o["id"].as_u64())
        .collect()
}

/// Links from `ports` to `node`'s input ports, from `pw-dump`.
fn links_into(node: u64, ports: &[u64]) -> usize {
    let out = Command::new("pw-dump").output().expect("pw-dump runs");
    let objects: Vec<Value> = serde_json::from_slice(&out.stdout).expect("pw-dump JSON");
    objects
        .iter()
        .filter(|o| o["type"] == "PipeWire:Interface:Link")
        .filter(|o| {
            o["info"]["input-node-id"] == node
                && o["info"]["output-port-id"]
                    .as_u64()
                    .is_some_and(|p| ports.contains(&p))
        })
        .count()
}

#[tokio::test]
#[ignore = "needs a running PipeWire daemon; links MIDI devices to a muted track"]
async fn arming_a_midi_track_links_midi_devices() {
    let devices = midi_device_ports();
    if devices.is_empty() {
        eprintln!("no MIDI devices; nothing to link");
        return;
    }
    let dawkey = Harness::headless(DAWKEY, ["--audio", "pipewire"])
        .await
        .unwrap();
    let status = wait_for_state(&dawkey, &["paused", "streaming"]).await;
    let node = status["pipewire"]["node_id"].as_u64().unwrap();
    let track = dawkey
        .call("add_track", json!({ "kind": "midi" }))
        .await
        .unwrap()["track"]
        .clone();
    // Muted, so a key pressed during the test isn't heard.
    dawkey
        .call("set_track_mute", json!({ "track": track, "mute": true }))
        .await
        .unwrap();
    dawkey
        .call("set_track_arm", json!({ "track": track, "armed": true }))
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while links_into(node, &devices) < devices.len() {
        assert!(Instant::now() < deadline, "MIDI devices were never linked");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    assert_eq!(status["midi_capturing"], true);
    assert_eq!(
        status["pipewire"]["midi_from"].as_array().unwrap().len(),
        devices.len(),
        "{status}"
    );
    // The audio source is left alone.
    assert_eq!(status["capturing"], false);
    // Recorded MIDI from each device is placed earlier by at least a
    // cycle, more where the device reports its latency.
    eprintln!(
        "input latency: {}, record latency: {}",
        status["midi_input_latency_ms"], status["midi_record_latency_ms"]
    );
    for device in status["midi_inputs"].as_array().unwrap() {
        let device = device.as_str().unwrap();
        let ms = status["midi_record_latency_ms"][device].as_f64();
        assert!(ms.is_some_and(|ms| ms > 0.0), "{device}: {status}");
    }

    dawkey
        .call("set_track_arm", json!({ "track": track, "armed": false }))
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while links_into(node, &devices) > 0 {
        assert!(Instant::now() < deadline, "MIDI links were never removed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    dawkey.shutdown().await.unwrap();
}

/// Links from `node`'s output ports, from `pw-dump`.
fn links_out_of(node: u64) -> usize {
    let out = Command::new("pw-dump").output().expect("pw-dump runs");
    let objects: Vec<Value> = serde_json::from_slice(&out.stdout).expect("pw-dump JSON");
    objects
        .iter()
        .filter(|o| o["type"] == "PipeWire:Interface:Link")
        .filter(|o| o["info"]["output-node-id"] == node)
        .count()
}

#[tokio::test]
#[ignore = "needs a running PipeWire daemon; links a MIDI device to a track's output, sends nothing"]
async fn a_track_output_links_to_its_midi_device() {
    let dawkey = Harness::headless(DAWKEY, ["--audio", "pipewire"])
        .await
        .unwrap();
    let status = wait_for_state(&dawkey, &["paused", "streaming"]).await;
    let node = status["pipewire"]["node_id"].as_u64().unwrap();
    // The router lists devices once it has seen them.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    let Some(device) = status["midi_outputs"][0].as_str().map(str::to_owned) else {
        eprintln!("no MIDI output devices; nothing to link");
        dawkey.shutdown().await.unwrap();
        return;
    };
    let base = links_out_of(node);
    let track = dawkey
        .call("add_track", json!({ "kind": "midi" }))
        .await
        .unwrap()["track"]
        .clone();
    dawkey
        .call(
            "set_track_midi_output",
            json!({ "track": track, "device": device }),
        )
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while links_out_of(node) <= base {
        assert!(
            Instant::now() < deadline,
            "the MIDI device was never linked"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (_, ports) = pw_dump_node(node);
    assert!(
        ports
            .iter()
            .any(|p| p["info"]["props"]["port.alias"] == device.as_str()),
        "a port named after the device"
    );
    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    assert_eq!(status["pipewire"]["midi_to"], json!([device]));

    dawkey
        .call("set_track_midi_output", json!({ "track": track }))
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while links_out_of(node) > base {
        assert!(Instant::now() < deadline, "the MIDI link was never removed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    dawkey.shutdown().await.unwrap();
}
