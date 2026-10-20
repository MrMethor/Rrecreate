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
disassembly, in which Ghidra's `FUN_` and `DAT_` names become the names you gave (`name[+N]` for a byte inside
a data item). Decompiled code is a hint, not a source (D2): it is partly wrong.

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
`rrc show <ID> --c`; `rrc show 2A40:13C2` finds the ID of an address Ghidra's code names (`DAT_2a40_13c2`).

**Where the tick ends (D47):** put the sync point where the wait is over and the pass hasn't read its inputs
yet, usually where the waiting function returns (its last instruction), not its entry: a tick's inputs and
state are what memory holds at the sync point, which is what the pass starting there reads. At the entry,
the elapsed time is the previous pass's, and a key that changes during the wait reaches the pass but not the
tick. A game has a loop per mode (play, demo playback, menus); one loop may also end its tick in two places
(demo playback skipping the usual wait for one of its own): each gets a sync point. Loops that poll as fast
as the CPU runs, with no timer wait (menus, title screens), don't fit a sync point yet: leave them without
one and say so in the notes.

Declare them in `knowledge/ticks.toml`, with the tick's **boundary values**: what the loop reads that comes
from outside it (the elapsed time the waiting function measures, the key-state array, the mouse). To find them:

```bash
rrc inputs first-level
```

scans the data segment the loop runs with and lists what the program reads that an interrupt handler or a
host service writes, by handler: the keyboard's (INT 09h) writes the key states, the timer's (INT 08h) the
timer counts and whatever runs at the timer's rate (music), the sound card's its own state. Which of these are
inputs and which belong to an interrupt-driven subsystem of the simulation (D22) is your call: inputs come
from the player and the clock. A session must press the keys for their bytes to show up. Values that come
back from a service in registers (the mouse driver's) don't show here: look for where the program stores them.
Record the evidence in the notes.

## 4. Record

```bash
rrc record first-level
rrc trace first-level
rrc trace first-level --item <name>
rrc trace first-level --tick 120
```

Each tick of the trace has the boundary values, the named data that changed, the frames shown and the sound
events. To see who writes a value, and from where:

```bash
rrc watch first-level <ID or name> --tick 120 --ticks 2 --reads
```

lists each access in those ticks (or the whole session) with the instruction, the value and the call chain. Name data as you learn it (`rrc data ID NAME --size N --evidence "..."`) and record again: the trace
follows every named data item. Recording the same session again must give the same hash: `rrc record` says
whether it did.

## Naming

```bash
rrc name GAME.EXE+0A2F0 <name> --evidence "what shows it" --confidence 0.8 --by agent:<model>
rrc data GAME.EXE+21C40 <name> --size 2 --type u16 --evidence "..."
rrc check
```

Longer evidence (how a loop works, why a sync point sits where it does, what couldn't be declared) goes in
`notes/*.md` in the port repository, one file per subject, referred to from the knowledge's evidence and notes.

Every decision carries its provenance and evidence; `rrc check` keeps the knowledge consistent (unique names,
data that doesn't overlap, IDs inside the image). Set `RRC_BY` once instead of passing `--by` each time.

## An honest pilot

A pilot whose source is released (Wolf3D, D3) proves the approach only if nothing from that source helps.
For such a port:

- don't read the game's source release, reimplementations, wikis or anything derived from them, nor the web
  about the game's code;
- keep the game's copy in a folder of its own, away from projects that hold derived knowledge;
- don't read Rrecreate's git history: its commit messages carry names from the project before it;
- set `[port] imports = false` in `rrc.toml`, so `rrc import` refuses;
- name things after what the code does, with evidence from this toolkit only.
