# Rrecreate: design

> The design as decided. Reasons and history are in `docs/decisions.md` (each decision once, with its
> status). When a decision changes, both files change in the same commit, and nothing here may describe a
> superseded decision.

**Status (2026-10-26):** M0, M1 and M2 done: Clockwork is in `clockwork/` and tools drive it through `clockwork
serve`; the toolkit, `rrc` (§9), keeps a port repository, analyses the game and traces its sessions. Two blind
tests of the toolkit on a Wolf3D port, with the playbook alone, shaped it (D46-D51, D60). M3 done: the runtime
(`runtime/`), the mapping, transplants, the comparison with who wrote, paired checkpoints and scenario edits
(D61-D64); the toy port (`examples/toy`) matches its session tick by tick, edited or not. Next: M4, a real
game's logic, tested on Arena (§12, D70, D71). Rrecreate began from Recreate's emulator and parts of its tooling
(§11); Recreate itself is disregarded (D69).

## 1. The product

What Rrecreate produces for each game is a **Rust port of the original game**:

- **As close to the original's behaviour as can be checked.** In its **faithful mode** it matches the original
  tick by tick, frame by frame and sound event by sound event, the original's bugs included, compared with the
  original running in an emulator.
- **Built to be modernized.** The game's logic is plain, named Rust, separate from how the game is shown, heard
  and controlled (§6.1), so each of these can change without touching it:
  - **graphics:** the faithful renderer is one way to draw the game's world; another can replace it, up to real
    3D: Arena's world drawn as 3D geometry instead of by its raycaster;
  - **sound:** the faithful OPL and Sound Blaster output is one backend; sampled, remastered or positional audio
    are others;
  - **performance:** native code, with the logic at a fixed rate independent of the frame rate and the CPU's
    speed;
  - **portability:** a new platform is a new platform layer.
- **Modernizations are toggles on top** (§6.4); the faithful mode stays checkable against the original for good.

The games it is for are those whose source was never released (D52). The goal is the complete toolkit, not a
particular game's port (D70): the games it is tested on (Wolf3D, whose released source grades it; Arena;
Daggerfall for 32-bit programs) get ports only as far as the tests need.

Rrecreate is the toolkit an AI agent uses to make such ports:

- **The person** supplies the game's files, makes the calls that are theirs (what to port, downloads,
  publishing) and answers what the agent escalates.
- **The agent** (Claude Code or another) drives: it analyses the game, writes the port, and finds and
  fixes differences.
- **The toolkit** does everything that isn't judgment: running the original deterministically, analysis,
  bookkeeping, comparing, and tracing a difference to the code that caused it.

## 2. Approach

The port is written as a new program from the start, not translated function by function. Ghidra's
decompiled code shows how the original works, not how it runs, and it has errors, so it is read to
understand, never translated; runs show how the original runs; the original's behaviour is the
specification. The agent works by the rules of `docs/porting.md`, "How to work" (D77): the analysis, outside
sources and the game's files first, then runs to confirm.

1. The original runs in **Clockwork**, our deterministic DOS emulator: the same inputs give the same run,
   bit for bit.
