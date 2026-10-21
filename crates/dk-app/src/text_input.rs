// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typing values: clicking a field (the tempo lane's BPM, the time
//! signature's numerator or denominator; double-clicking a MIDI output's
//! latency slider or a plugin parameter's slider) starts editing it with its
//! value selected. Digits (and a point, where values have fractions) type;
//! a plugin parameter takes any text, which the plugin reads the way it
//! shows its values ("-6 dB", a choice's name). Backspace deletes, Return or
//! Tab (or clicking elsewhere) sets it with one command, and Escape leaves
//! it as it was. Tab from a numerator goes on to the denominator. While a
//! field is being typed in, it gets every key.

use dk_core::{
    DEVICE_LATENCY_MS_MAX, SetPluginParam, SetTempo, SetTimeSignature, SetTrackMidiOutput,
    TEMPO_BPM_MAX, TEMPO_BPM_MIN,
};
use dk_ui::Modifiers;

use crate::App;
use crate::state::{TextEdit, TextField};
use crate::view::format_bpm;

/// The widget ID of a field.
pub(crate) fn field_id(field: &TextField) -> String {
    match field {
        TextField::Tempo(_) => "tempo/value".into(),
        TextField::MeterNumerator(_) => "meter/numerator/value".into(),
        TextField::MeterDenominator(_) => "meter/denominator/value".into(),
        TextField::DeviceLatency(track) => format!("track/{track}/midi_out_latency"),
        TextField::PluginParam { plugin, param } => format!("plugin/{plugin}/param/{param}"),
        TextField::RecordOffset => "prefs/record_offset".into(),
        TextField::McpPort => "prefs/mcp_port".into(),
    }
}

/// The longest text a field takes.
fn max_len(field: &TextField) -> usize {
    match field {
        TextField::MeterNumerator(_) | TextField::MeterDenominator(_) => 2,
        TextField::McpPort => 5,
        TextField::Tempo(_) | TextField::DeviceLatency(_) | TextField::RecordOffset => 10,
        TextField::PluginParam { .. } => 40,
    }
}

/// Whether the field's values have fractions.
fn fractional(field: &TextField) -> bool {
    matches!(
        field,
        TextField::Tempo(_) | TextField::DeviceLatency(_) | TextField::RecordOffset
    )
}

/// A latency in ms as a field shows it.
pub(crate) fn format_ms(ms: f64) -> String {
    format_bpm(ms)
}

impl App {
    /// Starts typing in `field`, with its current value selected.
    pub(crate) fn start_text_edit(&mut self, field: TextField) -> Result<(), String> {
        let text = match &field {
            &TextField::Tempo(n) => {
                let project = self.studio.session().project();
                let bpm = match n {
                    0 => project.tempo_bpm,
                    n => {
                        project
                            .tempo_changes
                            .get(n - 1)
                            .ok_or_else(|| format!("there is no tempo point {n}"))?
                            .bpm
                    }
                };
                format_bpm(bpm)
            }
            &TextField::MeterNumerator(n) | &TextField::MeterDenominator(n) => {
                let (_, sig) = *self
                    .meter_points()
                    .get(n)
                    .ok_or_else(|| format!("there is no time signature {n}"))?;
                match field {
                    TextField::MeterNumerator(_) => sig.numerator,
                    _ => sig.denominator,
                }
                .to_string()
            }
            &TextField::DeviceLatency(track) => {
                let output = self
                    .studio
                    .session()
                    .project()
                    .track(track)
                    .and_then(|t| t.midi_output.as_ref())
                    .ok_or_else(|| format!("track {track} has no MIDI output"))?;
                format_ms(output.latency_ms)
            }
            TextField::RecordOffset => format_ms(self.prefs.record_offset_ms),
            TextField::McpPort => self.prefs.mcp_http_port.to_string(),
            TextField::PluginParam { plugin, param } => {
                self.studio
                    .plugin_param(*plugin, param)
                    .ok_or_else(|| format!("plugin {plugin} has no parameter {param:?}"))?
                    .text
            }
        };
        self.view.text_edit = Some(TextEdit {
            field,
            text,
            selected: true,
        });
        Ok(())
    }

    /// A key for the field being typed in; `None` if no field is.
    pub(crate) fn on_text_key(&mut self, key: &str, m: Modifiers) -> Option<Result<(), String>> {
        let edit = self.view.text_edit.as_mut()?;
        match key {
            "tab" => {
                let field = edit.field.clone();
                let result = self.commit_text_edit();
                // On from the numerator to the denominator.
                if let (Ok(()), TextField::MeterNumerator(n)) = (&result, field) {
                    return Some(self.start_text_edit(TextField::MeterDenominator(n)));
                }
                return Some(result);
            }
            "return" => return Some(self.commit_text_edit()),
            "escape" => self.view.text_edit = None,
            "backspace" | "delete" => {
                if std::mem::take(&mut edit.selected) {
                    edit.text.clear();
                } else {
                    edit.text.pop();
                }
            }
            _ if matches!(edit.field, TextField::PluginParam { .. }) => {
                // Any text: the plugin reads it.
                let c = match key {
                    "space" => ' ',
                    _ => match key.chars().collect::<Vec<_>>()[..] {
                        [c] if !c.is_control() => c,
                        _ => return Some(Ok(())),
                    },
                };
                let c = if m.shift { c.to_ascii_uppercase() } else { c };
                if std::mem::take(&mut edit.selected) {
                    edit.text.clear();
                }
                if edit.text.chars().count() < max_len(&edit.field) {
                    edit.text.push(c);
                }
            }
            _ => {
                let c = match key {
                    "," => '.',
                    _ => match key.chars().collect::<Vec<_>>()[..] {
                        [c] => c,
                        _ => return Some(Ok(())),
                    },
                };
                let point = c == '.'
                    && fractional(&edit.field)
                    && (edit.selected || !edit.text.contains('.'));
                // A minus sign first, where values can be negative.
                let minus = c == '-'
                    && edit.field == TextField::RecordOffset
                    && (edit.selected || edit.text.is_empty());
                if c.is_ascii_digit() || point || minus {
                    if std::mem::take(&mut edit.selected) {
                        edit.text.clear();
                    }
                    if edit.text.len() < max_len(&edit.field) {
                        edit.text.push(c);
                    }
                }
            }
        }
        Some(Ok(()))
    }

