// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pure layout policy behind the multi-monitor and widescreen preferences:
//! which view goes on which monitor (or, for widescreen, which pane of one
//! window). Kept free of any Wayland or rendering types so it can be unit
//! tested without a compositor.

use crate::App;

impl App {
    /// Applies a `layout` preference change. `Standard` (today's single
    /// window with a page switcher) and `Widescreen` (Arrange, Rack and
    /// Mixer side by side in that one window) take effect immediately.
    /// `MultiMonitor` is stored but not yet applied — opening a window per
    /// monitor is follow-up work on top of this preference.
    pub(crate) fn apply_layout(&mut self) {
        self.redraw = true;
        self.rebuild();
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
