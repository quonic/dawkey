// SPDX-License-Identifier: AGPL-3.0-or-later
//! Timeline positions in beats and bars. Every tool argument that's a
//! timeline position in seconds (`position_s`, `start_s`, `end_s`, `at_s`,
//! ...) can be given as `*_beats` (quarter notes from the project's start)
//! or `*_bar` (a bar number as the ruler shows it) instead, and a clip
//! length as `length_beats`. They're resolved through the project's tempo
//! and meter maps before the tool sees them. [`with_beats`] adds beats to
//! the state tools' results the same way.

use dawkey_core::{MeterMap, Project, TempoMap};
use serde_json::{Map, Value, json};

/// Arguments in seconds that aren't timeline positions, so beats and bars
/// make no sense for them.
const NOT_POSITIONS: &[&str] = &["offset_s", "tail_s"];

/// Adds the `*_beats` and `*_bar` alternatives of every timeline position
/// (`*_s`) among a tool's top-level arguments to its schema.
pub(crate) fn augment_schema(schema: &mut Map<String, Value>) {
    let Some(Value::Object(properties)) = schema.get("properties") else {
        return;
    };
    let seconds: Vec<(String, String)> = properties
        .iter()
        .filter_map(|(name, prop)| {
            let stem = name.strip_suffix("_s")?;
            if NOT_POSITIONS.contains(&name.as_str()) {
                return None;
            }
            let description = prop
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            Some((stem.to_owned(), description))
        })
        .collect();
    if seconds.is_empty() {
        return;
    }
    let Some(Value::Object(properties)) = schema.get_mut("properties") else {
        return;
    };
    for (stem, description) in seconds {
        let name_s = format!("{stem}_s");
        if stem.contains("length") {
            properties.insert(
                format!("{stem}_beats"),
                json!({
                    "type": ["number", "null"],
                    "description": format!(
                        "Instead of {name_s}: the length in beats (quarter notes), at the \
                         tempo where it starts."
                    ),
                }),
            );
            continue;
        }
        properties.insert(
            format!("{stem}_beats"),
            json!({
                "type": ["number", "null"],
                "description": format!(
                    "Instead of {name_s}: the position in beats (quarter notes) from the \
                     project's start, through the tempo map. {description}"
                ),
            }),
        );
        properties.insert(
            format!("{stem}_bar"),
            json!({
                "type": ["number", "null"],
                "description": format!(
                    "Instead of {name_s}: the position as a bar number as the ruler shows \
                     it, from 1; a fraction is a part of that bar (2.5 is halfway through \
                     bar 2), and 33 is where 32 bars from the start end. {description}"
                ),
            }),
        );
    }
}

/// The stems of a tool's timeline positions in seconds (`start` for
/// `start_s`), which it also takes in beats and bars.
pub(crate) fn position_stems(schema: &Value) -> Vec<String> {
    let Some(properties) = schema["properties"].as_object() else {
        return Vec::new();
    };
    properties
        .keys()
        .filter(|name| !NOT_POSITIONS.contains(&name.as_str()))
        .filter_map(|name| name.strip_suffix("_s").map(str::to_owned))
        .collect()
}

/// Whether `args` give any of these positions in beats or bars.
pub(crate) fn has_beats(args: &Value, stems: &[String]) -> bool {
    args.as_object().is_some_and(|args| {
        args.keys()
            .any(|k| stem_of(k).is_some_and(|stem| stems.contains(&stem.to_owned())))
    })
}

/// `start` for `start_beats` or `start_bar`.
fn stem_of(key: &str) -> Option<&str> {
    key.strip_suffix("_beats")
        .or_else(|| key.strip_suffix("_bar"))
}

/// The project's tempo and meter maps, for resolving positions.
#[derive(Debug, Clone)]
pub(crate) struct Maps {
    pub(crate) tempo: TempoMap,
    pub(crate) meter: MeterMap,
}

impl Maps {
    pub(crate) fn of(project: &Project) -> Self {
        Self {
            tempo: project.tempo_map(),
            meter: project.meter_map(),
        }
    }

    /// Seconds from the project's start to `beat`.
    fn seconds_at_beat(&self, beat: f64) -> f64 {
        self.tempo.seconds_at(beat)
    }

    /// The beat where `bar` (as the ruler shows it, from 1; fractional)
    /// is.
    fn beat_of_bar(&self, bar: f64) -> f64 {
        let whole = bar.floor();
        let index = (whole - 1.0) as u32;
        let start = self.meter.bar_start(index);
        let length = self.meter.meter_of_bar(index).beats_per_bar();
        start + (bar - whole) * length
    }

