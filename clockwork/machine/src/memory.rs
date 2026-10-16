//! The PC's physical memory (PROJECT.md §5.2): the real-mode image (1 MB plus the HMA), which
//! the CPU addresses through the A20 gate, and extended memory above it, which real-mode
//! programs reach through XMS or INT 15h block moves ([`Memory::read_phys`] and friends).

use std::collections::BTreeMap;

use crate::addr::{CONVENTIONAL_SPACE, REAL_MODE_SPACE};

/// Start of the system BIOS ROM. Writes from the CPU to `ROM_START..CONVENTIONAL_SPACE` are
/// ignored, as on real hardware; the host fills the ROM through [`Memory::load`].
pub const ROM_START: u32 = 0xF_0000;

/// Extended memory is stored in pages of this size, created on first write.
const PAGE_SIZE: usize = 4096;

/// `Clone` is a full snapshot: the 1.1 MB real-mode image plus the extended pages written so far.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Memory {
    bytes: Box<[u8]>,
    a20: bool,
    /// Physical memory size in bytes (at least the real-mode space).
    size: u32,
    /// Extended memory pages that have been written, by page number; others read as zero.
    extended: BTreeMap<u32, Box<[u8]>>,
}

impl std::fmt::Debug for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Memory")
            .field("size", &self.size)
            .field("extended_pages", &self.extended.len())
            .field("a20", &self.a20)
            .finish()
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl Memory {
    /// Zero-filled, with the A20 line disabled (addresses wrap at 1 MB, as after boot), and no
    /// memory beyond the HMA.
    pub fn new() -> Self {
        Self::with_size(REAL_MODE_SPACE as u32)
    }

    /// Like [`Memory::new`], with `size` bytes of physical memory in all (16 MB is typical).
    pub fn with_size(size: u32) -> Self {
        Self {
            bytes: vec![0; REAL_MODE_SPACE].into_boxed_slice(),
            a20: false,
            size: size.max(REAL_MODE_SPACE as u32),
            extended: BTreeMap::new(),
        }
    }

    /// Physical memory size in bytes.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// A physical read, as DMA or an XMS move does it: no A20 wrapping, and the whole memory
    /// size is reachable. Beyond the end of memory reads FFh.
    pub fn read_phys(&self, addr: u32) -> u8 {
        if (addr as usize) < self.bytes.len() {
            self.bytes[addr as usize]
        } else if addr < self.size {
            let page = addr / PAGE_SIZE as u32;
            self.extended.get(&page).map_or(0, |data| data[addr as usize % PAGE_SIZE])
        } else {
            0xFF
        }
    }

    /// A physical write. Writes to the BIOS ROM and beyond the end of memory are ignored.
    pub fn write_phys(&mut self, addr: u32, value: u8) {
        if (addr as usize) < self.bytes.len() {
            if !(ROM_START as usize..CONVENTIONAL_SPACE).contains(&(addr as usize)) {
                self.bytes[addr as usize] = value;
            }
        } else if addr < self.size {
            let page = addr / PAGE_SIZE as u32;
            let data = self.extended.entry(page).or_insert_with(|| vec![0; PAGE_SIZE].into_boxed_slice());
            data[addr as usize % PAGE_SIZE] = value;
        }
    }

    /// Copies `len` bytes between physical addresses, lowest address first.
    pub fn copy_phys(&mut self, dest: u32, src: u32, len: u32) {
        for i in 0..len {
            let value = self.read_phys(src.wrapping_add(i));
            self.write_phys(dest.wrapping_add(i), value);
        }
    }

    #[inline]
    pub fn a20(&self) -> bool {
        self.a20
    }

    pub fn set_a20(&mut self, enabled: bool) {
        self.a20 = enabled;
    }

    /// The address after the A20 gate: with A20 disabled, bit 20 is forced to 0.
    #[inline]
    fn gate(&self, addr: u32) -> u32 {
        if self.a20 { addr } else { addr & !0x10_0000 }
    }

    /// Index into the real-mode image (for host accesses, which stay within it).
    #[inline]
    fn index(&self, addr: u32) -> usize {
        (self.gate(addr) as usize) % self.bytes.len()
    }

    /// A CPU read: through the A20 gate, from any physical address.
    #[inline]
    pub fn read_u8(&self, addr: u32) -> u8 {
        let addr = self.gate(addr);
        match self.bytes.get(addr as usize) {
            Some(&byte) => byte,
            None => self.read_extended(addr),
        }
    }

    /// A CPU write. Writes into the BIOS ROM are ignored.
    #[inline]
    pub fn write_u8(&mut self, addr: u32, value: u8) {
        let addr = self.gate(addr);
        if (addr as usize) < self.bytes.len() {
            if !(ROM_START as usize..CONVENTIONAL_SPACE).contains(&(addr as usize)) {
                self.bytes[addr as usize] = value;
            }
        } else {
            self.write_extended(addr, value);
        }
    }

    // Extended memory is kept out of line so the real-mode path stays small enough to inline.
    #[cold]
    #[inline(never)]
    fn read_extended(&self, addr: u32) -> u8 {
        self.read_phys(addr)
    }

    #[cold]
    #[inline(never)]
    fn write_extended(&mut self, addr: u32, value: u8) {
        self.write_phys(addr, value);
    }

    #[inline]
    pub fn read_u16(&self, addr: u32) -> u16 {
        u16::from_le_bytes([self.read_u8(addr), self.read_u8(addr + 1)])
    }

    #[inline]
    pub fn write_u16(&mut self, addr: u32, value: u16) {
        let [lo, hi] = value.to_le_bytes();
        self.write_u8(addr, lo);
        self.write_u8(addr + 1, hi);
    }

    pub fn read_u32(&self, addr: u32) -> u32 {
        u32::from(self.read_u16(addr)) | (u32::from(self.read_u16(addr + 2)) << 16)
    }

    pub fn write_u32(&mut self, addr: u32, value: u32) {
        self.write_u16(addr, value as u16);
        self.write_u16(addr + 2, (value >> 16) as u16);
    }

    /// Copies `len` bytes starting at `addr` (wrapping like CPU accesses do).
    pub fn read_bytes(&self, addr: u32, len: usize) -> Vec<u8> {
        (0..len).map(|i| self.read_u8(addr + i as u32)).collect()
    }

    /// A host write: also reaches the ROM area. Used for loading programs and building the BIOS.
    pub fn load(&mut self, addr: u32, data: &[u8]) {
        for (i, &b) in data.iter().enumerate() {
            let at = self.index(addr + i as u32);
            self.bytes[at] = b;
        }
    }

    /// A contiguous view, for devices that scan memory (e.g. the VGA framebuffer). Panics if the
    /// range runs past the end of the image.
    pub fn slice(&self, addr: u32, len: usize) -> &[u8] {
        let start = addr as usize;
        &self.bytes[start..start + len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a20_controls_wraparound() {
        let mut m = Memory::new();
        m.write_u8(0x10_0005, 0xAA);
        assert_eq!(m.read_u8(0x0_0005), 0xAA, "A20 off: wraps to the first megabyte");
        m.set_a20(true);
        m.write_u8(0x10_0005, 0xBB);
        assert_eq!(m.read_u8(0x0_0005), 0xAA);
        assert_eq!(m.read_u8(0x10_0005), 0xBB);
    }

    #[test]
    fn extended_memory_is_physical_and_sparse() {
        let mut m = Memory::with_size(16 << 20);
        assert_eq!(m.read_phys(0x20_0000), 0, "unwritten extended memory reads as zero");
        m.write_phys(0x20_0000, 0x5A);
        m.write_u8(0x05, 0x11);
        assert_eq!(m.read_phys(0x20_0000), 0x5A);
        assert_eq!(m.read_phys(0x05), 0x11, "the low megabyte is the same memory");
        m.copy_phys(0x30_0000, 0x0, 8);
        assert_eq!(m.read_phys(0x30_0005), 0x11);
        assert_eq!(m.read_phys(0x100_0000), 0xFF, "beyond 16 MB there is nothing");
        m.write_phys(0x100_0000, 1);
        assert_eq!(m.extended.len(), 2, "only written pages are stored");
    }

    #[test]
    fn rom_is_read_only_for_the_cpu() {
        let mut m = Memory::new();
        m.load(ROM_START, &[0x90]);
        m.write_u8(ROM_START, 0xCC);
        assert_eq!(m.read_u8(ROM_START), 0x90);
        m.write_u16(0xA_0000, 0x1234);
        assert_eq!(m.read_u16(0xA_0000), 0x1234);
    }
}
