// SPDX-License-Identifier: AGPL-3.0-or-later
//! Studio tests, with the virtual backend.

use std::path::{Path, PathBuf};

use dawkey_core::{AddPlugin, AddTrack, Project, SetPluginParam, SetTrackArm, SetTrackMonitor};
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
fn removing_a_track_while_recording_keeps_the_other_takes() {
    let dir = tempfile::tempdir().unwrap();
    let mut studio = recording_studio(dir.path());
    studio.set_virtual_midi_devices(vec!["Keys".into()], Vec::new());
    let vox = studio.session().project().tracks[0].id;
    let armed = |studio: &mut Studio, kind| {
        let track = studio
            .execute(
                AddTrack {
                    kind,
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
        track
    };
    let gtr = armed(&mut studio, dawkey_core::TrackKind::Audio);
    let keys = armed(&mut studio, dawkey_core::TrackKind::Midi);
    let pads = armed(&mut studio, dawkey_core::TrackKind::Midi);
    studio.record().unwrap();
    let note = [(0.1, [0x90, 60, 100], 0), (0.2, [0x80, 60, 0], 0)];
    studio.queue_virtual_midi(Some("Keys"), &note).unwrap();
    studio.advance(0.3).unwrap();
    for track in [gtr, pads] {
        studio
            .execute(dawkey_core::RemoveTrack { track }.into())
            .unwrap();
    }
    studio.stop().unwrap();

    let project = studio.session().project();
    for track in [vox, keys] {
        assert_eq!(project.track(track).unwrap().clips.len(), 1, "{track}");
    }
    // The removed track's recording went with it.
    assert_eq!(project.sources.len(), 1);
    let files = std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(files, 1, "only Vox's take is left");
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

/// A MIDI file with two tracks of one note each.
fn two_track_file() -> dawkey_media::MidiFile {
    use dawkey_media::{MidiFileNote, MidiFileTrack};
    let track = |name: &str, pitch| MidiFileTrack {
        name: name.into(),
        notes: vec![MidiFileNote {
            pitch,
            velocity: 100,
            start_beats: 0.0,
            length_beats: 4.0,
        }],
        ..Default::default()
    };
    dawkey_media::MidiFile {
        tempo: vec![],
        meters: vec![],
        tracks: vec![track("A", 60), track("B", 64)],
    }
}

/// A studio with `file` imported onto new tracks, and those tracks.
fn studio_with_imported(file: &dawkey_media::MidiFile) -> (Studio, Vec<dawkey_core::TrackId>) {
    let mut studio = Studio::new(
        LoadedProject::new(Project::default()),
        AudioBackend::Virtual,
    )
    .unwrap();
    let first = studio.import_midi(file, 0.0, None, &[], false).unwrap();
    (studio, first.tracks)
}

#[test]
fn reimport_onto_one_target_replaces_its_clip() {
    let file = two_track_file();
    let (mut studio, tracks) = studio_with_imported(&file);
    let targets = [crate::MidiImportTarget {
        file_track: 0,
        track: tracks[0],
    }];
    let again = studio
        .import_midi(&file, 0.0, None, &targets, true)
        .unwrap();
    assert_eq!(again.removed_clips.len(), 1);
}

#[test]
fn reimport_onto_several_targets_replaces_each_clip() {
    let file = two_track_file();
    let (mut studio, tracks) = studio_with_imported(&file);
    let targets = [
        crate::MidiImportTarget {
            file_track: 0,
            track: tracks[0],
        },
        crate::MidiImportTarget {
            file_track: 1,
            track: tracks[1],
        },
    ];
    let again = studio
        .import_midi(&file, 0.0, None, &targets, true)
        .unwrap();
    assert_eq!(again.removed_clips.len(), 2);
    assert_eq!(again.clips.len(), 2);
    // Each mapped track is left with only its new clip.
    let project = studio.session().project();
    for (&track, &clip) in tracks.iter().zip(&again.clips) {
        let clips: Vec<_> = project
            .track(track)
            .unwrap()
            .clips
            .iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(clips, [clip]);
    }
}

/// An armed, monitoring track plays its rack through live input, not just
/// recorded takes: the question "can you hear the effects without
/// recording?" asks exactly this.
#[test]
fn monitoring_hears_the_racks_effects_without_recording() {
    let mut studio = Studio::new(
        LoadedProject::new(Project::default()),
        AudioBackend::Virtual,
    )
    .unwrap();
    // Each `execute` queues a new engine graph; a real session has the
    // audio thread continuously draining that queue between user actions,
    // but nothing drives it here, so a tiny `advance` after each `execute`
    // stands in for that passage of time and lets the graph swap land
    // before the next command queues another one.
    let flush = |studio: &mut Studio| {
        studio.advance(0.001).unwrap();
    };
    let track = studio
        .execute(AddTrack::default().into())
        .unwrap()
        .track
        .unwrap();
    flush(&mut studio);
    studio
        .execute(SetTrackArm { track, armed: true }.into())
        .unwrap();
    flush(&mut studio);
    studio
        .execute(
            SetTrackMonitor {
                track,
                monitor: true,
            }
            .into(),
        )
        .unwrap();
    flush(&mut studio);

    // Not playing, not recording, nothing on the timeline: this is
    // monitoring live input, not playback of a take.
    studio
        .set_virtual_input(Some(AudioData::new(vec![vec![0.5; 480_000]])))
        .unwrap();
    let dry = studio.advance(0.1).unwrap();
    assert!(!dry.silent);

    let plugin = studio
        .execute(
            AddPlugin {
                track,
                plugin: "builtin:gate".to_owned(),
                index: None,
            }
            .into(),
        )
        .unwrap()
        .plugin
        .unwrap();
    flush(&mut studio);
    // At its default threshold (-45 dB), well under the monitored -6 dBFS
    // signal, the gate stays open and barely touches it.
    let with_defaults = studio.advance(0.1).unwrap();

    // Threshold at its max (0 dBFS), well above the monitored -6 dBFS
    // signal, so the gate now stays shut.
    studio
        .execute(
            SetPluginParam {
                plugin,
                param: "threshold".to_owned(),
                value: Some(0.0),
                text: None,
            }
            .into(),
        )
        .unwrap();
    flush(&mut studio);
    // With its default hold (40 ms) and release (120 ms), it needs some
    // real time to close; let it settle, then measure the steady,
    // fully-closed state (not the moment it's still on its way down).
    studio.advance(0.5).unwrap();
    let gated = studio.advance(0.1).unwrap();

    assert!(
        gated.rms_dbfs[0].unwrap() < with_defaults.rms_dbfs[0].unwrap() - 20.0,
        "monitoring should hear the mounted gate close once its threshold is \
         turned up: dry {:?}, with_defaults {:?}, gated {:?}",
        dry.rms_dbfs,
        with_defaults.rms_dbfs,
        gated.rms_dbfs
    );
}

#[test]
fn mixdown_tracks_bounces_audio_synth_and_bus_tracks_and_restores() {
    use dawkey_core::{AddClip, AddMidiClip, NoteSpec, RestoreMixdownTracks, TrackKind};
    use dawkey_media::DecodedFile;

    let dir = tempfile::tempdir().unwrap();
    let mut studio = Studio::new(
        LoadedProject::new(Project::default()),
        AudioBackend::Virtual,
    )
    .unwrap();
    studio.set_recording_dir(dir.path().to_owned());

    let audio_track = studio
        .execute(
            AddTrack {
                name: Some("Guitar".into()),
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap();
    let samples: Vec<f32> = (0..48_000)
        .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.25)
        .collect();
    let src = studio.add_source(
        Path::new("/tmp/guitar.wav"),
        DecodedFile {
            audio: AudioData::new(vec![samples]),
            source_rate: 48_000,
        },
    );
    studio
        .execute(AddClip::new(src, Some(audio_track), 0.0).into())
        .unwrap();

    let synth_track = studio
        .execute(
            AddTrack {
                name: Some("Synth".into()),
                kind: TrackKind::Midi,
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap();
    studio
        .execute(
            AddMidiClip {
                track: Some(synth_track),
                position_s: 0.0,
                notes: vec![NoteSpec {
                    pitch: 60,
                    start_beats: 0.0,
                    length_beats: 1.0,
                    velocity: 100,
                }],
                ..Default::default()
            }
            .into(),
        )
        .unwrap();

    let outcome = studio
        .mixdown_tracks(&[audio_track, synth_track], None, true)
        .unwrap();
    let mix_track = outcome.track.unwrap();
    let project = studio.session().project();
    assert_eq!(project.tracks.len(), 1);
    assert_eq!(project.tracks[0].id, mix_track);
    assert_eq!(project.tracks[0].name, "Mixdown 1");
    assert_eq!(project.tracks[0].clips.len(), 1);
    assert!(project.mixdown_archive(mix_track).is_some());

    // Confirm rendered audio is audible when playing back the mixdown track.
    let job = studio.render_job(0.0, 1.0).unwrap();
    let (rendered, leftovers) = job.run();
    studio.finish_render(leftovers);
    let analysis = dawkey_media::analyze(&rendered, 48_000);
    assert!(!analysis.silent);

    // Single undo restores both original tracks and removes the mixdown track.
    studio.undo().unwrap();
    assert_eq!(studio.session().project().tracks.len(), 2);
    assert!(studio.session().project().track(audio_track).is_some());
    assert!(studio.session().project().track(synth_track).is_some());

    // Redo re-applies mixdown; RestoreMixdownTracks restores permanently as one undo step.
    studio.redo().unwrap();
    assert_eq!(studio.session().project().tracks.len(), 1);
    studio
        .execute(
            RestoreMixdownTracks {
                mix_track,
                remove_mix_track: true,
            }
            .into(),
        )
        .unwrap();
    assert_eq!(studio.session().project().tracks.len(), 2);
    assert!(studio.session().project().track(mix_track).is_none());
}

#[test]
fn delete_tracks_non_destructive_and_permanent_cleanup() {
    use dawkey_core::{AddClip, AddPlugin, AddTrack, PluginState, RestoreDeletedTracks, TrackKind};
    use dawkey_media::DecodedFile;

    let dir = tempfile::tempdir().unwrap();
    let rec_dir = dir.path().join("recordings");
    std::fs::create_dir_all(&rec_dir).unwrap();
    let mut studio = Studio::new(
        LoadedProject::new(Project::default()),
        AudioBackend::Virtual,
    )
    .unwrap();
    studio.set_recording_dir(rec_dir.clone());

    let t1 = studio
        .execute(
            AddTrack {
                name: Some("Audio 1".into()),
                kind: TrackKind::Audio,
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap();
    let wav_in_rec = rec_dir.join("take1.wav");
    dawkey_media::write_wav(&wav_in_rec, &[vec![0.1; 1024]], 48_000).unwrap();
    let src1 = studio.add_source(
        &wav_in_rec,
        DecodedFile {
            audio: AudioData::new(vec![vec![0.1; 1024]]),
            source_rate: 48_000,
        },
    );
    studio
        .execute(AddClip::new(src1, Some(t1), 0.0).into())
        .unwrap();
    let plug1 = studio
        .execute(
            AddPlugin {
                track: t1,
                plugin: "builtin:eq".into(),
                index: None,
            }
            .into(),
        )
        .unwrap()
        .plugin
        .unwrap();
    studio
        .session
        .set_plugin_state(plug1, Some(PluginState::new(vec![42])));

    // Non-destructive delete preserves source file, pool entry, and plugin state.
    studio.delete_tracks(&[t1], false).unwrap();
    assert!(studio.session().project().track(t1).is_none());
    assert!(wav_in_rec.exists());
    assert!(studio.session().project().source(src1).is_some());
    assert!(
        studio
            .session()
            .project()
            .plugin_states
            .contains_key(&plug1)
    );

    // Restoring brings t1 back cleanly.
    studio
        .execute(RestoreDeletedTracks { index: None }.into())
        .unwrap();
    assert!(studio.session().project().track(t1).is_some());

    // Permanent delete removes the exclusive source, file inside recording_dir, and plugin state.
    studio.delete_tracks(&[t1], true).unwrap();
    assert!(studio.session().project().track(t1).is_none());
    assert!(!wav_in_rec.exists());
    assert!(studio.session().project().source(src1).is_none());
    assert!(
        !studio
            .session()
            .project()
            .plugin_states
            .contains_key(&plug1)
    );
}
