# Porting a game

The agent's playbook (docs/design.md §10, D154): the one workflow every port follows. Its rules come first ("How to
work"), then the workflow (its stages, each with a gate; the loop the work repeats; where to look when the port
differs; the best-known routes), then each stage's reference. Formats are in `docs/formats.md`.

Run `rrc` inside the port repository. A session of work starts with `rrc rules` and `rrc status`, and ends with `rrc
check` passing and a commit. `rrc status` names the stage the port is at, what its gate lacks and the next step;
`rrc doctor` says what's missing.

## How to work

These rules come before the steps below (D77). Every question about the game is answered the same way:
what a value means, where something is kept, how a rule works, where things are in a level, or why the port
differs.

**The sources, in this order:**

1. **The analysis: how the program works.** Start from what is already known (a named function, a datum, the
   loop) and read the program. `rrc show NAME` gives callers, calls, and who reads and writes a datum;
   `rrc show ID --c` gives the decompiled code with your names; `--asm` covers what the decompiled code gets
   wrong or misses. Follow the calls from the loop down to the subsystem in question.
2. **Outside sources: what others found.** Format documentation, reimplementations, wikis, walkthroughs, bug
   lists (D32). They are expected: look before working out what others have already written down. They are
   not relied upon: each is evidence, named with its source (`prior-art:<source>`), and the original decides
   where they disagree with it.
3. **The game's files: its data.** Levels, tables, texts, pictures and sounds are in the files the program
   reads. A structure in memory often comes from one: the code that reads the file says which, and so does
   the file itself, decoded with what the code and the sources say of its format.
4. **Runs: how the program runs.** Confirm with the original running:
   - `rrc watch`: who writes and reads a value, at which tick, from where;
   - `rrc search`: memory by how it behaves over ticks;
   - `rrc trace` and `rrc frame`: values and pictures by tick;
   - a `clockwork serve`: exploring step by step.

**The decompiled code shows how the program works, not how it runs.** It shows the logic and the data it
uses. It doesn't show:
- which paths run, with which values, how often and in which order;
- what interrupt handlers and code made at run time do;
- what the timing does.

It is partly wrong too (D2). Read it to understand; runs settle what actually happens. The comparison with
the original judges the port, not the code it was read from.

**Rules:**

**The first rule, the person's (D160, D163): every bug of the original that changes what the game does is reproduced
in the port and has a switch.** Kept by default, so the faithful port does what the original does; fixed by its
switch, so both can be had. Every one that changes the game's state, and what follows from it, with no exception:

- **registered the moment it is found** (`rrc quirk NAME --at ID --what TEXT --fix TEXT`), before the code that
  reproduces it is finished and before anything else is looked at: a note, a comment or a memory doesn't count;
- **branched on** in the port: `if rrc_runtime::keeps("NAME") { the original's way } else { the fix }`;
- **shown:** its switch tried on a session that reaches it (`rrc compare SESSION --fix NAME`), which keeps where the
  fix changes the run.

A bug left out, or fixed by accident while the code was made tidy, shows in no comparison and can't be found again:
one missed can cost the whole port. So it is never out of sight. `rrc status` and `rrc check` say the register's
state first, every time; `rrc check` refuses a commit while a bug is named in the code, the notes or the knowledge
without being registered; and closing an item asks which bugs it met (`rrc work done --bugs`).

A bug that changes nothing the game does is not registered and has no switch (D163): a value nothing reads
afterwards, a flag nothing tests, a register left wrong that nothing uses, a drawing or a sound that goes wrong while
the game goes on the same. Where it shows in a frame or a sound, the port reproduces it as it does the rest of the
original. Say so where it is met (`changes nothing: WHY` in a comment or a note; a suspect: `rrc suspects --judge ID
--changes-nothing WHY`). Whether the game's state is touched is read in the code and checked with a run (`rrc watch`
on what it wrote: nothing reads it, or what reads it goes the same).

1. **Plan before running.** Write the question down and the cheapest way to answer it from the sources
   above. A run checks an answer; it is not how to look for one.
2. **Don't play the game to find things.** Script a session from what the analysis, the sources and the
   game's files say: the level's layout, where the doors and the creatures are, how a door opens. Explore
   from a snapshot only for what they don't say. At each step read the state (the player's position, the
   value in question) instead of judging by pictures alone.
3. **Every finding has two kinds of evidence:** what the code or a source says, and what a run shows. One
   alone is a guess.
4. **Name what you learn as you learn it** (`rrc name`, `rrc data`, `notes/`), with its evidence and
   provenance: the next question starts from the names. A name that turns out wrong is taken back with
   `rrc forget`, not edited out of the files.
5. **Use the whole toolkit.** Pick the tool that answers the question at hand, not the one used last. If a
   few tries of one approach haven't answered, change the source, not the number of tries.
6. **The original is the specification.** Code, sources and files explain it; where they disagree with how
   the original runs, the run wins, and a disagreement worth keeping is a note or a quirk.
