# testbins/

Synthetic DOS test programs with known source, used to test Clockwork and the analysis against ground
truth.

Small C programs built with Open Watcom v2 for 16-bit DOS, each exercising one thing:

| Program | Model | What it exercises |
|---|---|---|
| `HELLO.EXE` | small | C startup, `printf`, an exit code |
| `CALLCONV.EXE` | small | cdecl, pascal and Watcom register calls; a long multiply in a runtime helper |
| `FARDATA.EXE` | large | far pointers and heap, a 40 000-byte block, `qsort` calling back through a pointer |
| `FILEIO.EXE` | small | create, write, seek, read, rename and delete through the C runtime |
| `MODE13.EXE` | small | mode 13h through the BIOS, the DAC ports, drawing through a far pointer |
| `TIMER.EXE` | small | an `__interrupt` handler on INT 08h, the PIT at 1000 Hz |
| `FLOAT.EXE` | small | 8087 code (`-fpi87`): arithmetic, `sqrt`, `sin`, `exp`, `printf("%f")` |
| `MEDIUM.EXE` | medium | two code segments with far calls, a switch compiled to a jump table, recursion |
| `TOY.EXE` | small | a toy game to port (M3, M4, `examples/toy/`): a 70 Hz timer and a keyboard handler of its own, a player, six enemies, a 32-bit random generator, a pointer to the nearest enemy, a trail on the far heap, packed status bits, text mode |
| `VAULT.EXE` | small | a game that loads its data (M5): `VAULT.DAT`, an archive of named members, stored, run-length coded (PCX's code) or LZSS coded (Okumura's); the title picture and its palette in mode 13h, then two levels' maps decoded onto the heap, the second loaded mid-play; two loops (the title, play); the drawing keeps what it drew and a blink's count (M6, D110) |
| `CHIME.EXE` | small | sound (M6): Space plays a note on the PC speaker (the timer's channel 2) and the OPL's channel 0 for 20 passes, Up and Down choose it; a note played while one sounds cuts it off; one loop |
| `FLAT32.EXE` | flat, 32-bit | an LE executable under the CauseWay extender: code and data objects the extender places, a call through a pointer |
| `FLAT4GW.EXE` | flat, 32-bit | the same under DOS/4GW: its stub runs `DOS4GW.EXE`, which loads it; it waits a quarter of a second before and after its work |
| `CLIB32.EXE` | flat, 32-bit | Open Watcom's 32-bit C runtime (`clib3r`, math, the 387 emulator) linked in by a table of 190 functions' addresses, for its signature pack |

`DOS4GW.EXE` (Rational Systems' DOS/4GW, in Open Watcom's `binw`) isn't committed: nothing says it may be
redistributed. The tests of `FLAT4GW.EXE` take it from the local Open Watcom (`WATCOM`) and skip without it,
so CI doesn't run them; run them with `WATCOM` set after changing the extender support.

`src/` has the sources, `bin/` the programs and their linker maps (`NAME.MAP`: every function's
segment:offset, the ground truth for function discovery and naming). The built files are
committed, so tests don't need the compiler; `clockwork/dos/tests/testbins.rs` runs each program on
Clockwork and checks its output.

`VAULT.DAT` is written by `python testbins/data.py` (the same bytes at every run), which encodes the members
with the codes `VAULT.EXE` decodes.

To rebuild (the EXEs come out byte for byte the same; the maps' "Created on" line changes):

```bash
WATCOM=C:/path/to/openwatcom sh testbins/build.sh
```

Not covered yet: packed executables (LZEXE, PKLITE), overlays, Borland and Microsoft compilers.
