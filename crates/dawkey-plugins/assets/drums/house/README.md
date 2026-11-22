# House sample kit

Eight one-shots synthesized for Dawkey, built for a house/dance-style sound
(a punchy sine-sweep kick, a noise-and-tone snare, bright noise-shaped
hats, a clustered-burst clap, and metallic cymbals mixing shaped noise with
a few inharmonic sine partials). Not sampled from any recording or sample
library — generated from scratch with simple oscillators, one-pole
filters and a deterministic PRNG for the noise, the same kind of synthesis
`dawkey-dsp`'s own drum engine uses, just rendered once and saved instead
of run live.

| Pad | How it's built |
|---|---|
| Kick | Sine with a fast pitch drop (165→48 Hz) and a short noise click for punch |
| Snare | Sine body plus bandpassed noise "snap" |
| Closed Hat | Short burst of heavily highpassed noise |
| Open Hat | Same noise coloring as Closed Hat, longer decay |
| Tom | Sine with a slower pitch drop (190→85 Hz) |
| Clap | Three clustered noise bursts (the classic "flam") plus a longer tail |
| Crash | Long highpassed noise with a few inharmonic sine partials for shimmer |
| Ride | Shorter, more tonal version of the same cymbal blend |

Being original work with no external source, it's dedicated to the public
domain under CC0 1.0 Universal (see `CC0.txt`) like the other kits, so it's
free to reuse the same way they are.

Embedded in the binary so sample playback works identically everywhere,
the same reason `dawkey-text` embeds its fonts.
