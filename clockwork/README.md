# Clockwork

A deterministic IBM PC with DOS, emulated in Rust: the controlled environment Rrecreate runs original games
in (`../DESIGN.md` §4). The same program with the same inputs gives the same run, bit for bit: time is
virtual, counted in executed instructions.

- **CPU:** our own 486 interpreter (real, protected and virtual-8086 mode, paging) with an 80387-compatible
  FPU; `--cpu 386` or `--cpu 286` for older processors, `--no-fpu` for none. It is checked against the
  SingleStepTests recordings of real CPUs (below).
- **Devices:** VGA and an S3 Trio64 with 4 MB and VESA BIOS Extensions 2.0, Sound Blaster 1.x to 16, OPL2 and
  OPL3, MPU-401, PC speaker, PIT, PIC, keyboard controller, CMOS and its real-time clock, game port, mouse.
- **DOS:** BIOS and DOS services as high-level emulation, child programs and TSRs, EMS and XMS, a CD-ROM
  drive with MSCDEX (a folder or a CUE/ISO image, CD audio included), a DPMI host, DOS extenders that bring
  their own host (CauseWay so far), and a COMMAND.COM with batch files.

Much of it is ported from DOSBox Staging; `../THIRD_PARTY.md` lists what came from where. Clockwork is
GPL-2.0-or-later (`LICENSE`).

## Running a program

```bash
cargo build --release -p clockwork
```

```bash
target/release/clockwork run "C:/Games/KEEN1" KEEN1.EXE --seconds 20
```

The game folder becomes drive C:, and nothing the program writes leaves the emulation. `run` is headless and
prints a short summary: why it stopped, console output, frame fingerprints, a state hash, and anything the
program used that isn't emulated yet (`--json` for tools). `clockwork --help` lists every option. The main
ones:

- `--input "25000:{enter}"` (repeatable) and `--input-file FILE` script keys and clicks at emulated times
  (from power-on): text, named keys (`{esc}`, `{up}`, `{f1}`, `{ctrl}`...), `{click X Y}` in the mouse
  driver's coordinates, `{move X Y}` (to a place: the cursor stays in the driver's range, and the motion
  stops with it), `{motion DX DY}` (by DX, DY mickeys, as a hand moves the mouse: a game that turns by the
  motion turns as far as asked), `{stick X Y BUTTONS}` (the first joystick, with `--joystick 1`;
  `{stick2 ...}` the second), `{hold KEY MS}`, `{keydown KEY}`/`{keyup KEY}`, `{wait MS}`. Run it, look at
  the `--screenshot`, add the next step, or explore step by step (below, "Exploring a game").
  `../testdata/inputs/` has walkthroughs that take Wolfenstein 3D into its first level and past a pushwall,
  and Arena and Daggerfall to their first dungeons;
- `--screenshot out.png` (or `.bmp`) saves the last screen in any video mode;
- `--save-changes DIR` writes the files the program created or changed to DIR (the game folder is never
  touched): that's how to install a game with its own installer;
- `--coverage FILE` records which instructions ran and which calls were made, in any mode and anywhere
  (linear addresses: the JSON gives the program's image, where its offsets count from), and
  `--contracts FILE` infers each called function's contract (registers read, returned and kept, stack
  arguments, memory used); both write JSON;
- `--save-snapshot FILE` saves the machine after the run, and `--load-snapshot FILE` goes on from one for
  `--seconds` more (the game's files aren't in a snapshot: give the same game folder and CD). A snapshot holds
  the machine's state, not what the run produced: frames, console output and sound count from the load on;
- `--dump-image DIR` writes the program's image for analysis: in real mode its block from the load segment
  (unpacked, generated code included), in protected mode the selector map and the memory its segments cover;
- `--cd D` (a CD drive showing C:'s files), `--cd D=DIR` (a folder of the game folder), `--cd D=GAME.CUE`;
  `--cd-label NAME`; `--drive D` and `--cwd DIR` for where the program starts; `--env NAME=VALUE`; `--ems KB`,
  `--memory MB`, `--no-xms`, `--dpmi`, `--joystick N`, `--sb SPEC`; `--wav out.wav` saves the sound.

```bash
target/release/clockwork play "C:/Games/KEEN1" KEEN1.EXE --record keen.session
```

`play` opens a window with keyboard, mouse and gamepad input and live sound (OPL, Sound Blaster and PC
speaker through the sound output, MIDI through the computer's synthesizer: `--midi-out NAME`, `--no-audio`).
With `--record`, the input is saved as a session that `run DIR --replay keen.session` reproduces exactly.
Session files are text: `<cycle> K <scancode hex>` for keys, `<cycle> M <x> <y> <buttons>` for the mouse
(`<cycle> R <dx> <dy>` for relative motion), `<cycle> J <stick> <x> <y> <buttons>` for a joystick.

`unpack` undoes an executable packer (PKLITE, LZEXE, EXEPACK, DIET...) by running the program until the
packer hands over; for a flat program under a DOS extender (an LE executable: CauseWay, DOS/4GW), it runs it
until the extender has placed its objects, then to its entry point, and writes the objects at their linear
addresses as an ELF file. `listing` prints a function's code from a load image (or a flat program's, from that ELF
file), following its control flow.

## Driving Clockwork from a tool

`clockwork serve` takes JSON requests, one per line, on stdin (or a local TCP port with `--port N`) and answers
each on one line. `load` takes `clockwork run`'s arguments; then a tool can run for a time or to a sync point's
next hit, read and write memory, take and restore snapshots, and watch memory: each read or write of the
watched bytes comes back with its instruction, cycle and call chain. Bounds checks, uninitialized reads and
input read after it changed since a tick began (`late_inputs`) work the same way. Addresses are `+OFF` (image offsets), `SEG:OFF` or `@LINEAR`. The operations are the emulator
protocol's, in `../docs/protocol.md` (its core, and the `dos-pc` platform's extension); `hello` says the
revision. A request that makes Clockwork panic gets an error and drops the machine; the server goes on.

