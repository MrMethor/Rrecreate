# File formats

The files a port repository holds and the toolkit writes (DESIGN.md §9). A port in any language reads and
writes these to be compared (D38). Text is UTF-8 with LF line ends.

## `rrc.toml`: the port

```toml
[game]
name = "wolf3d"                 # short, for file names and reports
title = "Wolfenstein 3-D"       # optional

[original]
program = "WOLF3D.EXE"          # as `clockwork run` takes it: in the game's folder, or the one --cwd names
options = ["--ems", "0"]        # optional: more `clockwork run` options (CD drive, current folder, ...)
# module = "GAME.EXE"           # optional: the program a launcher starts, analysed and named instead

[[version]]                     # one per version the port supports (D30)
name = "shareware 1.4"
[version.files]                 # each file by SHA-256 (lowercase hex); paths are `/`-separated
"WOLF3D.EXE" = "6d8f…"
```

A copy of the game is a version when it holds every file the version lists with that hash (paths compare
without case, as DOS's do); other files in the folder don't matter. The module part of the program's IDs is
its file name, uppercase (`WOLF3D.EXE`). When the program is a launcher that starts the game's program (a
setup check, then the game), `module` names the game's program, a path like `program`'s: it is the one
analysed, and its file name is the module of IDs (`FALL.EXE#1+8DEB4`); the sessions still start the launcher,
and find where it puts the game's program (`index/places`, below).

A port repository also has `.gitignore` (`rrc.local.toml`, `.rrc/`, `target/`) and `.gitattributes` (text with
LF line ends on every platform).

`[port] imports = false` makes `rrc sigs name` refuse: an honest test bench's knowledge comes from its own
analysis only (D46).

```toml
[port]
command = ["cargo", "run", "--quiet", "-p", "toy-port", "--"]   # the port's program (D61)
build = []                      # optional: what builds it first
```

`command` is a command line run in the port repository: `rrc compare` adds `run` and its options (below, "The
port's files"). `build`, if given, runs before it.

## `rrc.local.toml`: this machine (not committed)

```toml
game = "C:/Games/WOLF3D"       # the person's copy of the game
cache = "D:/rrc-cache"          # optional: the cache, if not the user's default

[tools]                         # optional, where rrc doesn't find them by itself
clockwork = "…/clockwork.exe"  # else RRC_CLOCKWORK, next to rrc, or on the PATH
ghidra = "C:/Tools/ghidra_12.1.4_PUBLIC"   # else GHIDRA_INSTALL_DIR
java = "C:/Tools/jdk-21"        # else JAVA_HOME
```

## `.rrc/`: this machine's state for the port (not committed)

- `files.tsv`: the hashes of the game's files with their size and modification time, so `rrc` hashes a file
  again only when it changed.
- `analysis.toml`: what `rrc analyze` left: the hash of the game's program it analyzed (`original`), what Ghidra
  imported (`program`, in the cache: unpacked by Clockwork when `unpacked`), Ghidra's export (`export`, in the
  cache), the records of the functions added to it (`additions`, in the cache, read after the export's), the
  entry point's offset in the load image (`entry`, which places Ghidra's addresses in the image), for an LE
  program its objects as Clockwork placed them (`objects`, `[linear address, size]` in the LE's order: Ghidra's
  addresses are these, and IDs name object `n` as `#n`) and Ghidra's version.
- `ghidra/`: the last analysis's working files: the program Ghidra imported, the export script, Ghidra's log.
- `knowledge.lock`: there while an `rrc` changes the knowledge files (from loading them to saving them), so
  two at once don't lose each other's changes; one older than a minute is taken over. The files themselves
  are written aside and renamed.
- `compare/<session>/`: the files of the session's last `rrc compare` (below, "The port's files").
- `assets/`: the pictures `rrc asset` wrote, the person's (D78).
- `frames/`: the pictures `rrc frame` wrote; `snapshot-<process>.tmp`: a snapshot `rrc record` is saving.

## Ghidra's export

