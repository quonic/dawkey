// SPDX-License-Identifier: AGPL-3.0-or-later
//! describe_audio and view_audio over MCP: listening to a small song made
//! with the built-in synth (a bass line and chords, Am F C G at 120 BPM).

use base64::Engine as _;
use dawkey_testkit::{Harness, ToolResult};
use serde_json::{Value, json};

const DAWKEY: &str = env!("CARGO_BIN_EXE_dawkey");

async fn start() -> Harness {
    Harness::headless(DAWKEY, std::iter::empty::<&str>())
        .await
        .expect("dawkey starts")
}

/// Four bars: a bass line on one MIDI track and triads on another.
/// Returns (bass track, chords track).
async fn song(dawkey: &Harness) -> (u64, u64) {
    dawkey
        .call("set_tempo", json!({ "bpm": 120 }))
        .await
        .unwrap();
    let bass: Vec<Value> = [45, 41, 48, 43]
        .iter()
        .enumerate()
        .map(|(bar, pitch)| json!({ "pitch": pitch, "start_beats": bar * 4, "length_beats": 4 }))
        .collect();
    let bass = dawkey
        .call("add_midi_clip", json!({ "position_s": 0, "notes": bass }))
        .await
        .unwrap()["track"]
        .as_u64()
        .unwrap();
    let chords: Vec<Value> = [[57, 60, 64], [53, 57, 60], [60, 64, 67], [55, 59, 62]]
        .iter()
        .enumerate()
        .flat_map(|(bar, triad)| {
            triad.iter().map(move |pitch| {
                json!({ "pitch": pitch, "start_beats": bar * 4, "length_beats": 4, "velocity": 70 })
            })
        })
        .collect();
    let chords = dawkey
        .call("add_midi_clip", json!({ "position_s": 0, "notes": chords }))
        .await
        .unwrap()["track"]
        .as_u64()
        .unwrap();
    dawkey
        .call("rename_track", json!({ "track": bass, "name": "Bass" }))
        .await
        .unwrap();
    dawkey
        .call("rename_track", json!({ "track": chords, "name": "Chords" }))
        .await
        .unwrap();
    (bass, chords)
}

fn image(result: &ToolResult) -> (u32, u32, String) {
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let image = result
        .content
        .iter()
        .find_map(|c| c.as_image())
        .expect("image content");
    assert_eq!(image.mime_type, "image/png");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&image.data)
        .unwrap();
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let reader = decoder.read_info().unwrap();
    let (w, h) = (reader.info().width, reader.info().height);
    let caption = result
        .content
        .iter()
        .find_map(|c| c.as_text())
        .map(|t| t.text.clone())
        .expect("a caption");
    (w, h, caption)
}

