# Rrecreate

A toolkit an AI agent uses to port DOS games to modern systems. What it produces for a game is a **Rust port as
close to the original's behaviour as can be checked, built to be modernized**: its logic is separate from how the
game is shown, heard and controlled, so graphics (up to real 3D), sound, performance and platforms can change
without touching it.

The original game runs in **Clockwork**, a deterministic DOS emulator; the agent writes the port and the toolkit
compares the two tick by tick, traces each difference to the original code that caused it, and keeps the books.
The port's faithful mode is checked against the original for good; modernizations are toggles on top.

**Start at the hub, [`docs/README.md`](docs/README.md):** the objective, how the project is built, its rules, where
the work stands, what was decided, and where every document is (the design, the decisions, the playbook for porting
a game, the file formats, the emulator protocol).

```bash
cargo build --release -p rrc -p clockwork
```

Rrecreate began from Recreate, a private predecessor whose emulator it took over (`docs/design.md` §11). Recreate is
disregarded: nothing is read or taken from it (D69).

## Layout

| Path | What |
|---|---|
| `docs/` | Every document, entered through the hub, `docs/README.md` |
| `clockwork/` | The emulator (GPL-2.0-or-later) |
| `rrc/` | The toolkit's command line, `rrc` |
| `runtime/`, `types/`, `ids/` | What a Rust port builds on: the simulation's step, and the files it exchanges with `rrc` |
| `platform/` | What a port plays on: a window, input devices, sound |
| `examples/` | Port repositories of the test programs: `toy`, `chime` (sound), `vault` (data files, from power-on) |
| `ghidra/` | The Ghidra scripts: export, and names written back |
| `sigs/` | Library signature packs |
| `testdata/inputs/` | Walkthroughs of the test games, as input scripts |
| `testbins/` | Small DOS test programs built with Open Watcom |

Each game's port lives in its own repository.

## Licence

MIT (`LICENSE`), except `clockwork/`, which is GPL-2.0-or-later because it contains code ported from DOSBox.
