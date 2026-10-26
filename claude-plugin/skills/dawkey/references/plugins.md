# Plugins

Dawkey hosts CLAP, LV2 and VST3 plugins. What's installed differs on every computer, often by hundreds of plugins, so search the user's own list. Don't assume a plugin exists.

## Finding one

`list_plugins { query }` matches a plugin's name, vendor or ID, not its category. Search several words for one job:

| Job | Words to try |
| --- | --- |
| Reverb | `reverb`, `verb`, `plate`, `hall`, `room`, `dragonfly` |
| EQ | `eq`, `equalizer`, `parametric`, `fil4`, `filter` |
| Compressor | `comp`, `compressor`, `dynamics` |
| Limiter | `limit`, `dpl`, `maximi` |
| Delay | `delay`, `echo` |
| Saturation | `satur`, `tape`, `distort`, `drive`, `tube` |
| Modulation | `chorus`, `flanger`, `phaser`, `tremolo` |
| Synth | `list_plugins { kind: "instrument" }`; look for synth names such as `surge`, `vital`, `odin`, `amsynth`, `synthv1`, `dexed`, `helm` |
| Piano, strings, General MIDI | `piano`, `fluid`, `sfizz`, `sf2`, `soundfont` |
| Drums | `drum`, `kit`, `geonkick`, `fabla` |

Notes on the list:

- `kind` (and `instrument` in `get_plugin`) is a guess from the plugin's ports. Some effects that also take MIDI (Calf's, for example) are listed as `midi_effect` or `instrument`, so don't filter effects by `kind: "effect"` alone, and don't trust the flag over the plugin's name.
- Many plugins come in mono and stereo versions. Use the stereo one on stereo material and buses. `list_plugins { query, detail: true }` shows the ports.
- `scan_failures` counts plugin files that couldn't be scanned. If something the user expects is missing, `rescan_plugins` after they install it.
- Well-known families (if installed): LSP (EQ, compressors, limiters; a lot of parameters each), x42 (`x42-dpl` limiter, `x42-eq`), Calf, Dragonfly (reverbs), ZAM, MDA and SWH (old but simple), Surge XT, Vital, Odin 2, amsynth, Dexed and synthv1 (synths).

## Plugins that need something first

These load without an error and then do nothing, or pass the signal through unchanged:

- **Sample players and soundfont players** (samplv1, drumkv1, Fabla, sfizz, FluidSynth, Calf Fluidsynth) are silent until a sample, kit or soundfont is loaded.
- **Convolution or impulse reverbs** (LSP Impulse Reverb, IR loaders) are dry until an impulse response file is loaded.
- **Analyzers and meters** (spectrum displays, A/B testers, scopes) don't change the sound.
- Plugins listed as instruments that aren't synths, such as autotune or MIDI gates, which process audio or MIDI.

For LV2 plugins you can load the file yourself. `get_plugin` lists `properties`. One whose `range` ends in `#Path` is a file. Set it with:

```json
{ "plugin": 4, "key": "http://samplv1.sourceforge.net/lv2#P101_SAMPLE_FILE", "value": "/home/me/samples/piano-c4.wav" }
```

That's `set_plugin_property`. It reloads the plugin with the file, and it's not an undo step.

Where files come from: plugins often ship factory samples, kits or impulse responses inside their bundles (an LV2 bundle such as `/usr/lib64/lv2/fabla808.lv2/`, or `/usr/share/<plugin>/`), and those are fair to use. Say which you used. For the user's own sample library, ask which folder or file they want rather than searching their home folder. CLAP and VST3 plugins load files only in their own GUI: `show_plugin_gui` opens it for the user (when `get_plugin` says `has_gui`; only in the window, where plugins run in their own processes).

## Loading and setting up

- `add_plugin { track, plugin }` adds it at the end of the chain, or at `index`. On a MIDI track, the first instrument in the chain plays the notes instead of the built-in synth. Put MIDI effects (arpeggiators, chord makers) before the instrument.
- `get_plugin { plugin }` lists the parameters: `id`, `name`, `min`/`max`, `default`, and the current value as the plugin shows it (`text`). Big plugins have hundreds of parameters, so page with `first`/`count`, and look for the names you need.
- `set_plugin_param { plugin, param, text: "-12 dB" }` sets a value the way the plugin shows it, which avoids guessing how it maps raw values. Re-read `get_plugin` to confirm the `text` you got.
- A choice (such as a waveform or filter type) lists its options under `choices`, each a `value` and its `text` as the plugin shows it. Set one with `set_plugin_param { plugin, param, text: "<its text>" }`. Choices with more than 32 options, and steps that are just numbers, aren't listed. Dawkey 0.1.4 and earlier don't list them at all: set the raw values from `min` to `max` one at a time and read each one's `text` back with `get_plugin` (with `first` and `count: 1`, it lists just that parameter).
- **On a bus fed by sends, set the effect fully wet** (mix 100 %, or dry off). A send bus that returns dry signal makes tracks louder instead of adding reverb. As an insert on a track, use the plugin's mix control instead.
- Parameter names aren't promises. A "Reverb time" of 4 may not mean 4 seconds. Measure what it does (below).
- `set_plugin_bypass`, `move_plugin` and `remove_plugin` do what they say. Plugin latency is compensated automatically.
- `set_plugin_sidechain { plugin, source: <track> }` feeds a track into a plugin's sidechain input (when `list_plugins` shows `sidechain`), for ducking a bass under the kick, for example.
- `set_plugin_output { plugin, output, bus }` sends a multi-out instrument's other outputs to buses.

## Proving a plugin works

Never report that an effect is working until a render shows it:

1. Render the track (`tracks: [id]`) and note the `hash` and levels.
2. Bypass the plugin, render again, then turn the bypass off.
3. If the hash is the same, the plugin does nothing to this audio. Check its mix and settings, or whether it needs a file.

For reverbs and delays, also render from before the notes and look at the stretch after they end (see "Listening without ears" in SKILL.md). A tail that's quiet but not silent means it works. For an instrument, render its track: `silent: true` means it isn't making sound.

## When a plugin misbehaves

- `get_plugin`'s `error` says the plugin crashed or failed to load. In the window, a crashed plugin passes audio through and the rest keeps playing. `reload_plugin` starts it again.
- `non_finite > 0` in a render means NaN or infinite samples. Bypass the plugin that makes them and tell the user.
- Plugins keep their own state (presets, loaded files) in the project, outside the undo history.
