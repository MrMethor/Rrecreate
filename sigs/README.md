# sigs/

Library signature packs: compiler runtime functions known by their Ghidra Function ID hashes, used for
library identification during analysis (`DESIGN.md` §5): a function whose hashes match one name is marked as
a library function and named after it (provenance `library:<pack>`), so nobody reimplements it. Functions
under 6 code units aren't matched (short ones hash alike too easily), nor a hash several names share.

A pack is a JSON file: `name`, `compiler`, `source` (where the hashes came from) and `functions`, each with
its name, code unit count, full hash and specific hash. Packs hold hashes and names only, no code. They are
made from an analyzed binary and the linker map of the same build, which says which functions came from
libraries (Borland: modules without a file extension, and the C0 startup; Watcom: `x.lib(y)` modules, 16-
or 32-bit). The Borland pack was made by Recreate's `rec sigs build`; `rrc sigs build` makes them now:

```bash
rrc init clib-port --program CLIB32.EXE --name clib32   # a port of the test program
rrc game testbins/bin --add-version ow2 && rrc analyze
rrc sigs name testbins/bin/CLIB32.MAP                    # the map's names, so analyze adds what Ghidra missed
rrc analyze
rrc sigs build testbins/bin/CLIB32.MAP --name openwatcom-2-clib3r-flat --compiler "..." --source "..."
```

A pack goes into `rrc` itself (`PACKS` in `rrc/src/analysis.rs`), so ports need no checkout of Rrecreate. A
runtime is best taken from a program built with the same compiler version as the game: a newer version's
runtime is compiled differently, and few of its hashes match.

| Pack | Functions | From |
|---|---|---|
| `borland-c++-1991-medium.json` | 146 | id Software's Wolfenstein 3D build and map (source release); finds 112 runtime functions in the shareware `WOLF3D.EXE` |
| `openwatcom-2-clib3r-flat.json` | 386 | `testbins/CLIB32.EXE` (Open Watcom 2, 32-bit flat, register calls) and its map; finds 331 of them in `CLIB32.EXE` itself, and 9 runtime functions in Daggerfall's `FALL.EXE` (Watcom C/C++32 10, 1994): `strcpy_`, `vsprintf_`, `toupper_`, ... |
