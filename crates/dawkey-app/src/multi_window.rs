// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pure layout policy behind the multi-monitor and widescreen preferences:
//! which view goes on which monitor (or, for widescreen, which pane of one
//! window). Kept free of any Wayland or rendering types so it can be unit
//! tested without a compositor.

use dawkey_ui::{AccessNode, Color, Scene, Ui};

use crate::{App, WindowInfo};

/// One of the other windows in the multi-monitor layout: its own retained
/// UI state (hover/press/drag/scroll — everything [`Ui`] tracks) and
/// current size, so its scene is independent of the primary window's.
/// Project state (`Studio`, the shared `ViewState`) is still the one on
/// [`App`]; only the UI-interaction/rendering state is per window.
#[derive(Debug)]
pub struct ExtraWindow {
    pub role: ViewRole,
    pub(crate) ui: Ui,
    pub(crate) window: WindowInfo,
}

impl App {
    /// Applies a `layout` preference change. `Standard` (today's single
    /// window with a page switcher) and `Widescreen` (Arrange, Rack and
    /// Mixer side by side in that one window) take effect immediately.
    /// `MultiMonitor` needs the platform to call [`Self::set_window_roles`]
    /// once it has placed a window per monitor, since only it knows the
    /// monitors; this just redraws in case `layout` moved away from it.
    pub(crate) fn apply_layout(&mut self) {
        self.redraw = true;
        self.rebuild();
    }

    /// Sets this (primary) window's fixed role (`None`: the page switcher,
    /// or the Widescreen layout if that preference is set) and the roles
    /// of the other windows in the multi-monitor layout. Called by the
    /// platform once it has worked out which monitor shows what (typically
    /// from [`plan_windows`]); an empty `extra_roles` and `primary` of
    /// `None` leaves multi-monitor mode. Extra windows beyond
    /// `extra_roles.len()` are dropped; new ones start sized as
    /// `initial_window` until the platform reports their real size.
    pub fn set_window_roles(
        &mut self,
        primary: Option<ViewRole>,
        extra_roles: &[ViewRole],
        initial_window: WindowInfo,
    ) {
        self.primary_role = primary;
        self.extra_windows.truncate(extra_roles.len());
        for (i, &role) in extra_roles.iter().enumerate() {
            match self.extra_windows.get_mut(i) {
                Some(w) => w.role = role,
                None => self.extra_windows.push(ExtraWindow {
                    role,
                    ui: Ui::new(),
                    window: initial_window,
                }),
            }
        }
        self.redraw = true;
        self.rebuild();
    }

    /// The other windows' roles, in the order [`Self::resize_extra_window`]
    /// and [`Self::handle_extra_input`] index them.
    pub fn extra_window_roles(&self) -> Vec<ViewRole> {
        self.extra_windows.iter().map(|w| w.role).collect()
    }

    /// This window's fixed role in the multi-monitor layout, if any (see
    /// [`Self::set_window_roles`]).
    pub fn primary_role(&self) -> Option<ViewRole> {
        self.primary_role
    }

    /// Leaves multi-monitor mode by setting the `layout` preference back to
    /// `Standard`, the same way changing it in the preferences panel would
    /// (applied and saved the same way). For when the platform can no
    /// longer show the layout it asked for — a monitor's window was closed,
    /// say.
    pub fn set_layout_standard(&mut self) {
        self.change_preferences(|p| p.layout = crate::prefs::LayoutChoice::Standard);
    }

    /// The pointer shape one of the other windows asks for, like
    /// [`Self::cursor`] for the primary one.
    pub fn extra_cursor(&self, index: usize) -> Option<dawkey_ui::Cursor> {
        self.extra_windows.get(index)?.ui.cursor()
    }

    /// Resizes one of the other windows (by index into
    /// [`Self::extra_window_roles`]).
    pub fn resize_extra_window(&mut self, index: usize, window: WindowInfo) {
        if let Some(w) = self.extra_windows.get_mut(index) {
            w.window = window;
            self.redraw = true;
        }
    }

    pub fn extra_scene(&mut self, index: usize) -> Option<Scene> {
        self.rebuild();
        self.extra_windows.get(index).map(|w| w.ui.scene())
    }

    pub fn extra_access_tree(&mut self, index: usize) -> Option<AccessNode> {
        self.rebuild();
        self.extra_windows.get(index).map(|w| w.ui.access_tree())
    }

