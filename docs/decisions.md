# Decisions

Each decision once, with its status. `accepted` decisions are what `DESIGN.md` describes. A changed decision
is marked `superseded by Dn` and the new one is added; the design is updated in the same commit. `open`
decisions wait for the person.

"With the person" marks decisions the person made or confirmed; the rest were made by the agent within what
the person asked for and can be revisited freely.

## The product

### D39. A faithful Rust port, built to be modernized. Accepted 2026-10-19, with the person.
What Rrecreate produces for a game is a Rust port as close to the original's behaviour as can be checked, whose
graphics, sound, performance and platforms can be modernized without touching its logic: Arena's faithful port,
for example, should be able to get a real 3D renderer.
**Why:** the faithful port is the base for modern versions of the game, not the end; behaviour that can't be
checked against the original is lost, and logic tangled with its presentation can't be modernized.

### D52. The targets are games whose source was never released. Accepted 2026-10-21, with the person.
Rrecreate is for games without a source release. A game whose source is out, such as Wolf3D, is a test bench:
its source grades the toolkit afterwards (D3), and its port goes only as far as testing the toolkit needs; it
doesn't have to become a finished game.
**Why:** a released source already lets people port such a game; what is missing is a way to port the games
whose source is lost or was never published.

## Approach

### D1. Port by behaviour, compared with the original as a whole program. Accepted 2026-10-19, with the person.
The port is a new native program; the original's behaviour is the specification; port and original are compared
tick by tick by mapped state, frames and sound.
**Why:** Recreate's hybrid approach (Rust overrides replacing the original's functions inside the emulator,
each checked by dual-run) verified functions well, but never became native: the emulator had to host the port,
the port had to mirror the original's structure call for call, and the per-function ceremony cost about 1 M
tokens a function (Wolf3D has 842). Recreate's first design also compared whole programs and was dropped for
poor localization; per-tick state comparison through the mapping (D16), "who wrote" (D8) and checkpoints
(D17) are what localize a difference now.
**Supersedes:** Recreate's "hybrid port, verified per function".

### D2. Decompiled code is a hint, not a source. Accepted 2026-10-19, with the person; how it works, not how it runs: D77.
Ghidra's output is read for how the original does something; nothing is translated from it mechanically.
**Why:** it is partly wrong (missed functions, lost segments, dropped flags, guessed types) and incomplete (no
hardware layer, no runtime), so a function-by-function translation can't produce a working program.

### D3. The honest pilot. Superseded by D65.
The pilot is Wolf3D, with id's released source kept out of the agent's reach and used only to grade the result
afterwards; the real test follows on a small game with no source release. Arena and Daggerfall are not test
ports: the person ports them with the finished toolkit (decided 2026-10-17).
**Why:** a source release lets the agent take shortcuts, even unintentionally, and then the pilot proves nothing.

### D65. The pilot is Arena. Accepted 2026-10-26, with the person; its prior art: D66; its purpose: D70.
M4 ports Arena's gameplay (`ACD.EXE`, the CD version) instead of Wolf3D's: its first dungeon, from a transplant
at its start, compared tick by tick. Arena has no source release, so the pilot is also the test where the
approach is needed, which M7 was to be; M7 goes. Like any port it starts from prior art (D32): Recreate's Arena
knowledge (`rrc import recreate`), the person's earlier Arena project (its notes and names as evidence; copying
its code needs the person's OK) and what OpenTESArena documents of the formats. Nothing grades it but the
comparison with the original. The pilot is the start of Arena's port (D39), not a throwaway test, though M4
covers only the first dungeon's gameplay. Wolf3D stays what the toolkit's tests have used (D46-D51, D60): a
test bench outside the plan.
**Why:** the person doesn't care about a Wolf3D port and chose Arena. A source-graded pilot showed whether an
agent kept away from the source; the comparison already shows whether a port behaves as the original does, and
Arena's port is wanted for itself. Arena already runs in Clockwork to its first dungeon
(`testdata/inputs/arena-first-dungeon.inputs`), with its mouse as a boundary value (D49).
**Supersedes:** D3.

### D70. The goal is the complete toolkit; Arena is a game it is tested on. Accepted 2026-10-27, with the person; its milestones: D71.
Rrecreate's goal is the toolkit: everything an agent needs to port a DOS game whose source was never released,
from analysis to a faithful port and its modern mode. No game's port is a goal of its own. Arena is where M4 to
M6 test the toolkit, because it has no source release and already runs in Clockwork; like Wolf3D's (D52), its
port goes only as far as those tests need and doesn't have to become a finished game. A milestone is done when
the toolkit can do its part, shown on a game; work that would only advance a game's port isn't planned. M7 is
now 32-bit protected-mode games, tested on Daggerfall: most DOS games the toolkit is for need it.
**Why:** the person: Arena is not the goal, the complete toolkit is; Arena is used where it helps finish the
toolkit.
**Supersedes:** D65's "the pilot is the start of Arena's port (D39), not a throwaway test" and "Arena's port is
wanted for itself" (the rest of D65 stands).

### D71. The plan to a complete toolkit: M4 to M9. Accepted 2026-10-27, with the person.
- **M4, a real game's logic** (Arena's first dungeon), now with the mapping's encodings D62 left for later
  (a pointer as an index, data on the heap, packed bits), the port's own call trace at a divergence (the
  port's code for the original's writer instead: D73), and the quirk register's tools.
- **M5, the game's data and the whole program:** sessions from power-on through several loops (menus,
  loading, play), which bytes of which file end up where in memory, and the runtime's file access; tested on
  a test program where one is enough.
