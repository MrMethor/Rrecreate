//! The real-mode memory image (PROJECT.md §5.2).

use crate::addr::{CONVENTIONAL_SPACE, REAL_MODE_SPACE};

/// Start of the system BIOS ROM. Writes from the CPU to `ROM_START..CONVENTIONAL_SPACE` are
/// ignored, as on real hardware; the host fills the ROM through [`Memory::load`].
pub const ROM_START: u32 = 0xF_0000;

/// 1 MB plus the HMA. `Clone` is a full snapshot (about 1.1 MB).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Memory {
    bytes: Box<[u8]>,
    a20: bool,
}

impl std::fmt::Debug for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Memory").field("size", &self.bytes.len()).field("a20", &self.a20).finish()
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl Memory {
    /// Zero-filled, with the A20 line disabled (addresses wrap at 1 MB, as after boot).
    pub fn new() -> Self {
        Self { bytes: vec![0; REAL_MODE_SPACE].into_boxed_slice(), a20: false }
    }

    pub fn a20(&self) -> bool {
        self.a20
    }

    pub fn set_a20(&mut self, enabled: bool) {
        self.a20 = enabled;
    }

    fn index(&self, addr: u32) -> usize {
        let addr = addr as usize;
        if self.a20 && addr < REAL_MODE_SPACE { addr } else { addr % CONVENTIONAL_SPACE }
    }

    pub fn read_u8(&self, addr: u32) -> u8 {
        self.bytes[self.index(addr)]
    }

    /// A CPU write. Writes into the BIOS ROM are ignored.
    pub fn write_u8(&mut self, addr: u32, value: u8) {
        let i = self.index(addr);
        if !(ROM_START as usize..CONVENTIONAL_SPACE).contains(&i) {
            self.bytes[i] = value;
        }
    }

    pub fn read_u16(&self, addr: u32) -> u16 {
        u16::from_le_bytes([self.read_u8(addr), self.read_u8(addr + 1)])
    }

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
    fn rom_is_read_only_for_the_cpu() {
        let mut m = Memory::new();
        m.load(ROM_START, &[0x90]);
        m.write_u8(ROM_START, 0xCC);
        assert_eq!(m.read_u8(ROM_START), 0x90);
        m.write_u16(0xA_0000, 0x1234);
        assert_eq!(m.read_u16(0xA_0000), 0x1234);
    }
}
