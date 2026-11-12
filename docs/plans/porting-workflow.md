# The standard porting workflow: the plan

> Proposed 2026-11-11 and accepted by the person the same day (D154). The workflow itself is the playbook's now
> (`docs/porting.md`, "The workflow"); this page stays as the plan of the toolkit changes of sections 4 and 5, with
> what is built listed in "Built so far". Where the playbook and this page differ, the playbook is right.

## Built so far

| Change | Decision |
|---|---|
| A: the playbook around the stages, with the six rules, the table, fresh eyes (M) and the recipes (N) | D154 |
| B: `rrc init` (and `rrc hooks`) makes the port's entry point: `RULES.md` as questions, `AGENTS.md`, `notes/`, the session hook | D155 |
| C: the scope, `[port] scope` in `rrc.toml`, set with `rrc scope` | D156 |
| K: outside sources in the port's books: `sources.toml`, `rrc sources`, the sweep's gate in `rrc status`, a source named by `rrc show` | D156 |
| H: the journal and the stuck signal (`stuck`, `untried`); O's measure, `rrc journal` | D157 |
| D: `rrc status` names the stage, what its gate lacks and the next step, worked out from the repository | D158 |
| E: `rrc work`, the work list: the scope's censuses merged and ranked; `--run` counts the stale ones again | D158 |
| L: the card, `rrc work take` with the question, two routes and a budget; `rrc work done` | D158 |
| F: `rrc status --all`, what a handoff would say | D158 |
| G: a flow test walks the toy port through the stages (`the_workflow_s_stages_are_walked`) | D158 |
| I: a comparison's difference ends with `next`, the moves for it, of more than one kind | D159 |
| J: `rrc show` says what else is held about a place: sources, mapped fields, the port's code, quirks, suspects | D159 |
| N: `rrc how WORD`, the recipes by a word, with the port's own (`notes/recipes.md`) | D159 |
| O: an honest test bench reports `rrc journal` with its result | D157, D159 |

## Tried on the Arena port (2026-11-12)

Read-only, but for this machine's state of the port (`.rrc/`: its censuses counted again, a journal).

| What was run | What it said | What it changed in the toolkit |
|---|---|---|
| `rrc status` | In 1.1 s: stage 1, the outside sources aren't swept; two things `rrc check` refuses | The exemption from the sweep was read off `imports = false`, which this port sets while reading wikis: now said outright (`rrc sources none`) |
| `rrc status --all` | 3: eight named data neither mapped nor ignored. 4: fields in two censuses, 27 sessions taking paths noted as not ported, `cell` stopping at the sound of tick 95. 5: `cell` from power-on. 6: fourteen censuses with frames or sound that differ | It printed a line a session (58): sessions that stand the same are one line now. Comparisons and censuses kept before this weren't counted: they count, as stale |
| `rrc work --run` | 58 sessions counted in 14.5 minutes; 121 items; the top, `movement.buttons`, in 46 passes of two sessions | The stale line stayed, since only the censuses were made again: `--run` compares again too |

Left for the port's own next session: `rrc hooks` (its entry files, which stage 0 now asks for), its sources into
`sources.toml`, its two suspects judged, and its scope, which is the person's.

Not built as proposed: J's "the file it was loaded from" and "the sessions and ticks that run it" (each needs a run;
`rrc files` and `rrc calls` answer them); E's "code no session runs" in the list (`rrc unrun` lists it); the gate
of stage 7, which `rrc` can't test. K's exemption was first read off `imports = false`; the Arena port sets that
and reads outside sources, so a port that reads none now says so (`rrc sources none`).

## 1. What exists, and where it falls short

The workflow is in `docs/porting.md`, the playbook: eight rules ("How to work"), steps 1 to 5 (set up, analyse,
sessions and ticks, record, port and compare), then sections added as the toolkit grew (frames and sound, play,
quirks). `docs/design.md` §10 lists seven steps. Today `rrc status` names the next step, `rrc rules` prints the
rules, and `rrc check` refuses a few breaches (D148).

