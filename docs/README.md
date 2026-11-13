# Rrecreate: the hub

> Where work on Rrecreate starts (D153). **An agent** has this page before anything else (`CLAUDE.md` loads it) and
> works by it. **The person** reads this page and what it links: the objective, how the project is built, the rules,
> where the work stands and what was decided. It has no code and nothing a design section or a manual holds; each
> thing is said in one place, and this page says where.

## 1. The objective

**The complete toolkit** an AI agent uses to port a DOS game whose source was never released to modern systems
(D52, D70). What it produces for a game is a Rust port:

- **as close to the original as can be checked:** in its faithful mode it matches the original tick by tick, frame
  by frame and sound by sound, the original's bugs included;
- **built to be modernized:** the game's logic is plain, named code, separate from how the game is shown, heard and
  controlled, so graphics, sound, performance and platforms change without touching it. Modernizations are toggles
  on top; the faithful mode stays checkable for good.

It is finished when an agent with the playbook alone ports a stretch of a game the toolkit wasn't developed on, and
the port matches and plays (M9, section 5). **No game's port is a goal:** Wolf3D, Arena and Daggerfall are where the
toolkit is tested, and their ports go only as far as a test needs.

Who does what:

- **The person** supplies the game's files, makes the calls that are theirs (what to port, downloads, publishing,
  deleting, the decisions marked "with the person") and answers what the agent escalates.
- **The agent** drives: it analyses the game, writes the port, finds and fixes differences, and builds the toolkit.
- **The toolkit** does everything that isn't judgment: running the original the same way every time, analysis,
  bookkeeping, comparing, and tracing a difference to its cause.

## 2. How it works

A port is written as a new program, not translated from the old one, and judged by behaviour: the original is the
specification ([design](design.md) §2).

```
the person's copy of the game
        │
   Clockwork ───────── rrc ───────── the port
   runs the original   compares      new Rust code on the runtime:
   the same way        the two,      simulation │ presentation │ platform
   every time          tick by tick  (the logic)  (draws, sounds) (window, input)
        │                  │
        └─ at the first difference: which value, at which tick, and what in the original wrote it
           → fix the port, or register the original's bug as a quirk
```