JSON lines from `ghidra/RecExport.java`: a `program` record (name, language, compiler, `entry`, Ghidra's
version), `block` records (memory blocks), `function` records (`address`, `name`, `source`, `convention`,
`signature`, `thunk`, `fid`: Function ID hashes `{units, full, specific, extra}`, `body`: `[address, length]`
ranges), `call` records (`caller`, `site`, `callee`), `ref` records (an instruction's reference to data: `from`,
`to`, `kind`: `read`, `write`, or `data` for an address taken) and `code` records (`address`, `asm`, `c`).
Addresses are `{"linear": n, "text": "SSSS:OOOO"}`; an address's image offset is its linear address minus the
entry point's, plus `entry`. Functions the knowledge names that Ghidra didn't find are created first (the
script's `functions FILE` arguments), so they are in the export like the others.

## The cache

A folder of files named by their SHA-256: `objects/<first 2 hex digits>/<the other 62>`; and `index/<kind>/<key>`,
what an earlier run made from given inputs:
- `index/analysis/<key>`: the `analysis.toml` of a program analysed by the same export script, keyed by the
  program's hash, the script and whether it decompiled; Ghidra's project of that analysis is kept in
  `ghidra/<key>/`, to add functions to it without analysing it again;
- `index/additions/<key>`: the records of the functions added to an analysis, keyed by the analysis and the
  functions' offsets;
- `index/snapshots/<key>`: the Clockwork snapshots `rrc record` kept of a session (`[[snapshot]]` with `tick`,
  `cycle`, `ms` and the snapshot's `hash`), keyed by what makes the run and its ticks the same: Clockwork's
  sources (`clockwork --source`; from one that doesn't say, its program's hash: D76), the game's files, the
  program and its options, the session's options and input (its file's hash), and the sync points (D51);
- `index/memory/<key>`: the memory `rrc search` read at every tick of a span (`[[span]]` with `first`, `last`,
  the `ranges` read and the memory's `hash`: the first tick's bytes, then each tick's changes, deflated), keyed
  by the session's run, its sync points and the range asked for (D76);
- `index/results/<key>`: the output of `rrc calls` and `rrc inputs`, and the file accesses `rrc files`
  found (JSON), keyed by the build of `rrc` that gave it (its program's hash), the command, its arguments, the
  session's run (as above, without the sync points), the knowledge files and the analysis;
- `index/places/<key>`: where a session's run puts the game's program, when it isn't simply loaded where
  `+OFF` counts from (an LE program, or one a launcher starts): `image BASE:SIZE`, then `object
  BASE:SIZE:FLAGS` for each LE object, a line each, in hex; keyed by the session's run, the module and the
  build of `rrc` (D55, D57).

It is `RRC_CACHE`,
else `cache` in `rrc.local.toml`, else `%LOCALAPPDATA%\rrecreate\cache` on Windows and `$XDG_CACHE_HOME/rrecreate`
or `~/.cache/rrecreate` elsewhere. Anything in it can be deleted; what refers to it is rebuilt or recorded again.

## `knowledge/functions.tsv` and `knowledge/data.tsv`: named functions and data

Tab-separated, a header line starting `#`, then one item per line sorted by ID (D43):

```
# id	name	provenance	confidence	evidence
GAME.EXE+0A2F0	wait_for_tick	agent:claude-opus-5-5	0.8	called once a pass of the main loop; waits for the timer
```

```
# id	name	size	type	provenance	confidence	evidence
GAME.EXE+21C40	tick_elapsed	2	u16	human
```

- **id:** `<MODULE>[#<n>]+<offset>` (D53), the offset in the loaded image, uppercase hex, 5 to 8 digits; or
  `MEM+<linear address>` for memory outside the program's image (the BIOS's data, the interrupt vectors).
- **name:** letters, digits, `_`, and the `@` and `$` of compilers' runtime names, not starting with a digit;
  unique across both tables.
- **size** (data): bytes, or empty when unknown. **type** (data): a type from `types.toml`, a number (`u8`,
  `i16`, `u32`, ...), `bool`, `far` (a far pointer), an array (`u8[256]`), or empty.
- **provenance:** who decided, and from what: `human`, `agent:<model>`, `ghidra`, `library:<pack>`,
  `prior-art:<source>`, `map` for a linker map's
  names (`rrc sigs name`, in a port made to build a signature pack).
- **confidence:** 0 to 1, or empty. **evidence:** free text (IDs, sessions, sources), no tabs.

Trailing empty columns may be left out. Lines starting `#` and blank lines are ignored.

## `knowledge/types.toml`: structures and enumerations

```toml
[[struct]]
name = "actor"
size = 12
provenance = "agent:claude-opus-5-5"
fields = [
    { offset = 0, size = 2, name = "x", type = "i16" },
    { offset = 2, size = 2, name = "y", type = "i16" },
]

[[enum]]
name = "direction"
size = 2
values = { north = 0, east = 1 }

[[pointer]]                     # a pointer type (D72)
name = "actor_ptr"
kind = "near"                   # an offset of `size` bytes from `base`; "far": offset, then segment;
                                # "segment": a segment alone (2 bytes), pointing at its start
size = 2
base = "GAME.EXE+20000"         # a near pointer's: the ID its offset 0 is (its segment's start)
into = "actors"                 # optional: named data, an array: read as the element's index

[[bits]]                        # packed bits (D72)
name = "status"
size = 1
fields = [
    { bit = 0, name = "moving", type = "bool" },
    { bit = 1, width = 2, name = "facing" },   # an unsigned number of `width` bits
]
```

A type's name is unique and isn't a built-in one (the numbers, `bool`, `far`, `segment`). A far pointer
(`far`, or a `[[pointer]]` of kind `far`) is 4 bytes, segment × 16 + offset (real mode); a `segment` is 2
bytes, pointing at segment × 16 (a block DOS allocated, or a table's segment), and `via` one reaches the
block, with `offset` into it.

## `knowledge/ticks.toml`: where ticks end and what they read

```toml
[[sync]]                        # a sync point: where a tick ends (D47)
name = "play"
at = "GAME.EXE+0A31C"
provenance = "agent:claude-opus-5-5"
confidence = 0.8
evidence = "rrc calls first-level: once a pass; notes/loops.md"
note = "where wait_for_tick returns: the wait is over and the pass hasn't read its inputs yet"

[[sync]]                        # a loop that polls with no wait (D48)
name = "menu"
at = "GAME.EXE+0B1F0"
when_changed = ["keys", "timer_count"]  # inputs or named data (with a size), by name

[[input]]                       # a boundary value: what the original's logic reads at each tick (D15)
name = "tick_elapsed"
at = "GAME.EXE+21C40"
size = 2
note = "timer counts since the last pass (70 Hz)"

[[input]]                       # a device's state instead of memory (D49)
name = "mouse"
device = "mouse"                # or "joystick"; the size is the device's
sync = ["menu"]                 # optional: read only at these sync points' ticks

[[input]]                       # what a part of the program not ported writes (D96)
name = "view_pick"
at = "GAME.EXE+4855E"
size = 2
part = "the 3D view"            # what writes it: its writes aren't followed
read_at = ["GAME.EXE+22DBB"]    # the ported code's reads of it, where the trace keeps what each saw

[[hook]]                        # a call a part not ported makes into ported code (D98)
name = "view_wakes"
at = "GAME.EXE+25F11"           # the entry; each run of it in a pass, outside interrupt handlers
part = "the 3D view"
registers = ["ax", "bx", "cl", "di"]  # what it takes: 32-, 16- and 8-bit names, segments, flags
```

`rrc sync` and `rrc input` write these. Each item may carry `provenance`, `confidence` and `evidence`, as the
tables' items do, and a `note` saying what it is. rrc writes a knowledge file only when its items changed, and
keeps the comments at the top of `ticks.toml` and `types.toml`; comments elsewhere are lost then, so explain
an item in its fields, and at length in `notes/`. A tick's inputs and state are what the memory holds at the
sync point: what the pass starting there reads.
With `when_changed`, a hit is a tick only when one of those items differs from what it was at this sync
point's last tick (the first hit is always one): list what the loop reads, and the counter its timing reads.

A device's state is bytes, like memory (little-endian):
- `mouse`, 46 bytes, what the mouse driver would report at the sync point, as 16-bit words: the buttons
  (INT 33h 03h's BX: bit 0 left, 1 right, 2 middle), X and Y (03h's CX and DX), the motion in mickeys since
  the last 0Bh call (0Bh's CX and DX, signed), then for the left, right and middle button its press count
  and the position of its last press (05h's BX, CX, DX), then the same for releases (06h).
- `joystick`, 12 bytes: for stick A, then B, whether it is connected (a byte, 1 or 0), its buttons (a byte:
  bit 0 button 1, bit 1 button 2, set while pressed), X and Y (signed 16-bit, -32767 to 32767).

## `knowledge/formats.toml`: the game's file formats

Each kind of file or archive member (D78, D103), which `rrc asset` decodes by and `rrc check` checks:

```toml
[[format]]
name = "archive"
files = ["VAULT.DAT"]               # names in the game's folder, `*` and `?` as DOS has them, any case
kind = "archive"                    # archive, chunks, picture, palette, map or data
header = "archive_header"           # a structure of types.toml at its start
directory = 6                       # where its directory (or its first chunk) begins
count = "header.count"              # how many entries: a number, or a field of the header
entry = "entry"                     # the entry's structure: name, offset, size; packed and method if coded
methods = { 0 = "stored", 1 = "runs", 2 = "lzss" }
read_by = "VAULT.EXE+0021F"         # the function that reads it
provenance = "map"
evidence = "main reads the directory; load (+0021F) a member"

[[format]]
name = "title"
files = ["VAULT.DAT/TITLE"]         # an archive's members: FILE/MEMBER, `#2` for the second of a name
kind = "picture"
width = 320                         # numbers may name the header's fields: "header.width"
height = 200
palette = "VAULT.DAT/PALETTE"       # a file or member of kind palette (`bits`: 6, the DAC's, or 8)
```

- **Kinds:** `archive`, a directory of `count` entries of type `entry` from `directory`, whose fields `name`
  (characters to the first NUL), `offset` and `size` place each member, and `packed` (its bytes in the file)
  and `method` (its codec, by `methods`) when they are coded; `chunks`, tagged chunks from `directory` (0),
  each a tag of `tag` bytes (4), its length (`length`: `u16` or `u32`) and that many bytes, a member named by
  its tag; `picture`, `width` × `height` palette indices shown with `palette`; `palette`, colours of three
  bytes; `map`, `width` × `height` cells; `data`, anything else.
- **Coding:** `coded` is how the bytes past the header are coded, decoded to `size` bytes (else `width` ×
  `height`, else as many as the codec gives). A codec is `stored`, `runs` (PCX's run-length code) or `lzss`,
  or one with its parameters, `{codec = "lzss", ring = 4096, fill = 0, start = 4078, shortest = 3}` (those not
  given are lzss.c's): the runtime's codecs (`rrc_runtime::codecs`), which a port decodes with (D101).
- A member of an archive is decoded by the archive's method, then by its own format.

## `mapping.toml`: the port's state and the original's

```toml
[[map]]
field = "player.x"              # a path in the port's serialized state: its field names, joined by .
at = "player_x"                 # the original's item: a data name, or an ID
type = "i16"                    # optional: how to read it, else data.tsv's type for it

[[map]]
field = "flags"
at = "world_flags"
mask = 0xFFFD                   # optional: the bits of an unsigned number that are the field (D95)

[[map]]
field = "level.cells"
via = "level_map"               # or: a pointer (named data, an ID, a path into one: world.level); the item
offset = 0                      # is where it points at each tick, this many bytes past (optional)
type = "u8[4096]"               # needed: what it points to

[[ignore]]                      # the original's memory that may differ (padding, caches, driver internals)
at = "mouse_buffer"             # a data name, or an ID
size = 0                        # bytes; 0: the data's size
note = "the mouse driver's"
```

The original's bytes are read as the type (D62, D72): `u8`, `i8`, `u16`, `i16`, `u32`, `i32` as numbers
(little-endian), `bool` as true (not 0) or false, `T[N]` as an array (`T[N][M]` is M arrays of `T[N]`), a
structure of `types.toml` as an object of its fields (a field with no type: an unsigned number of its size, or
bytes), an enumeration as its number, packed bits as an object of their fields, a pointer as null (0), the
index of the element it points to (`into`), or the ID it points to (`GAME.EXE+20104`; a far pointer whose ID
isn't known, `SEGMENT:OFFSET`).
The port's state, serialized as JSON, has the same numbers, arrays and objects at the mapped paths; what it has
beyond them is its own. A mapped item is named data of the type's size, or an ID; an input can't be mapped (the
port gets it with each tick's inputs). An item `via` a pointer is found at each tick: null while the pointer is.
A `mask` (on a `u8`, `u16` or `u32`) keeps those bits of the original's value: the others are a part's the port
doesn't have (a renderer's scratch in a flags word), so neither the transplant nor the comparison takes them,
and the comparison takes the port's value with the same mask.
`rrc record` traces every mapped item (one that isn't named data under its field's name, and an item via a
pointer as `*POINTER`, with the pointer). `rrc check` checks the mapping.

## `quirks.toml`: the quirk register

```toml
[[quirk]]                       # an original bug the faithful port keeps (DESIGN.md §6.5, D74)
name = "enemy_overrun"          # how the port's code names it: if rrc_runtime::keeps("enemy_overrun") { ... }
at = "GAME.EXE+002FF"           # the original's code that does it
what = "the sixth enemy's update reads the word after the table as its hits"
fix = "reads 0 instead"         # optional: what fixing it does
provenance = "agent:claude-opus-5-5"
evidence = "rrc suspects play: past enemies by GAME.EXE+002FF, offset 60"
```

`rrc quirk` writes it, sorted by name. Names are names, each once; `at` is an ID of the game's program.

## `sessions/<name>.toml`: a session

```toml
description = "the first level, up the main corridor"
inputs = "walk.inputs"          # an input script (clockwork run --input-file), beside it (rrc session add
                                # copies it here: edit this copy); or
# replay = "play.session"       # play recorded by clockwork play --record
seconds = 86.0                  # emulated seconds it runs
options = []                    # optional: more `clockwork run` options (rrc session --clockwork)

[trace]                         # written by rrc record
hash = "6e7f…"                  # the trace, in the cache
memory = "2b9a…"                # the data segment at every tick of its run, in the cache (D92)
run = "31ff…"                   # the run: what the machine did, whatever the knowledge read (D60)
ticks = 2770
clockwork = "0.1.0"             # the Clockwork that recorded it

[[edit]]                        # a scenario's edit (D64): rrc session edit writes these, or by hand
tick = 100                      # at this tick's sync point, before its pass reads
at = "enemies[2]"               # named data, or an ID, with a path into it as its type reads
type = ""                       # optional: how to read it, else data.tsv's type
note = "the third enemy gone"   # optional
value = { alive = 0 }           # a number, an array, some of a structure's fields, an enumeration's value name

[[pair]]                        # written by rrc compare --pair: a paired checkpoint (D63)
tick = 900
port = "9d5f…"                  # the port's state at the tick (rrc-port-state), in the cache
snapshot = "b9b3…"              # Clockwork's snapshot at the end of the tick, in the cache
```

Recording a session again must give the same run (D44, D60), and with the same knowledge the same trace:
`rrc record` says which of the two changed.

## Traces

JSON lines (D72): a header, a line per tick, and an end line.

```
{"format":"rrc-trace","version":6,"game":"game","session":"walk","program":"GAME.EXE","clockwork":"0.1.0",
 "syncs":[{"name":"play","at":"GAME.EXE+0A31C"},
          {"name":"menu","at":"GAME.EXE+0B1F0","when_changed":[{"name":"timer_count","at":"GAME.EXE+21C50","size":4}]}],
 "inputs":[{"name":"tick_elapsed","at":"GAME.EXE+21C40","size":2},
           {"name":"mouse","device":"mouse","size":46,"sync":["menu"]}],
 "state":[{"name":"door_state","at":"GAME.EXE+1E0A4","size":2}, …],
 "edits":[{"tick":100,"at":"GAME.EXE+1E0A4","hex":"0200","edit":"door_state = 2"}],
 "hooks":[{"name":"view_wakes","at":"GAME.EXE+25F11","registers":["ax","bx","cl","di"]}]}
{"tick":1,"sync":"play","cycle":20391734,"inputs":[[0,"0100"]],"state":[[0,"0000"], …],
 "frames":[[284,20380102,19,"61f5…","a3c0…"]],"sound":{"opl":[[2549120000,176,49]]}}
{"tick":2,"sync":"play","cycle":20504101,"late":[[0,"GAME.EXE+0A3B2","0300",1,[[0,3]]]],"frames":[[285,20496230,19,"61f5…","a3c0…"]]}
{"end":"the session's end","cycles":688000000,"frames":[[6020,687990122,19,"0e3a…","77d1…"]],"state_hash":"9c2c680ec5356ad6"}
```

- A tick ends when the original reaches one of the sync points (`knowledge/ticks.toml`), before that instruction
  runs; `sync` names which, `cycle` is Clockwork's instruction count then. At a gated sync point (its
  `when_changed` items in the header), `skipped` is how many hits it passed over since its last tick.
- `inputs`: `[index, value]` for each boundary value the tick reads (all of them, or those whose `sync` names
  the tick's sync point) that differs from what it was when last read: an input's value at a tick is the
  last one written. `state`: `[index, value]` for each named data item (`knowledge/data.tsv`, those with a
  size, but those that are inputs at every tick) that changed since the tick before; the first tick has them
  all. An item the mapping finds through a pointer (D72) has `via` in the header instead of `at`:
  `{"name":"*trail","size":64,"via":{"at":"GAME.EXE+01430","pointer":"far","size":4}}` (a near pointer's also
  has `base`, and `offset` the bytes past where it points); its value is read where the pointer points then. Values are the bytes memory holds, in hex (so little-endian; a device's state as its bytes); `null`
  where memory isn't mapped. Empty lists are left out.
- `edits` (in the header, only if the session has any): the scenario's writes, each at an ID with its bytes
  and the edit as the session has it (D64). A tick's state has its edits written.
- `hooks` (in the header, only if the knowledge has any): the calls a part not ported makes into ported code
  (D98); a tick's `hooks`, `[[index, [value, …]]]`, are those its pass made, in order, each with its
  registers' values as the header's hook lists them (version 9).
- `late`: `[index, ID, value, read, bytes]` for each read of the tick's pass that saw an input otherwise than
  the same instruction's read before it there (the first, otherwise than the tick's value): it changed after the
  sync point, before the program read it (D60). `ID` is the instruction (for a driver, the instruction after its
  call), `read` which of its reads of the input it was, from 1 (D93), and the value what that read saw: the
  tick's value with the bytes it read (D68); a read that saw the tick's value again, after one that didn't, is
  kept too. The program's own writes are followed, and so are its calls that change a driver's state (a mouse
  position set): `bytes`, `[[offset, value]]`, are the bytes the read saw otherwise than the program should have
  read them, the tick's value with its writes since (D97). Version 5 and 6 traces have no `read`: one entry an
  instruction, its first read; versions before 8 no `bytes`. Not counted: what interrupt
  handlers read, the wait that ends the next tick (an instruction whose late reads were all in the call the next
  hit comes in, the last at most 40 of the program's instructions before it), and memory a host service reads
  for the program (the BIOS keyboard buffer through INT 16h).
- `frames`: the frames Clockwork showed since the tick before (a display refresh each, 70 a second):
  `[number, cycle, BIOS video mode, fingerprint, picture]`, the picture's hash in the cache (below).
- `sound`: the sound events since the tick before, by device, only those with events: `opl` `[ns, register,
  value]`, `speaker` `[pit_tick, waveform]`, `pcm` (digitized sound starts) `[ns, rate, stereo]`, `midi`
  `[pit_tick, byte]`, `cd` `[ns, sector, sectors]`.
- The end line says why the run stopped (`the session's end`, at exactly its length, or the program exiting,
  halting or faulting) and when, the frames and sound after the last tick, and Clockwork's hash of the
  machine's state then.
- The run's hash (the session's `run`) is the SHA-256 of the JSON object `{"cycles", "end", "frames",
  "sound", "state_hash"}` with keys sorted and no spaces: every frame and the sound events of each device
  from the start to the end, in order, and the end line's fields. It doesn't depend on what the knowledge
  reads or where ticks end.

## Frame pictures

What a frame showed (D25), in the cache by SHA-256: raw deflate (RFC 1951) of
- `FRM1`, the width and the height (16-bit little-endian), the kind: 0 indexed, 1 RGB;
- indexed: the palette, 256 × R, G, B as the VGA's DAC holds them (0 to 63; entry `i` is the DAC's `i & pel
  mask`), then a byte a pixel;
- RGB (direct-colour modes): 3 bytes a pixel.

Pixels go row by row from the top left; text modes are drawn with their font, as the screen shows them.
`rrc frame SESSION --tick N` writes the picture shown by the end of a tick as a PNG.

## The port's files

`rrc compare` runs the port (`[port] command`) in the port repository with

```
run --inputs FILE --out FILE [--transplant FILE | --resume FILE] [--save-at TICK --save FILE] [--to TICK]
```

and its files in `.rrc/compare/<session>/` (D61). The port exits 0 when it ran; what it writes to stderr is
shown when it fails. Its environment has the person's copy of the game (D82): `RRC_GAME`, the game's folder,
and `RRC_PROGRAM`, the program as the analysis unpacked it (an MZ file; its load image is what IDs' offsets
count in), for the static data the port reads (`rrc_runtime::original`); run without them (its own tests), the
runtime finds both from the port repository it runs in (D91). The state at tick t is what the original holds at its t-th sync point hit; a step with tick
t's inputs makes tick t+1's. The port runs from the first tick (1, or the transplant's or the resumed state's)
to the inputs' last (or `--to`'s), writing its state at each tick before stepping. All files are JSON; the
inputs and the trace are JSON lines.

- **Inputs** (`--inputs`, written by `rrc`): the header
  `{"format":"rrc-inputs","version":1,"session":"play","inputs":[{"name":"timer_count","size":2},{"name":"mouse","size":46,"device":"mouse"}]}`,
  then a line per tick, `{"tick":1,"sync":"play","inputs":[[0,"0100"],[1,"0000…"]]}`: the inputs the tick reads
  whose value changed since they were last read (index in the header's list, bytes in hex as the trace has
  them); a value holds until it changes. A tick whose pass read inputs late has `late`,
  `[[index, "GAME.EXE+04BB9", "0300", 1, [[0, 3]]]]`: what each read saw, which of that instruction's reads it
  was (D68, D93), and its bytes that differ from what the program should have read (D97), which the runtime's
  `Inputs::at_site(ID)` gives the port, a call a read, over what the port wrote (`Inputs::write`). With hooks
  (D98) the header has `"hooks":[{"name":"view_wakes","registers":["ax","cl"]}]` and a tick whose pass made
  calls through them `hooks`, `[[0, [1472, 5]]]` (the hook's index, the registers' values), which
  `Inputs::calls(name)` gives the port. A tick with a scenario's edits has `set`, `{"enemies[2].alive":0}`: the
  mapped fields the edits wrote, by path (field names and `[index]`es), with the original's values, which the
  port sets in its state before writing the tick (D64). The last line is
  `{"end":"the session's end","ticks":1400}`.
- **Start state** (`--transplant`, written by `rrc`):
  `{"format":"rrc-state","version":1,"session":"play","tick":500,"state":{"player.x":2560,"enemies":[…]}}`, each
  mapped field's value at the tick by its path. The runtime sets each over the port's start state's
  serialization and deserializes that (`Simulation::transplant` may build the state otherwise).
- **Port state** (`--save` writes it at `--save-at`'s tick, `--resume` reads it):
  `{"format":"rrc-port-state","version":1,"tick":900,"state":{…}}`, the port's whole state as it serializes it,
  at the tick (before its step).
- **Port trace** (`--out`, written by the port): the header
  `{"format":"rrc-port-trace","version":1,"session":"play","from":1}`, then a line per tick,
  `{"tick":1,"state":{…}}`, with the top-level fields of the state that changed since the line before (all at the
  first), and last `{"end":1400}`. A mapped field the port hasn't set yet is `null`: it matches while the
  original's value is the one at the first tick (D102).
