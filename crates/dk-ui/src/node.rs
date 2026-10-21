// SPDX-License-Identifier: AGPL-3.0-or-later
//! The widget tree that views build.

use std::fmt;
use std::sync::Arc;

use serde::Serialize;

use crate::geometry::{Color, Insets, Point, Rect, Size};
use crate::scene::{FontFamily, FontWeight, TextStyle};

/// A stable widget identifier, written as a path such as `track/3/mute`.
/// Tests and MCP clients address widgets by these.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct WidgetId(String);

impl WidgetId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WidgetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for WidgetId {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for WidgetId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// What a node is, for the accessibility tree and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    #[default]
    Group,
    Window,
    TitleBar,
    Toolbar,
    StatusBar,
    List,
    ListItem,
    Button,
    Toggle,
    Label,
    Meter,
    Slider,
    Timeline,
    Ruler,
    Lane,
    Clip,
    /// The piano roll's note grid.
    Grid,
    /// A MIDI note in the piano roll.
    Note,
    /// A field to type in; its value is the text.
    TextField,
    /// How far a task is; its value says.
    ProgressBar,
    /// Drags to scroll a list; its value is how far down, in percent.
    ScrollBar,
}

/// How a node is sized along one axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sizing {
    /// Exactly this many logical pixels.
    Fixed(f32),
    /// A share of the space left after fixed and fit siblings, by weight.
    /// Across the parent's axis, fills the parent.
    Fill(f32),
    /// Just big enough for the content.
    Fit,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Row,
    Column,
}

/// Cross-axis placement of children.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Align {
    Start,
    #[default]
    Center,
    End,
}

/// Main-axis placement of children when none of them fill.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Justify {
    #[default]
    Start,
    Center,
    End,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextAlign {
    Start,
    #[default]
    Center,
    End,
}

/// Window-chrome behavior, carried out by the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Chrome {
    /// Dragging moves the window; double-click toggles maximize; right-click
    /// opens the window menu.
    DragArea,
    Minimize,
    ToggleMaximize,
    Close,
}

/// A pointer shape a widget asks for while it's under the pointer or being
/// dragged. The platform shows its default arrow otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cursor {
    /// Dragging moves an edge left or right.
    ResizeHorizontal,
}

/// Vector icons drawn by the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Icon {
    Close,
    Maximize,
    Restore,
    Minimize,
    Play,
    Stop,
    Record,
}

/// Custom content painted inside a node.
#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    Icon(Icon),
    /// A stereo peak meter; levels are 0..=1 of the meter's length, over
    /// `background`.
    Meter {
        levels: [f32; 2],
        vertical: bool,
        background: Color,
    },
    /// A waveform: one (min, max) sample range per logical pixel column,
    /// -1..=1, drawn around the vertical center.
    Waveform {
        columns: Arc<[(f32, f32)]>,
        color: Color,
    },
    /// Filled rectangles, positioned relative to the node's content area
    /// (e.g. the notes of a MIDI clip).
    Rects {
        rects: Arc<[Rect]>,
        color: Color,
    },
    /// Connected line segments through `points`, relative to the node's
    /// content area (e.g. an automation curve).
    Polyline {
        points: Arc<[Point]>,
        width: f32,
        color: Color,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    /// Drop shadow: blur radius and color.
    pub shadow: Option<(f32, Color)>,
    pub background: Option<Color>,
    pub border: Option<(f32, Color)>,
    /// Corner radii: top-left, top-right, bottom-right, bottom-left.
    pub radius: [f32; 4],
    pub color: Color,
    pub font_size: f32,
    pub weight: FontWeight,
    pub family: FontFamily,
    pub text_align: TextAlign,
}

thread_local! {
    static DEFAULT_TEXT: std::cell::Cell<Color> = const { std::cell::Cell::new(Color::hex(0xffffff)) };
}

/// The color of text that doesn't say, for nodes made from now on (on this
/// thread): the theme's.
pub fn set_default_text_color(color: Color) {
    DEFAULT_TEXT.set(color);
}

impl Default for Style {
    fn default() -> Self {
        Self {
            shadow: None,
            background: None,
            border: None,
            radius: [0.0; 4],
            color: DEFAULT_TEXT.get(),
            font_size: 13.0,
            weight: FontWeight::Regular,
            family: FontFamily::Sans,
            text_align: TextAlign::Center,
        }
    }
}