7. **Every bug of the original that changes what the game does is registered as it's found** (the first rule; `rrc
   quirk`, "Quirks" below; one that changes nothing is said so and left), and the port branches on
   `rrc_runtime::keeps`. A comment or a note saying so doesn't count: `rrc check` refuses it. Look for them too: `rrc
   suspects` on every session, and each finding judged.
8. **Nothing departs from the original silently.** Where the port would do other than the original does (a guard
   against its divide by zero, a reading of what the code means, a value it can't model), it's made faithful,
   registered as a quirk, or an `unported` note where it happens.
9. **An ID comes from a tool, never from arithmetic.** `rrc show DS:7A46` and `rrc data DS:7A46 NAME` take data as the
   code addresses it (D90), and `rrc show` says an address's ID. An ID worked out by hand is a guess.
10. **Batch what forces a recording.** The names, sync points, inputs and hooks of the item at hand go in together,
    then one `rrc record`: a recording is minutes, a comparison seconds.
11. **Find every loop of a stretch before building sessions on it.** A sync point added later numbers the ticks of
    every session through that loop otherwise.
12. **Never tune a layer over an unmatched one below it:** the mapped fields, then the frames, then the sound, then
    live play. A frame is only compared where the state matches.
13. **Where the work stands is the toolkit's to say** (`rrc status`, `rrc work`, `rrc progress`). Notes hold what was
    learned and why, never the state of the work.
14. **A gap in the toolkit is fixed in the toolkit,** as a general feature with a test of its own, never worked
    around in the port (D10). Then the item goes on.
15. **The person plays; the agent reads the play.** A play followed alongside ("Playing alongside") finds what no
    script reached and checks the port's input handling; its census is a work list like lockstep's, worked from
    transplants and short sessions cut from it. The agent never plays to find things (rule 2): it asks the person
    for a play of the stretch, from power-on or a session's tick.
16. **A play is evidence: kept, and followed again rather than trusted.** Commit its session with the port and
    never delete it. Its census counts for the port's code that made it: after the port changes, follow it again
    (`rrc play NAME --resume`); after Clockwork changes, with `--other-build`.
17. **A long session is heavy in every command.** A play of minutes is thousands of ticks: compare it from a
    transplant with `--to`, cut what the work comes back to into short sessions, keep a long play out of the scope
    once its items are, and run one heavy command at a time.

**Where to start, by question:**

| Question | Start from | Then | Confirm with |
|---|---|---|---|
| What does this value mean? | `rrc show DATUM`: who reads and writes it; their `--c` | sources on the subsystem | `rrc watch` at a tick |
| Where is X kept? | the code that does X, from the loop down | format and engine documentation | `rrc search`, `rrc watch` |
| How does rule X work? | the function that applies it, `--c` | sources, bug lists | a session that exercises it; `rrc compare` |
| Where are things in a level? | the level's file (`rrc files --file`) and the code that loads it | maps and walkthroughs | frames, the state at the place |
| Why does the port differ? | `rrc compare`: the original's writer and the port's code | the writer's `--c` | `rrc watch`, `rrc trace` |
| What is this memory at tick N? | `rrc peek SESSION --tick N ID...` (no naming or recording needed) | `rrc show ID`: who writes it | `rrc watch` across ticks |
| What does the program's own data hold (a list of names, a table)? | `rrc show ID --texts N` (N NUL-ended texts from there) or `--bytes N`, from the program's image | the code that indexes it (`--c`) | `rrc peek` at a tick, where the run writes it |
| What does a pass run, in what order? | `rrc pass SESSION --tick N`, `--in FUNCTION` | the loop's `--c` | `rrc watch` of a value it changes |
| What did the person's play find? | its census and `follow.log` (`.rrc/play/NAME/`), most passes first | `rrc compare NAME --transplant T --to T+N` at an item's first tick | a short session cut from the play's tick |
| Does the port's input handling do what the original's does? | the play's `live` (`live.json`), by input | the original's handler (`rrc show ID --c`) and the port's `Live` | `rrc compare NAME --live` |

A stretch whose screens aren't drawn yet (the logic first) is compared field by field alone:
`rrc compare SESSION --fields` (D136) stops at the first field that differs and at no screen or sound.

A pass's order matters as much as its code: `rrc pass SESSION --tick N` shows the calls one pass makes, in
order, by name (`--in FUNCTION` for one function's), which is the outline of the port's step (D83); tick 1's,
from power-on, is the start-up's, the outline of `power_on` (D134). A call
made again in a row shows once with its count (`×8`), and a loop's calls made again round after round show
once, then `the 4 calls above ×10800`. `--in FUNCTION --functions` lists every function that runs beneath one,
at any depth, with how many times: what porting a part takes (a renderer's fifty functions), to read and port
one by one.

## The workflow

Eight stages (D154). A stage is done when its gate holds, and `rrc status` works every gate out from the repository
each time it is asked (D158): a gate can't be marked done, and a note doesn't count. It names the stage the port is
at (the first whose gate lacks something), what the gate lacks and the next step. `rrc status --all` says what a
handoff would: every stage's gate, each session of the scope that doesn't match in everything, the top of the work
list, what parts not ported still give the port, and what the work was made of. A gate is reported, not refused:
stages overlap in honest work, and a commit is refused only for what `rrc check` refuses. What a later stage finds
wrong in an earlier one is fixed there first (rule 12). Stages 4 to 6 repeat one loop.

| Stage | What it is for | Its gate, as `rrc status` tests it |
|---|---|---|
| **0 Set up** | The repository, the game's version, the tools, the person's rules | The game's copy is a recorded version; Clockwork is found; `RULES.md` has no question left |
| **1 Survey** | Know the program before porting any of it | The analysis is in; there is a session, a sync point and a boundary value, and the session is recorded as the knowledge is now; the outside sources are swept, each kind found or looked for |
| **2 Sessions** | Fix what is to be reproduced | The scope is set and names sessions; each is recorded as the knowledge is now |
| **3 Skeleton** | A port that compares before it does anything | Something is mapped and the port's program named; every session has a census of its recording; no named datum is neither mapped, carried nor ignored |
| **4 Logic** | The simulation, subsystem by subsystem | No census has a field the port leaves otherwise or unset, or a path noted as not ported; every session matches in its mapped fields to its end with nothing fed; `rrc check` refuses nothing; every quirk's switch has been shown by a session |
| **5 Data and start-up** | The port loads the game itself | Every session that starts at power-on matches in its fields from the port's own start |
| **6 Presentation** | Frames, then sound | Every session matches from its start in state, frames and each device the original sounds in it; no census has a frame or a sound that differs |
| **7 Play** | A port a person plays | Not something `rrc` tests: it plays from power-on in a window, saves load both ways with the original, and what the person's play-test found became sessions. The person closes it |

The gate is what a tool can test. A stage does more, which only the agent can judge and its section below says:
every loop of the stretch given its sync point, the game's files listed, a play pass's top calls named, what hooks
and parts' inputs still give the port. The sessions measured are the scope's (`rrc scope`); a comparison counts for
the recording it was made of, and one made before the port's code changed is said to be `stale`.

Why this order: a transplant means nothing has to be ported before something is compared, so the port exists, empty,
before any logic is written; loops, state, files and sources are cheap to map early and expensive to discover late;
and each layer stands on the one below.

**The work loop** (stages 4 to 6), once for each item of the work list. `rrc work` is the list (D158): what the
censuses of the scope's sessions count (`rrc compare SESSION --lockstep`, D124), merged over the sessions: the mapped
fields first, then the paths noted as not ported, then the frames, then the sound, the most passes first. `rrc work
--run` records first each session the knowledge left stale (bases first, mostly without the emulator), then counts
again each session whose census the port's code as it is now didn't make and the change can reach, and compares
again each that nothing compared since (the fields alone before stage 6), so one command brings every gate up to the
code (D219). A change reaches a session when its run executed a place the changed code names (or, for a helper or a
constant naming none, the code that uses it): `rrc suspects` keeps what each run executed, so judge every session's
suspects early; `rrc status` names the sessions none looked through (`unlooked`: never spared; `rrc suspects --unlooked` looks
through each) and what the change
reaches, item by item (`reaches`: its places and how many sessions run them that their kept censuses and comparisons
were made before it changed; all of it with `--all`). The rest are `spared`, their censuses holding; the core sessions (`rrc scope --core`: the start from
power-on and a session of each main system) are counted at every change; `rrc work --run --all` counts every session,
before a stage's gate is taken as held and at the end of a day's work.
A play the person made alongside the port ("Playing alongside") has a census of its own (`.rrc/play/NAME/`): its
items are taken the same way, and worked from transplants and short sessions cut from it (rules 15 to 17).

1. **Take** the top item, with a card: `rrc work take ITEM --question TEXT --route TEXT --other TEXT [--budget N]`.
   The question, the route chosen, one other route considered, and after how many comparisons (6 unless said) the
   route changes. Naming the other route is the point: it is the one moment "is there a better tool" costs nothing.
2. **Plan and read**, in the sources' order: what is named already; what the outside sources say of it, which
   `rrc work take` prints from their digests (D185), and where it prints `search`, a search before anything else;
   the decompiled code; the disassembly where it is in doubt; the files it reads. What a source said that the work
   used is cited (`prior-art:NAME`), and what was found on a search goes in that source's digest.
3. **Name and map** everything the item touches, together; record once (rule 10).
4. **Port** it: a function for each of the original's, its ID in the comment; each bug registered as it is met; each
   path left out noted where it would be (`rrc_runtime::note`).
5. **Compare** from a transplant just before the item's first difference; at each divergence, the table below. When
   the card's comparisons are spent `rrc` says so (`budget`): change the route, and take the card again with it.
6. **Close:** `rrc work --run` (what the change reaches, and the core), the item gone from the list; the suspects of the sessions it touched judged; `rrc
   check`; `rrc work done --bugs none` (or `--bugs NAME,...`): closing asks which bugs of the original the item met,
   each one registered (the first rule), and says what the item took (it goes in the journal); a route found the
   hard way written as a recipe; commit.

**When the port differs,** `rrc compare` ends what it found with `next`: the moves for that difference, always of
more than one kind (its writer's code, the value over the pass, what is written of it), so the other tool is met
where the choice is made (D159). Then `source`: what the outside sources' digests say of the field and of the
function that wrote it, the lines themselves, so what others found is read before the code is (D185); or `search`,
where none says anything of it. The table is the same thing for reading:

| What the comparison shows | Look first at |
|---|---|
| A difference at the first tick after a transplant | The mapping: a field missing, or its encoding |
| A field differs and a writer is named | That function's code (`rrc show ID --c`), and the port's function of the same ID |
| The writer is an interrupt handler, or nothing in the pass wrote it | An input not declared, or a handler's work that is a subsystem (D22) |
| It matches for many ticks, then drifts with the player's input | A late read (`rrc trace SESSION --tick N`): read it where the original did |
| It differs only once a feed is removed | The fed part: the comparison says how often the port's own value differed |
| It differs right after a scenario's edit | The edit, before the port |
| The port does what the code means and the original doesn't | A bug of the original's: `rrc suspects`, then a quirk |
| The state matches and the frame doesn't | The drawing code's own state at the transplant (`[[present]]`), then who drew the pixel |
| The state matches and the sound doesn't | The order of events (`rrc trace SESSION --tick N`), then a driver's answer the port needs a hook for |
| A play says `lost` | Where the port's loop takes its passes at the first tick of the stretch: the pass from another sync point, and what the original's took |
| A play's `live` says `differs` | The original's handler for that input (keyboard, timer, mouse driver), then the port's `Live` |
| A play says `panic` | `rrc compare NAME --transplant T` at its tick: the port's own failure |
| Three tries with no progress | Another kind of source: the code instead of runs, an outside source, the game's file. Then fresh eyes, below |

**When stuck.** Three tries of one route that leave the first difference where it was mean the route has stopped
paying (rule 5). `rrc` says so, since it can't be seen from inside (D157): it keeps the commands run in the port,
and a comparison that ends at the same first difference a third time running says `stuck`, and `untried`: the kinds
of evidence not looked at since that difference first showed (the code, an outside source, the game's files, the
value over the run), with what the sources' digests say of it when no source was looked up. Take one of those next,
not a fourth try. When none is left, stop and write the question
without its history: what is asked, what is known (`rrc show` of the
place), what was tried. Give that, not the transcript, to a fresh agent, and ask it for routes: it has no sunk
cost. Where none can be started, or the port's `RULES.md` says not to, read `rrc rules` again and plan from the
question as if new. The same before each new subsystem: the routes first, then the work. If no route answers, it is
the person's.

`rrc journal` says what the work was made of: the commands, how they divide between comparing and each kind of
looking, how often a comparison was stuck. A stretch that is all comparing is the drift this is there to show.

## Recipes, by problem

The best-known route for what every port meets, its tools in order. A route found the hard way becomes a recipe
before its item closes: here when it is general, in the port's `notes/recipes.md` (a table like this one) when it
is the game's, so the next agent starts from it. `rrc how WORD...` prints the recipes that have every word, from
both, with the questions of "Where to start"; `rrc how` alone lists what they are about (D159).

| Problem | Route | Cost |
|---|---|---|
| Where a tick ends | `rrc calls SESSION`: the large groups are loops; in a group, the function that waits for the timer; check with `rrc show ID --c`; `rrc sync` where the wait returns | one run |
| A loop that polls (a menu) | Huge counts in `rrc calls`; a gated sync point at its read of the controls: `rrc sync ID NAME --when-changed ...` | one run |
| What a tick reads from outside | `rrc inputs SESSION`, by handler; `rrc ports SESSION` for a device answered in registers; `rrc input` | one run |
| Where a value is kept | `rrc trace SESSION --item KEYS` for when the keys are held; `rrc search` with conditions over a wide span; `rrc watch` the few left; name it | seconds once a span is kept |
| What a value means | `rrc show DATUM`: who reads and writes it; their `--c`; the sources on the subsystem; `rrc watch` at a tick | seconds |
| A file's format | `rrc files SESSION --file F`: who reads it; the reader's `--c`; format documentation; `knowledge/formats.toml`; `rrc asset F --session S` until `equal` | one run a check |
| A table the code reads | Its address as the code has it (`rrc show DS:7A46`); read from the person's copy (`original::program()`), never copied | seconds |
| The outline of a step | `rrc pass SESSION --tick N`; `--in FUNCTION --functions` for what a part takes | seconds from a snapshot |
| Code the analysis missed | `rrc analyze --session NAME`; `rrc show ID --asm`; made at run time: `--asm --session NAME` | minutes |
| A value that matches, then drifts | `rrc trace SESSION --tick N` lists the late reads; move the sync point, or read it with `at_site` | one recording |
| A state deep in the game | `rrc session add NAME --from SESSION --tick N`; a cheat of the game's; `rrc session edit` | one recording |
| What a part not ported gives the ported code | `--feed` at sync points; in the pass, `rrc input --part --read-at`; its calls, `rrc hook` | one recording |
| A frame that differs over matching state | The presentation's own state (`[[present]]`); who drew the pixel; `rrc frame SESSION --tick N --screen` | seconds |
| A sound that differs | `rrc trace SESSION --tick N`: the events; `rrc ports` for who wrote the device; a driver's answer, a hook | seconds |
| A bug of the original's | `rrc suspects SESSION`; its code; `rrc quirk`; `keeps`; `rrc compare --fix NAME` | one run a session |
| What no session runs | `rrc unrun`; a session from a tick that reaches it, or a reading against the code | seconds |
| What a stretch no script reaches does | Ask the person for a play of it (`rrc play NAME --from SESSION --tick N`); read its census and `follow.log`; cut the items into short sessions | the person's minutes, then one follow |
| A difference a play found | `rrc compare NAME --transplant T --to T+N` at its first tick; then the table "When the port differs" | seconds |
| The port's input handling | The play's `live` by input; the original's handler; fix `Live`; `rrc compare NAME --live` | one run |
| A play's census after the port changed | `rrc play NAME --resume` (`--other-build` after a change to Clockwork) | a follow, no window |

## Stage 0: Set up

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

`rrc init` also writes what an agent meets first (D155): `AGENTS.md` (run `rrc rules` and `rrc status` before
anything else, which this machine's session hook does too), `notes/`, and `RULES.md`, the person's rules for the
port, as questions. Ask the person each once and write the answer in its place: the stretch of the game to port,
the sources to keep out, where to commit and as whom, what needs asking first, whether a second agent may be
started when stuck. `rrc rules` prints the file with the playbook's rules, and `rrc doctor` counts the questions
left. A port made before this gets the files from `rrc hooks`.

## Stage 1: Survey

Know the program before porting any of it, in this order: the analysis; the outside sources; a first session from
power-on into play; its loops and their sync points (every loop of the stretch, rule 11); its boundary values; a
recording; then the files it read (`rrc files`) and the outline of a play pass (`rrc pass`), whose top calls get
their names.

### The analysis

```bash
rrc analyze
```

Clockwork unpacks the program if it was packed, Ghidra analyses it (minutes), and the runtime library's
functions get their names from the signature packs (`sigs/README.md` lists them; `rrc sigs build` makes one
from a program built with the game's compiler and its linker map). `rrc show <ID or name>` then tells what is known about a
function: its name and evidence, its size, calls and callers, and with `--c` its decompiled code, in which
Ghidra's `FUN_` and `DAT_` names become the names you gave (`name[+N]` for a byte inside a data item), or with
`--asm` its disassembly by Clockwork (D86), following the jumps and listing each table's cases: lines and branch
targets are IDs, and each instruction's data, as Ghidra's references have it, is named after it
(`; tick_count`). For data, it lists the instructions that read it, write
it or take its address, by function (ten functions a kind; `--all` lists them all).
Decompiled code is a hint, not a source (D2): it is partly wrong. `rrc show` also says what else the toolkit holds
about the place, so that it is met without being asked for (D159): the outside sources that cover it, the port's
fields mapped to it, the port's code that names it, the quirks registered and the suspects found in it.

Ghidra misses code that only a vector or a pointer reaches: interrupt handlers (`rrc inputs` names them
by their entry), and in many games most of the per-frame logic, called through far pointers. Once a session
is recorded, `rrc analyze --session NAME` adds every function its run called to Ghidra's analysis, with calls,
references and decompiled code (D79; Ghidra opens its analysis again: a few minutes). It also gives Ghidra the
register contracts the run showed (D81): hand-written assembly's register parameters (`update_actor(word si)`
instead of `unaff_SI`) and results, a carry flag among them (`bVar = FUN_1000_1cee()` instead of a lost
flag). A function whose callers use several results returns a structure of them (D88): `r =
test_position(...); if (r.cf)`, `r.ax`, `r.zf`. A register only an untaken path reads isn't one, so a session that runs the code you read gives the
best contracts. One function at a
time: `rrc show ID --asm` disassembles it with Clockwork, following its jumps, and once named (`rrc name`)
`rrc analyze` adds it. Code the program loaded at run time (drivers) is past its file: read it with
`rrc show ID --asm --session NAME`.

An address inside a function shows that function: `rrc show ID --asm` lists it from the instruction the
address is in, and `--c` gives the function's code. Data can be named by its offset in the data segment, as
the code addresses it: `rrc show DS:7A46`, `rrc data DS:7A46 NAME` (D90; `rrc show` says a datum's `DS:`
address). A near call through a table of handlers (`call word ptr [si+8]`) reaches functions in its own code
segment (D89), so `rrc show`'s calls and callers list the handlers.

Some games make code at run time (drawing routines generated for each size, say): `rrc calls` then counts
functions past the program's file image, which Ghidra can't see. `rrc show ID` says so, and `rrc show ID
--asm --session NAME [--tick N]` disassembles it from the session's memory, by tick N or at its last
snapshot. Port what the generator makes, not the bytes (docs/design.md §6).

### The game's files

The game's files are a source too (D78):

```bash
rrc files first-level
rrc files first-level --file LEVEL1.MAP
```

`rrc files` says which files the original read and wrote, how much, and by which code (with the functions it
was called from). With `--file` it lists each read or write: where in the file, how many bytes, where in
memory (named), by which code, when. In an archive `knowledge/formats.toml` describes, each access is named by
its member (`+A9ECB9 (RAT1.CFA)`, D112), and `--file` takes a member's name too (`--file RAT1.CFA`). A structure in memory leads to the file and offset it came from, the
file to the function that reads it, and a save routine's writes to which memory each part of a save is.

### Outside sources

What others wrote is looked for before anything is worked out (D32, D77), and kept in the port's books
(`sources.toml`, D156), with what each says (its digest, D185), so that it is met again where the work is:

```bash
rrc sources add the-wiki --kind wiki --at https://example.org/wiki --covers spells,items --trust "names, not timings"
rrc sources exclude an-old-port --why "the person: its code stays out"
rrc sources sweep --searched "the game's name and file formats; its sound driver's name" --none extender,tools
rrc sources spells
```

Sweep for each kind. For the game: its wikis, documentation of its file formats, reimplementations and source
ports (of this game or others on its engine), modding tools, patch notes, walkthroughs and maps (they script
sessions), bug lists and speedrunners' glitch lists (each entry a candidate quirk and a session to write). For the
program's parts, as the analysis identified them (the library names `rrc analyze` gave, the strings in the image):
the compiler's runtime library, the DOS extender, the sound driver, the compression. These are often documented or
published, and what is published isn't reverse engineered. `rrc sources sweep` records what was searched for and
the kinds looked for and not found; `rrc status` names the kinds neither found nor looked for.

**Each source kept has a digest** (D185): `notes/sources/NAME.md` (`rrc sources add` starts it), what it says in your
own words, a heading a subject and a line a fact, with where in the source it is (a page's section, a file and its
line in a reimplementation's code). A name and an address are found by nobody; a digest's lines are. `rrc` searches
them word by word, a fact under `## Flats` found by `flat`, and prints the lines that match where a move is chosen:
at a comparison's difference, on `rrc show`, when an item is taken, when a route is stuck. Where none matches at a
difference, a card or a stuck route, it says `search`: look for it outside before reading on, and write what is
found in the digest. `rrc sources WORD`
searches them on demand. The survey's gate asks for a digest of each source kept, three lines of its own at least,
and `rrc journal` counts how the sources were used: looked up, shown, cited. A bug a source reports is a lead, written
in its digest as such (a candidate quirk), and isn't refused there as an unregistered one: it is met where its
subsystem is worked on, read in the code, and registered once the code and a run show it (the first rule).

