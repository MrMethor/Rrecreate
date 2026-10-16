//! Service call tracing: one record per BIOS/DOS/EMS call handled by the host, with the
//! registers going in and coming out. Meant for finding where a program's expectations and the
//! emulation diverge (a failed open, an unexpected return value).

use std::fmt;

use cpu::Regs;
use cpu::flags::{CF, ZF};

use crate::machine::Machine;

/// DOS functions whose DS:DX (or DS:SI for 6Ch) argument is a path worth showing.
const PATH_FUNCTIONS: [u8; 12] = [0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x41, 0x43, 0x4B, 0x4E, 0x56, 0x5B, 0x6C];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServiceCall {
    pub cycle: u64,
    /// Interrupt number of the service (the callback id).
    pub service: u16,
    /// Caller's address: the return address on the interrupt frame.
    pub caller: (u16, u16),
    pub before: Regs,
    pub after: Regs,
    /// The path argument, for DOS functions that take one.
    pub path: Option<String>,
    /// Whether the service returned CF/ZF to the caller (otherwise `after.flags` is stale).
    pub returned_flags: bool,
}

impl ServiceCall {
    pub(crate) fn begin(m: &Machine, service: u16) -> Self {
        let regs = m.cpu.regs;
        let frame = m.cpu.linear(cpu::seg::SS, regs.sp());
        let caller = (m.mem.read_u16(frame + 2), m.mem.read_u16(frame));
        let path = (service == 0x21 && PATH_FUNCTIONS.contains(&regs.ah())).then(|| {
            let offset = if regs.ah() == 0x6C { regs.si() } else { regs.dx() };
            let at = Machine::lin(regs.ds(), offset);
            let bytes: Vec<u8> = (0..128).map(|i| m.mem.read_u8(at + i)).take_while(|&b| b != 0).collect();
            String::from_utf8_lossy(&bytes).into_owned()
        });
        Self { cycle: m.cpu.cycles, service, caller, before: regs, after: regs, path, returned_flags: false }
    }
}

impl fmt::Display for ServiceCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (b, a) = (&self.before, &self.after);
        write!(
            f,
            "{:>12} {:04X}:{:04X} INT {:02X}h AX={:04X} BX={:04X} CX={:04X} DX={:04X} DS={:04X} ES={:04X}",
            self.cycle,
            self.caller.0,
            self.caller.1,
            self.service,
            b.ax(),
            b.bx(),
            b.cx(),
            b.dx(),
            b.ds(),
            b.es()
        )?;
        if let Some(path) = &self.path {
            write!(f, " {path:?}")?;
        }
        write!(f, " -> AX={:04X} BX={:04X} CX={:04X} DX={:04X}", a.ax(), a.bx(), a.cx(), a.dx())?;
        if self.returned_flags {
            write!(f, " CF={} ZF={}", u8::from(a.flag(CF)), u8::from(a.flag(ZF)))?;
        }
        Ok(())
    }
}
