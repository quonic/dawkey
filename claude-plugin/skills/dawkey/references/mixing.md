# Mixing, mastering and exporting

Mix by measuring. Every step below ends with a render that shows whether it did what you meant.

## Start with the numbers

```text
render_range { start_bar: 1, end_bar: <end>, stems: true }
render_range { start_bar: 1, end_bar: <end> }
```

The stems show each track's own `lufs`, `rms_dbfs`, `peak_dbfs` and `bands`. The mix shows where the total sits. Use the loudest section (the chorus) for balance decisions, and the whole song for loudness.

## Balance

- **Leave headroom.** Before mastering, keep the mix's peaks below about −3 dBFS and don't chase loudness with the faders; the limiter does that last. If the mix peaks higher, lower every track by the same amount rather than limiting it on the master. Its loudness then lands wherever the material puts it, often −22 to −16 LUFS (dense dance mixes have less distance between peak and loudness than sparse acoustic ones).
- **Find what sets the peak.** If the mix peaks far above its loudness, render stems and find the track whose `peak_dbfs` matches the mix's. One spiky sound (a clap, a noise hit, a pluck) often is: tame it on its own track (a limiter or compressor there) rather than holding the whole mix down. If no stem's peak comes close to the mix's, hits landing together (kick, clap and bass on the same beat) set it, and lowering one of them, or the bass's attack, does more than a limiter. A limiter on a dense one-shot (a clap sample that's loud all the way through) mostly just turns it down: compare its peak minus its loudness before and after, and bring its level back up with the fader.
- **Set levels with the faders** (`set_track_gain`). Use `set_clip_gain` to even out one clip.
- **Rough starting balance** (stem loudness relative to the main element, which is usually the lead, vocal or kick; in a sketch without them, whatever carries the song, such as the chords): kick and lead or vocal at 0 dB, snare about −1 to −3 dB, bass about −2 to −4 dB, chords and pads about −6 to −10 dB, hats and percussion about −10 to −14 dB, effect returns about −12 to −18 dB. Genre changes this a lot: treat it as a place to start, then listen.
- **Pan** (`set_track_pan`, −1 to 1): keep kick, snare, bass and lead in the center. Spread chords, doubles, hats and percussion between about ±0.2 and ±0.7. Balance the left and right sides.
- **Bass build-up:** if the mix's `bands.low_dbfs` is several dB above `mid_dbfs`, find which stems are loud in the low band. Usually it's two parts fighting (kick and bass), or pads and chords that need a high-pass filter.
- **Masking:** two stems with high levels in the same band compete. Separate them with EQ, octaves, panning or arrangement (one plays while the other rests).

## Buses and sends

- `add_track { kind: "bus" }` makes a bus. `set_track_output { track, bus }` routes a track into it instead of the master (for a drum bus, say, so one fader and compressor control every drum). Leave `bus` out to send it back to the master.
- `set_track_send { track, bus, gain_db }` copies a track to a bus at its own level, after the fader unless `pre_fader`. One reverb bus with a different send from each track is the usual setup.
- Effects on a send bus should be fully wet (see plugins.md).
- Dawkey refuses routing loops, and tells you why.

Look the reverb's parameter IDs up first (`list_plugins { query: "<its name>", params: true }`), so the whole bus, fully wet, is one batch and one undo step. With Calf Reverb, for example, the wet and dry levels are `amount` and `dry`:

```json
{ "label": "reverb bus", "operations": [
  { "tool": "add_track", "args": { "kind": "bus", "name": "Reverb" } },
  { "tool": "add_plugin", "args": { "track": "$0.track", "plugin": "lv2:http://calf.sourceforge.net/plugins/Reverb" } },
  { "tool": "set_plugin_param", "args": { "plugin": "$1.plugin", "param": "amount", "value": 1 } },
  { "tool": "set_plugin_param", "args": { "plugin": "$1.plugin", "param": "dry", "value": 0 } },
  { "tool": "set_track_send", "args": { "track": 2, "bus": "$0.track", "gain_db": -14 } },
  { "tool": "set_track_send", "args": { "track": 5, "bus": "$0.track", "gain_db": -18 } }
] }
```

Then check what it adds with the bus-mute comparison in SKILL.md. A fully wet return at a −10 dB send can be nearly as loud as the dry track, so start sends around −14 to −18 dB, and check the source's peaks and the mix's again.

## EQ and compression

Your rendered numbers can't hear tone. Use them to check that a move did what you meant (the band level changed, the peaks came down), and keep moves modest:

- **High-pass** everything that isn't bass or kick, at about 80 to 150 Hz for chords, pads and leads. Check that the stem's `low_dbfs` drops and its `mid_dbfs` barely moves. The low band reaches up to 250 Hz, so a part whose lowest notes are above the cutoff (chords in the middle register) drops only a fraction of a dB; that's expected, and the `hash` changing shows the filter is working.
- **Cut before boosting.** Use 2 to 4 dB cuts, and boosts of 3 dB at most unless asked.
- **Compression:** a ratio of 2:1 to 4:1, a threshold that gives 2 to 6 dB of gain reduction, and makeup gain to match the bypassed loudness. Then compare the stem's `peak_dbfs` minus its `rms_dbfs` (crest factor) with the bypassed render's: it should shrink by a few dB, and the loudness should stay about the same.
- **Sidechain ducking:** a compressor on the bass with `set_plugin_sidechain` from the kick track.

## Automation

Targets: `{ kind: "gain" }` (dB), `{ kind: "pan" }`, `{ kind: "send", bus }` (dB), and `{ kind: "param", plugin, param }` (in the parameter's range). A lane overrides the fader or knob for the whole song, so give it a point at each end of the change, at the levels you want before and after.

- `add_automation_point { track, target, time_bar, value }` adds one point. Its time takes `time_s`, `time_beats` or `time_bar`. A `batch` of them is one undo step:

```json
{ "label": "fade in chords", "operations": [
  { "tool": "add_automation_point", "args": { "track": 2, "target": { "kind": "gain" }, "time_bar": 1, "value": -40 } },
  { "tool": "add_automation_point", "args": { "track": 2, "target": { "kind": "gain" }, "time_bar": 3, "value": 0 } }
] }
```

- `set_automation { track, target, points }` replaces a whole lane, and `write_automation` replaces a stretch of it (`from_bar`/`to_bar`). Their points take `time_s` only. At a constant tempo, a 4/4 bar lasts 240 / bpm seconds, and bar `n` starts at (n − 1) × 240 / bpm.
- Check with `render_range { ..., segment_bars: 1 }`: the levels should follow the curve.
- Common moves: fade-ins and fade-outs (gain −60 to 0 dB), a low-pass filter opening over a build-up, more reverb send at the end of phrases, a lead turned up in the chorus.

## Mastering

On the master (track 0), in this order, only as needed: gentle EQ, gentle bus compression (1 to 2 dB of reduction), then a limiter last.

To reach a loudness target with a limiter:

1. Render the whole song and read `lufs`.
2. Raise the limiter's input gain (or lower its threshold) by the difference between the target and that reading. Set its ceiling to −1 dB, and turn on true-peak mode if it has one.
3. Render again. Limiting costs some of the gain, so repeat until you're within about 0.5 LU of the target, with peaks at or under the ceiling.
4. If it takes more than about 6 dB of limiting to get there, the mix is too quiet or too dynamic. Say so instead of crushing it.

Loudness targets (integrated, whole song):

| Destination | LUFS | Peak ceiling |
| --- | --- | --- |
| Spotify, YouTube, Tidal, Amazon | −14 | −1 dBTP |
| Apple Music | −16 | −1 dBTP |
| Podcasts, spoken word | −16 (stereo), −19 (mono) | −1 dBTP |
| Broadcast (EBU R128) | −23 | −1 dBTP |
| Club, DJ use, SoundCloud | −9 to −6 | −1 dBTP |
| Unknown | ask; −14 is the usual default | −1 dBTP |

`peak_dbfs` is a sample peak, and the true peak can be up to about 0.5 dB higher (more after lossy encoding). For Ogg or Opus without a true-peak limiter, keep sample peaks at −1.5 dBFS or lower.

A render of an excerpt with silence or a tail reads quieter than the song does. Measure loudness over the whole song.

## Exporting

`export_audio { path }` renders and writes the whole song: from the start to where the last clip ends, as 24-bit at the project's sample rate, in the format of the file's extension.

- **Formats:** `.wav` (16, 24 or 32-bit float), `.flac` (16 or 24-bit, up to 96 kHz), `.ogg` (`quality` 0 to 10, 5 is about 160 kbit/s), `.opus` (`bitrate_kbps`, always 48 kHz).
- **Reverb and delay tails:** add `tail_s: 2` to `4`, or the end cuts them off.
- **Parts:** use a range (`start_bar`/`end_bar`), or `tracks` to hear only some.
- **Stems:** `stems: true` writes one file per track beside `path`.
- **Loops:** `loop: true` folds the tail onto the start so the file loops seamlessly (for games, or a loop someone asks for). Check `seam` in the result for a click: as a rule of thumb, a jump below about 0.01 (−40 dBFS) is inaudible, and one above about 0.1 (−20 dBFS) clicks. The tail is added after the master limiter, so the start can peak above its ceiling: leave about 0.5 dB more headroom, and check `peak_dbfs` in the result.
- **Metadata:** `tags: { title, artist, album, date }` for Ogg and Opus.
- Use an absolute path, and ask where to write if the user didn't say. The result reports the written file's levels and loudness: quote them back.