impl Style {
    pub fn text_style(&self) -> TextStyle {
        TextStyle {
            size: self.font_size,
            weight: self.weight,
            family: self.family,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: Option<WidgetId>,
    pub role: Role,
    /// Accessible name, if different from `text`.
    pub label: Option<String>,
    /// Accessible value, e.g. a time readout or a meter level.
    pub value: Option<String>,
    /// Visible text, drawn on one line.
    pub text: Option<String>,
    pub paint: Option<Paint>,
    pub width: Sizing,
    pub height: Sizing,
    pub direction: Direction,
    pub gap: f32,
    pub padding: Insets,
    pub align: Align,
    pub justify: Justify,
    pub style: Style,
    /// Responds to clicks (requires an `id`).
    pub clickable: bool,
    pub enabled: bool,
    /// For toggles.
    pub checked: Option<bool>,
    pub chrome: Option<Chrome>,
    pub cursor: Option<Cursor>,
    /// Pointer drags on this node produce drag actions (requires an `id`).
    pub draggable: bool,
    /// Scroll-wheel input over this node is routed to it (requires an `id`).
    pub scrollable: bool,
    /// Placed at this rectangle relative to the parent's content area,
    /// outside the parent's row/column flow.
    pub position: Option<Rect>,
    /// Clip children to this node's bounds.
    pub clip: bool,
    pub children: Vec<Node>,
    /// Filled in by layout.
    pub(crate) rect: Rect,
    pub(crate) fit: Size,
}

impl Default for Node {
    fn default() -> Self {
        Self {
            id: None,
            role: Role::Group,
            label: None,
            value: None,
            text: None,
            paint: None,
            width: Sizing::Fit,
            height: Sizing::Fit,
            direction: Direction::Row,
            gap: 0.0,
            padding: Insets::default(),
            align: Align::Center,
            justify: Justify::Start,
            style: Style::default(),
            clickable: false,
            enabled: true,
            checked: None,
            chrome: None,
            cursor: None,
            draggable: false,
            scrollable: false,
            position: None,
            clip: false,
            children: Vec::new(),
            rect: Rect::default(),
            fit: Size::default(),
        }
    }
}

/// Builder methods. Views read best as chains:
/// `Node::row().id("toolbar").height(Sizing::Fixed(40.0)).child(...)`.
impl Node {
    pub fn row() -> Self {
        Self::default()
    }

    pub fn column() -> Self {
        Self {
            direction: Direction::Column,
            ..Self::default()
        }
    }

    pub fn label(text: impl Into<String>) -> Self {
        Self {
            role: Role::Label,
            text: Some(text.into()),
            ..Self::default()
        }
    }

    /// A flexible empty space.
    pub fn spacer() -> Self {
        Self {
            width: Sizing::Fill(1.0),
            height: Sizing::Fill(1.0),
            ..Self::default()
        }
    }

    pub fn id(mut self, id: impl Into<WidgetId>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn role(mut self, role: Role) -> Self {
        self.role = role;
        self
    }

    pub fn accessible_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub fn paint(mut self, paint: Paint) -> Self {
        self.paint = Some(paint);
        self
    }

    pub fn width(mut self, sizing: Sizing) -> Self {
        self.width = sizing;
        self
    }

    pub fn height(mut self, sizing: Sizing) -> Self {
        self.height = sizing;
        self
    }

    pub fn size(self, w: f32, h: f32) -> Self {
        self.width(Sizing::Fixed(w)).height(Sizing::Fixed(h))
    }

    pub fn fill(self) -> Self {
        self.width(Sizing::Fill(1.0)).height(Sizing::Fill(1.0))
    }

    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    pub fn padding(mut self, padding: Insets) -> Self {
        self.padding = padding;
        self
    }

    pub fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    pub fn justify(mut self, justify: Justify) -> Self {
        self.justify = justify;
        self
    }

    pub fn style(mut self, f: impl FnOnce(&mut Style)) -> Self {
        f(&mut self.style);
        self
    }

    pub fn background(self, color: Color) -> Self {
        self.style(|s| s.background = Some(color))
    }

    pub fn clickable(mut self) -> Self {
        self.clickable = true;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    pub fn chrome(mut self, chrome: Chrome) -> Self {
        self.chrome = Some(chrome);
        self
    }

    pub fn cursor(mut self, cursor: Cursor) -> Self {
        self.cursor = Some(cursor);
        self
    }

    pub fn draggable(mut self) -> Self {
        self.draggable = true;
        self
    }

    pub fn scrollable(mut self) -> Self {
        self.scrollable = true;
        self
    }

    /// Places the node at `rect` relative to the parent's content area.
    pub fn absolute(mut self, rect: Rect) -> Self {
        self.position = Some(rect);
        self
    }

    pub fn clip(mut self) -> Self {
        self.clip = true;
        self
    }

    pub fn child(mut self, child: Node) -> Self {
        self.children.push(child);
        self
    }

    pub fn children(mut self, children: impl IntoIterator<Item = Node>) -> Self {
        self.children.extend(children);
        self
    }

    /// Bounds from the last layout.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Depth-first search by ID.
    pub fn find(&self, id: &str) -> Option<&Node> {
        if self.id.as_ref().is_some_and(|i| i.as_str() == id) {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }
}
