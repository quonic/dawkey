// SPDX-License-Identifier: AGPL-3.0-or-later
//! Colors and metrics. The colors come from the [`Palette`] in use, dark or
//! light: the preferences pick one, or follow the desktop's light or dark
//! setting. Views read it through the functions here (`text()`,
//! `accent()`, ...), which [`App::rebuild`](crate::App::rebuild) points at
//! the palette before it builds the view (on the app's thread).

use std::cell::Cell;

use dawkey_ui::Color;
use serde::Serialize;

use crate::prefs::ThemeChoice;

/// The colors the views use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Palette {
    pub(crate) window: Color,
    pub(crate) titlebar: Color,
    pub(crate) titlebar_inactive: Color,
    pub(crate) toolbar: Color,
    pub(crate) panel: Color,
    pub(crate) track: Color,
    pub(crate) border: Color,
    pub(crate) text: Color,
    pub(crate) text_dim: Color,
    pub(crate) hover: Color,
    pub(crate) pressed: Color,
    pub(crate) accent: Color,
    pub(crate) close_hover: Color,
    pub(crate) mute_on: Color,
    pub(crate) record_on: Color,
    pub(crate) solo_on: Color,
    pub(crate) monitor_on: Color,
    /// Text on a button that's on (in one of the colors above).
    pub(crate) on_text: Color,
    pub(crate) shadow: Color,
    /// Track lanes, alternating.
    pub(crate) lane_a: Color,
    pub(crate) lane_b: Color,
    /// Under a meter's bars.
    pub(crate) meter: Color,
}

impl Palette {
    pub(crate) const DARK: Self = Self {
        window: Color::hex(0x1e2025),
        titlebar: Color::hex(0x16171b),
        titlebar_inactive: Color::hex(0x1b1c20),
        toolbar: Color::hex(0x23262c),
        panel: Color::hex(0x1a1c20),
        track: Color::hex(0x262a31),
        border: Color::hex(0x34373f),
        text: Color::hex(0xe6e8eb),
        text_dim: Color::hex(0x9aa0a8),
        hover: Color::hex(0x31353e),
        pressed: Color::hex(0x3b404a),
        accent: Color::hex(0x4c8dff),
        close_hover: Color::hex(0xe5484d),
        mute_on: Color::hex(0xf0b429),
        record_on: Color::hex(0xe5484d),
        solo_on: Color::hex(0x3dd68c),
        monitor_on: Color::hex(0x2ec4c4),
        on_text: Color::hex(0x101114),
        shadow: Color::rgba(0.0, 0.0, 0.0, 0.45),
        lane_a: Color::hex(0x1d1f24),
        lane_b: Color::hex(0x202228),
        meter: Color::hex(0x1b1d22),
    };

    pub(crate) const LIGHT: Self = Self {
        window: Color::hex(0xf3f4f6),
        titlebar: Color::hex(0xe2e4e8),
        titlebar_inactive: Color::hex(0xebedf0),
        toolbar: Color::hex(0xeaecef),
        panel: Color::hex(0xfafbfc),
        track: Color::hex(0xe4e7eb),
        border: Color::hex(0xc5c9d0),
        text: Color::hex(0x1b1d21),
        text_dim: Color::hex(0x5b616a),
        hover: Color::hex(0xdadde2),
        pressed: Color::hex(0xcbcfd6),
        accent: Color::hex(0x2f6fe4),
        close_hover: Color::hex(0xe5484d),
        mute_on: Color::hex(0xe0a315),
        record_on: Color::hex(0xdf3e44),
        solo_on: Color::hex(0x23a86b),
        monitor_on: Color::hex(0x1b9e9e),
        on_text: Color::hex(0xffffff),
        shadow: Color::rgba(0.0, 0.0, 0.0, 0.22),
        lane_a: Color::hex(0xeef0f3),
        lane_b: Color::hex(0xe8eaee),
        meter: Color::hex(0xd5d8dd),
    };
}

/// Which palette is in use, as `ui_tree` reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

impl Theme {
    pub(crate) fn palette(self) -> Palette {
        match self {
            Self::Dark => Palette::DARK,
            Self::Light => Palette::LIGHT,
        }
    }

    /// The theme `choice` means, with the desktop preferring dark (`Some`
    /// true), light, or nothing known (dark).
    pub(crate) fn of(choice: ThemeChoice, desktop_dark: Option<bool>) -> Self {
        match choice {
            ThemeChoice::Dark => Self::Dark,
            ThemeChoice::Light => Self::Light,
            ThemeChoice::System if desktop_dark == Some(false) => Self::Light,
            ThemeChoice::System => Self::Dark,
        }
    }
}

thread_local! {
    static PALETTE: Cell<Palette> = const { Cell::new(Palette::DARK) };
}

/// Makes `palette` the one the views (and text without a color) use, on
/// this thread.
pub(crate) fn use_palette(palette: Palette) {
    PALETTE.set(palette);
    dawkey_ui::set_default_text_color(palette.text);
}

/// The palette in use (see [`use_palette`]).
pub(crate) fn pal() -> Palette {
    PALETTE.get()
}

/// Space around a floating window for its shadow (and resize handles).
pub const SHADOW_MARGIN: f32 = 16.0;
pub(crate) const SHADOW_BLUR: f32 = 12.0;
pub(crate) const WINDOW_RADIUS: f32 = 10.0;
pub(crate) const TITLEBAR_HEIGHT: f32 = 38.0;
/// Track colors, cycled by track position (the same in either theme).
pub(crate) const TRACK_COLORS: [Color; 8] = [
    Color::hex(0x4c8dff),
    Color::hex(0x3dd68c),
    Color::hex(0xf0b429),
    Color::hex(0xe5484d),
    Color::hex(0xb072ff),
    Color::hex(0x2ec4c4),
    Color::hex(0xff8a3d),
    Color::hex(0xe864b5),
];

impl crate::App {
    /// Uses the theme the preferences pick (following the desktop's, if
    /// they say so).
    pub(crate) fn apply_theme(&mut self) {
        self.theme = Theme::of(self.prefs.theme, self.desktop_dark);
        self.redraw = true;
        self.rebuild();
    }

    /// The desktop prefers dark (`Some(true)`) or light; `None` if it
    /// doesn't say. With the theme following the desktop, it changes too.
    pub fn set_desktop_dark(&mut self, dark: Option<bool>) {
        self.desktop_dark = dark;
        self.apply_theme();
    }

    pub fn theme(&self) -> Theme {
        self.theme
    }
}
