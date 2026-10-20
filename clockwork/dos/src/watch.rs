//! Memory watches: every write (and, if asked, read) of the watched bytes, with who did it: the
//! instruction, the call chain it ran in, and the cycle. Writes by host services (a DOS read
//! filling a buffer) and by devices (DMA) are found by comparing the watched bytes around them.
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
//! takes from outside.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::calls::Calls;
pub use crate::calls::Frame;

/// Hits kept, at most; later ones are counted in [`Watch::dropped`].
pub const DEFAULT_LIMIT: usize = 10_000;

/// Watched bytes compared around each service call and device event, at most (beyond, those
/// writes aren't seen).
pub const MAX_COMPARED: u32 = 0x1_0000;

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
}

impl Watch {
    /// Watches writes to `ranges` (physical addresses), and reads too if `reads`.
    pub fn new(ranges: Vec<Range<u32>>, reads: bool) -> Self {
        let bounds = ranges.iter().map(|r| r.start).min().unwrap_or(0)..ranges.iter().map(|r| r.end).max().unwrap_or(0);
        Self { ranges, bounds, reads, limit: DEFAULT_LIMIT, hits: Vec::new(), dropped: 0, touches: None }
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
                (Kind::Write, By::Instruction { .. }, Some(entry)) => _ = touch.interrupt_writer.get_or_insert(entry),
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

    /// The watched bytes, for comparing around a service or a device event; `None` when there are
    /// too many.
    pub(crate) fn bytes(&self, peek: impl Fn(u32) -> u8) -> Option<Vec<u8>> {
        let size: u32 = self.ranges.iter().map(|r| r.end - r.start).sum();
        (size <= MAX_COMPARED).then(|| self.ranges.iter().flat_map(|r| r.clone()).map(peek).collect())
    }

    /// Records what changed between `before` and `after` (from [`Watch::bytes`]) as writes by `by`.
    pub(crate) fn changed(&mut self, cycle: u64, before: &[u8], after: &[u8], by: By, calls: Option<&Calls>) {
        let addresses: Vec<u32> = self.ranges.iter().flat_map(|r| r.clone()).collect();
        for ((&address, &old), &value) in addresses.iter().zip(before).zip(after) {
            if old != value {
                self.record(Hit { cycle, address, kind: Kind::Write, value, old, by, chain: Vec::new() }, calls);
            }
        }
    }
}