- **M6, faithful and playable:** frames and sound compared, and the platform layer (window, input devices,
  audio, the game's folder, saved games).
- **M7, modern mode.**
- **M8, 32-bit games** under CauseWay (Daggerfall) and DOS/4GW (Ultimate Doom, a test bench: its source is
  released).
- **M9, acceptance:** an agent with the playbook alone ports a stretch of a game without a source release that
  the toolkit wasn't developed on, chosen by the person.

**Why:** set against the porting flow end to end, M4 to M7 had nothing for loading the game's data or running
from power-on (a transplant skips both), nothing for a port that can be played, and every test on one game:
that would shape the toolkit around Arena and leave "complete" (D70) without a finish line. The blind Wolf3D
tests showed that an agent with the playbook alone finds what the toolkit's authors don't (D46-D51).
**Supersedes:** D70's milestone numbers (32-bit games were M7; Arena tests M4, M6 and M7, and M5 where a test
program isn't enough).

## The emulator

### D4. Our own emulator, Clockwork, runs the original. Accepted 2026-10-19, with the person.
**Why:** the comparison needs runs that repeat bit for bit, inputs at exact virtual times, stops at given
instructions, snapshots and memory watches. DOSBox Staging paces its main loop by the host clock and its speed
adapts to the host, and has neither save states nor a control interface; DOSBox-X has save states but the same
timing; DOSBox Pure's runs aren't known to repeat (no netplay); Spice86 is real mode only; MAME runs DOS games
poorly. Retrofitting determinism is the hard part, and Recreate's emulator has it already and runs Wolf3D,
Arena and Daggerfall.

### D5. Adopt emulator features from other emulators. Accepted 2026-10-19, with the person.
Before adding a feature, look at how DOSBox Staging, DOSBox-X, 86Box, PCem, MAME, Spice86 and MartyPC do it,
and port or adapt the best, with attribution (`THIRD_PARTY.md` and the module's docs).

### D6. Clockwork is the reference; DOSBox Staging is the second opinion. Accepted 2026-10-19.
Ports match Clockwork. When a game behaves oddly, key sessions are checked against DOSBox Staging, which the
commercial releases of many DOS games ship with.

### D7. Clockwork is a long-lived process with a control protocol. Accepted 2026-10-19, with the person.
JSON lines over stdin or TCP, with batched requests; a GDB remote-protocol face for gdb and Ghidra's debugger.
**Why:** investigation needs a running session (run, stop, read, snapshot, restore, watch), not one command per
run; batching keeps per-tick comparison cheap; GDB is a protocol existing debuggers speak.

### D8. "Who wrote this value" is a first-class tool. Accepted 2026-10-19, with the person.
Clockwork reruns the original from a snapshot with a write watch and reports the instruction, function, call
chain and cycle of each write, and the first read of memory nothing wrote.
**Why:** it turns "this field differs at tick 412" into "this code wrote it", which is what the agent fixes from.

### D41. Snapshots: the machine without the game's files, refused across versions. Accepted 2026-10-19.
A snapshot is the whole machine state (serde, postcard, deflate) behind a header with a format number,
Clockwork's version and the state hash. The game's files and the CD image are inputs, given again on load; only
what the program changed on the drive is stored. A snapshot from another format or version is refused, and the
loaded machine must hash as the saved one did. Records (what a run produced: the sound devices' logs, frames,
console output, diagnostics) are history, not state: they are in neither the snapshot nor the state hash. A
loaded machine's records start empty, with what its sound devices are playing logged again (the OPL's
registers, the speaker's waveform, the CD track), so its sound renders from the load on.
**Why:** DOSBox-X's states broke between builds made with different compilers; a format that can load as a
subtly different machine would make every comparison after it meaningless. Storing the game's files would
make Daggerfall's snapshots hundreds of megabytes instead of two, and storing records would make a snapshot an
hour into a game carry an hour of sound; a state hash that covered history would differ between two machines
that will behave the same.

### D9. Clockwork stays deterministic. Accepted (carried from Recreate).
Virtual time from executed instructions, no wall clock, no randomness, no host-dependent floating point; any
difference between two runs is a bug.

### D10. Games only reveal gaps. Accepted (carried from Recreate).
Each gap becomes a general feature with a test of its own, never a game-specific workaround.

## Repositories and licences

### D11. Toolkit and emulator in one repository; each port in its own. Accepted 2026-10-19.
**Why:** Clockwork's protocol and the toolkit will change together for a while, so one commit should change
both; a port holds code derived from a copyrighted game, so whether it is published is the person's call per
game, apart from the toolkit. Clockwork can be split out later with `git subtree split`.

### D12. Licences: MIT, Clockwork GPL, a process boundary between them. Accepted 2026-10-19.
The toolkit and the runtime are MIT; Clockwork is GPL-2.0-or-later (it contains code ported from DOSBox). The
toolkit only talks to Clockwork through its protocol and the runtime contains no Clockwork code, so ports choose
their own licence. The person said licences matter little; the boundary stays because D7 wants it anyway.

## The port

### D13. Three layers from the start: simulation, presentation, platform. Accepted 2026-10-19, with the person.
**Why:** a port shaped around matching frames would block modernization (frame rate separate from update rate,
logic independent of the CPU's speed). Comparing frames is a mode of the presentation, not a constraint on the
structure.

### D40. Presentation through seams that carry meaning. Accepted 2026-10-19, with the person (carried from Recreate, 2026-10-17).
The simulation meets the presentation and the platform through a scene (camera, map geometry, entities, interface),
sound events (sounds and music as notes, not register writes) and actions (not scancodes). Faithful and modern
backends implement the same seams; what a faithful backend needs beyond the meaning goes into the seams too.
**Why:** a modern backend, up to a real 3D renderer, can only be written against what the world contains, not
against the original's pixels; keeping backends off the simulation's state keeps both kinds interchangeable.

### D14. Time is an input; faithful and modern modes. Accepted 2026-10-19, with the person.
A step takes the elapsed time; faithful mode feeds what the original measured, modern mode a fixed rate.
**Why:** the original's logic depends on how many timer ticks passed between frames, which depends on the CPU's
speed; making it an input reproduces the original exactly and frees the modern mode.

### D15. Inputs are boundary values read from the original's memory. Accepted 2026-10-19, with the person.
At each sync point the toolkit reads what the original's logic sees (key states, elapsed time, mouse) and feeds
it to the port's step.
**Why:** the port never has to model the keyboard controller or the timer chip to be comparable, and gets a clean
input model.

### D16. The state mapping. Accepted 2026-10-19, with the person.
Declared per port: fields to original locations and encodings, pointers translated to indices and back, fields
that may differ declared `ignore`. It works in both directions (comparison and transplant).

### D17. Save states and transplants. Accepted 2026-10-19, with the person.
Clockwork snapshots to disk, serialized port states, paired checkpoints, and the port's state built from the
original's memory through the mapping.
**Why:** comparisons start anywhere instead of from the beginning; divergences are bisected; scenario tests; the
port runs mid-game before menus and loading exist; gaps in the mapping show as immediate divergences.

### D18. The quirk register. Accepted 2026-10-19, with the person; its tools: D74.
Original bugs are kept in faithful mode as registered quirks with a toggle to fix each; comparisons keep them all.

### D74. The quirk register's tools. Accepted 2026-10-27.
`quirks.toml` in the port repository lists each quirk: its name (how the port's code names it), the original's
code that does it (an ID), what happens, what fixing it does, provenance and evidence. `rrc quirk` lists them,
each with the port's code that names it, and adds, changes or removes one; `rrc check` checks the register.
`rrc suspects SESSION` runs the session with Clockwork's bounds checks on all named data and its
uninitialized-read tracking: the instructions that reach past named data's ends, and those that read memory
nothing wrote, with when (the pass to tick N), registered ones marked. An instruction past three or more named
data is a sweep (a clear or a copy running over them) and is said once. At a divergence, a quirk registered at
the writing instruction or a function of its chain is named. The port branches on `rrc_runtime::keeps(NAME)`,
true unless the run fixes it (`rrc_runtime::fix`), which a comparison never does.
**Why:** D18 and DESIGN.md §6.5: candidates come from Clockwork's checks, and each kept bug is found from both
sides by its name and its ID. Measured on a test program: an index one past a table and a read of unwritten
memory are found and registered. On the toy game the C runtime's clearing of its data at start is one sweep,
not six suspects.

### D19. Toggles of two kinds. Accepted (carried from Recreate, with the person, 2026-10-17).
Presentation toggles read state only and keep the faithful comparison passing at their neutral setting; behaviour
toggles change state, declare what they change and have their own tests; saves and replays record them.

### D20. Readable code rules. Accepted (carried from Recreate, with the person, 2026-10-17).
Storage limits are not game rules (named constants with the original values); 16-bit wrapping only where
behaviour depends on it; provenance comments with the original's IDs.

### D21. The simulation is deterministic. Accepted 2026-10-19.
Integer arithmetic as the original had it, floats only where the original used them, no wall clock, no hash-map
iteration order, no threads; the presentation may use all of these.

### D22. Interrupt handlers' work is a subsystem of the simulation. Accepted 2026-10-19.
Music sequencing, timer counts and the like run at their own rate inside the simulation; their effects are part of
the state at each tick.

## Comparison

### D23. Sessions include the game's own demos. Accepted 2026-10-19, with the person.
Input scripts, recorded play and demo recordings (Wolf3D plays them on its title screen) are all sessions.

### D24. Progress is matched coverage. Accepted 2026-10-19, with the person.
The share of the original's executed code exercised by matching sessions, and the share of its state the mapping
covers.

### D25. Frames as indexed pixels, sound as events. Accepted 2026-10-19.
Faithful frames compare as indexed pixels plus palette at the moments the original presents a picture; sound as
events per tick (OPL and PC speaker writes, digitized sound starts), not samples.
**Why:** exact, cheap, and indifferent to mixing and resampling.

### D26. Traces are cached; comparisons without the emulator. Accepted 2026-10-19.
The original's per-tick mapped state is recorded once per session and cached; port regressions compare against it
without running Clockwork.

## Analysis and knowledge

### D27. Analysis starts from Clockwork's loaded image. Accepted 2026-10-19.
Ghidra imports the memory image after loading, unpacking or an extender's loader ran, with its segment or selector
map.
**Why:** packed programs, overlays and LE/LX programs under extenders take one path, and the offsets match what
Clockwork reports.

### D28. 32-bit protected mode is designed in from the start. Accepted 2026-10-19, with the person.
IDs, the protocol, coverage and the mapping handle protected mode from M1.
**Why:** Recreate's pipeline was real-mode only, and most of the person's library (Daggerfall, Battlespire,
Redguard, Ultimate Doom) is 32-bit.

### D29. IDs. Superseded by D53.
`<MODULE>[#<n>]+<offset>`: `#n` is an overlay (MZ) or an object (LE/LX); the offset is in the loaded image,
uppercase hex, 5 to 8 digits; functions and data alike.

### D53. IDs, and `MEM` for memory outside the images. Accepted 2026-10-21, with the person.
`<MODULE>[#<n>]+<offset>`: `#n` is an overlay (MZ) or an object (LE/LX); the offset is in the loaded image,
uppercase hex, 5 to 8 digits; functions and data alike. The module `MEM` (no parts) is memory outside any
program's image, its offset the linear address: `MEM+0046C` is the BIOS's timer count, `MEM+004F0` the
intra-application area.
**Why:** games read memory that isn't theirs: the BIOS's timer count and keyboard buffer (where INT 16h's keys
are), the interrupt vectors, other programs' memory (the first blind test found Wolf3D reading 0000:04F0).
Only IDs in the program's image could be named, so these couldn't be inputs or data. A reserved module keeps
one form for everything, so every tool that takes an ID takes these; a linear address, unlike `SEG:OFF`, has
one spelling and carries over to protected mode.
**Supersedes:** D29.

### D30. Game versions by hash. Accepted 2026-10-19, with the person.
A port targets exact executables; each supported version is identified by its hash.

### D31. Knowledge is text files in the port repository. Accepted 2026-10-19, with the person.
Databases and Ghidra projects are caches; large data sits in a content-addressed cache outside git.
**Why:** in Recreate the SQLite store lived outside the repository, and a fresh clone didn't have the port's names.

### D32. Prior art first. Accepted (carried from Recreate).
For every game, look for source releases, reimplementations (Daggerfall Unity, OpenTESArena, ECWolf), format
documentation and bug lists; import what licences allow, as evidence. Games that share an engine share its port.

### D33. Game files are never committed. Accepted (carried from Recreate).
Ports load the person's own files and check them by hash.

## Process

### D34. The agent drives; no program runs the agent. Accepted (carried from Recreate, with the person, 2026-10-18).
The toolkit checks, keeps the books and says what's next; parallel work is the agent's own subagents, on separate
subsystems.

### D35. One design document and this list. Accepted 2026-10-19, with the person.
**Why:** Recreate's long design document kept descriptions of superseded decisions next to current ones, which
confused readers, agents included.

## Languages and platforms

### D36. Rust, with room for other languages. Accepted 2026-10-19, with the person.
Rust for Clockwork, the toolkit, the runtime and the ports; Java for the Ghidra script. Other port languages stay
possible through D38: the Rust runtime is a convenience, not a requirement.
**Supersedes:** Recreate's output-language setting (Python or Rust, 2026-10-17).

### D37. Desktop now, other platforms possible. Accepted 2026-10-19, with the person.
The target is the desktop (Windows, Linux, macOS) with a permissive platform library (SDL3, zlib licence). The
platform layer owns the main loop and all file access, and calls the simulation and the presentation, so a web
(WASM) or mobile platform layer can be added without changing either.

### D38. The toolkit and a port exchange documented files, not code. Accepted 2026-10-19.
The session's boundary values, per-tick state traces and port states for transplants are files in documented
formats, so a port in any language is compared by reading and writing them.

## The toolkit

### D42. One program, `rrc`, run in the port repository. Accepted 2026-10-19.
The toolkit is a Rust command-line program (MIT) that finds its port repository by `rrc.toml` and drives
Clockwork as a child process through `clockwork serve`.
**Why:** D36 and D12; one binary needs no Python environment (Recreate's `rec` did), and an agent runs it from
where it works.

### D43. Knowledge tables are TSV, the rest TOML. Accepted 2026-10-19.
Functions and data are tab-separated tables, one item per line sorted by ID; types, sync points, boundary values,
sessions and the port's settings are TOML. Each item carries its provenance; a tool's name never replaces a
decision someone made.
**Why:** a Wolf3D has 800 functions: one line each keeps diffs and merges small (TOML would take five lines an
item), and Recreate's knowledge was already in this form. Nested things read better as TOML.

### D44. A per-user content-addressed cache; committed hashes check determinism. Accepted 2026-10-19.
Images, Ghidra exports, snapshots and traces are stored by SHA-256 outside git (`RRC_CACHE`, or the user's
cache folder). A session's recorded trace hash is committed; recording it again anywhere must give the same hash.
**Why:** D31 keeps large data out of git; committing the hash turns every re-recording into a check that
Clockwork runs the session the same way on another machine.

### D46. The Wolf3D port starts from its own analysis. Accepted 2026-10-19, with the person.
Recreate's Wolf3D knowledge (every function name transferred from id's released source, data named with it at
hand) and its overrides stay out of the Wolf3D port; the port starts from Clockwork, Ghidra and the signature
packs, and Recreate's knowledge grades the pilot afterwards, with id's source. The port repository is local for
now; its rrc.toml refuses imports (`[port] imports = false`), Rrecreate's examples use a made-up program, and
the pilot's agents follow the rules in docs/porting.md ("An honest pilot"). The pilot's copy of the game is
in a folder of its own, outside Recreate's tree (2026-10-21). Rrecreate's public history, whose commit messages
carry names from Recreate, stays as it is: the pilot doesn't read it (2026-10-21, with the person: a rewrite
would need a force-push).
**Why:** D3. Knowledge derived from the source would let the pilot take shortcuts, and it would prove nothing.
**Supersedes:** the plan (DESIGN.md §11, M2) to give the Wolf3D port Recreate's knowledge as evidence.

### D47. A tick ends where the wait is over. Accepted 2026-10-20.
A sync point sits where the loop's timer wait has ended and the pass hasn't read its inputs yet (usually the
waiting function's return), so a tick's inputs and state are what the pass starting there reads. A loop that
ends its tick in two places (play, and demo playback with its own wait) gets a sync point for each.
**Why:** the first blind test (the Wolf3D port's notes/toolkit-report.md) measured it: with the sync point at
the wait's entry, the elapsed time was the previous pass's and a key changing during the wait reached the pass
but not the tick; at the wait's exit neither happens, and the run is the same.

### D48. Loops that poll as fast as the CPU runs get gated sync points. Accepted 2026-10-21, with the person.
A sync point may name what its loop reads (`when_changed`: inputs or named data, the counter its timing reads
among them); a hit is a tick only when one of them differs from what it was at that point's last tick. The
hits passed over are counted (`skipped`), so a pass count that matters stays visible.
**Why:** the first blind test found menus and title screens polling about 19000 times a second with no timer
wait: a sync point at the poll gave about 253000 ticks for 13 s of menus, which measure the emulated CPU's
speed, not the game, and recording didn't finish. Such a loop's behaviour depends only on what it reads, so a
tick when that changed keeps every tick that matters. Measured on the Wolf3D port's first-level: 518 menu ticks
instead of 253000, recorded in 21 s, the frames identical. The alternative, no ticks for menus (compared some
other way), would have left menus outside the comparison.

### D49. The mouse and the joystick are boundary values as their devices hold them. Accepted 2026-10-21.
An input may be a device instead of an address: `mouse` is what the mouse driver would report at the sync
point (buttons and position as INT 33h 03h gives them, the motion 0Bh would return, the press and release
counts and positions of 05h and 06h); `joystick` is the game port's sticks (buttons and positions). Clockwork
gives them as bytes in a documented layout, so the trace stores them like any other input, and a gate can
name them.
**Why:** the first blind test found that Wolf3D never stores the mouse or the joystick: INT 33h answers in
registers kept in stack locals, port 201h is timed by the program, and Clockwork's mouse driver keeps its state
outside emulated memory. `[[input]]` could only name memory, so a pushwall tick that turned the player by the
mouse had no input that explained it. The driver's state at the sync point is what the pass starting there
reads, as with memory inputs (D47). Recording each service call's registers per tick, the alternative, would
belong to the pass after the tick and tie the format to INT 33h's functions.

### D50. Traces, version 2: inputs when they change, and a run hash apart from the trace's. Superseded by D54.
A trace writes an input when its value differs from when it was last read, as it does state, and an input may
be read only at some sync points. An item that is an input at every tick isn't written again as state. The
run ends at exactly the session's length, and the end line has the frames and sound after the last tick and
Clockwork's state hash then. The session commits a second hash, of the run (every frame and sound event, how
and when it ended, the state hash), which doesn't depend on what the knowledge reads.
**Why:** the first blind test found every input written in full at every tick (512 hex digits a tick for the
key states), items written twice, nothing recorded after the last tick (how a demo ended), and the committed
hash changing with every name or item added, so it couldn't tell "the knowledge changed" from "the run
changed" and checked determinism only between machines with the same knowledge (D44). Measured on the Wolf3D
port's first-level: traces half the size, and the same run hash with and without a menu tick (671 or 1189
ticks); the end state hash is `clockwork run`'s for the same input.
**Supersedes:** D45.

### D54. Traces, version 3: D50's, and every frame's picture. Superseded by D60.
A trace writes an input when its value differs from when it was last read, as it does state, and an input may
be read only at some sync points. An item that is an input at every tick isn't written again as state. The
run ends at exactly the session's length, and the end line has the frames and sound after the last tick and
Clockwork's state hash then. The session commits a second hash, of the run (every frame and sound event, how
and when it ended, the state hash), which doesn't depend on what the knowledge reads. Each frame also names its
picture: the indexed pixels and palette (RGB in direct-colour modes) Clockwork renders when the picture
changed, kept in the cache at every recording.
**Why:** D50's reasons stand. Frames compare as indexed pixels and palette (D25), and a comparison needs no
emulator (D26), so the original's pictures must be kept; the person chose to keep them always rather than on
request. Measured on the Wolf3D port's pushwall: 1860 pictures, about 16 MB, recording 48 s instead of 44.
**Supersedes:** D50.

### D51. Questions start from the session's snapshots; whole-session answers are kept. Accepted 2026-10-21; kept by Clockwork's sources: D76.
`rrc record` keeps a Clockwork snapshot at the first tick after every 10 emulated seconds, in the cache, keyed
by Clockwork's program (its hash, not its version: builds share one), the game's files, the program, the
session's options and input, and the sync points (which number the ticks and are in the snapshot, with their
counts and gates). `rrc watch` and `rrc inputs` start from the last one before their first tick. `rrc calls` and
`rrc inputs` keep their output under the same run, the command and the knowledge and analysis they read.
**Why:** the first blind test spent most of its time waiting: every question was a run from power-on (inputs
56 s, watch 20-40 s, calls 20 s), most of them about play, which starts 44 s into the session. Measured on the
Wolf3D port's pushwall: a watch at tick 2700 in 3 s instead of 40, `rrc calls` asked again in 0.1 s, and the
run hash unchanged by the snapshots. A snapshot (about 2 MB) loads the input still to come with it, so the
session's input isn't given again. `rrc record` tracks calls from power-on and a snapshot keeps the call
tracker's stack (beside the machine's state, not in its hash), so a watch from it sees whole call chains;
recording takes about 10% longer for it.

