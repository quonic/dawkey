# Composing and arranging

## Pitches

MIDI pitch 60 is middle C (C4), and each octave is 12 semitones higher. C = 0, C♯ = 1, D = 2, E♭ = 3, E = 4, F = 5, F♯ = 6, G = 7, A♭ = 8, A = 9, B♭ = 10, B = 11, so pitch = 12 × (octave + 1) + note. A3 = 57, A4 = 69 (440 Hz).

| Role | Range | Pitches |
| --- | --- | --- |
| Kick (built-in synth) | G0 to C1 | 31 to 36 |
| Sub bass, bass | E1 to E3 | 28 to 52 |
| Chords, pads | C3 to C5 | 48 to 72 |
| Lead, melody | C4 to C6 | 60 to 84 |

Scales as semitones from the root:

- Major: 0 2 4 5 7 9 11
- Natural minor: 0 2 3 5 7 8 10
- Dorian: 0 2 3 5 7 9 10
- Minor pentatonic: 0 3 5 7 10
- Major pentatonic: 0 2 4 7 9

Chords as semitones from the root:

- Major: 0 4 7
- Minor: 0 3 7
- Dominant 7: 0 4 7 10
- Major 7: 0 4 7 11
- Minor 7: 0 3 7 10
- sus2: 0 2 7
- sus4: 0 5 7

Voice chords close together, and move each voice to the nearest note of the next chord rather than jumping in parallel. Put the bass on the root, an octave or two below.

Progressions that work:

- I–V–vi–IV (pop): C G Am F
- vi–IV–I–V: Am F C G
- i–VI–III–VII (minor pop): Am F C G
- ii–V–I (jazz): Dm7 G7 Cmaj7
- i–iv–v (minor blues)

## Rhythm

Beats are quarter notes, so in 4/4 a bar is 4 beats. A 16th note is 0.25, an 8th is 0.5, an 8th-note triplet is 1/3, and a dotted 8th is 0.75.

- **Swing:** delay every second 8th (or 16th) by about a sixth to a third of its length. For 8ths, start them at 0.58 to 0.67 instead of 0.5.
- **Humanize:** vary velocities by ±5 to 15 and accent the downbeats. Shift timing by only ±0.01 to 0.02 beats, and only when the user wants a played feel; leave programmed drums on the grid.
- **Note lengths:** a gap before the next note (legato for pads, staccato for plucks) matters as much as the start. Bass notes that run into each other blur.

## Drums (General MIDI)

Drum plugins and most drum kits follow the General MIDI map:

| Sound | Pitch | Sound | Pitch |
| --- | --- | --- | --- |
| Kick | 36 | Closed hi-hat | 42 |
| Side stick | 37 | Pedal hi-hat | 44 |
| Snare | 38 | Open hi-hat | 46 |
| Clap | 39 | Crash | 49 |
| Electric snare | 40 | Ride | 51 |
| Low, mid and high toms | 45, 47, 50 | Tambourine | 54 |

Patterns, in beats within one bar of 4/4:

- **Four on the floor:** kick 0, 1, 2, 3; clap or snare 1, 3; closed hats on every 8th off-beat (0.5, 1.5, 2.5, 3.5).
- **Backbeat rock or pop:** kick 0, 2.5 (and 2); snare 1, 3; closed hats on every 8th.
- **Half-time:** kick 0, 1.75; snare 2; hats on 8ths or 16ths.
- **Boom bap:** kick 0, 1.75, 2.5; snare 1, 3; swung 8th hats.

Where the drums come from, best first:

