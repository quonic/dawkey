// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dawkey's MCP server.
//!
//! Every [`dawkey_core::Command`] is published as a tool, with its input schema
//! generated from the command's argument type. Session, transport, import and
//! render tools are defined in [`tools`]. Each tool call that changes the
//! project is exactly one undo step.
//!
//! Transports: stdio ([`serve_stdio`]), the window's Unix socket
//! ([`serve_unix`], bridged to stdio by [`attach`]) and, opt-in, streamable
//! HTTP on a loopback port with a token ([`http`]).

#![forbid(unsafe_code)]

pub mod http;
mod state;
mod times;
mod tools;

use std::fmt::Display;
use std::sync::Arc;

use base64::Engine as _;
use dawkey_app::{App, DragFrom, ImportRequest, Platform, WindowInfo};
use dawkey_core::{AddClip, CommandSpec, CoreError, Handle, Project, Session};
use dawkey_studio::{LoadedProject, Studio};
use dawkey_ui::{Modifiers, Point, Size};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::tools::*;

const INSTRUCTIONS: &str = "\
Dawkey is a digital audio workstation. These tools edit and play the open
project. Call get_state first: it returns the project, including the
track, clip and source IDs that other tools take (`summary: true`, or
`notes: false`, leaves the notes out of a big project, and `tracks` picks
tracks; open_project and new_project return such a summary). Every tool call that changes
the project is one undo step, so `undo` reverts exactly one call. Changes are
not written to disk until save_project is called.

Plugins: list_plugins shows the installed CLAP, LV2 and VST3 plugins (VST3
parameter values are the plugin's normalized ones, 0 to 1, or 0 to the step
count; get_plugin's `text` shows what they mean); add_plugin
puts one on a track (on a MIDI track, an instrument first in the chain plays
the notes). MIDI effects (midi_output in list_plugins) pass the MIDI they
send to the next plugin that takes notes; on a track with a MIDI output
device, what the effects at the start of the chain send goes to the device.
get_plugin lists a slot's parameters (with their values as the plugin shows
them); set_plugin_param (which also takes a value as text, e.g. `-6 dB`),
set_automation, add_automation_point and write_automation change them.
Plugins keep what they save of themselves (state beyond parameters, and
files LV2 plugins keep with it) in the project. A plugin running in its own
process (`process_id` in get_plugin) that crashes stops alone: its audio
passes through and get_plugin's `error` says so; reload_plugin starts it
again. show_plugin_gui opens such a plugin's own GUI (`has_gui`) on the
desktop; what's changed there goes into the project like any change. set_plugin_sidechain feeds a
track's signal into a plugin's sidechain input (`sidechain` in list_plugins);
set_plugin_output sends a multi-out instrument's other outputs (`outputs`) to
buses.

Mixing: add_track with kind `bus` makes a bus, and set_track_output sends a
track (or bus) to it instead of the master. The master is track 0 (`master`
in get_state): it takes plugins, set_track_gain and automation like any bus.

Clips: move_clip moves one, trim_clip moves its start or end edge (what it
plays stays in place), and split_clip cuts it in two. duplicate_clip copies
one (to anywhere, on its track or another of its kind), and repeat_clip lays
copies of it end to end after it. These place clips exactly where they're
told, even over others (overlapping clips play together).

Recording: set_track_arm arms a track and record starts recording on every
armed track (stop places the takes). set_track_monitor lets an armed audio
track play its input. A recording over earlier clips becomes a take in a lane
above them (`lane` in get_state), and they go silent where it plays; earlier
loop passes are muted takes. comp_take picks which take plays over a range.
Recordings are shifted earlier to line up with what was heard
(record_latency_ms in get_engine_status; MIDI by each device's
midi_record_latency_ms, from the latency PipeWire reports for it);
set_record_offset corrects the estimate.

Audio: import_audio puts a file on the timeline. render_range renders any part
of the project offline and reports levels and loudness, so you can check a mix
without listening. export_audio writes the mix (the whole song by default) to
a WAV or FLAC file. get_engine_status says whether this instance plays
through PipeWire or runs a virtual clock (advanced with advance_clock).

MIDI: MIDI tracks (add_track with kind `midi`) play their clips through a
built-in synth (set_synth). add_midi_clip creates a clip with notes;
add_midi_notes, edit_midi_notes, remove_midi_notes and quantize_midi_notes
edit them, and set_midi_controls sets pitch bend, CCs, pressure, program
changes and MIDI 2.0 per-note controllers (per-note pitch bend, volume,
brightness, ...). import_midi reads a .mid file. Note times are in beats (quarter
notes) from the clip's start; clip positions and ranges are in seconds, or
in beats from the project's start (`*_beats` instead of `*_s`, e.g.
start_beats) or bars as the ruler numbers them (`*_bar`, from 1; end_bar 33
is where 32 bars end; a clip length as length_beats). get_state lists
notes in frames and in beats from their clip's start (start_beats,
length_beats), and clips in frames and beats (start_beats, end_beats,
start_bar). set_tempo (with at_beats, and ramp for a gradual change)
and set_tempo_map change the tempo along the timeline; MIDI clips and notes
keep their beat positions. set_time_signature (with at_bar) and
set_time_signature_map change the meter from a bar on. set_track_midi_input
and set_track_midi_output pick a track's MIDI devices and channels
(get_engine_status lists the devices, and apps with MIDI ports);
set_track_midi_output's latency_ms compensates a device that sounds late, on
top of what PipeWire reports for its port (midi_output_latency_ms in
get_engine_status).

