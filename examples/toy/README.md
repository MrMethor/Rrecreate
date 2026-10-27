# The toy port

A whole port repository, small enough to read in one sitting: the toy game of `testbins`
([`testbins/src/toy.c`](../../testbins/src/toy.c), built as `testbins/bin/TOY.EXE`), its knowledge, a session,
the state mapping and the port in Rust. `rrc`'s tests run it (`rrc/tests/flow.rs`), and it is the example
`docs/porting.md` step 5 points to.

| File | What |
|---|---|
| `rrc.toml` | The game (`TOY.EXE`, its version by hash) and the port's program (`[port] command`) |
| `knowledge/` | The functions and data, named from the linker map (`TOY.MAP`, provenance `map`); the `enemy` structure, the pointer to one (`enemy_ptr`, read as an index into `enemies`) and the status bits; the sync point (where `wait_tick` returns) and the inputs (INT 08h's timer count, INT 09h's key states) |
| `sessions/play.*` | 20 seconds of play: the arrow keys and the three bombs |
| `mapping.toml` | The port's fields and the original's data they are, the trail on the heap through its far pointer (`via`) |
| `src/main.rs` | The port: the game's state and its pass, on `rrc-runtime` |

To run it, with `rrc` and `clockwork` built (`cargo build --release -p rrc -p clockwork`) and on the path:

```bash
cd examples/toy
rrc game ../../testbins/bin     # the folder with TOY.EXE
rrc record play
rrc compare play
```

`rrc compare play` says `match` at all 1400 ticks. `TOY_BUG=1 rrc compare play` leaves a bounce uncounted in
the port: the comparison diverges at tick 194 and names the instruction in `update` that wrote the count.
`TOY_BUG=trail` keeps the player's y in the trail: it diverges at tick 2, in the trail on the heap, and names
the instruction that wrote it there.
