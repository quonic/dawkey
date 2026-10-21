// SPDX-License-Identifier: AGPL-3.0-or-later
//! MIDI over MCP with the virtual backend: MIDI clips played by the
//! built-in synth, live input on armed tracks, and MIDI recording (fed
//! through advance_clock's midi_input, so no device is involved).

use dk_testkit::Harness;
use serde_json::{Value, json};

const DAWKEY: &str = env!("CARGO_BIN_EXE_dawkey");

/// Analysis hash of the chord in [`chord_clip`] rendered from 0 to 2 s with
/// the default synth. Changes to this value mean the synth's output changed.
const GOLDEN_CHORD_HASH: &str = "e6dde3ef5c0a2c5d";

async fn start() -> Harness {
    Harness::headless(DAWKEY, std::iter::empty::<&str>())
        .await
        .expect("dawkey starts")
}

/// A C major chord for a beat, then a G for a beat, at 0.5 s on a new MIDI
/// track. Returns (track, clip).
async fn chord_clip(dawkey: &Harness) -> (u64, u64) {
    let added = dawkey
        .call(
            "add_midi_clip",
            json!({
                "position_s": 0.5,
                "notes": [
                    { "pitch": 60, "start_beats": 0, "length_beats": 1 },
                    { "pitch": 64, "start_beats": 0, "length_beats": 1 },
                    { "pitch": 67, "start_beats": 0, "length_beats": 1, "velocity": 80 },
                    { "pitch": 67, "start_beats": 1, "length_beats": 1 },
                ],
            }),
        )
        .await
        .unwrap();
    assert_eq!(added["notes"].as_array().unwrap().len(), 4);
    (
        added["track"].as_u64().unwrap(),
        added["clip"].as_u64().unwrap(),
    )
}

async fn render(dawkey: &Harness, start_s: f64, end_s: f64) -> Value {
    dawkey
        .call(
            "render_range",
            json!({ "start_s": start_s, "end_s": end_s }),
        )
        .await
        .unwrap()["analysis"]
        .clone()
}

fn find_track(state: &Value, id: u64) -> &Value {
    state["project"]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .unwrap()
}