    /// Replaces every `*_beats` and `*_bar` in `args` whose stem is one of
    /// `stems` with its `*_s`.
    pub(crate) fn resolve(&self, args: &mut Value, stems: &[String]) -> Result<(), String> {
        let Value::Object(args) = args else {
            return Ok(());
        };
        let keys: Vec<String> = args
            .keys()
            .filter(|k| stem_of(k).is_some_and(|stem| stems.contains(&stem.to_owned())))
            .cloned()
            .collect();
        // Positions first: lengths in beats depend on where they start.
        let (lengths, positions): (Vec<_>, Vec<_>) =
            keys.into_iter().partition(|k| k.contains("length"));
        for key in positions {
            let (stem, value) = take_number(args, &key)?;
            let Some(value) = value else { continue };
            let seconds = if key.ends_with("_beats") {
                if value < 0.0 {
                    return Err(format!("{key} must be at least 0, got {value}"));
                }
                self.seconds_at_beat(value)
            } else {
                if value.is_nan() || value < 1.0 {
                    return Err(format!(
                        "{key} must be at least 1 (bars are numbered as the ruler shows \
                         them), got {value}"
                    ));
                }
                self.seconds_at_beat(self.beat_of_bar(value))
            };
            args.insert(format!("{stem}_s"), json!(seconds));
        }
        for key in lengths {
            let (stem, value) = take_number(args, &key)?;
            let Some(value) = value else { continue };
            if key.ends_with("_bar") {
                return Err(format!("{key}: give a length as {stem}_beats or {stem}_s"));
            }
            if value.is_nan() || value <= 0.0 {
                return Err(format!("{key} must be more than 0, got {value}"));
            }
            let from_s = args
                .get("position_s")
                .or_else(|| args.get("start_s"))
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let from_beat = self.tempo.beat_at_seconds(from_s);
            let seconds = self.seconds_at_beat(from_beat + value) - from_s;
            args.insert(format!("{stem}_s"), json!(seconds));
        }
        Ok(())
    }
}

/// How a render's range is cut into segments to analyze separately.
#[derive(Debug, Clone, Default)]
pub(crate) struct SegmentSpec {
    /// Segments this long in seconds, from the range's start.
    pub(crate) length_s: Option<f64>,
    /// Segments of this many bars, cut at bar lines.
    pub(crate) bars: Option<u32>,
    /// Explicit ranges, in absolute seconds.
    pub(crate) ranges: Option<Vec<(f64, f64)>>,
}

impl Maps {
    /// The segments of `[start_s, end_s)` that `spec` asks for, as absolute
    /// seconds, clipped to the range. Empty if none were asked for.
    pub(crate) fn segments(
        &self,
        start_s: f64,
        end_s: f64,
        spec: &SegmentSpec,
    ) -> Result<Vec<(f64, f64)>, String> {
        let given = [
            spec.length_s.is_some(),
            spec.bars.is_some(),
            spec.ranges.is_some(),
        ]
        .iter()
        .filter(|&&g| g)
        .count();
        if given > 1 {
            return Err(
                "give one of segment_length_s (or segment_length_beats), segment_bars and \
                 segments"
                    .to_owned(),
            );
        }
        let mut segments = Vec::new();
        if let Some(length) = spec.length_s {
            if length.is_nan() || length <= 0.0 || !length.is_finite() {
                return Err(format!(
                    "segment_length_s must be more than 0, got {length}"
                ));
            }
            let mut from = start_s;
            while from < end_s - 1e-9 {
                let to = (from + length).min(end_s);
                segments.push((from, to));
                from = to;
            }
        } else if let Some(bars) = spec.bars {
            if bars == 0 {
                return Err("segment_bars must be at least 1".to_owned());
            }
            let mut from = start_s;
            let mut bar = self
                .meter
                .bar_at(self.tempo.beat_at_seconds(start_s) + 1e-9);
            while from < end_s - 1e-9 {
                bar += bars;
                let to = self.tempo.seconds_at(self.meter.bar_start(bar)).min(end_s);
                if to <= from {
                    break;
                }
                segments.push((from, to));
                from = to;
            }
        } else if let Some(ranges) = &spec.ranges {
            for &(from, to) in ranges {
                if from.is_nan() || to.is_nan() || from >= to {
                    return Err(format!(
                        "a segment's end_s must be after its start_s, got {from}..{to}"
                    ));
                }
                let (from, to) = (from.max(start_s), to.min(end_s));
                if from >= to {
                    return Err(format!(
                        "segment {from}..{to} is outside the range {start_s}..{end_s}"
                    ));
                }
                segments.push((from, to));
            }
        }
        if segments.len() > 10_000 {
            return Err(format!("that's {} segments; at most 10000", segments.len()));
        }
        Ok(segments)
    }
}

/// Removes `key` from `args`, checking it's a number and that its `*_s`
/// isn't given as well. Returns the stem and the value (`None` for null).
fn take_number(args: &mut Map<String, Value>, key: &str) -> Result<(String, Option<f64>), String> {
    let stem = key
        .rsplit_once('_')
        .map(|(stem, _)| stem.to_owned())
        .unwrap_or_default();
    let value = args.remove(key).unwrap_or(Value::Null);
    let value = match value {
        Value::Null => None,
        Value::Number(n) => n.as_f64(),
        other => {
            return Err(format!(
                "invalid arguments: {key} must be a number, got {other}"
            ));
        }
    };
    if value.is_some() && args.get(&format!("{stem}_s")).is_some_and(|v| !v.is_null()) {
        return Err(format!(
            "invalid arguments: give {stem}_s or {key}, not both"
        ));
    }
    Ok((stem, value))
}