UI: ui_tree returns the window's widgets with stable IDs (e.g. transport/play,
track/<id>/mute), bounds and state; ui_click clicks one by ID; ui_screenshot
returns a PNG of the window; ui_type types into a field clicked first (e.g.
tempo/value, the tempo lane's BPM, or meter/numerator/value; double-click
track/<id>/midi_out_latency to type a latency), and ui_key return sets it.
In the plugin picker (track/<id>/add_plugin), ui_type searches the list
(plugin_picker/item/<n>, best matches first) and ui_key return adds the
first match.
arrange/tempo shows the tempo and time signature lanes (tempo/<n> and
meter/<n> points); track/<id>/takes shows a track's take lanes (take/<clip
id>).";

/// The MCP server. Cheap to clone; all clones talk to the same studio.
#[derive(Debug, Clone)]
pub struct DawkeyMcp {
    app: Handle<App>,
    tools: Arc<[Tool]>,
}

/// Bad arguments are reported to the caller as a tool error, not a protocol
/// error, so a model can read the message and retry.
macro_rules! try_args {
    ($parsed:expr) => {
        match $parsed {
            Ok(args) => args,
            Err(message) => return Ok(Err(message)),
        }
    };
}

/// Like `?` for tool-level errors inside `dispatch`.
macro_rules! tool_try {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(message) => return Ok(Err(message)),
        }
    };
}