    /// Before a click or drag on widget `id`: anywhere but the field being
    /// typed in sets it.
    pub(crate) fn leave_text_edit(&mut self, id: &str) -> Result<(), String> {
        match &self.view.text_edit {
            Some(edit) if field_id(&edit.field) != id => self.commit_text_edit(),
            _ => Ok(()),
        }
    }

    /// Sets the field to what was typed, and stops editing. Text that
    /// isn't a value in range is an error, and changes nothing.
    pub(crate) fn commit_text_edit(&mut self) -> Result<(), String> {
        let Some(TextEdit {
            field,
            text,
            selected,
        }) = self.view.text_edit.take()
        else {
            return Ok(());
        };
        match field {
            TextField::RecordOffset => {
                let ms: f64 = text
                    .parse()
                    .ok()
                    .filter(|ms| (-1000.0..=1000.0).contains(ms))
                    .ok_or_else(|| format!("type an offset from -1000 to 1000 ms, not {text:?}"))?;
                self.set_record_offset_pref(ms);
                Ok(())
            }
            TextField::McpPort => {
                let port: u16 = text
                    .parse()
                    .ok()
                    .filter(|&p| p >= 1024)
                    .ok_or_else(|| format!("type a port from 1024 to 65535, not {text:?}"))?;
                self.set_mcp_port_pref(port);
                Ok(())
            }
            TextField::PluginParam { plugin, param } => {
                // Untouched, or the same text: nothing changes.
                let shown = self.studio.plugin_param(plugin, &param).map(|p| p.text);
                if selected || shown.as_deref() == Some(text.trim()) {
                    return Ok(());
                }
                self.execute(
                    SetPluginParam {
                        plugin,
                        param,
                        value: None,
                        text: Some(text),
                    }
                    .into(),
                )
            }
            TextField::Tempo(n) => {
                let bpm: f64 = text
                    .parse()
                    .ok()
                    .filter(|b| (TEMPO_BPM_MIN..=TEMPO_BPM_MAX).contains(b))
                    .ok_or_else(|| {
                        format!(
                            "type a tempo from {TEMPO_BPM_MIN} to {TEMPO_BPM_MAX} BPM, not {text:?}"
                        )
                    })?;
                let project = self.studio.session().project();
                let (beat, current, ramp) = match n {
                    0 => (0.0, project.tempo_bpm, false),
                    n => {
                        let Some(c) = project.tempo_changes.get(n - 1) else {
                            return Ok(());
                        };
                        (c.beat, c.bpm, c.ramp)
                    }
                };
                if bpm == current {
                    return Ok(());
                }
                self.execute(
                    SetTempo {
                        bpm,
                        at_beats: (n > 0).then_some(beat),
                        ramp,
                    }
                    .into(),
                )
            }
            TextField::MeterNumerator(n) | TextField::MeterDenominator(n) => {
                let numerator = matches!(field, TextField::MeterNumerator(_));
                let value: u8 = text
                    .parse()
                    .ok()
                    .filter(|&v| match numerator {
                        true => (1..=32).contains(&v),
                        false => u8::is_power_of_two(v) && v <= 32,
                    })
                    .ok_or_else(|| match numerator {
                        true => format!("type from 1 to 32 beats per bar, not {text:?}"),
                        false => {
                            format!("type a beat length of 1, 2, 4, 8, 16 or 32, not {text:?}")
                        }
                    })?;
                let Some(&(bar, sig)) = self.meter_points().get(n) else {
                    return Ok(());
                };
                let (numerator, denominator) = match numerator {
                    true => (value, sig.denominator),
                    false => (sig.numerator, value),
                };
                if (numerator, denominator) == (sig.numerator, sig.denominator) {
                    return Ok(());
                }
                self.execute(
                    SetTimeSignature {
                        numerator,
                        denominator,
                        at_bar: (n > 0).then_some(bar + 1),
                    }
                    .into(),
                )
            }
            TextField::DeviceLatency(track) => {
                let ms: f64 = text
                    .parse()
                    .ok()
                    .filter(|ms| (0.0..=DEVICE_LATENCY_MS_MAX).contains(ms))
                    .ok_or_else(|| {
                        format!("type a latency from 0 to {DEVICE_LATENCY_MS_MAX} ms, not {text:?}")
                    })?;
                let project = self.studio.session().project();
                let Some(output) = project.track(track).and_then(|t| t.midi_output.clone()) else {
                    return Ok(());
                };
                if ms == output.latency_ms {
                    return Ok(());
                }
                self.execute(
                    SetTrackMidiOutput {
                        track,
                        device: Some(output.device),
                        channel: Some(output.channel),
                        latency_ms: Some(ms),
                    }
                    .into(),
                )
            }
        }
    }
}