    /// Draws one of the other windows into a surface texture, the same way
    /// [`Self::draw`] does the primary one, sharing its GPU and renderer.
    pub fn draw_extra(
        &mut self,
        index: usize,
        target: &dawkey_render::wgpu::TextureView,
        format: dawkey_render::wgpu::TextureFormat,
        size: (u32, u32),
    ) {
        let Some(scene) = self.extra_scene(index) else {
            return;
        };
        let scale = self
            .extra_windows
            .get(index)
            .map_or(1.0, |w| w.window.scale);
        let Some((gpu, renderer)) = &mut self.graphics else {
            tracing::warn!("draw_extra called before set_gpu");
            return;
        };
        renderer.render(
            gpu,
            &mut self.text,
            &scene,
            scale,
            target,
            format,
            size,
            Color::TRANSPARENT,
        );
    }
}

/// Which view (or combination of views) a window or pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewRole {
    Arrange,
    Rack,
    Mixer,
    /// Mixer and Rack stacked in one window (the 2-monitor case).
    MixerAndRack,
}

/// A monitor's name and logical horizontal position, as reported by
/// `xdg-output` (left edge, in logical pixels).
#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub name: String,
    pub x: i32,
}

/// The fixed three-pane layout used by the widescreen preference: Arrange,
/// Rack and Mixer side by side in one window, left to right.
pub const WIDESCREEN_ROLES: [ViewRole; 3] = [ViewRole::Arrange, ViewRole::Rack, ViewRole::Mixer];

/// Assigns a [`ViewRole`] to each monitor that should get its own window.
///
/// Monitors are considered left to right by logical `x`. One monitor means
/// multi-monitor mode has nothing to do (the caller should stay in
/// single-window mode); two monitors get Arrange and a combined Mixer+Rack;
/// three or more use the three leftmost for Arrange, Rack and Mixer and
/// ignore the rest.
///
/// Returns `(role, monitor_index)` pairs, `monitor_index` indexing into
/// `monitors` as given (not the sorted order).
pub fn plan_windows(monitors: &[MonitorInfo]) -> Vec<(ViewRole, usize)> {
    if monitors.len() < 2 {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..monitors.len()).collect();
    order.sort_by_key(|&i| monitors[i].x);

    if monitors.len() == 2 {
        vec![
            (ViewRole::Arrange, order[0]),
            (ViewRole::MixerAndRack, order[1]),
        ]
    } else {
        vec![
            (ViewRole::Arrange, order[0]),
            (ViewRole::Rack, order[1]),
            (ViewRole::Mixer, order[2]),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(name: &str, x: i32) -> MonitorInfo {
        MonitorInfo {
            name: name.to_string(),
            x,
        }
    }

    #[test]
    fn one_monitor_stays_single_window() {
        assert_eq!(plan_windows(&[monitor("eDP-1", 0)]), Vec::new());
    }

    #[test]
    fn two_monitors_get_arrange_and_combined_mixer_rack() {
        let monitors = [monitor("left", 0), monitor("right", 1920)];
        assert_eq!(
            plan_windows(&monitors),
            vec![(ViewRole::Arrange, 0), (ViewRole::MixerAndRack, 1)]
        );
    }

    #[test]
    fn two_monitors_sorts_by_logical_position_regardless_of_input_order() {
        let monitors = [monitor("right", 1920), monitor("left", 0)];
        assert_eq!(
            plan_windows(&monitors),
            vec![(ViewRole::Arrange, 1), (ViewRole::MixerAndRack, 0)]
        );
    }

    #[test]
    fn three_monitors_get_arrange_rack_mixer_left_to_right() {
        let monitors = [
            monitor("mid", 1920),
            monitor("left", 0),
            monitor("right", 3840),
        ];
        assert_eq!(
            plan_windows(&monitors),
            vec![
                (ViewRole::Arrange, 1),
                (ViewRole::Rack, 0),
                (ViewRole::Mixer, 2)
            ]
        );
    }

    #[test]
    fn four_or_more_monitors_use_only_the_three_leftmost() {
        let monitors = [
            monitor("a", 0),
            monitor("b", 1920),
            monitor("c", 3840),
            monitor("d", 5760),
        ];
        assert_eq!(
            plan_windows(&monitors),
            vec![
                (ViewRole::Arrange, 0),
                (ViewRole::Rack, 1),
                (ViewRole::Mixer, 2)
            ]
        );
    }
}