impl DawkeyMcp {
    pub fn new(app: Handle<App>) -> Self {
        Self {
            app,
            tools: build_tools().into(),
        }
    }

    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    /// Runs a tool. `Ok` holds the tool's JSON result; `Err` holds a message
    /// for the caller (bad arguments, failed command). Unknown tools are
    /// protocol errors.
    async fn dispatch(
        &self,
        name: &str,
        mut args: Value,
    ) -> Result<Result<Value, String>, McpError> {
        let studio = &self.app;
        // Positions in beats or bars become seconds, through the tempo map
        // as it is now.
        let stems = self
            .tools
            .iter()
            .find(|t| t.name == name)
            .map(|t| times::position_stems(&Value::Object((*t.input_schema).clone())))
            .unwrap_or_default();
        if times::has_beats(&args, &stems) {
            let maps = tool_try!(
                run(studio, |s| Ok::<_, String>(times::Maps::of(
                    s.session().project()
                )))
                .await
            );
            tool_try!(maps.resolve(&mut args, &stems));
        }
        let result = match name {
            GET_STATE => {
                let GetState {
                    notes,
                    controls,
                    automation,
                    tracks,
                    summary,
                } = try_args!(parse(args));
                let trim = state::Trim {
                    notes: summary || !notes,
                    controls: summary || !controls,
                    automation: summary || !automation,
                    tracks: tracks.map(|ids| ids.into_iter().map(|id| id.0).collect()),
                };
                run(studio, move |s| Ok::<_, CoreError>(state_json(s, trim))).await
            }
            UNDO => {
                let NoArgs {} = try_args!(parse(args));
                run(studio, |s| {
                    let undone = s.undo()?;
                    Ok::<_, CoreError>(json!({ "undone": undone, "revision": revision(s) }))
                })
                .await
            }
            REDO => {
                let NoArgs {} = try_args!(parse(args));
                run(studio, |s| {
                    let redone = s.redo()?;
                    Ok::<_, CoreError>(json!({ "redone": redone, "revision": revision(s) }))
                })
                .await
            }
            NEW_PROJECT => {
                let NewProject {
                    name,
                    discard_changes,
                } = try_args!(parse(args));
                run(studio, move |s| {
                    refuse_if_dirty(s.session(), discard_changes)?;
                    let project = Project::new(name.unwrap_or_else(|| "Untitled".into()));
                    s.replace(LoadedProject::new(project))
                        .map_err(|e| e.to_string())?;
                    Ok::<_, String>(state_json(s, state::Trim::summary()))
                })
                .await
            }
            OPEN_PROJECT => {
                let OpenProject {
                    path,
                    discard_changes,
                } = try_args!(parse(args));
                // Check before decoding (slow), and again before replacing.
                tool_try!(
                    run(studio, move |s| refuse_if_dirty(
                        s.session(),
                        discard_changes
                    ))
                    .await
                );
                let loaded = tool_try!(blocking(move || LoadedProject::open(&path)).await);
                run(studio, move |s| {
                    refuse_if_dirty(s.session(), discard_changes)?;
                    s.replace(loaded).map_err(|e| e.to_string())?;
                    Ok::<_, String>(state_json(s, state::Trim::summary()))
                })
                .await
            }
            SAVE_PROJECT => {
                let SaveProject { path } = try_args!(parse(args));
                run(studio, move |s| {
                    let saved = s.save(path.as_deref())?;
                    Ok::<_, CoreError>(json!({ "path": saved, "revision": revision(s) }))
                })
                .await
            }
            IMPORT_AUDIO => self.import_audio(try_args!(parse(args))).await,
            IMPORT_MIDI => {
                let ImportMidi {
                    path,
                    position_s,
                    use_file_tempo,
                } = try_args!(parse(args));
                let file = tool_try!(blocking(move || dawkey_media::read_midi_file(&path)).await);
                run(studio, move |s| {
                    let import = s.import_midi(&file, position_s, use_file_tempo)?;
                    let mut result = to_json(&import);
                    result["revision"] = json!(revision(s));
                    Ok::<_, dawkey_studio::StudioError>(result)
                })
                .await
            }
            PLAY => {
                let NoArgs {} = try_args!(parse(args));
                let cycles = tool_try!(run(studio, |s| s.play().map(|()| s.engine_cycles())).await);
                self.transport_after(cycles, |t| t.playing)
                    .await
                    .map(|t| to_json(&t))
            }
            STOP => {
                let NoArgs {} = try_args!(parse(args));
                let (was_recording, cycles) = tool_try!(
                    run(studio, |s| {
                        let was_recording = s.transport().recording;
                        s.stop()?;
                        Ok::<_, dawkey_studio::StudioError>((was_recording, s.engine_cycles()))
                    })
                    .await
                );
                let transport = tool_try!(self.transport_after(cycles, |t| !t.playing).await);
                let mut result = to_json(&transport);
                if was_recording {
                    let recorded = tool_try!(
                        run(studio, |s| Ok::<_, String>(
                            s.engine_status().last_recording
                        ))
                        .await
                    );
                    result["recorded"] = to_json(&recorded);
                }
                Ok(result)
            }
            RECORD => {
                let NoArgs {} = try_args!(parse(args));
                let cycles =
                    tool_try!(run(studio, |s| s.record().map(|()| s.engine_cycles())).await);
                self.transport_after(cycles, |t| t.recording)
                    .await
                    .map(|t| to_json(&t))
            }
            SET_RECORD_OFFSET => {
                let SetRecordOffset { ms } = try_args!(parse(args));
                run(studio, move |s| {
                    s.set_record_offset_ms(ms)?;
                    Ok::<_, dawkey_studio::StudioError>(json!({
                        "record_latency_ms": s.engine_status().record_latency_ms,
                        "record_offset_ms": ms,
                    }))
                })
                .await
            }
            LOCATE => {
                let Locate { position_s } = try_args!(parse(args));
                let (cycles, frame) = tool_try!(
                    run(studio, move |s| {
                        s.locate(position_s)?;
                        let frame = s.session().project().seconds_to_frames(position_s);
                        Ok::<_, dawkey_studio::StudioError>((s.engine_cycles(), frame))
                    })
                    .await
                );
                // While playing the playhead moves on from there.
                self.transport_after(cycles, move |t| t.position_frames >= frame)
                    .await
                    .map(|t| to_json(&t))
            }
            GET_ENGINE_STATUS => {
                let NoArgs {} = try_args!(parse(args));
                run(studio, |s| Ok::<_, CoreError>(to_json(&s.engine_status()))).await
            }
            GET_METERS => {
                let NoArgs {} = try_args!(parse(args));
                run(studio, |s| Ok::<_, CoreError>(to_json(&s.take_meters()))).await
            }
            ADVANCE_CLOCK => {
                let AdvanceClock {
                    seconds,
                    input_path,
                    midi_input,
                    midi_controls,
                    midi_management,
                } = try_args!(parse(args));
                // Controls first: at the same time, they come before notes.
                let mut midi = Vec::new();
                for control in &midi_controls {
                    midi.push((control.device.clone(), vec![try_args!(control.event())]));
                }
                for message in &midi_management {
                    midi.push((message.device.clone(), vec![try_args!(message.event())]));
                }
                for note in &midi_input {
                    midi.push((note.device.clone(), try_args!(note.events()).to_vec()));
                }
                if let Some(path) = input_path {
                    let rate = tool_try!(run(studio, |s| Ok::<_, String>(s.sample_rate())).await);
                    let decoded =
                        tool_try!(blocking(move || dawkey_media::decode_file(&path, rate)).await);
                    tool_try!(run(studio, move |s| s.set_virtual_input(Some(decoded.audio))).await);
                }
                run(studio, move |s| {
                    for (device, events) in &midi {
                        s.queue_virtual_midi(device.as_deref(), events)?;
                    }
                    let output = s.advance(seconds)?;
                    let mut result = json!({ "output": output, "transport": s.transport() });
                    if !s.sent_midi().is_empty() {
                        result["midi_output"] = to_json(&s.sent_midi());
                    }
                    Ok::<_, dawkey_studio::StudioError>(result)
                })
                .await
            }
            RENDER_RANGE => self.render_range(try_args!(parse(args))).await,
            EXPORT_AUDIO => self.export_audio(try_args!(parse(args))).await,
            UI_TREE => {
                let NoArgs {} = try_args!(parse(args));
                run_app(studio, |a| {
                    Ok::<_, String>(json!({
                        "platform": a.platform(),
                        "theme": a.theme(),
                        "window": a.window(),
                        "root": a.access_tree(),
                    }))
                })
                .await
            }
            UI_CLICK => {
                let UiClick {
                    id,
                    x,
                    y,
                    button,
                    modifiers,
                    double,
                } = try_args!(parse(args));
                run_app(studio, move |a| {
                    if double && !matches!(button, Button::Left) {
                        return Err("only the left button double-clicks".to_owned());
                    }
                    let chrome = match (id, x, y) {
                        (Some(id), None, None) if double => {
                            a.double_click(&id, modifiers.into())?
                        }
                        (Some(id), None, None) => a.click(&id, button.into(), modifiers.into())?,
                        (None, Some(x), Some(y)) if double => {
                            a.double_click_at(Point::new(x, y), modifiers.into())?
                        }
                        (None, Some(x), Some(y)) => {
                            a.click_at(Point::new(x, y), button.into(), modifiers.into())?
                        }
                        _ => return Err("give either id, or both x and y".to_owned()),
                    };
                    Ok::<_, String>(ui_result(a, chrome))
                })
                .await
            }
            UI_DRAG => {
                let UiDrag {
                    id,
                    x,
                    y,
                    dx,
                    dy,
                    steps,
                    modifiers,
                } = try_args!(parse(args));
                run_app(studio, move |a| {
                    let from = match (id, x, y) {
                        (Some(id), None, None) => DragFrom::Widget(id),
                        (None, Some(x), Some(y)) => DragFrom::Point(Point::new(x, y)),
                        _ => return Err("give either id, or both x and y".to_owned()),
                    };
                    let steps = steps.unwrap_or(8).clamp(1, 200);
                    let chrome = a.drag(from, Point::new(dx, dy), steps, modifiers.into())?;
                    Ok::<_, String>(ui_result(a, chrome))
                })
                .await
            }
            UI_HOVER => {
                let UiHover { id, x, y } = try_args!(parse(args));
                run_app(studio, move |a| {
                    let pos = match (id, x, y) {
                        (Some(id), None, None) => a.widget_center(&id)?,
                        (None, Some(x), Some(y)) => Point::new(x, y),
                        _ => return Err("give either id, or both x and y".to_owned()),
                    };
                    a.hover(pos);
                    Ok::<_, String>(json!({ "view": a.view_state() }))
                })
                .await
            }
            UI_SCROLL => {
                let UiScroll {
                    x,
                    y,
                    dx,
                    dy,
                    modifiers,
                } = try_args!(parse(args));
                run_app(studio, move |a| {
                    a.scroll(Point::new(x, y), Point::new(dx, dy), modifiers.into());
                    Ok::<_, String>(json!({ "view": a.view_state() }))
                })
                .await
            }
            UI_KEY => {
                let UiKey { key } = try_args!(parse(args));
                let (key, modifiers) = try_args!(parse_key_combo(&key));
                run_app(studio, move |a| {
                    let chrome = a.key(&key, modifiers)?;
                    Ok::<_, String>(ui_result(a, chrome))
                })
                .await
            }
            UI_TYPE => {
                let UiType { text } = try_args!(parse(args));
                run_app(studio, move |a| {
                    let mut chrome = Vec::new();
                    for c in text.chars() {
                        let key = if c == ' ' {
                            "space".to_owned()
                        } else {
                            c.to_string()
                        };
                        chrome.extend(a.key(&key, Modifiers::default())?);
                    }
                    Ok::<_, String>(ui_result(a, chrome))
                })
                .await
            }
            UI_DROP_FILES => self.drop_files(try_args!(parse(args))).await,
            LIST_PLUGINS => {
                let ListPlugins {
                    query,
                    kind,
                    params,
                    detail,
                    first,
                    count,
                } = try_args!(parse(args));
                let detail = detail || params;
                let count = count.unwrap_or(100);
                run(studio, move |s| {
                    let query = query.map(|q| q.to_lowercase());
                    let catalog = s.plugin_catalog();
                    let matching: Vec<_> = catalog
                        .plugins
                        .iter()
                        .filter(|p| match kind {
                            Some(PluginKind::Instrument) => p.instrument && !p.midi_output,
                            Some(PluginKind::MidiEffect) => p.instrument && p.midi_output,
                            Some(PluginKind::Effect) => !p.instrument,
                            None => true,
                        })
                        .filter(|p| {
                            query.as_ref().is_none_or(|q| {
                                [&p.name, &p.vendor, &p.id]
                                    .iter()
                                    .any(|f| f.to_lowercase().contains(q))
                            })
                        })
                        .collect();
                    let total = matching.len();
                    let plugins: Vec<Value> = matching
                        .into_iter()
                        .skip(first)
                        .take(count)
                        .map(|p| {
                            let kind = match (p.instrument, p.midi_output) {
                                (true, false) => "instrument",
                                (true, true) => "midi_effect",
                                (false, _) => "effect",
                            };
                            let mut v = json!({
                                "id": p.id,
                                "name": p.name,
                                "vendor": p.vendor,
                                "format": p.format,
                                "kind": kind,
                            });
                            if !detail {
                                return v;
                            }
                            v["instrument"] = json!(p.instrument);
                            v["midi_output"] = json!(p.midi_output);
                            v["audio_inputs"] = json!(p.audio_inputs);
                            v["audio_outputs"] = json!(p.audio_outputs);
                            v["param_count"] = json!(p.params.len());
                            if p.sidechain {
                                v["sidechain"] = json!(true);
                            }
                            if !p.outputs.is_empty() {
                                v["outputs"] = to_json(&p.outputs);
                            }
                            if params {
                                v["params"] = to_json(&p.params);
                            }
                            v
                        })
                        .collect();
                    let mut result = json!({
                        "plugins": plugins,
                        "total": total,
                        "scan_failures": catalog.failures.len(),
                    });
                    if first + count < total {
                        result["next"] = json!(first + count);
                    }
                    Ok::<_, String>(result)
                })
                .await
            }
            RESCAN_PLUGINS => {
                let NoArgs {} = try_args!(parse(args));
                run(studio, |s| {
                    let catalog = s.rescan_plugins();
                    Ok::<_, String>(json!({
                        "found": catalog.plugins.len(),
                        "failures": catalog.failures,
                    }))
                })
                .await
            }
            RELOAD_PLUGIN => {
                let ReloadPlugin { plugin } = try_args!(parse(args));
                run(studio, move |s| {
                    s.reload_plugin(plugin)?;
                    Ok::<_, dawkey_studio::StudioError>(to_json(&s.plugin_details(plugin)))
                })
                .await
            }
            SHOW_PLUGIN_GUI => {
                let ShowPluginGui { plugin, show } = try_args!(parse(args));
                run(studio, move |s| {
                    s.show_plugin_gui(plugin, show)?;
                    Ok::<_, dawkey_studio::StudioError>(to_json(&s.plugin_details(plugin)))
                })
                .await
            }
            GET_PLUGIN => {
                let GetPlugin {
                    plugin,
                    first,
                    count,
                } = try_args!(parse(args));
                run(studio, move |s| {
                    s.plugin_details_window(plugin, first, count.unwrap_or(200))
                        .map(|d| to_json(&d))
                        .ok_or_else(|| format!("plugin {plugin} does not exist"))
                })
                .await
            }
            UI_RESIZE => {
                let UiResize { width, height } = try_args!(parse(args));
                run_app(studio, move |a| {
                    if a.platform() != Platform::Headless {
                        return Err("ui_resize only works headless; the compositor sizes real \
                                    windows"
                            .to_owned());
                    }
                    if !(200.0..=8192.0).contains(&width) || !(150.0..=8192.0).contains(&height) {
                        return Err(format!("size out of range: {width}x{height}"));
                    }
                    a.set_window(WindowInfo {
                        size: Size::new(width, height),
                        ..a.window()
                    });
                    Ok(to_json(&a.window()))
                })
                .await
            }
            _ => {
                let spec = CommandSpec::find(name).ok_or_else(|| {
                    McpError::invalid_params(format!("unknown tool: {name}"), None)
                })?;
                let command =
                    try_args!((spec.parse)(args).map_err(|e| format!("invalid arguments: {e}")));
                run(studio, move |s| {
                    let outcome = s.execute(command)?;
                    let mut result = to_json(&outcome);
                    result["revision"] = revision(s).into();
                    Ok::<_, CoreError>(result)
                })
                .await
            }
        };
        Ok(result)
    }

