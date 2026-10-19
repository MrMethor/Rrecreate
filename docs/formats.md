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

`files.tsv` holds the hashes of the game's files with their size and modification time, so `rrc` hashes a file
again only when it changed.

## The cache

A folder of files named by their SHA-256: `objects/<first 2 hex digits>/<the other 62>`. It is `RRC_CACHE`,
else `cache` in `rrc.local.toml`, else `%LOCALAPPDATA%\rrecreate\cache` on Windows and `$XDG_CACHE_HOME/rrecreate`
or `~/.cache/rrecreate` elsewhere. Anything in it can be deleted; what refers to it is rebuilt or recorded again.
