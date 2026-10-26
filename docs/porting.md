# Porting a game

The agent's playbook (DESIGN.md §10). It grows with the toolkit: this is what `rrc` does so far (M3: the
original's knowledge and traces, the mapping, the port and the comparison). Formats are in `docs/formats.md`.

Run `rrc` inside the port repository. `rrc status` names the next step; `rrc doctor` says what's missing.

## 1. Set up

```bash
rrc init wolf3d-port --program WOLF3D.EXE --name wolf3d --title "Wolfenstein 3-D"
```

`rrc init` makes the folder a git repository too (a port is one of its own). Then, inside it, point it at the
person's copy of the game and record that copy as a version:

```bash
rrc game "C:/Games/WOLF3D" --add-version "shareware 1.4"
```

A version is every file in the folder, so leave out what isn't the game: saved games, a setup program's
configuration (`--without SAVEGAM?.WL1,CONFIG.WL1`; `*` and `?` are wildcards). The game's files never go
into the repository (`rrc.local.toml`, which holds the folder, isn't committed).
Tools are found through `rrc.local.toml` or the environment (`docs/formats.md`).

## 2. Analyse

```bash
rrc analyze
```

Clockwork unpacks the program if it was packed, Ghidra analyses it (minutes), and the runtime library's
functions get their names from the signature packs (`sigs/README.md` lists them; `rrc sigs build` makes one
from a program built with the game's compiler and its linker map). `rrc show <ID or name>` then tells what is known about a
function: its name and evidence, its size, calls and callers, and with `--c` or `--asm` its decompiled code or
disassembly, in which Ghidra's `FUN_` and `DAT_` names become the names you gave (`name[+N]` for a byte inside
a data item). In the disassembly, lines and branch targets are IDs, and each instruction's data is named after
it (`; tick_count`); a jump through a table says so, and `--listing` shows Clockwork's disassembly instead,
following the jumps and listing each table's cases. For data, it lists the instructions that read it, write
it or take its address, by function (ten functions a kind; `--all` lists them all).
Decompiled code is a hint, not a source (D2): it is partly wrong.

Ghidra misses code that only a vector or a pointer reaches, such as interrupt handlers (`rrc inputs` names
them by their entry). `rrc show ID --asm` then disassembles it with Clockwork, following its jumps; once you
name it (`rrc name`), `rrc analyze` adds it to Ghidra's analysis as a function, with its calls, references
and decompiled code (Ghidra opens its analysis again for that: a minute or so).

Some games make code at run time (drawing routines generated for each size, say): `rrc calls` then counts
functions past the program's file image, which Ghidra can't see. `rrc show ID` says so, and `rrc show ID
--asm --session NAME [--tick N]` disassembles it from the session's memory, by tick N or at its last
snapshot. Port what the generator makes, not the bytes (DESIGN.md §6).

Look for prior art (D32): source releases, reimplementations, format documentation, bug lists, the knowledge
of earlier projects (`rrc import`). For a test bench whose source is released, used to grade the toolkit, the
source stays out of reach (D46, below).

## 3. Sessions and ticks

A session is what to reproduce: an input script or recorded play, and how long it runs.

```bash
rrc session add first-level --seconds 51 --inputs first-level.inputs --description "..."
```

An input script presses keys, clicks and moves the mouse, and moves the joystick (`{stick X Y BUTTONS}`; the
session then needs `--clockwork "--joystick 1"`): `clockwork/README.md` lists its steps. A game that turns by
the mouse's motion turns with `{motion DX DY}` (mickeys), not `{move X Y}`, whose motion stops at the screen's
edge. To find a script, explore the game step by step from a snapshot instead of rerunning it
(`clockwork/README.md`, "Exploring a game"): the lines that worked are the script.

A tick is one pass of the original's main loop; a sync point is the instruction where it ends. To find one:

```bash
rrc calls first-level
```

lists the functions of the program the session calls, grouped by how often (an extender's and drivers' are
only counted). A loop calls its functions once a pass, so the
large groups are loops; in a group, the function that waits for the timer (it reads a counter the timer
interrupt advances, in a loop, until it changes) is where a tick of that loop ends. Check with
`rrc show <ID> --c`; `rrc show DAT_2a40_13c2` (Ghidra's name, as its code has it, or `2A40:13C2`; `FUN_0043525b` in a
flat program) finds the ID of an address.
Once there are sync points, the counts come in phases: before the first tick, then each run of ticks at one
sync point (the menus, play), so a loop that has no sync point yet shows in the phase it runs in.
`rrc calls first-level ID` says how often each phase called one function; `--top N` and `--min N` show more
or fewer groups.

**Where the tick ends (D47):** put the sync point where the wait is over and the pass hasn't read its inputs
yet, usually where the waiting function returns (its last instruction), not its entry: a tick's inputs and
state are what memory holds at the sync point, which is what the pass starting there reads. At the entry,
the elapsed time is the previous pass's, and a key that changes during the wait reaches the pass but not the
tick. A sync point in a function ticks at every call of it: if something else calls the waiting function too
(a level's setup), put the sync point after the loop's own call of it (the instruction the call returns
to). A game has a loop per mode (play, demo playback, menus); one loop may also end its tick in two places
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

**Input that arrives during the pass (D60):** input arrives at any cycle, so between the sync point and the
pass's read of an input there should be as little as possible. `rrc record` checks every read: `late` counts
the ticks whose pass read an input after it had changed since the sync point, with the reads that did it, most
first (the input, the instruction that read it, or for a driver the instruction after its call; in how many
ticks; the first). Such a pass did what the next tick's values say, and a port fed this tick's would do
something else (stay in a loop the press ended). `rrc trace SESSION --tick N` lists a tick's. The wait that
ends a tick isn't counted: late reads all made in the call the next hit comes in, the last at most 40 of the
program's instructions before it, are the wait polling its timer count. To fix one:

- move the sync point to the read: onto the instruction that reads the input (the load from the key array,
  the `int 33h`, the `in al, dx` of port 201h), or just before it in the same loop; not at the top of a pass
  that reads something first, nor at the entry of a function the loop calls after reading something;
- if several loops call the function that reads the input, each loop gets a sync point at its own call: a
  sync point in the function ticks for every caller;
- if the pass reads an input in several places, put the sync point on the first read; a later read that sees
  a change stays `late` (a timer count read again mid-pass as a timestamp), and the notes should say why that
  is harmless (a menu that sees a held key a pass later) or not (a one-shot exit);
- a scripted input that lands in the gap can be moved by a few milliseconds (`MS:` in the script) while the
  sync point is being placed.

Record again until no tick is `late`, or the reads left are explained in the notes.

Then the tick's **boundary values**: what the loop reads that comes from outside it (the elapsed time the
waiting function measures, the key-state array, the mouse). To find them:

```bash
rrc inputs first-level
```

scans the data segment the loop runs with and the BIOS's data (`MEM+00400`, 256 bytes) from tick 2 on (a loop
that has no sync point yet: `--from-ms MS [--to-ms MS]` scans that time), and lists what the program reads
that an interrupt handler or a host service writes, by handler: the keyboard's (INT 09h) writes the key states, the timer's (INT 08h) the
timer counts and whatever runs at the timer's rate (music), the sound card's its own state. A handler's bytes
are grouped by the instruction that wrote them, so the keys a session pressed in a key-state array show as one
candidate from the first to the last, with the instruction (and what Ghidra says it refers to: often the
array's start); once you name the array (`rrc data`), the candidate shows as it. Which of these are inputs is
your call. Inputs come from outside the game: the player (keys, mouse, joystick) and the clock the loop
measures time by. Work an interrupt handler does for the game at its own rate (music, sound effects, an
animation the timer advances) is part of the simulation (D22): the port does it too, and what it writes is
state, compared at each tick, not an input. A session must press the keys for their bytes to show up. Declare
each:

```bash
rrc input GAME.EXE+21C40 tick_elapsed --size 2 --evidence "..."
rrc input mouse mouse --evidence "INT 33h 03h and 0Bh every pass"
```

Memory outside the program's image is `MEM+<linear address>`: a game that times itself by the BIOS's count
reads `MEM+0046C` (4 bytes), one that reads keys through INT 16h takes them from the BIOS's buffer
(`MEM+0041A`, the head and tail words, then 32 bytes of keys). `rrc inputs` scans the BIOS's data with the
program's; `--at ID --len N` scans another range instead. `rrc show` names what Ghidra's code reads there
(`DAT_0040_006c` is `MEM+0046C`; a flat program's `_DAT_0000046c` too).

Values that come back from a driver or a port in registers don't show here: a program often reads the mouse
(INT 33h) or the joystick (port 201h) and uses the answer at once. `rrc inputs` says how often the program
called or read them (`unseen`). Declare those as the device (`rrc input mouse NAME`, D49): the trace then has
the driver's state at each tick, which is what the pass starting there reads, and `rrc trace --item NAME`
shows it. A gated sync point can name it too, so a menu ticks when the mouse moves. `rrc ports SESSION 201`
lists the instructions that read the joystick's port (any port: the display's retrace at 3DA, a sound card),
to find where a loop reads it. An input only one loop reads can be read only at its sync points (`--sync
menu`). A gated sync point names inputs that exist, so declare a polling loop's inputs first: `--sync` may
name its sync point before it is declared (`rrc record` refuses until it is). Declaring an item again replaces
it (`rrc` shows what it `was`). `rrc sync` and `rrc input` alone list what is declared; `--remove NAME` takes
one out. Record the evidence in the notes.

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
rrc frame first-level --tick 120
```

lists each access in those ticks (or the whole session) with the instruction, the value and the call chain;
`rrc frame` writes what the screen showed by the end of a tick as a PNG, to look at.
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

## 5. Port and compare

Map the state first: `mapping.toml` names each field of the port's state and the original's item it is
(`docs/formats.md`); record again, so the trace follows every mapped item. Then name the port's program in
`rrc.toml` (`[port] command`). A Rust port builds on `runtime/` (the crate `rrc-runtime`): its state is one
struct serde serializes, its fields named as the mapping says; it implements `Simulation` (`start`: the state at
the first tick; `step`: one pass with the tick's inputs) and its `main` calls `rrc_runtime::main`. `examples/toy`
is a whole port repository to copy from: the knowledge, a session, the mapping and the port of a toy game.

```bash
rrc compare first-level
rrc compare first-level --transplant 500
rrc compare first-level --pair 900
rrc compare first-level --resume 900
```

`rrc compare` says `match`, or where the port first differs: the tick, each field with both values, and who wrote
the original's value between the previous tick's sync point and this one's (the instruction and its call chain:
read that code). Fix the port and compare again. A divergence at the port's first tick means its start state
isn't the original's, or a transplant misses a field (map it). Work on a subsystem from a transplant at a tick
where it is in use, so what comes before needn't be ported first. Where the port matches through a tick, `--pair
N` keeps a paired checkpoint, and `--resume N` compares from it without the ticks before. `unmapped` lists the
named data the mapping neither maps nor ignores: the state nothing compares yet.

A scenario is set up by edits instead of play (D59, D64): where the game has a cheat or a debug mode, use it
first (it goes through the game's own code); otherwise

```bash
rrc session edit first-level 300 player_health 5
rrc session edit first-level 300 "actors[3]" "{\"state\": 2}"
rrc record first-level
```

writes the values into the original's memory at tick 300's sync point, in every run of the session; the port
gets the mapped fields they wrote at that tick with its inputs. Edit through the game's structures, whole (a
count with the list it counts): a state play can't reach makes a divergence after it a false alarm, and
`rrc compare` names the last edit before a divergence for that reason.

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

## A 32-bit program

A flat program under a DOS extender (an LE executable: CauseWay, DOS/4GW) works the same way, by its objects:
`rrc analyze` analyses them where the extender put them, and IDs name an object by number (`GAME.EXE#1+00024` in
the code object, `GAME.EXE#2+0041C` in a data object), as Ghidra's addresses (`FUN_00435024`) become. The first
question about a session runs it once to find where its objects go (kept afterwards); `rrc inputs` scans the
data objects (and the BIOS's data).

## A launcher

Some games start through a launcher (a small program that checks the setup, then runs the game's own, which
refuses to start alone). Start the launcher (`program`) and name the game's program as `module` in `rrc.toml`
(`docs/formats.md`): it is the one analysed and named, and sync points count only in it. The first question
about a session runs it once to find where the game's program goes (kept afterwards); from then on IDs work as
if the game had started alone, and the launcher's code is memory outside it (`MEM+...`).

## An honest test bench

A port of a game whose source is released, made to test the toolkit with the source as its grade (Wolf3D's,
D46), proves the approach only if nothing from that source helps. For such a port:

- don't read the game's source release, reimplementations, wikis or anything derived from them, nor the web
  about the game's code;
- keep the game's copy in a folder of its own, away from projects that hold derived knowledge;
- don't read Rrecreate's git history: its commit messages carry names from the project before it;
- set `[port] imports = false` in `rrc.toml`, so `rrc import` refuses;
- give it a cache of its own (`cache` in `rrc.local.toml`): the cache is shared by the ports on a machine,
  and `rrc analyze` reuses an analysis of the same program, with the functions other ports' knowledge added
  to Ghidra's;
- name things after what the code does, with evidence from this toolkit only.
