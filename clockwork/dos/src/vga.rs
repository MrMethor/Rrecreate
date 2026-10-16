//! VGA registers, DAC palette and display timing.
//!
//! Supported so far: text mode 3 and chained 256-color mode 13h, both read straight from the
//! memory image (B8000h and A0000h). Register writes for the planar modes (Mode X, EGA) are
//! stored but don't change how memory is interpreted yet (PROJECT.md §13, Phase 7).

use crate::clock::{FRAME_PIT_TICKS, LINES_PER_FRAME};

/// Scan lines (from the start of a frame) during which the vertical retrace pulse is active.
const RETRACE_LINES: u64 = 2;
/// Scan lines of vertical blanking, including the retrace; display starts after them.
const BLANK_LINES: u64 = 49;

pub const TEXT_BASE: u32 = 0xB_8000;
pub const GRAPHICS_BASE: u32 = 0xA_0000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Vga {
    pub mode: u8,
    /// 6-bit RGB per entry.
    pub dac: [[u8; 3]; 256],
    dac_write_index: u8,
    dac_read_index: u8,
    dac_write_component: u8,
    dac_read_component: u8,
    dac_latch: [u8; 3],
    dac_reading: bool,
    pub pel_mask: u8,
    attr_index: u8,
    attr_expect_data: bool,
    pub attr: [u8; 0x15],
    seq_index: u8,
    pub seq: [u8; 5],
    gc_index: u8,
    pub gc: [u8; 9],
    crtc_index: u8,
    pub crtc: [u8; 0x19],
    misc_output: u8,
}

impl Default for Vga {
    fn default() -> Self {
        Self::new()
    }
}

/// The 16 standard colors in 6-bit RGB.
const EGA_COLORS: [[u8; 3]; 16] = [
    [0, 0, 0],
    [0, 0, 42],
    [0, 42, 0],
    [0, 42, 42],
    [42, 0, 0],
    [42, 0, 42],
    [42, 21, 0],
    [42, 42, 42],
    [21, 21, 21],
    [21, 21, 63],
    [21, 63, 21],
    [21, 63, 63],
    [63, 21, 21],
    [63, 21, 63],
    [63, 63, 21],
    [63, 63, 63],
];

const GRAY_RAMP: [u8; 16] = [0, 5, 8, 11, 14, 17, 20, 24, 28, 32, 36, 40, 45, 50, 56, 63];

/// Entries 32–247 of the default 256-color palette: three intensities × three saturations, each
/// a ring of 24 hues (blue → magenta → red → yellow → green → cyan). Each row lists the five
/// component levels the ring is built from, lowest first.
const COLOR_RING_LEVELS: [[u8; 5]; 9] = [
    [0, 16, 31, 47, 63],
    [31, 39, 47, 55, 63],
    [45, 49, 54, 58, 63],
    [0, 7, 14, 21, 28],
    [14, 17, 21, 24, 28],
    [20, 22, 24, 26, 28],
    [0, 4, 8, 12, 16],
    [8, 10, 12, 14, 16],
    [11, 12, 13, 15, 16],
];

/// One 24-hue ring as indices into a level row (0 = lowest, 4 = highest), as (R, G, B).
const HUE_RING: [[usize; 3]; 24] = [
    [0, 0, 4],
    [1, 0, 4],
    [2, 0, 4],
    [3, 0, 4],
    [4, 0, 4],
    [4, 0, 3],
    [4, 0, 2],
    [4, 0, 1],
    [4, 0, 0],
    [4, 1, 0],
    [4, 2, 0],
    [4, 3, 0],
    [4, 4, 0],
    [3, 4, 0],
    [2, 4, 0],
    [1, 4, 0],
    [0, 4, 0],
    [0, 4, 1],
    [0, 4, 2],
    [0, 4, 3],
    [0, 4, 4],
    [0, 3, 4],
    [0, 2, 4],
    [0, 1, 4],
];

