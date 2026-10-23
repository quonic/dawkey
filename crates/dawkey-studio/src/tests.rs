// SPDX-License-Identifier: AGPL-3.0-or-later
//! Studio tests, with the virtual backend.

use std::path::{Path, PathBuf};

use dawkey_core::{AddTrack, Project, SetTrackArm};
use dawkey_media::AudioData;

use crate::{AudioBackend, LoadedProject, Studio, StudioOptions};

/// A studio recording unsaved projects into `dir`, with an armed audio
/// track fed a constant signal.
fn recording_studio(dir: &Path) -> Studio {
    let mut studio = Studio::new(
        LoadedProject::new(Project::default()),
        AudioBackend::Virtual,
    )
    .unwrap();
    studio.set_recording_dir(dir.to_owned());
    let track = studio
        .execute(AddTrack::default().into())
        .unwrap()
        .track
        .unwrap();
    studio
        .execute(SetTrackArm { track, armed: true }.into())
        .unwrap();
    studio
        .set_virtual_input(Some(AudioData::new(vec![vec![0.25; 96_000]])))
        .unwrap();
    studio
}

/// Records a quarter second; returns the take's file.
fn record(studio: &mut Studio) -> PathBuf {
    studio.record().unwrap();
    studio.advance(0.25).unwrap();
    studio.stop().unwrap();
    let source = studio.session().project().sources.last().unwrap().id;
    studio.session().source_path(source).unwrap()
}

#[test]
fn saving_deletes_cached_takes() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("cache");
    let mut studio = recording_studio(&cache);
    let take = record(&mut studio);
    assert!(
        take.starts_with(&cache) && take.exists(),
        "{}",
        take.display()
    );

    let bundle = studio.save(Some(&dir.path().join("Song"))).unwrap();
    assert!(!take.exists() && !cache.exists(), "the cache copy is gone");
    let source = studio.session().project().sources[0].id;
    let saved = studio.session().source_path(source).unwrap();
    assert!(saved.starts_with(&bundle) && saved.exists());

    // Recordings of a saved project go into it, and stay.
    let second = record(&mut studio);
    assert!(second.starts_with(&bundle));
    drop(studio);
    assert!(second.exists() && saved.exists());
}

#[test]
fn replacing_or_closing_an_unsaved_project_deletes_its_takes() {
    let dir = tempfile::tempdir().unwrap();
    let mut studio = recording_studio(dir.path());
    let first = record(&mut studio);
    studio
        .replace(LoadedProject::new(Project::default()))
        .unwrap();
    assert!(!first.exists());

    // Closing, even mid-recording.
    let mut studio = recording_studio(dir.path());
    let second = record(&mut studio);
    studio.record().unwrap();
    studio.advance(0.1).unwrap();
    drop(studio);
    assert!(!second.exists());
    assert!(!dir.path().exists() || dir.path().read_dir().unwrap().next().is_none());
}