### D55. An LE program's objects: found by their fixups, named by number, placed per session. Accepted 2026-10-21.
A flat program under a DOS extender is analysed and traced by its LE objects. Clockwork finds where the
extender's loader put them: the code object's bytes match the file's except at fixup sites (and a few the
program writes), and its fixups to itself hold its own base, which tells it from a copy of the file left in a
buffer; the other objects' bases are what its fixups to them hold. `clockwork unpack` writes the objects at
their linear addresses, at the entry point, as an ELF file, which Ghidra loads at those addresses. IDs name an
object by its number (`GAME.EXE#2+0041C`, D53). A session finds where its run places the objects once (a run
until they are placed) and keeps that by the run, as snapshots are kept (D51).
**Why:** extenders choose where objects go, and a session's options and memory change it, so neither the
file nor the analysis's run can give a session's addresses; Clockwork is deterministic, so one run of the
session can. Searching memory for the object without its fixups would find copies of the file; requiring the
fixups to hold the base doesn't. Measured: Open Watcom's FLAT32 under CauseWay (testbins) and Daggerfall's
FALL.EXE (three objects, placed in 1.7 s).

### D56. A launcher's game: `module` names it, and sync points count in it only. Superseded by D57.
A game whose program refuses to start alone (it checks it was started by its launcher) is started through the
launcher, and `[original] module` in `rrc.toml` names the program the launcher starts: it is the one analysed,
and its file name is the module of IDs. A sync point belongs to a program: Clockwork counts its hits only
while that program runs, or a program it started (an extender's loader), found by walking the PSPs' parents.
rrc gives each sync point the module. Only an LE program under a launcher is supported: its objects are found
by run (D55) and the module's must be the one running then; a 16-bit program's IDs count from the image of the
program started, so rrc refuses a 16-bit module under a launcher until its image is placed the same way.
**Why:** Daggerfall's DAGGER.EXE runs REPORT.EXE, then FALL.EXE, both CauseWay programs whose code object
the extender puts at the same address, so a sync point in FALL.EXE was hit in REPORT.EXE too, and REPORT.EXE's
objects were taken for FALL.EXE's. Measured on Daggerfall: one tick at FALL.EXE's entry, after the launcher.
Parents count too because an unbound DOS/4GW game runs DOS4GW.EXE, which runs the game's code.

### D57. The game's program, placed by the session's run: under a launcher, 16-bit or LE. Accepted 2026-10-22.
D56's `module` and sync points, and the game's program is found where the session's run puts it, whatever
starts it. Clockwork finds a program by name: the one running, or one that started it (walking the PSPs'
parents), with its load image and, for an LE program, its objects (D55). rrc runs the session once until the
game's program runs (and its objects are placed) and keeps where it is, by the run (`index/places/<run>`).
When a launcher starts a 16-bit game, every session start tells Clockwork to count `+OFF` from that image, so
IDs, sync points, watches and their answers are the game's, as when it starts alone; the launcher's code is
memory outside the image (`MEM+...`). An LE game's objects are found the same way, through its parent when
an extender it started runs its code.
**Why:** D56 refused a 16-bit game under a launcher, whose IDs counted from the launcher's image. Looking a
program up among the running one's parents also finds an unbound DOS/4GW game, whose stub runs DOS4GW.EXE,
which loads the game's objects: measured with Open Watcom's DOS/4GW, objects found while DOS4GW.EXE runs.
**Supersedes:** D56.

### D58. One protocol for emulators: a core and platform extensions, checked by a handshake. Accepted 2026-10-22, with the person.
The protocol tools drive an emulator with is a documented contract (`docs/protocol.md`) with a revision: a
core any emulated machine can answer (loading, running to times and sync points, memory, watches, call
tracking, coverage, checks, frames, sound, input, snapshots, the state hash, and the determinism that makes
them repeatable) and an extension per platform (Clockwork's `dos-pc`: `SEG:OFF` addresses, LE objects,
`rebase`, processes, devices, interrupt-numbered services). An emulator answers `hello` with its revision and
platform; `rrc` refuses another revision or a platform it doesn't know. Every change to the protocol is a new
revision, in the emulator, the tools and the document together.
**Why:** the person may port games from other machines (Apple II, Commodore 64) the same way, with an emulator
per machine, possibly written by others. A written contract, with what is common apart from what is DOS's,
keeps that door open at little cost now, where finding the DOS assumptions later would cost much; and the
handshake already catches today's mistake of an `rrc` and a `clockwork` from different builds. Not done yet,
because it is best shaped by a second platform in hand: a platform layer inside `rrc`, a core shared between
emulators, a `platform` setting in `rrc.toml` (a later default of `dos-pc` needs no change to existing ports).

### D59. Scenario sessions: a base, edits at ticks, then input. Accepted 2026-10-23, with the person; how edits run: D64.
A session may set up the scenario it needs instead of playing to it: it starts from power-on or a snapshot,
writes values into the original's memory at chosen ticks (named data through the knowledge's types where
there is any, IDs otherwise), then runs its input. The edits are part of the session, in what makes its run
the same (its snapshots, traces and kept answers follow them), and its trace lists them, so a divergence can
be traced to an edit. Only the original is edited: the port gets the same scenario by a transplant at that
tick (DESIGN.md §8). Where a game has its own cheats or debug modes, they come first: they reach the
scenario through the game's own code.
**Why:** reaching a scenario by play (input scripts found by trial) is the slowest part of the work, and the
goal is to implement a game as efficiently as possible; the person proposed it. The risk is a state play
can't reach (a count changed without the list it counts, a value the game derives from others left stale): a
port matching from there proves little, and a divergence there may be a false alarm. Edits therefore go
through the game's structures as the knowledge describes them, and are suspected first when a divergence
follows one.
**The emulator:** nothing new. The protocol's `write` already writes memory as the program sees it (through
the page tables), Clockwork drops its decoded code at every run, and the edits are part of the state and its
hash. rrc runs to the tick, writes, and goes on. Part of M3.

