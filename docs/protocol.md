# The emulator protocol

How a tool drives an emulator: `rrc` drives Clockwork this way, and so will the runtime and the comparison
(M3). It is a contract between programs, not a library: an emulator can be written in any language and under
any licence, and a tool never links one (D58). Revision **1**; Clockwork implements it in
`clockwork/cli/src/serve.rs` (`clockwork serve`) for the `dos-pc` platform.

The protocol has a **core**, which any machine can answer, and a **platform extension** per kind of machine
(`dos-pc` below). A tool knows which it talks to from `hello`.

## Transport

`<emulator> serve` reads requests on its standard input and writes answers on its standard output (Clockwork
also listens on a local TCP port with `serve --port N`). A request is a JSON object on one line, with `op` and
an optional `id`; each gets one line back, with the same `id`:

```json
{"id": 7, "op": "run", "ms": 100}
{"id": 7, "ok": true, "stop": "time limit reached", "cycles": 800000, "ms": 100}
{"id": 8, "op": "frobnicate"}
{"id": 8, "error": "unknown op \"frobnicate\""}
```

An answer is `{"ok": true, ...}` or `{"error": "..."}`. A field a request may leave out is absent or `null`;
given with another type, it is an error, never its default. An emulator that panics while answering answers
with an error and drops its machine (copies in memory stay). `quit` ends the session.

## What every emulator promises

- **Determinism.** The same `load` and the same input give the same run, bit for bit: the same stops, the
  same frames and sound events, the same `hash`. Time is the emulated machine's (its cycles, and
  milliseconds counted from them), never the host's clock; there is no randomness. Floating point gives the
  same bits on every host.
- **State and records.** `hash` covers the machine's state only. Frames, sound events and console output
  are records, and the analysis tools (watches, call tracking, coverage, bounds and uninitialized-read
  checks) never change what the machine does.
- **Snapshots.** `save` and `load_snapshot` (and `snapshot`/`restore` in memory) go on exactly as the run
  would have; a snapshot's file format is the emulator's own.

## Addresses

Core: `+OFF` is an offset in the image of the program the machine started with (hex), `@LINEAR` an address
in the CPU's address space (hex). Answers name addresses the same way, `+OFF` where they fall in the image.
A platform may add its own forms (`dos-pc`: `SEG:OFF`, `%PHYS`).

## Core operations

**`hello`**: what the emulator is: `{"protocol": 1, "platform": "dos-pc", "emulator": "clockwork",
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
cycles).

**Memory and registers.** `read` `{"ranges": [{"at", "len"} or {"device"}]}`: hex strings (`data`), or null
where nothing is mapped; a device's state is the bytes the platform defines for it (D49). `write` `{"at",
"hex"}`. `regs`: the CPU's registers (the platform's names) with `at` (where it runs), `cycles`. `hash`: the
machine's state hash, as the emulator's `run` prints it.

**Input.** `input` `{"lines": ["MS:STEPS", ...]}`: scripted input at emulated times from power-on, in the
platform's script language.

**Watches.** `watch` `{"ranges": [{"at", "len"}], "reads": false, "limit": 10000}` ... `watch_end`: each
write (and read) of the watched bytes: `hits`, each `{"at", "kind", "value", "old", "cycle", "by", "chain"}`,
where `by` is `{"instruction"}`, `{"service", "ax"}` or `{"device": true}` and `chain` the calls in progress,
innermost first (`{"entry", "from", "interrupt"}`); `dropped` counts what went past the limit. With
`"summary": true`, who touched each byte instead, as runs of bytes touched alike (`bytes`: `{"at", "len",
"read", "read_in_interrupt", "written", "interrupt_writer", "interrupt_write_by", "host"}`), with the services
called (`services`) and ports read (`ports`) meanwhile. An interrupt handler is what runs for a hardware
interrupt (and what it calls).

**Calls and coverage.** `calls` `{"on": true, "count": false}`: tracks calls from now on (so a watch started
later sees the whole chain); with `count`, `call_counts` gives the calls made since, by target (`[[addr,
n]]`). `coverage` ... `coverage_end`: where each instruction that ran started (`executed`) and the calls made
(`calls`: `[[site, target, n]]`).

**Checks.** `bounds` `{"objects": [{"at", "size"}]}` ... `bounds_end`: accesses past declared objects'
ends (`findings`). `uninit` `{"more": [{"at", "len"}]}` ... `uninit_end`: reads of memory nothing wrote
(`reads`).

**Frames and sound.** `frames` `{"from", "to"}` (cycles; `to` defaults to now): the frames shown then, as
`[number, cycle, mode, hash]`. `frame_images` `{"on": true}`: from now on, the picture of each frame whose
picture changed, handed over with each `tick` or by `take_frame_images` (`images`: `[[number, hex]]`). A
picture is `FRM1`, the width and height (16-bit, little-endian) and a kind (0 indexed: a 256-entry palette of
R, G, B at 0-63, then a byte a pixel; 1 RGB: 3 bytes a pixel), raw-deflated, pixels row by row from the top
left. `sound` `{"from", "to"}`: the sound events then, by kind (the platform's kinds).

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
- **Input scripts**: `clockwork/cli/src/input_script.rs` (keys on a US keyboard, `{click X Y}`, `{stick X Y
  BUTTONS}`...).
- **Sound events** (`sound`): `opl` (`[ns, register, value]`), `speaker` (`[pit_tick, wave]`), `pcm` (digitized
  sound starts: `[ns, rate, stereo]`), `midi` (`[pit_tick, byte]`), `cd` (`[ns, sector, sectors]`).
- **`objects`** `{"program"}`: the running program, or the one named if it runs or started the one running: its
  `program` path, its load image (`image`, `image_size`) and its LE objects where its extender put them
  (`objects`: `[{"at", "size", "flags"}]`, or null) (D55, D57).
- **`rebase`**: `+OFF` counts from the image of the program running now, or from `{"at", "len"}` (where a
  launcher's game will be loaded, D57).
- **`image`** `{"dir"}`: the loaded image for analysis, with its segment or selector map.

## Changing the protocol

Any change to the protocol, an operation added or a field changed, is a new revision: the revision in
`clockwork/cli/src/serve.rs` (`PROTOCOL`) and in `rrc/src/clockwork.rs` change together, with this document.
`rrc` refuses an emulator of another revision, so a tool and an emulator from different builds stop with a
clear message instead of misreading each other.
