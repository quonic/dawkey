---
name: dawkey
description: Make, edit, mix and export music in Dawkey, the Linux DAW, through its MCP tools (get_state, add_midi_clip, add_plugin, render_range, export_audio, ...). Use whenever the user mentions Dawkey or a .dawkey project, or asks to write or arrange music (a beat, chords, a bassline, a melody, a song), add instruments or effects, mix, master, record, or export audio while Dawkey's tools are connected.
license: AGPL-3.0-or-later
---

# Working in Dawkey

Dawkey is a DAW. Its MCP tools do everything the window does: tracks, clips, MIDI, plugins, mixing, automation, recording, rendering and exporting. You can't hear, but you can measure: `render_range` renders any part of the project offline and reports levels, loudness and a hash. Most of doing this job well is checking every change that way instead of assuming it worked.

Tool names below are bare (`get_state`). In your tool list they carry the server's prefix, such as `mcp__dawkey__get_state` or `mcp__plugin_dawkey_dawkey__get_state`.

## 1. Connect

There are two kinds of Dawkey server:

- **The user's window** (`dawkey mcp --attach`; often named `dawkey-live`, or `dawkey` from the Claude Code plugin). This is the project the user has open. They see every change as you make it, and `play` and `record` use their speakers and microphone.
- **A headless instance** (`dawkey mcp --headless`). A private project with no speakers and a virtual clock. Use it for scratch work, tests and batch jobs.

Call `get_engine_status` first. `backend: "pipewire"` means real audio; `"virtual"` means silent, with time moved only by `advance_clock`.

If there are no Dawkey tools, or the server failed to connect, tell the user in one step: "Open Dawkey, then run `/mcp` and reconnect `dawkey`" (attaching needs a running window). If they've never set it up: `claude mcp add dawkey -- dawkey mcp --attach`. HTTP and other clients are covered in Dawkey's `docs/MCP.md`.

## 2. Rules for someone else's project

1. **Start with `get_state`** (`summary: true` for a big project). It gives the track, clip, note and plugin IDs every other tool takes. The master bus is track 0. IDs are never reused, so they stay valid across undo. `dirty: true` means there are unsaved changes, and `path` is where the project was last saved.
2. **Catch up before you act.** The user may have changed things in the window since your last call. Keep the `revision` from your last result and call `get_changes { since_revision }` before the next edit. If it says `complete: false`, or `instance` changed (Dawkey restarted), it can't go back that far: call `get_state` again.
3. **Never throw work away without asking.** Don't pass `discard_changes: true` to `new_project` or `open_project`, don't `remove_track` or `remove_clip` things you didn't make, and don't `save_project` over a file, unless the user said to. When they ask for a new or another project while the open one has unsaved changes (yours included), stop and ask first: save it, save it as a new file, or discard the changes? "Clear everything out" or "start fresh" doesn't answer that. Nothing is written to disk until `save_project`, so otherwise remind them to save when you're done.
4. **Fix what was asked.** If you notice other problems (a mix that already clips, a quiet track), report them and offer a fix; don't make them unasked.
5. **Don't make sound or record unasked in the window.** Use `render_range` to check things. Call `play` only when the user asks to hear something, and `record` only when they're ready to play.
6. **One tool call is one undo step.** Group each logical change in one `batch` with a `label` ("add drums", "mix: reverb bus"), so the user can undo it with one Ctrl+Z. In a batch, `"$0.track"` is the `track` the first operation returned and `"$1.plugin"` the `plugin` the second did, so a plugin can be added and set up in one batch. To know its parameter IDs before it exists, look them up with `list_plugins { query, params: true }`. A batch is all or nothing, and it can't hold reads, imports, renders or transport calls.
7. **Errors are messages for you.** Bad input comes back as a tool error that says what was wrong. Read it, fix the call and retry. Don't give up on the tool.

## 3. Units