/// The palette the BIOS loads for mode 13h.
fn default_256_palette() -> [[u8; 3]; 256] {
    let mut dac = [[0; 3]; 256];
    dac[..16].copy_from_slice(&EGA_COLORS);
    for (i, &g) in GRAY_RAMP.iter().enumerate() {
        dac[16 + i] = [g; 3];
    }
    for (ring, levels) in COLOR_RING_LEVELS.iter().enumerate() {
        for (hue, rgb) in HUE_RING.iter().enumerate() {
            dac[32 + ring * 24 + hue] = rgb.map(|level| levels[level]);
        }
    }
    dac // 248–255 stay black
}

impl Vga {
    pub fn new() -> Self {
        let mut vga = Self {
            mode: 3,
            dac: [[0; 3]; 256],
            dac_write_index: 0,
            dac_read_index: 0,
            dac_write_component: 0,
            dac_read_component: 0,
            dac_latch: [0; 3],
            dac_reading: false,
            pel_mask: 0xFF,
            attr_index: 0,
            attr_expect_data: false,
            attr: [0; 0x15],
            seq_index: 0,
            seq: [0x03, 0x00, 0x03, 0x00, 0x02],
            gc_index: 0,
            gc: [0, 0, 0, 0, 0, 0x10, 0x0E, 0, 0xFF],
            crtc_index: 0,
            crtc: [0; 0x19],
            misc_output: 0x67,
        };
        vga.set_mode(3);
        vga
    }

    /// Resets the registers and palette the way the video BIOS does on a mode set.
    pub fn set_mode(&mut self, mode: u8) {
        self.mode = mode;
        self.pel_mask = 0xFF;
        if mode == 0x13 {
            self.dac = default_256_palette();
            self.seq[4] = 0x0E; // chain-4
            self.attr[0x10] = 0x41;
        } else {
            // Text modes: the 64-color EGA palette in the DAC, attribute registers mapping the
            // 16 text colors onto it.
            for i in 0..64u8 {
                let level = |hi: u8, lo: u8| ((i >> hi) & 1) * 42 + ((i >> lo) & 1) * 21;
                self.dac[usize::from(i)] = [level(2, 5), level(1, 4), level(0, 3)];
            }
            const TEXT_ATTR: [u8; 16] = [0, 1, 2, 3, 4, 5, 20, 7, 56, 57, 58, 59, 60, 61, 62, 63];
            self.attr[..16].copy_from_slice(&TEXT_ATTR);
            self.seq[4] = 0x02;
            self.attr[0x10] = 0x0C;
        }
    }

    /// Chained 256-color mode with chain-4 still enabled (what this module can display).
    pub fn is_chained_256(&self) -> bool {
        self.mode == 0x13 && self.seq[4] & 0x08 != 0
    }