2. Sessions (input scripts, the game's own demos, recorded play) define what to reproduce.
3. The agent identifies the game's state and declares a **state mapping**: which field of the port is
   which bytes of the original's memory.
4. The agent writes the port subsystem by subsystem, starting from state **transplanted** from the
   original at a checkpoint, so nothing has to be ported before it can be compared.
5. Port and original step through the same session tick by tick; the toolkit compares the mapped state at
   every tick, and in faithful mode the frames and sound events too.
6. At the first difference the toolkit names the tick and the field, and Clockwork finds the instruction,
   function and call chain in the original that wrote that value. The agent fixes the port, or records an
   original bug as a quirk (§6.5).

## 3. Repositories

| Repository | Contents | Licence |
|---|---|---|
| **Rrecreate** (this one) | `clockwork/`: the emulator. `rrc`: the toolkit CLI. `runtime/`: the crates a Rust port builds on. `ghidra/`: the Ghidra export script. Test programs. | MIT, except `clockwork/`: GPL-2.0-or-later (it contains code ported from DOSBox) |
| **One per game port** (e.g. `wolf3d-port`) | The port, its knowledge files (§9), its sessions and quirk register | The person's choice per game |

The toolkit talks to Clockwork only through its control protocol (§4.2), never by linking it, and the runtime
never contains Clockwork code, so a port doesn't inherit the GPL.

## 4. Clockwork, the emulator

### 4.1 What it is

An IBM PC with DOS, emulated in Rust: our own 486 CPU (real, protected and virtual-8086 mode, paging) with an
FPU, VGA and SVGA (S3 with VESA), Sound Blaster, OPL, MPU-401, PC speaker, PIT/PIC/keyboard, CMOS and RTC,
EMS and XMS, CD-ROM with MSCDEX, a DPMI host, DOS with child processes and a COMMAND.COM. It runs DOS extenders
that bring their own host (CauseWay so far). Its role is the original's controlled environment.

Rules:
- **Deterministic:** time is virtual, derived from executed instructions; no wall clock, no randomness, no
  host-dependent floating point. Any difference between two runs is a Clockwork bug.
- **Features are adopted, not invented:** before adding one, look at how DOSBox Staging, DOSBox-X, 86Box,
  PCem, MAME, Spice86 and MartyPC do it, and port or adapt the best (with attribution).
- **Games only reveal gaps:** each gap becomes a general feature with a test of its own, never a
  game-specific workaround.
- **Reference:** Clockwork is the reference the ports match. DOSBox Staging, which the commercial releases
  of many DOS games ship with, is the second opinion when a game behaves oddly.

### 4.2 Control protocol

Clockwork runs as a long-lived process (`clockwork serve`) and takes requests as JSON lines over stdin or a
TCP socket; requests can be batched ("run to the 500th sync point, then return these ranges"). The protocol is
a documented contract, `docs/protocol.md` (D58): a core any emulated machine can answer, and an extension per
platform (Clockwork's is `dos-pc`). An emulator says its revision and platform (`hello`), and a tool refuses
one it doesn't speak. Another machine (an Apple II, a Commodore 64) would be another emulator speaking the same
core; the toolkit would then need that platform's knowledge too (how its programs are found, analysed and
named, its boundary values), which it has for DOS only. Operations:

- load a program (with drives, CD, environment); run to a cycle, an address or the n-th hit of a sync point;
  step;
- read and write memory and registers, with addresses as image offsets, segment:offset or linear;
- inject input at exact virtual times;
- snapshots: in memory (cheap, for bisecting) and to disk (a versioned format);
- **who wrote:** a watch on the bus (as MAME's are) reports each read or write of the watched bytes with its
  instruction, cycle and call chain; host services and devices that change them are named. The chain is a
  shadow stack kept from what instructions do to the stack; tracking calls from the start of a run gives a
  watch started later the whole chain. Also the first read of memory nothing wrote;
- coverage (which instructions ran, which calls were made), memory-access tracing over a range of code,
  bounds checks on declared data (an access past a named table's end is reported: a quirk candidate);
- the loaded image: the program's memory after loading, unpacking or an extender's loader ran, with its
  segment or selector map, for analysis (§5);
- frames (indexed pixels and palette, at the moments the original presents a picture) and sound events
  (OPL and PC speaker writes, digitized sound starts), per tick.

A GDB remote-protocol face lets gdb and Ghidra's debugger attach to the running original.

**Built:** `clockwork serve` (stdin or a local TCP port) with loading and snapshots in memory and on disk, file
tracing (each read and write through DOS, with its code: D79),
runs for a time, to a sync point's next hit or to the next tick (any sync point's next hit), gated sync points
(D48), memory and registers, the input devices' state (D49), input, the frames (with their pictures as
indexed pixels and palette) and sound events of a span of time, watches with call chains, bounds checks,
uninitialized reads and image dumps (real mode in full; protected mode as the selector map and the segments it
bounds), coverage (any mode), the calls made by target (from the call tracker); and the GDB face
(`clockwork gdb`). A flat program under an
extender is dumped by `clockwork unpack`: its LE objects found where the extender's loader put them, at the
entry point, as an ELF file.

`clockwork run` and `clockwork play` stay for people: run headless, or play in a window with sound and record
the session.

## 5. Analysis

- **Identify:** every executable by hash; a port targets exact versions (CD or floppy release, patch level).
- **Load through Clockwork:** analysis starts from the loaded image Clockwork dumps, not the file, so packed
  programs, overlays and LE/LX programs under an extender take one path.
- **Ghidra, headless:** imports the image with its segment map, applies library signature packs, and its
  export (functions, calls, disassembly, decompiled code) lands in the knowledge files. A pack is made from a
  program built with the compiler and its linker map (`rrc sigs build`, `sigs/README.md`).
- **Dynamic:** sessions give coverage, call edges with counts (code Ghidra missed, calls through pointers),
  and per-function memory use (which functions read and write each global); `rrc analyze --session` adds the
  functions a session called that Ghidra lacks to its analysis (D79), and `rrc files` says which file bytes
  went where, read or written by which code (D78, D79).
- **Outside sources (D32, D77):** for every game, look for source releases, reimplementations, format
  documentation, wikis, walkthroughs and community bug lists, and use what licences allow. They are
  expected, and not relied upon: evidence named with its provenance (`prior-art:<source>`), which the
  original confirms or overrules.
- **Assets (D78, M5):** the game's files are a source too. File tracing says which bytes of which file the
  program put where; formats are knowledge (`knowledge/formats.toml`); `rrc asset` extracts pictures,
  palettes, sounds, music and levels, checked against what the original loaded.
- **IDs:** `<MODULE>[#<n>]+<offset>`, where `#n` is an overlay (MZ) or an object (LE/LX) and the offset is in
  the loaded image, uppercase hex, 5 to 8 digits. The same form names functions and data. The module `MEM`
  is memory outside the programs' images, by linear address (`MEM+0046C`, the BIOS's timer count; D53).

