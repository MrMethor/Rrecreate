//! The PC's physical memory: the real-mode image (1 MB plus the HMA), which
//! the CPU addresses through the A20 gate, and extended memory above it, which real-mode
//! programs reach through XMS or INT 15h block moves ([`Memory::read_phys`] and friends).

use crate::addr::{CONVENTIONAL_SPACE, REAL_MODE_SPACE};

/// Start of the system BIOS ROM. Writes from the CPU to `ROM_START..CONVENTIONAL_SPACE` are
/// ignored, as on real hardware; the host fills the ROM through [`Memory::load`].
pub const ROM_START: u32 = 0xF_0000;

/// Extended memory is stored in pages of this size, created on first write.
const PAGE_SIZE: usize = 4096;

/// `Clone` is a full snapshot: the 1.1 MB real-mode image plus the extended pages written so far.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Memory {
    bytes: Box<[u8]>,
    a20: bool,
    /// Physical memory size in bytes (at least the real-mode space).
    size: u32,
    /// Extended memory pages that have been written, indexed by page number (a direct table, as
    /// every access in protected mode looks here); others read as zero.
    extended: Vec<Option<Box<[u8]>>>,
    /// Which bytes have been written, when tracked ([`Memory::track_writes`]); not part of the
    /// memory's contents.
    written: Option<Box<Written>>,
}

/// A bit per byte of physical memory: written since tracking began (or initialized then).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Written(Vec<u64>);

impl Written {
    #[inline]
    fn set(&mut self, addr: u32) {
        if let Some(word) = self.0.get_mut((addr / 64) as usize) {
            *word |= 1 << (addr % 64);
        }
    }

    fn get(&self, addr: u32) -> bool {
        self.0.get((addr / 64) as usize).is_some_and(|word| word >> (addr % 64) & 1 != 0)
    }
}

impl PartialEq for Memory {
    fn eq(&self, other: &Self) -> bool {
        (&self.bytes, self.a20, self.size, &self.extended) == (&other.bytes, other.a20, other.size, &other.extended)
    }
}

impl Eq for Memory {}

impl std::hash::Hash for Memory {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (&self.bytes, self.a20, self.size, &self.extended).hash(state);
    }
}

