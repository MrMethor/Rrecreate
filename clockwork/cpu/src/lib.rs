//! Deterministic x86 real-mode CPU interpreter for the Recreate host (PROJECT.md §5.2).
//!
//! Two models are emulated, both in real mode only:
//! - [`Model::I286`]: the 8086/80186 instruction set with 80286 behavior: shift and rotate
//!   counts masked to 5 bits, `PUSH SP` pushes the value before the push, FLAGS bits 12–15
//!   always read as 0, divide errors return to the faulting instruction, `0F` isn't `POP CS`.
//! - [`Model::I386`] (the default): adds 32-bit registers and operands, FS and GS, the operand-
//!   and address-size prefixes, 32-bit addressing, and the 80386 two-byte opcodes (MOVZX, SETcc,
//!   bit tests, double shifts, ...). FLAGS bits 12–14 become writable, so CPU detection code sees
//!   an 80386. Entering protected mode stops the run with [`Exit::Unsupported`].
//!
//! There is no FPU: escape opcodes decode their operands and do nothing, which is how a machine
//! without a coprocessor behaves.
//!
//! The interpreter is deterministic. Its state is plain data, so `Clone` is a snapshot, and
//! time is counted in [`Cpu::cycles`], never read from the host clock. Memory and port I/O go
//! through the [`Bus`] trait, which the machine implements.
//!
//! The host's high-level emulation (HLE) is entered through a reserved opcode,
//! `FE 38 <id:u16>` (an invalid encoding of `INC/DEC r/m8`). Executing it makes [`Cpu::step`]
//! return [`Exit::Callback`]. BIOS and DOS entry points in the emulated ROM are such callbacks
//! followed by ordinary code like `IRET`.

mod exec;
pub mod flags;
mod regs;

use std::fmt;

pub use regs::{Regs, reg, seg};

/// First two bytes of the host callback instruction; a little-endian `u16` id follows.
pub const CALLBACK_OPCODE: [u8; 2] = [0xFE, 0x38];

/// Memory and I/O as seen by the CPU. Addresses are linear (`segment * 16 + offset`); the bus
/// applies A20 masking and memory-mapped devices.
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
    Unimplemented { cs: u16, ip: u16, bytes: [u8; 6] },
    /// The instruction is understood but asks for something the host doesn't emulate, such as
    /// switching to protected mode. CS:IP still point at it and nothing was changed.
    Unsupported { cs: u16, ip: u16, feature: &'static str },
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

/// Descriptor table register contents (LGDT/LIDT), kept so SGDT/SIDT read them back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TableRegister {
    pub limit: u16,
    pub base: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cpu {
    pub regs: Regs,
    pub model: Model,
    /// Control register 0. Only real mode is emulated, so PE stays 0.
    pub cr0: u32,
    pub gdtr: TableRegister,
    pub idtr: TableRegister,
    /// Executed instructions, plus one per repetition of a REP-prefixed string instruction and
    /// one per interrupt taken. The machine derives virtual time from this.
    pub cycles: u64,
    /// Set by `STI`, `MOV SS` and `POP SS`: no interrupt is taken before the next instruction.
    irq_inhibit: bool,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
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
            gdtr: TableRegister { limit: 0, base: 0 },
            idtr: TableRegister { limit: 0x3FF, base: 0 },
            cycles: 0,
            irq_inhibit: false,
        }
    }

    /// Executes one instruction (with its prefixes). A REP-prefixed string instruction runs all
    /// its repetitions in one step.
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> Result<(), Exit> {
        let trap = self.regs.flags & flags::TF != 0;
        self.irq_inhibit = false;
        let start_ip = self.regs.ip;
        let result = self.exec(bus, start_ip);
        if !matches!(result, Err(Exit::Unimplemented { .. } | Exit::Unsupported { .. })) {
            self.cycles += 1;
        }
        if trap && result.is_ok() {
            self.interrupt(bus, 1);
        }
        result
    }

    /// Whether a maskable hardware interrupt may be taken before the next instruction.
    pub fn interrupts_enabled(&self) -> bool {
        self.regs.flags & flags::IF != 0 && !self.irq_inhibit
    }

    /// Enters interrupt `vector` as if `INT vector` had been executed at the current CS:IP:
    /// pushes FLAGS, CS and IP, clears IF and TF, and jumps through the interrupt vector table.
    pub fn interrupt<B: Bus>(&mut self, bus: &mut B, vector: u8) {
        let flags = self.regs.flags;
        self.push16(bus, flags);
        self.regs.flags &= !(flags::IF | flags::TF);
        let (cs, ip) = (self.regs.seg[seg::CS], self.regs.ip);
        self.push16(bus, cs);
        self.push16(bus, ip);
        let entry = u32::from(vector) * 4;
        self.regs.ip = read_linear_u16(bus, entry);
        self.regs.seg[seg::CS] = read_linear_u16(bus, entry + 2);
        self.cycles += 1;
    }

    pub fn push16<B: Bus>(&mut self, bus: &mut B, value: u16) {
        let sp = self.regs.sp().wrapping_sub(2);
        self.regs.set_sp(sp);
        self.write_mem16(bus, seg::SS, sp, value);
    }

    pub fn pop16<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let sp = self.regs.sp();
        let value = self.read_mem16(bus, seg::SS, sp);
        self.regs.set_sp(sp.wrapping_add(2));
        value
    }

    /// Linear address of `offset` in segment register `seg` (an index from [`seg`]).
    pub fn linear(&self, seg: usize, offset: u16) -> u32 {
        (u32::from(self.regs.seg[seg]) << 4) + u32::from(offset)
    }

    /// Loads FLAGS the way POPF/IRET/SAHF do for this model.
    pub fn load_flags(&mut self, value: u16) {
        self.regs.flags = match self.model {
            Model::I286 => flags::normalize(value),
            Model::I386 => flags::normalize_386(value),
        };
    }
}

fn read_linear_u16<B: Bus>(bus: &mut B, addr: u32) -> u16 {
    u16::from(bus.read_u8(addr)) | (u16::from(bus.read_u8(addr + 1)) << 8)
}