## 6. The port

### 6.1 Layers and seams

- **Simulation:** all game logic, stepped one tick at a time. Its state is one plain, serializable struct.
  It never sees the screen, the clock or the hardware.
- **Presentation:** rendering and sound. Reads the simulation through the seams below, never changes it; runs
  at any rate.
- **Platform:** window, input devices, audio output and file access (SDL3 on the desktop). It owns the main
  loop and calls the simulation and the presentation, so a web or mobile platform layer can be added without
  changing either.

A tick is one step of the original's main loop. Work the original did in interrupt handlers (music
sequencing, timer counts) is a subsystem of the simulation with its own rate; its effects are part of the
state at each tick.

The simulation meets the rest through three **seams**, which carry meaning, never pixels or hardware registers:

- **Scene:** what there is to draw: the camera, the map's geometry and textures, the entities with their
  positions, orientations and current sprite or animation frame, effects, and the interface (status bar,
  menus, text).
- **Sound events:** which sound or music plays, from where and how loud; music as notes and instruments, not
  register writes.
- **Actions:** what the player does (move, turn, use, fire), not scancodes. The platform turns devices into
  actions; the faithful mode derives them from the boundary values (§6.2) exactly as the original's input code
  did.

Backends sit behind the seams. The faithful renderer and sound backend reproduce the original's pixels and its
OPL and Sound Blaster writes from the scene and the sound events; a modern backend uses the same seams its own
way, for example Arena's map and entities drawn as 3D geometry. What a faithful backend needs beyond the meaning
(a raycaster's fixed-point precision, the order sprites are clipped in) is added to the seams as well, so a
backend never reads the simulation's state directly and a modern one can ignore what it doesn't need.

### 6.2 Time and inputs

A step takes the state and the tick's inputs. Inputs are **boundary values read from the original's memory at
each sync point**: the key-state array its logic reads, the elapsed time; and what a program reads from a
device's driver or port without storing it, as the device holds it then: the mouse driver's state, the
joystick's (D49). The port therefore never models a keyboard controller or a timer chip to be comparable.

- **Faithful mode** feeds the elapsed time the original measured, tick by tick, so state matches the original
  at every tick.
- **Modern mode** steps at a fixed rate and renders at any frame rate with interpolation. It deliberately
  differs from the original (moving three timer counts at once isn't moving one three times), so it is a
  behaviour toggle (§6.4) with its own tests.

### 6.3 Determinism

The simulation is deterministic: integer arithmetic as the original had it (fixed point stays fixed point),
no floating point unless the original used it, no wall clock, no iteration over hash maps, no threads. The
presentation may use floats and threads; it doesn't feed back.

### 6.4 Toggles

Faithful is the default. Each departure is a toggle of one of two kinds:
- **Presentation** (resolution, widescreen, interpolation, scaling, controls and rebinding): reads state,
  changes none; at its neutral setting the faithful comparison still passes.
- **Behaviour** (fixed-rate logic, raised limits, rule changes, quirk fixes): changes state, so it declares what
  it changes and has its own tests; comparisons run with it off.

Saves and replays record the toggles in force.

### 6.5 Quirk register

An original bug that the faithful mode keeps is a **quirk**: an entry in the port's `quirks.toml` (an ID, what
happens, the evidence, the original's code and the port's site) and an explicit branch in the code, kept by
default, with a behaviour toggle that fixes it. Comparisons always keep every quirk. Sources: differences
where the port did what the code clearly means and the original didn't; Clockwork's bounds checks and
uninitialized reads; known bug lists. A quirk whose effect depends on the original's memory layout (a read past
a table returns its neighbour) reproduces that neighbour's data deliberately. `rrc suspects` lists a session's
candidates from Clockwork's checks, `rrc quirk` keeps the register and finds each quirk's branch by its name, and
the port branches on `rrc_runtime::keeps(NAME)` (D74).