impl std::fmt::Debug for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Memory")
            .field("size", &self.size)
            .field("extended_pages", &self.extended.iter().flatten().count())
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
        let size = size.max(REAL_MODE_SPACE as u32);
        Self {
            bytes: vec![0; REAL_MODE_SPACE].into_boxed_slice(),
            a20: false,
            size,
            extended: vec![None; (size as usize).div_ceil(PAGE_SIZE)],
            written: None,
        }
    }

    /// Tracks which bytes are written from now on (by the CPU or the host), every byte counting as
    /// initialized except those in `uninitialized` (physical ranges).
    pub fn track_writes(&mut self, uninitialized: &[std::ops::Range<u32>]) {
        let mut written = Written(vec![u64::MAX; (self.size as usize).div_ceil(64)]);
        for range in uninitialized {
            for addr in range.start..range.end.min(self.size) {
                written.0[(addr / 64) as usize] &= !(1 << (addr % 64));
            }
        }
        self.written = Some(Box::new(written));
    }

    /// Stops tracking writes.
    pub fn untrack_writes(&mut self) {
        self.written = None;
    }

    /// Whether the byte at physical address `addr` was written since tracking began (or counted as
    /// initialized then); true when nothing is tracked.
    pub fn was_written(&self, addr: u32) -> bool {
        self.written.as_deref().is_none_or(|w| w.get(self.gate(addr)))
    }

    #[inline(always)]
    fn mark(&mut self, addr: u32) {
        if self.written.is_some() {
            self.mark_written(addr);
        }
    }

    /// Kept out of line: tracking is off in ordinary runs, and the write path stays small.
    #[cold]
    #[inline(never)]
    fn mark_written(&mut self, addr: u32) {
        if let Some(written) = self.written.as_deref_mut() {
            written.set(addr);
        }
    }

    /// Physical memory size in bytes.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Physical addresses where two snapshots differ, ascending, at most `max` of them.
    pub fn differences(&self, other: &Memory, max: usize) -> Vec<u32> {
        let mut found = Vec::new();
        let compare = |base: u32, a: &[u8], b: &[u8], found: &mut Vec<u32>| {
            for (chunk, (x, y)) in a.chunks(64).zip(b.chunks(64)).enumerate() {
                if x != y {
                    let start = base + chunk as u32 * 64;
                    found.extend((0..x.len() as u32).filter(|&i| x[i as usize] != y[i as usize]).map(|i| start + i));
                }
                if found.len() >= max {
                    return;
                }
            }
        };
        compare(0, &self.bytes, &other.bytes, &mut found);
        let zero = [0u8; PAGE_SIZE];
        for (page, (a, b)) in self.extended.iter().zip(&other.extended).enumerate() {
            if found.len() >= max {
                break;
            }
            let (a, b) = (a.as_deref().unwrap_or(&zero), b.as_deref().unwrap_or(&zero));
            if a != b {
                compare((page * PAGE_SIZE) as u32, a, b, &mut found);
            }
        }
        found.truncate(max);
        found
    }

    /// A physical read, as DMA or an XMS move does it: no A20 wrapping, and the whole memory
    /// size is reachable. Beyond the end of memory reads FFh.
    pub fn read_phys(&self, addr: u32) -> u8 {
        if (addr as usize) < self.bytes.len() {
            self.bytes[addr as usize]
        } else if addr < self.size {
            let page = addr as usize / PAGE_SIZE;
            self.extended[page].as_ref().map_or(0, |data| data[addr as usize % PAGE_SIZE])
        } else {
            0xFF
        }
    }

    /// A physical write. Writes to the BIOS ROM and beyond the end of memory are ignored.
    pub fn write_phys(&mut self, addr: u32, value: u8) {
        self.mark(addr);
        if (addr as usize) < self.bytes.len() {
            if !(ROM_START as usize..CONVENTIONAL_SPACE).contains(&(addr as usize)) {
                self.bytes[addr as usize] = value;
            }
        } else if addr < self.size {
            let page = addr as usize / PAGE_SIZE;
            let data = self.extended[page].get_or_insert_with(|| vec![0; PAGE_SIZE].into_boxed_slice());
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
        self.mark(addr);
        if (addr as usize) < self.bytes.len() {
            if !(ROM_START as usize..CONVENTIONAL_SPACE).contains(&(addr as usize)) {
                self.bytes[addr as usize] = value;
            }
        } else {
            self.write_extended(addr, value);
        }
    }

    /// Above the real-mode image: a direct page lookup, kept out of line so that the
    /// real-mode path stays small enough to inline.
    #[cold]
    #[inline(never)]
    fn read_extended(&self, addr: u32) -> u8 {
        if addr >= self.size {
            return 0xFF;
        }
        let at = addr as usize;
        self.extended[at / PAGE_SIZE].as_ref().map_or(0, |page| page[at % PAGE_SIZE])
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

    /// Where `N` bytes at `addr` lie in one stored block, if they do: in the real-mode image
    /// (after the A20 gate, without wrapping) or in one extended page. `write` also excludes
    /// the ROM. The CPU's multi-byte accesses use this to skip the byte-by-byte path.
    #[inline]
    fn block<const N: usize>(&self, addr: u32, write: bool) -> Option<(Option<usize>, usize)> {
        let last = addr.checked_add(N as u32 - 1)?;
        let start = self.gate(addr) as usize;
        if (start as u32) + (N as u32 - 1) != self.gate(last) {
            return None;
        }
        if start + N <= self.bytes.len() {
            let rom = ROM_START as usize..CONVENTIONAL_SPACE;
            return (!write || start + N <= rom.start || start >= rom.end).then_some((None, start));
        }
        let (page, at) = (start / PAGE_SIZE, start % PAGE_SIZE);
        (start >= self.bytes.len() && last < self.size && at + N <= PAGE_SIZE).then_some((Some(page), at))
    }

    /// A CPU read of `N` consecutive bytes, the same as `N` calls of [`Memory::read_u8`].
    #[inline]
    pub fn read_n<const N: usize>(&self, addr: u32) -> [u8; N] {
        match self.block::<N>(addr, false) {
            Some((None, at)) => self.bytes[at..at + N].try_into().unwrap(),
            Some((Some(page), at)) => match &self.extended[page] {
                Some(data) => data[at..at + N].try_into().unwrap(),
                None => [0; N],
            },
            None => std::array::from_fn(|i| self.read_u8(addr.wrapping_add(i as u32))),
        }
    }

    /// A CPU write of `N` consecutive bytes, the same as `N` calls of [`Memory::write_u8`].
    #[inline]
    pub fn write_n<const N: usize>(&mut self, addr: u32, value: [u8; N]) {
        match self.block::<N>(addr, true) {
            Some((None, at)) => {
                if self.written.is_some() {
                    (0..N as u32).for_each(|i| self.mark(at as u32 + i));
                }
                self.bytes[at..at + N].copy_from_slice(&value)
            }
            Some((Some(page), at)) => {
                if self.written.is_some() {
                    let start = (page * PAGE_SIZE + at) as u32;
                    (0..N as u32).for_each(|i| self.mark(start + i));
                }
                let data = self.extended[page].get_or_insert_with(|| vec![0; PAGE_SIZE].into_boxed_slice());
                data[at..at + N].copy_from_slice(&value);
            }
            None => {
                for (i, byte) in value.into_iter().enumerate() {
                    self.write_u8(addr.wrapping_add(i as u32), byte);
                }
            }
        }
    }

    /// Copies `len` bytes starting at `addr` (wrapping like CPU accesses do).
    pub fn read_bytes(&self, addr: u32, len: usize) -> Vec<u8> {
        (0..len).map(|i| self.read_u8(addr + i as u32)).collect()
    }

    /// A host write: also reaches the ROM area. Used for loading programs and building the BIOS.
    pub fn load(&mut self, addr: u32, data: &[u8]) {
        for (i, &b) in data.iter().enumerate() {
            let at = self.index(addr + i as u32);
            self.mark(at as u32);
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
        assert_eq!(m.extended.iter().flatten().count(), 2, "only written pages are stored");
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
