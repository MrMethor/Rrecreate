//! Real-mode addressing.

/// Bytes reachable in real mode with the A20 line enabled: 1 MB plus the HMA
/// (FFFF:0010–FFFF:FFFF, 64 KB minus 16 bytes).
pub const REAL_MODE_SPACE: usize = 0x10_FFF0;

/// Bytes reachable with the A20 line disabled (addresses wrap at 1 MB).
pub const CONVENTIONAL_SPACE: usize = 0x10_0000;

/// Linear address of `seg:off` with A20 enabled (may reach into the HMA).
pub const fn linear(seg: u16, off: u16) -> u32 {
    ((seg as u32) << 4) + off as u32
}

/// Linear address of `seg:off` with A20 disabled: wraps at 1 MB like an 8086.
pub const fn linear_a20_off(seg: u16, off: u16) -> u32 {
    linear(seg, off) & 0xF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_matches_segment_arithmetic() {
        assert_eq!(linear(0x0A3C, 0x0112), 0x0A4D2);
        assert_eq!(linear(0xFFFF, 0xFFFF), 0x10_FFEF);
        assert_eq!(linear(0xFFFF, 0xFFFF) as usize, REAL_MODE_SPACE - 1);
    }

    #[test]
    fn a20_off_wraps_at_one_megabyte() {
        assert_eq!(linear_a20_off(0xFFFF, 0x0010), 0x00000);
        assert_eq!(linear_a20_off(0xFFFF, 0xFFFF), 0x0FFEF);
        assert_eq!(linear_a20_off(0x1234, 0x0005), 0x12345);
    }
}