### 6.6 Code rules

- Storage limits are not game rules: segments, near and far pointers, EMS paging and overlays disappear;
  fixed sizes (150 actors, 320×200, 70 Hz) become named constants with the original value as default.
- 16-bit wrapping stays only where behaviour depends on it, with explicit helpers, marked at its site.
- Each function names the original it reimplements (its ID) in a comment: provenance, not structure.
- Fidelity traps to watch: 16-bit wraparound, shift counts masked to 5 bits, division by zero and IDIV
  overflow, flag-dependent code (carry as an error return), signed against unsigned comparisons, uninitialized
  reads, code generated at run time (reimplemented as what its generator produces), busy-wait timing loops.

## 7. Comparison

- **Sessions:** input scripts, recorded play (`clockwork play --record`), and the game's own demo recordings.
- **Sync points:** addresses in the original where a tick ends (one per loop: menus, play, demo playback),
  where the loop's wait is over and the pass hasn't read its inputs yet (D47); a session's timeline is its
  sequence of sync hits. A loop that polls as fast as the CPU runs, with no wait, has a gated sync point: a
  hit is a tick only when what the loop reads changed since its last tick (D48). Input that changes between
  a sync point and the pass's read of it is flagged when recording (D60). Hits count only while the
  game's program runs, or a program it started (D57): a launcher may run other programs, whose code can sit
  at the same addresses, before the game.
- **State mapping** (`mapping.toml` in the port): port field to original location and encoding (sizes,
  signedness, fixed point, packed bits, arrays); a pointer in the original becomes the index of the element it
  points to or the ID there, and data on the heap is mapped through the pointer that finds it at each tick
  (D72); fields that may differ (padding, caches, driver internals) are declared `ignore`, so nothing differs
  silently. The mapping works both ways: it reads the original's state for
  comparison and builds the port's state for a transplant.