1. The original runs in **Clockwork**, our emulator: the same inputs give the same run, bit for bit.
2. **Sessions** (scripted input, recorded play, the game's own demos) define what to reproduce.
3. The agent finds the game's state and declares a **mapping**: which field of the port is which memory of the
   original.
4. The port is written subsystem by subsystem, starting from state **transplanted** from the original, so nothing
   has to be ported before it can be compared; what isn't ported yet is fed from the original.
5. Both step through the same session; the toolkit compares state at every tick, and frames and sound too.
6. At the first difference the toolkit names the tick, the value and the original's code that wrote it.

What the agent learns about a game (names, types, formats, the mapping, sessions, bugs) is text files in that game's
own repository; the game's files are never committed anywhere.

## 3. The systems

| System | What it is for | Where | In depth |
|---|---|---|---|
| **Clockwork** | The emulator: a deterministic DOS PC that runs the original and answers questions about its run (who wrote this value, what ran, what the screen held) | [`clockwork/`](../clockwork/) (GPL) | [design](design.md) §4, [its manual](../clockwork/README.md), [third-party sources](../THIRD_PARTY.md) |
| **rrc** | The toolkit's one command: keeps a port's books, analyses the game (through Ghidra), records sessions, compares port and original, extracts assets, checks the rules | [`rrc/`](../rrc/), [`ghidra/`](../ghidra/), [`sigs/`](../sigs/README.md) | [design](design.md) §5, §7 to §9, [the playbook](porting.md) |
| **The runtime** | What a Rust port builds on: the step it is run by, its inputs, transplants, the seams to its presentation, the game's files and their codecs | [`runtime/`](../runtime/), [`types/`](../types/), [`ids/`](../ids/) | [design](design.md) §6 |
| **The platform layer** | What a port plays on: a window, input devices, sound | [`platform/`](../platform/) | [design](design.md) §6.1 |
| **The contracts** | How the parts talk: tools drive an emulator through a protocol, and exchange documented files with a port, so neither the emulator nor the port's language is fixed | [protocol](protocol.md), [formats](formats.md) | [design](design.md) §4.2, §9 |
| **A port repository** | One per game, outside this repository: the port, its knowledge, sessions and quirk register | Examples: [toy](../examples/toy/README.md), [chime](../examples/chime/README.md) (sound), [vault](../examples/vault/README.md) (data files, from power-on) | [design](design.md) §3, §9 |
| **Tests** | Small DOS programs that test each feature, and real games that reveal the gaps: Wolf3D (its source grades the toolkit), Arena, Daggerfall (32-bit) | [`testbins/`](../testbins/README.md), [`testdata/`](../testdata/) | [design](design.md) §12 |

## 4. The rules

These hold for every session of work. A rule changes only by the person's word or a decision.

**Before all of them, the first rule of every port, the person's (D160, D163):** every bug of the original that
changes what the game does is reproduced in the port and has a switch, kept by default and fixed by it, so that both
can be had. Every one, registered the moment it is found: a bug left out or fixed by accident shows in no comparison,
and one missed can cost the port. A bug that changes nothing the game does (a value nothing reads, a drawing slip
the game goes on the same through) is not registered. The toolkit says it first at every step, asks for it when a
piece of work is closed, and refuses a commit that names a bug without registering it or saying it changes nothing.

**What the work serves**

1. **The toolkit is the goal** (D70). Judge every step by what it adds to the toolkit. A game is ported only as far
   as a toolkit feature needs testing; a small test program is preferred where it tests as well.
2. **Fidelity first** (D39). A port is as identical to the original as can be checked; where that is unreasonable,
   the closest thing that is still well built for modernizing.
3. **Games only reveal gaps** (D10). Each gap becomes a general feature with a test of its own, never a
   game-specific workaround. What isn't supported is reported, never guessed.
4. **Clockwork stays deterministic** (D9): no host clock, no randomness, the same bits on every host.
5. **Emulator features are adopted, not invented** (D5): look at how the established emulators do it, port the
   best, name the source.
6. **Licences** (D12): everything is MIT except Clockwork, which is GPL. GPL code goes into Clockwork only, and the
   toolkit and the runtime reach Clockwork as a separate program, never as a library.
7. **Game files are never committed** (D33, D82), nor anything extracted from them. Each port is its own repository.
8. **Recreate and the person's earlier Arena project are disregarded** (D66, D69): not read, not used.

**How the work is done**

9. **One design, one list of decisions** (D35). A changed decision: the old one is marked superseded, the new one
   added, and the design updated, in the same commit. Nothing describes a superseded decision.
10. **A document changes with what it describes**, in the same commit: the protocol, the formats, the playbook, and
    this page (section 8).
11. **Checks before every commit:** format, lint, tests (`CLAUDE.md` has the commands); after a change to Clockwork,
    the test games still reach their screenshots. CI runs the checks on Windows and Linux.
12. **Straight to `main`:** commit and push after each feature, no pull requests, no asking; under the person's git
    identity, with the agent's co-author line. Check CI afterwards.
13. **Keep going; ask only for what is the person's:** a decision marked "with the person", what to port, a
    download, publishing, deleting. Nothing is deleted permanently by an agent.
14. **A rule that matters is checked, not remembered** (D148): when a rule keeps being missed, a tool refuses.
15. **Tool output is read by agents:** short, a label and a value per line; large data goes to the cache.

**When porting a game:** the playbook, [`porting.md`](porting.md): its rules ("How to work", D77), which `rrc rules`
prints with the port's own, and its workflow (D154), the same for every game: eight stages, each done when the
toolkit says its gate holds, and one loop the work repeats. In short: the analysis first, then outside sources, then
the game's files, and runs to confirm; every bug of the original that changes what the game does is registered as a
quirk when found; nothing departs
from the original silently; where the work stands is the toolkit's to say, not the notes'.

## 5. Where we are

As of 2026-11-13 (decisions to D170). The plan is the design's ([design](design.md) §12, D71); this is its state,
kept here and nowhere else.

| Milestone | State | Shown by |
|---|---|---|
| M0 Repository | done | Clockwork's tests pass; the test games reach their checkpoints |
| M1 Control | done | Tools drive Clockwork and find who wrote a value at a tick |
| M2 Toolkit | done | A session of Wolf3D traced; two blind tests of the playbook shaped it (D46) |
| M3 Runtime and comparison | done | The toy port matches its session tick by tick |
| M4 A real game's logic | done | Arena's first dungeon matches from a transplant to the session's end (D100) |
| M5 The game's data: its assets and the whole program | done | The vault port matches from power-on; Arena's assets extracted equal what it loaded (D106) |
| M6 Faithful and playable | under way | Frames and sound are compared and a difference located (D107, D108); a port plays in a window with sound (D119) |
| M7 Modern mode | ahead | |
| M8 32-bit games | ahead | |
| M9 Acceptance | ahead | |

**Now (M6):** the comparison of frames and sound, the platform layer and play from a session's tick or from power-on
are built (D107 to D145). The work is widening what they are tested on: Arena's menus and systems (save and load,
the character sheet, the automap, the logbook, rest, spells, items), their logic first and their screens after,
compared in lockstep with the person's recorded play (D124). The latest gaps it turned into toolkit features:
scenario edits in a lockstep (D146, D147), the quirk rule checked by `rrc check` and each quirk given a switch
(D148, D149), what no session ran listed (D150 to D152), a scenario edit's tick drawn in a lockstep as the pass
left it (D166), a port run refused when its code changed under it (D167), a played port's sound output given the
buffer Windows needs (D168) and the Sound Blaster's output filter (D169, with the person), and each session run on
the game's folder as it was recorded, so that playing the game outside the toolkit changes no session (D170).

