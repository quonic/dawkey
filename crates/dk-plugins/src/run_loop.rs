// SPDX-License-Identifier: AGPL-3.0-or-later
//! Timers a plugin registers to be run on its main thread (its GUI's event
//! loop, on Linux): CLAP's `timer-support`, VST3's `IRunLoop`.

use std::time::{Duration, Instant};

/// A timer: what to call, how often, and when next.
#[derive(Debug, Clone)]
struct Timer<K> {
    key: K,
    period: Duration,
    due: Instant,
}

/// A plugin's timers, by key (an ID, or the handler to call).
#[derive(Debug)]
pub(crate) struct Timers<K> {
    timers: Vec<Timer<K>>,
}

impl<K> Default for Timers<K> {
    fn default() -> Self {
        Self { timers: Vec::new() }
    }
}

impl<K: Clone> Timers<K> {
    /// A timer every `period_ms`, first due a period from now.
    pub(crate) fn add(&mut self, key: K, period_ms: u64) {
        // At least 30 Hz is fine; faster than 200 Hz isn't needed.
        let period = Duration::from_millis(period_ms.max(5));
        self.timers.push(Timer {
            key,
            period,
            due: Instant::now() + period,
        });
    }

    /// Removes the timers `remove` picks.
    pub(crate) fn remove(&mut self, mut remove: impl FnMut(&K) -> bool) {
        self.timers.retain(|t| !remove(&t.key));
    }

    /// The timers due at `now`, each moved on a period.
    pub(crate) fn due(&mut self, now: Instant) -> Vec<K> {
        let mut due = Vec::new();
        for timer in self.timers.iter_mut().filter(|t| t.due <= now) {
            due.push(timer.key.clone());
            timer.due = now + timer.period;
        }
        due
    }

    /// When the next one is due.
    pub(crate) fn next(&self) -> Option<Instant> {
        self.timers.iter().map(|t| t.due).min()
    }

    pub(crate) fn clear(&mut self) {
        self.timers.clear();
    }
}
