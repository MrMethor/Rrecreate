//! The analysis tools a machine can run ([`Probes`]), and the bus while they run: contract
//! inference's access log, a memory watch, bounds checks, uninitialized reads and late inputs see
//! every data access of the CPU, any of them together. Instruction fetches aren't data accesses.
//!
//! Probes watch the machine; none of them changes what it does, and none is part of its state
//! (a snapshot holds none of them).

use cpu::Bus;

use crate::bounds::Bounds;
use crate::calls::Calls;
use crate::late::LateInputs;
use crate::machine::{ExecHit, PcBus};
use crate::uninit::Uninit;
use crate::watch::Watch;

#[derive(Clone, Default)]
pub struct Probes {
    /// When set, every host service call is recorded here (BIOS, DOS, EMS, MSCDEX); timer and
    /// keyboard interrupts are left out.
    pub service_trace: Option<Vec<crate::trace::ServiceCall>>,
    /// When set, every read and write of a file through DOS is recorded here (D78).
    pub files: Option<Vec<crate::trace::FileAccess>>,
    /// Hardware interrupts delivered to the CPU, per IRQ line (0-15).
    pub irq_counts: [u64; 16],
    /// When set, every instruction that starts inside this linear range is recorded in
    /// `exec_hits`.
    pub exec_watch: Option<std::ops::Range<u32>>,
    pub exec_hits: Vec<ExecHit>,
    /// What the started program's code executed ([`crate::Machine::start_coverage`]).
    pub coverage: Option<crate::coverage::Coverage>,
    /// Contract inference ([`crate::Machine::start_contracts`]).
    pub contracts: Option<Box<crate::contracts::Contracts>>,
    /// A memory watch ([`crate::Machine::start_watch`]). It takes the accesses contract inference
    /// would see: don't run both.
    pub watch: Option<Box<Watch>>,
    /// Call tracking ([`crate::Machine::track_calls`]), and the probes that turned it on
    /// ([`CALLS_FOR_WATCH`], [`CALLS_FOR_LATE`]; none: the tool did).
    pub calls: Option<Box<Calls>>,
    pub(crate) calls_for: u8,
    /// Bounds checks on declared data ([`crate::Machine::start_bounds`]).
    pub bounds: Option<Box<Bounds>>,
    /// Uninitialized reads ([`crate::Machine::start_uninit`]).
    pub uninit: Option<Box<Uninit>>,
    /// Input that changed within a tick ([`crate::Machine::start_late_inputs`]).
    pub late: Option<Box<LateInputs>>,
    /// Entries whose runs are kept with the registers ([`crate::Machine::start_entries`]).
    pub entries: Option<Box<Entries>>,
}

/// The program's runs of chosen entries, with the registers it ran them with (D98): the calls a part of
/// the program a port doesn't have makes into code the port has (a renderer waking a creature it drew).
/// Interrupt handlers' runs aren't the program's and are left out.
#[derive(Debug, Clone, Default)]
pub struct Entries {
    /// The entries (linear), sorted.
    pub at: Vec<u32>,
    pub hits: Vec<EntryHit>,
    /// Runs past [`crate::machine::MAX_EXEC_HITS`].
    pub dropped: u64,
}

#[derive(Debug, Clone)]
pub struct EntryHit {
    pub cycle: u64,
    pub at: u32,
    pub regs: cpu::Regs,
}

pub(crate) const CALLS_FOR_WATCH: u8 = 1;
pub(crate) const CALLS_FOR_LATE: u8 = 2;

/// One access, in order (contract inference's log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    Read(u32, u8),
    Write(u32, u8),
    Port,
}

/// The bytes the instruction running read and hasn't written elsewhere yet, oldest first: a write of the
/// oldest's value to another address copies it (`MOVS`, `PUSH` and `POP` of memory read a byte, then
/// write it). At most 8, as an instruction moves at most a doubleword at a time.
#[derive(Debug, Default)]
pub(crate) struct Pending {
    bytes: [(u32, u8); 8],
    start: usize,
    len: usize,
}

impl Pending {
    fn push(&mut self, byte: (u32, u8)) {
        if self.len == self.bytes.len() {
            self.start = (self.start + 1) % self.bytes.len();
            self.len -= 1;
        }
        self.bytes[(self.start + self.len) % self.bytes.len()] = byte;
        self.len += 1;
    }

