//! Deterministic x86 CPU interpreter for the Recreate host (PROJECT.md §5.2).
//!
//! Two models are emulated:
//! - [`Model::I286`]: the 8086/80186 instruction set with 80286 behavior in real mode: shift and
//!   rotate counts masked to 5 bits, `PUSH SP` pushes the value before the push, FLAGS bits
//!   12–15 always read as 0, divide errors return to the faulting instruction, `0F` isn't
//!   `POP CS`. Entering protected mode stops the run with [`Exit::Unsupported`].
//! - [`Model::I386`] (the default): adds 32-bit registers and operands, FS and GS, the operand-
//!   and address-size prefixes, 32-bit addressing, the 80386 two-byte opcodes, and protected
//!   mode: descriptor tables, privilege levels, call and interrupt gates, task switches,
//!   virtual-8086 mode and paging (see `protected.rs` and `paging.rs`, ported from DOSBox
//!   Staging). Segment limits aren't enforced, as in DOSBox.
//!
//! There is no FPU: escape opcodes decode their operands and do nothing, which is how a machine
//! without a coprocessor behaves.
//!
//! The interpreter is deterministic. Its state is plain data, so `Clone` is a snapshot, and
//! time is counted in [`Cpu::cycles`], never read from the host clock. Memory and port I/O go
//! through the [`Bus`] trait, which the machine implements. Faults (page faults, protection
//! errors) roll the instruction back and enter the exception handler, as on the real CPU.
//!
//! The host's high-level emulation (HLE) is entered through a reserved opcode,
//! `FE 38 <id:u16>` (an invalid encoding of `INC/DEC r/m8`). Executing it makes [`Cpu::step`]
//! return [`Exit::Callback`]. BIOS and DOS entry points in the emulated ROM are such callbacks
//! followed by ordinary code like `IRET`.

mod exec;
pub mod flags;
mod paging;
mod protected;
mod regs;

use std::fmt;

pub use regs::{Regs, reg, seg};

/// First two bytes of the host callback instruction; a little-endian `u16` id follows.
pub const CALLBACK_OPCODE: [u8; 2] = [0xFE, 0x38];

/// Memory and I/O as seen by the CPU. Addresses are physical (after segmentation and paging);
/// the bus applies A20 masking and memory-mapped devices.
pub trait Bus {
    fn read_u8(&mut self, addr: u32) -> u8;
    fn write_u8(&mut self, addr: u32, value: u8);
    fn port_in_u8(&mut self, port: u16) -> u8;
    fn port_out_u8(&mut self, port: u16, value: u8);

    fn port_in_u16(&mut self, port: u16) -> u16 {
        u16::from(self.port_in_u8(port)) | (u16::from(self.port_in_u8(port.wrapping_add(1))) << 8)
    }

    fn port_out_u16(&mut self, port: u16, value: u16) {
        self.port_out_u8(port, value as u8);
        self.port_out_u8(port.wrapping_add(1), (value >> 8) as u8);
    }

    fn port_in_u32(&mut self, port: u16) -> u32 {
        u32::from(self.port_in_u16(port)) | (u32::from(self.port_in_u16(port.wrapping_add(2))) << 16)
    }

    fn port_out_u32(&mut self, port: u16, value: u32) {
        self.port_out_u16(port, value as u16);
        self.port_out_u16(port.wrapping_add(2), (value >> 16) as u16);
    }
}

/// Which processor to behave like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Model {
    I286,
    #[default]
    I386,
}

/// Why [`Cpu::step`] stopped before completing an ordinary instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// The callback instruction ran; CS:IP points past it.
    Callback(u16),
    /// `HLT` ran; CS:IP points past it.
    Halt,
    /// The instruction isn't implemented (or is invalid in real mode). CS:IP still point at it
    /// and nothing was changed.
    Unimplemented { cs: u16, ip: u32, bytes: [u8; 6] },
    /// The instruction is understood but asks for something the host doesn't emulate, or the
    /// CPU reached a state it can't continue from (a triple fault). CS:IP still point at it.
    Unsupported { cs: u16, ip: u32, feature: &'static str },
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Exit::Callback(id) => write!(f, "host callback {id:#06x}"),
            Exit::Halt => f.write_str("HLT"),
            Exit::Unimplemented { cs, ip, bytes } => {
                write!(f, "unimplemented instruction at {cs:04X}:{ip:04X} (bytes")?;
                for b in bytes {
                    write!(f, " {b:02X}")?;
                }
                f.write_str(")")
            }
            Exit::Unsupported { cs, ip, feature } => write!(f, "{feature} at {cs:04X}:{ip:04X} is not supported"),
        }
    }
}

