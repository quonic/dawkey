// SPDX-License-Identifier: AGPL-3.0-or-later
//! Finding a plugin in the picker. While it's open, typing filters the list:
//! every word typed has to match the plugin's name, vendor, format or kind
//! (its letters in order, not necessarily together), and the best matches
//! come first. Up and Down (Home, End) move the highlight, Return adds the
//! highlighted plugin, Backspace deletes, and Escape clears what was typed,
//! then closes the picker.

use dk_studio::PluginInfo;
use dk_ui::Modifiers;

use crate::App;
use crate::view::plugins::{PICKER_TOP, ROW_H};

/// The longest search the picker takes.
const MAX_QUERY: usize = 60;

/// How well `term` (lowercase) matches `text`, or `None` if its letters
/// aren't all in `text` in that order. Letters together beat letters apart
/// (a substring always beats a scattered match), and matches at the start
/// of a word or of the text beat those inside one.
pub(crate) fn fuzzy_score(term: &str, text: &str) -> Option<i32> {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let term: Vec<char> = term.chars().collect();
    let word_start = |i: usize| i == 0 || !text[i - 1].is_alphanumeric();
    let n = term.len() as i32;
    if term.is_empty() {
        return Some(0);
    }
    if let Some(at) = text.windows(term.len()).position(|w| w == term) {
        let whole = text.len() == term.len();
        return Some(
            40 * n + 40 + if word_start(at) { 30 } else { 0 } + if whole { 30 } else { 0 }
                - (at as i32).min(20),
        );
    }
    // Scattered: the best way to place the letters. `best[j]` is the best
    // score with the letter so far at `j`. Each letter scores 10, 15 more
    // at a word's start and 15 more right after the one before; a gap
    // costs a point a letter, at most 10.
    const NONE: i32 = i32::MIN / 2;
    let letter = |j: usize| 10 + if word_start(j) { 15 } else { 0 };
    let mut best: Vec<i32> = (0..text.len())
        .map(|j| match text[j] == term[0] {
            true => letter(j) - (j as i32).min(20),
            false => NONE,
        })
        .collect();
    for &c in &term[1..] {
        let mut next = vec![NONE; text.len()];
        // The best of `best` at least 11 back, where a gap costs 10.
        let mut far = NONE;
        for j in 0..text.len() {
            if j >= 11 {
                far = far.max(best[j - 11]);
            }
            if text[j] != c {
                continue;
            }
            let near = (j.saturating_sub(10)..j).map(|k| match j - k - 1 {
                0 => best[k] + 15,
                gap => best[k] - gap as i32,
            });
            next[j] = near.fold(far - 10, i32::max) + letter(j);
        }
        best = next;
    }
    best.into_iter().max().filter(|&s| s > NONE / 2)
}

/// How well a plugin matches every one of `terms` (lowercase), or `None`
/// if one doesn't match. Its name counts double.
fn plugin_score(terms: &[String], p: &PluginInfo) -> Option<i32> {
    let format = p.id.split(':').next().unwrap_or("");
    let kind = if p.instrument { "instrument" } else { "effect" };
    let rest = format!("{} {format} {kind}", p.vendor);
    terms
        .iter()
        .map(|t| {
            fuzzy_score(t, &p.name)
                .map(|s| s * 2)
                .max(fuzzy_score(t, &rest))
        })
        .sum()
}

/// The plugins the picker shows: those of the kind picked (`instrument`)
/// that match every word of `query`, best first (in catalog order when
/// nothing's been typed).
pub(crate) fn picker_matches<'a>(
    plugins: &'a [PluginInfo],
    instrument: Option<bool>,
    query: &str,
) -> Vec<&'a PluginInfo> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut found: Vec<(i32, &PluginInfo)> = plugins
        .iter()
        .filter(|p| instrument.is_none_or(|i| p.instrument == i))
        .filter_map(|p| Some((plugin_score(&terms, p)?, p)))
        .collect();
    // Stable: equal scores keep the catalog's order.
    found.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
    found.into_iter().map(|(_, p)| p).collect()
}

