// SPDX-License-Identifier: AGPL-3.0-or-later
//! The on-disk project bundle:
//!
//! ```text
//! Song.dawkey/
//!   project.json   versioned document (see [`FORMAT_VERSION`])
//!   audio/         recorded and imported audio
//! ```

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::model::{Project, SourceId};

pub const BUNDLE_EXTENSION: &str = "dawkey";
pub const DOCUMENT_NAME: &str = "project.json";
pub const AUDIO_DIR: &str = "audio";
/// Files plugins keep with their state: a folder per slot.
pub const PLUGIN_DIR: &str = "plugins";
/// Where a bundle keeps its autosave.
pub const BACKUP_DIR: &str = "backups";
/// The autosave's file name (a project document, with absolute paths).
pub const AUTOSAVE_NAME: &str = "autosave.json";

const FORMAT_TAG: &str = "dawkey-project";
/// Bump when the document layout changes, and add a step to [`migrate`].
///
/// - 1: M0 to M4.
/// - 2: clips carry a `kind` ("audio" or "midi").
pub const FORMAT_VERSION: u32 = 2;

#[derive(Serialize)]
struct DocumentOut<'a> {
    format: &'static str,
    version: u32,
    project: &'a Project,
}

#[derive(Deserialize)]
struct DocumentHeader {
    format: String,
    version: u32,
}

#[derive(Deserialize)]
struct DocumentIn {
    project: Project,
}

/// Returns `path` with a `.dawkey` extension, adding one if missing.
pub fn bundle_path(path: &Path) -> PathBuf {
    if path.extension().is_some_and(|e| e == BUNDLE_EXTENSION) {
        path.to_owned()
    } else {
        let mut name = path.as_os_str().to_owned();
        name.push(".");
        name.push(BUNDLE_EXTENSION);
        PathBuf::from(name)
    }
}

/// Writes the project into the bundle at `bundle`, creating it if needed.
/// The document is replaced atomically.
pub fn save(project: &Project, bundle: &Path) -> Result<(), CoreError> {
    fs::create_dir_all(bundle.join(AUDIO_DIR)).map_err(|e| CoreError::io(bundle, e))?;
    write_document(project, &bundle.join(DOCUMENT_NAME))
}

/// Writes the project as a document at `file` (a bundle's `project.json`,
/// or an autosave), replacing it atomically and making its folder if
/// needed.
pub fn write_document(project: &Project, file: &Path) -> Result<(), CoreError> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).map_err(|e| CoreError::io(dir, e))?;
    }
    let doc = DocumentOut {
        format: FORMAT_TAG,
        version: FORMAT_VERSION,
        project,
    };
    let mut json = serde_json::to_vec_pretty(&doc).expect("project serializes to JSON");
    json.push(b'\n');
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = file.with_file_name(format!("{name}.tmp"));
    let write = || -> std::io::Result<()> {
        let mut out = fs::File::create(&tmp)?;
        out.write_all(&json)?;
        out.sync_all()?;
        fs::rename(&tmp, file)
    };
    write().map_err(|e| CoreError::io(file, e))
}

/// Makes sure the source file at `current` is inside `bundle`, copying it
/// into the bundle's audio directory if not. Returns its path relative to the
/// bundle.
pub fn store_in_bundle(current: &Path, bundle: &Path, id: SourceId) -> Result<PathBuf, CoreError> {
    if let Ok(relative) = current.strip_prefix(bundle) {
        return Ok(relative.to_owned());
    }
    let stem = current
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "audio".into());
    let mut name = format!("{stem}-{id}");
    if let Some(ext) = current.extension() {
        name.push('.');
        name.push_str(&ext.to_string_lossy());
    }
    let relative = Path::new(AUDIO_DIR).join(name);
    let dest = bundle.join(&relative);
    if !dest.exists() {
        let audio_dir = bundle.join(AUDIO_DIR);
        fs::create_dir_all(&audio_dir).map_err(|e| CoreError::io(&audio_dir, e))?;
        fs::copy(current, &dest).map_err(|e| CoreError::io(current, e))?;
    }
    Ok(relative)
}

