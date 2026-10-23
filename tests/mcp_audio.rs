// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end audio tests over MCP, using the virtual audio backend.

use std::path::{Path, PathBuf};

use dawkey_testkit::Harness;
use serde_json::{Value, json};

const DAWKEY: &str = env!("CARGO_BIN_EXE_dawkey");

/// Analysis hash of [`saw`] rendered from a mono clip at center pan, 0-1 s.
/// Changes to this value mean the engine's output changed.
const GOLDEN_SAW_HASH: &str = "2703c1eac1302f25";

async fn start() -> Harness {
    Harness::headless(DAWKEY, std::iter::empty::<&str>())
        .await
        .expect("dawkey starts")
}

/// A deterministic test signal (no transcendental functions): a 480 Hz
/// sawtooth at half scale.
fn saw(frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| ((i % 100) as f32 / 50.0 - 1.0) * 0.5)
        .collect()
}

fn write_wav(dir: &Path, name: &str, channels: &[Vec<f32>], rate: u32) -> PathBuf {
    let path = dir.join(name);
    dawkey_media::write_wav(&path, channels, rate).unwrap();
    path
}

fn peak(analysis: &Value, channel: usize) -> Option<f64> {
    analysis["peak_dbfs"][channel].as_f64()
}

fn approx(actual: Option<f64>, expected: f64) -> bool {
    actual.is_some_and(|a| (a - expected).abs() < 0.01)
}

/// dB of a half-scale signal panned to center with equal power.
const HALF_CENTER_DB: f64 = -9.0309;