| # | Shortfall | Evidence |
|---|---|---|
| 1 | The toolkit's driver stops where the work begins | `rrc status` knows the set-up chain (game, analysis, session, sync points, inputs, recording, mapping, port), then says `rrc compare <first session>` for the rest of the port. In the Arena port, 58 sessions in, it says `rrc compare a-16` |
| 2 | No step has a "done" | Whether the logic, the frames or the sound of a stretch are finished is in the agent's notes; the M6 assessment had to be done by hand |
| 3 | The order of work is partly outside the playbook | Logic before screens, work in batches closed by a census, and the person's play (D124, D136, the person's direction of 2026-11-04) are in the Arena port's handoff, not in the steps |
| 4 | Where things stand is written by hand | Arena's `notes/handoff.md` is 526 lines and holds the work list; D148 records that an agent's summaries softened a rule there. The census is per session; nothing ranks the work across sessions |
| 5 | A port repository has no entry point | `rrc init` makes `knowledge/` and `sessions/`: no `RULES.md`, no notes layout, no file an agent meets first. Arena's rules reach an agent through a hook written by hand in another repository |
| 6 | Mistakes a fixed order prevents are recorded only as traps | Sync points added late renumbered a session's ticks twice; data named one at a time made every session record again (about 15 minutes); an ID worked out by hand fed the port wrong memory for weeks; the sources' order was partly skipped on the save and load work |

## 2. What makes a port fast and right

1. **Compare from the first day.** A transplant means nothing has to be ported before something is compared, so the
   port exists, empty, before any logic is written.
2. **Survey before depth.** Loops, state, files and outside sources are cheap to map and expensive to discover late.
3. **The census is the work list.** A lockstep comparison counts every difference of a session at once (16 s for 775
   ticks on Arena); the first-divergence comparison is for working on one of them.
4. **Each layer stands on the one below:** state, then frames, then sound, then live play. A frame is only compared
   where the state matches.
5. **Recordings are the dear step** (minutes; a comparison is seconds): whatever forces one is done in batches.
6. **What is learned is named at once**, with its evidence: the next question starts from names, and so does the
   next agent.
7. **What can be checked is checked by the toolkit, and what stands is said by the toolkit**, not by notes.

## 3. The workflow

Eight stages. A stage is entered when the one before is done; 4 to 6 repeat the same work loop. "Done when" is the
stage's gate: each condition is one the toolkit can test.

| Stage | What it is for | Done when |
|---|---|---|
| **0 Set up** | The repository, the game's version, the tools, the person's rules | `rrc doctor` misses nothing; the copy is a recorded version; `RULES.md` holds the person's answers (scope, sources kept out, what to ask) |
| **1 Survey** | Know the program before touching it | The analysis is in, with a session's run added to it; one session from power-on into play is recorded and its run repeats; every loop it passes through has a sync point; the boundary values are declared and late reads settled or noted; the play pass's top calls are named; the outside sources and the game's files are listed |
| **2 Sessions** | Fix what is to be reproduced | The scope is written down (its sessions); each is recorded and repeats; the toolkit says how much code they run |
| **3 Skeleton** | A port that compares before it does anything | The port builds on the runtime; a transplant at each session's first tick loads without a difference at that tick; a census exists for every session; no named datum is unmapped |
| **4 Logic** | The simulation, subsystem by subsystem | Every session matches in its mapped fields from a transplant, nothing fed; the census is empty; every suspect is judged and every quirk registered, branched on and tried fixed; what the port names and no session runs is read or reached |
| **5 Data and start-up** | The port loads the game itself | No hook or feed gives data the game's files hold; each format the port loads decodes equal to what the original loaded; every session matches in its fields from power-on |
| **6 Presentation** | Frames, then sound | Every session matches state, frames and each sound device the original uses in it, from a transplant and from power-on |
| **7 Play** | A port a person plays | A scripted live run reaches play from power-on; saves load both ways with the original; each thing the person's play-test found became a session |

