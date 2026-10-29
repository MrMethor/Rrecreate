# The emulator protocol

How a tool drives an emulator: `rrc` drives Clockwork this way, and so will the runtime and the comparison
(M3). It is a contract between programs, not a library: an emulator can be written in any language and under
any licence, and a tool never links one (D58). Revision **9** (below, what each revision changed); Clockwork implements it in
`clockwork/cli/src/serve.rs` (`clockwork serve`) for the `dos-pc` platform.

The protocol has a **core**, which any machine can answer, and a **platform extension** per kind of machine
(`dos-pc` below). A tool knows which it talks to from `hello`.

## Transport

`<emulator> serve` reads requests on its standard input and writes answers on its standard output. Clockwork
also listens on a local TCP port with `serve --port N`: one connection at a time, all with the same machine, so
a tool can come back to it (`clockwork ask --port N REQUEST...` sends requests and prints the answers); `quit`
ends a connection there. A request is a JSON object on one line, with `op` and an optional `id`; each gets one
line back, with the same `id`:

```json
{"id": 7, "op": "run", "ms": 100}
{"id": 7, "ok": true, "stop": "time limit reached", "cycles": 800000, "ms": 100}
{"id": 8, "op": "frobnicate"}
{"id": 8, "error": "unknown op \"frobnicate\""}
```

An answer is `{"ok": true, ...}` or `{"error": "..."}`. A field a request may leave out is absent or `null`;
given with another type, it is an error, never its default. Counts (milliseconds, cycles, lengths) are whole
numbers, which may be written `16000.0`. An emulator that panics while answering answers
with an error and drops its machine (copies in memory stay). `quit` ends the session.

## What every emulator promises

- **Determinism.** The same `load` and the same input give the same run, bit for bit: the same stops, the
  same frames and sound events, the same `hash`. Time is the emulated machine's (its cycles, and
  milliseconds counted from them), never the host's clock; there is no randomness. Floating point gives the
  same bits on every host.
- **State and records.** `hash` covers the machine's state only. Frames, sound events and console output
  are records, and the analysis tools (watches, call tracking, coverage, bounds, uninitialized-read and
  late-input checks) never change what the machine does.
- **Snapshots.** `save` and `load_snapshot` (and `snapshot`/`restore` in memory) go on exactly as the run
  would have; a snapshot's file format is the emulator's own.

## Addresses

Core: `+OFF` is an offset in the image of the program the machine started with (hex), `@LINEAR` an address
in the CPU's address space (hex). Answers name addresses the same way, `+OFF` where they fall in the image.
A platform may add its own forms (`dos-pc`: `SEG:OFF`, `%PHYS`).

## Core operations

**`hello`**: what the emulator is: `{"protocol": 9, "platform": "dos-pc", "emulator": "clockwork",
"version": "0.1.0"}`. A tool refuses a revision or platform it doesn't speak.

**Loading.** `load` `{"argv": [dir, program, options...]}`: a machine with the game's folder and program,
started as the emulator's `run` command would start it (the options are the platform's); answers where the
image is (`image`, `@LINEAR`), `image_size` and `cycles`. `load_snapshot` `{"argv", "file"}`: from a snapshot
(the same folder and options). `save` `{"file"}`: a snapshot on disk. `snapshot`, `restore`, `forget`
`{"name"}`: copies in memory.

**Running.** `run` runs on and says why it stopped (`stop`), with `cycles` and `ms`; it takes one of:
`{"ms"}` or `{"cycles"}` (that much more), `{"until_ms"}` (to that time from power-on), `{"sync": addr, "n":
1}` (to the n-th next hit of that sync point) or `{"sync": "next", "n": 1}` (to the n-th next hit of any: the
next tick); `max_ms` (default 60000) bounds a run to a sync point. A stop at a sync point says `sync`: `{"at",
"hit"}` (and `skipped`, at a gated one). `tick` `{"from", "ranges", "max_ms"}` is a tick in one request: runs
to the next tick, then answers as `read` (`data`), `frames` and `sound` since cycle `from` would, and with
the pictures recorded meanwhile (`images`).

**Sync points.** `sync` `{"at", "log": false, "gate": [...]}`: counts the hits of the instruction at `at`
(`log`: their cycles too). With a gate (D48), a hit counts only when the gate's sources changed since the
point's last counted hit; a source is `{"at", "len"}` (memory) or `{"device"}`. `syncs` `{"at"}`: its hits (and
cycles), and the hits counted at every point (`ticks`). `edits` `{"edits": [{"tick", "at", "hex"}]}`: writes into
memory (as the program sees it) when the session's `tick`-th hit is counted, before that instruction runs,
replacing the edits set before; an edit at a tick counted already is refused (D64). They are part of the
machine's state: a snapshot keeps those still to come. A watch names their writes `{"edit": true}`.