### Exploring a game

Finding the input that takes a game somewhere by reruns from power-on is slow. A `serve` on a TCP port keeps
one machine for every connection, so a shell can step it with `clockwork ask`, which sends requests (JSON, or
an operation's name alone) and prints the answers:

```bash
target/release/clockwork serve --port 4000
```

```bash
target/release/clockwork ask --port 4000 '{"op": "load", "argv": ["C:/Games/WOLF3D", "WOLF3D.EXE"]}' '{"op": "run", "until_ms": 12000}' '{"op": "screenshot", "file": "now.png"}'
```

```bash
target/release/clockwork ask --port 4000 '{"op": "snapshot", "name": "hall"}' '{"op": "input", "lines": ["12000:{hold up 900}"]}' '{"op": "run", "until_ms": 13500}' '{"op": "screenshot", "file": "now.png"}'
```

Look at the picture, then go on, or `restore` the copy and try another step. Input lines are a script's
(`--input-file`'s): their times count from power-on, and a line whose time has passed is refused, since its
input would land later than it says. The lines that worked, in order, are the session's script.

`clockwork gdb [--port N] <dir> <program> [run's options]` starts the program stopped at its entry and waits
for a debugger speaking GDB's remote protocol (`target remote 127.0.0.1:1234` in gdb, or Ghidra's debugger):
registers, memory by linear address, breakpoints, stepping, Ctrl-C. In real mode `$eip` is the offset in CS
(`set architecture i8086`, then `x/10i $cs*16+$eip`).

`tools/who_wrote.py` is an example: who wrote a variable, and in which tick. With the instruction where a
game's tick ends at `+0A31C` and a two-byte variable at `+21C40`:

```bash
python clockwork/tools/who_wrote.py "<game folder>" GAME.EXE --sync +0A31C --var +21C40:2 --tick 120 -- --input-file walk.inputs
```

```text
tick 120 (from 4130 ms): 2 writes to +21C40
  +21C40 = 01 (was 00)  by +0B2E4
    called +0B2C0 (from +0A3F1) <- +0A350 (from +0A31F) <- ...
```

## Investigating a program

- `--hotspots N` samples where the CPU spends its time and disassembles the N hottest addresses;
- `--disasm SEG:OFF` (or `here`) prints registers and a disassembly after the run;
- `--dump SEG:OFF[:LEN]` hex-dumps memory after the run (LEN in hex), `--dump +OFF[:LEN]` the program's load
  image at an image offset;
- these three follow protected mode (a selector's descriptor, the page tables) without disturbing the
  machine;
- the summary shows the timer rate, who owns INT 08h/1Ch, how often each IRQ line was delivered and which
  are masked;
- `--trace FILE` writes every BIOS/DOS/EMS service call with its inputs and results; `--midi FILE` the MIDI
  bytes; `--opl FILE` the OPL register writes;
- `--watch-exec SEG:OFF[:LEN]` records every instruction run in a range and where it was entered from;
  `--watch-log FILE` writes each with its registers.

## The test games

The walkthroughs in `../testdata/inputs/` are the regression check for whole games: after a change to
Clockwork, they should still reach their screenshots. Steam's DOSBox setups translate as:

```bash
target/release/clockwork run "<Wolf3D folder>" WOLF3D.EXE --input-file testdata/inputs/wolf3d-first-level.inputs --seconds 51 --screenshot wolf3d.png
```

```bash
target/release/clockwork run "<Arena's ARENA folder>" ACD.EXE --cd D --drive D --env ARENADATA=C: --args "-Ssbpdig.adv -IOS220 -IRQS7 -DMAS1 -Msbp2fm.adv -IOM220 -IRQM7 -DMAM1" --input-file testdata/inputs/arena-first-dungeon.inputs --seconds 340 --screenshot arena.png
```

```bash
target/release/clockwork run "<Daggerfall's DF folder>" DAGGER.EXE --cwd DAGGER --cd D=DFCD --cd-label Daggerfall --input-file testdata/inputs/daggerfall-privateers-hold.inputs --seconds 370 --screenshot daggerfall.png
```

(Wolfenstein 3D's shareware v1.4 installs with its own installer: `run 1wolf14 INSTALL.EXE --seconds 60
--input "1000:{enter}" --input "12000:c" --save-changes installed`, then `run installed/WOLF3D WOLF.EXE
--seconds 60 --save-changes wolf3d`.)

## Validating the CPU

The interpreter is checked against the SingleStepTests suites: instruction tests recorded on a real 80386EX
and 80286 (MIT, data in the public domain). Fetch the real-mode sets into `reference/SingleStepTests`
(git-ignored):

```bash
git clone --depth 1 --filter=blob:none --sparse https://github.com/SingleStepTests/80386.git reference/SingleStepTests/80386
```

```bash
git -C reference/SingleStepTests/80386 sparse-checkout set v1_ex_real_mode
```

(the same for `80286` with `v1_real_mode`), then run them:

```bash
cargo run --release -p cpu --example singlestep -- reference/SingleStepTests/80386/v1_ex_real_mode --skip-limit-faults
```

A name filter (`D3`, `660FA4`) runs some files only; `--show N` and `--detail` show failures. Known
differences: real-mode segment-limit faults (not enforced, as in DOSBox; counted apart), an 8-bit IDIV quirk
(10 tests where an overflowing division returns AL=80h instead of #DE), and two tests where an address wraps at
a segment's end or above 1 MB (669A, C2). Anything else failing is a regression.