/// Loads the bundle at `path` (or the bundle containing `path`, if it names
/// the `project.json` inside one). Returns the project and the bundle path.
pub fn load(path: &Path) -> Result<(Project, PathBuf), CoreError> {
    let bundle = if path.file_name().is_some_and(|n| n == DOCUMENT_NAME) {
        path.parent().unwrap_or(Path::new(".")).to_owned()
    } else {
        path.to_owned()
    };
    let project = read_document(&bundle.join(DOCUMENT_NAME))?;
    Ok((project, bundle))
}

/// Reads a project document (a bundle's `project.json`, or an autosave),
/// upgrading an older one.
pub fn read_document(doc_path: &Path) -> Result<Project, CoreError> {
    let bytes = fs::read(doc_path).map_err(|e| CoreError::io(doc_path, e))?;
    let bad = |reason: String| CoreError::ProjectFormat {
        path: doc_path.to_owned(),
        reason,
    };

    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| bad(e.to_string()))?;
    let header: DocumentHeader =
        serde_json::from_value(value.clone()).map_err(|e| bad(e.to_string()))?;
    if header.format != FORMAT_TAG {
        return Err(bad(format!("unknown format {:?}", header.format)));
    }
    if header.version > FORMAT_VERSION {
        return Err(bad(format!(
            "made by a newer Dawkey (format version {}, this build reads up to {FORMAT_VERSION})",
            header.version
        )));
    }
    let value = migrate(header.version, value).map_err(bad)?;
    let mut project = serde_json::from_value::<DocumentIn>(value)
        .map_err(|e| bad(e.to_string()))?
        .project;

    let mut seen = HashSet::new();
    if let Some(dup) = project.tracks.iter().find(|t| !seen.insert(t.id)) {
        return Err(bad(format!("duplicate track id {}", dup.id)));
    }
    project.sanitize();
    Ok(project)
}

