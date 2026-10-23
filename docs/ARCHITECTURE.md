# Architecture

This is a map of Dawkey's code for people who want to work on it. [CLAUDE.md](../CLAUDE.md) has the detailed rules, and [PLAN.md](PLAN.md) has the history and the reasoning behind each milestone.

## Principles

- **One command bus.** Every change to a project is a serializable `dawkey_core::Command`, applied through `Session::execute`. The window, keyboard shortcuts, MCP and tests all take this path. MCP tool schemas are generated from the command types, every command is one undo step, and tests exercise exactly what users do.
- **The audio thread never waits.** The real-time code never allocates, frees, locks or blocks. The project is compiled into an immutable graph, which reaches the audio thread through a lock-free queue and is freed on another thread when it's replaced.
- **One app, two front ends.** `dawkey_app::App` (the project, the engine, the UI and the renderer) is the same in the window and in a headless MCP instance. The window adds Wayland; headless adds a virtual window rendered offscreen.
- **Deterministic tests.** A virtual audio clock, offline rendering and a software Vulkan renderer (lavapipe) make audio and screenshots exactly the same on every run.

## Crates

| Crate                     | What it does                                                                                                                                 |
| ------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `dawkey-core`             | The project model, the `Command` bus, undo history, project files, the tempo map                                                             |
| `dawkey-media`            | Decoding (Symphonia), resampling (rubato), reading `.mid` files, analysis (peak, RMS, LUFS, hash), and exporting to WAV and FLAC with dither |
| `dawkey-dsp`              | Built-in real-time DSP: the polysynth that plays MIDI tracks                                                                                 |
| `dawkey-engine`           | Compiles the project into a `Graph`, runs it in the real-time `EngineProcessor`, renders offline, records, and parses live MIDI              |
| `dawkey-plugins`          | CLAP, LV2 and VST3 hosting: scanning, loading, processing, state, GUIs, and the out-of-process plugin bridge and its sandbox                 |
| `dawkey-test-plugins`     | Tiny CLAP, LV2 and VST3 plugins with exact outputs, for tests                                                                                |
| `dawkey-backend-pipewire` | The PipeWire node (`pw_filter`) with audio and MIDI ports, and a router that links it to WirePlumber's default devices and to MIDI devices   |
| `dawkey-studio`           | `Studio`, a running instance: the session, decoded media, plugins, the engine and the audio backend                                          |
| `dawkey-ui`               | The widget tree with stable IDs, flex layout, input routing, a display list (`Scene`) and an accessibility tree                              |
| `dawkey-text`             | Text shaping and rasterizing (cosmic-text) with the embedded Noto fonts                                                                      |
| `dawkey-render`           | The wgpu (Vulkan) renderer, to a window surface or an offscreen image                                                                        |
| `dawkey-app`              | `App`: the views (arrange, mixer, piano roll), dialogs, input handling, preferences, themes and the HTTP MCP server's settings               |
| `dawkey-wayland`          | The Wayland window: client-side decorations, fractional scale, cursors, drag and drop, and portal dialogs                                    |
| `dawkey-mcp`              | The MCP server (rmcp): tools generated from commands, plus the UI and engine tools; stdio, Unix socket and HTTP transports                   |
| `dawkey-testkit`          | `Harness`, the MCP client the end-to-end tests use                                                                                           |

The `dawkey` binary is `src/main.rs`. It opens the window, runs `mcp --headless` or `mcp --attach`, and has two hidden subcommands that Dawkey runs itself: `scan-plugin` (scans one plugin file in a process of its own) and `plugin-host` (runs plugins for the bridge).

Unsafe code is allowed only in `dawkey-backend-pipewire`, `dawkey-wayland`, `dawkey-plugins` and `dawkey-test-plugins`. Every other crate is `#![forbid(unsafe_code)]`.

## From an edit to the speakers