A source is expected, and not relied upon: evidence, named with its source (`prior-art:<name>`), that the original
confirms or overrules. Reading a page is free; a download is asked of the person, once, as one list. Projects the
person excluded stay out, listed with `rrc sources exclude` so that they aren't found again (for Arena, Recreate
and tesa-decomp: D66, D69). For a test bench whose source is released, used to grade the toolkit, the source stays
out of reach (D46, below): such a port says that it reads none, `rrc sources none --why TEXT`, and is asked for no
sweep.

### A first session

A session is what to reproduce: an input script or recorded play, and how long it runs.

```bash
rrc session add first-level --seconds 51 --inputs first-level.inputs --description "..."
```

An input script presses keys, clicks and moves the mouse, and moves the joystick (`{stick X Y BUTTONS}`; the
session then needs `--clockwork "--joystick 1"`): `clockwork/README.md` lists its steps. A game that turns by
the mouse's motion turns with `{motion DX DY}` (mickeys), not `{move X Y}`, whose motion stops at the screen's
edge. To find a script, explore the game step by step from a snapshot instead of rerunning it
(`clockwork/README.md`, "Exploring a game"): the lines that worked are the script.

### Ticks

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

**Input that arrives during the pass (D60, D68, D93):** input arrives at any cycle, so between the sync point and
the pass's read of an input there should be as little as possible. `rrc record` checks every read: `late`
counts the ticks whose pass read an input after it had changed since the sync point, with the reads that did
it, most first (the input, the instruction that read it, or for a driver the instruction after its call; in how
many ticks; the first). Such a pass did what the next tick's values say, and a port fed only this tick's would
do something else (stay in a loop the press ended). The trace keeps what each such read saw, and `rrc trace
SESSION --tick N` lists a tick's. The wait that ends a tick isn't counted: reads all made in the call the next
hit comes in, the last at most 40 of the program's instructions before it, are the wait polling its timer
count. Where one read comes right after the sync point, move the sync point; where reads are spread through the
pass (a clock read again after drawing), the port reads the input where the original did, through
`tick.inputs.at_site("GAME.EXE+04BB9")`, and gets what that instruction read. Each call is one read there: the
port calls it as often as the original read there in the pass (a function called twice, each time round a
loop that polls the mouse until a click), and the n-th call gets what the original's n-th read saw (`rrc trace
SESSION --tick N` says which read, `its read 3 there`). A loop that polls checks
`tick.inputs.read_out("GAME.EXE+09017")` when what it waits for hasn't come: true once the port made as many
reads there as the original's last that saw a change, so the original's loop had ended by then and the port's
went another way; it stops there as at a path not ported, instead of polling forever (D94). Where the
program writes an input itself (it empties its keyboard buffer before a message box waits for a key), the
port makes the same write, `tick.inputs.write("key_queue", 106, &[0, 0])`, and the reads after it see it with
what arrived since (D97): a read is the tick's value, the port's writes, and the bytes that read saw otherwise.
The program's calls that change its mouse driver are such writes too, which `rrc_runtime::mouse` makes as the
driver does (D133): a position set or a range (INT 33h 04h, 07h, 08h: `MouseRanges`, which the port keeps in its
state for the ranges the device doesn't report), and the reads that clear what they read (0Bh, 05h, 06h:
`read_motion`, `read_presses`, `read_releases`). A live port's `Live::wrote` passes its ranges and the pass's
writes to the platform's driver (`MouseDriver::wrote`).
To move one:

- move the sync point to the read: onto the instruction that reads the input (the load from the key array,
  the `int 33h`, the `in al, dx` of port 201h), or just before it in the same loop; not at the top of a pass
  that reads something first, nor at the entry of a function the loop calls after reading something;
- if several loops call the function that reads the input, each loop gets a sync point at its own call: a
  sync point in the function ticks for every caller;
- if the pass reads an input in several places, put the sync point on the first read; a later read that sees
  a change stays `late` (a timer count read again mid-pass as a timestamp), and the port reads it there with
  `at_site`;
- a scripted input that lands in the gap can be moved by a few milliseconds (`MS:` in the script) while the
  sync point is being placed.

Record again until the reads left `late` are ones the sync point can't cover; the notes say which, and the
port reads each with `at_site`.

**What a part not ported hands the ported code in the pass (D96):** a part the port doesn't have yet may
compute something the ported code reads before the next sync point, where `--feed` can't give it (D84 feeds at
sync points): the renderer finds the object under a click while it draws, and calls the game's code with it.
Declare that memory an input the part writes, with the ported code's instructions that read it: `rrc input
GAME.EXE+4855E view_pick --size 2 --part "the 3D view" --read-at GAME.EXE+22DBB`. The program's writes to it
aren't followed, so each read there that saw something other than the read before is kept, and the port reads
it with `tick.inputs.at_site("GAME.EXE+22DBB")`, polling until `read_out` as a loop would (the part may hand
over nothing in a pass, or several times). Where the part calls ported code with its arguments in registers
(the renderer wakes each creature it draws through a function of the game's), declare the call a hook (D98):
`rrc hook GAME.EXE+25F11 view_wakes --part "the 3D view" --registers ax,bx,cl,di`. Recording keeps each run of
that entry in a pass with those registers, and the port makes the same calls where the part would have, at
its step's place for that part: `for call in tick.inputs.calls("view_wakes") { wake(call.get("ax"), …) }`.
The other way round, where the ported code calls a part not ported for a result (a lookup in data the port
doesn't load yet), put the hook where that call returns, with the registers it answers in, and the port takes
each result where it makes the call: `tick.inputs.next_call("level_texts")` (D99). A hook on an instruction of
the part that writes state the port has (the ray caster stepping a door in the map) gives its writes the same
way, with the registers that hold the place and the value: the port makes them at that part's place, and the
rest of that state stays the port's to compare, not fed (D100). A part an interrupt runs (a sound driver's
service, from a timer interrupt) is a hook with `--interrupts`: its runs count from interrupt handlers too, and
`tick.inputs.runs()` gives every hook's runs in the pass in the order they came, so the port makes the
interrupt's calls between the program's own calls into that part where they fell (D174).

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

### Record

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

lists each access in those ticks (or the whole session) with the instruction, the value and the call chain.
A watch keeps at most 100000 accesses: watch a few bytes over a few ticks, not a whole buffer over a session. The
host's own copies into watched memory (a DOS read, an EMS page mapped into its frame: 16 KB at a time) count byte by
byte; `--program` leaves them out and counts them, which is how the program's writes into such memory are found.
`SEG:OFF` is Ghidra's address; `MEM+LINEAR` the machine's outside the program. A far pointer read from a run's
memory is that run's address, written `@SEG:OFF` (or `@LINEAR`): `rrc show @23AC:03A1 --session NAME`, `rrc peek` and
`rrc watch` take it and name the ID, and `rrc show ID --session NAME` says where that run has an ID (D220). The run
loads the program elsewhere than Ghidra (whose segments count from 1000h): never translate a far pointer by hand.
A value is shown as the data's type reads it; an access to part of an element (`seed[+2]`, a word of a
doubleword) as the number the instruction wrote, in hex (`57E9h`: the bytes E9 57). `rrc frame` writes what the screen showed by the end of a tick as a PNG, to look at.
Name data as you learn it (`rrc data ID NAME --size N --evidence "..."`) and record again: the trace follows
every named data item. A recording keeps the data segment at every tick (D92), so recording again after naming
more of it takes seconds (`from the run recorded before`): only an item read through a pointer, a device, an
item in expanded memory (D217), a change to the sync points, inputs, the session or Clockwork runs it again (`--run` always does). With nothing to
add (a mapping or a type changed, the items not), it records nothing (`kept`, D111); a changed input, option
or length always runs again (D123). Recording a session
again with `--run` must give the same run (`same`); after a knowledge change
the trace differs and `rrc record` says the run is the same (`same run as before; the trace differs`). A run
that differs is a Clockwork bug, or the session, the game or Clockwork changed: say so in the notes. Commit the
session's file with the new hashes.

A session's first recording keeps the game's folder it ran on (`[folder]`, D170): its every run, the original's and
the port's, sees those files and no others, so playing the game outside the toolkit (its saves, the files it leaves
behind) changes no session. `rrc record` says when the folder differs from the recorded one; `--folder` takes it as
it is now, for a new version or a save the session needs. A session based on one whose run changed that way
starts at the tick's number again with `rrc record SESSION --rebase`.

`rrc record` also keeps a snapshot of the machine every 10 emulated seconds (on this machine, in the cache):
`rrc watch` and `rrc inputs` then start from the last one before the tick they look at, in seconds instead of
a run from power-on, with the whole call chains. Record a session before asking about its late ticks.
`rrc calls` and `rrc inputs` keep their answers: asked again with the same session and knowledge, they answer at
once (`kept`).

## Stage 2: Sessions

The scope is what the port is to reproduce, written down as sessions (`rrc scope SESSION...`, with `*` and `?`;
`rrc.toml`'s `[port] scope`, D156): the gates are measured over them. The person plays the scope once, for minutes:
scripts reach only what an agent can plan (D124). The agent adds a scripted session for each thing play missed,
from another session's tick, and a scenario's edits for a state play rarely reaches.

### The person plays

Scripts reach what an agent can plan; the person reaches the rest. `rrc play NAME --from SESSION --tick N` opens
Clockwork's window on the original from that tick (or power-on without `--from`): the person plays it, and the
play is recorded as the session `NAME`, followed as it goes with the port beside it (D192): what differs is said
as it is first found, and at the end the play's census and the port's input handling are listed. How it runs, what
it says and keeps, and how a play becomes work: "Playing alongside" below. A play is a session like any other;
commit it with the port (rule 16), and cut the stretches the work comes back to into short sessions of their own.

### A session from another's tick

A scenario deep in the game needn't be played to from power-on each time: a session can start from another's
tick (D118), with its own input from there:

```bash
rrc session add boss --from walk --tick 1200 --seconds 140 --inputs boss.inputs
rrc record boss
rrc compare boss --transplant 1
```

Its tick 1 is `walk`'s tick 1200; its input's times and `--seconds` count from power-on (`rrc session add` says
when that tick is), and a key held there is let go when `walk` let go of it. The port starts from a transplant
at its first tick, or a paired checkpoint; file tracing sees only what it reads from there.

### A scenario by edits

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

## Stage 3: Skeleton

A port that compares before it does anything. Copy `examples/toy`, find the core state from the loop down (who
writes what), map it, and compare the empty port in lockstep over every session of the scope (`rrc work --run`):
everything differs, and those censuses are the work list's first state. `rrc progress` names the largest changing
memory nothing maps: map it, or ignore it with a reason.

Find the state first. What the player changes is found by how it behaves over the session's ticks (D75):

```bash
rrc trace first-level --item key_down
rrc search first-level changed:78-88 same:90-93 same:65-75 --type u16
rrc search first-level less:64-76 more:123-130 same:78-88 --type u16
```

`rrc trace --item` gives the ticks each key is held; a position changes while walking and stays put while
turning or standing, an angle goes one way turning left and the other turning right. Each condition narrows
the places; watch the few left (`rrc watch`) to see the code that keeps them, and name them. The first search
reads every tick of its span and keeps it (D76): searches within those ticks answer at once, so ask about a
wide span first.

Then map it: `mapping.toml` names each field of the port's state and the original's item it is
(`docs/formats.md`); record again, so the trace follows every mapped item. Then name the port's program in
`rrc.toml` (`[port] command`). A Rust port builds on `runtime/` (the crate `rrc-runtime`): its state is one
struct serde serializes, its fields named as the mapping says; it implements `Simulation` (`start`: the state at
the first tick; `step`: one pass with the tick's inputs) and its `main` calls `rrc_runtime::main`. `examples/toy`
is a whole port repository to copy from: the knowledge, a session, the mapping and the port of a toy game.

The original's pointers, heap and flag bytes map too (D72): declare a pointer type in `types.toml` with `into`
the array it points into, and the port's field is an index (or none); map a heap block `via` the pointer that
finds it, and the port's field is its contents; declare a flag byte as `[[bits]]`, and the port's field is a
structure of named numbers and booleans. A flags word that holds bits of a part not ported (the renderer's
scratch next to the game's flags) maps with a `mask` of the bits that are the port's (D95), once the code that
reads the others is found to be that part's alone; a part the port's code reads is fed instead (`--feed`).

## Stage 4: Logic

The work loop on the mapped fields, with `--fields` so that no screen stops it (D136).

```bash
rrc compare first-level
rrc compare first-level --transplant 500
rrc compare first-level --pair 900
rrc compare first-level --resume 900
```

`rrc compare` says `match`, or where the port first differs: the tick, each field that differs with its first value
that does (both values) and how many of its values do (D140), and who wrote
the original's value between the previous tick's sync point and this one's (the instruction and its call chain:
read that code; a value copied there, by `MOVS` say, is followed to where its source was written, D85); `port` names the port's code for the function that wrote it (D73), found by the ID each port
function names in a comment (`/// update (GAME.EXE+00114): one pass`, docs/design.md §6.6). Fix the port and compare
again. A divergence at the port's first tick means its start state
isn't the original's, or a transplant misses a field (map it). From power-on, a buffer the program hasn't
filled yet holds leftovers (a heap block's links): leave the port's field `None` until the port sets it.
`rrc compare` takes it as matching while the original's value is unchanged, and says `unset` with who last
wrote it before the first tick: the C library's allocator means leftovers; the game's own code (a loader)
means `start` should set it (D102). Work on a subsystem from a transplant at a tick
where it is in use, so what comes before needn't be ported first. Where the port matches through a tick, `--pair
N` keeps a paired checkpoint, and `--resume N` compares from it without the ticks before. A subsystem not
ported yet would stop the comparison at the next tick: `--feed random,actors` gives the port the original's
values of those fields (or groups) before every step, so the subsystem you port is checked over the whole
session (D84); remove each from `--feed` once it is ported. For each field fed, `rrc compare` says how often the
port's own value differed from the original's (`fed       hit_effects: the port's own differed at 3 of 650 ticks
fed, first at tick 605 (hit_effects[16].look)`, D114): a feed doing work hides a difference, maybe a wrong
mapping; one that never does can go. Mark each path the port leaves out with `rrc_runtime::note("what
(GAME.EXE+01234)")` (D115): where the comparison diverges it lists the notes made in that pass (`noted`), the
paths not ported it took, and `--notes` lists every note of the run with the first tick it was made in.
`unmapped` lists the
named data the mapping neither maps nor ignores: the state nothing compares yet.

`rrc compare SESSION --lockstep` compares the whole session at once (D124): the port gets the original's mapped
state before every pass, so each pass starts where the original's did, and every field its own pass leaves
otherwise is counted with the first tick and what it was (`differs   player.position: in 12 of 774 passes, first
at dungeon's tick 197 (player.position.x: original 2448, port 2432)`), with the fields it leaves unset, the frames
and the sound that differ, and its notes; the census is in `.rrc/compare/SESSION/census.json`. A session that starts
at power-on is counted from there, the port started as it starts itself (its initial data, D135), not from a
transplant at its first tick, which would leave out what its power-on sets up and nothing maps (D164). At a
scenario's edit, the port's own values stay where the edit writes until its tick is drawn, as the pass left them
(D166). A run whose port's code changed while it ran is refused (D167): while a long census runs, edit a copy of
the port. It is the work
list: take a difference, then `rrc compare SESSION --transplant T` with T the tick before its first, for who wrote
the original's value.

`rrc compare SESSION --live` checks the port's own input handling (its `Live`, what the platform plays it with)
beside lockstep (D194): at each of the original's sync points, `Live` is given the session's keys and mouse as the
original's run had them, at the play's time, asked at the platform's frames and at the original's moment (D197), and
what it makes is judged against what the original's pass read: the same, timing (a pass early or late, its handlers
making the original's at the original's moment, or its loop still waiting a moment where the original's took a
pass) or a difference, by input with its first tick (`.rrc/compare/SESSION/live.json`). Timing is a moment's phase;
a difference is the port's handlers made otherwise than the original's: read the original's handler and fix `Live`.
`rrc play` checks it beside the play the same way (`.rrc/play/NAME/live.json`).

`rrc compare SESSION --corrected` runs the port free, on its own state, and sets the original's values over only
the fields a tick left otherwise, before the next tick's step; after a pass the port noted, every field (D191). Each
difference is counted where it arises, as in lockstep, and the census (`.rrc/compare/SESSION/corrected.json`) is
lockstep's where the mapping carries the state both ways exactly; where it isn't, the difference is what the mapping
can't carry (a pointer read as an index, state the port keeps and nothing maps).

`rrc progress` says how far the port is, as the sessions show it (D117): of the program's functions the
original ran in the sessions, those it ran in passes the port matches (as each session's last `rrc compare`
left it), and how many of those the port's code names by ID (a bare `+0A2F0` names the program's own); then
which ran in matching passes without code that names them, the most called first (a helper ported inside its
caller, library code, a part fed); and, of the data segment the recording keeps at every tick, the bytes that
change, how many the mapping maps or ignores, and the largest places that neither does, by name: the state to
map next.