### D60. Input that changes within a tick is flagged; traces, version 4. Accepted 2026-10-23, with the person; the values: D68.
A sync point is an instruction before the pass reads its inputs, and input arrives at any cycle: input that
changes between the two is read by the pass but recorded with the next tick. Recording checks for it: from
each tick's sync point on, Clockwork compares every read the program makes of an input (its memory; a device
at each call of its driver or read of its port) with what the input held at the sync point, following the
program's own writes and leaving out what interrupt handlers read and write. A tick that read another value
lists that input with the instruction that read it (`late`); `rrc record` sums them up by that instruction,
and `docs/porting.md` says where to put the sync point so it can't happen: on the instruction that reads the
input. The wait that ends a tick (D47) is left out: late reads all made in the call the next hit comes in, the
last at most 40 of the program's instructions before it, are the wait polling its timer count, which the next
tick records. Otherwise traces are version
3's: an input is written when its value differs from when it was last read, and may be read only at some sync
points; an item that is an input at every tick isn't written again as state; the run ends at exactly the
session's length, and the end line has the frames and sound after the last tick and Clockwork's state hash
then; the session commits the run's hash beside the trace's; each frame names its picture, kept in the cache.
**Why:** the second blind test (the Wolf3D blind port's notes/toolkit-report.md) found a joystick calibration
loop that leaves on a button press without a tick that has the press: the press landed between a hit of the
loop's gated sync point and its read of port 201h a few instructions later, so that pass read it and left the
loop, while the loop's ticks all had the button up. A port fed the trace would have stayed in the loop, and
nothing in the trace showed why. Recording each input where it is read instead would tie a tick's values to
the pass's reads, which differ from pass to pass; the person chose to detect it and document the placement.
Measured on the blind port's six sessions: the calibration tick is flagged, at the port read. Without the wait
rule, 1005 of play-joystick's 1430 ticks were flagged, nearly all by the waits' reads of the timer count
(their last read comes 3 to 22 instructions before the hit, the closest read of a pass 59); with it, 5 of the
attract session's 5052. What stays flagged is real: the main menu loop reads the timer count at the end of its
pass, before calling the function its sync point is at the entry of (160 to 496 ticks a session); a joystick
routine time-stamps its reads with the timer count (226 ticks); keys arrive between the sync point and a later
read of the play loop (12 ticks of play-keyboard). The runs are the same as before. D54's reasons stand for
the rest.
**Supersedes:** D54.

### D61. A port is a program `rrc` runs, and they exchange files. Accepted 2026-10-26.
A port repository names its port's program in `rrc.toml` (`[port] command`, a command line run in the
repository, with an optional `[port] build` run first). `rrc compare` writes the session's inputs, tick by tick,
from the original's trace (`rrc-inputs`) and runs the port with `run --inputs FILE --out FILE`, and with
`--transplant FILE`, `--resume FILE`, `--save-at TICK --save FILE` or `--to TICK` when asked; the port writes its
state at every tick (`rrc-port-trace`: the top-level fields of its serialized state that changed). The state at
tick t is what the original holds at its t-th sync point hit; a step with tick t's inputs makes tick t+1's. A
Rust port gets all of this from `runtime/` (the crate `rrc-runtime`): it implements `Simulation` (`start`,
`step`, and `transplant` when its state isn't the mapped fields alone) on one serializable struct and calls
`rrc_runtime::main`.
**Why:** D38: files a port in any language can read and write, and `rrc` needs to know nothing of how a port is
built. A program per run keeps the port's process apart from `rrc`'s, as Clockwork's is (a crash is the port's).
The toy port (`examples/toy`) is about 200 lines with its test, none of them for the contract.