#[tokio::test]
async fn midi_tools_are_published() {
    let dawkey = start().await;
    let tools = dawkey.tools().await.unwrap();
    for name in [
        "add_midi_clip",
        "add_midi_notes",
        "edit_midi_notes",
        "remove_midi_notes",
        "quantize_midi_notes",
        "set_clip_length",
        "set_synth",
        "set_midi_controls",
        "set_tempo",
        "set_tempo_map",
        "remove_tempo_change",
        "import_midi",
    ] {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        assert!(
            tool.description.as_deref().is_some_and(|d| d.len() > 20),
            "{name} needs a description"
        );
    }
    let advance = tools.iter().find(|t| t.name == "advance_clock").unwrap();
    assert!(advance.input_schema["properties"]["midi_input"].is_object());
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn midi_clips_play_through_the_synth() {
    let dir = tempfile::tempdir().unwrap();
    let dawkey = start().await;
    let (track, clip) = chord_clip(&dawkey).await;

    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let t = find_track(&state, track);
    assert_eq!(t["kind"], "midi");
    assert_eq!(t["synth"]["waveform"], "saw");
    let c = &t["clips"][0];
    assert_eq!(c["kind"], "midi");
    // One 4/4 bar at 120 BPM, from 0.5 s.
    assert_eq!(
        (c["start"].as_u64(), c["length"].as_u64()),
        (Some(24_000), Some(96_000))
    );
    assert_eq!(c["notes"][3]["start"], 24_000);

    // Silent before the clip, sounding during it.
    assert_eq!(render(&dawkey, 0.0, 0.5).await["silent"], true);
    let a = render(&dawkey, 0.0, 2.0).await;
    assert_eq!(a["silent"], false);
    let peak = a["peak_dbfs"][0].as_f64().unwrap();
    assert!((-20.0..-3.0).contains(&peak), "{a}");
    assert_eq!(a["hash"], GOLDEN_CHORD_HASH, "synth output changed: {a}");

    // The note starts on its frame: write the render and look.
    let out = dir.path().join("chord.wav");
    dawkey
        .call(
            "render_range",
            json!({ "start_s": 0.0, "end_s": 1.0, "output_path": out }),
        )
        .await
        .unwrap();
    let left = dk_media::decode_file(&out, 48_000)
        .unwrap()
        .audio
        .channel(0)
        .to_vec();
    // The oscillators start at a zero crossing: frame 24 000 is silent,
    // 24 001 isn't.
    assert!(left[..24_001].iter().all(|&s| s == 0.0));
    assert!(left[24_001] != 0.0);

    // A different synth sound changes the render; undo restores it.
    dawkey
        .call(
            "set_synth",
            json!({ "track": track, "waveform": "square", "cutoff_hz": 400 }),
        )
        .await
        .unwrap();
    assert_ne!(render(&dawkey, 0.0, 2.0).await["hash"], a["hash"]);
    dawkey.call("undo", json!({})).await.unwrap();
    assert_eq!(render(&dawkey, 0.0, 2.0).await["hash"], a["hash"]);

    // Muting the clip silences it.
    dawkey
        .call("set_clip_mute", json!({ "clip": clip, "muted": true }))
        .await
        .unwrap();
    assert_eq!(render(&dawkey, 0.0, 2.0).await["silent"], true);
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn editing_notes_over_mcp() {
    let dawkey = start().await;
    let (_, clip) = chord_clip(&dawkey).await;
    let added = dawkey
        .call(
            "add_midi_notes",
            json!({ "clip": clip, "notes": [{ "pitch": 72, "start_beats": 5, "length_beats": 0.5 }] }),
        )
        .await
        .unwrap();
    let note = added["notes"][0].as_u64().unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let c = &state["project"]["tracks"][0]["clips"][0];
    assert_eq!(c["length"], 192_000, "grew to two bars");

    dawkey
        .call(
            "edit_midi_notes",
            json!({ "clip": clip, "notes": [{ "id": note, "pitch": 74, "start_beats": 4.1 }] }),
        )
        .await
        .unwrap();
    dawkey
        .call(
            "quantize_midi_notes",
            json!({ "clip": clip, "grid_beats": 0.5 }),
        )
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let notes = state["project"]["tracks"][0]["clips"][0]["notes"]
        .as_array()
        .unwrap()
        .clone();
    let moved = notes.iter().find(|n| n["id"] == note).unwrap();
    assert_eq!(
        (moved["pitch"].as_u64(), moved["start"].as_u64()),
        (Some(74), Some(96_000))
    );

    dawkey
        .call(
            "remove_midi_notes",
            json!({ "clip": clip, "notes": [note] }),
        )
        .await
        .unwrap();
    dawkey.call("undo", json!({})).await.unwrap();

    // Bad input is a tool error the caller can read.
    let err = dawkey
        .call_err(
            "add_midi_notes",
            json!({ "clip": clip, "notes": [{ "pitch": 200, "start_beats": 0, "length_beats": 1 }] }),
        )
        .await
        .unwrap();
    assert!(err.contains("pitch"), "{err}");
    let err = dawkey
        .call_err(
            "remove_midi_notes",
            json!({ "clip": clip, "notes": [9999] }),
        )
        .await
        .unwrap();
    assert!(err.contains("no note 9999"), "{err}");
    let audio = dawkey.call("add_track", json!({})).await.unwrap()["track"].clone();
    let err = dawkey
        .call_err("add_midi_clip", json!({ "track": audio }))
        .await
        .unwrap();
    assert!(err.contains("audio track"), "{err}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn armed_tracks_play_live_midi() {
    let dawkey = start().await;
    let track = dawkey
        .call("add_track", json!({ "kind": "midi" }))
        .await
        .unwrap()["track"]
        .as_u64()
        .unwrap();
    let key = json!([{ "at_s": 0.1, "pitch": 69, "length_s": 0.2 }]);

    // Not armed: the keyboard isn't heard.
    let out = dawkey
        .call(
            "advance_clock",
            json!({ "seconds": 0.5, "midi_input": key }),
        )
        .await
        .unwrap();
    assert_eq!(out["output"]["silent"], true);

    dawkey
        .call("set_track_arm", json!({ "track": track, "armed": true }))
        .await
        .unwrap();
    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    assert_eq!(status["midi_capturing"], true);
    assert_eq!(status["capturing"], false);
    let out = dawkey
        .call(
            "advance_clock",
            json!({ "seconds": 0.5, "midi_input": key }),
        )
        .await
        .unwrap();
    assert_eq!(out["output"]["silent"], false, "{out}");
    assert_eq!(out["transport"]["playing"], false);
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn records_midi_into_clips() {
    let dir = tempfile::tempdir().unwrap();
    let dawkey = start().await;
    let track = dawkey
        .call("add_track", json!({ "kind": "midi", "name": "Keys" }))
        .await
        .unwrap()["track"]
        .as_u64()
        .unwrap();
    dawkey
        .call("set_track_arm", json!({ "track": track, "armed": true }))
        .await
        .unwrap();
    dawkey
        .call("locate", json!({ "position_s": 1.0 }))
        .await
        .unwrap();
    dawkey.call("record", json!({})).await.unwrap();
    dawkey
        .call(
            "advance_clock",
            json!({
                "seconds": 1.0,
                "midi_input": [
                    { "at_s": 0.25, "pitch": 60, "length_s": 0.25, "velocity": 90 },
                    { "at_s": 0.5, "pitch": 64, "length_s": 1.0 },
                ],
            }),
        )
        .await
        .unwrap();
    let stopped = dawkey.call("stop", json!({})).await.unwrap();
    let clips = stopped["recorded"]["clips"].as_array().unwrap();
    assert_eq!(clips.len(), 1, "{stopped}");

    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let c = &find_track(&state, track)["clips"][0];
    assert_eq!(c["kind"], "midi");
    // From the record start (1 s) to the stop (2 s).
    assert_eq!(
        (c["start"].as_u64(), c["length"].as_u64()),
        (Some(48_000), Some(48_000))
    );
    let notes = c["notes"].as_array().unwrap();
    let summary: Vec<_> = notes
        .iter()
        .map(|n| {
            (
                n["pitch"].as_u64(),
                n["velocity"].as_u64(),
                n["start"].as_u64(),
                n["length"].as_u64(),
            )
        })
        .collect();
    // The second key was still down at the stop, so it ends there.
    assert_eq!(
        summary,
        [
            (Some(60), Some(90), Some(12_000), Some(12_000)),
            (Some(64), Some(100), Some(24_000), Some(24_000)),
        ]
    );
    // One undo step removes the take.
    dawkey.call("undo", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert!(
        find_track(&state, track)["clips"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    dawkey.call("redo", json!({})).await.unwrap();

    // Notes survive saving and reopening.
    let saved = dawkey
        .call("save_project", json!({ "path": dir.path().join("Keys") }))
        .await
        .unwrap();
    dawkey
        .call("open_project", json!({ "path": saved["path"] }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(
        state["project"]["tracks"][0]["clips"][0]["notes"],
        json!(notes)
    );
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn loop_recording_makes_midi_takes() {
    let dawkey = start().await;
    let track = dawkey
        .call("add_track", json!({ "kind": "midi" }))
        .await
        .unwrap()["track"]
        .as_u64()
        .unwrap();
    dawkey
        .call("set_track_arm", json!({ "track": track, "armed": true }))
        .await
        .unwrap();
    dawkey
        .call(
            "set_loop",
            json!({ "start_s": 0.0, "end_s": 1.0, "enabled": true }),
        )
        .await
        .unwrap();
    dawkey.call("record", json!({})).await.unwrap();
    // Two passes with a note each, then half a pass with none.
    dawkey
        .call(
            "advance_clock",
            json!({
                "seconds": 2.5,
                "midi_input": [
                    { "at_s": 0.1, "pitch": 60, "length_s": 0.1 },
                    { "at_s": 1.2, "pitch": 62, "length_s": 0.1 },
                ],
            }),
        )
        .await
        .unwrap();
    dawkey.call("stop", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let clips = find_track(&state, track)["clips"]
        .as_array()
        .unwrap()
        .clone();
    let summary: Vec<_> = clips
        .iter()
        .map(|c| (c["muted"].as_bool(), c["notes"][0]["pitch"].as_u64()))
        .collect();
    assert_eq!(summary, [(Some(true), Some(60)), (Some(false), Some(62))]);
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn tempo_changes_move_midi_clips() {
    let dawkey = start().await;
    let (track, clip) = chord_clip(&dawkey).await;
    // The clip starts at beat 1 and is 4 beats long. Half speed from beat
    // 2: beat 2 is at 1 s, then a beat lasts a second, so it ends at 4 s.
    dawkey
        .call("set_tempo", json!({ "bpm": 60, "at_beats": 2 }))
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(
        state["project"]["tempo_changes"],
        json!([{ "beat": 2.0, "bpm": 60.0 }])
    );
    let c = &find_track(&state, track)["clips"][0];
    assert_eq!(
        (c["start"].as_u64(), c["length"].as_u64()),
        (Some(24_000), Some(168_000))
    );
    // The G on beat 2 now starts a second after the clip's start beat.
    assert_eq!(c["notes"][3]["start"], 24_000);
    assert_eq!(c["notes"][3]["length"], 48_000);

    dawkey
        .call("remove_tempo_change", json!({ "at_beats": 2 }))
        .await
        .unwrap();
    let a = render(&dawkey, 0.0, 2.0).await;
    assert_eq!(a["hash"], GOLDEN_CHORD_HASH, "back where it was: {a}");

    // Bad changes are tool errors.
    let err = dawkey
        .call("set_tempo", json!({ "bpm": 60, "at_beats": -1 }))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("tempo change"), "{err}");
    let err = dawkey
        .call("remove_tempo_change", json!({ "at_beats": 3 }))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no tempo change"), "{err}");

    // Pitch bend changes what the synth plays.
    dawkey
        .call(
            "set_midi_controls",
            json!({
                "clip": clip,
                "controller": { "kind": "pitch_bend" },
                "points": [{ "time_beats": 0.5, "value": 16383 }],
            }),
        )
        .await
        .unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let controls = &find_track(&state, track)["clips"][0]["controls"];
    assert_eq!(
        controls,
        &json!([{ "controller": { "kind": "pitch_bend" }, "frame": 12_000, "value": 16383 }])
    );
    let bent = render(&dawkey, 0.0, 2.0).await;
    assert_eq!(bent["silent"], false);
    assert_ne!(bent["hash"], GOLDEN_CHORD_HASH);
    dawkey.shutdown().await.unwrap();
}

/// A two-track MIDI file at 100 BPM in 3/4: a tempo track, and "Lead" with
/// three quarter notes and a mod wheel move.
fn write_midi_file(path: &std::path::Path) {
    use midly::num::{u4, u7, u15, u24, u28};
    use midly::{
        Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind,
    };
    let event = |delta: u32, kind| TrackEvent {
        delta: u28::new(delta),
        kind,
    };
    let midi = |message| TrackEventKind::Midi {
        channel: u4::new(0),
        message,
    };
    let note = |key: u8, vel: u8| {
        midi(MidiMessage::NoteOn {
            key: u7::new(key),
            vel: u7::new(vel),
        })
    };
    let mut smf = Smf::new(Header::new(
        Format::Parallel,
        Timing::Metrical(u15::new(96)),
    ));
    smf.tracks.push(vec![
        event(
            0,
            TrackEventKind::Meta(MetaMessage::Tempo(u24::new(600_000))),
        ),
        event(
            0,
            TrackEventKind::Meta(MetaMessage::TimeSignature(3, 2, 24, 8)),
        ),
        // 5/8 from beat 6 (bar 3).
        event(
            576,
            TrackEventKind::Meta(MetaMessage::TimeSignature(5, 3, 24, 8)),
        ),
        event(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ]);
    smf.tracks.push(vec![
        event(0, TrackEventKind::Meta(MetaMessage::TrackName(b"Lead"))),
        event(0, note(60, 100)),
        event(96, note(60, 0)),
        event(0, note(64, 100)),
        event(
            48,
            midi(MidiMessage::Controller {
                controller: u7::new(1),
                value: u7::new(90),
            }),
        ),
        event(48, note(64, 0)),
        event(0, note(67, 100)),
        event(96, note(67, 0)),
        event(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ]);
    smf.save(path).unwrap();
}

#[tokio::test]
async fn imports_midi_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("song.mid");
    write_midi_file(&path);
    let dawkey = start().await;
    let import = dawkey
        .call("import_midi", json!({ "path": path }))
        .await
        .unwrap();
    assert_eq!(import["used_file_tempo"], true);
    assert_eq!(import["notes"], 3);
    let track = import["tracks"][0].as_u64().unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let project = &state["project"];
    assert_eq!(project["tempo_bpm"], 100.0);
    assert_eq!(
        project["time_signature"],
        json!({ "numerator": 3, "denominator": 4 })
    );
    assert_eq!(
        project["meter_changes"],
        json!([{ "bar": 3, "numerator": 5, "denominator": 8 }])
    );
    let t = find_track(&state, track);
    assert_eq!(t["name"], "Lead");
    // At 100 BPM a beat is 28 800 frames; the clip is one 3/4 bar.
    let c = &t["clips"][0];
    assert_eq!(c["length"], 86_400);
    assert_eq!(c["notes"][2]["start"], 57_600);
    assert_eq!(c["controls"][0]["frame"], 43_200);
    assert_eq!(render(&dawkey, 0.0, 2.0).await["silent"], false);

    // One undo step takes it all back, tempo included.
    dawkey.call("undo", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tempo_bpm"], 120.0);
    assert_eq!(state["project"]["tracks"], json!([]));
    assert!(state["project"].get("meter_changes").is_none());

    // Into a project with clips, it keeps the project's tempo.
    chord_clip(&dawkey).await;
    let import = dawkey
        .call("import_midi", json!({ "path": path, "position_s": 2.0 }))
        .await
        .unwrap();
    assert_eq!(import["used_file_tempo"], false);
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let c = &find_track(&state, import["tracks"][0].as_u64().unwrap())["clips"][0];
    assert_eq!(
        (c["start"].as_u64(), c["notes"][2]["start"].as_u64()),
        (Some(96_000), Some(48_000))
    );

    // Not a MIDI file: a tool error.
    let junk = dir.path().join("junk.mid");
    std::fs::write(&junk, b"nope").unwrap();
    let err = dawkey
        .call("import_midi", json!({ "path": junk }))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("MIDI"), "{err}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn tracks_choose_midi_devices() {
    let dawkey = start().await;
    let add = |name: &'static str| {
        let dawkey = &dawkey;
        async move {
            let track = dawkey
                .call("add_track", json!({ "kind": "midi", "name": name }))
                .await
                .unwrap()["track"]
                .as_u64()
                .unwrap();
            dawkey
                .call("set_track_arm", json!({ "track": track, "armed": true }))
                .await
                .unwrap();
            track
        }
    };
    let (keys, pads) = (add("Keys").await, add("Pads").await);
    for (track, device) in [(keys, "Keyboard"), (pads, "Pad controller")] {
        dawkey
            .call(
                "set_track_midi_input",
                json!({ "track": track, "device": device }),
            )
            .await
            .unwrap();
    }
    // Each device's notes land on its own track.
    dawkey.call("record", json!({})).await.unwrap();
    dawkey
        .call(
            "advance_clock",
            json!({
                "seconds": 1.0,
                "midi_input": [
                    { "at_s": 0.25, "pitch": 60, "length_s": 0.25, "device": "Keyboard" },
                    { "at_s": 0.5, "pitch": 36, "length_s": 0.25, "device": "Pad controller" },
                    { "at_s": 0.5, "pitch": 90, "length_s": 0.25, "device": "Something else" },
                ],
            }),
        )
        .await
        .unwrap();
    dawkey.call("stop", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let pitches = |track| -> Vec<u64> {
        find_track(&state, track)["clips"][0]["notes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["pitch"].as_u64().unwrap())
            .collect()
    };
    assert_eq!(pitches(keys), [60]);
    assert_eq!(pitches(pads), [36]);
    assert_eq!(find_track(&state, keys)["midi_input"], "Keyboard");

    // A track sending to a device plays nothing itself.
    dawkey
        .call(
            "set_track_midi_output",
            json!({ "track": keys, "device": "Synth", "channel": 2 }),
        )
        .await
        .unwrap();
    dawkey
        .call("set_track_mute", json!({ "track": pads, "mute": true }))
        .await
        .unwrap();
    dawkey
        .call("locate", json!({ "position_s": 0.0 }))
        .await
        .unwrap();
    dawkey.call("play", json!({})).await.unwrap();
    let out = dawkey
        .call("advance_clock", json!({ "seconds": 1.0 }))
        .await
        .unwrap();
    dawkey.call("stop", json!({})).await.unwrap();
    assert_eq!(out["output"]["silent"], true, "{out}");
    assert_eq!(
        out["midi_output"],
        json!([
            { "at_s": 0.25, "device": "Synth", "data": [0x91, 60, 100] },
            { "at_s": 0.5, "device": "Synth", "data": [0x81, 60, 0] },
        ])
    );
    let err = dawkey
        .call(
            "set_track_midi_output",
            json!({ "track": keys, "device": "Synth", "channel": 0 }),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("channel"), "{err}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn tracks_listen_to_one_channel() {
    let dawkey = start().await;
    let track = dawkey
        .call("add_track", json!({ "kind": "midi", "name": "Drums" }))
        .await
        .unwrap()["track"]
        .as_u64()
        .unwrap();
    dawkey
        .call("set_track_arm", json!({ "track": track, "armed": true }))
        .await
        .unwrap();
    dawkey
        .call(
            "set_track_midi_input",
            json!({ "track": track, "channel": 10 }),
        )
        .await
        .unwrap();
    dawkey.call("record", json!({})).await.unwrap();
    dawkey
        .call(
            "advance_clock",
            json!({
                "seconds": 1.0,
                "midi_input": [
                    { "at_s": 0.25, "pitch": 60, "length_s": 0.25 },
                    { "at_s": 0.5, "pitch": 36, "length_s": 0.25, "channel": 10 },
                ],
            }),
        )
        .await
        .unwrap();
    dawkey.call("stop", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let t = find_track(&state, track);
    assert_eq!(t["midi_input_channel"], 10);
    let notes = t["clips"][0]["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["pitch"], 36);
    let err = dawkey
        .call(
            "set_track_midi_input",
            json!({ "track": track, "channel": 17 }),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("channel"), "{err}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn per_note_controllers_over_mcp() {
    let dawkey = start().await;
    let (track, clip) = chord_clip(&dawkey).await;
    // Per-note volume 0 on the E silences just that note: the render is
    // the same as without it.
    let volume = json!({ "kind": "note_controller", "pitch": 64, "number": 7 });
    dawkey
        .call(
            "set_midi_controls",
            json!({ "clip": clip, "controller": volume,
                    "points": [{ "time_beats": 0, "value": 0 }] }),
        )
        .await
        .unwrap();
    let muted = render(&dawkey, 0.5, 1.5).await;
    dawkey.call("undo", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let e = find_track(&state, track)["clips"][0]["notes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["pitch"] == 64)
        .unwrap()["id"]
        .clone();
    dawkey
        .call("remove_midi_notes", json!({ "clip": clip, "notes": [e] }))
        .await
        .unwrap();
    let without = render(&dawkey, 0.5, 1.5).await;
    assert_eq!(muted["hash"], without["hash"], "{muted} vs {without}");
    assert_ne!(muted["hash"], GOLDEN_CHORD_HASH);

    // Live per-note bend is recorded as a clip controller.
    let keys = dawkey
        .call("add_track", json!({ "kind": "midi", "name": "Keys" }))
        .await
        .unwrap()["track"]
        .as_u64()
        .unwrap();
    dawkey
        .call("set_track_arm", json!({ "track": keys, "armed": true }))
        .await
        .unwrap();
    dawkey
        .call("locate", json!({ "position_s": 4.0 }))
        .await
        .unwrap();
    dawkey.call("record", json!({})).await.unwrap();
    let bend = json!({ "kind": "note_pitch_bend", "pitch": 60 });
    dawkey
        .call(
            "advance_clock",
            json!({
                "seconds": 1.0,
                "midi_input": [{ "at_s": 0.25, "pitch": 60, "length_s": 0.5 }],
                "midi_controls": [{ "at_s": 0.5, "controller": bend, "value": 40_000 }],
            }),
        )
        .await
        .unwrap();
    dawkey.call("stop", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let take = &find_track(&state, keys)["clips"][0];
    assert_eq!(
        take["controls"],
        json!([{ "controller": bend, "frame": 24_000, "value": 40_000 }]),
        "{take}"
    );

    // Sent to a device, it goes out as a MIDI 2.0 packet.
    dawkey
        .call("set_track_arm", json!({ "track": keys, "armed": false }))
        .await
        .unwrap();
    dawkey
        .call(
            "set_track_midi_output",
            json!({ "track": keys, "device": "Synth", "channel": 2 }),
        )
        .await
        .unwrap();
    dawkey
        .call("locate", json!({ "position_s": 4.25 }))
        .await
        .unwrap();
    dawkey.call("play", json!({})).await.unwrap();
    let out = dawkey
        .call("advance_clock", json!({ "seconds": 0.5 }))
        .await
        .unwrap();
    dawkey.call("stop", json!({})).await.unwrap();
    let sent = out["midi_output"].as_array().unwrap();
    let packet = sent.iter().find(|m| m["at_s"] == 0.25).unwrap();
    // 40 000 (0x9c40) scaled up to 32 bits, its low 15 bits repeated.
    assert_eq!(
        packet["ump"],
        json!([0x4061_3c00u32, 0x9c40_3880u32]),
        "{out}"
    );

    // Keys out of range are tool errors.
    let err = dawkey
        .call(
            "set_midi_controls",
            json!({ "clip": clip,
                    "controller": { "kind": "note_assignable", "pitch": 200, "number": 1 },
                    "points": [] }),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("pitch"), "{err}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn per_note_management_over_mcp() {
    let dawkey = start().await;
    let keys = dawkey
        .call("add_track", json!({ "kind": "midi", "name": "Keys" }))
        .await
        .unwrap()["track"]
        .as_u64()
        .unwrap();
    dawkey
        .call("set_track_arm", json!({ "track": keys, "armed": true }))
        .await
        .unwrap();
    // A bend on key 60, then a reset: recorded as the bend going back.
    dawkey.call("record", json!({})).await.unwrap();
    let bend = json!({ "kind": "note_pitch_bend", "pitch": 60 });
    dawkey
        .call(
            "advance_clock",
            json!({
                "seconds": 1.0,
                "midi_input": [{ "at_s": 0.25, "pitch": 60, "length_s": 0.5 }],
                "midi_controls": [{ "at_s": 0.5, "controller": bend, "value": 40_000 }],
                "midi_management": [{ "at_s": 0.625, "pitch": 60, "reset": true }],
            }),
        )
        .await
        .unwrap();
    dawkey.call("stop", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let take = &find_track(&state, keys)["clips"][0];
    assert_eq!(
        take["controls"],
        json!([
            { "controller": bend, "frame": 24_000, "value": 40_000 },
            { "controller": bend, "frame": 30_000, "value": 0x8000 },
        ]),
        "{take}"
    );

    // Playing into a track with a device, it goes out as a MIDI 2.0 packet.
    dawkey
        .call(
            "set_track_midi_output",
            json!({ "track": keys, "device": "Synth", "channel": 2 }),
        )
        .await
        .unwrap();
    let out = dawkey
        .call(
            "advance_clock",
            json!({
                "seconds": 0.1,
                "midi_management": [{ "at_s": 0.05, "pitch": 62, "detach": true }],
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        out["midi_output"],
        json!([{ "at_s": 0.05, "device": "Synth", "data": [0x71, 62, 2],
                 "ump": [0x40f1_3e02u32, 0] }]),
        "{out}"
    );
    let err = dawkey
        .call(
            "advance_clock",
            json!({ "seconds": 0.1, "midi_management": [{ "at_s": 0, "pitch": 128 }] }),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("pitch"), "{err}");
    dawkey.shutdown().await.unwrap();
}