```text
 window input ──┐
 MCP tool call ─┼─▶ Command ─▶ Session::execute ─▶ Edits ─▶ Project (+ undo step)
 test harness ──┘                                            │
                                                             ▼
                                        Studio recompiles the engine Graph
                                                             │  rtrb queue
                                                             ▼
                                PipeWire data thread: EngineProcessor::process
                                 swaps in the new graph at a block boundary,
                                 sends the old one back to be freed
```

1. A front end builds a `Command`. Its `lower` method validates it and turns it into primitive `Edit`s, which the session applies as one undo step.
2. `Studio::execute` sees the session's revision change and compiles a new `Graph`: tracks and buses in dependency order with the master last. Each track runs its sources (or what's sent to it, for a bus), then its insert plugins, its fader and pan, and a delay that lines it up with everything else feeding the same bus (`graph::compensate`).
3. The graph goes to the audio thread over an `rtrb` queue. The audio thread takes it at the start of a block and carries over what must keep running: plugin processors and synth voices (`mix::carry_over`), so editing during playback never cuts notes. The old graph goes back to the control thread to be freed.
4. Meters, the transport position and recorded audio come back through lock-free buffers. Recorded audio is written to WAV files by a recorder thread.

The same graph runs in three places: the PipeWire callback, the virtual clock (time moves only when `advance_clock` is called), and offline renders and exports, which use fresh plugin instances.

## Threads

- **Main thread:** Wayland events, layout, rendering, and applying commands. CLAP plugin instances aren't `Send`, so the `Studio` stays on the thread that made it.
- **PipeWire data thread:** `EngineProcessor::process`, in real time.
- **PipeWire main loop:** ports, links and device changes, for the router.
- **MCP:** a tokio runtime running the rmcp server, which sends work to the main thread and waits for the result.
- **Workers:** plugin scanning, decoding imports, exports and the recorder.

## Plugins

Each format has a provider in `dawkey-plugins` that scans plugins into a `Catalog` and loads them as a control-thread `Instance` plus a real-time `PluginProcessor`. The processor lives in the engine graph; the instance stays in `dawkey-studio`.

In the window, plugins run in processes of their own (`dawkey plugin-host`): one per plugin file by default, or one per instance. The audio goes through shared memory, one block at a time, and control messages go over a socket. The audio thread never waits for a plugin process. It hands over this block and takes back the previous one, which adds one block of latency, so a late block is silence and a crashed process passes the audio through until the plugin is reloaded. Before loading anything, a plugin process enters a sandbox (Landlock and seccomp). Plugins' own GUIs run in these processes, through XWayland: the only place Dawkey uses X11.

What a plugin saves of itself is stored in the project per plugin slot, outside the undo history, and handed back when the plugin is loaded.

## User interface

The UI is rebuilt from `Studio`'s state every frame. `dawkey-ui` lays out a tree of widgets, each with a stable ID, and produces a `Scene` (a display list) that `dawkey-render` draws with wgpu. The same tree gives MCP's `ui_tree`, so clients and tests can find widgets by ID. Clicks become commands through `App::activate`, like every other change.

`dawkey-wayland` draws no decorations of its own; the title bar and the resize borders are widgets, and their actions (`ChromeAction`) are carried out with xdg-shell requests. File dialogs and the light or dark setting come from xdg-desktop-portal.

## Project files

A project is a `Name.dawkey/` folder holding `project.json` (a versioned schema), the audio it uses, plugin files, and `backups/autosave.json` while there are unsaved changes. Audio imported from outside is copied in when saving. Recordings of a project that was never saved go to `$XDG_CACHE_HOME/dawkey/recordings/<pid>` until it is.

## Tests

- **Unit tests** in each crate cover the model, commands, undo, the graph compiler and DSP.
- **End-to-end tests** in `tests/` drive the real binary over MCP with `dawkey-testkit`. See [MCP.md](MCP.md#testing-over-mcp).
- **Golden audio:** hashes of rendered output pin the engine and the synth.
- **Golden screenshots:** `tests/golden/*.png`, rendered on lavapipe.
- **Live PipeWire tests** (`tests/pipewire.rs`) are ignored by default and need a desktop session.
