// SPDX-License-Identifier: AGPL-3.0-or-later
//! Input routing and per-widget interaction state.

use serde::{Deserialize, Serialize};

use crate::TextMeasure;
use crate::access::AccessNode;
use crate::geometry::{Point, Rect, Size};
use crate::layout::layout;
use crate::node::{Chrome, Cursor, Node, WidgetId};
use crate::scene::Scene;

/// Two presses within this many milliseconds are a double-click.
const DOUBLE_CLICK_MS: u32 = 400;
/// ... if they're at most this far apart (logical px).
const DOUBLE_CLICK_SLOP: f32 = 4.0;
/// A press becomes a drag once the pointer moves this far (logical px).
const DRAG_THRESHOLD: f32 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Keyboard modifiers held during an event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modifiers {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub logo: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    PointerMove(Point),
    PointerLeave,
    /// `time_ms` is the event's timestamp, used for double-clicks.
    PointerDown {
        pos: Point,
        button: MouseButton,
        time_ms: u32,
    },
    PointerUp {
        pos: Point,
        button: MouseButton,
    },
    /// Scroll by `delta` logical pixels (positive y scrolls content up, as
    /// when turning the wheel towards you).
    Scroll {
        pos: Point,
        delta: Point,
    },
    /// The held modifiers changed.
    Modifiers(Modifiers),
    /// A key was pressed. `key` is a lowercase name: a character such as
    /// "a" or "+", or a named key such as "space", "delete", "home",
    /// "left", "escape".
    Key {
        key: String,
    },
}

/// A window-management request for the platform.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChromeAction {
    /// Start an interactive move (on press, with the press's serial).
    Move,
    ToggleMaximize,
    Minimize,
    Close,
    WindowMenu(Point),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DragPhase {
    Start,
    Move,
    End,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAction {
    Click {
        id: WidgetId,
        pos: Point,
        button: MouseButton,
        modifiers: Modifiers,
        /// 1 for a single click, 2 for the second click of a double-click.
        count: u8,
    },
    /// A drag on a draggable widget. `pos` is the current pointer position;
    /// `origin` is where the press happened.
    Drag {
        id: WidgetId,
        phase: DragPhase,
        origin: Point,
        pos: Point,
        modifiers: Modifiers,
    },
    /// Scrolling over a scrollable widget (`id`), or over nothing scrollable.
    Scroll {
        id: Option<WidgetId>,
        pos: Point,
        delta: Point,
        modifiers: Modifiers,
    },
    Key {
        key: String,
        modifiers: Modifiers,
    },
    Chrome(ChromeAction),
}

/// Interaction state that views read to style hovered, pressed and dragged
/// widgets.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UiState {
    pub hovered: Option<WidgetId>,
    pub pressed: Option<WidgetId>,
    /// True once the pressed widget is being dragged.
    pub dragging: bool,
    pub modifiers: Modifiers,
}

impl UiState {
    pub fn is_hovered(&self, id: &str) -> bool {
        self.hovered.as_ref().is_some_and(|h| h.as_str() == id)
    }

    pub fn is_pressed(&self, id: &str) -> bool {
        self.pressed.as_ref().is_some_and(|p| p.as_str() == id)
    }
}

#[derive(Debug, Default)]
pub struct Ui {
    state: UiState,
    root: Node,
    size: Size,
    last_drag_press_ms: Option<u32>,
    /// Where the current press happened, and with which button.
    press: Option<(Point, MouseButton)>,
    /// When the current press happened.
    press_ms: u32,
    /// The last click: widget, time of its press, position.
    last_click: Option<(WidgetId, u32, Point)>,
    /// Where the pointer last was, while it's over the window.
    pointer: Option<Point>,
}

impl Ui {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> &UiState {
        &self.state
    }

    pub fn size(&self) -> Size {
        self.size
    }

    /// Where the pointer is, if it's over the window (or dragging outside it).
    pub fn pointer(&self) -> Option<Point> {
        self.pointer
    }

    /// Installs a freshly built tree and lays it out at `size`.
    pub fn set_root(&mut self, mut root: Node, size: Size, text: &mut dyn TextMeasure) {
        layout(&mut root, Rect::from_size(size), text);
        self.root = root;
        self.size = size;
        // A widget that disappeared can't stay hovered or pressed.
        if self
            .state
            .hovered
            .as_ref()
            .is_some_and(|id| self.root.find(id.as_str()).is_none())
        {
            self.state.hovered = None;
        }
        if self
            .state
            .pressed
            .as_ref()
            .is_some_and(|id| self.root.find(id.as_str()).is_none())
        {
            self.state.pressed = None;
            self.state.dragging = false;
            self.press = None;
        }
    }

    /// The laid-out tree.
    pub fn root(&self) -> &Node {
        &self.root
    }

    pub fn find(&self, id: &str) -> Option<&Node> {
        self.root.find(id)
    }

    pub fn scene(&self) -> Scene {
        Scene::paint(&self.root, self.size)
    }

