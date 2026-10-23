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

### D2. Decompiled code is a hint, not a source. Accepted 2026-10-19, with the person.
Ghidra's output is read for how the original does something; nothing is translated from it mechanically.
**Why:** it is partly wrong (missed functions, lost segments, dropped flags, guessed types) and incomplete (no
hardware layer, no runtime), so a function-by-function translation can't produce a working program.

### D3. The honest pilot. Accepted 2026-10-19, with the person.
The pilot is Wolf3D, with id's released source kept out of the agent's reach and used only to grade the result
afterwards; the real test follows on a small game with no source release. Arena and Daggerfall are not test
ports: the person ports them with the finished toolkit (decided 2026-10-17).
**Why:** a source release lets the agent take shortcuts, even unintentionally, and then the pilot proves nothing.

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

### D18. The quirk register. Accepted 2026-10-19, with the person.
Original bugs are kept in faithful mode as registered quirks with a toggle to fix each; comparisons keep them all.

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

### D54. Traces, version 3: D50's, and every frame's picture. Accepted 2026-10-21, with the person.
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

### D51. Questions start from the session's snapshots; whole-session answers are kept. Accepted 2026-10-21.
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

### D59. Scenario sessions: a base, edits at ticks, then input. Accepted 2026-10-23, with the person.
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

### D45. Traces are JSON lines: inputs, changed state, frames and sound per tick. Superseded by D50.
A header names the items (boundary values and named data, with their IDs and sizes); each tick's line has the
sync point, the cycle, every input, the data that changed since the last tick (all of it at the first), the
frames shown and the sound events. Values are little-endian bytes in hex.
**Why:** D38 needs a format any language can read and write; only writing what changed keeps a session of
thousands of ticks small without compression.