### D62. The mapping: the port's fields as paths, the original's items as typed values. Accepted 2026-10-26; pointers, heap data and packed bits: D72.
`mapping.toml` maps each field of the port's state (a dotted path in its serialization: `player.x`) to the
original's item (a data name or an ID) and reads that item as the knowledge's type for it, or the one given:
primitives as numbers, arrays as arrays, the structures of `types.toml` as objects of their fields, enumerations
as their numbers. Both sides compare as these values, leaf by leaf; what the port's state has beyond them is
its own. `[[ignore]]` names the original's memory that may differ. `rrc record` traces every mapped item; a
transplant writes the mapped values at a tick (`rrc-state`), and the runtime sets them over the port's start
state's serialization. `rrc check` checks the mapping, and `rrc compare` lists the named data neither mapped
nor ignored.
**Why:** DESIGN.md §7: one mapping for comparison and transplant, so a gap shows as a divergence. The
knowledge's types already say how memory reads; with the port's serialization as the meeting point, a port in
any language maps by naming its fields. **Not yet:** encodings the types don't have (a pointer as an index,
packed bits, a flag as a boolean): until the mapping has them, a port serializes such a field the original's
way (in Rust, a serde attribute on the field).

### D72. Pointers, data on the heap and packed bits in the mapping; traces, version 6. Accepted 2026-10-27; segments: D80.
`types.toml` declares pointer types (`[[pointer]]`: `near`, an offset of 2 or 4 bytes from a base ID; `far`,
an offset then a segment, real mode) and packed bits (`[[bits]]`: fields of bits, each a number or a `bool`);
`bool` and `far` are built in. A pointer reads as null, as the index of the element it points to (`into` an array
of named data), or as the ID it points to; a far pointer's ID is named as Clockwork names that address in the
session. A mapped field may be `via` a pointer: the item is where the pointer points at each tick, `offset`
bytes past it (data on the heap). The trace reads it there at every tick, beside the pointer, as `*POINTER`;
the who-wrote of a divergence in it watches where the pointer pointed at that tick. An edit writes packed bits
only whole, with every bit declared, so it never clears bits nobody named. **Not yet:** a pointer found through
another (a list's links on the heap), and `into` an array on the heap: when a game needs them.
**Why:** real games keep state behind pointers (a target, a current level), on the heap and in flag bytes,
which D62 left for later; a port holds an index, an array and named booleans for them, and the mapping meets
them field by field. Measured on the toy game, now with a pointer to the nearest enemy, a 32-entry trail on
the far heap and status bits: its port matches all 1400 ticks, and a bug in the trail diverges at tick 2 and
names the instruction that wrote there.
**Supersedes:** D62's "Not yet" for these encodings (the rest of D62 stands).

### D75. `rrc search`: the original's memory found by how it behaves over ticks. Accepted 2026-10-27.
`rrc search SESSION CONDITION...` reads every place of the data segment (an LE program's data objects, or a
range given) as a number type at the ticks its conditions name, and lists those that meet them all, with their
values: `changed:A-B`, `same:A-B` (at every tick between), `more:A-B`, `less:A-B`, `is:T=V`. A tick's values are
memory at its sync point, read from the session's snapshots on. Traces of version 5 are read as well (version
6 only added items read through pointers).
**Why:** found on Arena (M4): the state worth mapping is the state the player changes, and nothing named
pointed at it. Memory scanners find a game's values the same way (what changed while the player walked, stayed
put while they stood); with ticks and the session's inputs, the conditions come from the trace. On Arena's
`cell` session, the player's position (changed while walking, the same while turning or standing) is 43 places
of 64 KB, 24 with more spans, and a watch on them shows the copy `world_loop` makes; the facing angle (less while
turning left, more while turning right) is one place among 6.