    pub fn access_tree(&self) -> AccessNode {
        AccessNode::build(&self.root)
    }

    /// The interactive widget under `pos`, if any.
    pub fn hit_test(&self, pos: Point) -> Option<&Node> {
        hit(&self.root, pos, &|n| {
            (n.id.is_some() && (n.clickable || n.draggable)) || n.chrome.is_some()
        })
    }

    /// The pointer shape asked for by the widget being pressed or dragged,
    /// or else by the one under the pointer.
    pub fn cursor(&self) -> Option<Cursor> {
        let id = self
            .state
            .pressed
            .as_ref()
            .or(self.state.hovered.as_ref())?;
        self.root.find(id.as_str())?.cursor
    }

    /// Routes one input event. Returns what the app or platform should do.
    pub fn handle(&mut self, event: InputEvent) -> Vec<UiAction> {
        let modifiers = self.state.modifiers;
        let mut actions = Vec::new();
        match event {
            InputEvent::PointerMove(pos)
            | InputEvent::PointerDown { pos, .. }
            | InputEvent::PointerUp { pos, .. }
            | InputEvent::Scroll { pos, .. } => self.pointer = Some(pos),
            InputEvent::PointerLeave if !self.state.dragging => self.pointer = None,
            _ => {}
        }
        match event {
            InputEvent::Modifiers(m) => self.state.modifiers = m,
            InputEvent::Key { key } => actions.push(UiAction::Key { key, modifiers }),
            InputEvent::Scroll { pos, delta } => {
                let id = hit(&self.root, pos, &|n| n.scrollable && n.id.is_some())
                    .and_then(|n| n.id.clone());
                actions.push(UiAction::Scroll {
                    id,
                    pos,
                    delta,
                    modifiers,
                });
            }
            InputEvent::PointerMove(pos) => {
                if let (Some(id), Some((origin, MouseButton::Left))) =
                    (self.state.pressed.clone(), self.press)
                {
                    let draggable = self.find(id.as_str()).is_some_and(|n| n.draggable);
                    if draggable && !self.state.dragging && distance(origin, pos) >= DRAG_THRESHOLD
                    {
                        self.state.dragging = true;
                        actions.push(UiAction::Drag {
                            id: id.clone(),
                            phase: DragPhase::Start,
                            origin,
                            pos: origin,
                            modifiers,
                        });
                    }
                    if self.state.dragging {
                        actions.push(UiAction::Drag {
                            id,
                            phase: DragPhase::Move,
                            origin,
                            pos,
                            modifiers,
                        });
                        // The dragged widget stays "hovered".
                        return actions;
                    }
                }
                self.state.hovered = self.hit_test(pos).and_then(|n| n.id.clone());
            }
            InputEvent::PointerLeave => {
                // A drag continues outside the window until release.
                if !self.state.dragging {
                    self.state.hovered = None;
                    self.state.pressed = None;
                    self.press = None;
                }
            }
            InputEvent::PointerDown {
                pos,
                button,
                time_ms,
            } => {
                let target = self.hit_test(pos).map(|n| (n.id.clone(), n.chrome));
                self.state.hovered = target.as_ref().and_then(|(id, _)| id.clone());
                match (target, button) {
                    (Some((_, Some(Chrome::DragArea))), MouseButton::Left) => {
                        let double = self
                            .last_drag_press_ms
                            .is_some_and(|t| time_ms.wrapping_sub(t) <= DOUBLE_CLICK_MS);
                        if double {
                            self.last_drag_press_ms = None;
                            actions.push(UiAction::Chrome(ChromeAction::ToggleMaximize));
                        } else {
                            self.last_drag_press_ms = Some(time_ms);
                            actions.push(UiAction::Chrome(ChromeAction::Move));
                        }
                    }
                    (Some((_, Some(Chrome::DragArea))), MouseButton::Right) => {
                        actions.push(UiAction::Chrome(ChromeAction::WindowMenu(pos)));
                    }
                    (Some((Some(id), _)), MouseButton::Left | MouseButton::Right) => {
                        self.state.pressed = Some(id);
                        self.state.dragging = false;
                        self.press = Some((pos, button));
                        self.press_ms = time_ms;
                    }
                    _ => {}
                }
            }
            InputEvent::PointerUp { pos, button } => {
                let Some((origin, pressed_with)) = self.press else {
                    return actions;
                };
                if pressed_with != button {
                    return actions;
                }
                self.press = None;
                let Some(pressed) = self.state.pressed.take() else {
                    return actions;
                };
                if std::mem::take(&mut self.state.dragging) {
                    actions.push(UiAction::Drag {
                        id: pressed,
                        phase: DragPhase::End,
                        origin,
                        pos,
                        modifiers,
                    });
                    self.state.hovered = self.hit_test(pos).and_then(|n| n.id.clone());
                    return actions;
                }
                let Some(node) = self.hit_test(pos) else {
                    return actions;
                };
                if node.id.as_ref() != Some(&pressed) {
                    return actions;
                }
                actions.push(match (node.chrome, button) {
                    (Some(Chrome::Minimize), MouseButton::Left) => {
                        UiAction::Chrome(ChromeAction::Minimize)
                    }
                    (Some(Chrome::ToggleMaximize), MouseButton::Left) => {
                        UiAction::Chrome(ChromeAction::ToggleMaximize)
                    }
                    (Some(Chrome::Close), MouseButton::Left) => {
                        UiAction::Chrome(ChromeAction::Close)
                    }
                    _ => {
                        let double = button == MouseButton::Left
                            && self.last_click.as_ref().is_some_and(|(id, t, p)| {
                                *id == pressed
                                    && self.press_ms.wrapping_sub(*t) <= DOUBLE_CLICK_MS
                                    && distance(*p, pos) <= DOUBLE_CLICK_SLOP
                            });
                        self.last_click = (!double).then(|| (pressed.clone(), self.press_ms, pos));
                        UiAction::Click {
                            id: pressed,
                            pos,
                            button,
                            modifiers,
                            count: if double { 2 } else { 1 },
                        }
                    }
                });
            }
        }
        actions
    }