/// Exception vectors.
pub mod vector {
    pub const DIVIDE: u8 = 0;
    pub const DEBUG: u8 = 1;
    pub const BREAKPOINT: u8 = 3;
    pub const OVERFLOW: u8 = 4;
    pub const BOUND: u8 = 5;
    pub const INVALID_OPCODE: u8 = 6;
    pub const DOUBLE_FAULT: u8 = 8;
    pub const INVALID_TSS: u8 = 10;
    pub const NOT_PRESENT: u8 = 11;
    pub const STACK: u8 = 12;
    pub const GENERAL_PROTECTION: u8 = 13;
    pub const PAGE_FAULT: u8 = 14;
}

/// An exception raised by an instruction: its vector and error code. Kept to eight bytes so
/// that fallible results return in registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fault {
    pub vector: u8,
    has_code: bool,
    error: u32,
}

impl Fault {
    pub fn new(vector: u8) -> Self {
        Self { vector, has_code: false, error: 0 }
    }

    pub fn with_code(vector: u8, error: u32) -> Self {
        Self { vector, has_code: true, error }
    }

    pub fn code(&self) -> Option<u32> {
        self.has_code.then_some(self.error)
    }
}

/// Why an instruction didn't complete: an exception to deliver, or a stop for the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Abort {
    Fault(Fault),
    /// Boxed so that results stay small enough to return in registers.
    Exit(Box<Exit>),
}

impl From<Fault> for Abort {
    fn from(fault: Fault) -> Self {
        Abort::Fault(fault)
    }
}

impl From<Exit> for Abort {
    fn from(exit: Exit) -> Self {
        Abort::Exit(Box::new(exit))
    }
}

pub(crate) type Res<T> = Result<T, Abort>;

/// Descriptor table register contents (LGDT/LIDT).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TableRegister {
    pub limit: u16,
    pub base: u32,
}

/// A segment register's hidden part, loaded from a descriptor in protected mode. The cache
/// remembers the selector it was loaded for: when the selector register changes without a
/// descriptor load (host code setting real-mode segments), real-mode addressing applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SegmentCache {
    pub selector: u16,
    pub base: u32,
    pub limit: u32,
    /// The D/B bit: a 32-bit code segment, or a stack addressed with ESP.
    pub big: bool,
}

/// LDTR or TR: a system segment selected from the GDT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SystemSegment {
    pub selector: u16,
    pub base: u32,
    pub limit: u32,
    /// A 32-bit (80386) TSS rather than a 16-bit one.
    pub is386: bool,
}

/// CPU state saved at the start of each protected-mode instruction, restored if it faults.
#[derive(Clone)]
struct Checkpoint {
    regs: Regs,
    segs: [SegmentCache; 6],
    cpl: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cpu {
    pub regs: Regs,
    pub model: Model,
    /// Control registers: CR0 (PE, PG, ...), CR2 (page-fault address), CR3 (page directory).
    pub cr0: u32,
    pub cr2: u32,
    pub cr3: u32,
    /// Debug registers (stored; breakpoints aren't emulated).
    pub dr: [u32; 8],
    pub gdtr: TableRegister,
    pub idtr: TableRegister,
    pub ldtr: SystemSegment,
    pub tr: SystemSegment,
    /// Current privilege level.
    pub cpl: u8,
    /// Hidden parts of ES, CS, SS, DS, FS, GS.
    pub segs: [SegmentCache; 6],
    pub(crate) tlb: paging::Tlb,
    /// Executed instructions, plus one per repetition of a REP-prefixed string instruction and
    /// one per interrupt taken. The machine derives virtual time from this.
    pub cycles: u64,
    /// Set by `STI`, `MOV SS` and `POP SS`: no interrupt is taken before the next instruction.
    irq_inhibit: bool,
    /// ESI, EDI and ECX of a REP string instruction that faulted part-way: it restarts there.
    string_restart: Option<(u32, u32, u32)>,
    /// Code fetch state for the current instruction (not part of the CPU's state).
    fetch: FetchState,
}

/// What instruction fetches need, computed once per instruction: CS base, EIP wrap mask, and
/// whether paging is on. Scratch data: equal and hashed alike whatever it holds.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FetchState {
    pub base: u32,
    pub ip_mask: u32,
    pub paged: bool,
}