    /// The transport state once the audio thread has applied a command sent
    /// when the engine had run `cycles` cycles: when `done` says so, or the
    /// engine has run two more cycles since (so the command was consumed),
    /// or after a short wait (a stopped audio device). The virtual backend
    /// applies commands at once, so this returns right away there.
    async fn transport_after(
        &self,
        cycles: u64,
        done: impl Fn(&dawkey_studio::TransportState) -> bool + Send + Sync + 'static,
    ) -> Result<dawkey_studio::TransportState, String> {
        let done = Arc::new(done);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            let done = done.clone();
            let (transport, settled) = run(&self.app, move |s| {
                let transport = s.transport();
                let settled = done(&transport) || s.engine_cycles() >= cycles + 2;
                Ok::<_, String>((transport, settled))
            })
            .await?;
            if settled || tokio::time::Instant::now() >= deadline {
                return Ok(transport);
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    async fn import_audio(&self, args: ImportAudio) -> Result<Value, String> {
        let ImportAudio {
            path,
            track,
            position_s,
        } = args;
        let path = std::path::absolute(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        // Validate the target before the (slow) decode.
        let rate = run(&self.app, move |s| {
            if let Some(id) = track {
                let t = s
                    .session()
                    .project()
                    .track(id)
                    .ok_or(CoreError::TrackNotFound(id))?;
                if t.kind != dawkey_core::TrackKind::Audio {
                    return Err(CoreError::InvalidArgument(format!(
                        "track {id} is a MIDI track; audio clips need an audio track"
                    )));
                }
            }
            Ok(s.sample_rate())
        })
        .await?;
        let decode_path = path.clone();
        let decoded = blocking(move || dawkey_media::decode_file(&decode_path, rate)).await?;
        let (channels, frames, source_rate) = (
            decoded.audio.channel_count(),
            decoded.audio.frames(),
            decoded.source_rate,
        );
        run(&self.app, move |s| {
            let source = s.add_source(&path, decoded);
            let outcome = s.execute(AddClip::new(source, track, position_s).into())?;
            Ok::<_, CoreError>(json!({
                "source": source,
                "track": outcome.track,
                "clip": outcome.clip,
                "channels": channels,
                "frames": frames,
                "duration_s": frames as f64 / f64::from(rate),
                "source_sample_rate": source_rate,
                "revision": revision(s),
            }))
        })
        .await
    }

    /// Drops files on the window and imports them before returning.
    async fn drop_files(&self, args: UiDropFiles) -> Result<Value, String> {
        let UiDropFiles { paths, x, y } = args;
        let paths = paths
            .into_iter()
            .map(|p| std::path::absolute(&p).unwrap_or(p))
            .collect();
        let requests = run_app(&self.app, move |a| {
            a.drop_files(paths, Point::new(x, y));
            Ok::<_, String>(a.take_imports())
        })
        .await?;
        let mut imported = Vec::new();
        for request in requests {
            imported.push(self.import(request).await?);
        }
        Ok(json!({ "imported": imported }))
    }

    /// Decodes and places one import.
    async fn import(&self, request: ImportRequest) -> Result<Value, String> {
        if request.is_midi() {
            let path = request.path.clone();
            let file = blocking(move || dawkey_media::read_midi_file(&path)).await?;
            return run_app(&self.app, move |a| {
                let placed = a.finish_midi_import(&request, &file)?;
                Ok::<_, String>(json!({ "path": request.path, "placed": placed }))
            })
            .await;
        }
        let rate = run(&self.app, |s| Ok::<_, String>(s.sample_rate())).await?;
        let path = request.path.clone();
        let decoded = blocking(move || dawkey_media::decode_file(&path, rate)).await?;
        run_app(&self.app, move |a| {
            let placed = a.finish_import(&request, decoded)?;
            Ok::<_, String>(json!({ "path": request.path, "placed": placed }))
        })
        .await
    }

    async fn render_range(&self, args: RenderRange) -> Result<Value, String> {
        let RenderRange {
            start_s,
            end_s,
            output_path,
        } = args;
        // Make the file's folder first, so a bad path fails before the
        // render, not after it.
        let output_path = output_path.map(|path| std::path::absolute(&path).unwrap_or(path));
        if let Some(path) = &output_path {
            dawkey_media::make_output_folder(path).map_err(|e| e.to_string())?;
        }
        let job = run(&self.app, move |s| s.render_job(start_s, end_s)).await?;
        let (result, leftovers) = blocking(move || {
            let rate = job.sample_rate();
            let (audio, leftovers) = job.run();
            let analysis = dawkey_media::analyze(&audio, rate);
            let written = match output_path {
                Some(path) => dawkey_media::write_wav(&path, &audio, rate).map(|()| Some(path)),
                None => Ok(None),
            };
            let result = written.map(|written| {
                json!({
                    "start_s": start_s,
                    "end_s": end_s,
                    "sample_rate": rate,
                    "analysis": analysis,
                    "output_path": written,
                })
            });
            Ok::<_, String>((result, leftovers))
        })
        .await?;
        // The render's plugin instances are unloaded on the studio's thread.
        run(&self.app, move |s| {
            s.finish_render(leftovers);
            Ok::<_, String>(())
        })
        .await?;
        result.map_err(|e| e.to_string())
    }
}

impl DawkeyMcp {
    async fn export_audio(&self, args: ExportAudio) -> Result<Value, String> {
        let ExportAudio {
            path,
            format,
            bit_depth,
            sample_rate,
            start_s,
            end_s,
            tail_s,
        } = args;
        let path = std::path::absolute(&path).unwrap_or(path);
        let file = match format {
            Some(ExportFormat::Wav) => dawkey_media::FileFormat::Wav,
            Some(ExportFormat::Flac) => dawkey_media::FileFormat::Flac,
            None => dawkey_media::FileFormat::from_path(&path).ok_or_else(|| {
                format!(
                    "{} doesn't end in .wav or .flac; give a format",
                    path.display()
                )
            })?,
        };
        let sample = match bit_depth.unwrap_or(24) {
            16 => dawkey_media::SampleFormat::Int16,
            24 => dawkey_media::SampleFormat::Int24,
            32 => dawkey_media::SampleFormat::Float32,
            bits => return Err(format!("bit_depth is 16, 24 or 32, not {bits}")),
        };
        let tail_s = tail_s.unwrap_or(0.0);
        if !(0.0..=60.0).contains(&tail_s) {
            return Err(format!("tail_s is from 0 to 60 seconds, not {tail_s}"));
        }
        let start_s = start_s.unwrap_or(0.0);
        // Make the file's folder first, so a bad path fails before the
        // render, not after it.
        dawkey_media::make_output_folder(&path).map_err(|e| e.to_string())?;
        let (job, spec, end_s) = run(&self.app, move |s| {
            let project = s.session().project();
            let end_s = match end_s {
                Some(end_s) => end_s,
                None if project.end() == 0 => {
                    return Err(
                        "there's nothing to export: the project has no clips that play \
                                (give end_s to export silence)"
                            .to_owned(),
                    );
                }
                None => project.frames_to_seconds(project.end()),
            } + tail_s;
            let spec = dawkey_media::ExportSpec {
                file,
                sample,
                sample_rate: sample_rate.unwrap_or(project.sample_rate),
            };
            spec.check()?;
            let job = s.export_job(start_s, end_s).map_err(|e| e.to_string())?;
            Ok::<_, String>((job, spec, end_s))
        })
        .await?;
        let (result, leftovers) = blocking(move || {
            let progress = dawkey_media::ExportProgress::default();
            let (result, leftovers) = job.run(&spec, &path, &progress);
            let result = result.map(|analysis| {
                json!({
                    "path": path,
                    "format": spec.file,
                    "sample_format": spec.sample,
                    "sample_rate": spec.sample_rate,
                    "start_s": start_s,
                    "end_s": end_s,
                    "analysis": analysis,
                })
            });
            Ok::<_, String>((result, leftovers))
        })
        .await?;
        // The render's plugin instances are unloaded on the studio's thread.
        run(&self.app, move |s| {
            s.finish_render(leftovers);
            Ok::<_, String>(())
        })
        .await?;
        result.map_err(|e| e.to_string())
    }
}

impl ServerHandler for DawkeyMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("dawkey", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tools.to_vec()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let args = Value::Object(request.arguments.unwrap_or_default());
        tracing::debug!(tool = %request.name, %args, "tool call");
        if request.name == UI_SCREENSHOT {
            return Ok(self.screenshot(args).await.into());
        }
        let result = match self.dispatch(&request.name, args).await? {
            Ok(value) => CallToolResult::structured(value),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        };
        Ok(result.into())
    }
}

impl DawkeyMcp {
    /// `ui_screenshot` returns an image, not JSON.
    async fn screenshot(&self, args: Value) -> CallToolResult {
        let UiScreenshot { scale } = match parse(args) {
            Ok(args) => args,
            Err(message) => return CallToolResult::error(vec![ContentBlock::text(message)]),
        };
        let scale = scale.unwrap_or(1.0);
        if !(0.25..=4.0).contains(&scale) {
            return CallToolResult::error(vec![ContentBlock::text(format!(
                "scale must be between 0.25 and 4, got {scale}"
            ))]);
        }
        let shot = run_app(&self.app, move |a| a.screenshot(scale)).await;
        match shot {
            Ok(image) => {
                let png = base64::engine::general_purpose::STANDARD.encode(image.to_png());
                CallToolResult::success(vec![
                    ContentBlock::image(png, "image/png"),
                    ContentBlock::text(format!(
                        "{}x{} px PNG at scale {scale}",
                        image.width, image.height
                    )),
                ])
            }
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }
}

/// Serves MCP to every client that connects to the Unix socket at `path`,
/// until the process exits. The socket is created owner-only (0600); file
/// permissions are the access control.
pub async fn serve_unix(app: Handle<App>, path: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if path.exists() {
        std::fs::remove_file(path)?;
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!("MCP listening on {}", path.display());
    loop {
        let (stream, _) = listener.accept().await?;
        let server = DawkeyMcp::new(app.clone());
        tokio::spawn(async move {
            let (read, write) = stream.into_split();
            match server.serve((read, write)).await {
                Ok(service) => {
                    let _ = service.waiting().await;
                }
                Err(e) => tracing::warn!("MCP client handshake failed: {e}"),
            }
        });
    }
}

/// Bridges stdin/stdout to a running Dawkey's MCP socket, so a stdio MCP
/// client can drive a live window.
pub async fn attach(path: &std::path::Path) -> anyhow::Result<()> {
    let stream = tokio::net::UnixStream::connect(path)
        .await
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    let (mut from_dawkey, mut to_dawkey) = stream.into_split();
    let upstream = async {
        tokio::io::copy(&mut tokio::io::stdin(), &mut to_dawkey).await?;
        tokio::io::AsyncWriteExt::shutdown(&mut to_dawkey).await
    };
    let downstream = async {
        let mut stdout = tokio::io::stdout();
        tokio::io::copy(&mut from_dawkey, &mut stdout).await?;
        tokio::io::AsyncWriteExt::flush(&mut stdout).await
    };
    // Done when Dawkey closes the connection or the client goes away.
    tokio::select! {
        r = downstream => r?,
        r = upstream => r?,
    }
    Ok(())
}

/// Serves MCP over stdin/stdout until the client disconnects.
pub async fn serve_stdio(app: Handle<App>) -> anyhow::Result<()> {
    let service = DawkeyMcp::new(app).serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

fn parse<T: DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))
}

fn to_json(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).expect("studio types serialize to JSON")
}

/// What a synthetic UI action led to. Headless instances can't show dialogs,
/// so requested ones are reported instead.
fn ui_result(app: &mut App, chrome: Vec<dawkey_ui::ChromeAction>) -> Value {
    let dialogs = match app.platform() {
        Platform::Headless => app.take_platform_requests(),
        Platform::Wayland => Vec::new(),
    };
    json!({ "window_actions": chrome, "dialogs": dialogs, "view": app.view_state() })
}

/// Parses "ctrl+shift+z" into a key and modifiers.
fn parse_key_combo(combo: &str) -> Result<(String, Modifiers), String> {
    let mut modifiers = Modifiers::default();
    let mut parts: Vec<&str> = combo.split('+').collect();
    // "ctrl++" means ctrl and the plus key.
    let key = match parts.pop() {
        Some("") if combo.ends_with("++") || combo == "+" => {
            parts.pop();
            "+".to_owned()
        }
        Some(k) if !k.is_empty() => k.to_lowercase(),
        _ => return Err(format!("no key in {combo:?}")),
    };
    for part in parts.into_iter().filter(|p| !p.is_empty()) {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => modifiers.ctrl = true,
            "shift" => modifiers.shift = true,
            "alt" => modifiers.alt = true,
            "super" | "logo" | "meta" => modifiers.logo = true,
            other => return Err(format!("unknown modifier {other:?} in {combo:?}")),
        }
    }
    Ok((key, modifiers))
}

