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
a data item). For data, it lists the instructions that read it, write it or take its address, by function.
Decompiled code is a hint, not a source (D2): it is partly wrong.

Ghidra misses code that only a vector or a pointer reaches, such as interrupt handlers (`rrc inputs` names
them by their entry). `rrc show ID --asm` then disassembles it with Clockwork, following its jumps; once you
name it (`rrc name`), `rrc analyze` adds it to Ghidra's analysis as a function, with its calls, references
and decompiled code (that takes Ghidra's minutes again).

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
Once there are sync points, the counts come in phases: before the first tick, then each run of ticks at one
sync point (the menus, play), so a loop that has no sync point yet shows in the phase it runs in.
`rrc calls first-level ID` says how often each phase called one function; `--top N` and `--min N` show more
or fewer groups.

**Where the tick ends (D47):** put the sync point where the wait is over and the pass hasn't read its inputs
yet, usually where the waiting function returns (its last instruction), not its entry: a tick's inputs and
state are what memory holds at the sync point, which is what the pass starting there reads. At the entry,
the elapsed time is the previous pass's, and a key that changes during the wait reaches the pass but not the
tick. A game has a loop per mode (play, demo playback, menus); one loop may also end its tick in two places
(demo playback skipping the usual wait for one of its own): each gets a sync point:

```bash
rrc sync GAME.EXE+0A31C play --evidence "where wait_for_tick returns; notes/loops.md" --confidence 0.8
```

**Loops that poll (D48):** menus and title screens often poll the controls as fast as the CPU runs, with no
timer wait: `rrc calls` shows them as huge counts (thousands a second). A sync point at their poll would tick
once a poll, which measures the emulated CPU, not the game. Gate it instead: `when_changed` lists what the
loop reads (its inputs, and the counter its timing reads, such as the timer count a blinking cursor follows),
and a hit is a tick only when one of them changed since that point's last tick. Put the sync point where the
loop reads its controls (`rrc sync ID menu --when-changed keys,timer_count,mouse`). `rrc trace` says how many
hits each gated sync point skipped; if the loop does work per poll that changes state (a random number drawn
each pass), the trace shows it, and the notes should say so.

Then the tick's **boundary values**: what the loop reads that comes from outside it (the elapsed time the
waiting function measures, the key-state array, the mouse). To find them:

```bash
rrc inputs first-level
```

scans the data segment the loop runs with and lists what the program reads that an interrupt handler or a
host service writes, by handler: the keyboard's (INT 09h) writes the key states, the timer's (INT 08h) the
timer counts and whatever runs at the timer's rate (music), the sound card's its own state. A handler's bytes
are grouped by the instruction that wrote them, so the keys a session pressed in a key-state array show as one
candidate from the first to the last, with the instruction (and what Ghidra says it refers to: often the
array's start); once you name the array (`rrc data`), the candidate shows as it. Which of these are inputs and
which belong to an interrupt-driven subsystem of the simulation (D22) is your call: inputs come from the player
and the clock. A session must press the keys for their bytes to show up. Declare each:

```bash
rrc input GAME.EXE+21C40 tick_elapsed --size 2 --evidence "..."
rrc input mouse mouse --evidence "INT 33h 03h and 0Bh every pass"
```

Memory outside the program's image is `MEM+<linear address>`: a game that times itself by the BIOS's count
reads `MEM+0046C` (4 bytes), one that reads keys through INT 16h takes them from the BIOS's buffer
(`MEM+0041A`, the head and tail words, then 32 bytes of keys). `rrc inputs SESSION --at MEM+00400 --len 256`
scans the BIOS's data; `rrc show` names what Ghidra's code reads there (`DAT_0040_006c` is `MEM+0046C`).

Values that come back from a driver or a port in registers don't show here: a program often reads the mouse
(INT 33h) or the joystick (port 201h) and uses the answer at once. `rrc inputs` says how often the program
called or read them (`unseen`). Declare those as the device (`rrc input mouse NAME`, D49): the trace then has
the driver's state at each tick, which is what the pass starting there reads, and `rrc trace --item NAME`
shows it. A gated sync point can name it too, so a menu ticks when the mouse moves. An input only one loop
reads can be read only at its sync points (`--sync menu`). `rrc sync` and `rrc input` alone list what is
declared; `--remove NAME` takes one out. Record the evidence in the notes.

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

lists each access in those ticks (or the whole session) with the instruction, the value and the call chain.
Name data as you learn it (`rrc data ID NAME --size N --evidence "..."`) and record again: the trace follows
every named data item. Recording a session again must give the same run (`same`); after a knowledge change
the trace differs and `rrc record` says the run is the same (`same run as before; the trace differs`). A run
that differs is a Clockwork bug, or the session, the game or Clockwork changed: say so in the notes. Commit the
session's file with the new hashes.

`rrc record` also keeps a snapshot of the machine every 10 emulated seconds (on this machine, in the cache):
`rrc watch` and `rrc inputs` then start from the last one before the tick they look at, in seconds instead of
a run from power-on, with the whole call chains. Record a session before asking about its late ticks.
`rrc calls` and `rrc inputs` keep their answers: asked again with the same session and knowledge, they answer at
once (`kept`).

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