    /// Simulates a click at the center of widget `id` (for tests and MCP).
    /// Fails if the widget doesn't exist or isn't clickable there.
    pub fn click(
        &mut self,
        id: &str,
        button: MouseButton,
        time_ms: u32,
    ) -> Result<Vec<UiAction>, String> {
        let pos = self.widget_center(id)?;
        let hit = self.hit_test(pos).and_then(|n| n.id.clone());
        if hit.as_ref().map(WidgetId::as_str) != Some(id) {
            return Err(format!(
                "widget {id:?} is not clickable at its center (hit: {:?})",
                hit.map(|h| h.to_string())
            ));
        }
        Ok(self.click_at(pos, button, time_ms))
    }

    /// Simulates a double-click at the center of widget `id`.
    pub fn double_click(&mut self, id: &str, time_ms: u32) -> Result<Vec<UiAction>, String> {
        let mut actions = self.click(id, MouseButton::Left, time_ms)?;
        actions.extend(self.click(id, MouseButton::Left, time_ms.wrapping_add(100))?);
        Ok(actions)
    }

    /// Simulates a double-click at `pos`.
    pub fn double_click_at(&mut self, pos: Point, time_ms: u32) -> Vec<UiAction> {
        let mut actions = self.click_at(pos, MouseButton::Left, time_ms);
        actions.extend(self.click_at(pos, MouseButton::Left, time_ms.wrapping_add(100)));
        actions
    }

    /// Simulates a click at `pos`.
    pub fn click_at(&mut self, pos: Point, button: MouseButton, time_ms: u32) -> Vec<UiAction> {
        let mut actions = self.handle(InputEvent::PointerMove(pos));
        actions.extend(self.handle(InputEvent::PointerDown {
            pos,
            button,
            time_ms,
        }));
        actions.extend(self.handle(InputEvent::PointerUp { pos, button }));
        actions
    }

    /// Simulates a left-button drag from `from` to `to` in `steps` moves.
    pub fn drag(&mut self, from: Point, to: Point, steps: u32, time_ms: u32) -> Vec<UiAction> {
        let mut actions = self.handle(InputEvent::PointerMove(from));
        actions.extend(self.handle(InputEvent::PointerDown {
            pos: from,
            button: MouseButton::Left,
            time_ms,
        }));
        let steps = steps.max(1);
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let p = Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
            actions.extend(self.handle(InputEvent::PointerMove(p)));
        }
        actions.extend(self.handle(InputEvent::PointerUp {
            pos: to,
            button: MouseButton::Left,
        }));
        actions
    }

    /// Center of widget `id`, for synthetic input.
    pub fn widget_center(&self, id: &str) -> Result<Point, String> {
        let node = self
            .find(id)
            .ok_or_else(|| format!("no widget with id {id:?}"))?;
        if !node.enabled {
            return Err(format!("widget {id:?} is disabled"));
        }
        Ok(node.rect.center())
    }
}

fn distance(a: Point, b: Point) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

/// Deepest enabled node under `pos` that `wanted` accepts. Later siblings are
/// drawn on top, so they are checked first. Clipping nodes hide children
/// outside their bounds.
fn hit<'a>(node: &'a Node, pos: Point, wanted: &dyn Fn(&Node) -> bool) -> Option<&'a Node> {
    let inside = node.rect.contains(pos);
    // Flow children lie inside their parent, but absolutely positioned ones
    // may stick out of an unclipped parent.
    if !inside && (node.clip || !node.children.iter().any(|c| c.position.is_some())) {
        return None;
    }
    for child in node.children.iter().rev() {
        if let Some(found) = hit(child, pos, wanted) {
            return Some(found);
        }
    }
    (inside && node.enabled && wanted(node)).then_some(node)
}
