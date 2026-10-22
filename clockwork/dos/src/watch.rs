//! Memory watches: every write (and, if asked, read) of the watched bytes, with who did it: the
//! instruction, the call chain it ran in, and the cycle. Writes by host services (a DOS read
//! filling a buffer) and by devices (DMA) are found from memory's log of writes around them
//! ([`crate::memory::Memory::start_log`]), and in video memory, which devices hold, by comparing
//! the watched bytes.
//!
//! Watches work like MAME's (`wpset`): on the bus, so every access counts, a write of the same
//! value included, rather than DOSBox's polling for changed values. Addresses are physical.
//!
//! The call chain comes from call tracking ([`crate::calls`]), which a watch turns on if nothing
//! did before.
//!
//! A summary watch ([`Watch::summary`]) keeps, instead of each access, who touched each byte: code
//! or interrupt handlers, host services or devices. It scans large ranges: the memory a game's loop
//! reads that only interrupt handlers write (its key states, its timer counts) is what the loop
//! takes from outside. It also counts the services called and the ports read meanwhile: what those
//! return stays in registers, where no watch looks (the mouse through INT 33h, the joystick's port).
//! An interrupt handler here is what runs for a hardware interrupt (and the software interrupts it
//! makes: INT 1Ch from the timer's), asynchronously to the program; a software interrupt the program
//! makes itself runs for it, as DOS's EXEC runs a launcher's game ([`Calls::interrupt_entry`]).

use std::collections::BTreeMap;
use std::ops::Range;

use crate::calls::Calls;
pub use crate::calls::Frame;

/// Hits kept, at most; later ones are counted in [`Watch::dropped`].
pub const DEFAULT_LIMIT: usize = 10_000;

/// Video memory, which devices hold rather than memory: the host's writes there are found by
/// comparing.
const VIDEO: Range<u32> = 0xA_0000..0xC_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Read,
    Write,
}

/// What made an access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    /// The instruction at `linear` (`cs:ip`).
    Instruction { cs: u16, ip: u32, linear: u32 },
    /// A host service (BIOS, DOS, EMS, ...), entered through callback `id` with AX = `ax`.
    Service { id: u16, ax: u16 },
    /// A device event (DMA, the mouse cursor).
    Device,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub cycle: u64,
    /// Physical address of the byte.
    pub address: u32,
    pub kind: Kind,
    /// The byte read or written, and for a write the byte it replaced.
    pub value: u8,
    pub old: u8,
    pub by: By,
    /// The call chain at the access, innermost first.
    pub chain: Vec<Frame>,
}

/// Who touched a byte, in a summary watch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Touch {
    /// Read by code outside interrupt handlers.
    pub read: bool,
    /// Read inside an interrupt handler.
    pub read_in_interrupt: bool,
    /// Written by code outside interrupt handlers.
    pub written: bool,
    /// Written inside an interrupt handler: the first such handler's entry (linear).
    pub interrupt_writer: Option<u32>,
    /// The instruction (linear) that wrote it first inside an interrupt handler.
    pub interrupt_write_by: Option<u32>,
    /// Written by a host service or a device.
    pub written_by_host: bool,
}

#[derive(Debug, Clone)]
pub struct Watch {
    ranges: Vec<Range<u32>>,
    bounds: Range<u32>,
    reads: bool,
    pub limit: usize,
    pub hits: Vec<Hit>,
    /// Hits past `limit`.
    pub dropped: u64,
    /// For a summary watch: who touched each byte (physical address), instead of `hits`.
    pub touches: Option<BTreeMap<u32, Touch>>,
    /// For a summary watch: the host services called (by callback, which for the BIOS's, DOS's and
    /// the mouse driver's is their interrupt) and the ports read, with how often.
    pub services: BTreeMap<u16, u64>,
    pub port_reads: BTreeMap<u16, u64>,
}

impl Watch {
    /// Watches writes to `ranges` (physical addresses), and reads too if `reads`.
    pub fn new(ranges: Vec<Range<u32>>, reads: bool) -> Self {
        let bounds = ranges.iter().map(|r| r.start).min().unwrap_or(0)..ranges.iter().map(|r| r.end).max().unwrap_or(0);
        Self {
            ranges,
            bounds,
            reads,
            limit: DEFAULT_LIMIT,
            hits: Vec::new(),
            dropped: 0,
            touches: None,
            services: BTreeMap::new(),
            port_reads: BTreeMap::new(),
        }
    }