/// Adds beats to a project state: clips get `start_beats`, `end_beats` and
/// `start_bar` (as the ruler numbers bars; fractional), notes `start_beats`
/// and `length_beats` and control points `at_beats` (from their clip's
/// start), and the loop and punch ranges `start_beats` and `end_beats`.
pub(crate) fn with_beats(project: &Project, state: &mut Value) {
    let maps = Maps::of(project);
    let Some(tracks) = state["project"]["tracks"].as_array_mut() else {
        return;
    };
    let beat_at = |frame: u64| maps.tempo.beat_at(frame as f64);
    for track in tracks {
        let Some(clips) = track["clips"].as_array_mut() else {
            continue;
        };
        for clip in clips {
            let (Some(start), Some(length)) = (clip["start"].as_u64(), clip["length"].as_u64())
            else {
                continue;
            };
            let start_beat = beat_at(start);
            clip["start_beats"] = json!(start_beat);
            clip["end_beats"] = json!(beat_at(start + length));
            clip["start_bar"] = json!(bar_of_beat(&maps.meter, start_beat));
            if let Some(notes) = clip["notes"].as_array_mut() {
                for note in notes {
                    let (Some(at), Some(len)) = (note["start"].as_u64(), note["length"].as_u64())
                    else {
                        continue;
                    };
                    let from = beat_at(start + at);
                    note["start_beats"] = json!(from - start_beat);
                    note["length_beats"] = json!(beat_at(start + at + len) - from);
                }
            }
            if let Some(controls) = clip["controls"].as_array_mut() {
                for point in controls {
                    if let Some(at) = point["frame"].as_u64() {
                        point["at_beats"] = json!(beat_at(start + at) - start_beat);
                    }
                }
            }
        }
    }
    for region in ["loop", "punch"] {
        let range = &mut state["project"][region]["range"];
        if let (Some(start), Some(end)) = (range["start"].as_u64(), range["end"].as_u64()) {
            range["start_beats"] = json!(beat_at(start));
            range["end_beats"] = json!(beat_at(end));
        }
    }
}

/// The bar `beat` is in, numbered as the ruler shows it (from 1), with how
/// far into it as a fraction.
fn bar_of_beat(meter: &MeterMap, beat: f64) -> f64 {
    let index = meter.bar_at(beat);
    let start = meter.bar_start(index);
    let length = meter.meter_of_bar(index).beats_per_bar();
    f64::from(index) + 1.0 + (beat - start) / length
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_beats_and_bars() {
        let mut project = Project::new("t");
        project.tempo_bpm = 120.0; // a beat is 0.5 s, a 4/4 bar 2 s
        let maps = Maps::of(&project);
        let stems: Vec<String> = ["start", "end", "length", "position"]
            .map(str::to_owned)
            .into();
        let mut args = json!({ "start_beats": 4, "end_bar": 3.5, "length_beats": 2 });
        maps.resolve(&mut args, &stems).unwrap();
        assert_eq!(
            args,
            json!({ "start_s": 2.0, "end_s": 5.0, "length_s": 1.0 })
        );

        let mut args = json!({ "position_s": 1.0, "position_beats": 2 });
        let err = maps.resolve(&mut args, &stems).unwrap_err();
        assert!(err.contains("not both"), "{err}");
        let mut args = json!({ "position_bar": 0 });
        let err = maps.resolve(&mut args, &stems).unwrap_err();
        assert!(err.contains("at least 1"), "{err}");
        let mut args = json!({ "position_s": 1.0, "position_beats": null });
        maps.resolve(&mut args, &stems).unwrap();
        assert_eq!(args, json!({ "position_s": 1.0 }));
        // A tool's own `*_beats` (set_tempo's at_beats) is left alone.
        let mut args = json!({ "at_beats": 2 });
        maps.resolve(&mut args, &stems).unwrap();
        assert_eq!(args, json!({ "at_beats": 2 }));
    }

    #[test]
    fn augments_schemas() {
        let schema = dawkey_core::args_schema::<dawkey_core::SetLoop>();
        let Value::Object(mut schema) = schema.to_value() else {
            panic!()
        };
        augment_schema(&mut schema);
        let props = schema["properties"].as_object().unwrap();
        for key in ["start_beats", "start_bar", "end_beats", "end_bar"] {
            assert!(props.contains_key(key), "{key}");
        }
        let stems = position_stems(&Value::Object(schema));
        assert_eq!(stems, ["end", "start"]);
        assert!(!has_beats(&json!({ "start_s": 1 }), &stems));
        assert!(has_beats(&json!({ "start_bar": 1 }), &stems));
    }
}