1. **A drum instrument plugin with a kit loaded.** Sample-based ones (drumkv1, Fabla, sfizz, Hydrogen kits) are silent until a kit is loaded. See plugins.md.
2. **Audio one-shots.** `import_audio { path, position_bar: 1 }` a drum sample (imports can't go in a batch; give `track` to put a second sample on an existing track), then `duplicate_clip` it to each hit, all in one `batch` per sound. A clip can't be longer than its sample, so `repeat_clip` won't space hits out: it's one `duplicate_clip` per hit, and a few bars of hats is a hundred or more. Write the operations with a short script rather than by hand, and set accents with `set_clip_gain` on `"$n.clip"`. Trim long samples (an open hat) shorter than the gap to the next hit with `trim_clip`, since overlapping clips both play. Plugins' factory kits are often presets rather than files a property can load, but the WAV files in their folders can be imported.
3. **A synth plugin with a noise oscillator** (amsynth, Surge XT, synthv1, Vital, ...). Hats are noise through a high-pass filter with a very short decay (30 to 80 ms). A clap or snare is noise through a band-pass filter with a short decay (100 to 200 ms), on its own track. Noise hits have sharp peaks, so a limiter on the clap track keeps one hit from setting the mix's peak.
4. **The built-in synth, for a kick only.** It has no noise, so it can't make snares or hats. For a kick, use a sine at pitch 31 to 36 with `attack_ms: 0, decay_ms: 150 to 250, sustain: 0, release_ms: 40, cutoff_hz: 400, filter_env_octaves: 3`. It comes out quiet (about −14 to −17 dBFS peak at velocity 120), so it needs about +10 dB. Faders stop at +12 dB, so raise `set_clip_gain` too if needed.

## The built-in synth

Every MIDI track without an instrument plugin plays through Dawkey's synth. It is a 16-voice subtractive synth: one oscillator (`saw`, `square`, `triangle` or `sine`), an ADSR envelope and a resonant low-pass filter that the envelope opens by `filter_env_octaves`. Its defaults are a saw at `cutoff_hz` 1200 with resonance 0.2. It responds to velocity, pitch bend (±2 semitones), the mod wheel (CC 1, vibrato), volume (CC 7, unity at 100), expression (CC 11) and the sustain pedal (CC 64).

Starting points for `set_synth`:

| Sound | waveform | attack / decay / sustain / release | cutoff_hz | resonance | filter_env_octaves |
| --- | --- | --- | --- | --- | --- |
| Sub bass | sine | 2 / 200 / 0.9 / 60 | 300 | 0 | 0 |
| Synth bass | square or saw | 2 / 180 / 0.5 / 80 | 500 to 800 | 0.3 | 2 |
| Pad | saw | 300 to 800 / 1000 / 0.8 / 800 to 1500 | 1500 to 2500 | 0.1 | 1 |
| Pluck | saw or square | 0 / 250 / 0 / 200 | 800 | 0.4 | 4 |
| Lead | saw or square | 5 / 300 / 0.7 / 150 | 2500 to 4000 | 0.3 | 1 |
| Soft keys | triangle | 5 / 600 / 0.3 / 400 | 3000 | 0 | 1 |
| Kick | sine | 0 / 180 / 0 / 40 | 400 | 0 | 3 |

For anything more realistic (piano, strings, real drums), load an instrument plugin.

## Making clips

One `add_midi_clip` makes a clip and its notes, and makes a new MIDI track too unless you give it `track`. Put a whole section's parts in one `batch`:

```json
{ "label": "verse 1", "operations": [
  { "tool": "add_midi_clip", "args": { "track_name": "Bass", "position_bar": 1, "length_beats": 16,
      "notes": [ { "pitch": 45, "start_beats": 0, "length_beats": 1.5 } ] } },
  { "tool": "set_synth", "args": { "track": "$0.track", "waveform": "square", "cutoff_hz": 600 } },
  { "tool": "duplicate_clip", "args": { "clip": "$0.clip", "position_bar": 5 } }
] }
```

- Make one clip per musical idea (a 1, 2, 4 or 8-bar loop), then `repeat_clip { clip, until_bar }` to lay copies end to end up to a bar (the last is cut short there if it doesn't fit), or `duplicate_clip` it to a bar (`position_bar`) on its track or another. Copies are independent: editing one doesn't change the others.
- Check the `clips` that `repeat_clip` returns: from a 1-bar clip at bar 1 to `until_bar: 9` there are 7. In Dawkey 0.1.4 and earlier, at tempos where a bar isn't a whole number of samples, it could add a sliver of an eighth copy (a couple of samples long, playing the clip's first note again) at `until_bar`; remove it with `remove_clip`.
- Clips land exactly where you say, even over other clips, and overlapping clips both play. Check `get_state` before placing a clip where one may already be.
- `edit_midi_notes` changes notes by ID (from `get_state` or the result that made them), `remove_midi_notes` removes them, and `quantize_midi_notes { grid_beats: 0.25 }` snaps starts to 16ths.
- `set_midi_controls` writes pitch bend, CCs and the sustain pedal into a clip. For a piano, use CC 64 at 127 to press the pedal and 0 to lift it, at chord changes.
- `import_midi` reads a .mid file. With `targets`, it puts the file's parts on existing tracks and keeps their plugins and mix.

## Song structure

Plan sections in bars before writing notes, and say the plan to the user. A typical pop or electronic layout at 90 to 128 BPM:

| Section | Bars | Typical content |
| --- | --- | --- |
| Intro | 4 to 8 | Chords or a filtered loop, no kick |
| Verse | 8 to 16 | Drums, bass, chords, lead or vocal space |
| Pre-chorus | 4 | Build: hats to 16ths, filter opening, kick drops out |
| Chorus | 8 to 16 | Everything, highest notes, fullest chords |
| Breakdown | 4 to 8 | Pads and melody, no drums |
| Outro | 4 to 8 | Elements dropping out one by one |

Contrast is what makes sections distinct: change the density (which tracks play), the register and the rhythm, not only the chords.

- `set_tempo { bpm }` sets the start tempo. With `at_beats` it adds a change, and with `ramp` the tempo glides to it.
- `set_time_signature { numerator, denominator, at_bar }` changes the meter.
- MIDI clips keep their beats when the tempo changes. Audio clips keep their times.

## Checking what you wrote

- `render_range { start_bar, end_bar, stems: true, segment_bars: 4 }` shows each track's level in each 4-bar segment. A part that's `silent` where it should play, or loud where it should rest, is placed wrong.
- `get_state { tracks: [id] }` lists the notes with `start_beats` and `pitch`. Re-read them when an edit is involved.
- `ui_screenshot` shows the arrangement as the user sees it, which is a good final check of the layout.