**Memory and registers.** `read` `{"ranges": [{"at", "len"} or {"device"}]}`: hex strings (`data`), or null
where nothing is mapped; a device's state is the bytes the platform defines for it (D49). `write` `{"at",
"hex"}`. `regs`: the CPU's registers (the platform's names) with `at` (where it runs), `cycles`. `hash`: the
machine's state hash, as the emulator's `run` prints it.

**Input.** `input` `{"lines": ["MS:STEPS", ...]}`: scripted input at emulated times from power-on, in the
platform's script language, read as a script file is (blank lines and `#` comments skipped, line ends
trimmed). A line whose time has passed is refused: its input would land later than it says.

**Watches.** `watch` `{"ranges": [{"at", "len"}], "reads": false, "limit": 10000}` ... `watch_end`: each
write (and read) of the watched bytes: `hits`, each `{"at", "kind", "value", "old", "cycle", "by", "chain"}`,
where `by` is `{"instruction"}`, `{"service", "ax"}` or `{"device": true}` and `chain` the calls in progress,
innermost first (`{"entry", "from", "interrupt"}`); a write that copied a byte the instruction read (`MOVS`,
`PUSH` and `POP` of memory) has `from`, that byte's address (D85); `dropped` counts what went past the limit. With
`"summary": true`, who touched each byte instead, as runs of bytes touched alike (`bytes`: `{"at", "len",
"read", "read_in_interrupt", "written", "interrupt_writer", "interrupt_write_by", "host"}`), with the services
called (`services`) and ports read (`ports`) meanwhile, and each instruction's uses of each port
(`port_uses`: `{"port", "at", "write", "interrupt", "n"}`, `interrupt` when it ran in an interrupt handler).
The ranges may be empty, to find only the services and ports. An interrupt handler is what runs for a hardware
interrupt (and what it calls).

**Calls and coverage.** `calls` `{"on": true, "count": false, "log": false, "limit": 100000}`: tracks calls
from now on (so a watch started later sees the whole chain); with `count`, `call_counts` gives the calls made
since, by target (`[[addr, n]]`); with `log`, `call_log` gives the calls and interrupts made since, in order,
and starts again (`calls`: `[[depth, site, target, kind]]`, the depth the frames open before it and the kind
`call`, `int` or `irq`; `start`: the chain when the log began, innermost first; `dropped`: those past
`limit`) (D83). `coverage` ... `coverage_end`: where each instruction that ran started (`executed`) and the calls made
(`calls`: `[[site, target, n]]`).

**Files.** `files` ... `files_end`: each read and write of a file the program made through the system
(`accesses`: `{"cycle", "write", "file", "offset", "len", "at", "by", "chain"}`: the file's name as the
program opened it, where in it and how many bytes, the program's buffer, where the call returns to, and the
functions it was made in, innermost first, while `calls` is on) (D78).

**Contracts.** `contracts` ... `contracts_end`: each function the program called since, as its calls used it
(`functions`: `{"at", "calls", "inputs", "flags_in", "outputs", "flags_out", "preserved", "pops", "returns",
"arg_bytes", ...}`: the registers and flags read before written, those the code after a call read as its
results, those every call kept, the bytes popped with the return, how it returned and the stack argument
bytes read; `clockwork run --contracts` writes the same, `clockwork/dos/src/contracts.rs`). Real mode (D81).

**Checks.** `bounds` `{"objects": [{"at", "size"}]}` ... `bounds_end`: accesses past declared objects'
ends (`findings`). `uninit` `{"more": [{"at", "len"}]}` ... `uninit_end`: reads of memory nothing wrote
(`reads`). `late_inputs` `{"sources": [{"at", "len"} or {"device"}, with "sync": [addr]; a part's output also
"follow_writes": false and "read_at": [addr], D96]}` ...
`late_inputs_end`: input read after it changed since a tick began (D60). A source's tick begins at each
counted hit of its sync points (`sync`; left out, any sync point's), where its value is kept; until the next,
each read the program makes of it (a device: each call of its driver, read of its port) is compared with
that value, which the program's own writes change, and what interrupt handlers read and write is left out
(the emulator tracks calls for it). The late reads of a source are the wait's, and left out, when the next hit
of one of its sync points (counted or not) comes in the call all of them were made in (the innermost call or
interrupt; for a driver, the one its call was made in), at most 40 of the program's instructions after the
last (interrupt handlers' don't count). An instruction's reads of a source are numbered from the last hit of
its sync points; a device's state the program changes through its driver (a mouse position set) is what it
should read next. The answer: `late`, `[{"tick", "source", "cycle", "at", "read", "bytes"}]`, each a read that
saw the source otherwise than the same instruction's read before it (the first, otherwise than the tick's
value): the tick (the cycle of its hit), the source (its index), when and by which instruction (for a driver,
the instruction its call returns to), which of that instruction's reads (from 1), and the bytes that differ
from the tick's value (`[offset, value]`; none: the tick's value again); `dropped`, what went past 10000.

