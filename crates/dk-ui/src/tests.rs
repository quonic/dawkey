// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::*;

/// Every character is 7 px wide; lines are 1.25 × the font size.
struct FakeText;

impl TextMeasure for FakeText {
    fn measure(&mut self, text: &str, style: &TextStyle) -> Size {
        Size::new(text.chars().count() as f32 * 7.0, style.size * 1.25)
    }
}

fn laid_out(root: Node, w: f32, h: f32) -> Ui {
    let mut ui = Ui::new();
    ui.set_root(root, Size::new(w, h), &mut FakeText);
    ui
}

fn rect(ui: &Ui, id: &str) -> Rect {
    ui.find(id).unwrap().rect()
}

#[test]
fn row_splits_fixed_fit_and_fill() {
    let ui = laid_out(
        Node::row()
            .fill()
            .padding(Insets::all(10.0))
            .gap(5.0)
            .child(Node::row().id("fixed").size(50.0, 20.0))
            .child(Node::label("abcd").id("fit"))
            .child(Node::row().id("a").width(Sizing::Fill(1.0)))
            .child(Node::row().id("b").width(Sizing::Fill(3.0))),
        400.0,
        100.0,
    );
    assert_eq!(rect(&ui, "fixed"), Rect::new(10.0, 40.0, 50.0, 20.0));
    // 4 chars × 7 px; centered vertically by default.
    let fit = rect(&ui, "fit");
    assert_eq!((fit.x, fit.w), (65.0, 28.0));
    // Free: 380 - 50 - 28 - 3 gaps × 5 = 287, split 1:3.
    let (a, b) = (rect(&ui, "a"), rect(&ui, "b"));
    assert_eq!((a.x, a.w), (98.0, 71.75));
    assert_eq!((b.x, b.w), (174.75, 215.25));
    assert_eq!(b.right(), 390.0);
}

#[test]
fn column_alignment_and_justify() {
    let ui = laid_out(
        Node::column()
            .fill()
            .align(Align::End)
            .justify(Justify::Center)
            .child(Node::row().id("x").size(20.0, 10.0))
            .child(
                Node::row()
                    .id("y")
                    .size(40.0, 10.0)
                    .width(Sizing::Fill(1.0)),
            ),
        100.0,
        100.0,
    );
    assert_eq!(rect(&ui, "x"), Rect::new(80.0, 40.0, 20.0, 10.0));
    // Fill across a column's cross axis takes the full width.
    assert_eq!(rect(&ui, "y"), Rect::new(0.0, 50.0, 100.0, 10.0));
}

fn chrome_window() -> Node {
    Node::column()
        .fill()
        .child(
            Node::row()
                .id("titlebar")
                .role(Role::TitleBar)
                .chrome(Chrome::DragArea)
                .width(Sizing::Fill(1.0))
                .height(Sizing::Fixed(30.0))
                .child(Node::spacer())
                .child(
                    Node::row()
                        .id("close")
                        .role(Role::Button)
                        .chrome(Chrome::Close)
                        .size(30.0, 30.0),
                ),
        )
        .child(
            Node::row()
                .id("play")
                .role(Role::Button)
                .clickable()
                .size(40.0, 40.0),
        )
        .child(
            Node::row()
                .id("disabled")
                .clickable()
                .enabled(false)
                .size(40.0, 40.0),
        )
}

#[test]
fn clicks_hover_and_chrome() {
    let mut ui = laid_out(chrome_window(), 200.0, 200.0);

    assert_eq!(
        ui.click("play", MouseButton::Left, 0).unwrap(),
        [UiAction::Click {
            id: "play".into(),
            pos: rect(&ui, "play").center(),
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
            count: 1,
        }]
    );
    // Clicking again soon after is a double-click; much later it isn't.
    let count = |actions: &[UiAction]| match actions {
        [UiAction::Click { count, .. }] => *count,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        count(&ui.click("play", MouseButton::Left, 1000).unwrap()),
        1
    );
    assert_eq!(
        count(&ui.click("play", MouseButton::Left, 1300).unwrap()),
        2
    );
    // A third quick click starts over.
    assert_eq!(
        count(&ui.click("play", MouseButton::Left, 1400).unwrap()),
        1
    );
    assert_eq!(
        count(&ui.click("play", MouseButton::Left, 5000).unwrap()),
        1
    );
    assert!(ui.state().is_hovered("play"));
    assert_eq!(
        ui.click("close", MouseButton::Left, 0).unwrap(),
        [UiAction::Chrome(ChromeAction::Close)]
    );
    assert!(ui.click("disabled", MouseButton::Left, 0).is_err());
    assert!(ui.click("missing", MouseButton::Left, 0).is_err());

    // Press on the title bar moves; a quick second press maximizes.
    let pos = Point::new(20.0, 10.0);
    let down = |time_ms| InputEvent::PointerDown {
        pos,
        button: MouseButton::Left,
        time_ms,
    };
    assert_eq!(
        ui.handle(down(1000)),
        [UiAction::Chrome(ChromeAction::Move)]
    );
    assert_eq!(
        ui.handle(down(1200)),
        [UiAction::Chrome(ChromeAction::ToggleMaximize)]
    );
    assert_eq!(
        ui.handle(down(5000)),
        [UiAction::Chrome(ChromeAction::Move)]
    );
    assert_eq!(
        ui.handle(InputEvent::PointerDown {
            pos,
            button: MouseButton::Right,
            time_ms: 6000
        }),
        [UiAction::Chrome(ChromeAction::WindowMenu(pos))]
    );
}

