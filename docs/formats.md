# File formats

The files a port repository holds and the toolkit writes (DESIGN.md §9). A port in any language reads and
writes these to be compared (D38). Text is UTF-8 with LF line ends.

## `rrc.toml`: the port

```toml
[game]
name = "wolf3d"                 # short, for file names and reports
title = "Wolfenstein 3-D"       # optional

[original]
program = "WOLF3D.EXE"          # as `clockwork run` takes it, in the game's folder
options = ["--ems", "0"]        # optional: more `clockwork run` options (CD drive, current folder, ...)

[[version]]                     # one per version the port supports (D30)
name = "shareware 1.4"
[version.files]                 # each file by SHA-256 (lowercase hex); paths are `/`-separated
"WOLF3D.EXE" = "6d8f…"
```

A copy of the game is a version when it holds every file the version lists with that hash (paths compare
without case, as DOS's do); other files in the folder don't matter. The module part of the program's IDs is
its file name, uppercase (`WOLF3D.EXE`).

A port repository also has `.gitignore` (`rrc.local.toml`, `.rrc/`, `target/`) and `.gitattributes` (text with
LF line ends on every platform).

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
  cache), the entry point's offset in the load image (`entry`, which places Ghidra's addresses in the image) and
  Ghidra's version.
- `ghidra/`: the last analysis's working files: the program Ghidra imported, the export script, Ghidra's log.

## Ghidra's export

JSON lines from `ghidra/RecExport.java`: a `program` record (name, language, compiler, `entry`, Ghidra's
version), `block` records (memory blocks), `function` records (`address`, `name`, `source`, `convention`,
`signature`, `thunk`, `fid`: Function ID hashes `{units, full, specific, extra}`, `body`: `[address, length]`
ranges), `call` records (`caller`, `site`, `callee`) and `code` records (`address`, `asm`, `c`). Addresses are
`{"linear": n, "text": "SSSS:OOOO"}`; an address's image offset is its linear address minus the entry point's,
plus `entry`.

## The cache

A folder of files named by their SHA-256: `objects/<first 2 hex digits>/<the other 62>`; and `index/<kind>/<key>`,
what an earlier run made from given inputs (`index/analysis/<key>`: the `analysis.toml` of a program analysed by
the same export script, keyed by the program's hash, the script and whether it decompiled). It is `RRC_CACHE`,
else `cache` in `rrc.local.toml`, else `%LOCALAPPDATA%\rrecreate\cache` on Windows and `$XDG_CACHE_HOME/rrecreate`
or `~/.cache/rrecreate` elsewhere. Anything in it can be deleted; what refers to it is rebuilt or recorded again.

## `knowledge/functions.tsv` and `knowledge/data.tsv`: named functions and data

Tab-separated, a header line starting `#`, then one item per line sorted by ID (D43):

```
# id	name	provenance	confidence	evidence
WOLF3D.EXE+0F280	CalcTics	agent:claude-opus-5-5	0.8	called once per frame from PlayLoop
```

```
# id	name	size	type	provenance	confidence	evidence
WOLF3D.EXE+4114C	tics	2	u16	human
```

- **id:** `<MODULE>[#<n>]+<offset>` (D29), the offset in the loaded image, uppercase hex, 5 to 8 digits.
- **name:** letters, digits, `_`, and the `@` and `$` of compilers' runtime names, not starting with a digit;
  unique across both tables.
- **size** (data): bytes, or empty when unknown. **type** (data): a type from `types.toml`, a primitive (`u8`,
  `i16`, `u32`, ...), an array (`u8[256]`), or empty.
- **provenance:** who decided, and from what: `human`, `agent:<model>`, `ghidra`, `library:<pack>`,
  `prior-art:<source>`, `recreate:<its provenance>` for what came from Recreate.
- **confidence:** 0 to 1, or empty. **evidence:** free text (IDs, sessions, sources), no tabs.

Trailing empty columns may be left out. Lines starting `#` and blank lines are ignored.

## `knowledge/types.toml`: structures and enumerations

```toml
[[struct]]
name = "objstruct"
size = 60
provenance = "agent:claude-opus-5-5"
fields = [
    { offset = 0, size = 2, name = "active", type = "u16" },
    { offset = 2, size = 2, name = "ticcount", type = "i16" },
]

[[enum]]
name = "dirtype"
size = 2
values = { east = 0, northeast = 1 }
```

## `knowledge/ticks.toml`: where ticks end and what they read

```toml
[[sync]]                        # a sync point: the instruction where a tick ends (one per loop)
name = "play"
at = "WOLF3D.EXE+0F280"
note = "CalcTics: once per frame of play"

[[input]]                       # a boundary value: what the original's logic reads at each tick (D15)
name = "tics"
at = "WOLF3D.EXE+4114C"
size = 2
note = "time since the last frame, in 70 Hz tics"
```

## `sessions/<name>.toml`: a session

```toml
description = "E1M1 up the main corridor, the pushwall pushed"
inputs = "pushwall.inputs"      # an input script (clockwork run --input-file), beside it; or
# replay = "play.session"       # play recorded by clockwork play --record
seconds = 86.0                  # emulated seconds it runs
options = []                    # optional: more `clockwork run` options

[trace]                         # written by rrc record
hash = "6e7f…"                  # the trace, in the cache
ticks = 2770
clockwork = "0.1.0"             # the Clockwork that recorded it
```

Recording a session again must give the same hash (D44): `rrc record` says whether it did.

## Traces

JSON lines (D45): a header, a line per tick, and an end line.

```
{"format":"rrc-trace","version":1,"game":"wolf3d","session":"pushwall","program":"WOLF3D.EXE","clockwork":"0.1.0",
 "syncs":[{"name":"play","at":"WOLF3D.EXE+0F280"}],
 "inputs":[{"name":"tics","at":"WOLF3D.EXE+4114C","size":2}],
 "state":[{"name":"pwallstate","at":"WOLF3D.EXE+3C174","size":2}, …]}
{"tick":1,"sync":"play","cycle":20391734,"inputs":["0100"],"state":[[0,"0000"], …],"frames":[[284,20380102,19,"61f5…"]],
 "sound":{"opl":[[2549120000,176,49]]}}
{"end":"the session's end","cycles":688003448}
```

- A tick ends when the original reaches one of the sync points (`knowledge/ticks.toml`), before that instruction
  runs; `sync` names which, `cycle` is Clockwork's instruction count then.
- `inputs`: every boundary value at every tick, in the header's order. `state`: `[index, value]` for each named
  data item (`knowledge/data.tsv`, those with a size) that changed since the tick before; the first tick has
  them all. Values are the bytes memory holds, in hex (so little-endian); `null` where memory isn't mapped.
- `frames`: the frames Clockwork showed since the tick before: `[number, cycle, BIOS video mode, hash]`.
- `sound`: the sound events since the tick before, by device, only those with events: `opl` `[ns, register,
  value]`, `speaker` `[pit_tick, waveform]`, `pcm` (digitized sound starts) `[ns, rate, stereo]`, `midi`
  `[pit_tick, byte]`, `cd` `[ns, sector, sectors]`.
- The end line says why the run stopped: `the session's end`, or the program exiting, halting or faulting.
