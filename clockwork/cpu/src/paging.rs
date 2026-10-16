//! 80386 paging: two-level page tables under CR3, accessed and dirty bits, user/supervisor and
//! read/write protection, page faults with CR2 and an error code, and a translation cache.
//!
//! Follows the 80386 rules as DOSBox Staging `src/cpu/paging.cpp` (GPL-2.0-or-later) applies
//! them; DOSBox handles faults by running the guest's handler in a nested loop, while here a
//! fault aborts the instruction, which is undone and restarted after the handler returns.
//! Supervisor writes ignore read-only pages unless CR0.WP is set (an 80486 addition).

use crate::{Abort, Bus, Cpu, Fault, Res, cr0, vector};

const TLB_ENTRIES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
struct Entry {
    /// Linear page number + 1; 0 marks an empty entry.
    tag: u32,
    frame: u32,
    writable: bool,
    user: bool,
    /// The page's dirty bit is already set in its page table entry.
    dirty: bool,
}

/// Translation cache, direct-mapped by linear page number. Flushed when CR3 is written or
/// paging is switched on or off.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct Tlb {
    entries: Box<[Entry]>,
}

impl std::fmt::Debug for Tlb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tlb({} entries in use)", self.entries.iter().filter(|e| e.tag != 0).count())
    }
}

impl Default for Tlb {
    fn default() -> Self {
        Self { entries: vec![Entry::default(); TLB_ENTRIES].into_boxed_slice() }
    }
}

impl Tlb {
    pub fn flush(&mut self) {
        self.entries.fill(Entry::default());
    }
}

fn read_u32<B: Bus>(bus: &mut B, addr: u32) -> u32 {
    u32::from_le_bytes(std::array::from_fn(|i| bus.read_u8(addr.wrapping_add(i as u32))))
}

fn write_u32<B: Bus>(bus: &mut B, addr: u32, value: u32) {
    for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
        bus.write_u8(addr.wrapping_add(i as u32), byte);
    }
}

impl Cpu {
    #[inline]
    pub(crate) fn paging(&self) -> bool {
        self.cr0 & (cr0::PG | cr0::PE) == (cr0::PG | cr0::PE)
    }

    /// For debuggers: the physical address the page tables map `linear` to now, or `None` if the
    /// page isn't present. Unlike a real access it changes nothing (no TLB fill, no accessed or
    /// dirty bits, no fault). `read_u32` reads physical memory.
    pub fn peek_translate(&self, read_u32: impl Fn(u32) -> u32, linear: u32) -> Option<u32> {
        if !self.paging() {
            return Some(linear);
        }
        let pde = read_u32((self.cr3 & !0xFFF) | ((linear >> 22) << 2));
        if pde & 1 == 0 {
            return None;
        }
        let pte = read_u32((pde & !0xFFF) | (((linear >> 12) & 0x3FF) << 2));
        (pte & 1 != 0).then_some((pte & !0xFFF) | (linear & 0xFFF))
    }

    /// The physical address of `linear` for an access by the current privilege (`user`: CPL 3),
    /// walking the page tables when the cache misses.
    pub(crate) fn translate<B: Bus>(&mut self, bus: &mut B, linear: u32, write: bool, user: bool) -> Res<u32> {
        let page = linear >> 12;
        let slot = page as usize % TLB_ENTRIES;
        let entry = self.tlb.entries[slot];
        let supervisor_write_ok = self.cr0 & cr0::WP == 0;
        if entry.tag == page + 1
            && (!write || entry.dirty)
            && (!user || entry.user)
            && (!write || entry.writable || (!user && supervisor_write_ok))
        {
            return Ok((entry.frame << 12) | (linear & 0xFFF));
        }
        let fault = |cpu: &mut Cpu, present: bool| -> Abort {
            cpu.cr2 = linear;
            let code = u32::from(present) | (u32::from(write) << 1) | (u32::from(user) << 2);
            Abort::Fault(Fault::with_code(vector::PAGE_FAULT, code))
        };
        let pde_addr = (self.cr3 & !0xFFF) | ((linear >> 22) << 2);
        let mut pde = read_u32(bus, pde_addr);
        if pde & 1 == 0 {
            return Err(fault(self, false));
        }
        let pte_addr = (pde & !0xFFF) | (((linear >> 12) & 0x3FF) << 2);
        let mut pte = read_u32(bus, pte_addr);
        if pte & 1 == 0 {
            return Err(fault(self, false));
        }
        let user_ok = pde & pte & 4 != 0;
        let writable = pde & pte & 2 != 0;
        if (user && !user_ok) || (write && !writable && (user || !supervisor_write_ok)) {
            return Err(fault(self, true));
        }
        if pde & 0x20 == 0 {
            pde |= 0x20;
            write_u32(bus, pde_addr, pde);
        }
        let wanted = pte | 0x20 | if write { 0x40 } else { 0 };
        if wanted != pte {
            pte = wanted;
            write_u32(bus, pte_addr, pte);
        }
        self.tlb.entries[slot] =
            Entry { tag: page + 1, frame: pte >> 12, writable, user: user_ok, dirty: pte & 0x40 != 0 };
        Ok((pte & !0xFFF) | (linear & 0xFFF))
    }
}
