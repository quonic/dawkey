// SPDX-License-Identifier: AGPL-3.0-or-later
//! Note expressions: what MIDI 2.0 per-note controllers are to plugins that
//! take expressions per note (CLAP, VST3). Each format maps [`NoteExpression`]
//! to its own IDs and value ranges; values here are CLAP's.

use dawkey_engine::{PerNote, PluginEventKind};

/// A note expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoteExpression {
    /// In semitones.
    Tuning,
    /// Linear gain: 1 is unity, up to 4 (+12 dB).
    Volume,
    /// 0 is left, 0.5 center, 1 right.
    Pan,
    /// The rest go from 0 to 1.
    Expression,
    Brightness,
    Vibrato,
    Pressure,
}

/// Every note expression at its default, for per-note management's reset
/// (neither CLAP nor VST3 has a reset of its own).
pub(crate) const DEFAULT_EXPRESSIONS: [(NoteExpression, f64); 7] = [
    (NoteExpression::Tuning, 0.0),
    (NoteExpression::Volume, 1.0),
    (NoteExpression::Pan, 0.5),
    (NoteExpression::Expression, 1.0),
    (NoteExpression::Brightness, 0.5),
    (NoteExpression::Vibrato, 0.0),
    (NoteExpression::Pressure, 0.0),
];

/// The note expression for a MIDI 2.0 per-note controller (32-bit value),
/// if there is one: per-note pitch bend is tuning (±48 semitones),
/// registered controllers 7, 10, 11, 74 and 1 are volume, pan, expression,
/// brightness and vibrato.
pub(crate) fn from_per_note(controller: PerNote, value: u32) -> Option<(NoteExpression, f64)> {
    let unit = f64::from(value) / f64::from(u32::MAX);
    let centered = (f64::from(value) - 2_147_483_648.0) / 2_147_483_648.0;
    Some(match controller {
        PerNote::PitchBend => (NoteExpression::Tuning, centered * 48.0),
        // Linear gain, unity at full (like the built-in synth).
        PerNote::Registered(7) => (NoteExpression::Volume, unit * unit),
        PerNote::Registered(10) => (NoteExpression::Pan, unit),
        PerNote::Registered(11) => (NoteExpression::Expression, unit),
        PerNote::Registered(74) => (NoteExpression::Brightness, unit),
        PerNote::Registered(1) => (NoteExpression::Vibrato, unit),
        _ => return None,
    })
}

/// What a note expression a plugin sends becomes: a MIDI 2.0 per-note
/// controller (the reverse of [`from_per_note`]), or poly pressure for
/// pressure.
pub(crate) fn to_per_note(
    expression: NoteExpression,
    channel: u8,
    key: u8,
    value: f64,
) -> PluginEventKind {
    let unit = |v: f64| (v.clamp(0.0, 1.0) * f64::from(u32::MAX)).round() as u32;
    let centered = |v: f64| (v.clamp(-1.0, 1.0) * 2_147_483_648.0 + 2_147_483_648.0).round();
    let per_note = |controller, value| PluginEventKind::PerNote {
        channel,
        key,
        controller,
        value,
    };
    match expression {
        NoteExpression::Tuning => per_note(
            PerNote::PitchBend,
            centered(value / 48.0).min(f64::from(u32::MAX)) as u32,
        ),
        NoteExpression::Volume => per_note(PerNote::Registered(7), unit(value.max(0.0).sqrt())),
        NoteExpression::Pan => per_note(PerNote::Registered(10), unit(value)),
        NoteExpression::Expression => per_note(PerNote::Registered(11), unit(value)),
        NoteExpression::Brightness => per_note(PerNote::Registered(74), unit(value)),
        NoteExpression::Vibrato => per_note(PerNote::Registered(1), unit(value)),
        NoteExpression::Pressure => PluginEventKind::Midi {
            data: [
                0xa0 | channel,
                key,
                (value.clamp(0.0, 1.0) * 127.0).round() as u8,
            ],
        },
    }
}
