# Dawkey — Project Plan

This is the plan Dawkey was built from, with notes added as each milestone was finished. It records why things are the way they are. For the code as it is now, see [ARCHITECTURE.md](ARCHITECTURE.md).

## Context

Dawkey is a DAW for the Linux desktop, started from an empty repository. Hard requirements:

- **Wayland only.** No X11 or XWayland in the core app.
- **PipeWire + WirePlumber** for audio and MIDI. No JACK or ALSA backends.
- **Our own client-side window decorations.** No libdecor and no toolkit-supplied decorations.
- **Native MCP from day one**, used both for Claude interaction and as the interface for an automated test harness.

Decisions made at the start:

- Plugins: an in-app generic parameter UI first. Native plugin GUIs come later through a separate out-of-process bridge, which may use XWayland.
- MVP covers audio + MIDI.
- License: **AGPL-3.0-or-later**.

Development machine at the start: Fedora 44, KDE Plasma Wayland, PipeWire 1.6.9, wayland-protocols 1.49, Vulkan 1.4, rustc/cargo 1.99, gcc 16, clang 22, meson, cmake. `lilv-devel` 0.26.4 (for LV2) and `mesa-vulkan-drivers` 26.2.4 (lavapipe, for deterministic headless rendering) are installed (`pkg-config lilv-0` resolves to 0.26.4).

## Language decision: Rust

| Option        | Verdict                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| ------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Rust**      | **Chosen.** No GC, so the real-time audio thread is deterministic. Memory and thread safety cover the riskiest part of a DAW: lock-free sharing between the UI and RT threads. Mature crates for every requirement: `pipewire` (pipewire-rs, maintained under freedesktop), `wayland-client`/`wayland-protocols`/`smithay-client-toolkit` (pure Rust, no libdecor), `wgpu` (Vulkan), `rmcp` (official MCP Rust SDK), `clack-host` (CLAP), `serde`/`schemars` (JSON plus JSON Schema for MCP tools), `symphonia`/`hound`/`rubato` (audio I/O and resampling). Cargo makes a test harness easy. |
| C++           | Industry standard (Ardour, JUCE). No memory safety, the build system is heavy, and MCP/JSON support is weak.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| C             | Same problems as C++, plus far more hand-written plumbing.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| Odin          | Pleasant for custom UI and has Vulkan bindings. No PipeWire, Wayland-protocol or MCP libraries, so all of that would be hand-bound. The ecosystem is too small for this scope.                                                                                                                                                                                                                                                                                                                                                                                                                |
| Go            | GC pauses and cgo call overhead in the RT callback are disqualifying.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| SBCL          | Stop-the-world GC makes RT audio impractical.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| Node / Python | Not viable for RT DSP. Fine as _external_ MCP test-client scripts.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |

## Architecture

### Core principle: one command bus

Every mutation of the project is a serializable `Command` (serde plus `schemars` JSON Schema) applied by `dawkey-core`. The GUI, MCP, keyboard shortcuts and tests all go through the same path. This has three effects:

- MCP tool schemas are **generated from the command types**, so they can't drift from the app.
- Undo/redo is uniform. Each MCP tool call is wrapped in one undo group, so the user can undo an AI edit in a single step.
- Tests exercise exactly what users do.

### Threads

- **Main/UI thread:** a `calloop` event loop that handles Wayland events, UI layout and rendering, and applies commands to the model.
- **PipeWire RT data thread:** the engine `process()` callback. No allocation, no locks, no syscalls. Enforced with `assert_no_alloc` in debug builds.
- **PipeWire main loop thread** (`pw_thread_loop`): registry, metadata, port and link management.
- **MCP thread:** a tokio runtime running the `rmcp` server. It sends commands to the main thread over a channel and awaits the results.
- **Disk I/O thread:** streams audio from disk during playback and writes recordings to disk.
- **Worker pool:** waveform peak building, plugin scanning, file decoding, offline export.

### Model ↔ engine sync (RT-safe)

1. The model compiles to an immutable `ProcessGraph`.
2. The graph is pushed to the RT thread over an `rtrb` SPSC queue.
3. The RT thread swaps graphs at a block boundary and returns the old graph on a second queue, so it is freed off the RT thread.
4. Parameter and automation changes travel as timestamped events, and blocks are split at event times for sample accuracy.
5. Meters, transport position and xrun counts flow back through `triple_buffer` and atomics.

### Audio backends (trait `AudioBackend`)

- **PipeWire.** Dawkey registers as a `pw_filter` node with dynamic audio and MIDI ports. Node props: `node.name=dawkey`, `media.class`, `media.role=Production`, `node.latency`.
  - Timing is sample-accurate from `spa_io_position`.
  - Latency is reported through latency params.
  - MIDI uses SPA control ports (UMP-aware in PipeWire 1.4+).
  - WirePlumber stays the policy owner. Dawkey reads default devices from the `default` metadata and creates the links itself. WirePlumber's stream policy can't link a `pw_filter` node (it has no audio adapter: "no usable format found"), so, like JACK clients, Dawkey's router links its outputs to `default.audio.sink` and follows changes. User-chosen routing will use the same mechanism.
  - pipewire-rs has no `pw_filter` wrapper, so the filter uses `pipewire-sys` FFI in one small unsafe module. The router uses pipewire-rs on its own connection.
  - If the graph runs at a different rate than the project, Dawkey outputs silence, holds the transport, and reports `rate_mismatch`. It never forces the system rate.
