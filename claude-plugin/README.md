# Dawkey for Claude

A Claude Code plugin that lets Claude work in Dawkey with you. It has two parts:

- **The MCP connection** to the Dawkey window you have open (`dawkey mcp --attach`), so Claude can edit and play your project. You see every change as it's made, and each one is an undo step of its own.
- **The `dawkey` skill**, which teaches Claude how to use Dawkey well: check every change by rendering it and reading the levels (it can't hear), write and arrange MIDI, find plugins among the ones you have installed and prove they do something, mix and master to a loudness target, record, and export.

## Installing

You need `dawkey` on your `PATH` (from a package or `cargo install dawkey`). Then, in Claude Code:

```text
/plugin marketplace add quonic/dawkey
/plugin install dawkey@dawkey
```

Open Dawkey before you start Claude Code. If Dawkey wasn't running when Claude Code started, open it and reconnect the server with `/mcp`.

To have Claude work in a headless instance of its own instead (no window, nothing plays, nothing you have open is touched), add one alongside:

```sh
claude mcp add dawkey-headless -- dawkey mcp --headless
```

## Using it

Ask for what you want in your own words:

- "Make an 8-bar lo-fi beat at 85 BPM with drums, bass and Rhodes chords."
- "Put a reverb bus on the chords and the lead, and make the lead sit on top of the mix."
- "Master this for Spotify and export it to ~/Music/song.flac."
- "Arm the guitar track and set up a loop from bar 9 to 17 so I can record takes."

Claude reports what it changed, what it measured, and what it couldn't check. Anything it did can be undone one step at a time (Ctrl+Z). It doesn't save your project unless you ask.

## Other clients

The skill is a standard [Agent Skill](https://agentskills.io): the folder [skills/dawkey](skills/dawkey) works in any client that supports skills. For Claude Desktop or claude.ai, zip that folder and upload it as a custom skill in the settings, then connect Dawkey as described in [docs/MCP.md](../docs/MCP.md).