impl App {
    /// The IDs of the plugins the open picker shows, in order.
    pub(crate) fn picker_ids(&self) -> Vec<String> {
        self.studio.cached_catalog().map_or_else(Vec::new, |c| {
            picker_matches(&c.plugins, self.view.picker_filter, &self.view.picker_query)
                .into_iter()
                .map(|p| p.id.clone())
                .collect()
        })
    }

    /// Keys while the picker is open: typing searches. `None` if it isn't
    /// open, or the key isn't one of its own.
    pub(crate) fn on_picker_key(&mut self, key: &str, m: Modifiers) -> Option<Result<(), String>> {
        self.view.plugin_picker?;
        if m.ctrl {
            return None;
        }
        let count = self.picker_ids().len();
        let view = &mut self.view;
        match key {
            "escape" if view.picker_query.is_empty() => self.close_overlays(),
            "escape" => self.set_picker_query(String::new()),
            "return" if count > 0 => return Some(self.add_picked(self.view.picker_selected)),
            "return" => {}
            "up" | "down" | "home" | "end" => {
                let last = count.saturating_sub(1);
                view.picker_selected = match key {
                    "up" => view.picker_selected.saturating_sub(1),
                    "down" => (view.picker_selected + 1).min(last),
                    "home" => 0,
                    _ => last,
                };
                self.scroll_to_picked();
            }
            "backspace" | "delete" => {
                let mut query = view.picker_query.clone();
                query.pop();
                self.set_picker_query(query);
            }
            _ => {
                let c = match key {
                    "space" => ' ',
                    _ => match key.chars().collect::<Vec<_>>()[..] {
                        [c] if !c.is_control() => c,
                        _ => return None,
                    },
                };
                let c = if m.shift { c.to_ascii_uppercase() } else { c };
                let mut query = view.picker_query.clone();
                if query.chars().count() < MAX_QUERY {
                    query.push(c);
                }
                self.set_picker_query(query);
            }
        }
        Some(Ok(()))
    }

    /// Searches for `query`, from the top of the list.
    fn set_picker_query(&mut self, query: String) {
        self.view.picker_query = query;
        self.view.picker_selected = 0;
        self.view.overlay_scroll = 0.0;
    }

    /// Scrolls the picker's list so the highlighted plugin shows.
    fn scroll_to_picked(&mut self) {
        let visible = self
            .ui
            .find("plugin_picker/list")
            .map_or(400.0, |n| n.rect().h);
        let top = PICKER_TOP + self.view.picker_selected as f32 * ROW_H;
        let scroll = &mut self.view.overlay_scroll;
        if top - PICKER_TOP < *scroll {
            *scroll = top - PICKER_TOP;
        } else if top + ROW_H + PICKER_TOP > *scroll + visible {
            *scroll = top + ROW_H + PICKER_TOP - visible;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substrings_beat_scattered_letters() {
        let reverb = fuzzy_score("verb", "Dragonfly Hall Reverb").unwrap();
        let scattered = fuzzy_score("verb", "Vintage Equalizer Rack Bus").unwrap();
        assert!(reverb > scattered, "{reverb} vs {scattered}");
        // At a word's start beats inside one.
        assert!(fuzzy_score("comp", "LSP Compressor") > fuzzy_score("comp", "Decompose"));
        // Word starts count when the letters are apart.
        assert!(fuzzy_score("gc", "Graphic Compressor") > fuzzy_score("gc", "Rage Cog"));
        // All the letters, in order.
        assert_eq!(fuzzy_score("vreb", "Reverb"), None);
        assert_eq!(fuzzy_score("x", "Reverb"), None);
        assert_eq!(fuzzy_score("", "Reverb"), Some(0));
        // Case doesn't matter; whole words of other scripts work too.
        assert!(fuzzy_score("über", "Überdrive").is_some());
    }
}
