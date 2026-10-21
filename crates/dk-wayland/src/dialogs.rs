// SPDX-License-Identifier: AGPL-3.0-or-later
//! File dialogs through the XDG desktop portal (no GTK), and the background
//! work that follows them: decoding imports and loading projects.
//!
//! Dialogs run on one long-lived thread with its own tokio runtime (ashpd
//! keeps one D-Bus connection for the process, bound to the runtime that made
//! it). Results come back to the app as jobs on its [`Handle`].

use std::path::PathBuf;
use std::sync::mpsc;

use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
use dk_app::{App, ImportRequest, PlatformRequest};
use dk_core::Handle;
use dk_studio::LoadedProject;

/// Runs portal dialogs and background loads for the GUI.
pub(crate) struct Dialogs {
    requests: mpsc::Sender<PlatformRequest>,
    app: Handle<App>,
}

impl Dialogs {
    pub(crate) fn start(app: Handle<App>) -> Self {
        let (tx, rx) = mpsc::channel::<PlatformRequest>();
        let handle = app.clone();
        std::thread::Builder::new()
            .name("dawkey-dialogs".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::warn!("no runtime for file dialogs: {e}");
                        return;
                    }
                };
                for request in rx {
                    runtime.block_on(run_dialog(&handle, request));
                }
            })
            .expect("failed to spawn dialog thread");
        Self { requests: tx, app }
    }

    pub(crate) fn request(&self, request: PlatformRequest) {
        let _ = self.requests.send(request);
    }

    /// Decodes each import on its own thread, then places it.
    pub(crate) fn import(&self, requests: Vec<ImportRequest>, sample_rate: u32) {
        for request in requests {
            let app = self.app.clone();
            std::thread::Builder::new()
                .name("dawkey-import".into())
                .spawn(move || {
                    if request.is_midi() {
                        let result = dk_media::read_midi_file(&request.path);
                        dispatch(&app, move |a| {
                            let placed = result
                                .map_err(|e| e.to_string())
                                .and_then(|file| a.finish_midi_import(&request, &file));
                            if let Err(e) = placed {
                                a.set_message(e);
                            }
                        });
                        return;
                    }
                    let result = dk_media::decode_file(&request.path, sample_rate);
                    dispatch(&app, move |a| match result {
                        Ok(decoded) => {
                            if let Err(e) = a.finish_import(&request, decoded) {
                                a.set_message(e);
                            }
                        }
                        Err(e) => a.set_message(e.to_string()),
                    });
                })
                .expect("failed to spawn import thread");
        }
    }
}

/// Follows the desktop's light or dark setting (the settings portal's
/// `color-scheme`) on a thread of its own, telling the app now and when it
/// changes. Without the portal, the app keeps its default (dark).
pub(crate) fn watch_color_scheme(app: Handle<App>) {
    use ashpd::desktop::settings::{ColorScheme, Settings};
    use futures_util::StreamExt;
    let dark = |scheme: ColorScheme| match scheme {
        ColorScheme::PreferDark => Some(true),
        ColorScheme::PreferLight => Some(false),
        ColorScheme::NoPreference => None,
    };
    let spawned = std::thread::Builder::new()
        .name("dawkey-appearance".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("no runtime for the desktop's color scheme: {e}");
                    return;
                }
            };
            runtime.block_on(async {
                let watched = async {
                    let settings = Settings::new().await?;
                    let scheme = settings.color_scheme().await?;
                    dispatch(&app, move |a| a.set_desktop_dark(dark(scheme)));
                    let mut changes = settings.receive_color_scheme_changed().await?;
                    while let Some(scheme) = changes.next().await {
                        dispatch(&app, move |a| a.set_desktop_dark(dark(scheme)));
                    }
                    Ok::<_, ashpd::Error>(())
                };
                if let Err(e) = watched.await {
                    tracing::info!("not following the desktop's color scheme: {e}");
                }
            });
        });
    if let Err(e) = spawned {
        tracing::warn!("watching the desktop's color scheme: {e}");
    }
}

/// Runs `f` on the app's thread without waiting for it.
fn dispatch(app: &Handle<App>, f: impl FnOnce(&mut App) + Send + 'static) {
    // Fails only when the app has shut down, and then nobody needs it.
    let _ = app.send(f);
}

