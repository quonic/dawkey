# Dawkey over MCP

Dawkey has a [Model Context Protocol](https://modelcontextprotocol.io) server built in. Anything you can do in the window, an MCP client can do too: build and edit a project, record, play, mix, load plugins, render and export, and work the window itself by clicking widgets and taking screenshots. AI assistants use it to work alongside you, and Dawkey's own end-to-end tests use it to drive the real program.

The MCP tools are generated from the same commands the window uses, so they can't fall behind the app. Each tool call that changes the project is one undo step, so you can undo anything an assistant did one call at a time.

## Connecting

There are three ways to reach Dawkey.

### A headless instance, on stdio

```sh
dawkey mcp --headless [Song.dawkey]
```

This starts Dawkey with no window and speaks MCP on stdin and stdout. It uses the virtual audio backend by default: nothing plays, and time moves only when a client calls `advance_clock`. That makes it the right choice for tests, scripts and batch work.

| Option                                | What it does                                                                                                                                                                                  |
| ------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--audio pipewire`                    | Play through PipeWire instead of the virtual clock.                                                                                                                                           |
| `--no-autoconnect`                    | With `--audio pipewire`: don't link Dawkey's outputs to the default sink.                                                                                                                     |
| `--bridge-plugins[=shared\|separate]` | Run plugins in processes of their own, as the window does: one per plugin file (`shared`, the default) or one per instance (`separate`). Without it, plugins run inside Dawkey's own process. |
| `--no-sandbox`                        | With `--bridge-plugins`: don't sandbox the plugin processes.                                                                                                                                  |
| `--http ADDR`                         | Serve MCP over HTTP at `http://ADDR/mcp` instead of stdio (see below).                                                                                                                        |

Headless instances ignore the preferences file.

### The running window, through a Unix socket

Each Dawkey window serves MCP on `$XDG_RUNTIME_DIR/dawkey/<pid>.sock`, which only your user can open (`dawkey --no-mcp` turns it off). `dawkey mcp --attach` connects stdin and stdout to that socket, so any client that can start a command can drive the window you're working in:

```sh
dawkey mcp --attach            # the most recently started window
dawkey mcp --attach --pid 1234 # a particular one
```

### HTTP

For clients that can't start a command, Dawkey can serve MCP's streamable HTTP transport. It's off unless you turn it on.

- **In the window:** turn on "Serve over HTTP" in the preferences (Ctrl+,). It listens on `http://127.0.0.1:7381/mcp` by default; the port is in the preferences too.
- **Headless:** `dawkey mcp --headless --http 127.0.0.1:0` serves it instead of stdio. Port 0 picks a free port, and the URL is printed on stdout. It runs until it gets SIGINT or SIGTERM.

Only loopback addresses are allowed, so only programs on your computer can connect. Every request needs `Authorization: Bearer <token>`. The token is in `~/.config/dawkey/mcp-token` (more exactly, `$XDG_CONFIG_HOME/dawkey/mcp-token`), which Dawkey makes the first time it's needed. A headless instance takes `$DAWKEY_MCP_TOKEN` instead when it's set. "New token" in the preferences replaces the token.

### Client configuration

Claude Code:

```sh
# The window you have open
claude mcp add dawkey-live -- dawkey mcp --attach

# A headless instance of its own
claude mcp add dawkey -- dawkey mcp --headless

# The window over HTTP
claude mcp add --transport http dawkey-http http://127.0.0.1:7381/mcp \
  --header "Authorization: Bearer $(cat ~/.config/dawkey/mcp-token)"
```

In a checkout of this repository, Claude Code picks up both stdio servers from [.mcp.json](../.mcp.json). They run the development build with `cargo run`.

Clients configured with JSON, such as Claude Desktop, take the same command:

```json
{
  "mcpServers": {
    "dawkey-live": {
      "command": "dawkey",
      "args": ["mcp", "--attach"]
    }
  }
}
```

### The Claude Code plugin

The repository is also a Claude Code plugin marketplace. Its `dawkey` plugin connects Claude Code to the window (`dawkey mcp --attach`) and adds a skill that teaches Claude to work in Dawkey: checking each change by rendering it, writing MIDI, choosing among your installed plugins, mixing to a loudness target, recording and exporting.

```text
/plugin marketplace add quonic/dawkey
/plugin install dawkey@dawkey
```

The skill itself is in [claude-plugin/skills/dawkey](../claude-plugin/skills/dawkey), and works in other clients that take Agent Skills. See [claude-plugin/README.md](../claude-plugin/README.md).

## Working with the tools

The server's instructions give clients these rules too. In short:

- **Call `get_state` first.** It returns the whole project: tracks, clips, the audio pool, and the IDs other tools take. Track IDs are never reused, so they stay valid across undo and redo. The master bus is track 0.
- **Times are in seconds**, except in MIDI tools, which take beats (quarter notes) from the clip's start. Gains are in dB.
- **Nothing is written to disk until `save_project`.**
- **Mistakes come back as tool errors** with a readable message, so the client can correct itself and try again.
- **Each change is one undo step.** `undo` reverts exactly one tool call.

### Tools

There are 84 tools. Each one's description says exactly what it takes and returns.

| Area              | Tools                                                                                                                                                                                                                  |
| ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Project           | `get_state`, `new_project`, `open_project`, `save_project`, `rename_project`, `undo`, `redo`                                                                                                                           |
| Importing         | `import_audio`, `import_midi`                                                                                                                                                                                          |
| Transport         | `play`, `stop`, `locate`, `set_loop`, `set_punch`                                                                                                                                                                      |
| Tempo and meter   | `set_tempo`, `remove_tempo_change`, `set_tempo_map`, `set_time_signature`, `remove_time_signature_change`, `set_time_signature_map`                                                                                    |
| Tracks            | `add_track` (audio, MIDI or bus), `remove_track`, `rename_track`, `set_track_gain`, `set_track_pan`, `set_track_mute`, `set_track_solo`, `set_track_output`                                                            |
| Clips             | `add_clip`, `move_clip`, `remove_clip`, `set_clip_gain`, `set_clip_mute`, `set_clip_length`, `trim_clip`, `split_clip`, `duplicate_clip`, `repeat_clip`, `comp_take`                                                   |
| Recording         | `record`, `set_track_arm`, `set_track_input`, `set_track_monitor`, `set_record_offset`                                                                                                                                 |
| MIDI              | `add_midi_clip`, `add_midi_notes`, `edit_midi_notes`, `remove_midi_notes`, `quantize_midi_notes`, `set_midi_controls`, `set_synth`, `set_track_midi_input`, `set_track_midi_output`                                    |
| Plugins           | `list_plugins`, `rescan_plugins`, `add_plugin`, `remove_plugin`, `move_plugin`, `get_plugin`, `set_plugin_param`, `set_plugin_bypass`, `set_plugin_sidechain`, `set_plugin_output`, `reload_plugin`, `show_plugin_gui` |
| Automation        | `set_automation`, `add_automation_point`, `remove_automation_points`, `write_automation`                                                                                                                               |
| Engine and output | `get_engine_status`, `get_meters`, `advance_clock`, `render_range`, `export_audio`                                                                                                                                     |
| User interface    | `ui_tree`, `ui_screenshot`, `ui_click`, `ui_drag`, `ui_scroll`, `ui_hover`, `ui_key`, `ui_type`, `ui_drop_files`, `ui_resize`                                                                                          |

### Listening without speakers

Two tools let a client check what the project sounds like:

- `render_range` renders part of the project offline and returns peak and RMS levels per channel, integrated loudness (LUFS) and a hash of the audio. It doesn't depend on the transport.
- `advance_clock` (virtual audio backend only) runs the engine for a stretch of time, as if a sound card had played it, and returns the same analysis of what came out. It can also feed audio from a WAV file into the inputs and MIDI into the MIDI input, so recording can be tested without a microphone or keyboard.

### The window

`ui_tree` returns the window's widget tree. Every widget has a stable ID, such as `transport/play`, `track/3/fader` or `clip/12`. The other `ui_*` tools act on those IDs, or on window positions, the same way a mouse and keyboard would. `ui_screenshot` returns a PNG. Headless instances have a window too; it's rendered offscreen.

The widget IDs are a public interface: tests and clients rely on them, and they don't change without a good reason. [CLAUDE.md](../CLAUDE.md) lists them all.

## Testing over MCP

Most of Dawkey's features are tested end to end. The tests in [tests/](../tests) start the real `dawkey` binary headless and drive it through `dawkey_testkit::Harness`, an MCP client:

```rust
use dawkey_testkit::Harness;
use serde_json::json;

const DAWKEY: &str = env!("CARGO_BIN_EXE_dawkey");

#[tokio::test]
async fn renders_an_imported_file() {
    let dawkey = Harness::headless(DAWKEY, std::iter::empty::<&str>())
        .await
        .unwrap();

    let imported = dawkey
        .call("import_audio", json!({ "path": "/path/to/saw.wav" }))
        .await
        .unwrap();
    let track = imported["track"].clone();

    dawkey
        .call("set_track_gain", json!({ "track": track, "gain_db": -6.0 }))
        .await
        .unwrap();
    let analysis = dawkey
        .call("render_range", json!({ "start_s": 0.0, "end_s": 1.0 }))
        .await
        .unwrap();
    assert!(analysis["peak_dbfs"][0].as_f64().unwrap() < 0.0);

    dawkey.shutdown().await.unwrap();
}
```

- `call` returns a tool's result as JSON, and `call_err` expects a tool error and returns its message.
- `Harness::headless_with_env` sets environment variables, such as `CLAP_PATH` for the test plugins in `crates/dawkey-test-plugins`. `Harness::headless_http` starts an instance served over HTTP.
- Each test process gets its own `XDG_CACHE_HOME`, so tests never touch your real cache.

See [CONTRIBUTING.md](../CONTRIBUTING.md#tests) for the rules tests follow.