Modern mode is a ninth stage once the toolkit has it (M7).

**0 Set up.** `rrc init`, `rrc game --add-version`, `rrc doctor`. The agent asks the person the questions of
`RULES.md` once: the stretch of the game to port, the sources to keep out, where to commit, what needs asking.

**1 Survey.** `rrc analyze` (a signature pack for the game's compiler first). Sweep the outside sources and list them
in `notes/sources.md`: what each covers and how far it is trusted; ask before downloading. Get a first session into
play: two minutes of the person's play (`rrc play`), or a script from a walkthrough. `rrc calls` finds the loops:
every loop of the stretch gets its sync point now, a polling one gated, before sessions are built on it. `rrc inputs`
finds the boundary values; record; settle the late reads. `rrc analyze --session` adds what Ghidra missed, `rrc files`
lists which file went where, and `rrc pass` of a play tick is the outline of the port's step: name its top calls.

**2 Sessions.** The person plays the scope once (`rrc play`, minutes): scripts reach only what an agent can plan
(D124). The agent adds a scripted session for each thing play missed, from another session's tick (D118), and a
scenario's edits for a state play rarely reaches (D59).

**3 Skeleton.** Copy `examples/toy`. Find the core state from the loop down (who writes what), map it, and run the
empty port in lockstep over every session: everything differs, and that census is the work list's first state.
`rrc progress` names the largest changing memory nothing maps: map it or ignore it, with a reason.

**The work loop** (stages 4 to 6), once per item of the work list:

1. **Take** the top item: the most passes, and among equals the earliest in the pass's order.
2. **Plan and read**, in the sources' order: what is named already, the decompiled code, the disassembly where it is
   in doubt, the outside sources on that subsystem, the files it reads. Write the question down first.
3. **Name and map** everything the item touches, together; record once.
4. **Port** it: a function for each of the original's, its ID in the comment; each bug registered as it is met; each
   path left out noted where it would be.
5. **Compare** from a transplant just before the item's first difference; at each divergence, the table below.
6. **Close:** the census again over every session, the item at zero; the suspects of the sessions it touched judged;
   `rrc check`; commit. A gap in the toolkit is fixed in the toolkit, then the item goes on.

**4 Logic** runs the loop on fields, with `--fields` so no screen stops it. **5 Data and start-up** runs it on what
hooks and feeds still give, then on the start-up's pass and the menus before play. **6 Presentation** runs it on
frames, most ticks first, then on sound, a device at a time (digitized sound, then music as a subsystem at its
interrupt's rate, D22).

**When the port differs:**

| What the comparison shows | Look first at |
|---|---|
| A difference at the first tick after a transplant | The mapping: a field missing, or its encoding |
| A field differs and a writer is named | That function's code, and the port's function of the same ID |
| The writer is an interrupt handler, or nothing in the pass wrote it | An input not declared, or a handler's work that is a subsystem |
| It matches for many ticks, then drifts with the player's input | A late read (`rrc trace --tick`): read it where the original did |
| It differs only once a feed is removed | The fed part: the comparison says how often the port's own value differed |
| It differs right after a scenario's edit | The edit, before the port |
| The port does what the code means and the original doesn't | A bug of the original's: `rrc suspects`, then a quirk |
| The state matches and the frame doesn't | The drawing code's own state at the transplant, then who drew the pixel |
| The state matches and the sound doesn't | The order of events, then a driver's answer the port needs a hook for |
| Three rounds with no progress | Another source (code instead of runs, or an outside source); then the person |

**The rules.** The playbook's eight stay (plan before running; don't play to find things; two kinds of evidence;
name what is learned; use the whole toolkit; the original is the specification; every bug registered; nothing departs
silently). Six more, each from a cost already paid:

9. **An ID comes from a tool, never from arithmetic.** Data is given as the code addresses it (`DS:7A46`).
10. **Batch what forces a recording:** names, sync points, inputs and hooks for an item go in together.
11. **Find every loop of a stretch before building sessions on it:** a sync point added later renumbers their ticks.
12. **Never tune a layer over an unmatched one below it:** fields, then frames, then sound, then live play.
13. **Where things stand is the toolkit's to say:** `rrc status` and the work list. Notes hold what was learned and
    why, never the state of the work.
14. **A toolkit gap is fixed in the toolkit**, as a general feature with a test, never worked around in a port.

A session of work starts with `rrc rules` and `rrc status`, and ends with `rrc check` passing and a commit.

## 4. Making it the toolkit's core

In the order I would build them. A, B and F are documents and scaffolding; the rest is `rrc`.

| | Change | What it gives | Size |
|---|---|---|---|
| A | The playbook rewritten around the stages: the workflow first (rules, stages, the loop, the table), then today's text regrouped as each stage's reference. `docs/design.md` §10 becomes the stages | One standard every port follows, short enough to print | small |
| B | `rrc init` makes the port's entry point: `RULES.md` with the questions for the person, the notes' layout, an agent file and a session-start hook that run `rrc rules` and `rrc status` | Every port starts as Arena does today by hand | small |
| C | The scope in `rrc.toml`: the sessions this port is to reproduce | Gates and the work list have something to be measured over; the person sets it | small |
| D | `rrc status` drives every stage: the stage, each condition of its gate not yet met, and the next command, worked out from the repository, never stored | The agent is told what is next through the whole port, and can't mark a stage done | medium |
| E | The work list: the census of every session in scope, run again where the port changed, merged and ranked, with the paths not ported, the suspects not judged and the code no session runs | The backlog leaves the hand-written handoff | medium |
| F | `rrc status --all`: the stage and gates, each session's last outcome, the top of the work list, the quirks, the progress | The handoff is generated; notes shrink to what was learned | small, after D and E |
| G | A test that walks the toy port through the stages: `rrc status` names each next step and each gate closes | The workflow can't rot, and M9's blind port runs on exactly it | medium |

Gates are **reported, not refused**: `rrc status` says what a stage still lacks, and a commit is refused only for
what `rrc check` refuses today. Stages overlap in real work (Arena drew its dungeon before its menus' logic was
in), and a refusal there would block honest work in progress. What an agent may not do is report a stage done while
`rrc status` lists a condition.

The Arena port is the test of D to F: given its scope, `rrc status` should say, unprompted, what the M6 assessment
found by hand.

## 5. Keeping the agent on the best tool

The person's review, 2026-11-11: the largest problem in practice is an agent that settles into one approach and
forces it through every problem, without asking whether another tool, the code, or what is already on the internet
would answer sooner.

The playbook already says not to (rule 5, the sources' order, "Where to start, by question"), and it still happens.
A rule that keeps being missed is given to a tool (D148). Why it happens, and which change below answers it:

| Why the agent stays on one tool | Answered by |
|---|---|
| The last tool is in front of it; the rule was read hours ago | H, I |
| Using another tool means knowing it exists and suspecting it would help | I, J, N |
| From inside, every retry looks like progress | H, L |
| Outside sources are nowhere in the toolkit, and a download needs asking | K |
| Whoever dug the hole keeps digging | M |
| Nothing measures it, so nobody sees it | H, O |

| | Change | What it gives | Size |
|---|---|---|---|
| H | **A journal and a stuck signal.** `rrc` notes each command in the cache: the command, the session, the place or field it was about, how it ended (a comparison: its first difference). A comparison that ends at the same first difference a third time says so, with the kinds of evidence not yet looked at for that place (its writer's code, a watch or trace of the value, a source on the subsystem). `rrc status` gives each open item its tool mix (`compare 14, show 0, watch 0, sources 0`) | The agent is told it is stuck, since it can't tell from inside; the person sees drift in one line | medium |
| I | **Each result names the next moves.** The table of section 3 and the playbook's "Where to start, by question" move into the tools: a difference ends with which row it is and the commands for it, always of two kinds (the code and a run, or a source) | The alternative is met where the choice is made, not in a document read at the start | medium |
| J | **One answer for "what is known here".** `rrc show` on a place, a name or a field gives all the toolkit holds: name and type, who reads and writes it, the sessions and ticks that run it, the file it was loaded from, the quirks and suspects near it, the sources on its subsystem, the port's code that names it | The other evidence appears without having to think of asking for it | small |
| K | **Outside sources in the port's books.** `sources.toml`: each source's address, what it covers, how far it is trusted, and the ones the person keeps out (listed, so they aren't found again). `rrc sources TOPIC` says who wrote about a thing, and the work list shows it on each item. The survey's gate asks for the sweep with its searches recorded, over a fixed list of kinds (below). Downloads are asked of the person once, as one list | The internet is part of the workflow, with a gate, and is brought up at each item | small |
| L | **A question, two routes and a budget for each item.** Taking an item records three lines: the question, the route chosen with one other considered, and the budget (runs or minutes) after which the route changes. The journal counts against it and says when it is spent | "Is there a better tool" is asked when it costs nothing: before starting | small, after E and H |
| M | **Fresh eyes when stuck.** At a stuck signal the playbook's step: write the question without its history and give it, with `rrc show`'s answer, to a fresh agent, which proposes routes without the sunk cost (where none can be started: `rrc rules`, then plan again from the question). The same before each new subsystem. D34 holds: the toolkit says when, the agent does it | The one way out of a rut that doesn't depend on the agent in it | small (playbook) |
| N | **Recipes.** The playbook gets "Recipes, by problem": the best-known route, its tools in order and its cost, for what every port meets (the main loop, the random numbers, a file's format, a table's layout, a value that drifts, a sound driver's calls, a level's contents). A route found the hard way is written as a recipe before its item closes; `rrc how WORD` prints those that match | The next port starts from the best-known approach instead of finding it again | small |
| O | **Measured in the blind tests.** Each blind test of the playbook (D46, M9) reports the journal's tool mix, stuck signals and time for each item; a change to the playbook is judged by them | Whether any of this works is known, not hoped | small, after H |

**The kinds of sources the sweep covers (K).** For the game: its wikis, format documentation, reimplementations (and
those of other games on the same engine), modding tools, patch notes, walkthroughs and maps (they script sessions),
bug lists and speedrunners' glitch lists (each entry a candidate quirk and a session to write). For the program's
parts, as the analysis identified them: the compiler's library, the DOS extender, the sound driver, the compression.
These are often documented or published, and what is published isn't reverse engineered.

**What I would not do.** Another rule alone: rule 5 is that rule, and it was missed. A refusal: a tool can't know
which route is right, only that one has stopped paying, so H and L signal and never block. H's threshold (three
times) is a guess to tune on the Arena port: a signal that fires too often is ignored.

**The order.** H, I and K first: the signal, the alternatives at hand, the internet in the books. J and N are cheap
and go with them. L and M need the work list (E) and the journal. O follows H.

## 6. What is yours to decide

1. **The stages and their order.** In particular stage 2: you play each port's scope once, early (minutes of your
   time against hours of scripted steering).
2. **Gates reported or refused.** I recommend reported, as above.
3. **The scope as a setting you give each port** (change C).
4. **The six new rules**, and whether any should become a refusal of `rrc check` now.
5. **When to build it.** A and B can be done at once; C to G are toolkit work that would come before M6 goes on, as
   you said, or beside it.
6. **Section 5's changes:** which of H to O, and whether fresh eyes (M) may start a second agent on its own or asks
   you first.
