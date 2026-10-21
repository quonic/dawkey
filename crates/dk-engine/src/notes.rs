// SPDX-License-Identifier: AGPL-3.0-or-later
//! Note bookkeeping for instrument plugins: which keys are down, and when
//! they should be released. It plays the role the synth's voices play for
//! the built-in synth, but emits note events instead of sound.

use crate::plugin::{PluginEvent, PluginEventKind};

/// Most notes held at once; a note beyond this releases the oldest.
const HELD: usize = 128;

#[derive(Debug, Clone, Copy, Default)]
struct Held {
    tag: u64,
    channel: u8,
    key: u8,
    /// When to release it (`u64::MAX` for never; 0 for at once).
    deadline: u64,
}

/// Held notes, oldest first. Real-time safe.
#[derive(Debug, Clone)]
pub(crate) struct NoteTracker {
    held: [Held; HELD],
    len: usize,
}

impl Default for NoteTracker {
    fn default() -> Self {
        Self {
            held: [Held::default(); HELD],
            len: 0,
        }
    }
}

/// Appends an event if there is room (the buffer never reallocates).
pub(crate) fn push_event(events: &mut Vec<PluginEvent>, offset: u32, kind: PluginEventKind) {
    if events.len() < events.capacity() {
        events.push(PluginEvent { offset, kind });
    }
}

impl NoteTracker {
    /// Starts a note, first releasing any held note with the same tag or
    /// key.
    #[expect(clippy::too_many_arguments, reason = "a note has this many parts")]
    pub(crate) fn note_on(
        &mut self,
        events: &mut Vec<PluginEvent>,
        at: u32,
        tag: u64,
        channel: u8,
        key: u8,
        velocity: f32,
        deadline: u64,
    ) {
        self.release(events, at, |h| {
            h.tag == tag || (h.channel == channel && h.key == key)
        });
        if self.len == HELD {
            self.release_index(events, at, 0);
        }
        self.held[self.len] = Held {
            tag,
            channel,
            key,
            deadline,
        };
        self.len += 1;
        push_event(
            events,
            at,
            PluginEventKind::NoteOn {
                channel,
                key,
                velocity,
            },
        );
    }

    pub(crate) fn note_off(&mut self, events: &mut Vec<PluginEvent>, at: u32, tag: u64) {
        self.release(events, at, |h| h.tag == tag);
    }

    /// Releases every note whose tag matches.
    pub(crate) fn release_where(
        &mut self,
        events: &mut Vec<PluginEvent>,
        at: u32,
        matches: impl Fn(u64) -> bool,
    ) {
        self.release(events, at, |h| matches(h.tag));
    }

    /// Releases notes whose deadline is at or before `now`.
    pub(crate) fn release_due(&mut self, events: &mut Vec<PluginEvent>, at: u32, now: u64) {
        self.release(events, at, |h| h.deadline <= now);
    }

    pub(crate) fn next_deadline(&self) -> Option<u64> {
        self.held[..self.len]
            .iter()
            .map(|h| h.deadline)
            .filter(|&d| d != u64::MAX)
            .min()
    }

    /// Updates deadlines after a graph swap: `f(tag, deadline)` gives the
    /// new deadline, or `None` to release at the next block.
    pub(crate) fn retarget(&mut self, mut f: impl FnMut(u64, u64) -> Option<u64>) {
        for h in &mut self.held[..self.len] {
            h.deadline = f(h.tag, h.deadline).unwrap_or(0);
        }
    }

    fn release(&mut self, events: &mut Vec<PluginEvent>, at: u32, matches: impl Fn(&Held) -> bool) {
        let mut i = 0;
        while i < self.len {
            if matches(&self.held[i]) {
                self.release_index(events, at, i);
            } else {
                i += 1;
            }
        }
    }

    fn release_index(&mut self, events: &mut Vec<PluginEvent>, at: u32, i: usize) {
        let h = self.held[i];
        push_event(
            events,
            at,
            PluginEventKind::NoteOff {
                channel: h.channel,
                key: h.key,
            },
        );
        self.held.copy_within(i + 1..self.len, i);
        self.len -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(events: &[PluginEvent]) -> Vec<(u32, PluginEventKind)> {
        events.iter().map(|e| (e.offset, e.kind)).collect()
    }

    #[test]
    fn tracks_notes_and_deadlines() {
        let mut t = NoteTracker::default();
        let mut events = Vec::with_capacity(16);
        t.note_on(&mut events, 0, 1, 0, 60, 1.0, 100);
        t.note_on(&mut events, 5, 2, 0, 64, 0.5, 50);
        assert_eq!(t.next_deadline(), Some(50));
        t.release_due(&mut events, 7, 50);
        // The same key again releases the held one first.
        t.note_on(&mut events, 9, 3, 0, 60, 1.0, u64::MAX);
        assert_eq!(
            kinds(&events),
            [
                (
                    0,
                    PluginEventKind::NoteOn {
                        channel: 0,
                        key: 60,
                        velocity: 1.0
                    }
                ),
                (
                    5,
                    PluginEventKind::NoteOn {
                        channel: 0,
                        key: 64,
                        velocity: 0.5
                    }
                ),
                (
                    7,
                    PluginEventKind::NoteOff {
                        channel: 0,
                        key: 64
                    }
                ),
                (
                    9,
                    PluginEventKind::NoteOff {
                        channel: 0,
                        key: 60
                    }
                ),
                (
                    9,
                    PluginEventKind::NoteOn {
                        channel: 0,
                        key: 60,
                        velocity: 1.0
                    }
                ),
            ]
        );
        assert_eq!(t.next_deadline(), None);
        // Retargeting to None releases at the next due check.
        t.retarget(|_, _| None);
        events.clear();
        t.release_due(&mut events, 0, 0);
        assert_eq!(
            kinds(&events),
            [(
                0,
                PluginEventKind::NoteOff {
                    channel: 0,
                    key: 60
                }
            )]
        );
    }
}
