# Vintage sample kit

Kick, snare and tom from the [Versilian Community Sample Library](
https://github.com/sgossner/VCSL)'s "Legacy" instruments — older, duller-
sounding recordings kept in the library alongside its modern ones, giving
this kit a dusty, 60s/70s-record character. Downmixed to mono; the kick
was trimmed to 2.5s with a short fade-out (the source rings for several
more seconds).

| Pad | Source file |
|---|---|
| Kick | `Membranophones/Struck Membranophones/Bass Drum 3 - Legacy/bdrum_f_rr1.wav` |
| Snare | `Membranophones/Struck Membranophones/Legacy Snares/OldSnare/snare_mf2.wav` |
| Tom | `Membranophones/Struck Membranophones/Legacy Toms/tenor_lower/tenor_mf_rr1.wav` |

Closed Hat, Open Hat, Crash, Ride and Clap reuse the same samples as the
`acoustic_vcsl` kit (shared `const`s in `builtin.rs`, not duplicated
`include_bytes!`) — cymbals and claps don't really have an "era", so there
was no reason to source separate ones.

Licensed CC0 1.0 Universal (see `CC0.txt`).
