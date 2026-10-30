# The chime port

A port with its sound: the chime of `testbins` ([`testbins/src/chime.c`](../../testbins/src/chime.c), built as
`testbins/bin/CHIME.EXE`). Space plays a note on the PC speaker and the OPL's channel 0 for 20 passes; Up and
Down choose it. `rrc`'s tests run it (`rrc/tests/flow.rs`), and it is M6's test program for sound (D108).

| File | What |
|---|---|
| `rrc.toml` | The game (`CHIME.EXE`, its version by hash) and the port's program |
| `knowledge/` | The functions and data, named from the linker map (`CHIME.MAP`, provenance `map`), with its static functions found from `main`'s calls; one sync point (`pass`, where `wait_tick` returns) and the inputs (INT 08h's timer count, INT 09h's key states) |
| `sessions/play.*` | 3 seconds: C5, then E5, played again before it ends, then Esc |
| `mapping.toml` | The pass's counters, the note chosen, the one sounding and how long, the notes played |
| `src/main.rs` | The port: the pass; its scene (the notes played, the one sounding) and `Sound`, which makes what the original's `instrument`, `speaker_on`, `speaker_off` and `opl` write. It draws nothing |

To run it, with `rrc` and `clockwork` built (`cargo build --release -p rrc -p clockwork`) and on the path:

```bash
cd examples/chime
rrc game ../../testbins/bin     # the folder with CHIME.EXE
rrc record play
rrc trace play --tick 36        # a note starts: opl: A0=59 B0=35, speaker: tone 2281/1141
rrc compare play
```

`rrc compare play` says `match` at all 182 ticks, and that the port's sound is the original's: the instrument
at start-up, each note's speaker tone and OPL key on, its end, and a note played over another (the speaker held
high while the timer is set again). `CHIME_BUG=block rrc compare play` keys the OPL an octave low: the sound
differs at tick 36, its second OPL event (`B0=35`, the port's `B0=31`), and the comparison lists who wrote the
OPL's ports in that pass.