async fn run_dialog(app: &Handle<App>, request: PlatformRequest) {
    let result = match request {
        PlatformRequest::ChooseAudioFiles { track, position_s } => {
            choose_audio_files().await.map(|paths| {
                let imports: Vec<ImportRequest> = paths
                    .into_iter()
                    .enumerate()
                    .map(|(i, path)| ImportRequest {
                        path,
                        // The first file goes where asked; the rest get new tracks.
                        track: if i == 0 { track } else { None },
                        position_s,
                    })
                    .collect();
                dispatch(app, move |a| a.queue_imports(imports));
            })
        }
        PlatformRequest::ChooseSaveLocation { then } => {
            choose_save_location(app).await.map(|path| {
                if let Some(path) = path {
                    dispatch(app, move |a| match a.save_as(&path) {
                        // Carry on with what the "save changes?" dialog was for.
                        Ok(()) => {
                            if let Some(action) = then {
                                a.proceed(action);
                            }
                        }
                        Err(e) => a.set_message(e),
                    });
                }
            })
        }
        PlatformRequest::ChooseFolder { kind } => choose_folder(kind).await.map(|path| {
            if let Some(path) = path {
                dispatch(app, move |a| a.add_plugin_folder(kind, &path));
            }
        }),
        PlatformRequest::ChooseExportLocation { name, format } => {
            choose_export_location(&name, format).await.map(|path| {
                if let Some(path) = path {
                    dispatch(app, move |a| {
                        if let Err(e) = a.start_export(&path) {
                            a.set_message(e);
                        }
                    });
                }
            })
        }
        PlatformRequest::ChooseProjectToOpen => choose_project().await.map(|path| {
            let Some(path) = path else { return };
            let app = app.clone();
            std::thread::spawn(move || match LoadedProject::open(&path) {
                Ok(loaded) => dispatch(&app, move |a| {
                    if let Err(e) = a.replace_project(loaded) {
                        a.set_message(e);
                    }
                }),
                Err(e) => dispatch(&app, move |a| a.set_message(e.to_string())),
            });
        }),
    };
    if let Err(e) = result {
        match e {
            ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => {}
            e => {
                tracing::warn!("file dialog: {e}");
                let message = format!("File dialog failed: {e}");
                dispatch(app, move |a| a.set_message(message));
            }
        }
    }
}

async fn choose_audio_files() -> ashpd::Result<Vec<PathBuf>> {
    let files = SelectedFiles::open_file()
        .title("Import Audio or MIDI")
        .accept_label("Import")
        .modal(true)
        .multiple(true)
        .filter(
            FileFilter::new("Audio files")
                .mimetype("audio/*")
                .glob("*.wav")
                .glob("*.flac")
                .glob("*.mp3")
                .glob("*.ogg")
                .glob("*.aif")
                .glob("*.aiff"),
        )
        .filter(
            FileFilter::new("MIDI files")
                .mimetype("audio/midi")
                .glob("*.mid")
                .glob("*.midi"),
        )
        .filter(FileFilter::new("All files").glob("*"))
        .send()
        .await?
        .response()?;
    Ok(files.uris().iter().filter_map(uri_to_path).collect())
}

async fn choose_project() -> ashpd::Result<Option<PathBuf>> {
    let selected = SelectedFiles::open_file()
        .title("Open Project (a .dawkey folder)")
        .accept_label("Open")
        .modal(true)
        .directory(true)
        .send()
        .await?
        .response()?;
    Ok(selected.uris().first().and_then(uri_to_path))
}

async fn choose_save_location(app: &Handle<App>) -> ashpd::Result<Option<PathBuf>> {
    let name = app
        .with(|a| a.studio.session().project().name.clone())
        .await
        .unwrap_or_else(|_| "Untitled".into());
    let selected = SelectedFiles::save_file()
        .title("Save Project")
        .accept_label("Save")
        .modal(true)
        .current_name(format!("{name}.dawkey").as_str())
        .filter(FileFilter::new("Dawkey project").glob("*.dawkey"))
        .send()
        .await?
        .response()?;
    Ok(selected.uris().first().and_then(uri_to_path))
}

async fn choose_folder(kind: dk_app::PluginKind) -> ashpd::Result<Option<PathBuf>> {
    let title = match kind {
        dk_app::PluginKind::Clap => "Add a Folder of CLAP Plugins",
        dk_app::PluginKind::Lv2 => "Add a Folder of LV2 Plugins",
        dk_app::PluginKind::Vst3 => "Add a Folder of VST3 Plugins",
    };
    let selected = SelectedFiles::open_file()
        .title(title)
        .accept_label("Add")
        .modal(true)
        .directory(true)
        .send()
        .await?
        .response()?;
    Ok(selected.uris().first().and_then(uri_to_path))
}

async fn choose_export_location(
    name: &str,
    format: dk_media::FileFormat,
) -> ashpd::Result<Option<PathBuf>> {
    let filter = match format {
        dk_media::FileFormat::Wav => FileFilter::new("WAV audio").glob("*.wav"),
        dk_media::FileFormat::Flac => FileFilter::new("FLAC audio").glob("*.flac"),
    };
    let selected = SelectedFiles::save_file()
        .title("Export the Mix")
        .accept_label("Export")
        .modal(true)
        .current_name(name)
        .filter(filter)
        .send()
        .await?
        .response()?;
    Ok(selected.uris().first().and_then(uri_to_path))
}

fn uri_to_path(uri: &ashpd::Uri) -> Option<PathBuf> {
    url::Url::parse(uri.as_str()).ok()?.to_file_path().ok()
}

/// Paths from a `text/uri-list` drop.
pub(crate) fn parse_uri_list(data: &[u8]) -> Vec<PathBuf> {
    String::from_utf8_lossy(data)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| url::Url::parse(line).ok()?.to_file_path().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_uri_lists() {
        let data = b"# comment\r\nfile:///home/me/My%20Song.wav\r\nhttps://example.com/x.wav\r\nfile:///tmp/b.flac\n";
        assert_eq!(
            parse_uri_list(data),
            [
                PathBuf::from("/home/me/My Song.wav"),
                PathBuf::from("/tmp/b.flac")
            ]
        );
    }
}
