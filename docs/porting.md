# Porting a game

The agent's playbook (DESIGN.md §10). It grows with the toolkit: this is what `rrc` does so far (M2); the state
mapping, the port itself and the comparison come with M3. Formats are in `docs/formats.md`.

Run `rrc` inside the port repository. `rrc status` names the next step; `rrc doctor` says what's missing.

## 1. Set up

```bash
rrc init wolf3d-port --program WOLF3D.EXE --name wolf3d --title "Wolfenstein 3-D"
```

Then, inside it, point it at the person's copy of the game and record that copy as a version:

```bash
rrc game "C:/Games/WOLF3D" --add-version "shareware 1.4"
```

The game's files never go into the repository (`rrc.local.toml`, which holds the folder, isn't committed).
Tools are found through `rrc.local.toml` or the environment (`docs/formats.md`).

## 2. Analyse

```bash
rrc analyze
```

Clockwork unpacks the program if it was packed, Ghidra analyses it (minutes), and the runtime library's
functions get their names from the signature packs. `rrc show <ID or name>` then tells what is known about a
function: its name and evidence, its size, calls and callers, and with `--c` or `--asm` its decompiled code or
disassembly. Decompiled code is a hint, not a source (D2): it is partly wrong.

Look for prior art (D32): source releases, reimplementations, format documentation, bug lists. For a pilot
whose source is released, the source stays out of reach (D3, D46).

## 3. Sessions and ticks

A session is what to reproduce: an input script or recorded play, and how long it runs.

```bash
rrc session add first-level --seconds 51 --inputs first-level.inputs --description "..."
```

A tick is one pass of the original's main loop; a sync point is the instruction where it ends. To find one:

```bash
rrc calls first-level
```

lists the functions the session calls, grouped by how often. A loop calls its functions once a pass, so the
large groups are loops; in a group, the function that waits for the timer (it reads a counter the timer
interrupt advances, in a loop, until it changes) is where a tick of that loop ends. Check with
`rrc show <ID> --c`; `rrc show 4783:991C` finds the ID of an address Ghidra's code names (`DAT_4783_991c`). A game
has a loop per mode (menus, play, demo playback): each gets a sync point.

Declare them in `knowledge/ticks.toml`, with the tick's **boundary values**: what the loop reads that comes
from outside it (the elapsed time the waiting function measures, the key-state array, the mouse). Record
evidence in the notes.

## 4. Record

```bash
rrc record first-level
rrc trace first-level
rrc trace first-level --item <name>
rrc trace first-level --tick 120
```

Each tick of the trace has the boundary values, the named data that changed, the frames shown and the sound
events. Name data as you learn it (`rrc data ID NAME --size N --evidence "..."`) and record again: the trace
follows every named data item. Recording the same session again must give the same hash: `rrc record` says
whether it did.

## Naming

```bash
rrc name WOLF3D.EXE+0F280 <name> --evidence "what shows it" --confidence 0.8 --by agent:<model>
rrc data WOLF3D.EXE+4114C <name> --size 2 --type u16 --evidence "..."
rrc check
```

Every decision carries its provenance and evidence; `rrc check` keeps the knowledge consistent (unique names,
data that doesn't overlap, IDs inside the image). Set `RRC_BY` once instead of passing `--by` each time.