- **Traces:** per tick, the mapped state of the original (recorded once per session through Clockwork, cached
  outside git) and of the port; comparing two traces needs no emulator.
- **Files, not code:** the toolkit and a port exchange the session's boundary values, traces and port states as
  files in documented formats, so a port in any language can be compared; Rust ports get this from the runtime.
- **First divergence:** the tick and the fields that differ, then *who wrote* on the original's side; on the
  port's side, its code for the function that wrote it, found by the ID it names (§6.6, D73).
- **Frames and sound:** in faithful mode, the port's rendering of each tick is compared with the original's
  frame as indexed pixels and palette, and sound as events per tick, not samples.
- **Progress:** the share of the original's executed code exercised by sessions that match, and the share of
  its state the mapping covers.

## 8. Save states and checkpoints

- **Clockwork snapshots** (full machine, to disk) and **port states** (the simulation struct, serialized).
- **Paired checkpoint:** a Clockwork snapshot, the port state, the session and tick, taken where both match.
- **Transplant:** the port's state built from the original's memory through the mapping. A gap in the mapping
  shows as a divergence right after loading, which names what's missing.
- Uses: resume a comparison anywhere instead of from the start; bisect between checkpoints; scenario tests
  (a checkpoint plus seconds of input); start the port mid-game before menus and loading are ported; explore
  from many points; save anywhere for players.
- **Scenario sessions** (D59): a session may set a scenario up instead of playing to it: from power-on or a
  snapshot, at a tick it writes values into the original's memory (named data, through the knowledge's types),
  then runs its input. The edits are part of the session, so its runs stay repeatable, and the trace lists
  them. Clockwork writes them when their ticks are counted, so every run of the session makes them (D64). Only
  the original is edited; the port reaches the same scenario through a transplant of the fields they wrote. A game's own
  cheats or debug modes, where it has them, reach a scenario through its own code and come first. An edited
  state may be one play can't reach (a count changed without its list): edits go through the game's
  structures, and a divergence right after one is suspected of the edit before the port.
- **Session snapshots** (D51): `rrc record` keeps a snapshot every 10 emulated seconds in the cache, keyed by
  what makes the run and its ticks the same; questions about a tick (`rrc watch`, `rrc inputs`) start from
  the last one before it. Answers about a whole session (`rrc calls`, `rrc inputs`) are kept by the run and the
  knowledge they read.

## 9. Knowledge and the port repository

Everything learned about a game is **text files in its port repository**: names and types of functions and
data (with evidence and provenance), the mapping, sync points, quirks, sessions. Databases and Ghidra
projects are caches rebuilt from them. Large data (program images, Ghidra exports, snapshots, traces) lives in
a content-addressed cache outside git, referenced by hash.

```
rrc.toml            the game, its versions (each file by SHA-256), how Clockwork runs the original
rrc.local.toml      this machine: the game's folder, the tools (not committed)
knowledge/
  functions.tsv     named functions: ID, name, provenance, confidence, evidence
  data.tsv          named data: ID, name, size, type, provenance, confidence, evidence
  types.toml        structures and enumerations
  ticks.toml        sync points (where a tick ends) and boundary values (what a tick reads)
sessions/           a TOML file per session (its inputs, its length, its recorded trace's hash)
```

- Tables have one item per line, sorted by ID, so parallel changes merge.
- **Provenance** says who decided an item and from what: `human`, `agent:<model>`, `ghidra`, `library:<pack>`,
  `prior-art:<source>`, `import:<source>`. A tool's name never replaces a decision someone made.
- The formats are in `docs/formats.md`. `rrc check` validates the files: IDs well formed and inside the
  program, names unique, sizes consistent.
- **The cache** is per user, outside git, addressed by SHA-256. A recorded trace's hash and its run's are
  committed with its session, so re-recording it on another machine checks that the run is the same there.