#[test]
fn reported_midi_device_latency_is_compensated() {
    let mut studio = Studio::new(
        LoadedProject::new(Project::default()),
        AudioBackend::Virtual,
    )
    .unwrap();
    let track = studio
        .execute(
            AddTrack {
                kind: dawkey_core::TrackKind::Midi,
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap();
    let mut set = dawkey_core::SetTrackMidiOutput::new(track, Some("Synth".into()));
    set.latency_ms = Some(5.0);
    studio.execute(set.into()).unwrap();
    assert_eq!(studio.engine_status().plugin_latency_frames, 240);

    // The device reports 10 ms more: the rest of the mix waits 15 ms.
    studio.set_virtual_midi_latency("Synth", Some(10.0));
    let status = studio.engine_status();
    assert_eq!(status.plugin_latency_frames, 720);
    assert_eq!(status.midi_output_latency_ms["Synth"], 10.0);
    studio.set_virtual_midi_latency("Synth", None);
    assert_eq!(studio.engine_status().plugin_latency_frames, 240);
}

#[test]
fn reported_midi_input_latency_places_recorded_notes_earlier() {
    let dir = tempfile::tempdir().unwrap();
    let mut studio = Studio::new(
        LoadedProject::new(Project::default()),
        AudioBackend::Virtual,
    )
    .unwrap();
    studio.set_recording_dir(dir.path().to_owned());
    studio.set_virtual_midi_devices(vec!["Keys".into(), "Pads".into()], Vec::new());
    let track = studio
        .execute(
            AddTrack {
                kind: dawkey_core::TrackKind::Midi,
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap();
    studio
        .execute(SetTrackArm { track, armed: true }.into())
        .unwrap();
    // "Keys" reports 10 ms (480 frames); "Pads" nothing.
    studio.set_virtual_midi_input_latency("Keys", Some(10.0));
    let status = studio.engine_status();
    assert_eq!(status.midi_input_latency_ms["Keys"], 10.0);
    assert_eq!(status.midi_record_latency_ms["Keys"], 10.0);
    assert_eq!(status.midi_record_latency_ms["Pads"], 0.0);

    studio.record().unwrap();
    let note = |pitch: u8| [(0.1, [0x90, pitch, 100], 0), (0.2, [0x80, pitch, 0], 0)];
    studio.queue_virtual_midi(Some("Keys"), &note(60)).unwrap();
    studio.queue_virtual_midi(Some("Pads"), &note(64)).unwrap();
    studio.advance(0.3).unwrap();
    studio.stop().unwrap();

    let project = studio.session().project();
    let clip = &project.track(track).unwrap().clips[0];
    let at = |pitch: u8| {
        let n = clip.notes().iter().find(|n| n.pitch == pitch).unwrap();
        (clip.start + n.start, n.length)
    };
    // Each note moves by its own device's latency.
    assert_eq!(at(60), (4800 - 480, 4800));
    assert_eq!(at(64), (4800, 4800));
    assert_eq!(clip.start, 0);
}

/// Renders `seconds` from the start; the left channel's peak in dBFS.
fn peak_db(studio: &mut Studio, seconds: f64) -> f64 {
    let job = studio.render_job(0.0, seconds).unwrap();
    let rate = job.sample_rate();
    let (audio, leftovers) = job.run();
    studio.finish_render(leftovers);
    dawkey_media::analyze(&audio, rate).peak_dbfs[0].unwrap()
}

#[test]
fn plugins_scan_in_the_background() {
    let dir = tempfile::tempdir().unwrap();
    let installed = dawkey_test_plugins::install(dir.path()).unwrap();
    let search = dawkey_plugins::SearchPath {
        clap: vec![installed.clap_path],
        lv2: Some(vec![installed.lv2_path]),
        vst3: vec![installed.vst3_path],
    };
    // A project with a gain plugin at -6 dB on a track of constant 0.5.
    let wav = dir.path().join("half.wav");
    dawkey_media::write_wav(&wav, &[vec![0.5; 4800]], 48_000).unwrap();
    let mut session = dawkey_core::Session::new(Project::default());
    let track = session
        .execute(AddTrack::default().into())
        .unwrap()
        .track
        .unwrap();
    let plugin = session
        .execute(
            dawkey_core::AddPlugin {
                track,
                plugin: "clap:dawkey.test.gain".into(),
                index: None,
            }
            .into(),
        )
        .unwrap()
        .plugin
        .unwrap();
    session
        .execute(dawkey_core::SetPluginParam::new(plugin, "1", -6.0).into())
        .unwrap();
    let options = StudioOptions {
        background_scan: true,
        plugin_search: Some((search, None)),
        ..StudioOptions::default()
    };
    let loaded = LoadedProject::new(session.project().clone());
    let mut studio = Studio::with_options(loaded, AudioBackend::Virtual, options).unwrap();
    // The first compile started a scan instead of waiting for one.
    assert!(studio.scanning_plugins());
    let decoded = dawkey_media::decode_file(&wav, 48_000).unwrap();
    let source = studio.add_source(&wav, decoded);
    studio
        .execute(
            dawkey_core::AddClip {
                source,
                track: Some(track),
                position_s: 0.0,
                offset_s: 0.0,
                length_s: None,
                muted: false,
                take: false,
            }
            .into(),
        )
        .unwrap();
    wait_for_scan(&mut studio);
    // Once it's done, the slot loads.
    assert!(
        studio
            .plugin_catalog_now()
            .unwrap()
            .get("clap:dawkey.test.gain")
            .is_some()
    );
    // 0.5, mono, panned to center with equal power: -9.03 dB, then -6 dB.
    assert!((peak_db(&mut studio, 0.1) - (-9.0309 - 6.0)).abs() < 0.01);

    // Rescanning keeps the list (and the running plugin) until it's done.
    studio.rescan_plugins_in_background();
    assert!(studio.scanning_plugins());
    assert!(studio.plugin_catalog_now().is_some());
    assert!(studio.engine_status().plugin_problems.is_empty());
    wait_for_scan(&mut studio);
    assert!((peak_db(&mut studio, 0.1) - (-9.0309 - 6.0)).abs() < 0.01);
}

fn wait_for_scan(studio: &mut Studio) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while studio.scanning_plugins() {
        assert!(std::time::Instant::now() < deadline, "the scan never ended");
        std::thread::sleep(std::time::Duration::from_millis(10));
        studio.tick();
    }
}

#[test]
fn autosaves_recover_a_never_saved_project() {
    let root = tempfile::tempdir().unwrap();
    // A Dawkey (pid 4294967295 can't be running) records, autosaves, and
    // stops without exiting cleanly.
    let dead = root.path().join("4294967295");
    let mut crashed = recording_studio(&dead);
    let take = record(&mut crashed);
    assert!(crashed.autosave().unwrap());
    assert!(!crashed.autosave().unwrap(), "nothing new to keep");
    let autosave = crashed.autosave_file();
    assert_eq!(autosave, dead.join("autosave.json"));
    std::mem::forget(crashed);

    // The next one finds it, recovers it, and takes its folder over.
    let own = root.path().join(std::process::id().to_string());
    let mut studio = Studio::new(
        LoadedProject::new(Project::default()),
        AudioBackend::Virtual,
    )
    .unwrap();
    studio.set_recording_dir(own);
    let recovery = studio.orphan_recovery().expect("an autosave to recover");
    assert_eq!(recovery.dir.as_deref(), Some(dead.as_path()));
    assert!(recovery.age() < std::time::Duration::from_secs(60));
    studio.recover(&recovery).unwrap();
    let project = studio.session().project();
    assert_eq!(project.tracks.len(), 1);
    assert_eq!(
        studio.session().source_path(project.sources[0].id),
        Some(take.clone())
    );
    assert!(studio.session().is_dirty() && studio.session().path().is_none());
    // While this Dawkey has it, nobody else recovers it.
    assert!(crate::cache::orphaned_autosaves(root.path()).is_empty());
    assert!(dead.join("autosave.json").exists());

    // Saving copies the take in, and leaves nothing in the cache.
    studio.save(Some(&root.path().join("Song"))).unwrap();
    assert!(!take.exists() && !dead.join("autosave.json").exists());
    drop(studio);
    assert!(!dead.exists());
}

#[test]
fn autosaves_recover_a_bundles_unsaved_changes() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("Song.dawkey");
    let mut studio = Studio::new(
        LoadedProject::new(Project::default()),
        AudioBackend::Virtual,
    )
    .unwrap();
    studio.save(Some(&bundle)).unwrap();
    assert!(!studio.autosave().unwrap(), "no changes to keep");
    studio.execute(AddTrack::default().into()).unwrap();
    assert!(studio.autosave().unwrap());
    assert_eq!(studio.autosave_file(), bundle.join("backups/autosave.json"));
    std::mem::forget(studio);

    let open =
        || Studio::new(LoadedProject::open(&bundle).unwrap(), AudioBackend::Virtual).unwrap();
    let mut studio = open();
    assert!(studio.session().project().tracks.is_empty());
    let recovery = studio.bundle_recovery().expect("newer than the project");
    studio.recover(&recovery).unwrap();
    assert_eq!(studio.session().project().tracks.len(), 1);
    assert!(studio.session().is_dirty());
    // Exiting cleanly (the changes were saved or discarded) leaves none.
    drop(studio);
    assert!(!bundle.join("backups").exists());
    assert!(open().bundle_recovery().is_none());

    // Discarding deletes it.
    let mut studio = open();
    studio.execute(AddTrack::default().into()).unwrap();
    studio.autosave().unwrap();
    std::mem::forget(studio);
    let recovery = open().bundle_recovery().unwrap();
    Studio::discard_recovery(&recovery);
    assert!(open().bundle_recovery().is_none());
}
