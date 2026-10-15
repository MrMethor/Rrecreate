# testbins/

Synthetic DOS test programs with known source, used to test the host and the pipeline
against ground truth (PROJECT.md §13, Phase 0 / calibration step 1).

Planned: small C and assembly programs built with Open Watcom v2 for 16-bit DOS, each
exercising one feature (calling conventions, far pointers, INT 21h file I/O, mode 13h,
PIT/IRQ0, packers). Built EXEs are committed next to their source so tests don't need
the compiler installed.
