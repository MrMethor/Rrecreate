# Rrecreate

A toolkit an AI agent uses to port DOS games to modern systems. The original game runs in **Clockwork**, a
deterministic DOS emulator; the agent writes a native port and the toolkit compares the two tick by tick, traces
each difference to the original code that caused it, and keeps the books. The port has a faithful mode, checked
against the original, and a modern mode on top.

**Status:** design only. Start with [`DESIGN.md`](DESIGN.md); the decisions and their reasons are in
[`docs/decisions.md`](docs/decisions.md).

Rrecreate succeeds [Recreate](https://github.com/MrMethor/Recreate), whose emulator it takes over.

## Layout (planned)

| Path | What |
|---|---|
| `clockwork/` | The emulator (GPL-2.0-or-later) |
| `rrc/` | The toolkit's command line |
| `runtime/` | The crates a port builds on |
| `ghidra/` | The Ghidra export script |
| `testbins/` | Small DOS test programs built with Open Watcom |

Each game's port lives in its own repository.

## Licence

MIT (`LICENSE`), except `clockwork/`, which is GPL-2.0-or-later because it contains code ported from DOSBox.