/// Upgrades an older document to [`FORMAT_VERSION`], one version at a time.
fn migrate(mut version: u32, mut value: serde_json::Value) -> Result<serde_json::Value, String> {
    while version < FORMAT_VERSION {
        match version {
            1 => {
                // Every clip was an audio clip.
                let tracks = value
                    .pointer_mut("/project/tracks")
                    .and_then(|t| t.as_array_mut());
                for track in tracks.into_iter().flatten() {
                    let clips = track.get_mut("clips").and_then(|c| c.as_array_mut());
                    for clip in clips.into_iter().flatten() {
                        if let Some(clip) = clip.as_object_mut() {
                            clip.entry("kind").or_insert_with(|| "audio".into());
                        }
                    }
                }
            }
            v => return Err(format!("unsupported format version {v}")),
        }
        version += 1;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Clip, ClipContent, Note, Track, TrackId, TrackKind};

    fn sample() -> Project {
        let mut p = Project::new("Demo");
        p.tempo_bpm = 93.5;
        p.tempo_changes = vec![crate::tempo::TempoChange {
            beat: 8.0,
            bpm: 140.0,
            ramp: true,
        }];
        let id = p.alloc_track_id();
        let (clip, note) = (p.alloc_clip_id(), p.alloc_note_id());
        p.tracks.push(Track {
            gain_db: -3.5,
            pan: 0.25,
            mute: true,
            input: crate::model::InputChannels::Left,
            monitor: true,
            clips: vec![Clip {
                id: clip,
                start: 4800,
                length: 9600,
                gain_db: 0.0,
                muted: false,
                lane: 2,
                content: ClipContent::Midi {
                    notes: vec![Note {
                        id: note,
                        pitch: 60,
                        velocity: 90,
                        start: 0,
                        length: 2400,
                    }],
                    controls: vec![crate::model::ControlPoint {
                        controller: crate::model::Controller::PitchBend,
                        frame: 1200,
                        value: 10_000,
                    }],
                },
            }],
            ..Track::new(id, "Bass".into(), TrackKind::Midi)
        });
        p
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Demo.dawkey");
        let project = sample();
        save(&project, &bundle).unwrap();
        assert!(bundle.join(AUDIO_DIR).is_dir());

        let (loaded, path) = load(&bundle).unwrap();
        assert_eq!(loaded, project);
        assert_eq!(path, bundle);

        let (via_doc, path) = load(&bundle.join(DOCUMENT_NAME)).unwrap();
        assert_eq!(via_doc, project);
        assert_eq!(path, bundle);
    }

    #[test]
    fn bundle_path_adds_extension_once() {
        assert_eq!(bundle_path(Path::new("a/song")), Path::new("a/song.dawkey"));
        assert_eq!(
            bundle_path(Path::new("song.dawkey")),
            Path::new("song.dawkey")
        );
        assert_eq!(bundle_path(Path::new("v1.2")), Path::new("v1.2.dawkey"));
    }

    fn write_doc(dir: &Path, json: serde_json::Value) -> PathBuf {
        let bundle = dir.join("x.dawkey");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join(DOCUMENT_NAME), json.to_string()).unwrap();
        bundle
    }

    #[test]
    fn rejects_newer_and_foreign_documents() {
        let dir = tempfile::tempdir().unwrap();
        let project = serde_json::to_value(sample()).unwrap();

        let newer = write_doc(
            dir.path(),
            serde_json::json!({"format": FORMAT_TAG, "version": 99, "project": project}),
        );
        let err = load(&newer).unwrap_err().to_string();
        assert!(err.contains("newer Dawkey"), "{err}");

        let foreign = write_doc(
            dir.path(),
            serde_json::json!({"format": "other", "version": 1, "project": project}),
        );
        assert!(load(&foreign).is_err());
    }

    #[test]
    fn migrates_version_1_audio_clips() {
        let dir = tempfile::tempdir().unwrap();
        let v1 = serde_json::json!({
            "format": FORMAT_TAG,
            "version": 1,
            "project": {
                "name": "Old", "sample_rate": 48000, "tempo_bpm": 120.0,
                "time_signature": {"numerator": 4, "denominator": 4},
                "next_track_id": 2, "next_source_id": 2, "next_clip_id": 2,
                "sources": [{"id": 1, "name": "a", "path": "audio/a.wav", "channels": 1, "frames": 10}],
                "tracks": [{
                    "id": 1, "name": "A", "kind": "audio", "gain_db": 0.0, "pan": 0.0,
                    "mute": false, "solo": false,
                    "clips": [{"id": 1, "source": 1, "start": 0, "offset": 2, "length": 8, "gain_db": 0.0}]
                }]
            }
        });
        let bundle = write_doc(dir.path(), v1);
        let (loaded, _) = load(&bundle).unwrap();
        let clip = &loaded.tracks[0].clips[0];
        assert_eq!(
            clip.content,
            ClipContent::Audio {
                source: crate::model::SourceId(1),
                offset: 2
            }
        );
        assert_eq!(loaded.tracks[0].synth, None);
    }

    #[test]
    fn repairs_next_id_and_rejects_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let mut value = serde_json::to_value(sample()).unwrap();
        value["next_track_id"] = 0.into();
        let bundle = write_doc(
            dir.path(),
            serde_json::json!({"format": FORMAT_TAG, "version": 1, "project": value}),
        );
        let (mut loaded, _) = load(&bundle).unwrap();
        assert_eq!(loaded.alloc_track_id(), TrackId(2));

        let track = value["tracks"][0].clone();
        value["tracks"] = serde_json::json!([track, track]);
        let bundle = write_doc(
            dir.path(),
            serde_json::json!({"format": FORMAT_TAG, "version": 1, "project": value}),
        );
        let err = load(&bundle).unwrap_err().to_string();
        assert!(err.contains("duplicate track id"), "{err}");
    }
}