**Frames and sound.** `frames` `{"from", "to"}` (cycles; `to` defaults to now): the frames shown then, as
`[number, cycle, mode, hash]`. `frame_images` `{"on": true}`: from now on, the picture of each frame whose
picture changed, handed over with each `tick` or by `take_frame_images` (`images`: `[[number, hex]]`). A
picture is `FRM1`, the width and height (16-bit, little-endian) and a kind (0 indexed: a 256-entry palette of
R, G, B at 0-63, then a byte a pixel; 1 RGB: 3 bytes a pixel), raw-deflated, pixels row by row from the top
left. `sound` `{"from", "to"}`: the sound events then, by kind (the platform's kinds). `screenshot` `{"file"}`:
the screen now as a picture file (PNG for `.png`, the platform's other format otherwise), answering its
`width`, `height` and the last `frame`'s number.

## The `dos-pc` platform

An IBM PC with DOS (Clockwork).

- **`load`'s options** are `clockwork run`'s (`clockwork --help`): drives, CD, `--cwd`, memory, devices,
  `--input-file`, `--replay`. The image is the started program's load image (from its PSP's paragraph 10h);
  after `load_snapshot` too, though another program may run then. `load` also answers its PSP (`psp`).
- **Addresses** also take `SEG:OFF` (as the CPU sees it now); physical memory a check reports while paging is
  on is `%PHYS`, and memory a watch reports is named by the linear address it was watched at.
- **Sync points** take `program` (a file name): their hits count only while it runs or a program it started
  does (D56, D57).
- **Devices** (`read`, gates): `mouse` (what INT 33h would report, 46 bytes) and `joystick` (the game port's
  sticks, 12 bytes); `docs/formats.md` has their layouts.
- **Registers** (`regs`): `eax` ... `edi`, `es`, `cs`, `ss`, `ds`, `eip`, `flags`, `protected`.
- **Services** in watches are by interrupt (`08`, `21`, `33`...); ports by number.
- **Input scripts**: `clockwork/cli/src/input_script.rs` (keys on a US keyboard, `{click X Y}`, `{motion DX
  DY}` in mickeys, `{stick X Y BUTTONS}`...). `screenshot` writes BMP for any other extension.
- **Sound events** (`sound`): `opl` (`[ns, register, value]`), `speaker` (`[pit_tick, wave]`), `pcm` (digitized
  sound starts: `[ns, rate, stereo]`), `midi` (`[pit_tick, byte]`), `cd` (`[ns, sector, sectors]`).
- **`objects`** `{"program"}`: the running program, or the one named if it runs or started the one running: its
  `program` path, its load image (`image`, `image_size`) and its LE objects where its extender put them
  (`objects`: `[{"at", "size", "flags"}]`, or null) (D55, D57).
- **`rebase`**: `+OFF` counts from the image of the program running now, or from `{"at", "len"}` (where a
  launcher's game will be loaded, D57).
- **`image`** `{"dir"}`: the loaded image for analysis, with its segment or selector map; answers what it
  wrote (`wrote`).

## Revisions

1. The first (D58).
2. `late_inputs`, `late_inputs_end` (D60).
3. `screenshot`; counts may be written as whole floats; `input` refuses a line whose time has passed and
   reads lines as a script file; a TCP `serve` keeps one machine for every connection.
4. A summary watch's `port_uses`; `image` answers what it wrote (it printed it, onto the answers).
5. `edits`; `syncs` answers `ticks`; a watch's `by` may be `{"edit": true}`.
6. `files`, `files_end` (D78).
7. `contracts`, `contracts_end` (D81); `calls` takes `log` and `limit`, and `call_log` (D83); a watch's write
   hit may have `from`, the byte it copied (D85).
8. `late_inputs_end` answers each read that saw otherwise than its instruction's read before, with `read`, its
   number (D93).
9. A `late_inputs` source of memory may have `"follow_writes": false` and `"read_at": [addr]`: what a part of
   the program writes (D96), the program's writes not followed, and only the reads of those instructions
   counted (none listed: all).

## Changing the protocol

Any change to the protocol, an operation added or a field changed, is a new revision: the revision in
`clockwork/cli/src/serve.rs` (`PROTOCOL`) and in `rrc/src/clockwork.rs` change together, with this document
and its list of revisions.
`rrc` refuses an emulator of another revision, so a tool and an emulator from different builds stop with a
clear message instead of misreading each other.