#[test]
fn release_outside_cancels_the_click() {
    let mut ui = laid_out(chrome_window(), 200.0, 200.0);
    let play = rect(&ui, "play").center();
    ui.handle(InputEvent::PointerDown {
        pos: play,
        button: MouseButton::Left,
        time_ms: 0,
    });
    assert!(ui.state().is_pressed("play"));
    let actions = ui.handle(InputEvent::PointerUp {
        pos: Point::new(190.0, 190.0),
        button: MouseButton::Left,
    });
    assert!(actions.is_empty());
    assert_eq!(ui.state().pressed, None);
}

#[test]
fn access_tree_skips_anonymous_containers() {
    let ui = laid_out(
        Node::column()
            .fill()
            .child(Node::row().child(Node::label("Hello").id("greeting")))
            .child(
                Node::row()
                    .id("mute")
                    .role(Role::Toggle)
                    .clickable()
                    .checked(true)
                    .text("M"),
            ),
        100.0,
        100.0,
    );
    let tree = ui.access_tree();
    let ids: Vec<_> = tree
        .children
        .iter()
        .map(|c| c.id.as_ref().unwrap().as_str())
        .collect();
    assert_eq!(ids, ["greeting", "mute"]);
    let json = serde_json::to_value(tree.find("mute").unwrap()).unwrap();
    assert_eq!(json["role"], "toggle");
    assert_eq!(json["checked"], true);
    assert_eq!(json["clickable"], true);
    assert_eq!(json["name"], "M");
    assert!(
        json.get("enabled").is_none(),
        "enabled is omitted when true"
    );
}

#[test]
fn scene_paints_backgrounds_text_and_clips() {
    let ui = laid_out(
        Node::row()
            .fill()
            .background(Color::hex(0x101010))
            .clip()
            .child(Node::label("Hi").style(|s| s.color = Color::hex(0xff0000)))
            .child(Node::row().size(20.0, 20.0).paint(Paint::Icon(Icon::Close))),
        100.0,
        50.0,
    );
    let scene = ui.scene();
    assert_eq!(scene.size, Size::new(100.0, 50.0));
    assert!(matches!(
        scene.primitives[0],
        Primitive::Rect { clip: None, .. }
    ));
    let Primitive::Text {
        text, color, clip, ..
    } = &scene.primitives[1]
    else {
        panic!("{:?}", scene.primitives[1]);
    };
    assert_eq!(text, "Hi");
    assert_eq!(*color, Color::hex(0xff0000));
    assert_eq!(*clip, Some(Rect::new(0.0, 0.0, 100.0, 50.0)));
    let lines = scene
        .primitives
        .iter()
        .filter(|p| matches!(p, Primitive::Line { .. }))
        .count();
    assert_eq!(lines, 2, "the close icon is two strokes");
}

fn timeline() -> Node {
    Node::column().fill().child(
        Node::row()
            .id("lanes")
            .scrollable()
            .fill()
            .clip()
            .child(
                Node::row()
                    .id("clip")
                    .draggable()
                    .absolute(Rect::new(50.0, 10.0, 40.0, 20.0)),
            )
            // Partly outside the clipped lane area: not hittable there.
            .child(
                Node::row()
                    .id("offscreen")
                    .clickable()
                    .absolute(Rect::new(190.0, 10.0, 40.0, 20.0)),
            ),
    )
}

#[test]
fn absolute_children_sit_outside_the_flow() {
    let ui = laid_out(timeline(), 200.0, 100.0);
    assert_eq!(rect(&ui, "clip"), Rect::new(50.0, 10.0, 40.0, 20.0));
    assert_eq!(rect(&ui, "lanes"), Rect::new(0.0, 0.0, 200.0, 100.0));
    assert!(ui.hit_test(Point::new(195.0, 15.0)).is_some());
    assert!(
        ui.hit_test(Point::new(210.0, 15.0)).is_none(),
        "clipped away"
    );
}

#[test]
fn drags_start_after_a_threshold() {
    let mut ui = laid_out(timeline(), 200.0, 100.0);
    let from = rect(&ui, "clip").center();

    // A tiny wobble is still a click.
    let actions = ui.drag(from, Point::new(from.x + 1.0, from.y), 2, 0);
    assert!(matches!(&actions[..], [UiAction::Click { id, .. }] if id.as_str() == "clip"));

    let to = Point::new(from.x + 30.0, from.y + 5.0);
    let actions = ui.drag(from, to, 3, 1000);
    let phases: Vec<DragPhase> = actions
        .iter()
        .filter_map(|a| match a {
            UiAction::Drag { phase, .. } => Some(*phase),
            _ => None,
        })
        .collect();
    assert_eq!(
        phases,
        [
            DragPhase::Start,
            DragPhase::Move,
            DragPhase::Move,
            DragPhase::Move,
            DragPhase::End
        ]
    );
    let UiAction::Drag { origin, pos, .. } = actions.last().unwrap() else {
        unreachable!()
    };
    assert_eq!((*origin, *pos), (from, to));
    assert!(!ui.state().dragging);
}

#[test]
fn scroll_keys_and_modifiers() {
    let mut ui = laid_out(timeline(), 200.0, 100.0);
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    ui.handle(InputEvent::Modifiers(ctrl));
    let actions = ui.handle(InputEvent::Scroll {
        pos: Point::new(10.0, 10.0),
        delta: Point::new(0.0, 40.0),
    });
    assert!(matches!(
        &actions[..],
        [UiAction::Scroll { id: Some(id), modifiers, .. }] if id.as_str() == "lanes" && modifiers.ctrl
    ));
    let actions = ui.handle(InputEvent::Key { key: "z".into() });
    assert_eq!(
        actions,
        [UiAction::Key {
            key: "z".into(),
            modifiers: ctrl
        }]
    );
}