**The toolkit, `rrc`**, is one Rust program run inside a port repository. It drives Clockwork as a child
process through `clockwork serve`. Its commands are short and print little: `init`, `doctor`, `status` (the
next step), `game` (the game's folder and its version), `analyze`, `check`, `name`, `data`, `sync`, `input`,
`show`, `session`, `record`, `calls` (how often a session calls each function: loops and their ticks), `inputs`
(what the program reads that interrupt handlers or services write: boundary value candidates), `watch` (who
writes or reads a value, with the call chain), `search` (memory by how it behaves over ticks), `files` (which file bytes went where), `ports`,
`frame`, `trace`, `compare`, `quirk`, `suspects` (quirk candidates) and `sigs`.

**Analysis** (`rrc analyze`) takes the program's image from Clockwork (D27). For a real-mode program it is
the program as Clockwork unpacks it (an MZ file with the relocations it recovered, so Ghidra's MZ loader sees
the segments); for a flat program under an extender, its LE objects where the extender's loader put them, at
the entry point (an ELF file, D55), whose IDs name the objects. When a launcher starts the game's program,
that program is the one analysed and named (`[original] module`, D57), found where each session puts it. Ghidra imports it headless, applies the
signature packs and exports functions, calls, disassembly and decompiled code (`ghidra/RecExport.java`) into
the cache; `rrc show` puts that next to the knowledge. Two analyses with the same options find the same functions, but the options
change them (decompiling adds one on Wolf3D: 778 functions against 777), so the cache keeps the analysis of a
program by its hash, the export script and the options, and later runs reuse it. Its Ghidra project is kept
too: functions the knowledge names that Ghidra missed are added to it without analysing the program again.

**Traces** (`rrc record <session>`): Clockwork runs the session and stops at every sync point hit, a tick;
at each it reads the boundary values and the named data, and takes the frames and sound events since the last
tick. The trace is a file of JSON lines in the cache (`docs/formats.md`): a header naming the items, then one
line per tick with the inputs and the data that changed, the frames (each with its picture, kept in the
cache) and the sound events, then an end line with what the run did after the last tick and the machine's
state hash. Besides the trace's hash, the session commits the run's (D60): of the frames, the sound and the
end, whatever the knowledge reads, so a re-recording tells a knowledge change from a different run. A tick
also lists the inputs its pass read after they changed since its sync point (D60).

## 10. The agent's workflow

The agent follows `docs/porting.md`, its rules first ("How to work", D77), and drives `rrc`:

1. Set up the port repository (`rrc init`); `rrc doctor` names what's missing.
2. Identify and load the game (`rrc game`), analyse it (`rrc analyze`), look for outside sources and the
   game's formats (§5).
3. Record sessions; find the main loop and the sync points; record the boundary values (`rrc record`).
4. Name the state and build the mapping, starting with what the first subsystem reads.
5. Port a subsystem; compare from a transplanted checkpoint; at each divergence, *who wrote*, then fix the
   port or register a quirk.
6. The game's data and the rest of the program: the port loads the game's files itself and runs from
   power-on through its menus and loading.
7. Faithful presentation, compared frame by frame and by sound events, and the platform layer, so the port
   plays; then modern mode.

Subagents work in parallel on separate subsystems. Output is short: agents read it.

## 11. Taken over from Recreate

Rrecreate began from Recreate, the person's earlier project, and took over what is listed here. Recreate is
disregarded (D69): nothing more is taken from it, and nothing is read from it.

- **Clockwork** is Recreate's `rec-host` (`host/cpu`, `host/dos`, `host/machine`, `host/frontend`), with its
  CPU tests and the SingleStepTests runner, minus what hosted the old ports inside the emulator: overrides and
  the `Call` interface, dual-run, captured-call replay, poison checks and lockstep with overrides. Kept and
  extended: snapshots, coverage, memory-access tracing, contract inference.
- The Ghidra export script, the library signature packs and how they are made (`rec sigs build`, now `rrc sigs
  build`, with its linker map parser), the test programs built with Open Watcom.
- Recreate's Wolf3D knowledge (names transferred from id's released source, globals, its 46 overrides written
  from that source) and its Arena knowledge aren't used, and `rrc` has no import of them (D46, D66, D67);
  nor is the person's earlier Arena project.