- **Offline.** Renders as fast as possible to a buffer or file. Used for export and tests.
- **Virtual clock.** The engine advances only when told (`advance(frames)`). This makes MCP-driven tests deterministic without PipeWire.
- **Media (M1):** imported audio is decoded and resampled to the project rate once, and held in memory. Streaming from disk comes with recording in M4. Sources live in a pool outside undo: undoing an import removes the clip, not the source.

### Wayland shell (`dawkey-wayland`, no libdecor)

- Uses `wayland-client` + `wayland-protocols` + `smithay-client-toolkit` for the registry, seat and xdg-shell helpers only. Never `sctk-adwaita` or any decoration crate.
- **Client-side decorations:**
  - Dawkey draws its own titlebar, buttons, shadow and rounded corners.
  - Moving and resizing use `xdg_toplevel.move/resize`, with edges hit-tested in our UI.
  - Right-clicking the titlebar calls `show_window_menu`.
  - `set_window_geometry` excludes the shadow, and the input region is set for the resize margins.
  - Shadows and rounding are dropped when the window is `maximized`, `fullscreen` or `tiled_*`.
  - If `zxdg_decoration_manager_v1` exists (KDE has it), Dawkey requests `client_side` mode. The protocol requires the client to honour the compositor's configure, so if the compositor forces server-side, Dawkey hides its own titlebar.
- **Protocols:**
  - `fractional-scale-v1` + `viewporter` for HiDPI
  - `cursor-shape-v1`
  - `relative-pointer` + `pointer-constraints` for endless knob and fader drags
  - `data-device` for audio-file drag-and-drop
  - `primary-selection`
  - `xdg-activation`
  - `idle-inhibit` while playing or recording
  - `xdg-dialog`, `xdg-toplevel-icon`
  - `presentation-time` / `fifo` / `commit-timing` for frame pacing
- **Input:** keyboard through `xkbcommon`.
- **File dialogs:** xdg-desktop-portal FileChooser via `ashpd` (D-Bus), with no GTK.

### Rendering and UI

- **`dawkey-render`:** a `wgpu` (Vulkan) 2D batch renderer. It draws instanced SDF rects, rounded rects and lines, waveform and meter primitives, and a glyph atlas.
  - Text uses `cosmic-text` (shaping and fallback) + `swash` (rasterization).
  - It can render to a swapchain surface **or an offscreen texture** (headless).
- **`dawkey-ui`:** our own retained widget tree with flex/grid layout, theming, damage tracking, and a **stable widget ID for every widget**.
  - The tree exports an accessibility-style snapshot (role, label, bounds, value, enabled). MCP exposes it so tests and Claude can target widgets by ID instead of pixels.
  - The UI never touches Wayland directly. A `Platform` trait supplies input events and presents frames, implemented by `WaylandPlatform` and `HeadlessPlatform`.

### MCP (`dawkey-mcp`, `rmcp`)

**Transports:**

- `dawkey mcp --headless [project]`: stdio, on the virtual clock unless given `--audio pipewire`, no window, no PipeWire required. This is the test harness and CI mode.
- The running GUI listens on a Unix socket at `$XDG_RUNTIME_DIR/dawkey/<pid>.sock` (mode 0600, so filesystem permissions are the auth).
- `dawkey mcp --attach [pid]` is a stdio↔socket proxy, so Claude Code (via a `.mcp.json` entry) can drive a live session.
- Streamable HTTP on localhost with a token is an opt-in setting (done after the MVP): the window serves `http://127.0.0.1:<port>/mcp` when the preferences say so, and `dawkey mcp --headless --http ADDR` serves it instead of stdio.

**Tools** (generated from `Command` where possible; all carry `readOnlyHint`/`destructiveHint` annotations):

- **project:** new, open, save, get_state, undo, redo
- **transport:** play, stop, locate, record, loop, set_tempo, set_time_signature
- **tracks/clips:** add_track, remove_track, import_audio, add_clip, move_clip, split, write_midi_notes
- **mixer/routing:** set_gain, pan, mute, solo, sends, insert_plugin, set_param, list_pw_ports, connect
- **render:** `render_range`. Runs offline and returns peak, RMS, LUFS, a content hash and coarse spectrum bins. It can also return the WAV as a resource.
- **engine:** dsp_load, xruns, latency, `advance_clock` (virtual audio backend only)
- **ui:**
  - `ui_tree`
  - `ui_screenshot` (PNG returned as MCP image content)
  - `ui_click(id|x,y)`, `ui_drag`, `ui_scroll`
  - `ui_key`, `ui_type`
  - `ui_wait_idle`

**Resources:** `dawkey://project`, `dawkey://tracks/{id}`, `dawkey://meters`, with subscriptions for change notifications.

### Plugins

- **MVP:** CLAP via `clack-host` (git dependency, pinned rev) and LV2 via `lilv` (`lilv-devel`). Scanning runs in a subprocess so a crashing plugin can't kill the scan. Both get the generic parameter UI and automation lanes. Plugin delay compensation (PDC) is handled in the graph compiler.
- **Post-MVP:** a `dawkey-plugin-host` out-of-process bridge (shared-memory audio, socket control) for crash isolation and native plugin GUIs. This bridge process is the _only_ place XWayland may be used. Then VST3, through the `vst3` crate's bindings (generated from Steinberg's VST 3.8 SDK headers, MIT-licensed since October 2025), with a host layer of Dawkey's own.

### Files

