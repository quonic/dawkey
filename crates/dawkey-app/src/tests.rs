// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

use dawkey_core::{
    AddClip, AddMidiClip, AddTrack, ClipId, NoteSpec, Project, TrackId, TrackKind, Waveform,
};
use dawkey_media::{AudioData, DecodedFile};
use dawkey_studio::{AudioBackend, LoadedProject, Studio};
use dawkey_ui::{ChromeAction, Modifiers, MouseButton, Point, Role};

use crate::state::{FADER_H, ROW_H, SEND_W};
use crate::view::format_time;
use crate::{App, DragFrom, Page, PendingAction, Platform, PlatformRequest, WindowInfo};

fn click(app: &mut App, id: &str) -> Result<Vec<ChromeAction>, String> {
    app.click(id, MouseButton::Left, Modifiers::default())
}

/// Presses each key in turn.
fn type_keys(app: &mut App, keys: &[&str]) {
    for key in keys {
        app.key(key, Modifiers::default()).unwrap();
    }
}

const CTRL: Modifiers = Modifiers {
    ctrl: true,
    shift: false,
    alt: false,
    logo: false,
};

fn app() -> App {
    let studio = Studio::new(
        LoadedProject::new(Project::new("Test")),
        AudioBackend::Virtual,
    )
    .unwrap();
    App::new(studio, Platform::Headless)
}

