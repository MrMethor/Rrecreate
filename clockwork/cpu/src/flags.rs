//! EFLAGS register bits. The low sixteen are `u16` (FLAGS); the 80386 additions are `u32`.

pub const CF: u16 = 0x0001;
pub const PF: u16 = 0x0004;
pub const AF: u16 = 0x0010;
pub const ZF: u16 = 0x0040;
pub const SF: u16 = 0x0080;
pub const TF: u16 = 0x0100;
pub const IF: u16 = 0x0200;
pub const DF: u16 = 0x0400;
pub const OF: u16 = 0x0800;
/// I/O privilege level (two bits) and nested task: writable in real mode from the 80386 on.
pub const IOPL: u16 = 0x3000;
pub const NT: u16 = 0x4000;

/// Resume flag: suppresses debug faults for one instruction.
pub const RF: u32 = 0x1_0000;
/// Virtual-8086 mode.
pub const VM: u32 = 0x2_0000;

/// Bit 1 always reads as 1.
pub const FIXED: u16 = 0x0002;

/// Bits software can change in 80286 real mode. Bits 3, 5 and 12–15 always read as 0.
pub const WRITABLE: u16 = CF | PF | AF | ZF | SF | TF | IF | DF | OF;

/// Bits software can change in 80386 real mode: IOPL and NT as well.
pub const WRITABLE_386: u16 = WRITABLE | IOPL | NT;

/// The value FLAGS takes on an 80286 when software loads `value` into it (POPF, IRET, SAHF).
pub const fn normalize(value: u16) -> u16 {
    (value & WRITABLE) | FIXED
}

/// The same for an 80386.
pub const fn normalize_386(value: u16) -> u16 {
    (value & WRITABLE_386) | FIXED
}

/// Arithmetic flags (DOSBox `FMASK_TEST`).
pub const ARITHMETIC: u32 = (CF | PF | AF | ZF | SF | OF) as u32;
/// What POPF/IRET may change at CPL > 0 (DOSBox `FMASK_NORMAL`).
pub const NORMAL: u32 = ARITHMETIC | (DF | TF | IF) as u32;
/// What POPF/IRET may change at CPL 0 or in real mode (DOSBox `FMASK_ALL`).
pub const ALL: u32 = NORMAL | (IOPL | NT) as u32;