- **Project:** a `Name.dawkey/` bundle holding `project.json` (versioned schema with migrations), `audio/`, `peaks/` and `backups/`, plus autosave and crash recovery.
- **Audio I/O:** decode with `symphonia`, write WAV with `hound`, write FLAC with `flacenc`, resample with `rubato`. No libsndfile is needed.

## Workspace layout (Cargo workspace)

As planned. Some crates were added or split since (`dawkey-media`, `dawkey-studio`, `dawkey-text`, `dawkey-test-plugins`); [ARCHITECTURE.md](ARCHITECTURE.md) lists them as they are.

```text
dawkey/
  Cargo.toml                     # workspace, AGPL-3.0-or-later, lints
  crates/
    dawkey-core/                 # model, Command, undo, serialization, graph compiler
    dawkey-engine/               # RT graph, DSP nodes, transport, metering (no_alloc)
    dawkey-backend-pipewire/     # pw_filter node, ports, metadata, links
    dawkey-dsp/                  # built-ins: gain/pan, EQ, comp, polysynth
    dawkey-plugins/              # CLAP + LV2 hosting, scanner
    dawkey-render/               # wgpu 2D renderer, text
    dawkey-ui/                   # retained widgets, layout, theme, a11y tree
    dawkey-wayland/              # Wayland platform + CSD
    dawkey-app/                  # views: arrange, mixer, piano roll, browser, titlebar
    dawkey-mcp/                  # rmcp server, tools/resources, transports
    dawkey-testkit/              # MCP test client, golden audio/image helpers
  src/main.rs                    # `dawkey` binary: gui | mcp --headless | mcp --attach
  tests/                         # end-to-end scenarios driven over MCP
  .mcp.json                      # lets Claude Code drive a dev build
  CLAUDE.md
```

## Milestones

1. **M0 Skeleton.** ✅ Done.
   - Workspace, CI (fmt, clippy, test) and license headers.
   - `dawkey-core` Command bus + undo, project save/load.
   - `dawkey mcp --headless` with `get_state`, `add_track` and `undo`.
   - First `dawkey-testkit` end-to-end test over stdio.
2. **M1 Engine.** ✅ Done.
   - Offline and virtual-clock backends, then the PipeWire `pw_filter` backend.
   - Transport, file playback, metering, `render_range`.
   - Golden-audio tests.
3. **M2 Wayland shell + renderer.** ✅ Done. Pulled forward from later milestones: `ui_click` (M3), and the GUI's MCP socket with `mcp --attach` (M7), so the live window is testable from the start.
   - Window and full client-side decorations (move, resize, window menu, maximize/tile states).
   - Fractional scale, cursor shapes, the wgpu renderer and text.
   - `HeadlessPlatform`, with `ui_tree` and `ui_screenshot`.
4. **M3 Core UI.** ✅ Done.
   - Arrange view, clips with waveforms, transport bar, mixer.
   - File drag-and-drop and portal dialogs.
   - `ui_click`/`ui_drag`/`ui_key` input injection.
   - Follow-ups (done after M5):
     - Trimming: dragging a clip's start or end edge (`clip/<id>/start|end`, a horizontal-resize pointer) trims it on release with one `trim_clip`, snapped to beats unless Shift is held. What the clip plays stays in place: an audio clip's source offset follows its start, between the source's ends; a MIDI clip's notes and controller changes shift, notes that start before a new start go, and the clip starts with each controller's value there.
     - Splitting: S or the toolbar's Split button splits the selected clip at the playhead (`split_clip`) and selects the second part. Notes go to the part they start in; the second part starts with each controller's value at the split. Splitting an audio clip renders sample-identical.
     - Copying: Ctrl+C copies the selected clip and Ctrl+V pastes it at the pointer, onto the track under it if that's a track of the clip's kind, else onto the clip's own track (at the playhead when the pointer isn't over the timeline). Ctrl-dragging a clip leaves it in place and drops a copy (`duplicate_clip`); Ctrl-dragging its end edge lays copies end to end up to there, the last one cut short (`repeat_clip`). In the window clips never overlap: a moved, copied or pasted clip goes to the nearest space it fits in (`Track::free_start`), flush against the clip in the way, and edges and repeats stop at the next clip. Over MCP, `move_clip`, `duplicate_clip` and `repeat_clip` place clips exactly, and recordings still stack as takes.
     - A "save changes?" dialog (Save, Don't save, Cancel; Return and Escape) before opening another project or closing the window over unsaved changes, whether the close comes from our title bar or the compositor. Saving a project without a file asks where first, then carries on.
5. **M4 Recording.** ✅ Done. Notes: the automatic latency estimate (two quanta plus the output device delay) is rough, and `set_record_offset` corrects it; a loopback measurement tool would be better. Input ports, disk-writer thread, loop and punch recording, latency compensation.
   - Follow-ups (done after M5):
     - Input monitoring: an armed audio track's meter shows its input. With monitoring on (`set_track_monitor`, the header's "In" toggle) it also plays the input, through its plugins, delay compensation, fader and pan, along with its clips, playing or stopped. Off by default, since a microphone near speakers feeds back. Armed MIDI tracks already play their input.
     - Takes and comping: a recording over earlier clips is placed as a take (`take` on `add_clip`/`add_midi_clip`), in a take lane (`Clip::lane`) above what it overlaps, which goes silent (muted, split at the take's edges) where it plays, so punching in replaces. Earlier loop passes are muted takes in lanes of their own. `comp_take` makes a take lane play over a range and silences the rest of the track there, splitting clips at the edges; one undo step. In the arrange view, a track with takes has a "Takes" toggle that shows its lanes under it (the track's own lane then shows only what plays): clicking a take plays it, dragging across a lane plays that lane over the range (snapped to beats unless Shift is held), and clicking a lane's header plays that take wherever it has clips.
     - Cached recordings: takes of a project that was never saved (in `$XDG_CACHE_HOME/dawkey/recordings/<pid>`) are deleted once saving copies them into the project, or when the project is replaced or Dawkey exits. Folders left by an instance that crashed are deleted at startup once nothing was written to them for a week, so their takes can still be rescued by hand until then.