- **Timeline positions** are seconds (`position_s`, `start_s`, ...). Any top-level `*_s` position also takes `*_beats` (quarter notes from the project start) or `*_bar` (bars as the ruler numbers them, from 1). `end_bar: 9` is where 8 bars end. `2.5` is halfway through bar 2. Give one form, not two: with `start_bar`, leave `start_s` out. (Dawkey 0.1.4 and earlier marked `start_s` required in the schema anyway; leave it out.)
- **Nested times are seconds only**, such as automation points' `time_s`. Convert: seconds = beats × 60 / bpm, at a constant tempo.
- **`get_state` gives clip positions in samples** (`start`, `length`, at the project's `sample_rate`) and in `start_beats`, `end_beats` and `start_bar`, not in seconds. Seconds = samples / `sample_rate`.
- **Notes** are in beats from their clip's start: `start_beats`, `length_beats`. A 16th note is 0.25 and a bar of 4/4 is 4.
- **Levels** are in dB. Pan runs from −1 (left) to 1 (right). Velocity runs from 1 to 127.
- **Plugin parameters** use the plugin's own range. `get_plugin` shows `min`, `max` and `text`, and `set_plugin_param` takes `text` ("-6 dB", "50 %"), which is usually safer than a raw value. A choice (a waveform, a filter type) lists its options as `choices`: pass one's name as `text`. VST3 values are normalized 0 to 1.

## 4. Listening without ears

The loop is: change something, render it, read the numbers, decide.

```text
render_range { start_bar: 1, end_bar: 9 }                       # the mix
render_range { start_bar: 1, end_bar: 9, stems: true }          # each track alone
render_range { start_bar: 1, end_bar: 9, tracks: [3] }          # as if track 3 were soloed
render_range { start_bar: 1, end_bar: 17, segment_bars: 4 }     # how it changes over time
```

`tracks` and `stems` work the way soloing does, following the signal. A track's render includes what its sends add through their buses, so a chords stem includes its reverb. A bus's render includes every track that feeds it, with their dry sound too. So rendering a bus alone tells you little. To measure what an effect bus adds, render the source track twice, with the bus muted (`set_track_mute`) and not, and compare: the difference is the return, and the stretch after the last note should be silent (or below about −150 dBFS; instruments can leave a residue) with the bus muted, and audible without. Then `undo` the mute, so the user's history is left as it was. Adding a return also raises the source's stem and the mix, so check their peaks again afterwards.

**Measure the stems before you mix.** Instruments' default levels differ by 20 dB or more. In one test, an electric piano plugin came in at −13 LUFS and the built-in synth's kick at −35.

What the result means:

- `silent: true` and `null` peaks: nothing came out. The track is muted, the instrument has nothing loaded, the clip is somewhere else, or a plugin is broken.
- `hash`: identical before and after a change means **the change did nothing audible**. Use this to prove a plugin or parameter does something: render, `set_plugin_bypass` on, render, bypass off.
- `peak_dbfs` is the sample peak per channel. Above about −1 risks clipping. Identical left and right peaks and RMS mean the signal is mono.
- `lufs` is integrated loudness. It needs at least 0.4 s of audio, so short segments give `null`.
- `rms_dbfs` is the average level, useful for comparing tracks in stems.
- `bands` are RMS levels below 250 Hz, from 250 Hz to 4 kHz, and above 4 kHz (gentle filters, on the mono sum). Use them to compare brightness and spot bass build-up, not as an EQ curve.
- `non_finite > 0`: a plugin is producing NaN or infinity. Bypass it and tell the user.

**A render starts cold at `start_s`.** Reverbs, delays and compressors haven't heard anything before the range starts. To check a tail, render the source track from before its last notes, and analyze the stretch after its dry sound has died away (allow for the instrument's release) as a segment:

```text
render_range { start_bar: 1, end_bar: 7, tracks: [<source track>], segments: [{ start_s: 11.25, end_s: 15 }] }
```

The segment should be quiet but not silent. If it's silent, the reverb or delay adds nothing: it's bypassed, fully dry, needs a file, or the send is off.

When taste matters (a melody, a mix balance), measurements aren't enough. Say what you measured, then offer the user a way to listen: `play` in their window, or `render_range { ..., output_path: "/tmp/preview.wav" }` for a file.

## 5. Which reference to read

Read the file for the job before you start it:

| Job | Read |
| --- | --- |
| Writing notes, chords, drums, basslines; arranging sections; the built-in synth | [references/composing.md](references/composing.md) |
| Finding, loading and setting up plugins (instruments and effects) | [references/plugins.md](references/plugins.md) |
| Balancing, buses and sends, EQ and compression, automation, mastering, loudness, exporting | [references/mixing.md](references/mixing.md) |
| Recording audio or MIDI, takes and comping, MIDI devices, the window's `ui_*` tools | [references/recording-and-window.md](references/recording-and-window.md) |

## 6. Reporting back

Finish with a short report:

- **What you changed**, by name, such as "added a Drums track (track 7) with a 4-bar beat repeated to bar 17". Mention the undo steps you made.
- **What you measured**, with the numbers: loudness, peaks, which stems play and where.
- **What you couldn't check**, such as how something sounds as music, and how the user can hear it.
- **Unsaved changes**, if there are any: say so, unless you were asked to save.