### D76. Runs are kept by Clockwork's sources; a search keeps each tick's memory. Accepted 2026-10-27.
A session's run (its snapshots, its places, kept answers) is kept by the hash of Clockwork's sources, not of
its program: `clockwork --source` prints it, made when Clockwork is built from its crates' sources and
manifests, the workspace's manifest and lock file and the compiler's version, line endings left out (a
Clockwork that doesn't say is kept by its program's hash, as before). `rrc search` reads every tick from the
first to the last its conditions name, not only those, and keeps them in the cache (the first tick's bytes,
then each tick's changes, deflated; up to 256 MB read): a later search within them runs no emulator.
**Why:** any rebuild changes the program's bytes, a rebuild of the same sources too, so every rebuild set
aside all snapshots and questions replayed sessions from power-on: on Arena's `cell`, a search ran the
330-second walkthrough again, 1m40s each. The sources decide how Clockwork behaves; a change to them still sets
the old snapshots aside. Narrowing state down takes several searches over the same ticks, and the memory of
a span is small (64 KB a tick, little of it changing).

### D63. The comparison reports the first divergence and who wrote it; paired checkpoints. Accepted 2026-10-26.
`rrc compare SESSION` reads the original's trace from the cache, runs the port and compares every mapped field
at every tick. At the first tick one differs it shows the differing leaves (up to 8) with both values, notes
late inputs in the pass before (D60), and watches the first leaf's bytes in that pass (from the session's
snapshots) for the instruction that wrote the original's value; at the port's first tick it says the start or
the transplant is what differs instead. It exits 1 on a divergence. `--transplant N` starts the port from the
original's mapped values at tick N. `--pair N` keeps a paired checkpoint when the port matched through tick N:
its own state there (`rrc-port-state`) and Clockwork's snapshot at the end of tick N, in the cache, listed in
the session's file (`[[pair]]`); `--resume N` starts the port from it.
**Why:** DESIGN.md §7, §8: the first divergence localizes a bug, and who wrote it says which original code to
read. Comparing needs no emulator (D26): only the watch and a pair's snapshot run Clockwork. Measured on the toy
port: its 20-second session matches at all 1400 ticks; with a bounce left uncounted it diverges at tick 194, and
the report names `update`'s instruction that wrote the count. **Not yet:** the port's side of the divergence
(D73), frames and sound (M6).

### D73. The port's side of a divergence is its code for the original's writer, found by ID. Accepted 2026-10-27.
After who wrote the original's value, `rrc compare` names the port's code for the function that wrote it: the
lines of the port's files that name that function's ID (`port      update (GAME.EXE+00114) is
src/main.rs:113`), else a caller's in the chain, else that no code names them. It reads files of any language,
not the repository's knowledge, sessions, notes, settings or build output.
**Why:** a call trace of the port would need tracing put into every port, in each language, and the agent
would still have to find the code from it. The code rules already have each function name the original it
reimplements (DESIGN.md §6.6), so one ID finds both sides, and the comparison stays a matter of files (D61).
Where a game shows a divergence this can't place (a field written from many places), the runtime can add a
trace then. Measured on the toy port: a bounce left uncounted names `update` and the line of `step`.
**Supersedes:** the port's own call trace at a divergence (DESIGN.md §7, D63's "Not yet", D71's M4).

