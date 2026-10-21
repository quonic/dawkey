# Contributing to Dawkey

Thanks for your interest in Dawkey. Bug reports, ideas and pull requests are all welcome. Everyone taking part is expected to follow the [code of conduct](CODE_OF_CONDUCT.md).

## Reporting bugs and asking for features

Open an issue on GitHub. For a bug, include:

- what you did, what you expected, and what happened instead;
- your distribution, desktop (compositor) and PipeWire version;
- `dawkey --version`;
- the log, if there's anything in it. Run Dawkey from a terminal with `DAWKEY_LOG=debug` for more detail;
- for plugin problems, the plugin's name, format (CLAP, LV2 or VST3) and version, and whether it still fails with "Sandbox" or "Own processes" turned off in the preferences (these apply after a restart).

Report security problems privately instead; see [SECURITY.md](SECURITY.md).

## Before you start

For anything bigger than a small fix, open an issue first, so we can agree on the approach before you spend time on it.

## Setting up

Install the build dependencies listed in the [README](README.md#build-dependencies), plus `rustfmt` and `clippy`. Then:

```sh
cargo build                                              # build
cargo test --workspace                                   # unit and end-to-end tests
cargo clippy --workspace --all-targets -- -D warnings    # lint
cargo fmt --all                                          # format
```

CI runs `cargo fmt --all --check`, clippy with `-D warnings` and the whole test suite on Fedora 44 for every pull request, so run those before sending one.

The tests need a Vulkan driver: the screenshot tests render on lavapipe (Mesa's software renderer, `mesa-vulkan-drivers` on Fedora). They don't need PipeWire, a sound card or a display.

Tests that talk to a real PipeWire daemon are marked `#[ignore]`. They need a desktop session and never start the transport, so they're silent:

```sh
cargo test --test pipewire -- --ignored --test-threads 1
```

## How the code is organized

[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) describes the crates and how they fit together. [CLAUDE.md](CLAUDE.md) has the complete set of conventions. It's written for Claude Code, but it's the most detailed reference for people too.

The rules that matter most:

- **Every change to a project is a `dk_core::Command`.** The window, MCP and tests all go through `Session::execute`; nothing changes `Project` directly. A new command is an argument struct in `crates/dk-core/src/command.rs` (deriving `Serialize, Deserialize, JsonSchema`, with `#[serde(deny_unknown_fields)]`), a `lower` method that validates it and returns `Edit`s, and an entry in the `commands!` list. It then becomes an MCP tool automatically, and its doc comment becomes the tool's description, so write it for a model reading the tool list.
- **Bad tool input is a tool error** with a readable message, not a protocol error, so the client can see what was wrong and try again.
- **One tool call is one undo step.**
- **Real-time code never allocates, frees, locks or blocks.** That's `EngineProcessor::process`, everything it calls, and the PipeWire process callback. Debug builds abort on an allocation there (`assert_no_alloc`).
- **Unsafe code stays in the crates that need it**: `dk-backend-pipewire`, `dk-wayland`, `dk-plugins` and `dk-test-plugins`. Every other crate is `#![forbid(unsafe_code)]`.
- **Every source file starts with** `// SPDX-License-Identifier: AGPL-3.0-or-later`.
- **In `mcp` mode, stdout carries the protocol.** Log through `tracing` (to stderr, filtered by `DAWKEY_LOG`), never `println!`.
- **Widget IDs are a public interface.** MCP's `ui_*` tools and the tests find widgets by ID. Don't rename them casually, and give new interactive widgets IDs.
- **Colors come from the palette** (`crates/dk-app/src/theme.rs`). Add a `Palette` field with a dark and a light value instead of a color constant in a view.

## Tests

Most features are tested end to end: `tests/` starts the real `dawkey` binary headless and drives it over MCP with `dk-testkit`'s `Harness`, using the virtual audio clock so the results are the same on every run. New features should come with a test like that. See [docs/MCP.md](docs/MCP.md#testing-over-mcp).

Tests never touch the machine they run on:

- no real audio or MIDI devices: feed audio with `advance_clock`'s `input_path` and MIDI with its `midi_input`, and fake devices with `Studio::set_virtual_midi_devices`;
- no installed plugins: use the tiny plugins in `crates/dk-test-plugins`;
- no real cache or config folders: `dk-testkit` gives each test process its own.

Some tests pin exact output:

- **Audio hashes** in `tests/mcp_audio.rs` (and `GOLDEN_CHORD_HASH` in `tests/mcp_midi.rs` for the synth). If a change alters the output on purpose, update the constant and say why in the commit message.
- **Screenshots** in `tests/golden/*.png`, rendered on lavapipe with the dark theme. After an intended visual change, run `DAWKEY_UPDATE_GOLDENS=1 cargo test --test mcp_ui`, look at the new images, and commit them.

## Sending a change

- Open the pull request against `main`. Keep it to one topic, and rebase it on `main` if it falls behind.
- Write the commit title as a short summary of what changed for the user, such as "Copy, paste and repeat clips", and explain why in the body when it isn't obvious.
- Make sure formatting, clippy and the tests pass.
- Say in the pull request what you tested by hand. Pointer and keyboard input, dialogs, real audio and plugin GUIs aren't covered by the automated tests.

## License

Dawkey is licensed under the [GNU Affero General Public License, version 3 or later](LICENSE). By contributing, you agree that your contribution is licensed under the same terms.