## Stage 5: Data and start-up

The work loop on what hooks, parts' inputs and feeds still give the port, then on the start-up's pass and the menus
before play: the port loads the game's files itself and matches from power-on.

Tables the original's code reads (a sine table, combat tables) are loaded from the person's copy, not copied
into the port (D82): `rrc_runtime::original::program().i16s(0x46A3C, 640)` reads them at their image offset,
under `rrc compare` and in the port's own `cargo test` alike (D91). So are the game's files:
`rrc_runtime::original::file("DATA\\LEVEL1.MAP")` reads one by the name the program opens it with, and
`rrc_runtime::codecs` decodes the common compressions (PCX's run-length code, LZSS with lzss.c's parameters or
others); a game's own is the port's code (D101). A port from power-on loads in `start` what the original loads
before its first sync point hit: `examples/vault` is one. `start` is also the state a transplant's mapped fields
go over (its unmapped fields come from it), so what only power-on has (the program in its title or menu) belongs
in `power_on`, which runs from power-on alone. What the start-up makes of input it reads before then
(a generator seeded from the BIOS clock) is the port's `Simulation::power_on` (D134): declare the input where
the start-up reads it (`rrc input`, its `read_at`), record, and `power_on` gets the start-up's pass, tick 0,
whose `tick.inputs.at_site(ID)` is what that read saw; a live port's `Live::power_on` gives it from the host.
A pass that arrives at a gated wait asks `Gates::arrives` (D143) whether the tick is there: not where the next
tick is at another sync point (`Tick::next`); at the next tick's, the gate, or the original having gone round no
more (`Tick::spun`).
The program's command line is such an input: DOS's command tail in the program's PSP (its offset 80h: a length,
then the text), where the C start-up reads it; the session's options give it (`clockwork run --args`).
A port whose `Simulation::INITIAL_DATA` is true starts from the program's own initial data instead (D135): each
mapped field at a fixed place in the program's image as the image holds it, and the carried memory there (D137),
set over `start`'s state, so `start` and `power_on` make only what the start-up's code does.