### D64. Scenario edits: written by the emulator at counted ticks, and given to the port with the inputs. Accepted 2026-10-26.
A session's edits (`[[edit]]` in its file, or `rrc session edit`) name named data or an ID, a path into it as
its type reads (`enemies[2].alive`) and a value (a number, an array, some of a structure's fields, an
enumeration's value name), written as the type's bytes. Clockwork writes them when the session's tick is
counted (the hits of every sync point), before that instruction runs, so the tick's state has them: they are
part of the machine's state (its hash while some are to come, its snapshots), and every run of the session
gets them, whichever `rrc` command makes it (protocol revision 5, `edits`; a watch names their writes "the
session's edit"). They are part of what makes the run the same (D51's key), and the trace's header lists them.
The port gets each as the mapped leaves it wrote, set to the original's values at its tick (`set` in the inputs
file): a transplant of those fields alone, so a scenario compares from the first tick, from a transplant or from
a paired checkpoint alike. Re-recording a session whose run changed drops its paired checkpoints. A divergence
after an edit names the last edit before it, to be suspected first. **Not yet:** a session based on a snapshot
(D59's "or a snapshot"): a session reaches the scenario by its input and edits from power-on.
**Why:** D59 planned rrc to run to each edit's tick and write it, with nothing new in the emulator. But every
command runs sessions its own way (a recording tick by tick, a watch from a snapshot, a scan by milliseconds),
and a snapshot taken before an edit must still make it: in the emulator, one place covers them all, and its
snapshots carry the edits still to come. The port can't be edited by rrc, so it gets the edits as values, by
the same mapping that compares it. Measured on the toy port: edits at tick 100 (a score, an enemy gone) and at
tick 120 (the lives); the session matches from tick 1.
**Supersedes:** D59's paragraph "The emulator" (the rest of D59 stands).

### D66. The Arena port uses no knowledge from Recreate or the person's earlier Arena project. Accepted 2026-10-27, with the person; other outside sources: D77.
The Arena port's knowledge comes from its own analysis and runs only: nothing from Recreate (its Arena
knowledge, with what it imported from OpenTESArena) or from the person's earlier Arena project
(`MrMethor/tesa-decomp`), which D65 had it start from. What was imported (141 function names, 390 data names)
was taken out again and the analysis redone without it; the port's `rrc.toml` refuses imports. Names the port
decided itself stay, on its own evidence.
**Why:** the person found they bring confusion into the project: generated names (`rtl_file_6010`,
`mouse_reset_3c9f`), provenance through two other projects, names the port can't vouch for. Recording the
session again without them gave the same run.
**Supersedes:** D65's sentences on prior art (the rest of D65 stands).

### D67. `rrc` has no import of Recreate's knowledge. Accepted 2026-10-27, with the person.
`rrc import recreate` goes, with its provenance `recreate:`. `[port] imports = false` still makes `rrc sigs name`
refuse (a linker map's names). The person deletes Recreate's and the earlier Arena project's folders.
**Why:** D66: their knowledge confuses the project, and no port uses it any more; the command would only bring
it back.

### D77. How to work: the analysis, outside sources and the game's files first, runs to confirm. Accepted 2026-10-27, with the person.
The playbook opens with rules for using the toolkit (`docs/porting.md`, "How to work"). Every question about a
game draws on four sources, in this order:
1. the analysis (decompiled code, disassembly, calls, who reads and writes what);
2. outside sources (format documentation, reimplementations, wikis, walkthroughs, bug lists);
3. the game's files;
4. runs of the original, which confirm.

- **Outside sources are expected, not relied upon.** Look before working out what others have written
  down, and treat each as evidence named with its source, which the original confirms or overrules.
  Projects the person excludes stay out (for Arena: Recreate and tesa-decomp).
- **The decompiled code shows how the program works, not how it runs.** It gives the logic and the data it
  uses. It doesn't give which paths run, with which values, in which order, nor what interrupt handlers,
  code made at run time and timing do: runs settle those.
- **Rules:** plan before running; don't play the game to find things; two kinds of evidence for each finding
  (code or a source, and a run); name what is learned at once; use the tool that fits the question; the
  original is the specification.

**Why:** on Arena (M4), finding the map, a door and a fight by memory searches, watches and walking with
screenshots took hours and missed what was close by. The decompiled code of the functions already found leads
to the level, the doors and the creatures. A walkthrough answered at once what the screenshots couldn't (the
cell's key). The person: "It's a toolkit, all the tools are meant to be used as they are fit."
**Supersedes:** D66's "from its own analysis and runs only" (Recreate and tesa-decomp stay out: D66, D69);
D2's framing of decompiled code as a hint only (it stays a reading, never a translation).

### D84. `rrc compare --feed`: the original's values of a subsystem not ported yet. Accepted 2026-10-27.
`rrc compare SESSION --feed FIELD,...` gives the port the original's values of those mapped fields (or the groups
they are in) at every tick before it steps, as a scenario's edits are given (D64): they match by construction,
and the subsystem being ported reads what the original's other subsystems made.
**Why:** DESIGN.md §2 ports subsystem by subsystem from a transplant, but a subsystem not ported yet diverges at
the next tick and the comparison stops there. Arena's walking matched at tick 401 while the creatures' random
draws (not ported) changed the generator's seed, so nothing after tick 401 could be checked. A fed field still
in the port's state can't hide a bug of the ported code: it is overwritten, not compared.

### D83. `rrc pass`: what one pass runs, in order. Accepted 2026-10-27.
`rrc pass SESSION --tick N` prints the calls made between tick N-1's sync point and tick N's as a tree of named
functions in the order they ran: `--depth` levels (2), a call repeated in a row once with a count, how many
calls lie under the last level shown, `--in FUNCTION` for what one function runs, hardware interrupts' handlers
left out unless asked for. Clockwork's call tracker keeps an ordered log for it (`calls {"log": true}`,
`call_log`; protocol revision 7).
**Why:** a port's step is one pass, and its order decides values. In Arena, the acceleration of walking is
raised by the input code and lowered by the command-key code; the traced values fit only one order, which no
tool showed: call counts have no order, and the decompiled loop dispatches through far pointers. One
`rrc pass cell --tick 404` showed it: `update_actors` first, the 3D view, `move_player`, then the command keys.

### D82. A port reads the original's static data from the person's copy. Accepted 2026-10-27.
When `rrc` runs a port it gives it `RRC_PROGRAM`, the program as the analysis unpacked it, and `RRC_GAME`, the
game's folder. `rrc_runtime::original::program()` reads the program's load image at the offsets IDs use
(`program().i16s(0x46A3C, 640)` for `ACD.EXE+46A3C`), so tables the code reads are loaded, not copied into the
port's source. A port played without `rrc` (M6) will need the unpacking itself.
**Why:** Arena's walking multiplies by a 640-word sine table at `ACD.EXE+46A3C` that no rounding of `sin`
gives; ACD.EXE is packed with PKLITE, so the table is only in the unpacked image. Copying it into the port would
put the game's data in a repository (D78 keeps extracted data out of them), and every port has such tables.