6. **M5 MIDI.** ✅ Done. PipeWire MIDI ports, MIDI clips, piano roll, built-in polysynth, MIDI recording.
   - One `midi_in` port (`8 bit raw midi`; MIDI 1.0 and UMP control events are both parsed). There's no default MIDI device in WirePlumber, so while a MIDI track is armed the router links every MIDI bridge output except ALSA's "Midi Through". Armed MIDI tracks play live input through their synth, even when stopped.
   - Note times are frames in the model and beats (quarter notes) in the commands. Like audio clips, MIDI clips and notes stay at their frame positions when the tempo changes.
   - The synth (`dawkey-dsp`) is 16-voice subtractive: polyBLEP saw/square, triangle, sine, ADSR, a resonant low-pass opened by the envelope, and sustain pedal. Its voices carry over graph swaps, so edits during playback don't cut notes off.
   - MIDI recording shifts takes by the same latency estimate as audio; each loop pass with notes is a take, earlier ones muted.
   - Follow-ups (done after M6):
     - Tempo map: a start tempo plus changes at beats (`set_tempo` with `at_beats`, `remove_tempo_change`, `set_tempo_map`). Notes stay stored in frames; a tempo edit re-times every MIDI clip and note so they keep their beat positions. Audio clips, automation, loop and punch keep their times. The ruler, grids and snapping follow the map.
     - Controllers: clips hold pitch bend, CCs (0–119) and channel pressure (`set_midi_controls`), recorded from live input and played to the synth (bend ±2 semitones, mod wheel vibrato, volume, expression, sustain) or to plugins as MIDI. Live input passes every channel message to plugins.
     - Chasing: when playback starts, jumps or loops, notes already under the playhead start and each controller's latest value is sent (or its default).
     - `import_midi` (and dropping or opening `.mid` files) reads Standard MIDI Files with `midly`: one MIDI track per file track and channel, with notes and controllers, one undo step. It takes the file's tempo map and time signature by default when the project has no clips yet.
     - Piano roll: a lane under the grid shows velocities (drag a bar, or draw across them) or one controller (double-click adds a change, drag moves it, drawing replaces changes on the grid, Delete removes the selected one); its header cycles through velocity, the common controllers and any others the clip uses. Dragging on empty grid selects with a rectangle (Shift/Ctrl adds).
     - Devices per track: live MIDI is tagged with the device it came from. Each MIDI track can listen to one device (`set_track_midi_input`) and send its notes to one device on a channel instead of its instrument (`set_track_midi_output`). The router gives each device its own Dawkey port from a pool of eight each way (`midi_in_N`, `midi_out_N`, made before the filter connects because `pw_filter` can't add ports safely while its data thread runs), labelled with the device's name in `port.alias`; the generic `midi_in` stays for patching by hand. Stopping (or jumping) resets the controllers clips set, so a clip's sustain pedal never holds notes on a device.
   - More follow-ups:
     - Time signature map: a time signature at the start plus changes at bars (`set_time_signature` with `at_bar`, `remove_time_signature_change`, `set_time_signature_map`). Changes move where bars start, nothing else; the rulers, snapping, new clips and fitting clips to bars follow the map, and `import_midi` takes every time signature in the file.
     - Tempo ramps: a tempo change with `ramp` glides from the previous tempo, in a straight line over time (closed-form beat ↔ frame conversions, so re-timing stays exact).
     - Channel filtering: a MIDI track can listen to one channel (`set_track_midi_input` with `channel`; `track/<id>/midi_in_channel` cycles it), for playing and recording.
     - Poly pressure and program changes are clip controllers like CCs: recorded, imported from `.mid` files, edited with `set_midi_controls`, chased and sent to plugins and devices. Programs don't reset when playback stops, and only chase once a clip has set one.
     - MIDI out of plugins: CLAP note output ports and LV2 MIDI atom outputs. What a plugin sends goes to the next plugin that takes notes; plugins that don't take notes pass it on, so an arpeggiator can play an instrument. On a track with a MIDI device, what the MIDI effects at the start of the chain send goes to the device. A plugin without audio outputs leaves the track's audio alone. `list_plugins` marks MIDI effects (`midi_output`, kind `midi_effect`).
     - MIDI to apps: other apps' MIDI ports (JACK clients, PipeWire filters) are listed with the devices as `node:port` and can be picked for a track's input or output; "every device" still means hardware only.
     - Delay compensation for external MIDI: MIDI sent to devices is held back by the graph's plugin latency, so the device plays in time with the delayed audio.
   - Last follow-ups:
     - Tempo and time signature lanes: the arrange view's "Tempo" toggle (or clicking the toolbar's BPM or time signature) shows a tempo lane and a time signature lane under the ruler. Tempo points drag up and down (BPM; Ctrl for fine steps) and along the beat grid, double-clicking the lane adds one at the tempo under the pointer, and the header changes the selected point (or the one at the playhead): −/+ BPM, Ramp, remove. Time signature chips drag from bar to bar, double-clicking adds one, and the header steps the numerator and denominator. Up/Down and Delete work on the selection. Each edit is one `set_tempo`/`set_tempo_map`/`set_time_signature`/… command, so one undo step, and drags commit on release.
     - Device latency: a track's MIDI output has a latency (`set_track_midi_output`'s `latency_ms`, the piano roll's "Lat" slider), how long the device takes to sound. It counts toward delay compensation like a plugin's latency: the rest of the mix is delayed to match the slowest, and each device's MIDI is held back by the graph's latency less its own, so no look-ahead is needed and live playing works too.
     - MIDI 2.0 per-note controllers: per-note pitch bend (±48 semitones) and registered and assignable per-note controllers are clip controllers with 16-bit values (`note_pitch_bend`, `note_controller`, `note_assignable`), parsed from UMP input, recorded, chased, edited with `set_midi_controls` and in the piano roll's lane. The built-in synth plays per-note bend, volume (7), expression (11) and brightness (74); CLAP plugins get them as note expressions (tuning, volume, pan, expression, brightness, vibrato); devices get MIDI 2.0 UMP packets, which PipeWire converts (or drops) for MIDI 1.0 gear. LV2 plugins don't get them (their MIDI is 1.0).
   - Follow-ups (done after M4's last items):
     - Typing a tempo: the tempo lane header's BPM is a text field (`tempo/value`). Clicking it selects the value of the selected tempo point (or the one at the playhead); digits and a point type over it, Backspace deletes, Return, Tab or a click elsewhere sets it (one `set_tempo`), and Escape leaves it. While a field is being typed in it gets every key. MCP's `ui_type` types text, a key per character.
     - Per-note management: MIDI 2.0 per-note management (detach, reset) is parsed from UMP input and travels like the per-note controllers, as status 0x70 (its UMP opcode, 0xF, would collide with MIDI 1.0's system messages). The built-in synth resets the key's per-note controllers, and detaching leaves the key's sounding voices with the values they have; CLAP plugins get a reset as every note expression at its default; devices get the packet; recording turns a reset into the key's per-note controllers going back to their defaults. `advance_clock` takes `midi_management` to feed it.
     - Note expressions that plugins send (CLAP) become per-note controllers again (tuning, volume, pan, expression, brightness, vibrato; pressure as poly pressure), so they go down the chain to the next plugin and out to devices like the rest of a MIDI effect's output. (clack 0.2 doesn't recognize the "expression" expression when reading; the host maps the raw IDs itself.)
     - Device latency from PipeWire: the router watches the Latency param of MIDI devices' input ports (ALSA's MIDI bridge reports a cycle) and the reported time is added to a track's own `latency_ms` for delay compensation; the graph is rebuilt when it changes (a new quantum). `get_engine_status` lists it as `midi_output_latency_ms`, and the piano roll's latency slider shows it after the set value.
   - Last items:
     - MIDI input latency from PipeWire: the router also watches the Latency param of MIDI devices' output ports (how long ago what comes out of them was played; a cycle for ALSA's MIDI bridge). Recorded MIDI is placed earlier by what playback takes to be heard (a cycle plus the output device's delay), plus each note's own device's reported latency instead of the audio estimate's input cycle (still used for devices that report nothing), plus plugin latency and `set_record_offset`. Notes from different devices on one track each move by their own. `get_engine_status` lists `midi_input_latency_ms` and the resulting `midi_record_latency_ms` per device.
     - Typing other values: the time signature lane header's numerator and denominator are text fields (`meter/numerator/value`, `meter/denominator/value`; Tab goes from the numerator to the denominator), and double-clicking a MIDI output's latency slider (`track/<id>/midi_out_latency`) turns it into a field for a value in ms. Each sets one `set_time_signature` or `set_track_midi_output`.
7. **M6 Plugins.** ✅ Done. CLAP and LV2 hosting, scanner, generic param UI, automation, PDC.
   - Insert chains on every track; on a MIDI track an instrument first in the chain replaces the built-in synth. Each track runs sources (at unity) → inserts → delay compensation → fader and pan.
   - CLAP through `clack-host` 0.2 (crates.io, no git pin needed). LV2 through `lilv`, with URID map/unmap, options and bounded block length; plugins that need more (worker, state, CV ports) are reported as unsupported.
   - Scanning: each `.clap` file in its own `dawkey scan-plugin` process (a crash only fails that file), LV2 from its Turtle files in-process; cached, and rescanned when plugin files change.
   - Parameters, bypass and automation (gain, pan, plugin parameters) are commands; automation is sample-accurate for CLAP and split at changes for LV2, the fader ramps every 32 frames. Delay compensation lines tracks up to the slowest chain; offline renders come out aligned, and recordings shift by the extra latency.
   - Follow-ups (done after M5's last items):
     - Plugin state: what a plugin saves of itself beyond its parameters (CLAP `state`; LV2 `state:interface`, its properties kept with URIs) is kept per slot in the project file, outside the undo history. It's captured on save, before offline renders, when a slot is removed (so undo brings the plugin back as it was) and when a CLAP plugin marks its state dirty (which counts as an unsaved change), and restored when an instance loads. `get_plugin` reports its size.
     - Transport and tempo: plugins get the transport every block (playing, recording, position in samples, seconds and beats, tempo and its ramp, bar, time signature, loop): CLAP as the process call's transport event, LV2 as `time:Position` objects on the atom input that takes them, whenever it moves.
     - Parameter text: CLAP plugins format their own values (`value_to_text`); LV2 values show their scale point's label or the port's unit (`units:unit`). `get_plugin` has `text`, and the editor shows it.
     - Master and buses: the master is track 0 and takes plugins, a fader, mute and automation like any track. Buses (`add_track` kind `bus`) mix the tracks sent to them (`set_track_output`, the mixer's output button) through their own plugins and fader. Tracks render in dependency order; delay compensation lines up everything arriving at each bus and the master (so latency adds up along a path), and automation is read at the audio's latency where it applies. Soloing follows the routing.
     - Sidechains and multi-out: `set_plugin_sidechain` feeds a track's signal (after its fader, muted or not) into a plugin's sidechain input (CLAP: its first other input port; LV2: `lv2:isSideChain` ports, or the inputs after the first two), delayed to line up when it can be. `set_plugin_output` sends a plugin's other outputs (CLAP ports; LV2 outputs after the first two, in pairs) to buses; outputs that go nowhere are silent. Loops are refused. The plugin editor has buttons for both.
     - Recording automation: with "Auto" on while playing, moving a fader, a pan control or a plugin parameter writes its lane at the playhead, replacing what was there while the control is held ("touch"); holding it still keeps writing. One undo step per gesture. `write_automation` does the same over MCP. Faders and pan show their automation's value at the playhead.
     - Scanning in the background: the window scans plugins on a worker thread; the picker says it's scanning, and slots load when the scan is done. Headless instances wait for scans, as before.
   - Last items:
     - Typing parameter values: double-clicking a parameter's slider in the plugin editor turns it into a text field; what's typed is read the way the plugin shows its values (CLAP plugins read it themselves with `text_to_value`; for LV2, a scale point's label in any case, or a number with or without the port's unit). Ctrl-click sets the default (double-click used to). Over MCP, `set_plugin_param` takes `text` instead of `value`; text the plugin can't read is a tool error that shows a value it can.
     - Latch and write automation modes: the button next to "Auto" (`transport/automation_mode`) picks how moving a control writes its lane. Touch writes while it's held. Latch keeps writing the value it was left at until the transport stops. Write writes every lane, at its value where playback started until a control is moved, from where playback starts until it stops, then goes back to touch so the next pass doesn't flatten everything again. A latch or write pass is one undo step.
     - LV2 files in state: plugins get `state:mapPath`, `state:makePath` and `state:freePath`. Paths under the project are stored relative to it, so a project can move, and files a plugin saves of its own go in the project's `plugins/<slot>/` folder (before the first save, in the recordings cache; saving writes them into the project). Files of removed slots stay in the folder.
     - Late sidechains: when a sidechain comes through more latency than the plugin's own input (a plugin with latency on the source track), the plugin's track is held back to match before that plugin, and the rest of the mix waits for it, so the two line up. Notes going to that plugin aren't held back.
     - LV2 hosting fixes, found by loading every LV2 plugin on a Fedora desktop: plugins without extensions (a null `extension_data`, as in the SWH plugins) no longer crash Dawkey (lilv-sys calls it unchecked); ports are connected before `activate`, which some plugins read them in; and plugins whose ports lilv can't read from their Turtle files are refused (scan failures) rather than run with nothing connected. Plugins that crash by themselves while instantiating (QMidiArp's LFO and Seq on Fedora) still take Dawkey down with them until plugins run out of process.
   - Not yet: native plugin GUIs (post-MVP; until then a plugin's state only changes by itself).
8. **M7 MVP polish.** WAV/FLAC export, autosave and recovery, preferences, theming. GUI attach socket + `--attach` (done in M2).
   - Export: `export_audio` over MCP, and the toolbar's "Export…" (Ctrl+E) with a dialog of format (WAV or FLAC), samples (16 or 24-bit, dithered, or 32-bit float for WAV), sample rate (the project's or another, converted), range (the song, to where its last clip ends, or the loop) and a tail for reverbs to ring out. The render streams to the file a block at a time (up to 6 hours), on a worker thread in the window with its progress in the status bar and a button to cancel; a cancelled or failed export leaves what was at the path. A float WAV export is sample-identical to `render_range`.
   - Autosave and recovery: every couple of minutes (preferences), unsaved changes go to an autosave in the bundle (`backups/autosave.json`) or, for a project never saved, beside its recordings in the cache. Saving, opening another project and exiting cleanly delete it. After a crash, opening the project (or starting with none, for a never-saved one) offers to recover the changes or discard them.
   - Preferences (`$XDG_CONFIG_HOME/dawkey/preferences.json`, from the status bar's "Preferences" or Ctrl+,): the audio backend and auto-connect (from the next start; the command line still wins), the default record offset, autosave on or off and how often, the theme and a UI scale, and extra CLAP and LV2 folders with a rescan. Headless instances ignore them.
   - Theming: a light palette beside the dark one; by default it follows the desktop's light or dark setting (the settings portal), and the preferences can pin one.
9. **Post-MVP.** Out-of-process plugin bridge with native GUIs, VST3, sandboxing, MIDI 2.0/UMP, opt-in HTTP MCP.
   - Plugin bridge (done): each plugin instance runs in a process of its own (`dawkey plugin-host`), on by default in the window (preferences) and with `--bridge-plugins` headless. Audio, events, parameters, transport, sidechains and other outputs cross in shared memory a block at a time; the audio thread never waits for the plugin, which therefore adds one engine block of latency (reported to delay compensation, and grown if blocks get longer). Offline renders and the virtual clock wait for each block instead. A plugin that crashes (or hangs) takes only its slot down: its audio passes through, `get_plugin` and the editor say so, and `reload_plugin` (or the editor's "Reload") starts it again from its saved state. QMidiArp's LFO and Seq, which abort while loading, now fail alone.
   - Native GUIs (done): a bridged plugin with a GUI gets a "GUI" button in its editor (and `show_plugin_gui` over MCP), which opens it in a window of its own from its process: CLAP GUIs embedded in an X11 window (through XWayland), or floating when that's what they do; LV2 `ui:X11UI` UIs embedded the same way. What's changed on them (and what plugins change themselves, like CLAP parameter output events) goes into the project as `set_plugin_param`, one undo step per gesture, written as automation while "Auto" writes. Tried with LSP's CLAP and LV2 compressors, x42-eq and ZamComp.
   - LV2 UIs and the worker (done): LV2 UIs get the plugin's output control ports (meters, gain reduction) and atoms both ways: what a UI sends the plugin (`patch:Set` for a file, the messages LSP's, x42's and DPF's UIs talk to their plugins with) goes into its atom input at the start of the next block, and what the plugin writes to its atom outputs reaches the UI while it's open (spectra, scopes, graphs). `ui:portMap` too, and a GUI that doesn't say its size (DPF's: the Zam plugins) gets the size of the window it made. The LV2 worker (`work:schedule`): work runs on a thread of its own, and its responses reach the plugin after `run`; for offline renders and the virtual clock it's done as it's scheduled, so they come out the same every time, and restoring a state can schedule work too. Plugins that require it load now (setBfree, the rncbc synths, midimap, zeroconvo), and LSP's and Fabla's use it. Atom ports get the buffer size they ask for (`rsz:minimumSize`, up to 64 MiB; it was 16 KiB, which left out what LSP's and x42's plugins send their UIs). A plugin says its state changed with `state:StateChanged`, or by its UI sending it a change, which counts as an unsaved change like a CLAP plugin's `mark_dirty`. Checked by loading every installed LV2 plugin (in-process and bridged) and by opening LSP's Spectrum Analyzer and Compressor, x42's Simple Scope and EQ, and ZamComp.
   - Gtk LV2 UIs (done): a plugin with no X11 UI but a `ui:GtkUI` (Gtk 2) or `ui:Gtk3UI` shows it in a Gtk window of its own, from its process: Calf's (all 52), abGate, amsynth, IR, sapistaEQ and ll-plugins'. Gtk is loaded at run time (Dawkey doesn't link it), its main loop runs in the plugin process's poll loop, and Gtk UI libraries stay loaded once used (`ui:makeResident`). Checked by opening, closing and reopening every Gtk UI installed here: all work but ll-plugins' Peak Meter, whose GUI crashes in its own drawing code (its process stops alone). Suil couldn't help: it only wraps X11 UIs into Gtk and Qt hosts.
   - MCP over HTTP (done): an opt-in setting in the preferences ("Serve over HTTP", a port, default 7381) serves MCP's streamable HTTP transport at `http://127.0.0.1:<port>/mcp`, starting, moving and stopping as it's changed. Only loopback addresses are served; every request needs `Authorization: Bearer <token>`, with the token kept owner-only in `$XDG_CONFIG_HOME/dawkey/mcp-token` (made on first use; "New token" replaces it, and the old one stops working at the next request), and requests naming another host (DNS rebinding) or coming from web pages elsewhere are refused. Clients share the studio like the socket's: each call is one undo step. `dawkey mcp --headless --http ADDR` serves it without a window (the token from `$DAWKEY_MCP_TOKEN`, the URL printed on stdout, until SIGINT or SIGTERM), which the end-to-end tests use.
   - Real-time scheduling for plugin processes (done): a plugin process's audio thread runs one priority below Dawkey's own (PipeWire's data loop, read from `/proc`), so a plugin that's too slow makes its own block late (silence) instead of holding up the mix; it used to ask for a fixed 70, above Dawkey's. It's real-time only when Dawkey's audio is (not on the virtual clock). The kernel is asked first (rlimits, e.g. an `audio` group); otherwise RealtimeKit, as PipeWire does (with `RLIMIT_RTTIME` set as rtkit requires; priorities capped at its maximum, 20 by default), and in a Flatpak the desktop portal's Realtime interface. rtkit is asked from a helper thread, so the audio thread never waits, and again for about a minute while it's busy: it grants a user only so many requests every 20 seconds and only so many real-time processes at once (here about 11 plugin processes beside PipeWire's), so without rlimits the plugins beyond that run without real-time scheduling, with a warning (now that instances of a plugin file share a process, that's plugin files, not instances). Checked on Fedora: `SCHED_FIFO` 54 under Dawkey's 55 with rlimits, `SCHED_RR` 19 under 20 through rtkit without them, and every installed LV2 plugin loaded bridged through rtkit (none died of `RLIMIT_RTTIME`).
   - Shared plugin processes (done): the instances of one plugin file (a `.clap` file, an LV2 bundle) share a process, so a project with many plugins runs far fewer processes (every plugin installed here, 855 of them, in 233), loads faster (one LV2 world per process, not per instance) and asks rtkit for far fewer real-time processes. Each instance keeps its own shared memory and audio thread there, so they run in parallel as before, and GUIs open side by side. A crash takes down that plugin's instances, which a buggy plugin would likely crash anyway; Gtk 2 and Gtk 3 UIs never meet in one process. The preferences (or `--bridge-plugins=separate`) can still give each instance a process of its own. Checked by loading every installed plugin bridged and shared and playing through them (only QMidiArp's LFO and Seq die, as before), and by opening two GUIs each from LSP's CLAP file, Calf's Gtk 2 bundle and x42-eq at once.
   - Sandboxed plugin processes (done): before loading anything, a plugin process enters a sandbox, on by default (the preferences, or headless `--no-sandbox`, turn it off). It guards against a plugin that misbehaves, or was made to: it may read anything (samplers load files from wherever they're kept), but write only where plugins keep their own things (the XDG config, cache, data and state folders, `~/.lv2`, temporary folders, devices), so it can't damage the user's files; it can't reach the network, so it can't send them anywhere; and it can't debug, signal or read the memory of other processes, load kernel modules, mount, make namespaces or use BPF or `io_uring`. Landlock does the filesystem, TCP, Unix socket (only the X server's, the Wayland compositor's and the system bus, for rtkit: not the session bus, PipeWire or Dawkey's MCP socket) and signal parts (kernels without some of it enforce what they have); seccomp the system calls and the other Internet sockets. Files LV2 plugins make for their state now stay in Dawkey's cache while it runs, saved as where they'll be in the project and copied there when saving (a sandbox can't follow a project to wherever it's saved next). Not stopped: what a GUI can do over the X server (watch or type into other X11 windows), and reading files. Checked by loading every installed plugin sandboxed and playing through them, and by opening real GUIs (X11, Gtk 2) in the sandbox.
   - Qt LV2 UIs (done): a plugin whose only UI is `ui:Qt5UI` or `ui:Qt6UI` (QMidiArp here) shows it in a Qt window of its own, from its process; X11 and Gtk UIs are still preferred where a plugin has them. Qt has no C API, so a small C++ shim is built against Qt 5 and Qt 6 where their headers are installed, embedded in Dawkey and loaded from memory when a Qt UI opens; Dawkey doesn't link Qt (the UI's own library brings it). Checked with two QMidiArp Arp UIs from one sandboxed process.
   - GUIs that hold up their process (done): a plugin GUI's modal dialog (QMidiArp's "no `.qmidiarprc`" message, any file chooser) stops its process's main thread until it's answered, which the bridge used to take for a hang and kill the process (with every instance in it). Now replies carry request numbers, and one that's late makes the plugin busy, not dead: Dawkey stops asking it until the late reply comes (questions get Dawkey's own answers meanwhile), a GUI that opens late still counts as shown, and its audio goes on. Only a load that never finishes is a hang.
   - VST3 (done): `.vst3` bundles are found in `~/.vst3`, `/usr/lib/vst3`, `/usr/lib64/vst3` and `/usr/local/lib/vst3` (or `$VST3_PATH`, and folders added in the preferences), each scanned in its own process, and hosted like CLAP plugins: audio with sidechains (a plugin's first aux input) and other outputs (its aux outputs), notes, MIDI out, parameters with automation and the plugin's own text (values are VST3's normalized ones, 0 to 1, so the text is what to read), MIDI controllers through the plugin's MIDI mapping, the transport, latency, MIDI 2.0 per-note controllers as note expressions both ways, state (the component's and the controller's) in the project, and the bridge with its sandbox. What a plugin changes on its controller (a knob on its GUI, a preset loaded there) goes into the project as `set_plugin_param`, and its editor shows embedded through XWayland from its process, run by Dawkey's run loop. The parameters plugins have only to stand for MIDI controllers (JUCE's and Dplug's, by the thousand) are left out of the list. Checked by loading, playing, saving and reopening every VST3 plugin installed here (the Zam plugins, Odin2, Graillon 3), in-process and bridged, and by opening the editors of ZamComp, Odin2 and Graillon 3.
   - Not yet: nothing for LV2 UIs. For VST3: program lists and units (`IUnitInfo`), MIDI learn and MIDI 2.0 mapping (`IMidiLearn`, `IMidiMapping2`), note expressions beyond tuning, volume, pan, vibrato, expression and brightness, the 3.8 Wayland editor, context menus (`IComponentHandler3`), 64-bit processing, and plugins asking to be reloaded when their buses change (`kIoChanged`, `kReloadComponent`); a latency change is picked up when a plugin is next loaded, as for CLAP.

## Verification

- **Unit tests:** `cargo test --workspace` covers the model, commands, undo, the graph compiler and DSP nodes against reference outputs.
- **RT safety:**
  - Debug builds run the engine under `assert_no_alloc`.
  - A stress test drives randomized command streams while rendering and asserts no allocation and no panics on the RT thread.
- **End-to-end over MCP:**
  - `dawkey-testkit` spawns `dawkey mcp --headless` (on the virtual clock) and runs scenarios such as "import file → add track → render 2 s → assert peak/RMS/hash".
  - The same scenarios can run from any MCP client, including Claude Code through `.mcp.json`.
- **UI goldens:** `ui_screenshot` renders headless on lavapipe (deterministic software Vulkan) and is compared to golden PNGs within a tolerance. `ui_tree` assertions check widget state after injected input.
- **Live checks** (manual or Claude-driven via `--attach`) on KDE Plasma:
  - `pw-dump`/`pw-top` show the `dawkey` node, its ports and latency.
  - `wpctl status` shows WirePlumber linking it.
  - Client-side decorations behave correctly when moving, resizing, maximizing, tiling and changing fractional scale.
  - Playback runs with zero xruns at a 128-frame quantum.

## Setup

The build and runtime dependencies are listed in the [README](../README.md#building-from-source).
