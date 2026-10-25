// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `batch` tool: several commands as one call and one undo step, where
//! later ones can use what earlier ones made (`"$0.track"`).

use dawkey_core::{Command, CommandSpec, CoreError, Outcome, Project};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// Apply several changes in order as one call and one undo step: the same
/// tools as the individual commands (add_track, add_plugin, set_track_gain,
/// ...; not get_state, imports, renders, transport or UI tools), with the
/// same arguments. All or nothing: if one fails, nothing is applied, and the
/// error says which one and why. An argument can be the result of an earlier
/// operation in the batch: `"$0.track"` is the `track` the first operation
/// returned, `"$2.clip"` the third's `clip`, `"$1.notes[0]"` the second's
/// first note. Returns each operation's result under `outcomes`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Batch {
    pub(crate) operations: Vec<Operation>,
    /// The undo step's name. Defaults to "batch".
    #[serde(default)]
    pub(crate) label: Option<String>,
}

/// One tool call in a batch.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Operation {
    /// The tool's name, e.g. "add_plugin".
    pub(crate) tool: String,
    /// Its arguments, as for the tool itself; positions may be in beats or
    /// bars too.
    #[serde(default)]
    pub(crate) args: Value,
}

/// An operation ready to run: its command spec, and the stems of its
/// positions in seconds (for beats and bars).
pub(crate) struct Prepared {
    pub(crate) spec: &'static CommandSpec,
    pub(crate) args: Value,
    pub(crate) stems: Vec<String>,
}

impl Prepared {
    /// The command for a step, from the project as the earlier steps left
    /// it and their outcomes. (The session says which step an error is in.)
    pub(crate) fn command(
        &self,
        project: &Project,
        outcomes: &[Outcome],
    ) -> Result<Command, CoreError> {
        let fail = |message: String| CoreError::InvalidArgument(message).in_context(self.spec.name);
        let mut args = self.args.clone();
        let outcomes: Vec<Value> = outcomes
            .iter()
            .map(|o| serde_json::to_value(o).expect("outcomes serialize"))
            .collect();
        substitute(&mut args, &outcomes).map_err(fail)?;
        crate::times::resolve(project, &mut args, &self.stems).map_err(fail)?;
        (self.spec.parse)(args).map_err(|e| fail(format!("invalid arguments: {e}")))
    }
}

/// Replaces every `"$N.path"` string in `args` with that part of operation
/// `N`'s outcome.
fn substitute(args: &mut Value, outcomes: &[Value]) -> Result<(), String> {
    match args {
        Value::String(s) if s.starts_with('$') => {
            *args = lookup(s, outcomes)?;
        }
        Value::Array(items) => {
            for item in items {
                substitute(item, outcomes)?;
            }
        }
        Value::Object(fields) => {
            for value in fields.values_mut() {
                substitute(value, outcomes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// `$1.notes[0]` → outcome 1's first note.
fn lookup(reference: &str, outcomes: &[Value]) -> Result<Value, String> {
    let body = &reference[1..];
    let (index, path) = body
        .split_once('.')
        .ok_or_else(|| format!("{reference}: a reference is \"$N.field\", e.g. \"$0.track\""))?;
    let index: usize = index
        .parse()
        .map_err(|_| format!("{reference}: {index:?} isn't an operation number"))?;
    let outcome = outcomes.get(index).ok_or_else(|| {
        format!(
            "{reference}: operation {index} hasn't run yet (only earlier operations can be \
             referred to)"
        )
    })?;
    let mut value = outcome;
    for segment in path.split('.') {
        let (key, item) = match segment.split_once('[') {
            Some((key, rest)) => {
                let item = rest
                    .strip_suffix(']')
                    .and_then(|i| i.parse::<usize>().ok())
                    .ok_or_else(|| format!("{reference}: bad index in {segment:?}"))?;
                (key, Some(item))
            }
            None => (segment, None),
        };
        value = value
            .get(key)
            .ok_or_else(|| format!("{reference}: operation {index} returned no {key:?}"))?;
        if let Some(item) = item {
            value = value.get(item).ok_or_else(|| {
                format!("{reference}: operation {index} returned no {key}[{item}]")
            })?;
        }
    }
    Ok(value.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn substitutes_references() {
        let outcomes = vec![json!({ "track": 3, "notes": [7, 8] }), json!({ "clip": 4 })];
        let mut args =
            json!({ "track": "$0.track", "clips": ["$1.clip", 9], "note": "$0.notes[1]" });
        substitute(&mut args, &outcomes).unwrap();
        assert_eq!(args, json!({ "track": 3, "clips": [4, 9], "note": 8 }));

        for (reference, expected) in [
            ("$2.track", "hasn't run yet"),
            ("$0.clip", "returned no \"clip\""),
            ("$0.notes[5]", "returned no notes[5]"),
            ("$x.track", "isn't an operation number"),
            ("$0", "\"$N.field\""),
        ] {
            let err = lookup(reference, &outcomes).unwrap_err();
            assert!(err.contains(expected), "{reference}: {err}");
        }
    }
}
