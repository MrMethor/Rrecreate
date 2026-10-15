//! DOS, BIOS and PC device services as high-level emulation (PROJECT.md §5.2, "HLE services").
//!
//! Phase 1 scope: INT 21h file subset, PSP/environment, INT 10h text + mode 13h, PIT/PIC/IRQ0,
//! keyboard and VGA palette. These services become the native port's runtime later (§7.3).
