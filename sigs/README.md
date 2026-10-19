# sigs/

Library signature packs: compiler runtime functions known by their Ghidra Function ID hashes, used for
library identification during analysis (`DESIGN.md` §5): a function whose hashes match one name is marked as
a library function and named after it (provenance `library-id`), so nobody reimplements it.

A pack is a JSON file: `name`, `compiler`, `source` (where the hashes came from) and `functions`, each with
its name, code unit count, full hash and specific hash. Packs hold hashes and names only, no code. They are
made from an analyzed binary and the linker map of the same build, which says which functions came from
libraries (Borland: modules without a file extension, and the C0 startup; Watcom: `x.lib(y)` modules). The
pack below was made by Recreate's `rec sigs build`; Rrecreate's toolkit takes that over in M2
(`DESIGN.md` §12).

| Pack | Functions | From |
|---|---|---|
| `borland-c++-1991-medium.json` | 146 | id Software's Wolfenstein 3D build and map (source release); finds 112 runtime functions in the shareware `WOLF3D.EXE` |
