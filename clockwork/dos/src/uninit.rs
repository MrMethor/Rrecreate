//! Uninitialized reads: the first read of each byte nothing wrote since tracking began. On a real
//! PC such a byte holds whatever was there before; here it is deterministic (zero, mostly), so a
//! game that depends on it behaves as it does in Clockwork only by luck. A port has to reproduce
//! the value, or the behaviour, deliberately: a quirk.
//!
//! [`crate::machine::Machine::start_uninit`] counts as uninitialized what a DOS program finds so
//! when it starts: its own memory block past its load image (its BSS, heap and stack), the free
//! memory blocks and extended memory. Writes by the CPU and by the host both count.

use std::collections::BTreeSet;

/// Reads kept, at most; later ones are counted in [`Uninit::dropped`].
pub const DEFAULT_LIMIT: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Read {
    /// Physical address of the byte, and the value it had.
    pub address: u32,
    pub value: u8,
    /// Linear address of the instruction that read it.
    pub instruction: u32,
    pub cycle: u64,
}

#[derive(Debug, Clone)]
pub struct Uninit {
    pub reads: Vec<Read>,
    pub limit: usize,
    pub dropped: u64,
    seen: BTreeSet<u32>,
}

impl Default for Uninit {
    fn default() -> Self {
        Self { reads: Vec::new(), limit: DEFAULT_LIMIT, dropped: 0, seen: BTreeSet::new() }
    }
}

impl Uninit {
    /// A read of a byte nothing wrote: recorded the first time.
    pub(crate) fn read(&mut self, address: u32, value: u8, instruction: u32, cycle: u64) {
        if !self.seen.insert(address) {
            return;
        }
        if self.reads.len() >= self.limit {
            self.dropped += 1;
            return;
        }
        self.reads.push(Read { address, value, instruction, cycle });
    }
}
