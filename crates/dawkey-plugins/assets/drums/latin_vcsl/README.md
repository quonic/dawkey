# Latin Percussion sample kit

Hand percussion from the [Versilian Community Sample Library](
https://github.com/sgossner/VCSL), mapped onto the engine's eight pads the
way many drum machines remap a hand-percussion ensemble onto a standard
kit layout — these aren't literal substitutes for a kick or a hi-hat, they
play that *role* in the pattern. Downmixed to mono.

| Pad | Plays | Source file |
|---|---|---|
| Kick | Cajon | `Idiophones/Struck Idiophones/Cajon/Cajon_hit1_f_rr1.wav` |
| Snare | High bongo | `Membranophones/Struck Membranophones/Bongos/BongoH_Hit1_v2_rr1_Mid.wav` |
| Closed Hat | Claves | `Idiophones/Struck Idiophones/Claves/Claves1_Hit_v2_rr1_Mid.wav` |
| Open Hat | Cowbell | `Idiophones/Struck Idiophones/Cowbells/Cowbell1_Normal_v3_rr1_Mid.wav` |
| Tom | Conga | `Membranophones/Struck Membranophones/Conga/Conga_HitN_v2_rr1_Sum.wav` |

Clap, Crash and Ride reuse the same samples as the `acoustic_vcsl` kit
(shared `const`s in `builtin.rs`, not duplicated `include_bytes!`).

Licensed CC0 1.0 Universal (see `CC0.txt`).
