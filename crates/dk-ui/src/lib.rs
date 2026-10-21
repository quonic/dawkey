// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dawkey's UI toolkit.
//!
//! Views are plain functions that build a [`Node`] tree every frame. Nodes that
//! matter carry a stable [`WidgetId`] (like `transport/play`), which is what
//! input, styling state, tests and the MCP `ui_*` tools refer to. [`Ui`] lays
//! the tree out, routes input to IDs, and produces a [`Scene`] (a display list
//! in logical pixels) and an [`AccessNode`] tree.
//!
//! The crate knows nothing about GPUs, fonts or Wayland: text is measured
//! through [`TextMeasure`], scenes are drawn by `dk-render`, and window chrome
//! requests ([`ChromeAction`]) are carried out by the platform.

#![forbid(unsafe_code)]

mod access;
mod geometry;
mod layout;
mod node;
mod scene;
mod ui;

pub use access::AccessNode;
pub use geometry::{Color, Insets, Point, Rect, Size};
pub use node::{
    Align, Chrome, Cursor, Direction, Icon, Justify, Node, Paint, Role, Sizing, Style, TextAlign,
    WidgetId, set_default_text_color,
};
pub use scene::{FontFamily, FontWeight, Primitive, Scene, TextStyle};
pub use ui::{ChromeAction, DragPhase, InputEvent, Modifiers, MouseButton, Ui, UiAction, UiState};

/// Measures text for layout. Implemented by `dk-text`.
pub trait TextMeasure {
    /// Size of `text` on a single line, in logical pixels.
    fn measure(&mut self, text: &str, style: &TextStyle) -> Size;
}

#[cfg(test)]
mod tests;