impl PartialEq for FetchState {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for FetchState {}

impl std::hash::Hash for FetchState {
    fn hash<H: std::hash::Hasher>(&self, _: &mut H) {}
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

/// CR0 bits.
pub mod cr0 {
    pub const PE: u32 = 1 << 0;
    pub const ET: u32 = 1 << 4;
    pub const WP: u32 = 1 << 16;
    pub const PG: u32 = 1 << 31;
}

impl Cpu {
    /// An 80386 in real mode.
    pub fn new() -> Self {
        Self::with_model(Model::default())
    }

    pub fn with_model(model: Model) -> Self {
        Self {
            regs: Regs::default(),
            model,
            cr0: 0,
            cr2: 0,
            cr3: 0,
            dr: [0; 8],
            gdtr: TableRegister { limit: 0, base: 0 },
            idtr: TableRegister { limit: 0x3FF, base: 0 },
            ldtr: SystemSegment::default(),
            tr: SystemSegment::default(),
            cpl: 0,
            segs: [SegmentCache { selector: 0, base: 0, limit: 0xFFFF, big: false }; 6],
            tlb: paging::Tlb::default(),
            cycles: 0,
            irq_inhibit: false,
            string_restart: None,
            fetch: FetchState { base: 0, ip_mask: 0xFFFF, paged: false },
        }
    }

    /// Protected mode (CR0.PE), including virtual-8086 mode.
    #[inline]
    pub fn protected_mode(&self) -> bool {
        self.cr0 & cr0::PE != 0
    }

    /// Virtual-8086 mode (EFLAGS.VM in protected mode).
    pub fn v86(&self) -> bool {
        self.regs.flags & flags::VM != 0
    }

    /// Segments follow real-mode rules (base = selector × 16): real mode and V86 mode.
    pub(crate) fn real_addressing(&self) -> bool {
        !self.protected_mode() || self.v86()
    }

    /// Base address of segment register `seg`.
    #[inline]
    pub fn segment_base(&self, seg: usize) -> u32 {
        let cache = &self.segs[seg];
        if cache.selector == self.regs.seg[seg] { cache.base } else { u32::from(self.regs.seg[seg]) << 4 }
    }

    fn cache_big(&self, seg: usize) -> bool {
        let cache = &self.segs[seg];
        cache.selector == self.regs.seg[seg] && cache.big
    }

    /// The code segment is 32-bit (default operand and address size).
    pub fn code32(&self) -> bool {
        self.cache_big(seg::CS) && !self.v86()
    }

    /// The stack is addressed with ESP rather than SP.
    pub fn stack32(&self) -> bool {
        self.cache_big(seg::SS) && !self.v86()
    }