**The standard workflow, built before M6 goes on** (D154 to D159, 2026-11-12), at the person's word. The playbook is
one workflow for every game: stages with gates, one work loop, where to look when the port differs, the best-known
routes. The toolkit drives it: a port is given its entry point when it is made; its books hold its scope and its
outside sources; `rrc status` works out the stage the port is at and what its gate lacks; `rrc work` is the ranked
work list, with a card for the item taken that names a second route; a journal says when a route is stuck; each
difference ends with the next moves, of more than one kind. A test walks the toy port through the stages
([the plan](plans/porting-workflow.md) lists what was built and what the trial changed). Above the playbook's rules
stands the first rule (D160, section 4): the toolkit states it first in what an agent runs most, asks for each
quirk's fix and for a session that shows its switch, and reads the notes for bugs left unregistered. Its scope is
the bugs that change what the game does (D163, the person): the rest are ignored.

Tried on the Arena port: the toolkit answers in about a second, and what the assessment below found by hand it says
unasked. It puts the port at stage 1, because its outside sources aren't in its books; past that, the gates name
eight named data nothing maps or ignores, two suspects not judged, the first session stopping from power-on at the
cinematic's sound, and fourteen sessions whose frames or sound differ. Its 58 sessions counted again give a work
list of 121 items. Its scope is all 58 (the person, 2026-11-12). The first rule's checks found two bugs of the
original its notes named and its register didn't (both now registered, with switches), and three registered bugs
whose kept path was still a note. Taken through the gates since (2026-11-12): its outside sources are swept and in
its books (Arena's sound driver is Miles' AIL 2, whose source its author released), the named data are all mapped
or set aside, and the toolkit puts it at stage 4. One of the three kept paths is reproduced; the other two read
memory the port doesn't model (a buffer ten routines write their texts to; the renderer's state, for people in
towns), which its notes say. 14 of its 30 quirks' switches are shown by a session (three that changed nothing
the game does left the register, D163).

**Open against M6's row** (measured on the Arena port, 2026-11-11):

- **The music.** The port makes the digitized sounds only. The OPL's music, most of Arena's sound events, is neither
  made nor compared on a real game; the chime test program alone tests that path (D108, D121).
- **The actions seam** ([design](design.md) §6.1). Built (D162): a port declares its game's actions, each the PC
  input the original takes for it, and the platform's controls bind the host's keys to them, each still given to
  the port as the original's own input; tried on the vault, and Arena's world (21 actions) reads its keys through
  them.
- **Frames beyond the first session.** The first dungeon's session matches state, screen and digitized sound at all
  656 of its world's ticks, and its continuation now at all 775 (2026-11-12: the floor's and ceiling's tables kept
  as the original's logic builds them, the pits' sides from inside a chasm, the splash into water). All 58 sessions
  match the original's state in a lockstep census. Since 2026-11-13 the logbook, the rest's screens, the status, the
  character sheet (both pages), a load (in the game and from the main menu), a wall dissolving (Passwall),
  levitating and a lockstep's scenario edits (D166) match in frames and sound too: 56 of the 58 sessions match
  in every field, frame and digitized sound. What still differs: the automap's screen (it needs the game's record of
  the cells seen, not ported yet) and the cinematic's sound (the cinematic isn't ported).
- **From power-on** the comparison stops at the sound of the cinematic before the dungeon, which isn't ported.

**Next:** the Arena port taken through the workflow from where the toolkit puts it (its entry files, its sources
recorded, its suspects judged, the top of its work list); then the open points above, which stay M6's, in the
person's order (D161): the frames past the first session, the actions seam, then the music (the person is told
before it starts); then M7.

**The person's calls ahead:** closing M6, as M4 and M5 were closed (D100, D106); the game M9 is run on, which stays
out of the toolkit's development until then (D71).

The test games' own state is in their repositories on the person's machine (Arena's: `rrc status` there; its
`notes/handoff.md` says how to pick it up).

## 6. What was decided

Every decision is in [`decisions.md`](decisions.md) once, numbered, with its reason and its status;
"with the person" marks those the person made or confirmed (about a third), and the rest were the agent's within
what the person asked for. The ones that shape the project:

