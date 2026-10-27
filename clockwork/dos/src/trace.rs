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

/// A read or a write of a file the program made through DOS (D78): which file, where in it and how
/// much, where in memory the bytes went or came from, and where the call returns to. The file tracing
/// of `rrc files` links a structure in memory to its file, and the file to the code that reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAccess {
    pub cycle: u64,
    pub write: bool,
    pub path: String,
    /// Where in the file, and how many bytes were read or written.
    pub offset: u32,
    pub len: u32,
    /// Linear address of the program's buffer.
    pub at: u32,
    /// Linear address the call returns to (the interrupt frame's return address).
    pub caller: u32,
    /// The call chain then, innermost first, if call tracking is on.
    pub chain: Vec<crate::calls::Frame>,
}

impl FileAccess {
    /// Records one, if file tracing is on (the call's registers are DS:DX, CX as DOS takes them).
    pub(crate) fn record(m: &mut Machine, write: bool, path: &str, offset: u32, len: u32) {
        if m.probes.files.is_none() {
            return;
        }
        let regs = m.cpu.regs;
        let frame = m.cpu.linear(cpu::seg::SS, regs.sp());
        let (ip, cs) = (m.mem.read_u16(frame), m.mem.read_u16(frame + 2));
        let caller = m.peek_address(cs, u32::from(ip)).map_or(Machine::lin(cs, ip), |(linear, _)| linear);
        let at = Machine::lin(regs.ds(), regs.dx());
        let chain = m.probes.calls.as_deref().map(crate::calls::Calls::chain).unwrap_or_default();
        let access = FileAccess { cycle: m.cpu.cycles, write, path: path.to_owned(), offset, len, at, caller, chain };
        if let Some(files) = m.probes.files.as_mut() {
            files.push(access);
        }
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
