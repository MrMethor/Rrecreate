# Rrecreate

A toolkit an AI agent uses to port DOS games to modern systems. What it produces for a game is a **Rust port as
close to the original's behaviour as can be checked, built to be modernized**: its logic is separate from how the
game is shown, heard and controlled, so graphics (up to real 3D), sound, performance and platforms can change
without touching it.

The original game runs in **Clockwork**, a deterministic DOS emulator; the agent writes the port and the toolkit
compares the two tick by tick, traces each difference to the original code that caused it, and keeps the books.
The port's faithful mode is checked against the original for good; modernizations are toggles on top.

**Status:** design only. Start with [`DESIGN.md`](DESIGN.md); the decisions and their reasons are in
[`docs/decisions.md`](docs/decisions.md).

Rrecreate succeeds Recreate, a private predecessor whose emulator it takes over.

## Layout (planned)

| Path | What |
|---|---|
| `clockwork/` | The emulator (GPL-2.0-or-later) |
| `rrc/` | The toolkit's command line |
| `runtime/` | The crates a Rust port builds on |
| `ghidra/` | The Ghidra export script |
| `testbins/` | Small DOS test programs built with Open Watcom |

Each game's port lives in its own repository.

## Licence

MIT (`LICENSE`), except `clockwork/`, which is GPL-2.0-or-later because it contains code ported from DOSBox.