- **The product:** a faithful Rust port built to be modernized (D39), for games without a source release (D52); the
  goal is the toolkit, and games test it (D70, D71).
- **The approach:** port by behaviour, compared with the original as a whole program (D1); decompiled code is read
  to understand, never translated (D2); the analysis, outside sources and the game's files first, runs to confirm
  (D77).
- **The emulator:** our own, deterministic, a long-lived process tools drive (D4, D7, D9), with "who wrote this
  value" as a first-class tool (D8); one protocol any emulator could speak (D58).
- **The port:** three layers, simulation, presentation and platform (D13), meeting through seams that carry meaning
  (D40); time is an input, so faithful and modern modes (D14); departures are toggles (D19).
- **The comparison:** inputs read from the original's memory (D15), a state mapping that works both ways (D16),
  transplants and save states (D17), frames as pixels and sound as events (D25); the person plays the original and
  the toolkit follows (D124).
- **The original's bugs:** kept in the faithful port, each registered, checked and switchable (D18, D74, D148,
  D149); every one that changes what the game does, as the first rule of a port, never out of an agent's sight
  (D160, D163).
- **Knowledge and files:** what is learned is text in the port's repository (D31); tools and ports exchange
  documented files, not code (D38, D61); game files are never committed (D33).
- **Process:** the agent drives, and no program runs the agent (D34); one design and one list of decisions (D35),
  entered through this page (D153); every port follows one workflow, its stages' gates said by the toolkit (D154).

## 7. The documents

| Document | What it is | Who reads it, when |
|---|---|---|
| [`README.md`](README.md) (this page) | The hub: objective, structure, rules, state, the map | Everyone, first |
| [`design.md`](design.md) | The design as decided, by section, and the plan (§12) | The agent: the sections its work touches. The person: for depth on a system |
| [`decisions.md`](decisions.md) | Every decision, its reason and status | Before changing anything a decision covers; to learn why |
| [`porting.md`](porting.md) | The playbook: how an agent ports a game with `rrc`, its rules first | An agent porting a game; an agent changing a step of the workflow |
| [`plans/porting-workflow.md`](plans/porting-workflow.md) | The plan being built (accepted, D154): the standard porting workflow and the toolkit changes that make it the toolkit's core, with what is built so far | The agent building it; the person, for what is left |
| [`formats.md`](formats.md) | The files `rrc` and a port read and write | An agent changing or reading them |
| [`protocol.md`](protocol.md) | How tools drive an emulator | An agent changing Clockwork's control or `rrc`'s use of it |
| [`../clockwork/README.md`](../clockwork/README.md) | Clockwork's manual: running, driving and investigating a program, validating the CPU | An agent working on or with the emulator |
| [`../sigs/README.md`](../sigs/README.md), [`../testbins/README.md`](../testbins/README.md) | Library signature packs; the test programs and how they are built | An agent adding one |
| [`../examples/`](../examples/) | Three whole port repositories, small enough to read | Anyone wanting to see what a port is |
| [`../THIRD_PARTY.md`](../THIRD_PARTY.md) | What came from which emulator, under which licence | An agent porting a feature into Clockwork |
| [`../CLAUDE.md`](../CLAUDE.md), [`../AGENTS.md`](../AGENTS.md) | What loads this page for an agent, and its technical notes (commands, test helpers) | Agents; loaded automatically |
| [`../README.md`](../README.md) | The project's face for a visitor | A visitor |

## 8. A session of work, and keeping this page

An agent working on Rrecreate:

1. **Start here.** Find the work in section 5; if what was asked isn't on the way to the objective, say so before
   starting.
2. **Read what the work touches:** the design's sections and the decisions they name, then the code.
3. **Work by the rules** (section 4). A choice between designs that the rules and decisions don't settle, and that
   is the person's kind, is asked; the rest is decided, and recorded as a decision with its reason.
4. **Finish in the documents:** the decision, the design, the manual a changed step belongs to, and this page.
5. **Report in this page's terms:** which milestone the work served, what holds now, what is next, what waits for
   the person.

This page changes when: a milestone's state or the work under way changes (section 5, with its date); the person
sets a rule (section 4); a system or a document is added (sections 3 and 7); a decision joins those that shape the
project (section 6). A new document goes in `docs/` (a plan that outlives a session: `docs/plans/`) and gets a row
in section 7. It stays a page a person can read through: no code, addresses, formats or command reference.

A test keeps it true (`rrc/tests/hub.rs`): it fails when a document isn't listed here, a link leads nowhere, the
milestones aren't the plan's, section 5 doesn't name the latest decision (so every decision passes through it), a
state appears outside this page, or `CLAUDE.md` stops loading it.