- Not taken: the Python `rec` toolkit and its SQLite store, the task queue and packets, `rec submit`/`rec land`,
  Python ports.

## 12. Plan

| Milestone | Contents | Done when |
|---|---|---|
| M0 Repository | Clockwork imported and cut down; workspace, tests, CI | Clockwork's tests pass; Wolf3D, Arena and Daggerfall reach their checkpoints as before |
| M1 Control | `clockwork serve`, snapshots to disk, sync points, who wrote, bounds checks, loaded-image dumps | A script finds who wrote a given Wolf3D variable at a given tick |
| M2 Toolkit | `rrc`: port repositories, analysis into knowledge files, sessions, boundary values, traces | A session of the Wolf3D port is traced |
| M3 Runtime and comparison | The simulation step API, the mapping (both directions), transplant, paired checkpoints, scenario sessions, the comparison and its report | A toy port of a test program matches tick by tick |
| M4 A real game's logic | The toolkit through a real game's gameplay, from its own analysis (D66): finding and mapping its state (with the encodings D62 left for later: a pointer as an index, data on the heap, packed bits), a transplant at a checkpoint, the comparison and who wrote on a large state, the port's code for the original's writer at a divergence (D73), the quirk register's tools (§6.5), and each gap the port work finds, fixed as a general feature. Tested on Arena's first dungeon (D65, D70): the player's movement and collisions, doors, creatures and combat, the random generator, the game's clock. The presentation is M6's | A session of play in Arena's first dungeon (walking, doors, a fight) matches tick by tick from the transplant, and the gaps found are fixed in the toolkit: the approach works on a game without a source release |
| M5 The game's data: its assets and the whole program | File tracing (which bytes of which file go where in memory), formats as knowledge, extraction of assets (pictures and palettes, sounds, music, levels; common compressions) checked against what the original loaded (D78); the runtime's file access, so a port reads the person's copy of the game; sessions from power-on through several loops (menus, loading, play), compared as one. Tested on a test program, and on Arena where that isn't enough (D70) | A port of a test program that loads its data files matches from power-on through two loops; Arena's level map and a picture extracted from its files equal what the original loaded into memory |
| M6 Faithful and playable | Frame and sound-event comparison, the presentation seams (§6.1), a faithful renderer and sound, and the platform layer: window, input devices turned into actions, audio output, the game's folder, saved games. Tested on Arena | A port's frames and sound events are compared with the original's and a difference is located; Arena's dungeon port plays in a window with sound, its frames matching through the session |
| M7 Modern mode | Fixed-rate logic with interpolation, free frame rate and resolution, toggles (§6.4) and quirk fixes, tested on Arena | The modern toggles pass their own tests, and the faithful mode still matches |
| M8 32-bit games | The whole flow on protected-mode programs (LE/LX objects) under CauseWay and DOS/4GW: analysis, sessions, mapping, transplant, comparison, frames. Tested on Daggerfall and Ultimate Doom (a test bench: its source is released, D52) | A session of each game's play matches tick by tick from a transplant |
| M9 Acceptance | An agent with `docs/porting.md` alone, as in the blind tests (D46), ports a stretch of a game without a source release that the toolkit wasn't developed on, chosen by the person (D71) | Its port matches a session of play, state and frames, and plays; what the agent needed and didn't find is in the toolkit: the toolkit is complete |

The games are where the toolkit is tested: their ports go only as far as the tests need (D70). M9's game stays
out of the toolkit's development until then. 32-bit protected-mode programs are designed in from M1 (IDs,
protocol, mapping).
