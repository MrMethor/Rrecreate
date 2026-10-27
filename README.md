# Rrecreate

A toolkit an AI agent uses to port DOS games to modern systems. What it produces for a game is a **Rust port as
close to the original's behaviour as can be checked, built to be modernized**: its logic is separate from how the
game is shown, heard and controlled, so graphics (up to real 3D), sound, performance and platforms can change
without touching it.

The original game runs in **Clockwork**, a deterministic DOS emulator; the agent writes the port and the toolkit
compares the two tick by tick, traces each difference to the original code that caused it, and keeps the books.
The port's faithful mode is checked against the original for good; modernizations are toggles on top.

**Status:** the emulator, Clockwork, runs and tools drive it (`clockwork/`, with its own
[README](clockwork/README.md)). The toolkit, `rrc` (`rrc/`), keeps a port repository: the game's versions,
analysis through Ghidra, knowledge files, sessions and per-tick traces of the original
([`docs/porting.md`](docs/porting.md)), and runs a port against the original tick by tick: the runtime a Rust
port builds on is `runtime/`, and [`examples/toy`](examples/toy) is a whole port of a toy game. The design is
[`DESIGN.md`](DESIGN.md); the decisions and their reasons are in [`docs/decisions.md`](docs/decisions.md), the
file formats in [`docs/formats.md`](docs/formats.md), and how tools drive an emulator in
[`docs/protocol.md`](docs/protocol.md).

```bash
cargo build --release -p rrc -p clockwork
```

Rrecreate began from Recreate, a private predecessor whose emulator it took over (`DESIGN.md` §11). Recreate is
disregarded: nothing is read or taken from it (D69).

## Layout

| Path | What |
|---|---|
| `clockwork/` | The emulator (GPL-2.0-or-later) |
| `rrc/` | The toolkit's command line, `rrc` |
| `runtime/` | What a Rust port builds on: the simulation's step, and the files it exchanges with `rrc` |
| `examples/toy/` | A port repository: the toy game of `testbins`, ported and compared |
| `ghidra/` | The Ghidra scripts: export, and names written back |
| `sigs/` | Library signature packs |
| `testdata/inputs/` | Walkthroughs of the test games, as input scripts |
| `testbins/` | Small DOS test programs built with Open Watcom |

Each game's port lives in its own repository.

## Licence

MIT (`LICENSE`), except `clockwork/`, which is GPL-2.0-or-later because it contains code ported from DOSBox.