/// The session state as get_state returns it: with beats added, and with
/// what `trim` says left out.
fn state_json(studio: &Studio, trim: state::Trim) -> Value {
    let mut state = to_json(&studio.session().state());
    times::with_beats(studio.session().project(), &mut state);
    trim.apply(&mut state);
    state
}

fn revision(studio: &Studio) -> u64 {
    studio.session().revision()
}

fn refuse_if_dirty(session: &Session, discard_changes: bool) -> Result<(), String> {
    if session.is_dirty() && !discard_changes {
        return Err(
            "the open project has unsaved changes; save it first or pass discard_changes: true"
                .into(),
        );
    }
    Ok(())
}

/// Runs `f` against the studio on the app's thread, mapping errors to
/// tool-level messages.
async fn run<F, T, E>(app: &Handle<App>, f: F) -> Result<T, String>
where
    F: FnOnce(&mut Studio) -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: Display + Send + 'static,
{
    run_app(app, move |a: &mut App| f(&mut a.studio)).await
}

/// Runs `f` on the app's thread, mapping errors to tool-level messages.
async fn run_app<F, T, E>(app: &Handle<App>, f: F) -> Result<T, String>
where
    F: FnOnce(&mut App) -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: Display + Send + 'static,
{
    match app.with(f).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(e.to_string()),
        Err(closed) => Err(closed.to_string()),
    }
}

/// Runs slow work (decoding, rendering) on tokio's blocking pool.
async fn blocking<F, T, E>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: Display + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) => Err(format!("worker failed: {e}")),
    }
}