#[tokio::test]
async fn import_and_render() {
    let dir = tempfile::tempdir().unwrap();
    let wav = write_wav(dir.path(), "saw.wav", &[saw(48_000)], 48_000);
    let dawkey = start().await;

    let imported = dawkey
        .call("import_audio", json!({ "path": wav }))
        .await
        .unwrap();
    assert_eq!(imported["frames"], 48_000);
    assert_eq!(imported["channels"], 1);
    let track = imported["track"].clone();

    let state = dawkey.call("get_state", json!({})).await.unwrap();
    let t = &state["project"]["tracks"][0];
    assert_eq!(t["name"], "saw");
    assert_eq!(t["clips"][0]["length"], 48_000);
    assert_eq!(state["project"]["sources"][0]["path"], json!(wav));

    let render = dawkey
        .call("render_range", json!({ "start_s": 0.0, "end_s": 1.0 }))
        .await
        .unwrap();
    let a = &render["analysis"];
    assert_eq!(a["frames"], 48_000);
    assert!(approx(peak(a, 0), HALF_CENTER_DB), "{a}");
    assert!(approx(peak(a, 1), HALF_CENTER_DB), "{a}");
    assert_eq!(a["hash"], GOLDEN_SAW_HASH, "engine output changed: {a}");

    // Fader gain applies; the clip ends after 1 s.
    dawkey
        .call("set_track_gain", json!({ "track": track, "gain_db": -6.0 }))
        .await
        .unwrap();
    let render = dawkey
        .call("render_range", json!({ "start_s": 0.5, "end_s": 1.5 }))
        .await
        .unwrap();
    assert!(approx(peak(&render["analysis"], 0), HALF_CENTER_DB - 6.0));
    let tail = dawkey
        .call("render_range", json!({ "start_s": 1.0, "end_s": 2.0 }))
        .await
        .unwrap();
    assert_eq!(tail["analysis"]["silent"], true);

    // Mute silences; undo brings it back.
    dawkey
        .call("set_track_mute", json!({ "track": track, "mute": true }))
        .await
        .unwrap();
    let muted = dawkey
        .call("render_range", json!({ "start_s": 0.0, "end_s": 1.0 }))
        .await
        .unwrap();
    assert_eq!(muted["analysis"]["silent"], true);
    dawkey.call("undo", json!({})).await.unwrap();
    let unmuted = dawkey
        .call("render_range", json!({ "start_s": 0.0, "end_s": 1.0 }))
        .await
        .unwrap();
    assert_eq!(unmuted["analysis"]["silent"], false);

    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn render_to_wav_file() {
    let dir = tempfile::tempdir().unwrap();
    let wav = write_wav(dir.path(), "saw.wav", &[saw(4_800)], 48_000);
    let out = dir.path().join("render.wav");
    let dawkey = start().await;
    dawkey
        .call("import_audio", json!({ "path": wav, "position_s": 0.25 }))
        .await
        .unwrap();
    let render = dawkey
        .call(
            "render_range",
            json!({ "start_s": 0.0, "end_s": 0.5, "output_path": out }),
        )
        .await
        .unwrap();
    assert_eq!(render["output_path"], json!(out));

    let decoded = dawkey_media::decode_file(&out, 48_000).unwrap();
    assert_eq!(decoded.audio.channel_count(), 2);
    assert_eq!(decoded.audio.frames(), 24_000);
    // Silence until the clip starts at 0.25 s.
    let left = decoded.audio.channel(0);
    assert!(left[..12_000].iter().all(|&s| s == 0.0));
    assert!(left[12_000..].iter().any(|&s| s != 0.0));
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn import_resamples_to_project_rate() {
    let dir = tempfile::tempdir().unwrap();
    let wav = write_wav(dir.path(), "cd.wav", &[saw(44_100), saw(44_100)], 44_100);
    let dawkey = start().await;
    let imported = dawkey
        .call("import_audio", json!({ "path": wav }))
        .await
        .unwrap();
    assert_eq!(imported["source_sample_rate"], 44_100);
    assert_eq!(imported["channels"], 2);
    let frames = imported["frames"].as_u64().unwrap();
    assert!((47_990..=48_010).contains(&frames), "{frames}");

    let err = dawkey
        .call_err(
            "import_audio",
            json!({ "path": dir.path().join("nope.wav") }),
        )
        .await
        .unwrap();
    assert!(err.contains("nope.wav"), "{err}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn virtual_clock_transport_and_meters() {
    let dir = tempfile::tempdir().unwrap();
    let wav = write_wav(dir.path(), "saw.wav", &[saw(96_000)], 48_000);
    let dawkey = start().await;
    let track = dawkey
        .call("import_audio", json!({ "path": wav }))
        .await
        .unwrap()["track"]
        .clone();

    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    assert_eq!(status["backend"], "virtual");
    assert_eq!(status["sample_rate"], 48_000);
    assert_eq!(status["transport"]["playing"], false);

    // Stopped: advancing outputs silence and doesn't move the playhead.
    let idle = dawkey
        .call("advance_clock", json!({ "seconds": 0.1 }))
        .await
        .unwrap();
    assert_eq!(idle["output"]["silent"], true);
    assert_eq!(idle["transport"]["position_frames"], 0);

    let playing = dawkey.call("play", json!({})).await.unwrap();
    assert_eq!(playing["playing"], true);
    let ran = dawkey
        .call("advance_clock", json!({ "seconds": 0.5 }))
        .await
        .unwrap();
    assert!(approx(peak(&ran["output"], 0), HALF_CENTER_DB), "{ran}");
    assert_eq!(ran["transport"]["position_frames"], 24_000);
    assert_eq!(ran["transport"]["position_s"], 0.5);

    let meters = dawkey.call("get_meters", json!({})).await.unwrap();
    assert!(approx(meters["master"][0].as_f64(), HALF_CENTER_DB));
    assert_eq!(meters["tracks"][0]["track"], track);
    // Peaks reset after reading.
    let meters = dawkey.call("get_meters", json!({})).await.unwrap();
    assert_eq!(meters["master"], json!([null, null]));

    // Playing past the end of the clip: silence.
    dawkey
        .call("locate", json!({ "position_s": 2.0 }))
        .await
        .unwrap();
    let past = dawkey
        .call("advance_clock", json!({ "seconds": 0.25 }))
        .await
        .unwrap();
    assert_eq!(past["output"]["silent"], true);
    assert_eq!(past["transport"]["position_frames"], 108_000);

    let stopped = dawkey.call("stop", json!({})).await.unwrap();
    assert_eq!(stopped["playing"], false);
    assert_eq!(stopped["position_s"], 2.25);

    let err = dawkey
        .call_err("advance_clock", json!({ "seconds": 0.0 }))
        .await
        .unwrap();
    assert!(err.contains("seconds must be"), "{err}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn saved_project_keeps_its_audio() {
    let dir = tempfile::tempdir().unwrap();
    let wav = write_wav(dir.path(), "take.wav", &[saw(24_000)], 48_000);
    let bundle = dir.path().join("Song");

    let dawkey = start().await;
    dawkey
        .call("import_audio", json!({ "path": wav, "position_s": 0.1 }))
        .await
        .unwrap();
    let before = dawkey
        .call("render_range", json!({ "start_s": 0.0, "end_s": 1.0 }))
        .await
        .unwrap();
    let saved = dawkey
        .call("save_project", json!({ "path": bundle }))
        .await
        .unwrap();
    let bundle = PathBuf::from(saved["path"].as_str().unwrap());
    dawkey.shutdown().await.unwrap();

    // The original file is gone; the bundle has its own copy.
    std::fs::remove_file(&wav).unwrap();
    let dawkey = Harness::headless(DAWKEY, [&bundle]).await.unwrap();
    let status = dawkey.call("get_engine_status", json!({})).await.unwrap();
    assert_eq!(status["missing_media"], json!([]));
    let after = dawkey
        .call("render_range", json!({ "start_s": 0.0, "end_s": 1.0 }))
        .await
        .unwrap();
    assert_eq!(after["analysis"]["hash"], before["analysis"]["hash"]);
    dawkey.shutdown().await.unwrap();
}

/// A signal with no repeats, so playing the wrong part of a source shows:
/// a ramp from -0.5 to 0.5.
fn ramp(frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| i as f32 / frames as f32 - 0.5)
        .collect()
}

#[tokio::test]
async fn split_and_trimmed_clips_play_the_same_audio() {
    let dir = tempfile::tempdir().unwrap();
    let wav = write_wav(dir.path(), "ramp.wav", &[ramp(48_000)], 48_000);
    let dawkey = start().await;
    let imported = dawkey
        .call("import_audio", json!({ "path": wav }))
        .await
        .unwrap();
    let clip = imported["clip"].clone();
    let hash = |start_s: f64, end_s: f64| {
        let dawkey = &dawkey;
        async move {
            let render = dawkey
                .call(
                    "render_range",
                    json!({ "start_s": start_s, "end_s": end_s }),
                )
                .await
                .unwrap();
            render["analysis"]["hash"].as_str().unwrap().to_owned()
        }
    };
    let whole = hash(0.0, 1.0).await;
    let middle = hash(0.25, 0.75).await;

    // Split anywhere, the two parts play on without a seam.
    let split = dawkey
        .call("split_clip", json!({ "clip": clip, "at_s": 0.37 }))
        .await
        .unwrap();
    assert!(split["clip"].is_u64(), "{split}");
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(
        state["project"]["tracks"][0]["clips"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(hash(0.0, 1.0).await, whole);
    dawkey.call("undo", json!({})).await.unwrap();

    // Trimmed, the middle plays where it was and the rest is silent.
    dawkey
        .call(
            "trim_clip",
            json!({ "clip": clip, "start_s": 0.25, "end_s": 0.75 }),
        )
        .await
        .unwrap();
    assert_eq!(hash(0.25, 0.75).await, middle);
    for (start_s, end_s) in [(0.0, 0.25), (0.75, 1.0)] {
        let render = dawkey
            .call(
                "render_range",
                json!({ "start_s": start_s, "end_s": end_s }),
            )
            .await
            .unwrap();
        assert_eq!(render["analysis"]["silent"], true, "{start_s}..{end_s}");
    }

    // Bad input is a tool error a model can read.
    let error = dawkey
        .call_err("split_clip", json!({ "clip": clip, "at_s": 0.9 }))
        .await
        .unwrap();
    assert!(error.contains("inside clip"), "{error}");
    let error = dawkey
        .call_err("trim_clip", json!({ "clip": clip, "start_s": -1.0 }))
        .await
        .unwrap();
    assert!(error.contains("position_s"), "{error}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn duplicated_and_repeated_clips_play_the_same_audio() {
    let dir = tempfile::tempdir().unwrap();
    let wav = write_wav(dir.path(), "ramp.wav", &[ramp(48_000)], 48_000);
    let dawkey = start().await;
    let imported = dawkey
        .call("import_audio", json!({ "path": wav }))
        .await
        .unwrap();
    let clip = imported["clip"].clone();
    let hash = |start_s: f64, end_s: f64| {
        let dawkey = &dawkey;
        async move {
            let render = dawkey
                .call(
                    "render_range",
                    json!({ "start_s": start_s, "end_s": end_s }),
                )
                .await
                .unwrap();
            render["analysis"]["hash"].as_str().unwrap().to_owned()
        }
    };
    let whole = hash(0.0, 1.0).await;
    let half = hash(0.0, 0.5).await;

    // A copy at 3 s plays what the clip plays.
    let copy = dawkey
        .call("duplicate_clip", json!({ "clip": clip, "position_s": 3.0 }))
        .await
        .unwrap();
    assert!(copy["clip"].is_u64(), "{copy}");
    assert_eq!(hash(3.0, 4.0).await, whole);
    dawkey.call("undo", json!({})).await.unwrap();

    // Repeated to 2.5 s: a whole copy, then half of one.
    let repeated = dawkey
        .call("repeat_clip", json!({ "clip": clip, "until_s": 2.5 }))
        .await
        .unwrap();
    assert_eq!(repeated["clips"].as_array().unwrap().len(), 2, "{repeated}");
    assert_eq!(hash(1.0, 2.0).await, whole);
    assert_eq!(hash(2.0, 2.5).await, half);
    let render = dawkey
        .call("render_range", json!({ "start_s": 2.5, "end_s": 3.0 }))
        .await
        .unwrap();
    assert_eq!(render["analysis"]["silent"], true);

    // Bad input is a tool error a model can read.
    let error = dawkey
        .call_err("repeat_clip", json!({ "clip": clip, "until_s": 0.5 }))
        .await
        .unwrap();
    assert!(error.contains("after clip"), "{error}");
    let error = dawkey
        .call_err("duplicate_clip", json!({ "clip": 999 }))
        .await
        .unwrap();
    assert!(error.contains("999"), "{error}");
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn exports_the_song_to_wav_and_flac() {
    let dir = tempfile::tempdir().unwrap();
    let wav = write_wav(dir.path(), "saw.wav", &[saw(48_000)], 48_000);
    let dawkey = start().await;
    // Nothing to export yet.
    let err = dawkey
        .call_err("export_audio", json!({ "path": dir.path().join("x.wav") }))
        .await
        .unwrap();
    assert!(err.contains("nothing to export"), "{err}");
    dawkey
        .call("import_audio", json!({ "path": wav, "position_s": 0.5 }))
        .await
        .unwrap();

    // The whole song (to where the clip ends, 1.5 s), as 32-bit float: the
    // same audio render_range makes.
    let out = dir.path().join("song.wav");
    let exported = dawkey
        .call("export_audio", json!({ "path": out, "bit_depth": 32 }))
        .await
        .unwrap();
    assert_eq!(exported["end_s"], 1.5);
    let rendered = dawkey
        .call("render_range", json!({ "start_s": 0.0, "end_s": 1.5 }))
        .await
        .unwrap();
    // (RMS sums in blocks, so only its last digits may differ.)
    for key in ["frames", "hash", "peak_dbfs", "lufs"] {
        assert_eq!(
            exported["analysis"][key], rendered["analysis"][key],
            "{key}"
        );
    }
    let decoded = dawkey_media::decode_file(&out, 48_000).unwrap().audio;
    assert_eq!(decoded.frames(), 72_000);
    assert_eq!(
        dawkey_media::analyze(decoded.channels(), 48_000).hash,
        rendered["analysis"]["hash"]
    );

    // FLAC at CD quality, with a tail: 2 s converted to 44.1 kHz.
    let out = dir.path().join("song.flac");
    let exported = dawkey
        .call(
            "export_audio",
            json!({ "path": out, "bit_depth": 16, "sample_rate": 44_100, "tail_s": 0.5 }),
        )
        .await
        .unwrap();
    assert_eq!(exported["analysis"]["frames"], 88_200);
    // A sawtooth's edges ring once resampled (its peaks overshoot), but its
    // level stays.
    let rendered = dawkey
        .call("render_range", json!({ "start_s": 0.0, "end_s": 2.0 }))
        .await
        .unwrap();
    let rms = |a: &Value| a["rms_dbfs"][0].as_f64().unwrap();
    let (got, want) = (rms(&exported["analysis"]), rms(&rendered["analysis"]));
    assert!((got - want).abs() < 0.1, "{got} vs {want}");
    let decoded = dawkey_media::decode_file(&out, 44_100).unwrap();
    assert_eq!(
        (decoded.source_rate, decoded.audio.frames()),
        (44_100, 88_200)
    );

    // Bad requests are tool errors.
    for (args, expected) in [
        (json!({ "path": dir.path().join("x.mp3") }), "give a format"),
        (json!({ "path": out, "bit_depth": 32 }), "not 32-bit float"),
        (json!({ "path": out, "bit_depth": 12 }), "16, 24 or 32"),
    ] {
        let err = dawkey.call_err("export_audio", args).await.unwrap();
        assert!(err.contains(expected), "{err}");
    }
    dawkey.shutdown().await.unwrap();
}
