# The vault port

A port from power-on that loads the game's files: the vault game of `testbins`
([`testbins/src/vault.c`](../../testbins/src/vault.c), built as `testbins/bin/VAULT.EXE`, its data
`testbins/bin/VAULT.DAT` written by [`testbins/data.py`](../../testbins/data.py)). The game shows a title until
Enter, then a maze of two levels walked with the arrow keys; it loads the title's picture and palette and each
level from its archive, run-length and LZSS coded. `rrc`'s tests run it (`rrc/tests/flow.rs`), and it is M5's
test program (D101).

| File | What |
|---|---|
| `rrc.toml` | The game (`VAULT.EXE` and `VAULT.DAT`, its version by hash) and the port's program |
| `knowledge/` | The functions and data, named from the linker map (`VAULT.MAP`, provenance `map`), with the far heap's two functions; the archive's header and directory `entry`; the formats (`formats.toml`: the archive, the title, the palette, the levels); two sync points, one per loop (`title`, `play`, where `wait_tick` returns), and the inputs (INT 08h's timer count, INT 09h's key states) |
| `sessions/play.*` | 9 seconds: the title, Enter, the first level walked to its exit (a wall bumped on the way), the second to its exit, where the program ends |
| `mapping.toml` | The counters, the archive's directory, the palette, and the map and the picture on the heap through their far pointers (`via`) |
| `src/main.rs` | The port: `start` is `main` before its first tick (the directory read, the palette and the title loaded); a step is the title's pass or the play's |

To run it, with `rrc` and `clockwork` built (`cargo build --release -p rrc -p clockwork`) and on the path:

```bash
cd examples/vault
rrc game ../../testbins/bin     # the folder with VAULT.EXE and VAULT.DAT
rrc record play
rrc compare play
```

`rrc compare play` says `match` at all 506 ticks, from power-on to the program's exit. Before the first level
the map's buffer holds what the far heap left in it, which the port leaves unset (`None`, D102): the comparison
says so, `unset map at ticks 1-100`, and that `__AllocSeg` (called by `_fmalloc`) wrote it last.

The archive by its formats (D103):

```bash
rrc asset VAULT.DAT                          # its members
rrc asset VAULT.DAT/TITLE                    # the title as PNG, in its palette
rrc asset VAULT.DAT/LEVEL2 --session play    # decoded as the original loaded it
```

The last says where the session read the level (in the pass to tick 374, by `load` from `load_level`) and
that the original's memory holds the 1024 bytes it decoded at tick 374.