    /// Executes one instruction (with its prefixes). A REP-prefixed string instruction runs all
    /// its repetitions in one step. An instruction that faults is undone and its exception
    /// handler entered.
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> Result<(), Exit> {
        let trap = self.regs.flags & u32::from(flags::TF) != 0;
        self.irq_inhibit = false;
        let start_ip = self.regs.ip;
        // Only protected mode can fault mid-instruction, so only it pays for the checkpoint.
        let checkpoint = if self.protected_mode() {
            Some(Checkpoint { regs: self.regs, segs: self.segs, cpl: self.cpl })
        } else {
            None
        };
        match self.exec(bus, start_ip) {
            Ok(()) => {
                self.cycles += 1;
                if trap {
                    return self.raise(bus, Fault::new(vector::DEBUG), false);
                }
                Ok(())
            }
            Err(Abort::Exit(exit)) => {
                let exit = *exit;
                if matches!(exit, Exit::Callback(_) | Exit::Halt) {
                    self.cycles += 1;
                    return Err(exit);
                }
                // Unimplemented or unsupported: nothing happened, CS:IP point at the instruction.
                match checkpoint {
                    Some(saved) => {
                        self.regs = saved.regs;
                        self.segs = saved.segs;
                        self.cpl = saved.cpl;
                    }
                    None => self.regs.ip = start_ip,
                }
                Err(match exit {
                    Exit::Unsupported { feature, .. } => {
                        Exit::Unsupported { cs: self.regs.seg[seg::CS], ip: start_ip, feature }
                    }
                    other => other,
                })
            }
            Err(Abort::Fault(fault)) => {
                match checkpoint {
                    Some(saved) => {
                        self.regs = saved.regs;
                        self.segs = saved.segs;
                        self.cpl = saved.cpl;
                    }
                    None => self.regs.ip = start_ip,
                }
                if let Some((si, di, cx)) = self.string_restart.take() {
                    self.regs.gpr[reg::SI] = si;
                    self.regs.gpr[reg::DI] = di;
                    self.regs.gpr[reg::CX] = cx;
                }
                self.cycles += 1;
                self.raise(bus, fault, false)
            }
        }
    }

    /// Whether a maskable hardware interrupt may be taken before the next instruction.
    pub fn interrupts_enabled(&self) -> bool {
        self.regs.flag(flags::IF) && !self.irq_inhibit
    }

    /// Enters a hardware interrupt (from the PIC) at the current CS:IP: through the interrupt
    /// vector table in real mode, through the IDT in protected mode.
    pub fn interrupt<B: Bus>(&mut self, bus: &mut B, vector: u8) -> Result<(), Exit> {
        self.cycles += 1;
        match self.deliver(bus, vector, protected::IntKind::External, None) {
            Ok(()) => Ok(()),
            Err(Abort::Exit(exit)) => Err(*exit),
            Err(Abort::Fault(fault)) => self.raise(bus, fault, true),
        }
    }

    /// Delivers an exception; a fault while doing so becomes a double fault, and a fault during
    /// that a triple fault, which stops the run (a real PC would reset).
    fn raise<B: Bus>(&mut self, bus: &mut B, fault: Fault, nested: bool) -> Result<(), Exit> {
        let mut fault = fault;
        let mut depth = u8::from(nested);
        loop {
            match self.deliver(bus, fault.vector, protected::IntKind::Exception, fault.code()) {
                Ok(()) => return Ok(()),
                Err(Abort::Exit(exit)) => return Err(*exit),
                Err(Abort::Fault(next)) => {
                    depth += 1;
                    if depth >= 2 || fault.vector == vector::DOUBLE_FAULT {
                        return Err(Exit::Unsupported {
                            cs: self.regs.seg[seg::CS],
                            ip: self.regs.ip,
                            feature: "triple fault (CPU reset)",
                        });
                    }
                    let _ = next;
                    fault = Fault::with_code(vector::DOUBLE_FAULT, 0);
                }
            }
        }
    }

    pub fn push16<B: Bus>(&mut self, bus: &mut B, value: u16) {
        let _ = self.push(bus, exec::Width::Word, u32::from(value));
    }

    pub fn pop16<B: Bus>(&mut self, bus: &mut B) -> u16 {
        self.pop(bus, exec::Width::Word).unwrap_or(0) as u16
    }

    /// Linear address of `offset` in segment register `seg` (an index from [`seg`]).
    pub fn linear(&self, seg: usize, offset: u16) -> u32 {
        self.segment_base(seg).wrapping_add(u32::from(offset))
    }

    /// Loads FLAGS the way POPF/IRET/SAHF do in real mode for this model.
    pub fn load_flags(&mut self, value: u16) {
        let low = match self.model {
            Model::I286 => flags::normalize(value),
            Model::I386 => flags::normalize_386(value),
        };
        self.regs.flags = (self.regs.flags & 0xFFFF_0000) | u32::from(low);
    }
}
