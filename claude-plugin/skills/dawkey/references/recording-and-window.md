# Recording, MIDI devices and the window

## Recording

The user performs and you set things up. Arm only the tracks they want, and start `record` only when they say they're ready. It records from their real microphone or keyboard and plays through their speakers.

Setting up an audio take:

1. `set_track_input { track, input }`: `stereo` (both inputs), or `left` or `right` for a mono source such as one microphone or a guitar.
2. `set_track_arm { track, armed: true }`. Dawkey links the default audio source to its inputs only while an audio track is armed. An armed track's meter shows its input, so `get_meters` tells you whether a signal is arriving before you record.
3. `set_track_monitor { track, monitor: true }` if the user wants to hear themselves through Dawkey. Leave it off if they monitor through their interface, or there'll be an echo.
4. `locate` to a bar or two before the part (a count-in), then `record`. `stop` ends it and places the takes.

Loops and punches:

- **Loop recording:** `set_loop { start_s, end_s, enabled: true }`. Each pass becomes a take, and the earlier passes are muted.
- **Punching in:** `set_punch { start_s, end_s, enabled: true }`. Only that range is recorded, while the rest plays back.
- A recording over existing clips becomes a take in a lane above them (`lane` in `get_state`), and they go silent where it plays. `comp_take { clip, start_s, end_s }` picks which take plays over a range, so the best parts of several takes can be combined.

Timing: recordings are shifted earlier by the latency Dawkey knows about (`record_latency_ms` in `get_engine_status`). If takes still land late or early, measure the offset (record a click back through a cable, or compare a clap to the beat) and correct it with `set_record_offset`.

After a take, render the recorded track to check it isn't silent or clipped (`peak_dbfs` near 0 means the input gain was too high), and tell the user what you found.

## MIDI devices

- `get_engine_status` lists `midi_inputs` and `midi_outputs`: the hardware and apps PipeWire offers.
- `set_track_midi_input { track, device, channel }` picks the keyboard a MIDI track listens to. Leave `device` out for every device, and `channel` out for every channel. Arm the track to play it live through its instrument and to record it.
- `set_track_midi_output { track, device, channel, latency_ms }` sends the track's notes to an external synth instead of sounding them. `latency_ms` compensates a device that sounds late.

## Headless and virtual-clock instances

With `backend: "virtual"` nothing plays in real time. `advance_clock { seconds }` runs the engine and returns an analysis of what came out. It can feed a WAV to the inputs (`input_path`) and notes to the MIDI input (`midi_input`), so recording can be tried without a microphone or keyboard. `play`, `stop` and `locate` still work, and time moves only when you advance it. For checking the sound of the project, `render_range` is simpler.

## The window

Every tool works on the project directly, and that's how you should edit it. The `ui_*` tools are for showing the user things and for checking what they see:

- `ui_screenshot` returns a PNG of the window. Use it to check an arrangement's layout, or to show the user what you did. In a headless instance the window is rendered offscreen, so it works there too.
- `ui_tree` lists the widgets with stable IDs, bounds and state.
- `ui_click { id }` clicks one. These are useful for the user:
  - `view/arrange`, `view/mixer` and `view/piano_roll` switch the main view.
  - `arrange/tempo` shows the tempo and time signature lanes.
  - `track/<id>/automation` shows a track's automation lanes, and `track/<id>/takes` its take lanes.
  - `plugin/<id>/gui` opens a plugin's own GUI. (`show_plugin_gui` does the same.)
- `ui_key`, `ui_type`, `ui_drag`, `ui_scroll` and `ui_hover` act like the keyboard and mouse. Use them only when the user asks you to show them how something is done in the window.

Switch views only when it helps the user. They may be working in another view.
