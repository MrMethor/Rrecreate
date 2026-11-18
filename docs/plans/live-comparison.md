# Live comparison (M6.5): the plan

> Accepted 2026-11-16 with the person (D184): M6.5, between M6 and M7 ([design](../design.md) §12). The person
> plays the original; the port runs alongside, and every difference is said within a second or two of the tick it
> happened at. Each step below becomes a decision as it is built, and what it found is written under it. Section 10
> lists what is still the person's to decide.

## 1. What it is for

"The sessions are identical" means: the person plays the original in Clockwork's window, and the port, given
what the original read at each tick, does the same thing at every tick: every mapped field, the screen at each
tick, the pictures between ticks and the sound. Today `rrc play` (D124) checks this after the fact:

- each minute of play becomes a part, recorded again by a second run of the original at about the speed of play
  (D124 measured 147 s for 157 s of the dungeon), then compared in lockstep: a difference is said two to four
  minutes after it happened, when the person has long moved on and can't do it again on the spot;
- a recording that only just keeps up falls behind for good on any slower stretch;
- the parts are sessions of their own, each based on the one before, so one play is dozens of sessions.

Live comparison closes those gaps, and adds a check nothing makes yet: the port's own input handling (`Live`,
D119), which turns the host's keyboard, mouse and clock into what a pass reads, is compared with what the
original's handlers made from the same keys at the same moments. Today that code is tried only by playing the
port and looking.

What it isn't: a way for the agent to explore by playing (D77 stands), a modern-mode check (modern mode differs
on purpose), or a replacement for the comparison's tools: a difference found live is investigated afterwards with
`rrc compare`, `rrc watch` and the rest, on the play's session.

## 2. The shape

```
 the person
     │ keys, mouse
 ┌───▼──────────────┐  the play file: inputs at their cycles,      ┌──────────────────────────┐
 │ the window        │  a mark every 100 ms ("all is in to here"),  │ the follower              │
 │ clockwork play    ├─ a state hash every 30 s ───────────────────►│ clockwork serve, run by   │
 │ (as today: never  │                                              │ rrc: the same run, never  │
 │ slowed, never     │                                              │ past the last mark, at a  │
 │ waited on)        │                                              │ tick reads what a         │
 └──────────────────┘                                              │ recording reads           │
                                                                    └────────────┬─────────────┘
                                                                                 │ ticks, as they are made
                       ┌──────────────────────────┐  inputs line by line  ┌─────▼──────────────┐
                       │ the port                  │◄──────────────────────┤ the comparer (rrc) │
                       │ its comparison run,       ├──────────────────────►│ one tick at a time,│
                       │ reading and writing as    │  its tick's line,     │ said in the        │
                       │ the lines come            │  frame, sound         │ terminal, kept in  │
                       └──────────────────────────┘                       │ the session        │
                       ┌──────────────────────────┐  devices at their     │                    │
                       │ the port's Live           │◄─ virtual times ──────┤                    │
                       │ (its input handling)      ├─ boundary values ────►│                    │
                       └──────────────────────────┘                       └────────────────────┘
```

Four properties carry the design, each argued in section 3:

1. **The window is the person's alone.** It runs as `clockwork play` does now; nothing the checking does can slow
   it, pause it or change its run.
2. **The follower's run is the window's run**, because Clockwork is deterministic (D9): the same snapshot, the
   same inputs at the same cycles. The window writes its machine's state hash every so often, and the follower
   checks its own against it, so this is proved as the play goes, not assumed.
3. **One pipeline.** The live check and the offline comparison are the same code: recording, the port's run and
   the comparison become streams, and the offline tools run them from files at full speed. Nothing compares
   differently live, and the live mode is tested in CI by replaying recorded plays.
4. **Everything is bounded:** the lag, the memory, the disk. When the checking can't keep up, it says so and falls
   behind; it never makes the window wait, and never grows without bound.

## 3. The decisions it rests on

### 3.1 The person plays the original, not the port

The original is the specification. Played, it reacts to the person, and the port is held to it. Played the other
way round (the person plays the port, the original follows), the person reacts to the port's screen: at the port's
first difference the person's next input means something else to the original (they turned because the port
showed a wall the original doesn't have), and every tick after compares two different games. D124's reason holds:
the port is compared with the play, not played. The port's playable side is still checked, by its `Live`
(section 3.7), which is what playing the port exercises that a comparison doesn't.

### 3.2 The checking runs in a second process: the follower

The window could be instrumented itself (stop at each sync point, read the mapped state, stream it), with no second
run. It isn't, because then the checking's cost is the person's: every stop, every read, every picture hashed
comes out of the window's frame time, and a window that stutters changes how the person plays (Arena's combat is
real time) and so the session. The follower costs a second emulator, which the machine has room for (8 logical
cores; the window uses one and its sound thread). It must run faster than real time to keep up; step 0 measures
whether it can, and the window instrumented is the fallback only if raw emulation can't (section 7, step 0).

