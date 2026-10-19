//! Memory-access tracing: a bus that logs the data a piece of code reads and writes, in order
//! (contract inference reads it). Instruction fetches aren't logged.

use cpu::Bus;

use crate::machine::PcBus;

/// One access, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    Read(u32, u8),
    Write(u32, u8),
    Port,
}

/// A bus that logs data accesses.
pub(crate) struct TracingBus<'a, 'b> {
    pub(crate) inner: PcBus<'a>,
    pub(crate) log: &'b mut Vec<Access>,
}

impl Bus for TracingBus<'_, '_> {
    fn read_u8(&mut self, addr: u32) -> u8 {
        let value = self.inner.read_u8(addr);
        self.log.push(Access::Read(addr, value));
        value
    }

    fn write_u8(&mut self, addr: u32, value: u8) {
        self.log.push(Access::Write(addr, value));
        self.inner.write_u8(addr, value);
    }

    fn fetch_u8(&mut self, addr: u32) -> u8 {
        self.inner.read_u8(addr)
    }

    fn code_window(&mut self, addr: u32) -> Option<[u8; 16]> {
        self.inner.code_window(addr)
    }

    fn port_in_u8(&mut self, port: u16) -> u8 {
        self.log.push(Access::Port);
        self.inner.port_in_u8(port)
    }

    fn port_out_u8(&mut self, port: u16, value: u8) {
        self.log.push(Access::Port);
        self.inner.port_out_u8(port, value);
    }
}