#[tokio::test]
async fn tools_are_listed_as_read_only() {
    let dawkey = start().await;
    let tools = dawkey.tools().await.unwrap();
    for name in ["describe_audio", "view_audio"] {
        let tool = tools.iter().find(|t| t.name == name).expect(name);
        assert_eq!(
            tool.annotations.as_ref().and_then(|a| a.read_only_hint),
            Some(true)
        );
        let schema = Value::Object((*tool.input_schema).clone());
        // Positions also come in bars and beats.
        assert!(schema["properties"]["start_bar"].is_object(), "{schema}");
        assert!(schema["properties"]["end_beats"].is_object(), "{schema}");
    }
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn describes_a_bass_line_and_chords() {
    let dawkey = start().await;
    let (bass, chords) = song(&dawkey).await;

    let heard = dawkey
        .call(
            "describe_audio",
            json!({ "start_bar": 1, "end_bar": 5, "tracks": [bass] }),
        )
        .await
        .unwrap();
    let notes: Vec<&str> = heard["notes"]["list"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["pitch"].as_str().unwrap())
        .collect();
    assert_eq!(notes, ["A2", "F2", "C3", "G2"], "{heard:#}");
    let first = &heard["notes"]["list"][1];
    assert!(
        (first["bar"].as_f64().unwrap() - 2.0).abs() < 0.02,
        "{first}"
    );
    // One bar per segment.
    let timeline = heard["timeline"].as_array().unwrap();
    assert_eq!(timeline.len(), 4, "{heard:#}");
    assert_eq!(timeline[2]["bar"], 3.0);
    assert_eq!(timeline[2]["notes"], "C3");

    let heard = dawkey
        .call(
            "describe_audio",
            json!({ "start_bar": 1, "end_bar": 5, "stems": true, "notes": false }),
        )
        .await
        .unwrap();
    let stems = heard["stems"].as_array().unwrap();
    assert_eq!(stems.len(), 2, "{heard:#}");
    let chords_stem = stems
        .iter()
        .find(|s| s["track"].as_u64() == Some(chords))
        .unwrap();
    assert_eq!(chords_stem["name"], "Chords");
    let heard_chords: Vec<Option<&str>> = chords_stem["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["chord"].as_str())
        .collect();
    assert_eq!(
        heard_chords,
        [Some("Am"), Some("F"), Some("C"), Some("G")],
        "{chords_stem:#}"
    );
    assert!(chords_stem.get("notes").is_none());
    let observations = chords_stem["observations"].as_array().unwrap();
    assert!(
        observations
            .iter()
            .any(|o| o.as_str().unwrap().contains("Am → F → C → G")),
        "{observations:#?}"
    );
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn hears_the_pulse_against_the_project() {
    let dawkey = start().await;
    dawkey
        .call("set_tempo", json!({ "bpm": 120 }))
        .await
        .unwrap();
    // A short note on every beat for four bars.
    let notes: Vec<Value> = (0..16)
        .map(|beat| json!({ "pitch": 72, "start_beats": beat, "length_beats": 0.25 }))
        .collect();
    let track = dawkey
        .call("add_midi_clip", json!({ "position_s": 0, "notes": notes }))
        .await
        .unwrap()["track"]
        .clone();
    let heard = dawkey
        .call(
            "describe_audio",
            json!({ "end_bar": 5, "tracks": [track], "onsets": true }),
        )
        .await
        .unwrap();
    let rhythm = &heard["rhythm"];
    assert_eq!(rhythm["onsets"], 16, "{rhythm:#}");
    assert_eq!(rhythm["list"].as_array().unwrap().len(), 16);
    assert_eq!(rhythm["tempo"]["project_bpm"], 120.0);
    let bpm = rhythm["tempo"]["bpm"].as_f64().unwrap();
    assert!(
        [60.0, 120.0, 240.0].iter().any(|b| (bpm - b).abs() < 2.0),
        "{rhythm:#}"
    );
    // The built-in synth plays on the grid.
    let offset = rhythm["timing"]["median_offset_ms"].as_f64().unwrap();
    assert!(offset.abs() < 5.0, "{rhythm:#}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn draws_stems_and_files() {
    let dawkey = start().await;
    song(&dawkey).await;

    let result = dawkey
        .call_raw(
            "view_audio",
            json!({ "start_bar": 1, "end_bar": 5, "stems": true }),
        )
        .await
        .unwrap();
    let (w, h, caption) = image(&result);
    assert_eq!(w, 1200);
    // Three panels: the mix and two stems.
    assert!(h > 3 * 280, "{h}");
    assert!(caption.starts_with("Spectrogram"), "{caption}");
    assert!(
        caption.contains("Mix, Track 1: Bass, Track 2: Chords"),
        "{caption}"
    );

    let result = dawkey
        .call_raw(
            "view_audio",
            json!({ "end_bar": 5, "stems": true, "kind": "spectrum", "with_mix": false,
                    "min_hz": 30, "max_hz": 2000, "width": 800 }),
        )
        .await
        .unwrap();
    let (w, _, caption) = image(&result);
    assert_eq!(w, 800);
    assert!(caption.contains("Track 1: Bass = cyan"), "{caption}");

    let result = dawkey
        .call_raw("view_audio", json!({ "end_bar": 5, "kind": "chroma" }))
        .await
        .unwrap();
    let (_, _, caption) = image(&result);
    assert!(caption.starts_with("Chromagram"), "{caption}");

    // A file: render the song to one, then look at it and describe it.
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("song.wav");
    dawkey
        .call(
            "render_range",
            json!({ "start_s": 0, "end_s": 8, "output_path": wav }),
        )
        .await
        .unwrap();
    let result = dawkey
        .call_raw("view_audio", json!({ "path": wav, "end_s": 4 }))
        .await
        .unwrap();
    let (_, _, caption) = image(&result);
    assert!(caption.contains("song.wav"), "{caption}");
    let heard = dawkey
        .call("describe_audio", json!({ "path": wav, "notes": false }))
        .await
        .unwrap();
    assert_eq!(heard["end_s"], 8.0);
    assert!(heard["path"].as_str().unwrap().ends_with("song.wav"));
    // Off the grid: no bars, and up to 16 segments of round lengths.
    assert!(heard["timeline"][0].get("bar").is_none(), "{heard:#}");
    assert_eq!(heard["timeline"].as_array().unwrap().len(), 16);
    assert_eq!(heard["timeline"][1]["start_s"], 0.5);
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn refuses_bad_input_with_readable_errors() {
    let dawkey = start().await;
    let message = dawkey.call_err("describe_audio", json!({})).await.unwrap();
    assert!(message.contains("nothing to listen to"), "{message}");
    song(&dawkey).await;
    let message = dawkey
        .call_err("describe_audio", json!({ "end_s": 700 }))
        .await
        .unwrap();
    assert!(message.contains("at most 600 s"), "{message}");
    let message = dawkey
        .call_err(
            "describe_audio",
            json!({ "path": "/nonexistent.wav", "stems": true }),
        )
        .await
        .unwrap();
    assert!(message.contains("leave them out with path"), "{message}");
    let message = dawkey
        .call_err("describe_audio", json!({ "volume": 3 }))
        .await
        .unwrap();
    assert!(message.contains("unknown field"), "{message}");

    for (args, expected) in [
        (json!({ "min_hz": 500, "max_hz": 400 }), "min_hz"),
        (json!({ "width": 5000 }), "width"),
        (json!({ "kind": "waveform" }), "unknown variant"),
        (json!({ "start_bar": 0 }), "at least 1"),
    ] {
        let result = dawkey.call_raw("view_audio", args.clone()).await.unwrap();
        assert_eq!(result.is_error, Some(true), "{args}");
        let text = &result.content[0].as_text().unwrap().text;
        assert!(text.contains(expected), "{args}: {text}");
    }
    dawkey.shutdown().await.unwrap();
}