fn add_track(app: &mut App, name: &str) -> u64 {
    app.studio
        .execute(
            AddTrack {
                name: Some(name.into()),
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap()
        .0
}

#[test]
fn tree_has_stable_ids() {
    let mut app = app();
    let tree = app.access_tree();
    for id in [
        "window",
        "window/titlebar",
        "window/close",
        "transport/play",
        "transport/stop",
        "transport/position",
        "master/meter",
        "tracks/empty",
        "status/engine",
    ] {
        assert!(tree.find(id).is_some(), "missing {id}");
    }
    let title = tree.find("window/title").unwrap();
    assert_eq!(title.name.as_deref(), Some("Test — Dawkey"));
    assert_eq!(
        tree.find("status/engine").unwrap().name.as_deref(),
        Some("Virtual clock · 48 kHz")
    );

    let id = add_track(&mut app, "Drums");
    let tree = app.access_tree();
    assert!(tree.find("tracks/empty").is_none());
    let row = tree.find(&format!("track/{id}")).unwrap();
    assert_eq!(row.role, Role::ListItem);
    assert_eq!(row.name.as_deref(), Some("Drums"));
    assert_eq!(
        tree.find("window/title").unwrap().name.as_deref(),
        Some("• Test — Dawkey")
    );
}

#[test]
fn clicks_drive_the_studio() {
    let mut app = app();
    let id = add_track(&mut app, "Bass");

    click(&mut app, "transport/play").unwrap();
    assert!(app.studio.transport().playing);
    assert_eq!(
        app.access_tree().find("transport/play").unwrap().checked,
        Some(true)
    );
    click(&mut app, "transport/play").unwrap();
    assert!(!app.studio.transport().playing);

    click(&mut app, &format!("track/{id}/mute")).unwrap();
    assert!(app.studio.session().project().tracks[0].mute);
    let tree = app.access_tree();
    assert_eq!(
        tree.find(&format!("track/{id}/mute")).unwrap().checked,
        Some(true)
    );
    // Toggling is undoable like any command.
    app.studio.undo().unwrap();
    assert!(!app.studio.session().project().tracks[0].mute);

    // Unsaved changes: closing asks first (see
    // closing_over_unsaved_changes_asks_first).
    assert!(click(&mut app, "window/close").unwrap().is_empty());
    assert!(click(&mut app, "nope").is_err());
}

#[test]
fn window_state_changes_the_frame() {
    let mut app = app();
    let floating = app.access_tree().find("window").unwrap().bounds;
    assert_eq!((floating.x, floating.y), (16.0, 16.0));

    app.set_window(WindowInfo {
        maximized: true,
        ..WindowInfo::headless()
    });
    let tree = app.access_tree();
    let maximized = tree.find("window").unwrap().bounds;
    assert_eq!((maximized.x, maximized.y), (0.0, 0.0));
    assert_eq!(
        tree.find("window/maximize").unwrap().name.as_deref(),
        Some("Restore")
    );

    // Server-side decorations: no title bar of our own.
    app.set_window(WindowInfo {
        csd: false,
        ..WindowInfo::headless()
    });
    assert!(app.access_tree().find("window/titlebar").is_none());
}

#[test]
fn screenshot_renders_the_window() {
    let mut app = app();
    add_track(&mut app, "Keys");
    let image = app.screenshot(1.0).unwrap();
    assert_eq!((image.width, image.height), (1280, 800));
    // The shadow margin is (mostly) transparent; the window body is opaque.
    assert!(image.pixel(1, 1)[3] < 40);
    assert_eq!(image.pixel(640, 400)[3], 255);
    let half = app.screenshot(0.5).unwrap();
    assert_eq!((half.width, half.height), (640, 400));
}

#[test]
fn time_format() {
    assert_eq!(format_time(0.0), "00:00.000");
    assert_eq!(format_time(83.25), "01:23.250");
    assert_eq!(format_time(3725.5), "1:02:05.500");
}

/// Adds a 2 s source (a ramp) and places it at `at_s` on a new track.
fn add_clip(app: &mut App, at_s: f64) -> (u64, u64) {
    let samples: Vec<f32> = (0..96_000)
        .map(|i| (i % 1000) as f32 / 1000.0 - 0.5)
        .collect();
    let source = app.studio.add_source(
        Path::new("/tmp/ramp.wav"),
        DecodedFile {
            audio: AudioData::new(vec![samples]),
            source_rate: 48_000,
        },
    );
    let outcome = app
        .studio
        .execute(AddClip::new(source, None, at_s).into())
        .unwrap();
    (outcome.track.unwrap().0, outcome.clip.unwrap().0)
}

fn clip_start_s(app: &App, clip: u64) -> f64 {
    let project = app.studio.session().project();
    let (_, c) = project.clip(dawkey_core::ClipId(clip)).unwrap();
    project.frames_to_seconds(c.start)
}

#[test]
fn arrange_shows_clips_with_waveforms() {
    let mut app = app();
    let (track, clip) = add_clip(&mut app, 1.0);
    let tree = app.access_tree();
    let node = tree.find(&format!("clip/{clip}")).unwrap();
    assert_eq!(node.role, Role::Clip);
    assert_eq!(node.name.as_deref(), Some("ramp"));
    assert_eq!(node.value.as_deref(), Some("1.000 s - 3.000 s"));
    // 80 px per second: 2 s is 160 px wide.
    assert_eq!(node.bounds.w, 160.0);
    let lane = tree.find(&format!("track/{track}/lane")).unwrap();
    assert_eq!(node.bounds.x - lane.bounds.x, 80.0);
    assert!(tree.find("timeline/ruler").is_some());
    let waveform_bars = app
        .scene()
        .primitives
        .iter()
        .filter(|p| matches!(p, dawkey_ui::Primitive::Rect { rect, .. } if rect.w == 1.0))
        .count();
    assert!(waveform_bars >= 160, "{waveform_bars}");
}

#[test]
fn dragging_a_clip_moves_it_in_one_undo_step() {
    let mut app = app();
    let (_, clip) = add_clip(&mut app, 1.0);
    let id = format!("clip/{clip}");
    // 170 px = 2.125 s, snapped to the half-second beat at 120 BPM.
    app.drag(
        DragFrom::Widget(id.clone()),
        Point::new(170.0, 0.0),
        6,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(clip_start_s(&app, clip), 3.0);
    assert_eq!(app.studio.session().undo_label(), Some("move_clip"));
    assert_eq!(
        app.view_state().selected_clip,
        Some(dawkey_core::ClipId(clip))
    );

    // Shift disables snapping.
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    app.drag(DragFrom::Widget(id), Point::new(-20.0, 0.0), 3, shift)
        .unwrap();
    assert!((clip_start_s(&app, clip) - 2.75).abs() < 1e-6);

    app.studio.undo().unwrap();
    assert_eq!(clip_start_s(&app, clip), 3.0);
    app.studio.undo().unwrap();
    assert_eq!(clip_start_s(&app, clip), 1.0);
}

#[test]
fn renaming_tracks_and_clips() {
    let mut app = app();
    let (track, clip) = add_clip(&mut app, 1.0);
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let track_name = |app: &App| {
        let project = app.studio.session().project();
        project.track(TrackId(track)).unwrap().name.clone()
    };
    let shown = |app: &mut App, id: &str| app.access_tree().find(id).unwrap().name.clone();
    let message = |app: &mut App| shown(app, "status/message").unwrap_or_default();

    // Double-clicking a track's name types a new one, in one undo step.
    let name = format!("track/{track}/name");
    app.double_click(&name, Modifiers::default()).unwrap();
    let field = app.access_tree().find(&name).unwrap().clone();
    assert_eq!(
        (field.role, field.value.as_deref()),
        (Role::TextField, Some("ramp"))
    );
    app.key("d", shift).unwrap();
    type_keys(&mut app, &["r", "u", "m", "s", "space", "2", "return"]);
    assert_eq!(track_name(&app), "Drums 2");
    assert_eq!(app.studio.session().undo_label(), Some("rename_track"));
    assert_eq!(shown(&mut app, &name).as_deref(), Some("Drums 2"));

    // The mixer's strip renames it the same way. Escape leaves it as it
    // was, and an empty name is refused.
    click(&mut app, "view/mixer").unwrap();
    assert_eq!(shown(&mut app, &name).as_deref(), Some("Drums 2"));
    app.double_click(&name, Modifiers::default()).unwrap();
    type_keys(&mut app, &["x", "escape"]);
    assert_eq!(track_name(&app), "Drums 2");
    app.double_click(&name, Modifiers::default()).unwrap();
    type_keys(&mut app, &["backspace", "return"]);
    assert_eq!(track_name(&app), "Drums 2");
    let msg = message(&mut app);
    assert!(msg.contains("empty"), "{msg}");
    click(&mut app, "view/arrange").unwrap();

    // A clip's name is a handle of its own: double-click it to type one.
    let clip_name = format!("clip/{clip}/name");
    let named = |app: &App| {
        let project = app.studio.session().project();
        project.clip(ClipId(clip)).unwrap().1.name.clone()
    };
    app.double_click(&clip_name, Modifiers::default()).unwrap();
    assert_eq!(app.view_state().selected_clip, Some(ClipId(clip)));
    let field = app.access_tree().find(&clip_name).unwrap().clone();
    assert_eq!(
        (field.role, field.value.as_deref()),
        (Role::TextField, Some("ramp"))
    );
    type_keys(&mut app, &["k", "i", "c", "k", "return"]);
    assert_eq!(named(&app).as_deref(), Some("kick"));
    assert_eq!(app.studio.session().undo_label(), Some("rename_clip"));
    assert_eq!(
        shown(&mut app, &format!("clip/{clip}")).as_deref(),
        Some("kick")
    );

    // F2 renames the selected clip; an empty name goes back to the file's.
    app.key("f2", Modifiers::default()).unwrap();
    assert_eq!(
        app.access_tree().find(&clip_name).unwrap().value.as_deref(),
        Some("kick")
    );
    type_keys(&mut app, &["backspace", "return"]);
    assert_eq!(named(&app), None);
    assert_eq!(
        shown(&mut app, &format!("clip/{clip}")).as_deref(),
        Some("ramp")
    );

    // Clicking elsewhere sets what was typed.
    app.double_click(&clip_name, Modifiers::default()).unwrap();
    type_keys(&mut app, &["a"]);
    click(&mut app, "view/arrange").unwrap();
    assert_eq!(named(&app).as_deref(), Some("a"));

    // Dragging the name moves the clip.
    app.drag(
        DragFrom::Widget(clip_name),
        Point::new(80.0, 0.0),
        4,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(clip_start_s(&app, clip), 2.0);

    // On a MIDI clip, double-clicking the name renames it and doesn't open
    // the piano roll; double-clicking the rest of it still does.
    let (_, midi) = add_midi(&mut app, 0.0, &[(60, 0.0, 1.0)]);
    app.double_click(&format!("clip/{midi}/name"), Modifiers::default())
        .unwrap();
    assert_eq!(app.view_state().page, Page::Arrange);
    assert!(
        app.access_tree()
            .find(&format!("clip/{midi}/name"))
            .unwrap()
            .role
            == Role::TextField
    );
    app.key("escape", Modifiers::default()).unwrap();
    app.double_click(&format!("clip/{midi}"), Modifiers::default())
        .unwrap();
    assert_eq!(app.view_state().page, Page::PianoRoll);
}

#[test]
fn dragging_a_clip_to_another_lane_changes_track() {
    let mut app = app();
    let (_, clip) = add_clip(&mut app, 0.0);
    let (track_b, _) = add_clip(&mut app, 4.0);
    app.drag(
        DragFrom::Widget(format!("clip/{clip}")),
        Point::new(0.0, crate::state::LANE_H),
        4,
        Modifiers::default(),
    )
    .unwrap();
    let project = app.studio.session().project();
    assert_eq!(
        project.clip(dawkey_core::ClipId(clip)).unwrap().0.id.0,
        track_b
    );
}

#[test]
fn mixer_faders_and_pan() {
    let mut app = app();
    let (track, _) = add_clip(&mut app, 0.0);
    click(&mut app, "view/mixer").unwrap();
    assert_eq!(app.view_state().page, Page::Mixer);
    let fader = format!("track/{track}/fader");
    // From unity (75 %) to the top: +12 dB, in one undo step.
    app.drag(
        DragFrom::Widget(fader.clone()),
        Point::new(0.0, -FADER_H * 0.25),
        8,
        Modifiers::default(),
    )
    .unwrap();
    let gain = |app: &App| app.studio.session().project().tracks[0].gain_db;
    assert_eq!(gain(&app), 12.0);
    let tree = app.access_tree();
    assert_eq!(tree.find(&fader).unwrap().value.as_deref(), Some("12.0 dB"));
    app.studio.undo().unwrap();
    assert_eq!(gain(&app), 0.0);

    app.drag(
        DragFrom::Widget(format!("track/{track}/pan")),
        Point::new(-19.0, 0.0),
        4,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(app.studio.session().project().tracks[0].pan, -0.5);
}

#[test]
fn keyboard_shortcuts() {
    let mut app = app();
    let (_, clip) = add_clip(&mut app, 0.0);
    app.key("space", Modifiers::default()).unwrap();
    assert!(app.studio.transport().playing);
    app.key("space", Modifiers::default()).unwrap();
    assert!(!app.studio.transport().playing);

    app.key("tab", Modifiers::default()).unwrap();
    assert_eq!(app.view_state().page, Page::Mixer);
    app.key("tab", Modifiers::default()).unwrap();

    click(&mut app, &format!("clip/{clip}")).unwrap();
    app.key("delete", Modifiers::default()).unwrap();
    assert!(
        app.studio
            .session()
            .project()
            .clip(dawkey_core::ClipId(clip))
            .is_none()
    );
    app.key("z", CTRL).unwrap();
    assert!(
        app.studio
            .session()
            .project()
            .clip(dawkey_core::ClipId(clip))
            .is_some()
    );

    // Never saved: Ctrl+S asks the platform for a location.
    app.key("s", CTRL).unwrap();
    assert_eq!(
        app.take_platform_requests(),
        [PlatformRequest::ChooseSaveLocation { then: None }]
    );
}

#[test]
fn opening_over_unsaved_changes_asks_first() {
    let mut app = app();
    // A clean project opens straight away.
    app.key("o", CTRL).unwrap();
    assert_eq!(
        app.take_platform_requests(),
        [PlatformRequest::ChooseProjectToOpen]
    );
    assert!(app.access_tree().find("confirm").is_none());

    add_track(&mut app, "Keys");
    app.key("o", CTRL).unwrap();
    assert!(app.take_platform_requests().is_empty());
    assert_eq!(app.view_state().confirm, Some(PendingAction::OpenProject));
    let tree = app.access_tree();
    for id in ["confirm/save", "confirm/discard", "confirm/cancel"] {
        assert!(tree.find(id).is_some(), "missing {id}");
    }
    // While it's up, nothing under it reacts: not clicks, nor keys.
    let covered = click(&mut app, "transport/play").unwrap_err();
    assert!(covered.contains("confirm/backdrop"), "{covered}");
    app.key("space", Modifiers::default()).unwrap();
    assert!(!app.studio.transport().playing);

    // Cancel (or Escape) leaves everything as it was.
    click(&mut app, "confirm/cancel").unwrap();
    assert_eq!(app.view_state().confirm, None);
    assert!(app.studio.session().is_dirty());
    app.key("o", CTRL).unwrap();
    app.key("escape", Modifiers::default()).unwrap();
    assert_eq!(app.view_state().confirm, None);
    assert!(app.take_platform_requests().is_empty());

    // Don't save: on to the open dialog.
    app.key("o", CTRL).unwrap();
    click(&mut app, "confirm/discard").unwrap();
    assert_eq!(
        app.take_platform_requests(),
        [PlatformRequest::ChooseProjectToOpen]
    );
    assert!(app.studio.session().is_dirty(), "nothing replaced yet");

    // Save, never saved before: the save dialog first, then the platform
    // carries on with opening.
    app.key("o", CTRL).unwrap();
    click(&mut app, "confirm/save").unwrap();
    assert_eq!(
        app.take_platform_requests(),
        [PlatformRequest::ChooseSaveLocation {
            then: Some(PendingAction::OpenProject)
        }]
    );
    let dir = tempfile::tempdir().unwrap();
    app.save_as(&dir.path().join("Keys.dawkey")).unwrap();
    app.proceed(PendingAction::OpenProject);
    assert_eq!(
        app.take_platform_requests(),
        [PlatformRequest::ChooseProjectToOpen]
    );

    // Saved before: Return saves in place and opens.
    add_track(&mut app, "More");
    app.key("o", CTRL).unwrap();
    app.key("return", Modifiers::default()).unwrap();
    assert!(!app.studio.session().is_dirty());
    assert_eq!(
        app.take_platform_requests(),
        [PlatformRequest::ChooseProjectToOpen]
    );
}

#[test]
fn closing_over_unsaved_changes_asks_first() {
    let mut app = app();
    assert!(app.request_close());
    assert_eq!(
        click(&mut app, "window/close").unwrap(),
        [ChromeAction::Close]
    );
    add_track(&mut app, "Keys");
    // The close button and the compositor both ask.
    assert!(click(&mut app, "window/close").unwrap().is_empty());
    assert_eq!(app.view_state().confirm, Some(PendingAction::Close));
    click(&mut app, "confirm/cancel").unwrap();
    assert!(!app.request_close());
    assert_eq!(app.view_state().confirm, Some(PendingAction::Close));

    // Don't save closes.
    assert_eq!(
        click(&mut app, "confirm/discard").unwrap(),
        [ChromeAction::Close]
    );

    // Saving through the save dialog closes once it's saved.
    let mut app = self::app();
    add_track(&mut app, "Keys");
    click(&mut app, "window/close").unwrap();
    assert!(click(&mut app, "confirm/save").unwrap().is_empty());
    let dir = tempfile::tempdir().unwrap();
    app.save_as(&dir.path().join("Keys.dawkey")).unwrap();
    app.take_chrome_requests();
    app.proceed(PendingAction::Close);
    assert_eq!(app.take_chrome_requests(), [ChromeAction::Close]);
    assert!(app.request_close(), "saved, so no need to ask");
}

#[test]
fn trim_clips_by_dragging_their_edges() {
    let mut app = app();
    // Two seconds of audio at 1 s; 80 px per second; snapping to beats
    // (half a second at 120 BPM).
    let (_, clip) = add_clip(&mut app, 1.0);
    let edges = |app: &App| {
        let project = app.studio.session().project();
        let c = project.clip(ClipId(clip)).unwrap().1.clone();
        let dawkey_core::ClipContent::Audio { offset, .. } = c.content else {
            panic!("audio clip")
        };
        (c.start, c.length, offset)
    };
    let tree = app.access_tree();
    let end = tree.find(&format!("clip/{clip}/end")).unwrap();
    assert_eq!(end.name.as_deref(), Some("Clip end"));
    let handle = app.ui.find(&format!("clip/{clip}/end")).unwrap().rect();
    app.ui
        .handle(dawkey_ui::InputEvent::PointerMove(handle.center()));
    assert_eq!(app.cursor(), Some(dawkey_ui::Cursor::ResizeHorizontal));

    // The end half a second earlier.
    app.drag(
        DragFrom::Widget(format!("clip/{clip}/end")),
        Point::new(-40.0, 0.0),
        4,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(edges(&app), (48_000, 72_000, 0));
    assert_eq!(app.studio.session().undo_label(), Some("trim_clip"));
    assert_eq!(app.view_state().selected_clip, Some(ClipId(clip)));

    // The start half a second later: the audio stays where it was.
    app.drag(
        DragFrom::Widget(format!("clip/{clip}/start")),
        Point::new(40.0, 0.0),
        4,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(edges(&app), (72_000, 48_000, 24_000));

    // Not past the source's ends.
    app.drag(
        DragFrom::Widget(format!("clip/{clip}/start")),
        Point::new(-400.0, 0.0),
        4,
        Modifiers::default(),
    )
    .unwrap();
    app.drag(
        DragFrom::Widget(format!("clip/{clip}/end")),
        Point::new(400.0, 0.0),
        4,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(edges(&app), (48_000, 96_000, 0));
    app.studio.undo().unwrap();
    assert_eq!(edges(&app), (48_000, 72_000, 0));
}

/// Window x of timeline time `t`.
fn x_at(app: &App, t: f64) -> f32 {
    let lanes = app.lanes_rect();
    lanes.x + ((t - app.view_state().scroll_s) * f64::from(app.view_state().px_per_s)) as f32
}

/// Window y in the middle of the `n`th track's lane.
fn lane_y(app: &App, n: usize) -> f32 {
    app.lanes_rect().y + crate::state::LANE_H * (n as f32 + 0.5)
}

/// The clips on track `track`: (start, length) in seconds, by start.
fn clips_on(app: &App, track: u64) -> Vec<(f64, f64)> {
    let project = app.studio.session().project();
    let mut clips: Vec<_> = project
        .track(dawkey_core::TrackId(track))
        .unwrap()
        .clips
        .iter()
        .map(|c| {
            (
                project.frames_to_seconds(c.start),
                project.frames_to_seconds(c.length),
            )
        })
        .collect();
    clips.sort_by(|a, b| a.0.total_cmp(&b.0));
    clips
}

#[test]
fn copy_and_paste_clips_at_the_pointer() {
    let mut app = app();
    // Two-second clips at 1 s on track A and at 6 s on track B.
    let (track_a, clip) = add_clip(&mut app, 1.0);
    let (track_b, _) = add_clip(&mut app, 6.0);
    // Nothing copied yet.
    app.key("v", CTRL).unwrap();
    assert_eq!(clips_on(&app, track_a), vec![(1.0, 2.0)]);
    click(&mut app, &format!("clip/{clip}")).unwrap();
    app.key("c", CTRL).unwrap();
    assert_eq!(app.view_state().clipboard, Some(ClipId(clip)));

    // Over track B at 2 s: pasted there, and selected.
    app.hover(Point::new(x_at(&app, 2.0), lane_y(&app, 1)));
    app.key("v", CTRL).unwrap();
    assert_eq!(clips_on(&app, track_b), vec![(2.0, 2.0), (6.0, 2.0)]);
    assert_eq!(app.studio.session().undo_label(), Some("duplicate_clip"));
    let copy = app.view_state().selected_clip.unwrap();
    assert_ne!(copy, ClipId(clip));
    assert_eq!(
        app.studio.session().project().clip(copy).unwrap().0.id.0,
        track_b
    );

    // Again at 2.5 s, over that copy: in the nearest gap that fits,
    // flush against it.
    app.hover(Point::new(x_at(&app, 2.5), lane_y(&app, 1)));
    app.key("v", CTRL).unwrap();
    assert_eq!(
        clips_on(&app, track_b),
        vec![(2.0, 2.0), (4.0, 2.0), (6.0, 2.0)]
    );

    // Below the tracks: onto the clip's own track, at the pointer.
    app.hover(Point::new(x_at(&app, 8.0), lane_y(&app, 2)));
    app.key("v", CTRL).unwrap();
    assert_eq!(clips_on(&app, track_a), vec![(1.0, 2.0), (8.0, 2.0)]);

    // Not over the timeline: at the playhead.
    app.studio.locate(4.0).unwrap();
    app.hover(Point::new(app.lanes_rect().x - 10.0, lane_y(&app, 1)));
    app.key("v", CTRL).unwrap();
    assert_eq!(
        clips_on(&app, track_a),
        vec![(1.0, 2.0), (4.0, 2.0), (8.0, 2.0)]
    );

    // A MIDI track under the pointer isn't one an audio clip can go on.
    let midi = app
        .studio
        .execute(
            AddTrack {
                kind: TrackKind::Midi,
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap();
    app.hover(Point::new(x_at(&app, 11.0), lane_y(&app, 2)));
    app.key("v", CTRL).unwrap();
    assert!(clips_on(&app, midi.0).is_empty());
    assert_eq!(clips_on(&app, track_a).last(), Some(&(11.0, 2.0)));

    // One undo step each.
    let undone: Vec<_> = (0..6)
        .map(|_| app.studio.undo().unwrap().unwrap())
        .collect();
    assert_eq!(
        undone,
        ["duplicate_clip", "add_track"]
            .into_iter()
            .chain(["duplicate_clip"; 4])
            .collect::<Vec<_>>()
    );
    assert_eq!(clips_on(&app, track_a), vec![(1.0, 2.0)]);
    assert_eq!(clips_on(&app, track_b), vec![(6.0, 2.0)]);
}

#[test]
fn dragged_clips_land_clear_of_others_and_ctrl_copies() {
    let mut app = app();
    // Two-second clips at 1 s and 5 s on one track.
    let (track, first) = add_clip(&mut app, 1.0);
    let source = app
        .studio
        .session()
        .project()
        .clip(ClipId(first))
        .unwrap()
        .1
        .source()
        .unwrap();
    let second = app
        .studio
        .execute(AddClip::new(source, Some(dawkey_core::TrackId(track)), 5.0).into())
        .unwrap()
        .clip
        .unwrap();

    // Dragged 3 s on, the first would cover the second: it stops against it.
    app.drag(
        DragFrom::Widget(format!("clip/{first}")),
        Point::new(240.0, 0.0),
        6,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(clip_start_s(&app, first), 3.0);
    assert_eq!(app.studio.session().undo_label(), Some("move_clip"));

    // With Ctrl, the second stays and a copy goes in the nearest space:
    // after it rather than before the first.
    app.drag(
        DragFrom::Widget(format!("clip/{}", second.0)),
        Point::new(-40.0, 0.0),
        6,
        CTRL,
    )
    .unwrap();
    assert_eq!(
        clips_on(&app, track),
        vec![(3.0, 2.0), (5.0, 2.0), (7.0, 2.0)]
    );
    assert_eq!(app.studio.session().undo_label(), Some("duplicate_clip"));
    let copy = app.view_state().selected_clip.unwrap();
    assert_ne!(copy, second);
    app.studio.undo().unwrap();
    assert_eq!(clips_on(&app, track), vec![(3.0, 2.0), (5.0, 2.0)]);

    // Ctrl-dragged onto another track: a copy there.
    let (other, _) = add_clip(&mut app, 0.0);
    app.drag(
        DragFrom::Widget(format!("clip/{}", second.0)),
        Point::new(0.0, crate::state::LANE_H),
        4,
        CTRL,
    )
    .unwrap();
    assert_eq!(clips_on(&app, track), vec![(3.0, 2.0), (5.0, 2.0)]);
    assert_eq!(clips_on(&app, other), vec![(0.0, 2.0), (5.0, 2.0)]);
}

#[test]
fn clip_edges_stop_at_the_next_clip_and_ctrl_repeats() {
    let mut app = app();
    let track = app
        .studio
        .execute(
            AddTrack {
                kind: TrackKind::Midi,
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap();
    let midi_clip = |app: &mut App, position_s: f64| {
        app.studio
            .execute(
                AddMidiClip {
                    track: Some(track),
                    position_s,
                    length_s: Some(1.0),
                    notes: vec![NoteSpec {
                        pitch: 60,
                        velocity: 100,
                        start_beats: 0.0,
                        length_beats: 1.0,
                    }],
                    ..Default::default()
                }
                .into(),
            )
            .unwrap()
            .clip
            .unwrap()
    };
    // One-second MIDI clips at 0 s, 2 s and 4.5 s.
    let first = midi_clip(&mut app, 0.0);
    let second = midi_clip(&mut app, 2.0);
    midi_clip(&mut app, 4.5);

    // The first's end, dragged 3 s on, stops at the second's start.
    app.drag(
        DragFrom::Widget(format!("clip/{}/end", first.0)),
        Point::new(240.0, 0.0),
        6,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(
        clips_on(&app, track.0),
        vec![(0.0, 2.0), (2.0, 1.0), (4.5, 1.0)]
    );

    // Ctrl-dragging the second's end repeats it, up to the next clip; the
    // last copy is cut short there.
    app.drag(
        DragFrom::Widget(format!("clip/{}/end", second.0)),
        Point::new(400.0, 0.0),
        6,
        CTRL,
    )
    .unwrap();
    assert_eq!(
        clips_on(&app, track.0),
        vec![(0.0, 2.0), (2.0, 1.0), (3.0, 1.0), (4.0, 0.5), (4.5, 1.0)]
    );
    assert_eq!(app.studio.session().undo_label(), Some("repeat_clip"));
    app.studio.undo().unwrap();
    assert_eq!(clips_on(&app, track.0).len(), 3);
}

#[test]
fn split_clips_at_the_playhead() {
    let mut app = app();
    let (track, clip) = add_clip(&mut app, 1.0);
    assert!(app.access_tree().find("toolbar/split").is_none());
    click(&mut app, &format!("clip/{clip}")).unwrap();
    assert!(app.access_tree().find("toolbar/split").is_some());

    // The playhead isn't in the clip yet.
    app.key("s", Modifiers::default()).unwrap();
    assert_eq!(app.studio.session().project().tracks[0].clips.len(), 1);

    app.studio.locate(1.5).unwrap();
    app.key("s", Modifiers::default()).unwrap();
    let parts = |app: &App| -> Vec<(u64, u64)> {
        let project = app.studio.session().project();
        project
            .track(dawkey_core::TrackId(track))
            .unwrap()
            .clips
            .iter()
            .map(|c| (c.start, c.length))
            .collect()
    };
    assert_eq!(parts(&app), [(48_000, 24_000), (72_000, 72_000)]);
    // The part after the playhead is selected, so splitting further on
    // splits it again.
    let second = app.view_state().selected_clip.unwrap();
    assert_ne!(second, ClipId(clip));
    app.studio.locate(2.5).unwrap();
    click(&mut app, "toolbar/split").unwrap();
    assert_eq!(
        parts(&app),
        [(48_000, 24_000), (72_000, 48_000), (120_000, 24_000)]
    );
    app.studio.undo().unwrap();
    app.studio.undo().unwrap();
    assert_eq!(parts(&app), [(48_000, 96_000)]);
}

#[test]
fn zoom_scroll_ruler_and_drops() {
    let mut app = app();
    let (track, _) = add_clip(&mut app, 0.0);
    let lanes = app.lanes_rect();
    let at = Point::new(lanes.x + 200.0, lanes.y + 10.0);

    app.scroll(at, Point::new(0.0, -200.0), CTRL);
    let zoomed = app.view_state().px_per_s;
    assert!(zoomed > 80.0, "{zoomed}");
    // The time under the pointer stays put: 200 px at 80 px/s = 2.5 s.
    let t = app.view_state().scroll_s + 200.0 / f64::from(zoomed);
    assert!((t - 2.5).abs() < 1e-3, "{t}");

    app.click_at(
        Point::new(lanes.x + 40.0, lanes.y - 10.0),
        MouseButton::Left,
        Modifiers::default(),
    )
    .unwrap();
    let pos = app.studio.transport().position_s;
    let expected = app.view_state().scroll_s + 40.0 / f64::from(zoomed);
    assert!((pos - expected).abs() < 1e-3, "{pos} vs {expected}");

    app.drop_files(
        vec!["/a.wav".into(), "/b.wav".into()],
        Point::new(lanes.x + 10.0, lanes.y + 10.0),
    );
    let imports = app.take_imports();
    assert_eq!(imports.len(), 2);
    assert_eq!(imports[0].track.map(|t| t.0), Some(track));
    assert_eq!(imports[1].track, None);

    click(&mut app, "toolbar/import").unwrap();
    assert!(matches!(
        app.take_platform_requests()[..],
        [PlatformRequest::ChooseAudioFiles { track: None, .. }]
    ));
}

#[test]
fn arm_record_with_keys_and_clicks() {
    let mut app = app();
    let (track, _) = add_clip(&mut app, 4.0);
    click(&mut app, &format!("track/{track}/arm")).unwrap();
    assert!(app.studio.session().project().tracks[0].armed);
    assert_eq!(
        app.access_tree()
            .find(&format!("track/{track}/arm"))
            .unwrap()
            .checked,
        Some(true)
    );

    let dir = tempfile::tempdir().unwrap();
    app.studio.set_recording_dir(dir.path().to_owned());
    // Feed half a second of input and record it with R … R.
    app.studio
        .set_virtual_input(Some(AudioData::new(vec![vec![0.25; 24_000]])))
        .unwrap();
    app.key("r", Modifiers::default()).unwrap();
    assert!(app.studio.transport().recording);
    assert_eq!(
        app.access_tree().find("transport/record").unwrap().checked,
        Some(true)
    );
    app.studio.advance(0.5).unwrap();
    app.key("r", Modifiers::default()).unwrap();
    assert!(!app.studio.transport().recording);
    let project = app.studio.session().project();
    let take = project.tracks[0]
        .clips
        .iter()
        .find(|c| c.start == 0)
        .expect("a take at 0 s");
    assert_eq!(take.length, 24_000);
    assert_eq!(app.studio.session().undo_label(), Some("record"));
}

#[test]
fn ranges_from_ruler_drags() {
    let mut app = app();
    add_clip(&mut app, 0.0);
    // No loop range yet: the toggle explains how to make one.
    click(&mut app, "transport/loop").unwrap();
    let message = app
        .access_tree()
        .find("status/message")
        .unwrap()
        .name
        .clone();
    assert!(message.unwrap().contains("Shift+drag"));

    let ruler = app.access_tree().find("timeline/ruler").unwrap().bounds;
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    // 80 px/s: from 1 s to 3 s (snapped to beats).
    app.drag(
        DragFrom::Point(Point::new(ruler.x + 81.0, ruler.y + 10.0)),
        Point::new(158.0, 0.0),
        5,
        shift,
    )
    .unwrap();
    let project = app.studio.session().project();
    let loop_range = project.loop_region.active().expect("loop on");
    assert_eq!((loop_range.start, loop_range.end), (48_000, 144_000));
    let tree = app.access_tree();
    assert_eq!(
        tree.find("timeline/loop").unwrap().value.as_deref(),
        Some("1.000 s - 3.000 s")
    );
    assert_eq!(tree.find("transport/loop").unwrap().checked, Some(true));
    app.key("l", Modifiers::default()).unwrap();
    assert!(!app.studio.session().project().loop_region.enabled);

    app.drag(
        DragFrom::Point(Point::new(ruler.x + 40.0, ruler.y + 10.0)),
        Point::new(40.0, 0.0),
        3,
        CTRL,
    )
    .unwrap();
    let punch = app
        .studio
        .session()
        .project()
        .punch
        .active()
        .expect("punch on");
    assert_eq!((punch.start, punch.end), (24_000, 48_000));
}

/// A MIDI clip at `at_s` on a new MIDI track, with (pitch, start beat,
/// length in beats) notes. Returns (track, clip).
fn add_midi(app: &mut App, at_s: f64, notes: &[(u8, f64, f64)]) -> (u64, u64) {
    let outcome = app
        .studio
        .execute(
            AddMidiClip {
                position_s: at_s,
                notes: notes
                    .iter()
                    .map(|&(pitch, start_beats, length_beats)| NoteSpec {
                        pitch,
                        start_beats,
                        length_beats,
                        velocity: 100,
                    })
                    .collect(),
                ..Default::default()
            }
            .into(),
        )
        .unwrap();
    (outcome.track.unwrap().0, outcome.clip.unwrap().0)
}

/// The edited clip's notes as (pitch, start beat, length in beats).
fn notes_of(app: &App, clip: u64) -> Vec<(u8, f64, f64)> {
    let project = app.studio.session().project();
    let map = project.tempo_map();
    let (_, c) = project.clip(ClipId(clip)).unwrap();
    c.notes()
        .iter()
        .map(|n| {
            let start = map.clip_beats(c.start, n.start);
            (n.pitch, start, map.clip_beats(c.start, n.end()) - start)
        })
        .collect()
}

/// Window position of `beat` (plus `dx`) and the middle of `pitch`'s row in
/// the piano roll grid.
fn grid_point(app: &mut App, beat: f64, pitch: u8, dx: f32) -> Point {
    let grid = app.access_tree().find("piano_roll/grid").unwrap().bounds;
    let roll = app.view_state().piano_roll;
    Point::new(
        grid.x + crate::view::roll_x(&roll, beat) + dx,
        grid.y + crate::view::roll_y(&roll, i32::from(pitch)) + ROW_H / 2.0,
    )
}

#[test]
fn midi_tracks_and_clips_in_the_arrange_view() {
    let mut app = app();
    click(&mut app, "arrange/add_midi_track").unwrap();
    let track = &app.studio.session().project().tracks[0];
    assert_eq!(
        (track.kind, track.name.as_str()),
        (TrackKind::Midi, "MIDI 1")
    );
    let id = track.id;

    // Double-clicking the empty lane makes a one-bar clip, snapped to a beat.
    let lanes = app.lanes_rect();
    app.double_click_at(
        Point::new(lanes.x + 85.0, lanes.y + 20.0),
        Modifiers::default(),
    )
    .unwrap();
    let project = app.studio.session().project();
    let clip = &project.track(id).unwrap().clips[0];
    assert_eq!((clip.start, clip.length), (48_000, 96_000));
    assert_eq!(app.view_state().selected_clip, Some(clip.id));
    let clip = clip.id;

    // Notes show in the clip.
    app.studio
        .execute(
            dawkey_core::AddMidiNotes {
                clip,
                notes: vec![NoteSpec {
                    pitch: 60,
                    start_beats: 0.0,
                    length_beats: 2.0,
                    velocity: 100,
                }],
            }
            .into(),
        )
        .unwrap();
    let node = app
        .access_tree()
        .find(&format!("clip/{clip}"))
        .unwrap()
        .clone();
    assert_eq!(node.value.as_deref(), Some("1.000 s - 3.000 s"));
    let bar = app.scene().primitives.into_iter().any(|p| {
        matches!(p, dawkey_ui::Primitive::Rect { rect, .. }
            if rect.x == node.bounds.x && (rect.w - 80.0).abs() < 0.01)
    });
    assert!(bar, "a 2-beat (80 px) note bar at the clip's start");

    // Return (or the toolbar toggle) opens the selected clip.
    app.key("return", Modifiers::default()).unwrap();
    assert_eq!(app.view_state().page, Page::PianoRoll);
    assert_eq!(app.view_state().editing_clip, Some(clip));
    let tree = app.access_tree();
    assert_eq!(tree.find("view/piano_roll").unwrap().checked, Some(true));
    let note = tree.find("note/1").unwrap();
    assert_eq!(note.name.as_deref(), Some("C4"));
    assert_eq!(
        note.value.as_deref(),
        Some("0.000-2.000 beats · velocity 100")
    );

    // Audio clips can't be dragged onto MIDI tracks.
    let (_, audio) = add_clip(&mut app, 0.0);
    app.key("escape", Modifiers::default()).unwrap();
    assert_eq!(app.view_state().page, Page::Arrange);
    app.drag(
        DragFrom::Widget(format!("clip/{audio}")),
        Point::new(0.0, -crate::state::LANE_H),
        4,
        Modifiers::default(),
    )
    .unwrap();
    let project = app.studio.session().project();
    assert_ne!(project.clip(ClipId(audio)).unwrap().0.id, id);
}

#[test]
fn piano_roll_edits_notes() {
    let mut app = app();
    let (_, clip) = add_midi(&mut app, 0.0, &[(60, 0.0, 1.0)]);
    app.open_piano_roll(ClipId(clip)).unwrap();

    // Double-click adds a grid-long note at the grid line left of the
    // pointer.
    let at = grid_point(&mut app, 1.1, 62, 0.0);
    app.double_click_at(at, Modifiers::default()).unwrap();
    assert_eq!(notes_of(&app, clip), [(60, 0.0, 1.0), (62, 1.0, 0.25)]);
    let added = app.view_state().selected_notes[0];

    // Drag the first note right a beat and up two semitones: one undo step.
    let ppb = app.view_state().piano_roll.px_per_beat;
    app.drag(
        DragFrom::Widget("note/1".into()),
        Point::new(ppb, -2.0 * ROW_H),
        6,
        Modifiers::default(),
    )
    .unwrap();
    // Sorted by start, pitch, then ID: the moved note (1) comes first.
    assert_eq!(notes_of(&app, clip), [(62, 1.0, 1.0), (62, 1.0, 0.25)]);
    assert_eq!(app.studio.session().undo_label(), Some("edit_midi_notes"));

    // Drag its end: two beats long.
    app.drag(
        DragFrom::Widget("note/1/end".into()),
        Point::new(ppb, 0.0),
        4,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(notes_of(&app, clip)[0], (62, 1.0, 2.0));

    // Keys: transpose the selection, select all, delete.
    app.key("up", Modifiers::default()).unwrap();
    assert_eq!(notes_of(&app, clip), [(62, 1.0, 0.25), (63, 1.0, 2.0)]);
    click(&mut app, &format!("note/{added}")).unwrap();
    app.key("delete", Modifiers::default()).unwrap();
    assert_eq!(notes_of(&app, clip).len(), 1);
    app.key("a", CTRL).unwrap();
    assert_eq!(app.view_state().selected_notes.len(), 1);

    // The grid buttons set the snap; quantize uses it.
    click(&mut app, "piano_roll/grid/4").unwrap();
    assert_eq!(app.view_state().piano_roll.grid_beats, 1.0);
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    app.drag(
        DragFrom::Widget("note/1".into()),
        Point::new(ppb * 0.4, 0.0),
        4,
        shift,
    )
    .unwrap();
    assert!((notes_of(&app, clip)[0].1 - 1.4).abs() < 1e-3);
    click(&mut app, "piano_roll/quantize").unwrap();
    assert_eq!(notes_of(&app, clip)[0].1, 1.0);

    // Undoing the clip away closes the piano roll.
    while app.studio.session().project().clip(ClipId(clip)).is_some() {
        app.studio.undo().unwrap();
    }
    app.rebuild();
    assert_eq!(app.view_state().page, Page::Arrange);
    assert_eq!(app.view_state().editing_clip, None);
}

#[test]
fn piano_roll_marquee_selects_notes() {
    let mut app = app();
    let (_, clip) = add_midi(
        &mut app,
        0.0,
        &[
            (60, 0.0, 1.0),
            (64, 1.0, 1.0),
            (67, 2.0, 1.0),
            (72, 0.5, 0.5),
        ],
    );
    app.open_piano_roll(ClipId(clip)).unwrap();
    let ids: Vec<_> = app
        .studio
        .session()
        .project()
        .clip(ClipId(clip))
        .unwrap()
        .1
        .notes()
        .iter()
        .map(|n| (n.pitch, n.id))
        .collect();
    let id = |pitch| ids.iter().find(|(p, _)| *p == pitch).unwrap().1;
    // From empty space before C4 to the middle of the E's beat, down to C4:
    // C4 and E4, not G4 (later) or C5 (higher).
    let from = grid_point(&mut app, 0.2, 66, 0.0);
    let to = grid_point(&mut app, 1.5, 60, 0.0);
    app.drag(
        DragFrom::Point(from),
        Point::new(to.x - from.x, to.y - from.y),
        5,
        Modifiers::default(),
    )
    .unwrap();
    let mut selected = app.view_state().selected_notes.clone();
    selected.sort();
    assert_eq!(selected, [id(60), id(64)]);
    // Shift adds to the selection.
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let from = grid_point(&mut app, 2.2, 68, 0.0);
    app.drag(
        DragFrom::Point(from),
        Point::new(10.0, ROW_H * 2.0),
        3,
        shift,
    )
    .unwrap();
    assert_eq!(app.view_state().selected_notes.len(), 3);
    assert!(app.view_state().selected_notes.contains(&id(67)));
    // Nothing changed in the project.
    assert_eq!(app.studio.session().undo_label(), Some("add_midi_clip"));
}

#[test]
fn piano_roll_lane_edits_velocity_and_controllers() {
    use crate::state::{ROLL_LANE_H, RollLane};
    use dawkey_core::Controller;

    let mut app = app();
    let (_, clip) = add_midi(&mut app, 0.0, &[(60, 0.0, 1.0), (64, 1.0, 1.0)]);
    app.open_piano_roll(ClipId(clip)).unwrap();
    let velocities = |app: &App| -> Vec<u8> {
        let project = app.studio.session().project();
        project
            .clip(ClipId(clip))
            .unwrap()
            .1
            .notes()
            .iter()
            .map(|n| n.velocity)
            .collect()
    };
    // Dragging a bar down a quarter of the lane: about 32 less.
    app.drag(
        DragFrom::Widget("note/1/velocity".into()),
        Point::new(0.0, (ROLL_LANE_H - 9.0) / 4.0),
        4,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(velocities(&app), [100 - 32, 100]);
    assert_eq!(app.studio.session().undo_label(), Some("edit_midi_notes"));

    // Painting across the lane near the bottom (right to left, starting
    // clear of the bars) sets both.
    let lane = app.access_tree().find("piano_roll/lane").unwrap().bounds;
    let ppb = app.view_state().piano_roll.px_per_beat;
    let y = lane.y + ROLL_LANE_H - 5.0 - (ROLL_LANE_H - 9.0) * 10.0 / 126.0;
    app.drag(
        DragFrom::Point(Point::new(lane.x + ppb * 1.6, y)),
        Point::new(-ppb * 1.6, 0.0),
        6,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(velocities(&app), [11, 11]);

    // The lane header cycles to pitch bend.
    click(&mut app, "piano_roll/lane_kind").unwrap();
    assert_eq!(
        app.view_state().roll_lane,
        RollLane::Control {
            controller: Controller::PitchBend
        }
    );
    // Double-click adds a change at the top (full bend) on beat 1.
    app.double_click_at(
        Point::new(lane.x + ppb * 1.1, lane.y + 4.0),
        Modifiers::default(),
    )
    .unwrap();
    let controls = |app: &App| -> Vec<(u64, u16)> {
        let project = app.studio.session().project();
        project
            .clip(ClipId(clip))
            .unwrap()
            .1
            .controls()
            .iter()
            .map(|c| (c.frame, c.value))
            .collect()
    };
    assert_eq!(controls(&app), [(24_000, 16_383)]);
    // Drag it to the middle (no bend) and half a beat later.
    app.drag(
        DragFrom::Widget("piano_roll/point/0".into()),
        Point::new(ppb * 0.5, (ROLL_LANE_H - 9.0) / 2.0),
        4,
        Modifiers::default(),
    )
    .unwrap();
    let (frame, value) = controls(&app)[0];
    assert_eq!(frame, 36_000);
    assert!((8100..=8300).contains(&value), "{value}");
    // Select it and delete it.
    click(&mut app, "piano_roll/point/0").unwrap();
    app.key("delete", Modifiers::default()).unwrap();
    assert!(controls(&app).is_empty());
}

#[test]
fn piano_roll_picks_midi_devices() {
    let mut app = app();
    let (track, clip) = add_midi(&mut app, 0.0, &[]);
    app.studio.set_virtual_midi_devices(
        vec!["Keys:Keys MIDI 1".into()],
        vec!["Synth:Synth MIDI 1".into(), "Drums:Drums MIDI 1".into()],
    );
    app.open_piano_roll(ClipId(clip)).unwrap();
    let t = |app: &App| app.studio.session().project().tracks[0].clone();
    let input = format!("track/{track}/midi_in");
    let output = format!("track/{track}/midi_out");
    assert_eq!(
        app.access_tree().find(&input).unwrap().value.as_deref(),
        Some("In: all")
    );
    click(&mut app, &input).unwrap();
    assert_eq!(t(&app).midi_input.as_deref(), Some("Keys:Keys MIDI 1"));
    assert_eq!(
        app.access_tree().find(&input).unwrap().value.as_deref(),
        Some("In: Keys MIDI 1")
    );
    // The channel cycles through all, 1 to 16, and keeps the device.
    let channel = format!("track/{track}/midi_in_channel");
    click(&mut app, &channel).unwrap();
    click(&mut app, &channel).unwrap();
    assert_eq!(t(&app).midi_input_channel, Some(2));
    assert_eq!(t(&app).midi_input.as_deref(), Some("Keys:Keys MIDI 1"));
    assert_eq!(
        app.access_tree().find(&channel).unwrap().value.as_deref(),
        Some("Ch 2")
    );
    click(&mut app, &input).unwrap();
    assert_eq!(t(&app).midi_input, None);
    assert_eq!(t(&app).midi_input_channel, Some(2));

    click(&mut app, &output).unwrap();
    click(&mut app, &output).unwrap();
    let out = t(&app).midi_output.unwrap();
    assert_eq!(
        (out.device.as_str(), out.channel),
        ("Drums:Drums MIDI 1", 1)
    );
    // The device's latency: a slider from 0 to 1000 ms, cubic, that only
    // shows with an output device. A fifth of the way is 8 ms.
    let latency = format!("track/{track}/midi_out_latency");
    app.drag(
        DragFrom::Widget(latency.clone()),
        Point::new(crate::state::SLIDER_W * 0.2, 0.0),
        6,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(t(&app).midi_output.unwrap().latency_ms, 8.0);
    assert_eq!(
        app.access_tree().find(&latency).unwrap().value.as_deref(),
        Some("Lat 8.0 ms")
    );
    app.studio.undo().unwrap();
    assert_eq!(t(&app).midi_output.unwrap().latency_ms, 0.0);
    // Double-clicking it types a value instead.
    app.double_click(&latency, Modifiers::default()).unwrap();
    let field = app.access_tree().find(&latency).unwrap().clone();
    assert_eq!(
        (field.role, field.value.as_deref()),
        (Role::TextField, Some("0"))
    );
    for key in ["1", "2", ".", "5", "return"] {
        app.key(key, Modifiers::default()).unwrap();
    }
    let out = t(&app).midi_output.unwrap();
    assert_eq!((out.latency_ms, out.channel), (12.5, 1));
    assert_eq!(
        app.access_tree().find(&latency).unwrap().value.as_deref(),
        Some("Lat 12.5 ms")
    );
    app.studio.undo().unwrap();
    click(&mut app, &output).unwrap();
    assert_eq!(t(&app).midi_output, None);
    assert!(app.access_tree().find(&latency).is_none());
}

#[test]
fn synth_controls() {
    let mut app = app();
    let (track, clip) = add_midi(&mut app, 0.0, &[]);
    app.open_piano_roll(ClipId(clip)).unwrap();
    click(&mut app, &format!("track/{track}/synth/square")).unwrap();
    let synth = |app: &App| app.studio.session().project().tracks[0].synth.unwrap();
    assert_eq!(synth(&app).waveform, Waveform::Square);

    // Dragging the cutoff slider all the way right: 20 kHz, one undo step.
    let slider = format!("track/{track}/synth/cutoff");
    app.drag(
        DragFrom::Widget(slider.clone()),
        Point::new(crate::state::SLIDER_W, 0.0),
        8,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(synth(&app).cutoff_hz, 20_000.0);
    let tree = app.access_tree();
    assert_eq!(
        tree.find(&slider).unwrap().value.as_deref(),
        Some("Cut 20.0 kHz")
    );
    app.studio.undo().unwrap();
    assert_eq!(synth(&app).cutoff_hz, 1200.0);
    assert_eq!(synth(&app).waveform, Waveform::Square);
}

#[test]
fn synth_slider_mapping_round_trips() {
    use crate::state::SynthParam;
    for param in SynthParam::ALL {
        for value in [param.value(0.0), param.value(0.37), param.value(1.0)] {
            let back = param.value(param.position(value));
            assert!(
                (back - value).abs() <= value.abs() * 0.01 + 0.1,
                "{param:?} {value} {back}"
            );
        }
    }
    assert_eq!(SynthParam::Cutoff.value(0.0), 20.0);
    assert_eq!(SynthParam::Release.value(1.0), 20_000.0);
}

/// An app whose studio finds the test plugins (and only them).
fn app_with_plugins() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let installed = dawkey_test_plugins::install(dir.path()).unwrap();
    let mut app = app();
    app.studio.set_plugin_search(
        dawkey_plugins::SearchPath {
            clap: vec![installed.clap_path],
            lv2: Some(vec![installed.lv2_path]),
            vst3: vec![installed.vst3_path],
        },
        None,
    );
    (app, dir)
}

fn slot_param(app: &App, slot: dawkey_core::PluginId) -> Option<f64> {
    let (_, s) = app.studio.session().project().plugin(slot).unwrap();
    s.params.get("1").copied()
}

#[test]
fn plugins_from_the_mixer() {
    let (mut app, _dir) = app_with_plugins();
    let (track, _) = add_clip(&mut app, 0.0);
    click(&mut app, "view/mixer").unwrap();
    click(&mut app, &format!("track/{track}/add_plugin")).unwrap();
    let tree = app.access_tree();
    let picker = tree.find("plugin_picker").unwrap();
    assert_eq!(picker.name.as_deref(), Some("Add a plugin"));
    click(&mut app, "plugin_picker/filter/effects").unwrap();
    // Effects, by name: Test Delay, Test Delay (VST3), Test Gain, ...
    let item = app
        .access_tree()
        .find("plugin_picker/item/2")
        .unwrap()
        .clone();
    assert_eq!(item.value.as_deref(), Some("clap:dawkey.test.gain"));
    click(&mut app, "plugin_picker/item/2").unwrap();

    // Added, and its editor is open.
    let project = app.studio.session().project();
    let slot = project.tracks[0].plugins[0].id;
    assert_eq!(project.tracks[0].plugins[0].plugin, "clap:dawkey.test.gain");
    assert_eq!(app.view_state().plugin_editor, Some(slot));
    let param = format!("plugin/{slot}/param/1");
    let tree = app.access_tree();
    // As the plugin shows it.
    assert_eq!(tree.find(&param).unwrap().value.as_deref(), Some("0.0 dB"));
    assert_eq!(
        tree.find(&format!("plugin/{slot}"))
            .unwrap()
            .value
            .as_deref(),
        Some("on")
    );

    // Dragging the slider all the way left: -60 dB, one undo step.
    app.drag(
        DragFrom::Widget(param.clone()),
        Point::new(-crate::state::PARAM_W * 2.0, 0.0),
        6,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(slot_param(&app, slot), Some(-60.0));
    app.studio.undo().unwrap();
    assert_eq!(slot_param(&app, slot), None);
    // Ctrl-click sets the default explicitly.
    app.click(&param, MouseButton::Left, CTRL).unwrap();
    assert_eq!(slot_param(&app, slot), Some(0.0));
    // Double-click types a value, which the plugin reads.
    app.double_click(&param, Modifiers::default()).unwrap();
    let field = app.access_tree().find(&param).unwrap().clone();
    assert_eq!(field.role, dawkey_ui::Role::TextField);
    assert_eq!(field.value.as_deref(), Some("0.0 dB"));
    type_keys(
        &mut app,
        &["-", "1", "2", ".", "5", "space", "d", "b", "return"],
    );
    assert_eq!(slot_param(&app, slot), Some(-12.5));
    assert_eq!(app.view_state().text_edit, None);
    // Text it can't read is an error, and changes nothing.
    app.double_click(&param, Modifiers::default()).unwrap();
    type_keys(&mut app, &["l", "o", "u", "d", "return"]);
    let err = app.message.clone().unwrap_or_default();
    assert!(err.contains("can't read \"loud\""), "{err}");
    assert_eq!(slot_param(&app, slot), Some(-12.5));
    // Escape leaves it as it was; one undo step per value typed.
    app.double_click(&param, Modifiers::default()).unwrap();
    type_keys(&mut app, &["3", "escape"]);
    assert_eq!(slot_param(&app, slot), Some(-12.5));
    app.studio.undo().unwrap();
    assert_eq!(slot_param(&app, slot), Some(0.0));

    click(&mut app, &format!("plugin/{slot}/bypass")).unwrap();
    assert!(app.studio.session().project().tracks[0].plugins[0].bypass);
    // Automate: a lane at the playhead, shown in the arrange view.
    click(&mut app, &format!("plugin/{slot}/param/1/automate")).unwrap();
    assert_eq!(app.studio.session().project().tracks[0].automation.len(), 1);
    assert_eq!(
        app.view_state().automation_shown,
        [dawkey_core::TrackId(track)]
    );

    click(&mut app, &format!("plugin/{slot}/remove")).unwrap();
    assert!(app.studio.session().project().tracks[0].plugins.is_empty());
    assert_eq!(app.view_state().plugin_editor, None);
    // Escape closes the picker.
    click(&mut app, &format!("track/{track}/add_plugin")).unwrap();
    app.key("escape", Modifiers::default()).unwrap();
    assert_eq!(app.view_state().plugin_picker, None);
}

#[test]
fn instruments_from_the_picker_go_first() {
    let (mut app, _dir) = app_with_plugins();
    let (track, _) = add_midi(&mut app, 0.0, &[(60, 0.0, 1.0)]);
    app.studio
        .execute(
            dawkey_core::AddPlugin {
                track: dawkey_core::TrackId(track),
                plugin: "clap:dawkey.test.gain".into(),
                index: None,
            }
            .into(),
        )
        .unwrap();
    click(&mut app, "view/mixer").unwrap();
    click(&mut app, &format!("track/{track}/add_plugin")).unwrap();
    click(&mut app, "plugin_picker/filter/instruments").unwrap();
    click(&mut app, "plugin_picker/item/0").unwrap();
    let plugins: Vec<String> = app.studio.session().project().tracks[0]
        .plugins
        .iter()
        .map(|p| p.plugin.clone())
        .collect();
    assert_eq!(plugins, ["clap:dawkey.test.beep", "clap:dawkey.test.gain"]);
}

/// The plugins the picker shows, in order.
fn picker_items(app: &mut App) -> Vec<String> {
    let tree = app.access_tree();
    (0..)
        .map_while(|n| tree.find(&format!("plugin_picker/item/{n}")))
        .map(|item| item.value.clone().unwrap_or_default())
        .collect()
}

#[test]
fn finding_plugins_in_the_picker() {
    let (mut app, _dir) = app_with_plugins();
    let (track, _) = add_clip(&mut app, 0.0);
    click(&mut app, "view/mixer").unwrap();
    click(&mut app, &format!("track/{track}/add_plugin")).unwrap();
    let everything = picker_items(&mut app);
    assert!(everything.len() > 10, "{everything:?}");

    // Typing searches, best matches first: the name beats the vendor.
    type_keys(&mut app, &["g", "a", "i", "n"]);
    let search = app
        .access_tree()
        .find("plugin_picker/search")
        .unwrap()
        .clone();
    assert_eq!(search.value.as_deref(), Some("gain"));
    let found = picker_items(&mut app);
    assert_eq!(found[0], "clap:dawkey.test.gain");
    assert!(
        found.len() >= 3 && found.len() < everything.len(),
        "{found:?}"
    );
    // Words narrow it, and letters needn't be together.
    type_keys(&mut app, &["space", "v", "s", "3"]);
    let found = picker_items(&mut app);
    assert!(found.iter().all(|id| id.starts_with("vst3:")), "{found:?}");
    // Nothing matches.
    type_keys(&mut app, &["z", "z"]);
    assert_eq!(picker_items(&mut app), Vec::<String>::new());
    assert!(app.access_tree().find("plugin_picker/status").is_some());
    // Backspace deletes; Escape clears it all, then closes.
    type_keys(&mut app, &["backspace", "backspace"]);
    assert_eq!(app.view_state().picker_query, "gain vs3");
    type_keys(&mut app, &["escape"]);
    assert_eq!(picker_items(&mut app), everything);
    // Keys don't reach the rest of the window while searching: "r"
    // doesn't record, space doesn't play.
    type_keys(&mut app, &["r", "space"]);
    assert!(!app.studio.transport().playing);
    assert!(!app.studio.transport().recording);
    type_keys(&mut app, &["escape", "escape"]);
    assert_eq!(app.view_state().plugin_picker, None);

    // Down moves the highlight, Return adds it; the filter applies too.
    click(&mut app, &format!("track/{track}/add_plugin")).unwrap();
    assert_eq!(app.view_state().picker_query, "");
    click(&mut app, "plugin_picker/filter/effects").unwrap();
    type_keys(&mut app, &["d", "e", "l", "a", "y"]);
    let found = picker_items(&mut app);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0], "clap:dawkey.test.delay");
    type_keys(&mut app, &["down", "down", "up", "down", "return"]);
    let project = app.studio.session().project();
    assert_eq!(project.tracks[0].plugins[0].plugin, found[1]);
    assert_eq!(
        app.view_state().plugin_editor,
        Some(project.tracks[0].plugins[0].id)
    );
}

#[test]
fn the_picker_scrolls() {
    let (mut app, _dir) = app_with_plugins();
    let (track, _) = add_clip(&mut app, 0.0);
    click(&mut app, "view/mixer").unwrap();
    click(&mut app, &format!("track/{track}/add_plugin")).unwrap();
    // (Only the rows in view are built.)
    let rows = app.studio.cached_catalog().unwrap().plugins.len();
    let tree = app.access_tree();
    let list = tree.find("plugin_picker/list").unwrap().bounds;
    let bar = tree.find("plugin_picker/scrollbar").unwrap().clone();
    assert_eq!(bar.role, dawkey_ui::Role::ScrollBar);
    assert_eq!(bar.value.as_deref(), Some("0%"));
    let max = crate::view::plugins::content_height(rows) - list.h;
    assert!(max > 0.0);

    // The wheel stops at the end.
    let middle = Point::new(list.x + list.w / 2.0, list.y + list.h / 2.0);
    app.scroll(middle, Point::new(0.0, 10_000.0), Modifiers::default());
    assert_eq!(app.view_state().overlay_scroll, max);
    let shown = app.access_tree();
    assert!(
        shown
            .find(&format!("plugin_picker/item/{}", rows - 1))
            .is_some()
    );
    assert_eq!(
        shown
            .find("plugin_picker/scrollbar")
            .unwrap()
            .value
            .as_deref(),
        Some("100%")
    );
    app.scroll(middle, Point::new(0.0, -10_000.0), Modifiers::default());
    assert_eq!(app.view_state().overlay_scroll, 0.0);

    // Clicking the bar off its thumb jumps there: at its bottom, the end.
    let bar = bar.bounds;
    let bottom = Point::new(bar.x + bar.w / 2.0, bar.y + bar.h - 2.0);
    app.click_at(bottom, MouseButton::Left, Modifiers::default())
        .unwrap();
    assert_eq!(app.view_state().overlay_scroll, max);
    // Dragging the thumb follows the pointer: up half the way it can go
    // is half way.
    let thumb = bar.h * list.h / (max + list.h);
    let travel = bar.h - thumb;
    app.drag(
        DragFrom::Point(Point::new(bottom.x, bar.y + bar.h - thumb / 2.0)),
        Point::new(0.0, -travel / 2.0),
        4,
        Modifiers::default(),
    )
    .unwrap();
    let half = app.view_state().overlay_scroll;
    assert!((half - max / 2.0).abs() < 1.0, "{half} of {max}");
    // And no further than the top.
    app.drag(
        DragFrom::Widget("plugin_picker/scrollbar".into()),
        Point::new(0.0, -1000.0),
        4,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(app.view_state().overlay_scroll, 0.0);

    // A search that fits shows no bar.
    type_keys(&mut app, &["w", "o", "r", "k", "e", "r"]);
    assert_eq!(app.view_state().overlay_scroll, 0.0);
    assert!(app.access_tree().find("plugin_picker/scrollbar").is_none());
    // The highlight stays in view as it moves.
    type_keys(&mut app, &["escape", "end"]);
    let last = format!("plugin_picker/item/{}", rows - 1);
    let item = app.access_tree().find(&last).unwrap().bounds;
    assert!(item.y + item.h <= list.y + list.h, "{item:?} in {list:?}");
    type_keys(&mut app, &["home"]);
    assert_eq!(app.view_state().overlay_scroll, 0.0);
}

#[test]
fn automation_lanes_in_the_arrange_view() {
    let mut app = app();
    let (track, _) = add_clip(&mut app, 0.0);
    let gain_lane = |app: &App| -> Vec<(u64, f64)> {
        app.studio.session().project().tracks[0]
            .lane(&dawkey_core::AutomationTarget::Gain)
            .map(|l| l.points.iter().map(|p| (p.frame, p.value)).collect())
            .unwrap_or_default()
    };
    // The A toggle shows the lanes; a track without any gets a gain lane.
    click(&mut app, &format!("track/{track}/automation")).unwrap();
    assert_eq!(gain_lane(&app), [(0, 0.0)]);
    let lane_id = format!("track/{track}/auto/0/lane");
    let lane = app.access_tree().find(&lane_id).unwrap().bounds;
    assert_eq!(lane.h, crate::state::AUTO_H - 1.0);
    let header = app
        .access_tree()
        .find(&format!("track/{track}/auto/0"))
        .unwrap()
        .clone();
    assert_eq!(header.name.as_deref(), Some("Gain"));
    assert_eq!(header.value.as_deref(), Some("+0.0 dB"));

    // Double-click 1 s in, at the bottom: a point at 1 s, near silence.
    app.double_click_at(
        Point::new(lane.x + 80.0, lane.y + lane.h - 4.0),
        Modifiers::default(),
    )
    .unwrap();
    let points = gain_lane(&app);
    assert_eq!(points.len(), 2);
    assert_eq!(points[1].0, 48_000);
    assert!(points[1].1 < -100.0, "{points:?}");

    // Drag it back up to the middle of the lane, half a second later.
    app.drag(
        DragFrom::Widget(format!("auto/{track}/0/1")),
        Point::new(40.0, -(lane.h - 8.0) / 2.0),
        6,
        Modifiers::default(),
    )
    .unwrap();
    let points = gain_lane(&app);
    assert_eq!(points[1].0, 72_000);
    assert!((-30.0..0.0).contains(&points[1].1), "{points:?}");
    assert_eq!(app.studio.session().undo_label(), Some("set_automation"));

    // Select it and delete it.
    click(&mut app, &format!("auto/{track}/0/1")).unwrap();
    app.key("delete", Modifiers::default()).unwrap();
    assert_eq!(gain_lane(&app), [(0, 0.0)]);

    // Tracks below move down by the shown lanes, and clicks find them.
    let (other, _) = add_clip(&mut app, 4.0);
    let other_lane = app
        .access_tree()
        .find(&format!("track/{other}/lane"))
        .unwrap()
        .bounds;
    assert_eq!(other_lane.y - lane.y, crate::state::AUTO_H);
    click(&mut app, &format!("track/{track}/automation")).unwrap();
    assert!(app.access_tree().find(&lane_id).is_none());
}

#[test]
fn tempo_lanes_edit_the_tempo_map() {
    let mut app = app();
    // A MIDI note at beat 8 keeps its beat through tempo edits.
    let (_, clip) = add_midi(&mut app, 0.0, &[(60, 8.0, 1.0)]);
    let project = |app: &App| app.studio.session().project().clone();
    let value = |app: &mut App, id: &str| app.access_tree().find(id).and_then(|n| n.value.clone());
    assert!(app.access_tree().find("tempo/lane").is_none());
    click(&mut app, "arrange/tempo").unwrap();
    for id in [
        "tempo/header",
        "tempo/lane",
        "tempo/0",
        "meter/header",
        "meter/0",
    ] {
        assert!(app.access_tree().find(id).is_some(), "{id}");
    }
    // The start tempo has no ramp or remove button.
    assert!(app.access_tree().find("tempo/ramp").is_none());
    click(&mut app, "tempo/up").unwrap();
    assert_eq!(project(&app).tempo_bpm, 121.0);
    app.studio.undo().unwrap();

    // Double-clicking the lane adds a change on the beat grid, at the
    // tempo under the pointer: 2 s (beat 4) and the lane's middle, which
    // is 120 BPM (the lane spans 110 to 130).
    let lane = app.access_tree().find("tempo/lane").unwrap().bounds;
    let x = |app: &App, s: f64| {
        lane.x + ((s - app.view_state().scroll_s) * f64::from(app.view_state().px_per_s)) as f32
    };
    let mid = lane.y + crate::view::tempo::tempo_y((110.0, 130.0), 120.0);
    app.double_click_at(Point::new(x(&app, 2.05), mid), Modifiers::default())
        .unwrap();
    assert_eq!(
        project(&app).tempo_changes,
        [dawkey_core::TempoChange::new(4.0, 120.0)]
    );
    assert_eq!(
        app.view_state().selected_marker,
        Some(crate::Marker::Tempo(1))
    );
    assert_eq!(
        value(&mut app, "tempo/1").as_deref(),
        Some("120 BPM at beat 4")
    );
    click(&mut app, "tempo/ramp").unwrap();
    assert!(project(&app).tempo_changes[0].ramp);

    // Dragging the change up 20 px and right a beat (40 px at 80 px/s and
    // 120 BPM): 130 BPM at beat 5, in one undo step.
    app.drag(
        DragFrom::Widget("tempo/1".into()),
        Point::new(40.0, -20.0),
        6,
        Modifiers::default(),
    )
    .unwrap();
    let change = project(&app).tempo_changes[0];
    assert_eq!((change.beat, change.bpm, change.ramp), (5.0, 130.0, true));
    app.studio.undo().unwrap();
    assert_eq!(project(&app).tempo_changes[0].beat, 4.0);
    app.studio.redo().unwrap();
    // The note is still at beat 8.
    let notes =
        crate::view::note_views(&project(&app), project(&app).clip(ClipId(clip)).unwrap().1);
    assert!((notes[0].start_beats - 8.0).abs() < 1e-3);
    // The start tempo only moves up and down.
    app.drag(
        DragFrom::Widget("tempo/0".into()),
        Point::new(80.0, 10.0),
        4,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(project(&app).tempo_bpm, 115.0);
    assert_eq!(project(&app).tempo_changes[0].beat, 5.0);
    // Keys: Down slows the selected point, Delete removes a change.
    click(&mut app, "tempo/1").unwrap();
    app.key("down", Modifiers::default()).unwrap();
    assert_eq!(project(&app).tempo_changes[0].bpm, 129.0);
    app.key("delete", Modifiers::default()).unwrap();
    assert!(project(&app).tempo_changes.is_empty());
    assert_eq!(app.view_state().selected_marker, None);
}

#[test]
fn meter_lane_edits_time_signatures() {
    let mut app = app();
    let project = |app: &App| app.studio.session().project().clone();
    click(&mut app, "transport/time_signature").unwrap();
    assert!(app.view_state().tempo_shown);
    // Bars are 2 s long at 120 BPM in 4/4: 160 px at 80 px/s.
    let lane = app.access_tree().find("meter/lane").unwrap().bounds;
    let y = lane.y + lane.h / 2.0;
    // Near the start of bar 3 (4 s).
    app.double_click_at(Point::new(lane.x + 4.3 * 80.0, y), Modifiers::default())
        .unwrap();
    let change = |n: u8, d: u8, bar: u32| dawkey_core::MeterChange {
        bar,
        numerator: n,
        denominator: d,
    };
    assert_eq!(project(&app).meter_changes, [change(4, 4, 3)]);
    click(&mut app, "meter/numerator/down").unwrap();
    click(&mut app, "meter/denominator/up").unwrap();
    assert_eq!(project(&app).meter_changes, [change(3, 8, 3)]);
    assert_eq!(
        app.access_tree().find("meter/1").unwrap().value.as_deref(),
        Some("3/8 from bar 3")
    );
    // Dragged a bar left (2 s): bar 2. Then right by 3 of its own bars
    // (0.75 s each): bar 5.
    app.drag(
        DragFrom::Widget("meter/1".into()),
        Point::new(-160.0, 0.0),
        5,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(project(&app).meter_changes, [change(3, 8, 2)]);
    app.drag(
        DragFrom::Widget("meter/1".into()),
        Point::new(3.0 * 60.0 + 10.0, 0.0),
        5,
        Modifiers::default(),
    )
    .unwrap();
    assert_eq!(project(&app).meter_changes, [change(3, 8, 5)]);
    // The start: Up adds a beat, and it can't be removed.
    click(&mut app, "meter/0").unwrap();
    app.key("up", Modifiers::default()).unwrap();
    assert_eq!(project(&app).time_signature.numerator, 5);
    assert!(app.access_tree().find("meter/remove").is_none());
    click(&mut app, "meter/1").unwrap();
    app.key("delete", Modifiers::default()).unwrap();
    assert!(project(&app).meter_changes.is_empty());
    // Hiding the lanes forgets the selection.
    click(&mut app, "arrange/tempo").unwrap();
    assert!(app.access_tree().find("meter/lane").is_none());
}

#[test]
fn toolbar_fits_the_window() {
    let mut app = app();
    let (_, audio) = add_clip(&mut app, 0.0);
    let (_, midi) = add_midi(&mut app, 4.0, &[(60, 0.0, 1.0)]);
    let mut overflows = Vec::new();
    for clip in [audio, midi] {
        for w in [
            900.0, 940.0, 960.0, 980.0, 1000.0, 1040.0, 1060.0, 1080.0, 1100.0, 1180.0, 1230.0,
            1280.0, 1310.0, 1600.0,
        ] {
            app.set_window(WindowInfo {
                size: dawkey_ui::Size::new(w, 700.0),
                ..WindowInfo::headless()
            });
            click(&mut app, &format!("clip/{clip}")).unwrap();
            app.rebuild();
            let toolbar = app.ui.find("toolbar").unwrap();
            let right = toolbar.rect().x + toolbar.rect().w;
            let split = app.ui.find("toolbar/split").is_some();
            for child in &toolbar.children {
                let r = child.rect();
                if r.x + r.w > right + 0.5 {
                    overflows.push(format!(
                        "{w} px, clip {clip}, split {split}: {:?} ends at {} past {right}",
                        child.id,
                        r.x + r.w
                    ));
                }
            }
        }
    }
    // The mixer always has "Auto" (in place of "Snap" in narrow windows).
    click(&mut app, "view/mixer").unwrap();
    for w in [900.0, 960.0, 1000.0, 1100.0, 1280.0] {
        app.set_window(WindowInfo {
            size: dawkey_ui::Size::new(w, 700.0),
            ..WindowInfo::headless()
        });
        app.rebuild();
        assert!(app.ui.find("transport/automation").is_some(), "{w} px");
        let toolbar = app.ui.find("toolbar").unwrap();
        let right = toolbar.rect().x + toolbar.rect().w;
        for child in &toolbar.children {
            let r = child.rect();
            if r.x + r.w > right + 0.5 {
                overflows.push(format!("mixer, {w} px: {:?}", child.id));
            }
        }
    }
    assert!(overflows.is_empty(), "{overflows:#?}");
}

#[test]
fn typing_a_tempo() {
    let mut app = app();
    let (_, clip) = add_midi(&mut app, 0.0, &[(60, 8.0, 1.0)]);
    let tempo = |app: &App| app.studio.session().project().tempo_bpm;
    click(&mut app, "arrange/tempo").unwrap();
    let field = app.access_tree().find("tempo/value").unwrap().clone();
    assert_eq!(
        (field.role, field.value.as_deref()),
        (Role::TextField, Some("120"))
    );

    // The value starts selected: typing replaces it, and Space doesn't
    // play while typing. Return sets it, one undo step.
    click(&mut app, "tempo/value").unwrap();
    type_keys(&mut app, &["9", "space", "7", ",", "5"]);
    assert!(!app.studio.transport().playing);
    let field = app.access_tree().find("tempo/value").unwrap().clone();
    assert_eq!(field.value.as_deref(), Some("97.5"));
    assert_eq!(tempo(&app), 120.0);
    type_keys(&mut app, &["return"]);
    assert_eq!(tempo(&app), 97.5);
    assert!(app.view_state().text_edit.is_none());
    assert_eq!(app.studio.session().undo_label(), Some("set_tempo"));
    // The MIDI note kept its beat.
    let note = app
        .studio
        .session()
        .project()
        .clip(ClipId(clip))
        .unwrap()
        .1
        .notes()[0];
    let beats = app
        .studio
        .session()
        .project()
        .tempo_map()
        .clip_beats(0, note.start);
    assert!((beats - 8.0).abs() < 1e-4);

    // Backspace edits; Escape leaves it as it was.
    click(&mut app, "tempo/value").unwrap();
    type_keys(
        &mut app,
        &["backspace", "1", "4", "0", "backspace", "escape"],
    );
    assert_eq!(tempo(&app), 97.5);
    // Clicking elsewhere sets it.
    click(&mut app, "tempo/value").unwrap();
    type_keys(&mut app, &["1", "3", "0"]);
    click(&mut app, "timeline/ruler").unwrap();
    assert_eq!(tempo(&app), 130.0);

    // Out of range: an error, and nothing changes.
    click(&mut app, "tempo/value").unwrap();
    type_keys(&mut app, &["9", "9", "9", "9", "return"]);
    assert_eq!(tempo(&app), 130.0);
    let message = app.access_tree().find("status/message").unwrap().clone();
    assert!(format!("{message:?}").contains("BPM"), "{message:?}");
}

#[test]
fn typing_a_time_signature() {
    let mut app = app();
    let meter = |app: &App| {
        let project = app.studio.session().project();
        let sig = project.time_signature;
        (sig.numerator, sig.denominator)
    };
    click(&mut app, "arrange/tempo").unwrap();
    let field = app
        .access_tree()
        .find("meter/numerator/value")
        .unwrap()
        .clone();
    assert_eq!(
        (field.role, field.value.as_deref()),
        (Role::TextField, Some("4"))
    );

    // Tab sets the numerator and goes on to the denominator; Return sets
    // that. Two commands, so two undo steps. No points: beats are whole.
    click(&mut app, "meter/numerator/value").unwrap();
    type_keys(&mut app, &["7", ".", "tab"]);
    assert_eq!(meter(&app), (7, 4));
    let editing = app.view_state().text_edit.clone().unwrap();
    assert_eq!(editing.field, crate::state::TextField::MeterDenominator(0));
    type_keys(&mut app, &["8", "return"]);
    assert_eq!(meter(&app), (7, 8));
    assert_eq!(
        app.studio.session().undo_label(),
        Some("set_time_signature")
    );
    app.studio.undo().unwrap();
    assert_eq!(meter(&app), (7, 4));

    // A later change: the selected chip's fields set it.
    app.studio
        .execute(
            dawkey_core::SetTimeSignature {
                at_bar: Some(3),
                ..dawkey_core::SetTimeSignature::new(3, 4)
            }
            .into(),
        )
        .unwrap();
    click(&mut app, "meter/1").unwrap();
    click(&mut app, "meter/numerator/value").unwrap();
    type_keys(&mut app, &["5", "return"]);
    let change = app.studio.session().project().meter_changes[0];
    assert_eq!((change.bar, change.numerator), (3, 5));
    assert_eq!(meter(&app), (7, 4));

    // Not a power of two: an error, and nothing changes.
    click(&mut app, "meter/denominator/value").unwrap();
    type_keys(&mut app, &["6", "return"]);
    assert_eq!(
        app.studio.session().project().meter_changes[0].denominator,
        4
    );
    let message = app.access_tree().find("status/message").unwrap().clone();
    assert!(
        format!("{message:?}").contains("beat length"),
        "{message:?}"
    );
}

#[test]
fn buses_and_the_master_in_the_mixer() {
    let (mut app, _dir) = app_with_plugins();
    let (track, _) = add_clip(&mut app, 0.0);
    click(&mut app, "arrange/add_bus").unwrap();
    let project = app.studio.session().project();
    let bus = project.tracks[1].id;
    assert_eq!(project.tracks[1].kind, TrackKind::Bus);
    click(&mut app, "view/mixer").unwrap();
    // Buses don't record.
    let tree = app.access_tree();
    assert!(tree.find(&format!("track/{bus}/arm")).is_none());
    assert!(tree.find(&format!("track/{bus}/solo")).is_some());

    // The output button cycles: the master, then each bus.
    let output = format!("track/{track}/output");
    let value = |app: &mut App, id: &str| app.access_tree().find(id).unwrap().value.clone();
    assert_eq!(value(&mut app, &output).as_deref(), Some("Master"));
    click(&mut app, &output).unwrap();
    assert_eq!(app.studio.session().project().tracks[0].output, Some(bus));
    assert_eq!(value(&mut app, &output).as_deref(), Some("Bus 1"));
    click(&mut app, &output).unwrap();
    assert_eq!(app.studio.session().project().tracks[0].output, None);
    // A bus can't go to itself: with no other bus, there's nowhere else.
    click(&mut app, &format!("track/{bus}/output")).unwrap();
    assert_eq!(app.studio.session().project().tracks[1].output, None);
    let message = app.access_tree().find("status/message").unwrap().clone();
    assert!(format!("{message:?}").contains("no bus"), "{message:?}");

    // The master strip: plugins, a fader and mute, as track 0.
    click(&mut app, "track/0/add_plugin").unwrap();
    let tree = app.access_tree();
    assert!(tree.find("plugin_picker").is_some());
    click(&mut app, "plugin_picker/filter/effects").unwrap();
    click(&mut app, "plugin_picker/item/2").unwrap();
    let master = &app.studio.session().project().master;
    assert_eq!(master.plugins[0].plugin, "clap:dawkey.test.gain");
    let slot = master.plugins[0].id;
    assert!(app.access_tree().find(&format!("plugin/{slot}")).is_some());
    click(&mut app, "plugin_editor/close").unwrap();
    app.drag(
        DragFrom::Widget("track/0/fader".into()),
        Point::new(0.0, FADER_H * 0.25),
        8,
        Modifiers::default(),
    )
    .unwrap();
    assert!(app.studio.session().project().master.gain_db < -10.0);
    click(&mut app, "track/0/mute").unwrap();
    assert!(app.studio.session().project().master.mute);
    assert!(app.access_tree().find("master/strip_meter").is_some());
}

#[test]
fn plugin_routing_in_the_editor() {
    let (mut app, _dir) = app_with_plugins();
    let (kick, _) = add_clip(&mut app, 0.0);
    let (bass, _) = add_clip(&mut app, 0.0);
    click(&mut app, "arrange/add_bus").unwrap();
    let add = |app: &mut App, track: u64, plugin: &str| {
        app.studio
            .execute(
                dawkey_core::AddPlugin {
                    track: dawkey_core::TrackId(track),
                    plugin: plugin.into(),
                    index: None,
                }
                .into(),
            )
            .unwrap()
            .plugin
            .unwrap()
    };
    let subtract = add(&mut app, bass, "clap:dawkey.test.subtract");
    click(&mut app, "view/mixer").unwrap();
    click(&mut app, &format!("plugin/{subtract}")).unwrap();
    // The sidechain cycles through the other tracks, then none.
    let button = format!("plugin/{subtract}/sidechain");
    let value = |app: &mut App, id: &str| app.access_tree().find(id).unwrap().value.clone();
    assert_eq!(value(&mut app, &button).as_deref(), Some("none"));
    click(&mut app, &button).unwrap();
    let sidechain = |app: &App| {
        app.studio
            .session()
            .project()
            .plugin(subtract)
            .unwrap()
            .1
            .sidechain
    };
    assert_eq!(sidechain(&app), Some(dawkey_core::TrackId(kick)));
    assert_eq!(value(&mut app, &button).as_deref(), Some("ramp"));
    click(&mut app, &button).unwrap();
    // The bus (no loop), then none.
    assert!(sidechain(&app).is_some_and(|t| t.0 != kick && t.0 != bass));
    click(&mut app, &button).unwrap();
    assert_eq!(sidechain(&app), None);

    // A multi-out instrument's other output goes to the bus.
    click(&mut app, "plugin_editor/close").unwrap();
    let midi = app
        .studio
        .execute(
            AddTrack {
                kind: TrackKind::Midi,
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap();
    let split = add(&mut app, midi.0, "clap:dawkey.test.split");
    click(&mut app, &format!("plugin/{split}")).unwrap();
    let output = format!("plugin/{split}/output/0");
    assert_eq!(value(&mut app, &output).as_deref(), Some("→ nowhere"));
    click(&mut app, &output).unwrap();
    assert_eq!(value(&mut app, &output).as_deref(), Some("→ Bus 1"));
    let project = app.studio.session().project();
    assert!(project.plugin(split).unwrap().1.outputs.contains_key(&0));
    click(&mut app, &output).unwrap();
    assert!(
        app.studio
            .session()
            .project()
            .plugin(split)
            .unwrap()
            .1
            .outputs
            .is_empty()
    );
}

#[test]
fn moving_a_fader_while_playing_writes_automation() {
    use dawkey_core::AutomationTarget;
    use dawkey_ui::InputEvent;
    let mut app = app();
    let (track, _) = add_clip(&mut app, 0.0);
    click(&mut app, "view/mixer").unwrap();
    click(&mut app, "transport/automation").unwrap();
    assert!(app.view_state().automation_write);
    app.studio.play().unwrap();
    app.studio.advance(0.25).unwrap();
    // Grab the fader at 0.25 s, pull it down at 0.5 s, hold it there until
    // 0.75 s, let go.
    let fader = format!("track/{track}/fader");
    app.rebuild();
    let at = app.ui.widget_center(&fader).unwrap();
    let down = |dy: f32| Point::new(at.x, at.y + dy);
    app.handle_input(InputEvent::PointerDown {
        pos: at,
        button: MouseButton::Left,
        time_ms: 0,
    });
    app.handle_input(InputEvent::PointerMove(down(4.0)));
    app.studio.advance(0.25).unwrap();
    app.handle_input(InputEvent::PointerMove(down(FADER_H * 0.25)));
    app.studio.advance(0.25).unwrap();
    app.tick();
    app.handle_input(InputEvent::PointerUp {
        pos: down(FADER_H * 0.25),
        button: MouseButton::Left,
    });
    let project = app.studio.session().project();
    let t = &project.tracks[0];
    // The fader's own setting stays; the lane has the moves.
    assert_eq!(t.gain_db, 0.0);
    let lane = t.lane(&AutomationTarget::Gain).expect("a gain lane");
    let times: Vec<f64> = lane
        .points
        .iter()
        .map(|p| project.frames_to_seconds(p.frame))
        .collect();
    assert!((times[0] - 0.25).abs() < 1e-3, "{times:?}");
    assert!((times.last().unwrap() - 0.75).abs() < 1e-3, "{times:?}");
    let low = lane.points.last().unwrap().value;
    assert!(low < -10.0, "{low}");
    // The fader shows the automation's value at the playhead.
    let shown = app
        .access_tree()
        .find(&fader)
        .unwrap()
        .value
        .clone()
        .unwrap();
    assert_eq!(shown, format!("{low:.1} dB"));
    // One undo step for the whole gesture.
    app.studio.undo().unwrap();
    assert!(
        app.studio.session().project().tracks[0]
            .automation
            .is_empty()
    );

    // With "Auto" off, the fader moves as usual.
    click(&mut app, "transport/automation").unwrap();
    app.drag(
        DragFrom::Widget(fader),
        Point::new(0.0, FADER_H * 0.25),
        4,
        Modifiers::default(),
    )
    .unwrap();
    let t = &app.studio.session().project().tracks[0];
    assert!(t.automation.is_empty() && t.gain_db < -10.0);
}

#[test]
fn latch_and_write_automation_modes() {
    use dawkey_core::{AddAutomationPoint, AutomationTarget};
    let mut app = app();
    let (track, _) = add_clip(&mut app, 0.0);
    let gain = |app: &App| -> Vec<(f64, f64)> {
        let project = app.studio.session().project();
        project.tracks[0]
            .lane(&AutomationTarget::Gain)
            .map(|l| {
                l.points
                    .iter()
                    .map(|p| (project.frames_to_seconds(p.frame), p.value))
                    .collect()
            })
            .unwrap_or_default()
    };
    click(&mut app, "view/mixer").unwrap();
    click(&mut app, "transport/automation").unwrap();
    let mode = "transport/automation_mode";
    let shown = |app: &mut App| app.access_tree().find(mode).unwrap().value.clone();
    assert_eq!(shown(&mut app).as_deref(), Some("touch"));
    click(&mut app, mode).unwrap();
    assert_eq!(shown(&mut app).as_deref(), Some("latch"));

    // Latch: pulled down at 0.25 s and let go, the fader keeps writing
    // where it was left until the transport stops.
    app.studio.play().unwrap();
    app.studio.advance(0.25).unwrap();
    let fader = format!("track/{track}/fader");
    app.drag(
        DragFrom::Widget(fader.clone()),
        Point::new(0.0, FADER_H * 0.25),
        4,
        Modifiers::default(),
    )
    .unwrap();
    for _ in 0..3 {
        app.studio.advance(0.25).unwrap();
        app.tick();
    }
    app.studio.stop().unwrap();
    app.tick();
    let points = gain(&app);
    let low = points[0].1;
    assert!(low < -10.0, "{points:?}");
    assert!((points[0].0 - 0.25).abs() < 1e-3, "{points:?}");
    let last = *points.last().unwrap();
    assert!((last.0 - 1.0).abs() < 1e-3 && last.1 == low, "{points:?}");
    // Held still, the stretch is two points.
    assert_eq!(points.len(), 2, "{points:?}");
    // Stopped: nothing more is written, and the pass was one undo step.
    app.studio.play().unwrap();
    app.studio.advance(0.25).unwrap();
    app.tick();
    app.studio.stop().unwrap();
    app.tick();
    assert_eq!(gain(&app), points);
    app.studio.undo().unwrap();
    assert!(gain(&app).is_empty());

    // Write: every lane is written from where playback starts, at the
    // value it has there, until the transport stops. Then back to touch.
    for (time_s, value) in [(0.0, 0.0), (1.0, -20.0)] {
        app.studio
            .execute(
                AddAutomationPoint {
                    track: dawkey_core::TrackId(track),
                    target: AutomationTarget::Gain,
                    time_s,
                    value,
                }
                .into(),
            )
            .unwrap();
    }
    click(&mut app, mode).unwrap();
    assert_eq!(shown(&mut app).as_deref(), Some("write"));
    app.studio.locate(0.0).unwrap();
    app.studio.play().unwrap();
    app.tick();
    for _ in 0..2 {
        app.studio.advance(0.25).unwrap();
        app.tick();
    }
    app.studio.stop().unwrap();
    app.tick();
    let points = gain(&app);
    assert_eq!(points.len(), 3, "{points:?}");
    assert_eq!((points[0].1, points[1].1), (0.0, 0.0), "{points:?}");
    assert!((points[1].0 - 0.5).abs() < 1e-3, "{points:?}");
    assert_eq!(points[2], (1.0, -20.0));
    assert_eq!(
        app.view_state().automation_mode,
        crate::state::AutomationMode::Touch
    );
    app.studio.undo().unwrap();
    assert_eq!(gain(&app), [(0.0, 0.0), (1.0, -20.0)]);
}

#[test]
fn exporting_the_mix() {
    use crate::state::ExportRange;
    use dawkey_media::{FileFormat, SampleFormat};
    let mut app = app();
    // Nothing plays yet: nothing to export.
    app.key("e", CTRL).unwrap();
    assert!(app.view_state().export_dialog);
    click(&mut app, "export/start").unwrap();
    assert!(
        app.message
            .as_deref()
            .is_some_and(|m| m.contains("nothing to export"))
    );
    add_clip(&mut app, 0.5);
    // Clicking between the buttons keeps it open; each button steps
    // through its choices.
    let panel = app.access_tree().find("export").unwrap().bounds;
    app.click_at(
        Point::new(panel.x + 6.0, panel.y + 6.0),
        MouseButton::Left,
        Modifiers::default(),
    )
    .unwrap();
    assert!(app.view_state().export_dialog);
    let value = |app: &mut App, id: &str| app.access_tree().find(id).unwrap().value.clone();
    assert_eq!(
        value(&mut app, "export/range").as_deref(),
        Some("Song (to 00:02.500)")
    );
    click(&mut app, "export/format").unwrap();
    assert_eq!(value(&mut app, "export/format").as_deref(), Some("FLAC"));
    click(&mut app, "export/bits").unwrap();
    // FLAC has no float samples: 24 goes round to 16.
    assert_eq!(app.view_state().export.sample, SampleFormat::Int16);
    // The lossy formats: the samples row is their quality.
    click(&mut app, "export/format").unwrap();
    assert_eq!(
        value(&mut app, "export/format").as_deref(),
        Some("Ogg Vorbis")
    );
    assert_eq!(value(&mut app, "export/bits").as_deref(), Some("q 5"));
    click(&mut app, "export/bits").unwrap();
    assert_eq!(app.view_state().export.quality, 6.0);
    click(&mut app, "export/format").unwrap();
    assert_eq!(value(&mut app, "export/format").as_deref(), Some("Opus"));
    assert_eq!(
        value(&mut app, "export/bits").as_deref(),
        Some("128 kbit/s")
    );
    click(&mut app, "export/bits").unwrap();
    assert_eq!(app.view_state().export.bitrate_kbps, 160);
    assert_eq!(
        value(&mut app, "export/rate").as_deref(),
        Some("48 kHz (Opus)")
    );
    // Back round to WAV, then FLAC as before.
    click(&mut app, "export/format").unwrap();
    click(&mut app, "export/format").unwrap();
    assert_eq!(value(&mut app, "export/format").as_deref(), Some("FLAC"));
    click(&mut app, "export/rate").unwrap();
    assert_eq!(app.view_state().export.sample_rate, Some(44_100));
    // No loop range: the range stays the song.
    click(&mut app, "export/range").unwrap();
    assert_eq!(app.view_state().export.range, ExportRange::Song);
    click(&mut app, "export/tail").unwrap();
    assert_eq!(app.view_state().export.tail_s, 1.0);
    app.take_platform_requests();
    click(&mut app, "export/start").unwrap();
    assert!(!app.view_state().export_dialog);
    assert_eq!(
        app.take_platform_requests(),
        [PlatformRequest::ChooseExportLocation {
            name: "Test.flac".into(),
            format: FileFormat::Flac,
        }]
    );

    // The platform picked a place: it exports there in the background, and
    // says how far it is until it's done.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Test.flac");
    app.start_export(&path).unwrap();
    assert!(app.export_progress().is_some());
    assert!(app.start_export(&path).is_err(), "one at a time");
    for _ in 0..500 {
        app.tick();
        if app.export_progress().is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(app.export_progress(), None);
    assert!(
        app.message
            .as_deref()
            .is_some_and(|m| m.starts_with("Exported Test.flac")),
        "{:?}",
        app.message
    );
    // 2.5 s of song and a 1 s tail, at 44.1 kHz.
    let decoded = dawkey_media::decode_file(&path, 44_100).unwrap();
    assert_eq!(decoded.audio.frames(), 154_350);
}

#[test]
fn autosaving_and_recovering() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("Song.dawkey");
    let mut app = app();
    app.studio.save(Some(&bundle)).unwrap();
    // Autosaving when it's time, if there are changes.
    app.autosave_every = Some(std::time::Duration::ZERO);
    let autosave = bundle.join("backups/autosave.json");
    app.tick();
    assert!(!autosave.exists());
    add_track(&mut app, "Lead");
    app.tick();
    assert!(autosave.exists());
    // Then Dawkey stops without exiting cleanly.
    std::mem::forget(app);

    let open = || {
        let studio =
            Studio::new(LoadedProject::open(&bundle).unwrap(), AudioBackend::Virtual).unwrap();
        App::new(studio, Platform::Headless)
    };
    let mut app = open();
    app.offer_recovery();
    let tree = app.access_tree();
    let text: Vec<String> = tree
        .find("recover")
        .unwrap()
        .children
        .iter()
        .filter_map(|c| c.name.clone())
        .collect();
    assert!(
        text.iter().any(|t| t.contains("before “Song” was saved")),
        "{text:?}"
    );
    // It takes every key and click until answered.
    click(&mut app, "view/mixer").unwrap_err();
    app.key("tab", Modifiers::default()).unwrap();
    assert_eq!(app.view_state().page, Page::Arrange);
    click(&mut app, "recover/recover").unwrap();
    assert_eq!(app.view_state().recovery, None);
    assert_eq!(app.studio.session().project().tracks[0].name, "Lead");
    assert!(app.studio.session().is_dirty());
    drop(app);

    // Discarding deletes what was kept.
    let mut app = open();
    add_track(&mut app, "Bass");
    app.studio.autosave().unwrap();
    std::mem::forget(app);
    let mut app = open();
    app.offer_recovery();
    click(&mut app, "recover/discard").unwrap();
    assert!(app.studio.session().project().tracks.is_empty());
    assert!(!autosave.exists());
}

#[test]
fn preferences_apply_at_once_and_are_saved() {
    use crate::prefs::{AudioChoice, PluginKind, Preferences};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dawkey/preferences.json");
    let mut app = app();
    app.set_preferences(Preferences::default(), Some(path.clone()));
    click(&mut app, "status/preferences").unwrap();
    assert!(app.view_state().prefs_open);
    let value = |app: &mut App, id: &str| app.access_tree().find(id).unwrap().value.clone();

    click(&mut app, "prefs/audio").unwrap();
    assert_eq!(
        value(&mut app, "prefs/audio").as_deref(),
        Some("None (virtual clock)")
    );
    assert_eq!(Preferences::load(&path).audio, AudioChoice::Virtual);

    // Autosave every 5 minutes, then never.
    click(&mut app, "prefs/autosave_every").unwrap();
    assert_eq!(
        app.autosave_every,
        Some(std::time::Duration::from_secs(300))
    );
    click(&mut app, "prefs/autosave").unwrap();
    assert_eq!(app.autosave_every, None);

    // A record offset, typed.
    click(&mut app, "prefs/record_offset").unwrap();
    type_keys(&mut app, &["-", "1", "2", ".", "5", "return"]);
    assert_eq!(app.studio.engine_status().record_offset_ms, -12.5);
    assert_eq!(Preferences::load(&path).record_offset_ms, -12.5);

    // The UI scale: the same window, in bigger units.
    let before = app.access_tree().find("window").unwrap().bounds;
    click(&mut app, "prefs/scale").unwrap();
    assert_eq!(app.preferences().ui_scale, 1.1);
    let after = app.access_tree().find("window").unwrap().bounds;
    assert!(
        (after.w * 1.1 - before.w).abs() < 0.5,
        "{after:?} vs {before:?}"
    );
    assert_eq!(app.window().size, WindowInfo::headless().size);

    // Plugin processes, shared by default.
    assert!(app.preferences().share_plugin_processes);
    click(&mut app, "prefs/plugins/share").unwrap();
    assert!(!Preferences::load(&path).share_plugin_processes);
    // Sandboxed by default.
    assert!(app.preferences().sandbox_plugins);
    click(&mut app, "prefs/plugins/sandbox").unwrap();
    assert!(!Preferences::load(&path).sandbox_plugins);

    // Plugin folders, added from the folder dialog and removed.
    app.take_platform_requests();
    click(&mut app, "prefs/plugins/add_lv2").unwrap();
    assert_eq!(
        app.take_platform_requests(),
        [PlatformRequest::ChooseFolder {
            kind: PluginKind::Lv2
        }]
    );
    app.add_plugin_folder(PluginKind::Lv2, Path::new("/opt/lv2"));
    assert_eq!(
        value(&mut app, "prefs/plugins/folder/lv2/0").as_deref(),
        Some("/opt/lv2")
    );
    click(&mut app, "prefs/plugins/remove/lv2/0").unwrap();
    assert!(Preferences::load(&path).lv2_folders.is_empty());
    click(&mut app, "prefs/plugins/add_vst3").unwrap();
    assert_eq!(
        app.take_platform_requests(),
        [PlatformRequest::ChooseFolder {
            kind: PluginKind::Vst3
        }]
    );
    app.add_plugin_folder(PluginKind::Vst3, Path::new("/opt/vst3"));
    assert_eq!(
        Preferences::load(&path).vst3_folders,
        [std::path::PathBuf::from("/opt/vst3")]
    );
    assert_eq!(
        value(&mut app, "prefs/plugins/folder/vst3/0").as_deref(),
        Some("/opt/vst3")
    );
    click(&mut app, "prefs/plugins/remove/vst3/0").unwrap();
    assert!(Preferences::load(&path).vst3_folders.is_empty());

    app.key("escape", Modifiers::default()).unwrap();
    assert!(!app.view_state().prefs_open);
    app.key(",", CTRL).unwrap();
    assert!(app.view_state().prefs_open);
}

#[test]
fn the_preferences_serve_mcp_over_http() {
    use std::cell::RefCell;
    use std::net::SocketAddr;
    use std::rc::Rc;

    use crate::mcp_http::{McpEndpoint, McpHttpServer, McpHttpState};
    use crate::prefs::Preferences;

    /// Records what it's asked to serve; port 9999 is taken.
    struct Fake(Rc<RefCell<Vec<Option<McpEndpoint>>>>);
    impl McpHttpServer for Fake {
        fn serve(&mut self, endpoint: Option<McpEndpoint>) -> Result<Option<SocketAddr>, String> {
            self.0.borrow_mut().push(endpoint.clone());
            match endpoint {
                Some(e) if e.addr.port() == 9999 => Err("port 9999 is taken".into()),
                e => Ok(e.map(|e| e.addr)),
            }
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let prefs_path = dir.path().join("dawkey/preferences.json");
    let token_path = dir.path().join("dawkey/mcp-token");
    let mut app = app();
    let status = |app: &mut App| {
        app.access_tree()
            .find("prefs/mcp_status")
            .unwrap()
            .value
            .clone()
            .unwrap()
    };
    click(&mut app, "status/preferences").unwrap();
    assert_eq!(app.mcp_http_state(), &McpHttpState::Unavailable);
    assert!(status(&mut app).starts_with("Only the window"));

    let calls = Rc::new(RefCell::new(Vec::new()));
    app.set_preferences(Preferences::default(), Some(prefs_path.clone()));
    app.set_mcp_http_server(Box::new(Fake(calls.clone())), token_path.clone());
    // Off by default, and no token is made until it's on.
    assert_eq!(calls.borrow().as_slice(), [None]);
    assert_eq!(app.mcp_http_state(), &McpHttpState::Off);
    assert!(!token_path.exists());

    click(&mut app, "prefs/mcp_http").unwrap();
    let token = std::fs::read_to_string(&token_path).unwrap();
    let served = calls.borrow().last().cloned().flatten().unwrap();
    assert_eq!(served.addr, "127.0.0.1:7381".parse().unwrap());
    assert_eq!(served.token, token.trim());
    assert_eq!(status(&mut app), "Serving http://127.0.0.1:7381/mcp");
    assert!(Preferences::load(&prefs_path).mcp_http);

    // Another port, typed; one that's taken says so.
    click(&mut app, "prefs/mcp_port").unwrap();
    type_keys(&mut app, &["8", "0", "0", "0", "return"]);
    assert_eq!(app.preferences().mcp_http_port, 8000);
    assert_eq!(status(&mut app), "Serving http://127.0.0.1:8000/mcp");
    click(&mut app, "prefs/mcp_port").unwrap();
    type_keys(&mut app, &["9", "9", "9", "9", "return"]);
    assert_eq!(status(&mut app), "Not serving: port 9999 is taken");
    click(&mut app, "prefs/mcp_port").unwrap();
    type_keys(&mut app, &["8", "0", "return"]);
    assert_eq!(app.preferences().mcp_http_port, 9999);
    click(&mut app, "prefs/mcp_port").unwrap();
    type_keys(&mut app, &["8", "0", "0", "0", "return"]);

    // A new token replaces the old one at once.
    click(&mut app, "prefs/mcp_token").unwrap();
    let renewed = std::fs::read_to_string(&token_path).unwrap();
    assert_ne!(renewed, token);
    let served = calls.borrow().last().cloned().flatten().unwrap();
    assert_eq!(served.token, renewed.trim());

    click(&mut app, "prefs/mcp_http").unwrap();
    assert_eq!(calls.borrow().last(), Some(&None));
    assert_eq!(app.mcp_http_state(), &McpHttpState::Off);
}

#[test]
fn the_theme_follows_the_preferences_or_the_desktop() {
    use crate::prefs::{Preferences, ThemeChoice};
    use crate::theme::Theme;
    let mut app = app();
    let toolbar = |app: &mut App| {
        app.rebuild();
        app.scene()
            .primitives
            .iter()
            .find_map(|p| match p {
                dawkey_ui::Primitive::Rect { color, .. } => Some(*color),
                _ => None,
            })
            .unwrap()
    };
    // Following the desktop, which says nothing yet: dark.
    assert_eq!(app.theme(), Theme::Dark);
    let dark = toolbar(&mut app);
    app.set_desktop_dark(Some(false));
    assert_eq!(app.theme(), Theme::Light);
    assert_ne!(toolbar(&mut app), dark);
    // The preferences can pin one.
    app.set_preferences(
        Preferences {
            theme: ThemeChoice::Dark,
            ..Preferences::default()
        },
        None,
    );
    assert_eq!(app.theme(), Theme::Dark);
    assert_eq!(toolbar(&mut app), dark);
    click(&mut app, "status/preferences").unwrap();
    click(&mut app, "prefs/theme").unwrap();
    assert_eq!(app.preferences().theme, ThemeChoice::Light);
    assert_eq!(app.theme(), Theme::Light);
}

#[test]
fn parameters_a_plugin_changes_go_into_the_project() {
    let (mut app, _dir) = app_with_plugins();
    // Key 127 makes the test instrument turn its Level to half, as a knob
    // on its GUI would.
    let (track, _) = add_midi(&mut app, 0.0, &[(127, 0.0, 0.5)]);
    let slot = app
        .studio
        .execute(
            dawkey_core::AddPlugin {
                track: dawkey_core::TrackId(track),
                plugin: "clap:dawkey.test.beep".into(),
                index: None,
            }
            .into(),
        )
        .unwrap()
        .plugin
        .unwrap();
    let revision = app.studio.session().revision();
    app.studio.play().unwrap();
    app.studio.advance(0.1).unwrap();
    app.tick();
    assert_eq!(slot_param(&app, slot), Some(0.5));
    // One undo step.
    assert_eq!(app.studio.session().revision(), revision + 1);
    app.studio.undo().unwrap();
    assert_eq!(slot_param(&app, slot), None);
}

#[test]
fn the_window_keeps_drawing_while_stopped_when_something_moves() {
    let mut app = app();
    add_clip(&mut app, 0.0);
    app.rebuild();
    app.tick();
    assert!(!app.animating(), "nothing moves");

    // Meters fall back after playback stops.
    app.studio.play().unwrap();
    app.studio.advance(0.25).unwrap();
    app.studio.stop().unwrap();
    app.tick();
    assert!(app.animating(), "the meters are still falling back");

    // A change that doesn't come from the window's input (a plugin's GUI).
    let mut app = self::app();
    let track = add_track(&mut app, "Bass");
    app.rebuild();
    app.tick();
    assert!(!app.animating());
    app.studio
        .execute(
            dawkey_core::SetTrackGain {
                track: dawkey_core::TrackId(track),
                gain_db: -6.0,
            }
            .into(),
        )
        .unwrap();
    app.tick();
    assert!(app.animating(), "the project changed");
    app.rebuild();
    assert!(!app.animating());
}

#[test]
fn sends_in_the_mixer() {
    let mut app = app();
    let track = add_track(&mut app, "Keys");
    // No bus: nothing to send to, so no button.
    click(&mut app, "view/mixer").unwrap();
    assert!(
        app.access_tree()
            .find(&format!("track/{track}/add_send"))
            .is_none()
    );
    let bus = app
        .studio
        .execute(
            AddTrack {
                kind: TrackKind::Bus,
                ..Default::default()
            }
            .into(),
        )
        .unwrap()
        .track
        .unwrap()
        .0;
    click(&mut app, &format!("track/{track}/add_send")).unwrap();
    let sends = &app.studio.session().project().tracks[0].sends;
    assert_eq!(sends.len(), 1);
    assert_eq!((sends[0].bus.0, sends[0].gain_db), (bus, 0.0));
    // One bus: nothing more to add; the bus can't send to itself.
    let tree = app.access_tree();
    assert!(tree.find(&format!("track/{track}/add_send")).is_none());
    assert!(tree.find(&format!("track/{bus}/add_send")).is_none());
    let row = format!("track/{track}/send/{bus}");
    assert_eq!(tree.find(&row).unwrap().value.as_deref(), Some("+0.0 dB"));
    // Dragging left turns it down, as one undo step.
    app.drag(
        DragFrom::Widget(row.clone()),
        Point::new(-SEND_W * 0.25, 0.0),
        8,
        Modifiers::default(),
    )
    .unwrap();
    assert!(app.studio.session().project().tracks[0].sends[0].gain_db < -10.0);
    app.studio.undo().unwrap();
    assert_eq!(
        app.studio.session().project().tracks[0].sends[0].gain_db,
        0.0
    );
    app.studio.redo().unwrap();
    // Ctrl-click removes it, and the button is back.
    app.click(&row, MouseButton::Left, CTRL).unwrap();
    assert!(app.studio.session().project().tracks[0].sends.is_empty());
    assert!(
        app.access_tree()
            .find(&format!("track/{track}/add_send"))
            .is_some()
    );
}
