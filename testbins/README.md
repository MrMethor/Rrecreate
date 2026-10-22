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

To rebuild (the EXEs come out byte for byte the same; the maps' "Created on" line changes):

```bash
WATCOM=C:/path/to/openwatcom sh testbins/build.sh
```

Not covered yet: packed executables (LZEXE, PKLITE), overlays, Borland and Microsoft compilers.