### D81. Contracts from a run give Ghidra register parameters and results. Accepted 2026-10-27.
`rrc analyze --session NAME` runs the session once with Clockwork's contract inference (kept by the run) and
gives Ghidra each called function's contract before decompiling: the registers it read before writing them
become register parameters, beside the stack ones Ghidra found, and the result its callers used (`AX`, `DX:AX`,
`EAX`, the carry flag, another register) its return. The functions with a contract and their callers are
decompiled again. A contract is what the run's calls did: a register only an untaken path reads isn't in it.
Contract inference counts a flag a call set as its result even when it kept its value (`CLC` on a clear carry).
Clockwork's serve has `contracts`/`contracts_end` (protocol revision 7). Real mode only.
**Why:** Arena's hand-written assembly passes the actor in SI and answers in the carry flag. Ghidra's code read
`unaff_SI`, and `bVar8 = false; FUN_1000_1cee(); if (bVar8)` for a carry it lost, and it dropped the random draw
`update_actor` makes (`(rand >> 13) + angle`, a random turn). With 716 contracts from `cell` (55 of them a
carry), `update_actor(word si, word es)` reads `bVar8 = FUN_1000_1cee()` and uses the draw.

### D80. A segment alone is a pointer kind. Accepted 2026-10-27.
`types.toml`'s pointers have a third kind, `segment`, and `segment` is built in: 2 bytes, pointing at
segment × 16. A mapped item `via` one is the block it starts, `offset` bytes in. Edits write it as
`SEGMENT:0000`.
**Why:** DOS programs keep blocks they allocated, and tables in segments of their own, by the segment alone.
Arena reaches its creature data (8 records of 1054 bytes) through the segment at `ACD.EXE+3CA42`, and its map
layers through the segment at `+49206`; neither is a far pointer.
**Supersedes:** nothing; D72 has near and far pointers.

### D79. Runs feed the analysis: `rrc analyze --session`, and file tracing (`rrc files`) built ahead of M5. Accepted 2026-10-27.
- **`rrc analyze --session NAME`** runs the session with call counting and adds every function the run
  called that Ghidra lacks to Ghidra's analysis (as the knowledge's named functions are added), so they get
  calls, references and decompiled code. Code past the program's file (drivers it loaded) can't be
  added; it is read with `rrc show ID --asm --session NAME`.
- **`rrc files SESSION`** is D78's file tracing, built now:
  - Clockwork's `files` probe records each read and write through DOS: the file, the offset, the length,
    the buffer, the return address, and the call chain while `calls` is on (protocol revision 6).
  - `rrc files` lists them by file, or one file's accesses with `--file`, naming the buffers and the code.
  - It is kept by the run, so asking again runs nothing.

**Why:** both came from Arena (M4), under D77's order.
- Most of what a pass of Arena's world loop runs is reached through far pointers: 24 of the 27 functions
  that run once a pass had no decompiled code until a session added 150 functions.
- The level's file led to its loader in one question: `START.MIF`'s chunks, read by `+0E930` from
  `+0E880`, the 61-byte header into `+47D42` and the compressed map layers through `MEM+E0000`.

### D78. A game's assets are a source of the toolkit; M5 extracts them. Accepted 2026-10-27, with the person; file tracing built: D79.
A game's files are knowledge (D77) and what a port loads: archives, compressed data, level maps, pictures with
their palettes, fonts, texts, sounds and music. M5 gives the toolkit:
- **File tracing** (`rrc files SESSION`): which files the program opens and reads, at which tick, and where in
  memory each read's bytes went (Clockwork's DOS file services report them). A structure in memory then leads
  to its file and offset, and the file to the function that reads it.
- **Formats as knowledge** (`knowledge/formats.toml`): each kind of file, its layout in the knowledge's types,
  its compression, the function that reads it, and the provenance (the analysis, or a source).
- **Extraction** (`rrc asset FILE`): a file, or a member of an archive, decoded by its format and shown as
  values. Pictures are written as PNG with their palettes, sounds as WAV, music as MIDI. Common compressions
  (RLE and LZ variants) are decoded by the toolkit; a game's own is described by its format.
- **The original as the check.** What the original's reader put in memory (from file tracing) is the expected
  output of an extraction and of a port's loader, so a decoder is compared with the original as the logic is.

Extracted assets are the person's: they go to the cache or the port's `.rrc/`, never into a repository (D33).
**Why:** the person: assets are part of porting. Arena's level map, its doors and where its creatures are
come from its files, and reading those with the code that loads them is faster and surer than finding them in
memory. A port loads the person's copy of the game, so it needs the formats anyway.

### D69. Recreate is disregarded. Accepted 2026-10-27, with the person.
Nothing more is taken from Recreate and nothing is read from it: not its code, its design (`PROJECT.md`) or its
knowledge. What came from it (DESIGN.md §11) is Rrecreate's own now, and the decisions carried from it stand as
Rrecreate's decisions. Clockwork no longer reads the session files Recreate's `rec-host` wrote.
**Why:** the person knows of nothing in Recreate that is still relevant, and it confuses the project (D66).
Its folder is deleted, and no work here may need it.

### D68. A late read's value is kept, and the port reads it where the original did; traces, version 5. Accepted 2026-10-27, with the person.
Recording keeps, for each instruction of a pass that read an input after it had changed since the sync point,
what it read: the trace's `late` entries are `[input, the instruction's ID, the input as that instruction read
it]` (the tick's value with the bytes it read as it first read each), each reading instruction once a tick. The
wait's reads stay out, now instruction by instruction (all in the call of the next hit, the last at most 40 of
the program's instructions before it). `rrc compare` gives them to the port with the tick's inputs, and the
runtime's `Inputs::at_site(ID)` is the inputs as that instruction read them: the port reads a clock or a key
there through it, at the place that does what that instruction did. A live platform reads its clock or device
again at that call. Otherwise traces are version 4's.
**Why:** the goal is a port as close to the original as can be checked (D39); where that costs too much, the
closest thing that is still well built for modernizing (the person, 2026-10-27). Arena's world loop is
CPU-bound and reads its clock again after drawing the frame, every pass (an action every 90 timer ticks, at
+04BB9): no sync point can fix it, and how far the clock moved depends on how long the original's drawing
took, which a port can't compute. A value per reading instruction is what the original's code saw there; a
port that reads its clock in the same place through `at_site` stays a clean simulation (a clock read is an
input), and a modern mode can read a real clock there. Moving the sync point to the read (D60) stays the fix
where one read comes right after it.
**Supersedes:** D60's "detect it and document the placement" for the values: detection, the wait rule and the
placement advice stand.

### D45. Traces are JSON lines: inputs, changed state, frames and sound per tick. Superseded by D50.
A header names the items (boundary values and named data, with their IDs and sizes); each tick's line has the
sync point, the cycle, every input, the data that changed since the last tick (all of it at the first), the
frames shown and the sound events. Values are little-endian bytes in hex.
**Why:** D38 needs a format any language can read and write; only writing what changed keeps a session of
thousands of ticks small without compression.