    pub fn read(&mut self, port: u16, now: u64) -> u8 {
        match port {
            0x3C1 => self.attr.get(usize::from(self.attr_index & 0x1F)).copied().unwrap_or(0),
            0x3C5 => self.seq.get(usize::from(self.seq_index)).copied().unwrap_or(0),
            0x3C6 => self.pel_mask,
            0x3C7 => {
                if self.dac_reading {
                    0x03
                } else {
                    0x00
                }
            }
            0x3C8 => self.dac_write_index,
            0x3C9 => {
                let value = self.dac[usize::from(self.dac_read_index)][usize::from(self.dac_read_component)];
                self.dac_read_component += 1;
                if self.dac_read_component == 3 {
                    self.dac_read_component = 0;
                    self.dac_read_index = self.dac_read_index.wrapping_add(1);
                }
                value
            }
            0x3CC => self.misc_output,
            0x3CF => self.gc.get(usize::from(self.gc_index)).copied().unwrap_or(0),
            0x3D5 => self.crtc.get(usize::from(self.crtc_index)).copied().unwrap_or(0),
            0x3DA => {
                self.attr_expect_data = false;
                status(now)
            }
            0x3C4 => self.seq_index,
            0x3CE => self.gc_index,
            0x3D4 => self.crtc_index,
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, port: u16, value: u8) {
        match port {
            0x3C0 => {
                if self.attr_expect_data {
                    if let Some(r) = self.attr.get_mut(usize::from(self.attr_index & 0x1F)) {
                        *r = value;
                    }
                } else {
                    self.attr_index = value;
                }
                self.attr_expect_data = !self.attr_expect_data;
            }
            0x3C2 => self.misc_output = value,
            0x3C4 => self.seq_index = value,
            0x3C5 => {
                if let Some(r) = self.seq.get_mut(usize::from(self.seq_index)) {
                    *r = value;
                }
            }
            0x3C6 => self.pel_mask = value,
            0x3C7 => {
                self.dac_read_index = value;
                self.dac_read_component = 0;
                self.dac_reading = true;
            }
            0x3C8 => {
                self.dac_write_index = value;
                self.dac_write_component = 0;
                self.dac_reading = false;
            }
            0x3C9 => {
                self.dac_latch[usize::from(self.dac_write_component)] = value & 0x3F;
                self.dac_write_component += 1;
                if self.dac_write_component == 3 {
                    self.dac[usize::from(self.dac_write_index)] = self.dac_latch;
                    self.dac_write_component = 0;
                    self.dac_write_index = self.dac_write_index.wrapping_add(1);
                }
            }
            0x3CE => self.gc_index = value,
            0x3CF => {
                if let Some(r) = self.gc.get_mut(usize::from(self.gc_index)) {
                    *r = value;
                }
            }
            0x3D4 => self.crtc_index = value,
            0x3D5 => {
                if let Some(r) = self.crtc.get_mut(usize::from(self.crtc_index)) {
                    *r = value;
                }
            }
            _ => {}
        }
    }
}

/// Input status register 1 (3DAh) at PIT tick `now`: bit 3 = vertical retrace,
/// bit 0 = display disabled (horizontal or vertical blanking).
pub fn status(now: u64) -> u8 {
    let scaled = (now % FRAME_PIT_TICKS) * LINES_PER_FRAME;
    let line = scaled / FRAME_PIT_TICKS;
    let in_line = scaled % FRAME_PIT_TICKS;
    let hblank = in_line >= FRAME_PIT_TICKS * 4 / 5;
    let vblank = line < BLANK_LINES;
    let retrace = line < RETRACE_LINES;
    (u8::from(retrace) << 3) | u8::from(hblank || vblank)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dac_write_and_read_back() {
        let mut vga = Vga::new();
        vga.write(0x3C8, 10);
        for v in [63, 32, 1, 5, 6, 7] {
            vga.write(0x3C9, v);
        }
        assert_eq!(vga.dac[10], [63, 32, 1]);
        assert_eq!(vga.dac[11], [5, 6, 7]);
        vga.write(0x3C7, 11);
        let read: Vec<u8> = (0..3).map(|_| vga.read(0x3C9, 0)).collect();
        assert_eq!(read, [5, 6, 7]);
    }

    #[test]
    fn retrace_happens_once_per_frame() {
        let in_retrace: Vec<bool> = (0..FRAME_PIT_TICKS * 3).map(|t| status(t) & 0x08 != 0).collect();
        let starts = in_retrace.windows(2).filter(|w| !w[0] && w[1]).count();
        assert_eq!(starts, 2, "two rising edges after the first frame's");
        assert!(in_retrace[0]);
        let retrace_ticks = in_retrace.iter().take(FRAME_PIT_TICKS as usize).filter(|&&r| r).count();
        assert!((60..100).contains(&retrace_ticks), "about 2 lines of 449: {retrace_ticks}");
    }

    #[test]
    fn mode_13h_palette() {
        let mut vga = Vga::new();
        vga.set_mode(0x13);
        assert!(vga.is_chained_256());
        assert_eq!(vga.dac[15], [63, 63, 63]);
        assert_eq!(vga.dac[31], [63, 63, 63]);
        assert_eq!(vga.dac[32], [0, 0, 63], "blue starts the first hue ring");
        assert_eq!(vga.dac[40], [63, 0, 0]);
        assert_eq!(vga.dac[104], [0, 0, 28], "medium-intensity ring");
        assert_eq!(vga.dac[247], [11, 12, 16]);
        assert_eq!(vga.dac[248], [0, 0, 0]);
        vga.write(0x3C4, 4);
        vga.write(0x3C5, 0x06); // chain-4 off: Mode X
        assert!(!vga.is_chained_256());
    }
}