The follower is `clockwork serve` driven by rrc as a recording is now, given the play's inputs as the window
writes them (the protocol's input at exact virtual times) and asked to run to the next tick, **never past the
last mark**: input after it may still be coming, and a run past it would miss it and quietly become another game.
That one rule is the follower's correctness; a test runs it against a play file written slowly, line by line.

### 3.3 The follower proves it is the window's run

Determinism makes the two runs the same; a bug in it (a device reading the host's clock, a window-only code path
that touches the machine) would make every comparison after it meaningless while looking fine. So the window
writes `# hash <cycle> <state hash>` every 30 s of play, and the follower, at that cycle, hashes its own and
compares. A mismatch stops the check at once, says so plainly ("the follower's run left the window's at 4:30 of
play: a Clockwork bug, D9"), and keeps the play file, which still replays (the window's run is the session). The
state hash exists already (a trace's end line); its cost on the window is measured in step 0 and the period set
so that no frame is late. At the end, the play's trace is the follower's, and re-recording the play
(`rrc record --run`, D111) must give the same run hash: the final proof, run in CI on replays.

### 3.4 One pipeline: streams everywhere

Three things become streams, and the offline tools are rebuilt on them rather than beside them:

- **Recording** (`trace::record`): ticks come out as they are made, to a sink (the trace file, the comparer or
  both), and the input may be a play file still being written. A streamed recording of a finished session must
  equal the batch one, byte for byte (its trace hash): the test of the change.
- **The port's run** (`rrc_runtime`, `docs/formats.md`'s contract): the inputs file may be a pipe; the port reads a
  tick's line when it needs it, and writes and flushes its own line, frame and sound for a tick before reading
  further than the next tick's line (it needs that one: `Tick::next`, D143). rrc writes the inputs and reads the
  port's lines on separate threads, so neither side's pipe filling up can deadlock the other. A port in another
  language keeps working: reading a file to its end is reading a pipe that ends.
- **The comparison** (`compare.rs`): one tick at a time, with its census kept as it goes (fields, screen, pictures
  between ticks, sound, notes); `rrc compare` and `--lockstep` become this comparer fed from files. The test: all
  58 of Arena's sessions, and every example's, give the same results as before the change.

Two implementations (a live one and an offline one) would drift apart, and the live one, which runs only when the
person plays, would be the one that is wrong. So there is one.

### 3.5 The verdict is a free run, corrected where it differs

Lockstep (D124) feeds the port the original's mapped state before every pass, so a difference in one pass doesn't
spoil the next, and every difference is counted. A free run (the port on its own state, as it plays) is what
"identical" means, and it alone tests what the mapping can't carry both ways exactly: pointers read as indexes
(D72, D178), fields read as set or not (D178), unset fields (D102), and everything the port keeps that isn't
mapped. Its weakness is the cascade: after the first difference, every tick differs.

The live check does both in one: **the port runs free, and only where a tick differs are the differing fields fed
back** to the original's values (D84's feed, for those fields from that tick on, until they match again by
themselves). A play with no difference is then a pure free run, the strongest verdict; a play with differences
counts each once where it arises, as lockstep would, without the cascade. A difference the feed can't correct
(state that isn't mapped: the presentation's own, carried memory) recurs; it is said once with how many ticks it
lasted, never once a tick. After a pass that took a path the port hasn't ported (its notes, D115), the whole
mapped state is fed, since nothing the port holds is trusted then.

This mode is new offline too (`rrc compare SESSION --corrected`), and its census must equal `--lockstep`'s on all
58 sessions where the mapping is exact: a strong test of both.

### 3.6 Bounded everything

- **Lag:** the comparer shows how far behind it is. If the follower is slower than play, the lag grows and is
  shown; the window is never waited on. When the play ends, the follower and the port finish what is left.
- **Queues:** at most a few ticks between the follower and the comparer, and between the comparer and the port
  (backpressure: a slow port slows the follower, never fills memory).
- **Memory:** the follower keeps no snapshots in memory and runs no watches; the comparer holds the current tick
  and the census; the port is the port. Step 0 measures the three processes and sets a ceiling (about 2 GB in
  all on this 8 GB machine, D171's lesson); a test runs a long replay and checks that memory stays flat.
- **Disk:** see section 4, "Storage".
- **Priority:** the follower, the port and the comparer run below normal priority (on Windows,
  `BELOW_NORMAL_PRIORITY_CLASS` when spawned), so the window keeps first call on the processor.

### 3.7 The port's `Live` is checked at virtual time

The port's input handling is a function from the devices and the clock to the boundary values a pass reads:
the key states and queue its keyboard handler kept, the timer count its timer handler kept, the mouse its driver
reported (D119, D133, D145). The check runs it on the play's own device events (the play file's keys and mouse at
their cycles) and the virtual time of each of the original's sync points, and compares what it makes with what the
original's pass read there. It needs the platform's clock to be a parameter (the host's when played, the
play's when checked); with that, it is deterministic and runs offline too (`rrc compare SESSION --live`).

What the pass itself wrote into its inputs (a key queue it emptied, D97) is given from the original's run, so the
check tests the handlers alone, not the pass.

A read in the middle of a pass (D93: Arena's FLC loop polls the key queue every frame) happens at a moment of the
original's processor time, which the port doesn't model: a key that arrived between two of the original's reads
may be seen one read earlier or later by the port. These are compared as the same events in the same order, not
read by read, and a difference only in which read saw an event is said as "timing inside a pass", a class of its
own, as a fleeting picture is (D181). Anything else (a key lost, doubled, the wrong timer count at a sync point) is
a difference.

## 4. The pitfalls, and what keeps each away

Each with what would go wrong, what the design does, and how it is checked.

**The runs**

1. *The follower runs past input not yet written.* It would run without the person's next key and become another
   game silently. → It never runs past the last `# at` mark (3.2). → A test writes a play file a line at a time
   with pauses and checks the follower's trace equals the finished file's.
2. *The window's and the follower's runs differ* (a nondeterminism bug, a window-only path touching the machine).
   → State hashes every 30 s (3.3); the final re-recording's run hash. → CI replays; a test that makes a machine
   differ on purpose and sees the check stop.
3. *Different builds.* The window and the follower from different Clockwork builds would differ for no reason in
   the game. → Both are the same executable, and the follower's `hello` source hash is checked against the
   window's, written in the play file's header.
4. *Different starts.* → Both load the same snapshot by its hash (or both start at power-on on the same folder,
   D170); the play file names it.
5. *Shared files between the runs.* If the window's saved games reached the follower's drive, the follower would
   load a save the window made later. → The original's file writes are the machine's state (Clockwork's
   `changed_files`, D131), so each run has its own; step 0 confirms it with a save made during a test play. The
   person's game folder is never written: both runs use the session's folder from the cache (D170).
6. *Input marks too coarse.* `# at` every emulated second puts the follower a second behind at best. → Every
   100 ms of play (a setting); the file grows by a few lines a second.
7. *A mark written before its inputs are flushed.* → The window writes and flushes the inputs, then the mark
   (as it does now); the follower reads only complete lines up to the last mark. On Windows the file is shared
   for reading while written (Rust's default sharing allows it), checked by the line-at-a-time test on Windows CI.

**Speed and the machine**

8. *The follower slower than play.* D124 measured recording at about real time on the dungeon: it would fall
   behind for good. → Step 0 finds where the time goes (the per-tick protocol round trips, the data segment read
   every tick, every refresh hashed and compressed, the call tracker) and makes the follower at least twice as
   fast as play on the dungeon, the heaviest stretch, before anything else is built. If raw emulation can't reach
   that, the fallback is the window instrumented (3.2), decided then with the person.
9. *The checking makes the window stutter.* → Separate processes, lower priority (3.6); step 0 measures the
   window's frame pacing with the checking running against without, and M6.5's gate holds it to the same.
10. *Memory on an 8 GB machine.* → The ceiling and the flat-memory test (3.6). A watch or an analysis tool is never
    run during a play.
11. *A long pass.* A cinematic or the start-up is one pass of minutes; its tick, and so its comparison, comes at
    its end. → The status line says the pass is going on ("in a pass: 1:10 so far"); nothing is late, there is no
    tick yet.

**The port**

12. *The port panics.* → The comparer reports the panic with its tick and the port's message, starts the port
    again from the original's state at the next tick (a transplant, made from the follower's machine: no snapshot
    needed), and goes on. Counted in the census.
13. *The port hangs* (a gated wait it goes round for ever, D143's case). → A time limit per tick (generous: ten
    times the slowest tick seen); past it, as a panic.
14. *The port's code changes during the play* (the agent builds while the person plays). → Built before the
    window opens, as `rrc play` does; D167 refuses a run whose code changed under it.
15. *The knowledge changes during the play* (the mapping, sync points, edited by the agent). → Read once at the
    start; the play's session records the knowledge's hash, as a recording does.
16. *Pipes deadlock.* → Writing inputs and reading the port's output on separate threads; bounded queues (3.4).

**What is compared**

17. *A cascade after the first difference* drowning everything after it. → The corrected free run (3.5).
18. *A difference said every tick.* → Each is said once, when it starts, with how long it lasted when it ends;
    the census counts the rest.
19. *Play outside what the port covers* (towns, the outdoors: paths not ported). → Said once by what the port
    noted ("towns: not ported, from 12:40"), not as differences; the state fed whole after each such pass (3.5).
    At the start, the port's scope is said in a line.
20. *Pictures between ticks and sound*: the same rules as offline (D180, D181, D108), since it is the same code.
21. *Timing inside a pass* in the `Live` check: a class of its own (3.7), never mixed with real differences.
22. *Quirks*: comparisons keep every quirk (§6.5); so does the live check. A switch is tried offline on the play's
    session afterwards (`rrc compare NAME --fix`).

**Storage**

23. *Pictures fill the disk.* A recording keeps every picture the display showed that differs from the one before
    (`cell`'s start-up pass alone: 456 of its 3616 refreshes), and a long play shows tens of thousands; step 0
    measures how many an hour of the dungeon keeps. → A live play's trace keeps the
    screens at sync points (each tick's, which the comparison needs) and, between ticks, the pictures' hashes
    only; the pictures themselves are kept around a difference, for its report. Any other is made again on
    demand by running from the nearest snapshot, which determinism allows. This changes what a trace promises,
    so it is a decision of its own (step 1).
24. *Snapshots fill the disk.* Every 10 emulated seconds (D51) is 180 for a half hour. → Every 60 s in a live
    play (a setting), and one at the tick of each difference.
25. *Not enough room to start.* → The free space is checked against step 0's measure per hour of play, and the
    play refused with a plain message if it is short. (The cache is 3.1 GB today for Arena's 58 sessions.)
26. *The trace held in memory.* → Written to a file as it grows, hashed and moved into the cache at the end.

**Failure and recovery**

27. *rrc dies, or the machine does, mid-play.* → The play file is the truth and is complete up to its last mark;
    `rrc play NAME --resume` (or the offline comparison) does everything again from it at full speed, without
    the window. Nothing the check made is needed to recover.
28. *The person closes the window, or quits the game to DOS.* → The play ends at the program's end or the
    window's close; the follower and the port finish what is left; the session is saved.

**The person**

29. *Too much to read while playing.* → One status line, rewritten every second, and a line per new difference
    (section 5). No sound, no window of the check's own unless asked for.
30. *Not knowing what a difference means.* → Each says its tick, the play's time ("at 4:12"), the field and both
    values; the end report gives the next moves (D159), on the play's session.

## 5. What the person sees

```
live      arena-walk: the original from power-on; the port follows (built), its scope: the menus, the
          creation, the first dungeon
window    play in it; close it to stop
live      0:42 played · tick 85 · 0.4 s behind · all matching
new       at 3:51 (tick 612): player.fatigue 140, the port 139: once, matching again at tick 613
new       at 7:05 (tick 1190): the screen, 312 pixels in (64, 40)-(250, 120), 6 ticks
live      12:40: a town, which the port doesn't port (what it noted): its differences counted, not said, until
          the play is back in what the port covers
play      arena-walk: 25:10 played, 3970 ticks, 2 differences, 0 panics, the runs the same (8 hashes)
next      rrc compare arena-walk --transplant 611: player.fatigue at tick 612, with who wrote the original's value
```

`--show-port` opens the port's frames in a second window, silent, as the comparer reaches them (a little behind
the play): to see a difference rather than read it. Off by default: it costs a core.

The play is one session (`sessions/NAME`), its trace recorded by the follower, investigated like any other.

## 6. Contracts that change

- **The play file** (`clockwork play --record`, `docs/formats.md`): marks every 100 ms (a setting), `# hash
  <cycle> <hash>` lines, and the header names the snapshot and Clockwork's source hash.
- **The protocol** (`docs/protocol.md`): "run to the next tick, but not past cycle C", if the present requests
  can't say it; and input given while a run is paused, which they can. A new revision if anything changes.
- **A port's run** (`docs/formats.md`): the inputs may arrive line by line; a tick's line, frame and sound are
  flushed before the port reads past the next tick's line; corrections (fed fields) may begin at any tick, as
  `fed` values already do (rrc-inputs version 2).
- **Traces:** pictures between ticks may be kept by hash alone (pitfall 23).
- **The platform:** `Live`'s clock is a parameter (3.7).
- **D124** is superseded: a play is followed continuously, as one session, instead of in parts.

## 7. The steps

Each ends with its test, and the next starts only when it holds.

0. **Measure** (before any design detail is final). On Arena's dungeon and the cinematic: raw emulation speed
   against play; where a recording's time goes; the window's frame pacing alone and with a second emulator
   running; the state hash's cost; memory of each process; disk per hour of play. Gate: a plan to make the
   follower at least twice as fast as play, or the fallback chosen with the person.
1. **Make the follower fast**, by what step 0 found. Gate: twice play's speed on the dungeon; traces unchanged
   (their hashes), or changed by a decision (pitfall 23).
2. **Streaming recording** from a growing play file, never past its last mark, with state hash checks. Gate:
   streamed equals batch on every session; the line-at-a-time test; the deliberate-nondeterminism test.
3. **Streaming port runs** (runtime, contract). Gate: every example and Arena's sessions give the same port
   traces through a pipe as from a file.
4. **The streaming comparer**, with `rrc compare` rebuilt on it, then the corrected free run. Gate: all 58
   sessions' results unchanged; `--corrected` equals `--lockstep` where the mapping is exact.
5. **`rrc play` live:** the window, the follower, the port and the comparer together; priorities, bounded
   queues, lag, panics and hangs, the status line, `--resume`. Gate: a flow test replays a recorded play of
   the toy and the vault through the whole pipeline at full speed, and a test port with a planted bug is caught
   at the right tick; memory stays flat over a long replay.
6. **The `Live` check** (3.7): the platform's clock a parameter, `--live` offline, then live. Gate: the
   examples' `Live` against their originals; Arena's against `cell` and the dungeon session, with every
   difference either fixed in the port or classed as timing inside a pass. Done (D194, D195, D197).
7. **The person plays** (section 8).

### What step 0 found (2026-11-16, D186)

Measured on the person's machine (Intel i5-8300H: 4 cores, 8 threads; 8 GB) with two of Arena's sessions:
`dungeon`, 157 s of play in the first dungeon (the heaviest stretch), and `cell`, 465 s from power-on (the intro,
the menus, the creation, the new game's story, the cinematic, the first minutes in the dungeon). The method:
`RRC_PROTOCOL_LOG` writes each request rrc makes to Clockwork, with how long its answer took, while a session is
recorded; the log is then replayed against `clockwork serve` with each thing a recording tracks left out, the
window's emulation is timed frame by frame, and a sampling profiler says where Clockwork's own time goes. The
scripts are in `clockwork/tools/speed/` (Clockwork's manual, "Measuring speed").

**Speed, against play:**

| | `dungeon` | `cell`, from power-on |
|---|---|---|
| `rrc record`, all of it | 0.74x (213 s for 157 s) | 0.93x (501 s for 465 s) |
| Clockwork as a recording runs it | 0.93x on its first 64 s; its slowest 10 s 0.78x | 1.01x; the new game's story 0.71x, the cinematic 1.03x |
| Clockwork, emulation alone | 2.2x on its first 64 s | 2.95x; its slowest 10 s (in the dungeon) 2.15x; the story 2.36x, the cinematic 3.3x, the intro 6.2x |

**Where a recording's time goes** (the dungeon's first 64 s of play: 68.7 s as recorded, 28.9 s emulation alone):
the late reads' watch 13.9 s, the hooks' entries 9.0 s, the reads of the state 7.7 s, the pictures between ticks
5.4 s, the call tracker 4.6 s and more; the screen at each tick and the data segment little. They overlap: all
together cost 40 s. The profile says why. The hooks' check asks at every instruction whether it runs in an interrupt
handler before it asks whether the instruction is a hook's; the late reads' watch sends every access between its
lowest source and its highest down the slow path; the call tracker works at every instruction; a recording's snapshot
brings its call tracker along (for watches from it), so a run from one tracks calls with nothing asking. Of emulation
alone, the instruction interpreter is about 60%, and EMS page mapping 7.5% (a 16 KB page copied out and in, byte by
byte, at each mapping). rrc's own work is 28 s of the dungeon's 213: the answers' 365 MB parsed (most of it the data
segment kept at every tick, D92) and the pictures written. Whole-program optimisation (LTO, one codegen unit) gains
nothing.

**The window** (each 70 Hz frame's emulation against its 14.3 ms, in the dungeon): from a recording's snapshot, as
`rrc play --from` starts it, 8.8 ms on average, 13.0 at the 99th percentile, 0.3% of frames over; that is the call
tracker the snapshot brought: without it 6.6 ms, 10.5, none over. With a follower running beside it, 7.0 ms, 12.8,
0.3% over (with the tracker, 9.4 ms and 2.3%). Below-normal priority changes nothing: what the follower takes is the
shared processor (its turbo, its caches), not the window's time. A state hash takes 9 ms: once every 30 s on the
window, one frame in 2100 a few milliseconds late.

**Memory:** Clockwork 180 MB recording the dungeon, 394 MB recording from power-on (a long pass's pictures are held
until its tick); rrc 131 and 259 MB. Three processes stay well under the 2 GB of section 3.6.

**Disk:** as recordings keep a run today, about 0.9 GB an hour of the dungeon (snapshots 380 MB, pictures 370, the
trace 160) and 1.1 GB of the start-up's stretch (its pictures between ticks 750 MB: the story, the cinematic). With
section 4's live settings (a snapshot every 60 s, pictures between ticks by their hash): about 0.6 GB an hour of the
dungeon, whose pictures are nearly all its ticks' screens, and 0.3 GB of the start-up's. Twenty minutes of play: 0.2
to 0.35 GB. The disk has 37 GB free.

**The plan for step 1.** Emulation alone keeps more than twice the pace of play (2.15x at its slowest); what a
recording tracks brings it under, and most of that is cost, not need. In the order of what it gains:

1. The hooks' entries: the address first; the handler question only at a hook.
2. The late reads: each source's own spans (a map of the pages they are on), not one range from the lowest to the
   highest.
3. The call tracker: its work only where the stack pointer moved; off in the follower where nothing it runs needs
   it; and the window without the one a snapshot brings.
4. EMS: a page mapped by copying it whole.
5. The reads through pointers in the tick's own request; no data segment kept by the follower (D92 is for
   recordings, so items can be added later without a run).
6. Pictures between ticks by hash in a live play (pitfall 23).

Estimated, the follower then runs the dungeon at about 1.7x. The rest of the way to 2x is the interpreter's (its
checks at each instruction, its decoding), measured again after 1 to 6. A change to the machine's code, as opposed
to an analysis tool's file (D173), changes Clockwork's source hash: step 1 ends with every session recorded again,
each trace's hash unchanged, which is its gate.

### What step 1 did, its first part (2026-11-17, D187)

Each change does what the code did before, faster; the measure is step 0's (the same logs, replayed).

1. The hooks' entries: an instruction's address is tested against a 64-bit mask of the entries' (their bits mixed)
   before the entries themselves, and whether a handler runs is asked only at an entry.
2. Whether a hardware interrupt's handler runs: counted as frames open and close (again when a snapshot is loaded),
   not looked for through the chain at every instruction and access.
3. The call tracker: an instruction that leaves the stack as it was, with the newest frame on this stack and the
   stack already unwound as it is, does nothing more; and the tracker alone runs on the plain bus, as it looks at
   instructions, not accesses.
4. The late reads: a bus of their own when no other probe looks at each access (a recording's case), the plain bus
   with each access's address tested against the sources' own bytes (a bit a page, then a bit a byte), not one range.
5. EMS pages, and memory read or loaded below 1 MB, copied whole.
6. No heap allocation where a polling loop meets one: the mouse driver's state, the device states the late reads and
   the gates compare, the tick answers' hex (it was a string a byte).
7. The window run from a snapshot drops the call tracker the snapshot brought.

| Against play | before | after |
|---|---|---|
| A recording's run of the dungeon, all of it | 0.90x | 1.83x (slowest 10 s 1.79x) |
| Its first 64 s | 0.93x | 1.92x |
| From power-on, all of it | 1.01x | 2.18x |
| The new game's story / the cinematic | 0.71x / 1.03x | 1.44x / 1.61x |
| Emulation alone, the dungeon's first 64 s | 2.2x | 2.45x |
| `rrc record`: the dungeon / from power-on | 213 s / 501 s | 109 s / 241 s |
| Arena's 58 sessions recorded | about 1.5 h | 18 min |
| The window's emulation per frame, from a snapshot | 8.8 ms | 6.6 ms |

All 58 of Arena's sessions were recorded again with the changed Clockwork (`clockwork/tools/speed/rerecord.py`):
every trace the same, by its hash; only each session's run identity changed, which names Clockwork's source. The
test games' walkthroughs end in the same state.

What is left before twice the pace of play on the dungeon (1.83x): the call tracker's work at each instruction that
moves the stack, about 5 s of the dungeon's first 64 s of play; in polling loops (the story), the late reads' work at
each device read; and the interpreter itself (51 ns an instruction, emulation alone). rrc's own work is now a fifth
of a recording (23 s of the dungeon's 109), which step 2's streaming runs beside Clockwork's instead of after it.

Tried next and left out (2026-11-17): working out each instruction's address once for every analysis, and the
stack's base and top once for the call tracker's two looks, changed nothing measurable; on this machine the same
replay varies by about 5% from run to run (its processor's turbo and heat), so a change is judged by alternating the
two builds, and one that doesn't show isn't kept (it would change Clockwork's source, and every session would be
recorded again for nothing). What remains is the interpreter's: its operand decoding isn't inlined into the
instruction loop for the late reads' bus (a tenth of the time), and a byte at a time is how it reads and writes
memory without paging. That is a project of its own, with the CPU suites run after it.

Where it stands against the gate: every stretch measured runs faster than play (the slowest, the new game's story,
at 1.44x), so a follower doesn't fall behind; the margin on the dungeon is 1.83x, not 2x. The person took that margin
(D188): step 1 is done, and the interpreter waits.

### What step 2 did (2026-11-17, D189)

- **The window** (`clockwork play --record`) writes a mark every 100 ms of play (it was every second), its
  Clockwork's source hash after the header, and the machine's state hash when the play starts and every 30 s
  (`--hash-every`), taken at a frame's start, before that frame's input.
- **The protocol** (revision 20): `run` and `tick` take `until_cycle`, a cycle they don't run past; `input` takes
  `events`, a play's lines as they are read from its file, refused at a cycle the machine has passed.
- **The follower**, `rrc record SESSION --follow`: it reads the play file's whole lines up to its last mark (a line
  after it may be half written), gives Clockwork the input up to there, and holds the run at the mark until more is
  written. At each state hash's cycle it compares its state with the window's and stops, saying where the runs
  parted, if they differ. The input at a hash's cycle and after is given only once that hash is checked: input
  queued ahead changes when Clockwork next services its devices, which the state hash covers, so a follower that
  queued it would differ from the window for no fault of the machine's. A play by another Clockwork is refused.
- **A window without a window**, `clockwork play --headless FILE`: the window's loop, a play's input given frame by
  frame at the frame's cycle, as a person's is, as fast as the machine runs. The tests use it in place of a person.

Tested (`rrc/tests/flow.rs`): a play file written a line at a time, a third of its lines in two halves, gives the
trace its finished file gives, and the window's state hash at its end is the follower's; a headless window's play
of the toy with a state hash every 0.25 s and its input at frames' cycles is followed with every hash the same, and
recorded whole gives the same trace; a hash that isn't the follower's stops it at its cycle ("left the window's at
0:01.5 of play"); another Clockwork's play is refused. Every one of Arena's sessions recorded again with this
Clockwork gives the same trace.

Left for step 4, where the comparer takes them: the ticks still go to the trace when the recording ends, not to a
sink as they are made.

Found on the way: Clockwork's source hash (D76) named each file by its whole path, the build script having reached
the workspace through `..`, so the same sources hashed differently in every checkout, and a session recorded with a
worktree's build named a Clockwork the main checkout's build of the same commit isn't. It names each file by its
path in the workspace now, and the build stops if one isn't in it.

### What step 3 did (2026-11-17, D190)

- **The runtime** reads the inputs a line at a time, from a file or from the standard input (`--inputs -`), and
  writes and flushes each tick's line, frame and sound before it reads the next tick's line, which its step needs.
- **rrc** gives every port run its inputs through a pipe, written by a thread of its own, and reads the trace as a
  file: the port's output never waits on rrc, so neither side can hold the other up. `rrc compare --streamed`
  gives a tick's line only once the port wrote the tick before, as the live comparison will, and says so when a
  port doesn't keep to that.
- **Tested:** every example's comparisons run through the pipe; the chime's sound and the vault's from power-on,
  with its frames and the game's files, fed a tick at a time give the same trace and frames as fed whole. Each of
  Arena's 58 sessions counted in lockstep with its port given the inputs file (`RRC_PORT_INPUTS=file`) and then
  through the pipe gives the same port trace byte for byte, the same frames and the same census.

### What step 4 did (2026-11-17, D191)

- **The comparer** takes the port's trace a line at a time and keeps the census as it goes: lockstep's census is
  counted so now, whether the lines are read from the finished trace or as the port writes them.
- **The corrected free run** (`rrc compare SESSION --corrected`, section 3.5): the port runs on its own state, fed
  through the pipe a tick at a time, and the inputs line of the tick after one it left otherwise carries `correct`,
  the original's values of the fields that differed or were unset, which the runtime sets over its state before
  that tick's step. After a pass the port noted (D115), every field that isn't exactly the original's is set. Its
  census is saved beside lockstep's; it says at how many ticks it corrected which fields, and apart from those, at
  how many ticks it set the state after a noted pass.
- **Tested:** the toy with its planted bug gives the same census corrected as in lockstep, byte for byte, the
  bounce it notes included; without the bug, nothing is corrected. Each of Arena's 58 sessions gives the census it
  gave before the comparer was rebuilt, and the same census corrected as in lockstep. Arena's free run differed
  nowhere; but 4744 of its 5076 passes were noted (a flat in the floor's walk, a path of the port's drawing not
  ported, noted in almost every pass), so the run was free in 332.
- **Found on the way:** the corrected run first set every field after a noted pass, the whole mapped state written
  into each such tick's line and read back by the port: on Arena it took 3.8 times lockstep's time (68 minutes
  for the 58 sessions to lockstep's 18). Only the fields that aren't the original's already are set now, and the
  free run's state is no longer copied at every tick: 15.2 minutes to lockstep's 14.7.
- **Left for step 5:** the original's ticks still come from the finished recording, not from the follower as it
  makes them (left from step 2); the first-difference comparison (`rrc compare` alone, with `--feed` or `--fix`)
  keeps its own loop, which the live check doesn't use.

### What step 5 did, its first part (2026-11-17, D192)

- **A recording gives its ticks as they are made.** Clockwork hands over what its late-input check and its entries
  found so far (`late_inputs_take`, `entries_take`); a tick is given once nothing more can come for it, and the trace
  is written to a file as it grows, hashed as it goes, its run's hash taken from the frames and sound written aside.
  `rrc record` is this recorder; what it holds no longer grows with the run (the plan's pitfall 26).
- **`rrc play` follows the play as it goes** (section 2): the follower on a thread of its own, at most four ticks
  ahead of the comparer; each tick decoded, given to the comparer and, as an inputs line, to the port, run free and
  corrected where it differs (step 4's comparer, fed by the follower instead of a finished trace). From power-on the
  port starts at once; from a session's tick, once the play's second tick is made, from the original's state at
  the first, read as `rrc compare` reads it. What differs is said when first found, with the play's time; a status
  line (on a terminal) says how much is played, the tick compared, the lag, a long pass. The follower's Clockwork
  and the port run below normal priority on Windows. The play's session ends where its recording would: the
  follower never runs past the millisecond of the last mark.
- **`--resume`** follows a play cut short again from its file, to its last mark; a window closed without its end
  mark ends the play at its last mark; `--replay` follows a play recorded before at full speed.
- **Tested:** the toy's play from a session's tick and the vault's from power-on (recorded by the headless window
  from the session's script) at full speed: the live census is `rrc compare --corrected`'s, the play recorded again
  is the same run, `--resume` gives the same census; the toy's planted bug is said at the tick the offline comparison
  finds. Arena's 58 sessions recorded again with this recorder give the same traces, byte for byte (in 20.7 minutes; 18 at step 1).
- **Left for the second part:** a port that panics or hangs is said and the play goes on being recorded, but its
  comparison stops (pitfalls 12 and 13: started again from the original's state at the follower's next tick, a
  hang timed against the slowest tick); memory over a long replay (pitfall 10).

### What step 5 did, its second part (2026-11-17, D193)

- **A port that stops starts again** (pitfalls 12, 13): one that panics, or writes no line for ten times its slowest
  tick so far (ten seconds at least; it is killed), is counted where it stopped and started again from the
  follower's machine at its next tick, as from a transplant: what it carries, the presentation's own and the games
  saved are read there, and its first line gives every input's value. Ten times at most; the play is recorded to its
  end whatever the port does. Tested on the toy: a panic and a hang each counted once, the port started again and
  matching from there.
- **Memory over a long play** (pitfall 10): Followed live, a 12:45 play of Arena from power-on into the first dungeon
  (2572 ticks, recorded by the headless window from the cell session's input and five more minutes in the dungeon)
  kept each process's memory flat (clockwork/tools/speed/memory.py, the committed memory each minute: rrc 22 to 34 MB,
  the follower's Clockwork 63 to 84, the port 12 to 18), once the data segment a recording keeps at every tick (D92)
  wasn't kept in a play followed live: it grew rrc by 23 MB a minute, to its 256 MB limit. The play ran at 1.43 times
  its pace, every one of the window's 26 state hashes the follower's; a 7:45 play's live census was `rrc compare
  --corrected`'s byte for byte, one field differing at one tick (a difference of Arena's port that its sessions don't
  reach).

### What step 6 did, its first part (2026-11-17, D194)

- **The clock a parameter:** a played port's `Live` takes its time from the host's clock when played (`Host::us`,
  in µs since D197); checked, the play's.
- **`rrc compare SESSION --live`:** the port in lockstep, its `Live` given at each of the original's sync points the
  session's devices as the machine had them (Clockwork's `queued`), on each tick's inputs line (`devices`); what it
  made is on the tick's line of the port's trace (`live`), and rrc judges it: the same, timing (a pass early or late,
  or its loop still waiting a moment), or a difference.
- **The examples:** the chime's `Live` ended at Escape a pass before the original (which reads Escape at the loop's
  top, right after a pass): fixed in the chime's and the vault's. Left: timing, their timers' phase (counted from
  play's start, the originals' from their install).
- **Next, its second part:** Arena's `Live` against `cell` and the dungeon session; then the check live, beside
  `rrc play`.

### What step 6 did, its second part (2026-11-18, D194, D195)

- **The check, a pass at a time:** `Live`'s frames for each tick start at the original's sync point before it (where
  the port's pass is), and where the port's `Live` takes it, its handlers start each pass from the original's state
  there (`Live::given`): each pass judged on its own. Timing now also takes a value the original's pass before read
  in its middle; a pass skipped (none by the original's next sync point) is a difference.
- **Arena:** five things fixed in its `Live` and its port (the driver's counts and motion, the swing's motion reads,
  the timer's interrupts, each wait's own snapshot, `given`); its censuses unchanged. The dungeon: timing only.
  `cell`: timing, and four ticks left (D195): a timer's interrupt in the sync point's millisecond (2), a placed
  mouse's motion counted from the host's cursor (1), a pass skipped where a wait's snapshot is the port's own (1).
- **Left:** those four; then the check beside a live play (`rrc play`).

### What step 6 did, its third part (2026-11-18, D197)

- **The check made exact:** the clock in µs (a timer's interrupt in the sync point's millisecond); the platform's
  mouse driver as an emulator's (motion from its own position, at its own precision; the host's mouse to a fraction
  of a pixel; a move to where it was taken); a wait's last poll the original's after each pass; `Live` asked at the
  original's moment too, and where the port's pass comes before it, a copy of its handlers asked there (timing when
  they make the original's); a byte the port's pass wrote itself, where the player's input came during the pass,
  timing too (the order of the two inside the original's pass).
- **Arena:** its retraces counted to the screen's refreshes after the pass, not as whole frames from the frame after
  it (its next pass up to two frames late, played too); each screen's poll puts the driver at the game's pointer
  (`+077F0`'s INT 33h 04h), as the original's does.
  `cell` and the dungeon: timing only. The chime: every tick; the vault: timing only.
- **Beside a live play:** `rrc play` gives the port the play's input on each tick's line and judges its `Live` at the
  play's end; the vault's play is judged as its session offline. A 12:45 play of Arena replayed through `rrc play`
  (2572 ticks): as the original's handlers made it but for timing at every tick.
- **Step 6's gate holds.** Next, step 7: the person plays (section 8).

## 8. Done when

The person plays Arena for at least twenty minutes from power-on, through the menus and the creation into the
first dungeon, with the port alongside, and:

- every difference is said within two seconds of its tick (the lag never above two seconds on the dungeon);
- the window's frame pacing is the same as without the check;
- the runs are proved the same throughout (every state hash, and the re-recording's run hash);
- the live census equals the offline comparison's of the same session;
- the port's `Live` makes the original's boundary values at every tick, but for differences classed as timing
  inside a pass;
- and the same pipeline, replaying recorded plays, passes in CI.

## 9. Considered and not chosen

- **The person plays the port, the original follows** (3.1): the comparison stops meaning anything at the first
  difference.
- **The window instrumented** (3.2): the person pays for the checking. The fallback if step 0 says the follower
  can't keep up.
- **Shorter parts** (D124 with `--every 5`): still a recording per part and a lockstep per part, a snapshot each,
  and every part a session; the seams and the overhead remain, the lag only shrinks.
- **Lockstep alone** (3.5): never says "identical" for the free run the person actually plays.
- **Comparing live in the port's process** (the port reading the original's trace itself): the port would carry
  the comparison, which belongs to the toolkit, and a port in another language would need it too.

## 10. What is the person's to decide

Decided 2026-11-16: M6.5 accepted, with section 8 as its done-when, and the `Live` check (3.7, step 6) inside it.
Decided 2026-11-17 (D188): step 1 is done with the follower at 1.83x on the dungeon, every stretch faster than play;
the interpreter's speed waits, and no fallback is needed. Decided 2026-11-18 (D196): no disk budget, as little as
can be: a play followed live keeps the pictures between ticks by their hashes alone and a snapshot a minute (a
recording of its session makes the rest again). Nothing is left the person's in this plan.