Once the reader's code says how a kind of file is laid out and coded (and format documentation, as a source,
what it is), write it in `knowledge/formats.toml` (`docs/formats.md`): an archive's directory, tagged chunks,
a picture with its palette and its frames, members one after another, a map; its header a structure of
`types.toml`; its coding one of the toolkit's codecs (PCX run-length, counted runs, LZSS with its parameters,
LZHUF), pixels packed in bits through a table of colours, frames patched from the one before.

```bash
rrc asset LEVELS.DAT
rrc asset LEVELS.DAT/LEVEL1 --session first-level
rrc asset TITLE.IMG
rrc asset GLOBAL.BSA/RAT1.CFA/9 --session cell --tick 488
```

`rrc asset` decodes a file or an archive's member by its format: an archive's members, a picture as PNG with
its palette, a sound as WAV, music as MIDI (in `.rrc/assets/`), the bytes in the cache. With `--session`, the original checks it (D103):
where the session read those bytes and by which code, and whether the original's memory holds what this
decoded at moments after the read (1 ms, then doubling to about 4 s), `equal` and when and where (a map laid
out in a wider grid is found as its rows), or how far it agrees (D104). What the original decodes when it draws
(an animation's frame) is searched for at a tick it is drawn in, `--tick N`, and found as decoded or as a step
of its decoding left it (a frame's runs before its bits are unpacked, D116). A format that decodes equal is the one
the port's loader decodes by (`rrc_runtime::codecs`). A game's own coding, which no codec of the toolkit is,
is the port's code, checked by the comparison as the loaded data it makes (mapped state).

## Stage 6: Presentation

The work loop on frames, the most ticks first, then on sound, a device at a time: the digitized sounds, then the
music, which is a subsystem at its interrupt's rate (D22).

Draw a frame from what the scene says its pass drew from, never from a copy the presentation kept of the frame before:
a scenario's edit is set at the sync point after its tick is drawn (D165), so the next pass's place, or a table the
logic builds for the drawing (Arena's floor and ceiling rows, built only when its logic asks), is the step's to record
as the pass begins.

Once the logic matches, the port draws and sounds (M6, D107, D108). Its simulation never draws: it shows a scene, what there is
to draw at a tick as meaning (the map, the player, the picture shown: `rrc_runtime::present::Shows`), and a
presentation draws it (`Presentation`: `start`, the screen at the first tick; `draw`, what a pass drew onto
the screen before), keeping its own state as the original's drawing code keeps its own (what it drew last).
`main` becomes `rrc_runtime::main_drawn::<Game, Screen>()`; `examples/vault` is one.

```bash
rrc compare first-level
rrc frame first-level --tick 102 --screen
```

`rrc compare` then compares each tick's frame with what the original's screen held at the tick's sync point
(its memory and palette), pixel by pixel and colour by colour, once the state matches there. At the first
frame that differs it says how many pixels and where, the first one with both indices, the areas they make
when there are several (D113: each one's box, size and first pixel; one area of a few rows is an edge off by
a row, thousands of single pixels a view shifted or lit differently), the colours that differ, writes a picture (the original's screen, the port's frame, the differing pixels in red), and in mode
13h who drew the first pixel in the pass to it (through copies: a view drawn into a buffer and copied to the
screen is found where it was drawn), with the port's code for that function, as for a value
(`draw_cell (GAME.EXE+004DD) is src/main.rs:251`). `rrc frame --screen` writes the original's screen at a
tick.

From a transplant or a resume the presentation gets the original's screen then, but not what its drawing code
keeps that the screen doesn't show: what it drew last, an off-screen buffer, the frame an animation is at.
Name those items and map them in `mapping.toml`'s `[[present]]` (D110), and `rrc compare --transplant T`
reads them at tick T and gives them to `Presentation::given` before `start`; never guess them (a counter from 0,
the screen for the buffer): the frames would differ later, or hide that they would.

Sound is a presentation's too (`Presentation::sound`, `SOUNDS = true`; `DRAWS = false` for one that doesn't
draw yet): what the pass before a tick made, by device, as the original's would be logged without times (the
OPL's register writes, the speaker's states, digitized sounds started, MIDI bytes; the start-up's from
power-on). `rrc trace SESSION --tick N` shows the original's (`opl: A0=59 B0=35`, `speaker: tone 2281/1141`),
and `rrc compare` compares the port's with them in order, device by device; at the first that differs it
names the device, the event, and who wrote the device's ports in the pass (`examples/chime`). A presentation
that makes some devices' sound and not yet the others' (the digitized sounds before the music) names them,
`SOUND_DEVICES = &["pcm"]`, and only those are compared; the comparison says which aren't (D121). Where the
game asks a sound driver it doesn't port (whether a sample still plays), a hook gives the port the driver's
answers in a comparison (D99), and live, `Host::pcm_playing` says whether the platform still plays the last
sample, which `Live::next` answers with (`Inputs::with_calls`).

## Stage 7: Play

The play-test has two sides: the person plays the original with the port followed beside it from power-on
(`rrc play`, "Playing alongside" below), which says where the port's logic, frames, sound and input handling differ
from the original's as the person plays; and the person plays the port in its own window (`PORT play`, below),
which only a person can judge. What either finds becomes a session or an item of the work list.

A port that draws and sounds plays in a window (D119): `rrc_platform` owns the loop, shows the presentation's
frames, gives the host's keyboard as the PC's (scan codes, set 1) and the mouse in the screen's coordinates,
and plays the sound (the OPL3 by Nuked OPL3, the speaker, digitized sound). What a comparison gives the port
from the original's memory, the boundary values, a live port makes from those devices as the original's
handlers did: implement `rrc_platform::Live` (`next`: from the host's devices, the original's keyboard and timer
handlers' work, and whether its loop takes a pass now, still waits, or ends), and make `main`

```rust
fn main() -> ExitCode {
    rrc_platform::main::<Game, Screen>("Game", Some(Game::start)).unwrap_or_else(rrc_runtime::main_drawn::<Game, Screen>)
}
```

so `PORT play [--game DIR] [--start DIR] [--no-sound | --sound-buffer FRAMES] [--sb-filter MODEL] [--fix QUIRK]...
[--controls FILE]` plays (a larger sound buffer if the sound breaks up on a host, D168; the Sound Blaster's output
filter, the SB Pro's by default, D169) and anything else runs a
comparison as before. Declare the game's actions in `Live::ACTIONS` (D162: each the key or button the original takes
for it, with what it does, as the analysis found its input code), and read keys in the simulation through them
(`Action::held`) where the original reads a key-state array: a person binds them to other keys with a controls
file (`PORT play --actions` lists them), and each still reaches the port as the original's own key. A
port that doesn't reach a stretch from power-on yet (the menus and the character's creation before a dungeon)
plays from where a session's original is (D120): `rrc start SESSION --tick N` writes the mapped fields' values,
the presentation's own state and the screen there into `.rrc/start/SESSION-N`, made once from the person's copy,
and `PORT play --start .rrc/start/SESSION-N` plays from them without the original running. A port that
matches from power-on plays the whole game from tick 0's folder (D141): `rrc start SESSION --tick 0` writes the
program's initial data and the inputs the session's start-up had (its command line; no recording is needed for
them, D225), and the port's
`Live::power_on` gives the start-up those with what the host has instead (its clock). A program that draws its own
mouse pointer sets `Live::OWN_POINTER`, and the host's cursor is hidden over the window (D142); `Live::pointer` says
where its driver's pointer is, and the host's cursor is kept there, so the program's ranges hold as on a PC (D145). Its `Live::wrote`
takes what a pass wrote into its own inputs (a keyboard queue the program empties) into the handlers' state. A game the port
saves goes through `rrc_runtime::original::save` and `saved` (the port's saves, `.rrc/saves` of its repository
or `RRC_SAVES`; the person's in the game's folder are read, never written). `examples/vault` and
`examples/chime` play so (`cargo run -p vault-port -- play --game testbins/bin`). **A port is played on its own** (D226, the person's): the
program alone is what the person gets, and it reads their copy of the game and nothing else. `rrc standalone` writes
the port's book (`standalone.json`: where each field's initial value is in the program, the inputs there, the
program's versions and arguments), the port builds it in (`Live::STANDALONE`, `include_str!`) and its `power_on` gives
from the host what the image doesn't hold (a mouse driver's reset, the book's command line); started with nothing,
it finds the game, unpacks its program and plays from power-on. Write the book again when the mapping changes (`rrc
check` refuses it behind), and give the person the program alone.

## Playing alongside: the live comparison

The person plays the original; the port runs beside it, and what it does otherwise is said as the play goes (M6.5,
D184; `docs/plans/live-comparison.md` has the design and what each step found). It is the toolkit's way to the
stretches no script reaches and the check of the port's own input handling, which nothing else runs. It doesn't
replace the work loop: a play finds; a difference is then worked from the play's session like any other.

**What runs.** `rrc play NAME` builds the port first (no build slows the window), then opens Clockwork's window on the
original and records the play into `sessions/NAME.session` as it is played: the input at its cycles, a mark every
100 ms ("all is in to here") and the machine's state hash every 30 s. A second Clockwork, the follower, runs the
same run behind the marks and checks each hash as it reaches it (D189): its run is proved the window's, not assumed,
and a hash that differs stops the follow as a Clockwork bug (D9). The follower records the run as `rrc record` would,
tick by tick; the port runs beside it, fed the play's input a tick at a time, free on its own state and corrected
where a tick differs (`rrc compare --corrected`, D191), so each difference is counted where it arises without the
cascade a free run has after it; and the port's own input handling (`Live`) is given the play's keys and mouse at
the play's moments and judged against what the original's handlers made (D194, D197). Nothing waits on the window:
when the checking falls behind, it says by how much and catches up where it can.

**Starting one.** The person plays; the agent prepares and reads. From power-on, or from a session's tick:

```bash
rrc play first-run
rrc play dungeon-2 --from cell --tick 1200
```

- `--ips N` plays on a faster emulated PC (D203; 8 million by default): the game draws more frames a second, and
  the follower has more to do. At 8 million it keeps up; at 16 million it falls behind in the heaviest stretches
  (D211: on Arena, up to about two minutes early in a fifteen-minute play, held from there): what differs is then
  said later than it happens, never lost.
- `--sound-buffer 4096` (or more) where the window's sound breaks up (D199).
- `--replay FILE` follows a play recorded before with `clockwork play --record`, at its own speed.
- `--resume` follows a play again from its file, to its end: one cut short (the window, rrc or the machine stopped),
  or one to check again once the port's code changed. With `--other-build` it follows a play that another build
  of Clockwork played (after a change to Clockwork), its run proved the same by the play's hashes (D212).

**What it says.** In the terminal, as it is first found, with the play's time: `new at 3:51: differs ...`, `new at
12:40: note ...` (what the port noted, D115); `many` for a burst of more than five at once, each kept in the log;
`lost` once where the port's loop takes its passes at another sync point than the original's (another screen, or
waiting where the original took a pass), what differs then counted but not said, until `back` (D207); `port started
again at tick N` where the port panicked or hung (counted as `panic`, D193; ten times at most, then the port is
compared no more). A status line, rewritten each second, says how far the comparison is behind the play. Once the
window is closed the follow goes on to the play's end: leave its terminal open until the census. At the end: the
play's census, most passes first, with `next` (where to look first, as `rrc compare` gives it), and `live`: the
port's input handling judged against the play's ticks.

**What it keeps** (`docs/formats.md`): the play's session (`sessions/NAME.toml` and `.session`, small: commit them
with the port); `.rrc/play/NAME/census.json`, the corrected run's census with its `panics` and `lost` stretches;
`live.json`, the input handling's judgement; `follow.log`, each thing said with how late it was said (`new`; `counted`
while lost; `lost`, `back`) and every ten seconds a `pace` line: the play's time and the comparison's, and where the
time went (the comparer waiting for the follower, reading, the port; the follower making ticks or held). A play keeps
little on disk (D196): the pictures between its ticks by their hashes alone, a snapshot a minute; `rrc record NAME
--run` makes the pictures again where a comparison wants them.

**Reading a play's census.** The same census as lockstep's (`rrc compare --lockstep`), counted on a free run kept
on the original's track: each field the port's pass left otherwise, with its first tick and both values; the fields
it left unset; the frames and the sound that differ; the notes. The most passes first is the order of the work, as
in `rrc work`. A `lost` stretch is one item, not its many differences: what the port's loop does where the
original's went on (its first tick, the two sync points). A `panic` is the port's own failure: `rrc compare NAME
--transplant T` at its tick.

**Reading the input handling (`live`).** For each input, the ticks the port's `Live` made as the original's handlers
did (`same`), a moment apart (`timing`: a key or a move a pass early or late, its handlers making the original's at
the original's moment; or its loop still waiting where the original's took a pass, `waits`), and otherwise
(`differs`: its handlers made something else, with the first tick and byte); and the passes its loop would take from
another sync point (`syncs`) or not take at all (`skips`, `ends`). Timing is a moment's phase and is expected; a
difference is a fix to `Live`, read from the original's handler (the keyboard's, the timer's, the mouse driver's
answers). `rrc compare NAME --live` checks it again on the play's session.

**From a play to the work.** A play is a session: compare it, watch it, peek into it, trace it, as any other. Its
census items are worked by the loop: take the item, then compare from a transplant just before its first tick, kept
short with `--to` (a play is thousands of ticks: rule 17):

```bash
rrc compare first-run --transplant 2432 --to 2460
rrc watch first-run player_health --tick 2420 --ticks 20
rrc session add sheet-2 --from first-run --tick 1660 --seconds 60 --inputs sheet-2.inputs
```

A stretch the work comes back to is cut into a session of its own from the play's tick (`rrc session add --from`,
with a script for what follows): short sessions are what the scope and `rrc work --run` should count. A long play in
the scope makes every `rrc work --run` count its thousands of ticks again: put it there only while its census is
the work list, and take it out once its items are short sessions.

**Following again.** A play's census counts for the port's code that made it. After the port's code changes, follow
the play again (`rrc play NAME --resume`) rather than reading the old census as current; after a change to
Clockwork, with `--other-build`. A follow of a play already made needs no window: the person needn't be there.

**How it fits the workflow.** Stage 2: the person's plays are the scope's first sessions, from power-on and from
the sessions' ticks, minutes each, and the agent's scripted sessions fill what they missed. Stages 4 to 6: a play's
census is a work list beside lockstep's; its items are worked from transplants and short sessions cut from it.
Stage 7: the play-test is a play followed from power-on to as far as the person goes, and the port played in its own
window (`PORT play`); what either finds becomes a session or an item, and the person closes the gate.

## Giving the person the port

What the person gets is two packages, each a release of the port's repository (private like it), made by a script in
the port (Arena's `tools/package-windows.sh`, its `notes/releases.md`):

- **The port to play: the program alone** (D226, the person's). Built for the person's system with its book in it
  (`rrc standalone`, `Live::STANDALONE`), with a README and the licences of what it links (Rrecreate's MIT, and any
  other crate's, such as Nuked OPL3's LGPL). Nothing beside it: started, it finds the game, checks its version,
  unpacks its program and plays from power-on, and keeps its settings and saved games beside itself.
- **The live comparison** (M6.5, "Playing alongside"): `rrc`, `clockwork` and the port built, the port's books (the
  committed files, by `git archive`) with `rrc.toml`'s command pointing at the built port, shortcuts that find the
  game and run `rrc play`, and Clockwork's GPL beside the rest. Neither Rust nor Ghidra is needed (D224). A play made
  with it is evidence: the person commits its session to the port's repository.

Before one: the port's code committed (the script refuses otherwise), the book current (`rrc check`), every session
counted at that commit (`rrc work --run`), Rrecreate's `main` pushed. Never in either: the game's files or anything
made from them (D33, D82): no cache, no start folder, no unpacked program. Check what a package holds and, for a
program built for another system, what libraries it needs; play the program alone once where the machine allows (on
Linux, under a virtual display). A release is published by a tag a workflow builds from; in a cloud session the agent
can't push a tag, so the person pushes it, and the agent gives the person the built files in the meantime.

## Quirks

The original's bugs stay in the faithful port, each as a registered quirk (docs/design.md §6.5, D74):

```bash
rrc suspects first-level
rrc quirk enemy_overrun --at GAME.EXE+002FF --what "the sixth enemy reads the word after the table" --fix "reads 0"
rrc quirk
```

`rrc suspects` runs the session as its recording did, its edits at their ticks, and refuses if it ends otherwise (D152). It
lists the session's candidates: instructions that reach past named data's ends (`past`; a
`sweep` is a clear or a copy over many, not an index) and that read memory nothing wrote (`uninit`). Read each
one's code; a divergence where the port did what the code clearly means and the original didn't is another
source. Register the bug, and branch on it where the port does that code: `if rrc_runtime::keeps("enemy_overrun")
{ the original's way } else { the fix }`. `rrc quirk` lists each quirk with the port's code that names it.
Comparisons keep every quirk. A quirk that depends on the original's memory layout (a read past a table
returns its neighbour) reproduces that neighbour's data deliberately.

Every quirk has both sides (D160). `--fix` says what its switch does, and is asked for when the bug is registered.
`rrc compare SESSION --fix NAME` then shows the switch: where the port matches the original with the bug kept and
differs with it fixed, it keeps the session, the tick and what differed with the quirk (`shown`), and `rrc quirk`
lists it. A quirk no session has shown is one of stage 4's lacks: it wants a session that reaches it. A switch in
what no comparison runs (the port's own keyboard or timer handlers, which only play uses) is shown by a test of the
port's own that runs it both ways, and `rrc quirk NAME --shown "test NAME"` says which.

A comparison only shows what a session reaches and what's mapped, so a bug fixed by accident elsewhere shows
nowhere: this is why they're registered as they're found. `rrc check` refuses (and `rrc status` names, D148) a comment
in the port's code that says `bug` or `quirk` with no registered quirk named within two lines (a bug of the port's own
says `not the original's`, one of the original's that touches nothing the game does `changes nothing`), a `keeps`
not registered, a
quirk nothing branches on, a quirk with no fix said, a session's run that `rrc suspects` hasn't looked through, and a
suspect not judged (`rrc suspects --judge ID --quirk NAME`, `--not-a-bug WHY`, or `--changes-nothing WHY` for a bug
that touches nothing the game does, kept in `suspects.toml`). The notes and the knowledge are read the same way
(D160): a line of `notes/` or `knowledge/` that says `bug`, `quirk`, `glitch` or `buggy` names a registered quirk
within two lines, or says `not a bug` or `changes nothing`, or is about the register itself (`rrc
quirk`, `quirks.toml`); a bug written down there and nowhere else is a bug the port can lose. `rrc hooks` writes a commit hook that
refuses a commit while `rrc check` fails (`rrc init` does too); don't skip it. `rrc rules` prints the rules as they're
written, with the port's own from `RULES.md`, for the start of a session and after a summary.

`rrc unrun` (D151) lists the places in the original's code that the port's code names and no session's run executed, as
`rrc suspects` saw them: a comparison says nothing of them, so each wants a reading against the original's code, or a
session that reaches it.

A quirk's switch is a player's and a test's (D149): `PORT play --fix NAME` fixes it for that game, its name checked against
the port's `quirks.toml` (`Live::QUIRKS`, its `include_str!`); `rrc compare SESSION --fix NAME` runs the port with
it fixed, so a difference shows what the fix changes and proves the branch is reached. Try each on a session that reaches
it, since nothing else runs a fix's code; it says nothing of the port's progress, and isn't kept for `rrc progress`.

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

## Working in a cloud session

Where the person's machine has no room for the work beside their own use (Arena's: 8 GB, in use while the agent
works), the work runs in a Claude Code cloud session (claude.ai/code: a Linux machine of about 16 GB that starts
empty), and the person's machine keeps what needs a window (D221). Arena's port has the whole setup:
`tools/cloud-setup.sh` and `notes/cloud.md`.

**Once for a port**, with the person (publishing and their files are their calls):

1. The port's repository on GitHub, private.
2. The game's files: the person's own copy, zipped, as a release asset of a private repository of theirs (Arena's:
   `arena-files`), never a commit. Another release of the game only if `rrc game` finds it the port's version.
3. `tools/cloud-setup.sh` in the port: the libraries the platform links (as Rrecreate's CI installs them), Rrecreate
   cloned beside the port and `rrc` and `clockwork` built in release, the game fetched through GitHub's REST API
   (`gh api repos/OWNER/REPO/releases/...`: the cloud's GitHub proxy refuses GraphQL, which `gh release download`
   uses) and checked by its SHA-256, Ghidra from its GitHub release, `rrc.local.toml`, and `rrc game` to say the
   version. It skips what is there already.
4. `notes/cloud.md` (what a session needs and what it costs), a line in `AGENTS.md` (in a cloud session,
   `CLAUDE_CODE_REMOTE` is `true`: run the setup first), and the person's rules that a machine's memory held put
   into `RULES.md` (where to push, as whom, what not to format).

**A session:** the person starts it with the port's repository, the game's files' repository and Rrecreate
attached; an agent can't attach one. The agent runs the setup, `rrc analyze` and `rrc work --run` (it records every
session first: an hour or two on an empty machine), then works the loop as anywhere, toolkit fixes included
(pushed to Rrecreate's `main` with the checks of what they touch).

- **Keep one session for a stretch of work:** its machine keeps its files between pauses; a new one starts empty.
- **No session talks to another:** the repository is the handover. Commit and push before stopping; `rrc status`,
  the card and the journal (`work/`, committed: D222) say where the work stands, and `notes/handoff.md` what the
  next session needs.
- **One writer:** while a cloud session works on the port, no other session does; each pulls before it starts.
- **The person's plays stay on their machine** (`rrc play` needs a window): a play is committed and pushed with its
  session, and the cloud session counts and compares it as any other.

## An honest test bench

A port of a game whose source is released, made to test the toolkit with the source as its grade (Wolf3D's,
D46), proves the approach only if nothing from that source helps. For such a port:

- don't read the game's source release, reimplementations, wikis or anything derived from them, nor the web
  about the game's code;
- keep the game's copy in a folder of its own, away from projects that hold derived knowledge;
- don't read Rrecreate's git history: its commit messages carry names from the project before it;
- set `[port] imports = false` in `rrc.toml`, so `rrc sigs name` refuses, and say that no outside source is read
  (`rrc sources none --why "its source grades the toolkit"`);
- give it a cache of its own (`cache` in `rrc.local.toml`): the cache is shared by the ports on a machine,
  and `rrc analyze` reuses an analysis of the same program, with the functions other ports' knowledge added
  to Ghidra's;
- name things after what the code does, with evidence from this toolkit only;
- report `rrc journal` with the result: how the commands divided between comparing and each kind of looking, how
  often a comparison was stuck, and what each item took. A change to the playbook is judged by these, not by an
  impression (D157).