    /// A summary watch: who touched each byte of `ranges`, not each access.
    pub fn summary(ranges: Vec<Range<u32>>, reads: bool) -> Self {
        Self { touches: Some(BTreeMap::new()), ..Self::new(ranges, reads) }
    }

    /// Watches one range.
    pub fn range(range: Range<u32>, reads: bool) -> Self {
        Self::new(std::iter::once(range).collect(), reads)
    }

    #[inline]
    fn watched(&self, address: u32) -> bool {
        self.bounds.contains(&address) && self.ranges.iter().any(|r| r.contains(&address))
    }

    /// Keeps `hit`, with the call chain now (in a summary watch, who touched its byte).
    fn record(&mut self, mut hit: Hit, calls: Option<&Calls>) {
        if let Some(touches) = &mut self.touches {
            let interrupt = calls.and_then(Calls::interrupt_entry);
            let touch = touches.entry(hit.address).or_default();
            match (hit.kind, hit.by, interrupt) {
                (Kind::Read, _, None) => touch.read = true,
                (Kind::Read, _, Some(_)) => touch.read_in_interrupt = true,
                (Kind::Write, By::Instruction { .. }, None) => touch.written = true,
                (Kind::Write, By::Instruction { linear, .. }, Some(entry)) => {
                    if touch.interrupt_writer.is_none() {
                        (touch.interrupt_writer, touch.interrupt_write_by) = (Some(entry), Some(linear));
                    }
                }
                (Kind::Write, _, _) => touch.written_by_host = true,
            }
            return;
        }
        if self.hits.len() >= self.limit {
            self.dropped += 1;
            return;
        }
        hit.chain = calls.map(Calls::chain).unwrap_or_default();
        self.hits.push(hit);
    }

    /// A read by the instruction running (the probe bus calls this).
    pub(crate) fn on_read(&mut self, cycle: u64, addr: u32, value: u8, by: By, calls: Option<&Calls>) {
        if self.reads && self.watched(addr) {
            let hit = Hit { cycle, address: addr, kind: Kind::Read, value, old: value, by, chain: Vec::new() };
            self.record(hit, calls);
        }
    }

    /// A write by the instruction running, of `value` over `old`.
    pub(crate) fn on_write(&mut self, cycle: u64, addr: u32, value: u8, old: u8, by: By, calls: Option<&Calls>) {
        if self.watched(addr) {
            let hit = Hit { cycle, address: addr, kind: Kind::Write, value, old, by, chain: Vec::new() };
            self.record(hit, calls);
        }
    }

    /// A service was called (a summary watch counts them).
    pub(crate) fn service_called(&mut self, id: u16) {
        if self.touches.is_some() {
            *self.services.entry(id).or_default() += 1;
        }
    }

    /// A port was read (a summary watch counts them).
    pub(crate) fn port_read(&mut self, port: u16) {
        if self.touches.is_some() {
            *self.port_reads.entry(port).or_default() += 1;
        }
    }

    /// The watched video memory, to compare around a service or a device event (`None`: none
    /// watched).
    pub(crate) fn video_bytes(&self, peek: impl Fn(u32) -> u8) -> Option<Vec<u8>> {
        let mut bytes = self.video_addresses().map(peek).peekable();
        bytes.peek().is_some().then(|| bytes.collect())
    }

    fn video_addresses(&self) -> impl Iterator<Item = u32> + '_ {
        self.ranges.iter().flat_map(|r| r.start.max(VIDEO.start)..r.end.min(VIDEO.end))
    }

    /// Records the watched bytes that `by` changed: from memory's `log` of writes (address, the value
    /// before) and, in video memory, `video_before` (from [`Watch::video_bytes`]) against now.
    pub(crate) fn changed(
        &mut self,
        cycle: u64,
        log: &[(u32, u8)],
        video_before: Option<&[u8]>,
        now: impl Fn(u32) -> u8,
        by: By,
        calls: Option<&Calls>,
    ) {
        // Each byte's value before the first write, if it is watched.
        let mut first: BTreeMap<u32, u8> = BTreeMap::new();
        for &(address, old) in log {
            if self.watched(address) {
                first.entry(address).or_insert(old);
            }
        }
        if let Some(before) = video_before {
            let addresses: Vec<u32> = self.video_addresses().collect();
            first.extend(addresses.into_iter().zip(before.iter().copied()));
        }
        for (address, old) in first {
            let value = now(address);
            if old != value {
                self.record(Hit { cycle, address, kind: Kind::Write, value, old, by, chain: Vec::new() }, calls);
            }
        }
    }
}