    /// The source of a write of `value` to `addr`, if it copies the oldest byte read.
    fn copied(&mut self, addr: u32, value: u8) -> Option<u32> {
        if self.len == 0 {
            return None;
        }
        let (from, read) = self.bytes[self.start];
        self.start = (self.start + 1) % self.bytes.len();
        self.len -= 1;
        (from != addr && read == value).then_some(from)
    }
}

pub(crate) struct ProbeBus<'a, 'b> {
    pub(crate) inner: PcBus<'a>,
    pub(crate) log: Option<&'b mut Vec<Access>>,
    pub(crate) watch: Option<&'b mut Watch>,
    pub(crate) bounds: Option<&'b mut Bounds>,
    pub(crate) uninit: Option<&'b mut Uninit>,
    pub(crate) late: Option<&'b mut LateInputs>,
    pub(crate) calls: Option<&'b Calls>,
    /// What the instruction read, for a watch's copies.
    pub(crate) pending: Pending,
    /// The instruction running: `cs:ip`, and its linear address.
    pub(crate) cs: u16,
    pub(crate) ip: u32,
    pub(crate) at: u32,
    pub(crate) now: u64,
}

impl Bus for ProbeBus<'_, '_> {
    fn read_u8(&mut self, addr: u32) -> u8 {
        if let Some(uninit) = self.uninit.as_deref_mut()
            && !self.inner.was_written(addr)
        {
            uninit.read(addr, self.inner.peek_u8(addr), self.at, self.now);
        }
        let value = self.inner.read_u8(addr);
        if let Some(log) = self.log.as_deref_mut() {
            log.push(Access::Read(addr, value));
        }
        if let Some(watch) = self.watch.as_deref_mut() {
            let by = crate::watch::By::Instruction { cs: self.cs, ip: self.ip, linear: self.at };
            watch.on_read(self.now, addr, value, by, self.calls);
            self.pending.push((addr, value));
        }
        if let Some(bounds) = self.bounds.as_deref_mut() {
            bounds.access(self.at, addr, self.now);
        }
        if let Some(late) = self.late.as_deref_mut()
            && late.watches(addr)
            && !self.calls.is_some_and(Calls::in_hardware_interrupt)
        {
            late.memory_read(addr, value, late_read(self.now, self.at, self.calls));
        }
        value
    }

    fn write_u8(&mut self, addr: u32, value: u8) {
        if let Some(log) = self.log.as_deref_mut() {
            log.push(Access::Write(addr, value));
        }
        if let Some(watch) = self.watch.as_deref_mut() {
            let by = crate::watch::By::Instruction { cs: self.cs, ip: self.ip, linear: self.at };
            let from = self.pending.copied(addr, value);
            watch.on_write(self.now, addr, value, self.inner.peek_u8(addr), by, from, self.calls);
        }
        if let Some(bounds) = self.bounds.as_deref_mut() {
            bounds.access(self.at, addr, self.now);
        }
        if let Some(late) = self.late.as_deref_mut()
            && late.watches(addr)
            && !self.calls.is_some_and(Calls::in_hardware_interrupt)
        {
            late.memory_written(addr, value);
        }
        self.inner.write_u8(addr, value);
    }

    fn fetch_u8(&mut self, addr: u32) -> u8 {
        self.inner.read_u8(addr)
    }

    fn code_window(&mut self, addr: u32) -> Option<[u8; 16]> {
        self.inner.code_window(addr)
    }

    fn port_in_u8(&mut self, port: u16) -> u8 {
        if let Some(log) = self.log.as_deref_mut() {
            log.push(Access::Port);
        }
        if let Some(watch) = self.watch.as_deref_mut() {
            watch.port_used(port, self.at, false, self.calls);
        }
        if let Some(late) = self.late.as_deref_mut()
            && port == crate::joystick::PORT
            && !self.calls.is_some_and(Calls::in_hardware_interrupt)
        {
            let read = late_read(self.now, self.at, self.calls);
            late.device_read(crate::Device::Joystick, &self.inner.joystick_state(), read);
        }
        self.inner.port_in_u8(port)
    }

    fn port_out_u8(&mut self, port: u16, value: u8) {
        if let Some(log) = self.log.as_deref_mut() {
            log.push(Access::Port);
        }
        if let Some(watch) = self.watch.as_deref_mut() {
            watch.port_used(port, self.at, true, self.calls);
        }
        self.inner.port_out_u8(port, value);
    }
}

/// A read by the instruction at `at`, for late inputs.
fn late_read(cycle: u64, at: u32, calls: Option<&Calls>) -> crate::late::Read {
    crate::late::Read { cycle, at, call: calls.and_then(|c| c.frame(0)) }
}
