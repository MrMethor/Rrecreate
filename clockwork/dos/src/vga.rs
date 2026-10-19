//! VGA: registers, DAC palette, display timing and the 256 KB of planar video memory.
//!
//! Video memory access is ported from DOSBox Staging `src/hardware/video/vga_memory.cpp` and the
//! register semantics from `vga_gfx.cpp`, `vga_seq.cpp`, `vga_crtc.cpp` and `vga_attr.cpp`
//! (GPL-2.0-or-later). As in DOSBox, chain-4 (mode 13h) accesses go straight to memory, while
//! unchained accesses (planar EGA modes, Mode X) go through the latches, write modes 0-3, the
//! logical operations, bit mask and map mask, and read modes 0-1.
//!
//! The CPU reaches this memory through the A0000-AFFFF window (when the memory map select allows
//! it). Text and CGA modes keep their screen in ordinary RAM at B0000-BFFFF, read by the
//! renderer in `vga_draw.rs`; the text font lives in plane 2, as on real hardware.
//!
//! The card is an S3 Trio64 with 4 MB, as DOSBox's default SVGA card. Its extended registers
//! ([`S3`]) are ported from DOSBox Staging `src/hardware/video/vga_s3.cpp` and the SVGA memory
//! access from `vga_memory.cpp` (GPL-2.0-or-later): with enhanced mapping (CR31 bit 3) the
//! window reaches all of video memory through a 64 KB bank (CR6A), chained accesses address it
//! linearly, and the linear frame buffer at [`LFB_BASE`] shows it whole. The hardware cursor
//! and the graphics engine are not emulated.

use crate::clock::{FRAME_PIT_TICKS, LINES_PER_FRAME};
use crate::hash::fnv64;

/// Scan lines (from the start of a frame) during which the vertical retrace pulse is active.
const RETRACE_LINES: u64 = 2;
/// Scan lines of vertical blanking, including the retrace; display starts after them.
const BLANK_LINES: u64 = 49;

pub const TEXT_BASE: u32 = 0xB_8000;
pub const GRAPHICS_BASE: u32 = 0xA_0000;
/// Video memory on the card.
pub const VRAM_SIZE: usize = 4 * 1024 * 1024;
/// The part VGA-compatible addressing reaches (DOSBox's `vmemwrap` without enhanced mapping).
pub const VGA_MEMORY: usize = 256 * 1024;
/// Physical address of the linear frame buffer, which the BIOS programs into CR59/CR5A
/// (DOSBox `PciGfxLfbBase`).
pub const LFB_BASE: u32 = 0xC000_0000;

/// Video memory. In VGA-compatible modes it is four planes, interleaved (byte
/// `offset * 4 + plane`), as in DOSBox; SVGA modes address the same bytes linearly.
#[derive(Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Vram(Box<[u8]>);

impl std::fmt::Debug for Vram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Vram({:016x})", fnv64(&self.0))
    }
}

impl std::ops::Deref for Vram {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl std::ops::DerefMut for Vram {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

/// How the attribute and graphics controllers interpret memory (DOSBox `VGA_DetermineMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DisplayKind {
    Text,
    /// CGA 4-color graphics (modes 4, 5), from RAM at B8000.
    Cga4,
    /// CGA 2-color graphics (mode 6), from RAM at B8000.
    Cga2,
    /// 16-color planar graphics (modes 0Dh-12h).
    Ega,
    /// 256-color graphics: mode 13h (chain-4) and Mode X (unchained).
    Vga256,
    /// 16-color planar SVGA graphics beyond 256 KB (DOSBox `M_LIN4`).
    Svga16,
    /// 256-color SVGA graphics, one byte per pixel (`M_LIN8`).
    Svga256,
    /// 15-bit high color, 5:5:5 (`M_LIN15`).
    HiColor15,
    /// 16-bit high color, 5:6:5 (`M_LIN16`).
    HiColor16,
    /// 24-bit true color, blue-green-red bytes (`M_LIN24`).
    TrueColor24,
    /// 32-bit true color, blue-green-red-unused bytes (`M_LIN32`).
    TrueColor32,
}

/// The S3 Trio64's extended registers: CRTC indexes 2Dh-6Bh and sequencer indexes 08h-18h
/// (DOSBox `SVGA_S3_WriteCRTC`, `SVGA_S3_ReadCRTC`, `SVGA_S3_WriteSEQ`, `SVGA_S3_ReadSEQ`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct S3 {
    /// CR31 memory configuration; bit 3 enables enhanced mapping (memory above 256 KB).
    pub reg_31: u8,
    reg_35: u8,
    reg_lock1: u8,
    reg_lock2: u8,
    /// CR3A; bit 4 selects the enhanced 256-color mode (8 pixels per character clock).
    pub reg_3a: u8,
    reg_40: u8,
    reg_41: u8,
    reg_43: u8,
    cursor_mode: u8,
    cursor_origin: [u16; 2],
    cursor_colors: [[u8; 3]; 2],
    cursor_stack: [usize; 2],
    cursor_start: u16,
    cursor_pattern: [u8; 2],
    reg_50: u8,
    reg_51: u8,
    reg_52: u8,
    reg_53: u8,
    reg_55: u8,
    reg_58: u8,
    /// CR59/CR5A: the linear frame buffer's physical address bits 16-31 (0 = not mapped yet).
    pub la_window: u16,
    /// CR5D/CR5E: bit 8 of the horizontal and bit 10 of the vertical timing registers.
    pub hor_overflow: u8,
    pub ver_overflow: u8,
    reg_63: u8,
    /// CR67 bits 4-7: pixel format (0 and 1: 8-bit, 3: 15-bit, 5: 16-bit, 7: 24-bit, 13: 32-bit).
    pub misc_control_2: u8,
    reg_6b: u8,
    /// The 64 KB bank the A0000 window shows (CR6A; also CR35 bits 0-3 and CR51 bits 2-3).
    pub bank: u8,
    /// Display start address bits 16-20 (CR31 bits 4-5, CR51 bits 0-1, CR69).
    pub start_high: u8,
    /// Offset register bits 8-9 (CR51 bits 4-5, CR43 bit 2).
    pub scan_high: u8,
    /// SR08: 06h unlocks sequencer registers 09h and up.
    pll_lock: u8,
    /// SR10-SR18 (clock synthesizer), as written.
    sr: [u8; 0x19],
}

/// CR36 for 4 MB of fast-page DRAM (DOSBox `SVGA_Setup_S3`).
const S3_REG_36: u8 = 0b0001_1110;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Vga {
    /// The mode number of the last BIOS mode set, VESA modes included (informational; drawing
    /// follows the registers).
    pub mode: u16,
    /// The mode VBE function 4F02h set, with its flags (what 4F03h reports); cleared by any
    /// other mode set.
    pub vesa_mode: Option<u16>,
    /// 6-bit RGB per entry.
    #[serde(with = "serde_big_array::BigArray")]
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
    pub misc_output: u8,
    pub s3: S3,
    pub vram: Vram,
    latch: u32,
    /// Set by anything that can change the picture; cleared when a frame is fingerprinted.
    pub(crate) changed: bool,
    /// Fingerprint of the video-memory part of the picture as of the last `changed`.
    pub(crate) display_hash: u64,
}

impl Default for Vga {
    fn default() -> Self {
        Self::new()
    }
}

/// The 16 CGA colors in 6-bit RGB, as VGA cards emulate them.
const CGA_COLORS: [[u8; 3]; 16] = [
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

/// The palette the BIOS loads for 256-color modes.
pub fn default_256_palette() -> [[u8; 3]; 256] {
    let mut dac = [[0; 3]; 256];
    dac[..16].copy_from_slice(&CGA_COLORS);
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

/// The 64-color EGA palette (bits rgbRGB) in the first 64 DAC entries: text, 350/480-line modes.
pub fn ega_palette() -> [[u8; 3]; 64] {
    std::array::from_fn(|i| {
        let level = |hi: usize, lo: usize| (((i >> hi) & 1) * 42 + ((i >> lo) & 1) * 21) as u8;
        [level(2, 5), level(1, 4), level(0, 3)]
    })
}

/// The CGA colors spread over 64 entries (bit 4 = intensity): 200-line graphics modes.
pub fn cga64_palette() -> [[u8; 3]; 64] {
    std::array::from_fn(|i| CGA_COLORS[(i & 7) | ((i >> 1) & 8)])
}

/// Each plane's byte set to `value`.
fn expand(value: u8) -> u32 {
    u32::from(value) * 0x0101_0101
}

/// 0xFF in the bytes of the planes whose bit is set in `planes`.
fn fill(planes: u8) -> u32 {
    (0..4).filter(|p| planes & (1 << p) != 0).fold(0, |acc, p| acc | (0xFF << (8 * p)))
}

/// Where a chain-4 CPU address lands in interleaved memory (DOSBox `ToLinear`).
pub fn chained_index(address: u32) -> usize {
    (((address & !3) << 2) + (address & 3)) as usize & (VRAM_SIZE - 1)
}

/// How a CPU access in the window reaches memory (DOSBox's page handler choice in
/// `VGA_SetupHandlers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Access {
    /// Chain-4 in VGA-compatible addressing: interleaved, plane = address & 3.
    Chained,
    /// Enhanced chain-4 and the direct-color modes: byte for byte.
    Linear,
    /// Through the latches and write modes, four planes per address.
    Planar,
}

/// Font block offsets in plane 2, selected by the character map select register.
pub const FONT_BLOCKS: [u32; 8] = [0x0000, 0x4000, 0x8000, 0xC000, 0x2000, 0x6000, 0xA000, 0xE000];

impl Vga {
    pub fn new() -> Self {
        Self {
            mode: 3,
            vesa_mode: None,
            dac: [[0; 3]; 256],
            dac_write_index: 0,
            dac_read_index: 0,
            dac_write_component: 0,
            dac_read_component: 0,
            dac_latch: [0; 3],
            dac_reading: false,
            pel_mask: 0xFF,
            attr_index: 0x20,
            attr_expect_data: false,
            attr: [0; 0x15],
            seq_index: 0,
            seq: [0x03, 0x00, 0x03, 0x00, 0x02],
            gc_index: 0,
            gc: [0, 0, 0, 0, 0, 0x10, 0x0E, 0, 0xFF],
            crtc_index: 0,
            crtc: [0; 0x19],
            misc_output: 0x67,
            s3: S3::default(),
            vram: Vram(vec![0; VRAM_SIZE].into_boxed_slice()),
            latch: 0,
            changed: true,
            display_hash: 0,
        }
    }

    /// Loads one entry of the BIOS parameter table into the registers (`vga_params.rs` layout).
    pub fn load_parameters(&mut self, params: &[u8; 64]) {
        self.seq[0] = 0x03;
        self.seq[1..5].copy_from_slice(&params[5..9]);
        self.misc_output = params[9];
        self.crtc.copy_from_slice(&params[10..35]);
        self.attr[..0x14].copy_from_slice(&params[35..55]);
        self.attr[0x14] = 0;
        self.gc.copy_from_slice(&params[55..64]);
        self.attr_index = 0x20;
        self.attr_expect_data = false;
        self.pel_mask = 0xFF;
        self.changed = true;
    }

    /// DOSBox `VGA_DetermineMode`: the S3 pixel format (CR67) first, then the attribute and
    /// graphics controllers; enhanced mapping (CR31 bit 3) turns the 16- and 256-color modes
    /// into their SVGA forms.
    pub fn display_kind(&self) -> DisplayKind {
        match self.s3.misc_control_2 >> 4 {
            1 => return DisplayKind::Svga256,
            3 => return DisplayKind::HiColor15,
            5 => return DisplayKind::HiColor16,
            7 => return DisplayKind::TrueColor24,
            13 => return DisplayKind::TrueColor32,
            _ => {}
        }
        let enhanced = self.s3.reg_31 & 0x08 != 0;
        if self.attr[0x10] & 0x01 == 0 {
            DisplayKind::Text
        } else if self.gc[5] & 0x40 != 0 {
            if enhanced { DisplayKind::Svga256 } else { DisplayKind::Vga256 }
        } else if self.gc[5] & 0x20 != 0 {
            DisplayKind::Cga4
        } else if self.gc[6] & 0x0C == 0x0C {
            DisplayKind::Cga2
        } else if enhanced {
            DisplayKind::Svga16
        } else {
            DisplayKind::Ega
        }
    }

    fn chain4(&self) -> bool {
        self.seq[4] & 0x08 != 0
    }

    /// Video memory addressing wraps at this size: all of it with enhanced mapping, 256 KB
    /// otherwise (DOSBox `vmemwrap`).
    pub fn wrap(&self) -> usize {
        if self.s3.reg_31 & 0x08 != 0 { VRAM_SIZE } else { VGA_MEMORY }
    }

    /// The window's bytes per memory map select: A0000-BFFFF for map 0 (S3), A0000-AFFFF for
    /// map 1. Maps 2 and 3 (B0000, B8000) are the text and CGA screens in RAM.
    pub fn window_size(&self) -> Option<u32> {
        match (self.gc[6] >> 2) & 3 {
            0 => Some(0x2_0000),
            1 => Some(0x1_0000),
            _ => None,
        }
    }

    fn access(&self) -> Access {
        // The direct-color pixel formats of CR67 (as in `display_kind`), checked cheaply: this
        // runs on every access.
        if matches!(self.s3.misc_control_2 >> 4, 3 | 5 | 7 | 13) {
            Access::Linear
        } else if !self.chain4() {
            Access::Planar
        } else if self.s3.reg_31 & 0x08 != 0 {
            Access::Linear
        } else {
            Access::Chained
        }
    }

    /// Byte offset in video memory of window offset `offset`, after the bank.
    fn banked(&self, offset: u32) -> u32 {
        offset + u32::from(self.s3.bank) * 0x1_0000
    }

    /// The four plane bytes at `offset`, plane 0 in the low byte.
    pub fn planes(&self, offset: u32) -> u32 {
        let at = (offset as usize & (self.wrap() / 4 - 1)) * 4;
        u32::from_le_bytes([self.vram[at], self.vram[at + 1], self.vram[at + 2], self.vram[at + 3]])
    }

    fn set_planes(&mut self, offset: u32, value: u32) {
        let at = (offset as usize & (self.wrap() / 4 - 1)) * 4;
        self.vram[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// A byte of the linear frame buffer (`offset` from [`LFB_BASE`]).
    pub fn lfb_read(&self, offset: u32) -> u8 {
        self.vram[offset as usize & (self.wrap() - 1)]
    }

    pub fn lfb_write(&mut self, offset: u32, value: u8) {
        let at = offset as usize & (self.wrap() - 1);
        self.vram[at] = value;
        self.changed = true;
    }

    /// Where the linear frame buffer is, once the BIOS has programmed it.
    pub fn lfb_range(&self) -> Option<std::ops::Range<u32>> {
        let base = u32::from(self.s3.la_window) << 16;
        (base != 0).then(|| base..base.saturating_add(VRAM_SIZE as u32))
    }

    fn raster_op(&self, input: u32, mask: u32) -> u32 {
        match (self.gc[3] >> 3) & 3 {
            0 => (input & mask) | (self.latch & !mask),
            1 => (input | !mask) & self.latch,
            2 => (input & mask) | self.latch,
            _ => (input & mask) ^ self.latch,
        }
    }

    /// The four plane bytes a CPU write of `value` produces, before the map mask.
    fn mode_operation(&self, value: u8) -> u32 {
        let rotate = u32::from(self.gc[3] & 7);
        let bit_mask = expand(self.gc[8]);
        match self.gc[5] & 3 {
            0 => {
                let enable = fill(self.gc[1] & 0x0F);
                let full = (expand(value.rotate_right(rotate)) & !enable) | fill(self.gc[0] & self.gc[1] & 0x0F);
                self.raster_op(full, bit_mask)
            }
            1 => self.latch,
            2 => self.raster_op(fill(value & 0x0F), bit_mask),
            _ => self.raster_op(fill(self.gc[0] & 0x0F), expand(value.rotate_right(rotate)) & bit_mask),
        }
    }

    /// A CPU read at `offset` in the window: loads the latches (unchained) and applies the
    /// read mode.
    pub fn cpu_read(&mut self, offset: u32) -> u8 {
        let offset = self.banked(offset);
        match self.access() {
            Access::Chained => return self.vram[chained_index(offset & (VGA_MEMORY as u32 - 1))],
            Access::Linear => return self.vram[offset as usize & (self.wrap() - 1)],
            Access::Planar => {}
        }
        self.latch = self.planes(offset);
        if self.gc[5] & 0x08 == 0 {
            (self.latch >> (8 * (self.gc[4] & 3))) as u8
        } else {
            // Color compare: a bit is set where every plane not marked "don't care" matches.
            let dont_care = self.gc[7] & 0x0F;
            let t = (self.latch & fill(dont_care)) ^ fill(self.gc[2] & dont_care);
            !((t | (t >> 8) | (t >> 16) | (t >> 24)) as u8)
        }
    }

    /// A CPU write at `offset` in the window.
    pub fn cpu_write(&mut self, offset: u32, value: u8) {
        let offset = self.banked(offset);
        self.changed = true;
        match self.access() {
            Access::Chained => {
                self.vram[chained_index(offset & (VGA_MEMORY as u32 - 1))] = value;
                return;
            }
            Access::Linear => {
                let at = offset as usize & (self.wrap() - 1);
                self.vram[at] = value;
                return;
            }
            Access::Planar => {}
        }
        let data = self.mode_operation(value);
        let map_mask = fill(self.seq[2] & 0x0F);
        let old = self.planes(offset);
        self.set_planes(offset, (old & !map_mask) | (data & map_mask));
    }

    /// A CPU write of `N` bytes at `offset` in the window, the same as `N` byte writes. In
    /// chain-4 and linear access they land on consecutive bytes of video memory (an aligned
    /// chain-4 word or doubleword fills one interleaved group), so they are copied at once; the
    /// planar path goes byte by byte through the latches and write modes.
    pub fn cpu_write_n<const N: usize>(&mut self, offset: u32, value: [u8; N]) {
        let banked = self.banked(offset);
        let at = match self.access() {
            Access::Chained if (banked & 3) as usize + N <= 4 => chained_index(banked & (VGA_MEMORY as u32 - 1)),
            Access::Linear => banked as usize & (self.wrap() - 1),
            _ => usize::MAX,
        };
        if at.checked_add(N).is_some_and(|end| end <= self.vram.len()) {
            self.vram[at..at + N].copy_from_slice(&value);
            self.changed = true;
        } else {
            for (i, byte) in value.into_iter().enumerate() {
                self.cpu_write(offset + i as u32, byte);
            }
        }
    }

    /// What a CPU read would return, without loading the latches (for tools and tests).
    pub fn peek(&self, offset: u32) -> u8 {
        let offset = self.banked(offset);
        match self.access() {
            Access::Chained => self.vram[chained_index(offset & (VGA_MEMORY as u32 - 1))],
            Access::Linear => self.vram[offset as usize & (self.wrap() - 1)],
            Access::Planar => (self.planes(offset) >> (8 * (self.gc[4] & 3))) as u8,
        }
    }

    /// Writes character patterns into a font block of plane 2, 32 bytes per character, as the
    /// BIOS font loader does (DOSBox `load_font`).
    pub fn load_font(&mut self, block: u8, first: usize, height: usize, glyphs: &[u8]) {
        let base = FONT_BLOCKS[usize::from(block & 7)] as usize;
        for (i, glyph) in glyphs.chunks(height.max(1)).enumerate() {
            for (line, &bits) in glyph.iter().enumerate() {
                let offset = (base + (first + i) * 32 + line) & 0xFFFF;
                self.vram[offset * 4 + 2] = bits;
            }
        }
        self.changed = true;
    }

    /// Plane 2 byte `offset` (font data).
    pub fn font_byte(&self, offset: u32) -> u8 {
        self.vram[(offset as usize & 0xFFFF) * 4 + 2]
    }

    /// Clears all of video memory.
    pub fn clear_memory(&mut self) {
        self.vram.fill(0);
        self.changed = true;
    }

    /// I/O ports 3B4h/3B5h/3BAh answer when the CRTC is at the monochrome address
    /// (miscellaneous output bit 0 clear), 3D4h/3D5h/3DAh when it is at the color address.
    fn crtc_port(&self, port: u16) -> Option<u16> {
        let color = self.misc_output & 1 != 0;
        match port {
            0x3B4 | 0x3B5 | 0x3BA if !color => Some(port + 0x20),
            0x3D4 | 0x3D5 | 0x3DA if color => Some(port),
            0x3B0..=0x3BF | 0x3D0..=0x3DF => None,
            _ => Some(port),
        }
    }

    pub fn read(&mut self, port: u16, now: u64) -> u8 {
        let Some(port) = self.crtc_port(port) else { return 0xFF };
        match port {
            0x3C0 => self.attr_index,
            0x3C1 => self.attr.get(usize::from(self.attr_index & 0x1F)).copied().unwrap_or(0),
            0x3C2 => 0x70,
            0x3C4 => self.seq_index,
            0x3C5 => match self.seq.get(usize::from(self.seq_index)) {
                Some(&value) => value,
                None => self.s3_read_seq(self.seq_index),
            },
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
            0x3CA => 0x00,
            0x3CC => self.misc_output,
            0x3CE => self.gc_index,
            0x3CF => self.gc.get(usize::from(self.gc_index)).copied().unwrap_or(0),
            0x3D4 => self.crtc_index,
            0x3D5 => match self.crtc.get(usize::from(self.crtc_index)) {
                Some(&value) => value,
                None => self.s3_read_crtc(self.crtc_index),
            },
            0x3DA => {
                self.attr_expect_data = false;
                status(now)
            }
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, port: u16, value: u8) {
        let Some(port) = self.crtc_port(port) else { return };
        self.changed = true;
        match port {
            0x3C0 => {
                if self.attr_expect_data {
                    if let Some(r) = self.attr.get_mut(usize::from(self.attr_index & 0x1F)) {
                        *r = value;
                    }
                } else {
                    self.attr_index = value & 0x3F;
                }
                self.attr_expect_data = !self.attr_expect_data;
            }
            0x3C2 => self.misc_output = value,
            0x3C4 => self.seq_index = value,
            0x3C5 => match self.seq.get_mut(usize::from(self.seq_index)) {
                Some(r) => *r = value,
                None => self.s3_write_seq(self.seq_index, value),
            },
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
                let index = usize::from(self.crtc_index);
                // CRTC registers 0-7 are write-protected by bit 7 of register 11h, except the
                // line compare bit 8 in register 7.
                if index <= 7 && self.crtc[0x11] & 0x80 != 0 {
                    if index == 7 {
                        self.crtc[7] = (self.crtc[7] & !0x10) | (value & 0x10);
                    }
                } else if let Some(r) = self.crtc.get_mut(index) {
                    *r = value;
                } else {
                    self.s3_write_crtc(self.crtc_index, value);
                }
            }
            _ => {}
        }
    }

    fn s3_write_crtc(&mut self, index: u8, value: u8) {
        let s3 = &mut self.s3;
        match index {
            0x31 => {
                s3.reg_31 = value;
                s3.start_high = (s3.start_high & !0x03) | ((value >> 4) & 0x03);
            }
            0x35 => {
                // Only with the registers unlocked (uvconfig's detection depends on it).
                if s3.reg_lock1 == 0x48 {
                    s3.reg_35 = value & 0xF0;
                    s3.bank = (s3.bank & 0xF0) | (value & 0x0F);
                }
            }
            0x38 => s3.reg_lock1 = value,
            0x39 => s3.reg_lock2 = value,
            0x3A => s3.reg_3a = value,
            0x40 => s3.reg_40 = value,
            0x41 => s3.reg_41 = value,
            0x43 => {
                s3.reg_43 = value & !0x04;
                s3.scan_high = (s3.scan_high & !0x01) | ((value >> 2) & 0x01);
            }
            0x45 => s3.cursor_mode = value,
            0x46 => s3.cursor_origin[0] = (s3.cursor_origin[0] & 0x00FF) | (u16::from(value) << 8),
            0x47 => s3.cursor_origin[0] = (s3.cursor_origin[0] & 0xFF00) | u16::from(value),
            0x48 => s3.cursor_origin[1] = (s3.cursor_origin[1] & 0x00FF) | (u16::from(value) << 8),
            0x49 => s3.cursor_origin[1] = (s3.cursor_origin[1] & 0xFF00) | u16::from(value),
            0x4A | 0x4B => {
                let which = usize::from(index - 0x4A);
                if s3.cursor_stack[which] > 2 {
                    s3.cursor_stack[which] = 0;
                }
                s3.cursor_colors[which][s3.cursor_stack[which]] = value;
                s3.cursor_stack[which] += 1;
            }
            0x4C => {
                let start = (s3.cursor_start & 0xFF) | (u16::from(value & 0x0F) << 8);
                // A pattern beyond video memory is refused, as in DOSBox.
                if (usize::from(start) << 10) + 64 * 64 * 2 / 8 <= VRAM_SIZE {
                    s3.cursor_start = start;
                }
            }
            0x4D => s3.cursor_start = (s3.cursor_start & 0xFF00) | u16::from(value),
            0x4E => s3.cursor_pattern[0] = value & 0x3F,
            0x4F => s3.cursor_pattern[1] = value & 0x3F,
            0x50 => s3.reg_50 = value,
            0x51 => {
                s3.reg_51 = value & 0xC0;
                s3.start_high = (s3.start_high & !0x0C) | ((value & 0x03) << 2);
                s3.bank = (s3.bank & 0xCF) | ((value & 0x0C) << 2);
                s3.scan_high = (value >> 4) & 0x03;
            }
            0x52 => s3.reg_52 = value,
            0x53 => s3.reg_53 = value,
            0x55 => s3.reg_55 = value,
            0x58 => s3.reg_58 = value,
            0x59 => s3.la_window = (s3.la_window & 0x00FF) | (u16::from(value) << 8),
            0x5A => s3.la_window = (s3.la_window & 0xFF00) | u16::from(value),
            0x5D => s3.hor_overflow = value,
            0x5E => s3.ver_overflow = value,
            0x63 => s3.reg_63 = value,
            0x67 => s3.misc_control_2 = value,
            0x69 => s3.start_high = value & 0x1F,
            0x6A => s3.bank = value & 0x7F,
            0x6B => s3.reg_6b = value,
            _ => {}
        }
    }

    fn s3_read_crtc(&mut self, index: u8) -> u8 {
        let s3 = &mut self.s3;
        match index {
            0x24 | 0x26 => self.attr_index & 0x3F,
            0x2D => 0x88, // extended chip ID: PCI device ID high byte
            0x2E => 0x11, // Trio64
            0x2F => 0x00,
            0x30 => 0xE1,
            0x31 => s3.reg_31,
            0x35 => s3.reg_35 | (s3.bank & 0x0F),
            0x36 => S3_REG_36,
            0x37 => 0x2B,
            0x38 => s3.reg_lock1,
            0x39 => s3.reg_lock2,
            0x3A => s3.reg_3a,
            0x40 => s3.reg_40,
            0x41 => s3.reg_41,
            0x42 => 0x0D, // not interlaced
            0x43 => s3.reg_43 | ((s3.scan_high & 0x01) << 2),
            0x45 => {
                s3.cursor_stack = [0, 0];
                s3.cursor_mode | 0xA0
            }
            0x46 => (s3.cursor_origin[0] >> 8) as u8,
            0x47 => s3.cursor_origin[0] as u8,
            0x48 => (s3.cursor_origin[1] >> 8) as u8,
            0x49 => s3.cursor_origin[1] as u8,
            0x4A | 0x4B => {
                let which = usize::from(index - 0x4A);
                s3.cursor_colors[which][s3.cursor_stack[which].min(2)]
            }
            0x50 => s3.reg_50,
            0x51 => ((s3.start_high >> 2) & 0x03) | ((s3.bank & 0x30) >> 2) | ((s3.scan_high & 0x03) << 4) | s3.reg_51,
            0x52 => s3.reg_52,
            0x53 => s3.reg_53,
            0x55 => s3.reg_55,
            0x58 => s3.reg_58,
            0x59 => (s3.la_window >> 8) as u8,
            0x5A => s3.la_window as u8,
            0x5D => s3.hor_overflow,
            0x5E => s3.ver_overflow,
            0x63 => s3.reg_63,
            0x67 => s3.misc_control_2,
            0x69 => s3.start_high & 0x1F,
            0x6A => s3.bank & 0x7F,
            0x6B => s3.reg_6b,
            _ => 0,
        }
    }

    fn s3_write_seq(&mut self, index: u8, value: u8) {
        let s3 = &mut self.s3;
        if index > 0x08 && s3.pll_lock != 0x06 {
            return;
        }
        match index {
            0x08 => s3.pll_lock = value,
            0x10 | 0x12 => s3.sr[usize::from(index)] = value & 0x7F, // PLL N and R
            0x11 | 0x13 => s3.sr[usize::from(index)] = value & 0x7F, // PLL M
            0x15 | 0x18 => s3.sr[usize::from(index)] = value,
            _ => {}
        }
    }

    fn s3_read_seq(&self, index: u8) -> u8 {
        let s3 = &self.s3;
        if index > 0x08 && s3.pll_lock != 0x06 {
            return if index < 0x1B { 0 } else { index };
        }
        match index {
            0x08 => s3.pll_lock,
            0x10..=0x13 | 0x15 | 0x18 => s3.sr[usize::from(index)],
            _ => 0,
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

    fn set(vga: &mut Vga, index_port: u16, index: u8, value: u8) {
        vga.write(index_port, index);
        vga.write(index_port + 1, value);
    }

    /// A VGA in mode 12h-like planar state: unchained, write mode 0, all planes enabled.
    fn planar() -> Vga {
        let mut vga = Vga::new();
        set(&mut vga, 0x3C4, 4, 0x06);
        set(&mut vga, 0x3C4, 2, 0x0F);
        set(&mut vga, 0x3CE, 5, 0x00);
        set(&mut vga, 0x3CE, 6, 0x05);
        vga
    }

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
    fn palettes() {
        let dac = default_256_palette();
        assert_eq!(dac[15], [63, 63, 63]);
        assert_eq!(dac[31], [63, 63, 63]);
        assert_eq!(dac[32], [0, 0, 63], "blue starts the first hue ring");
        assert_eq!(dac[40], [63, 0, 0]);
        assert_eq!(dac[104], [0, 0, 28], "medium-intensity ring");
        assert_eq!(dac[247], [11, 12, 16]);
        assert_eq!(dac[248], [0, 0, 0]);
        let ega = ega_palette();
        assert_eq!((ega[6], ega[20], ega[63]), ([42, 42, 0], [42, 21, 0], [63, 63, 63]));
        let cga = cga64_palette();
        assert_eq!((cga[6], cga[0x14], cga[0x17]), ([42, 21, 0], [63, 21, 21], [63, 63, 63]));
    }

    #[test]
    fn chain4_writes_go_to_plane_address_mod_4() {
        let mut vga = Vga::new();
        set(&mut vga, 0x3C4, 4, 0x0E);
        vga.cpu_write(5, 0xAB);
        assert_eq!(vga.vram[(4 << 2) + 1], 0xAB, "offset 4 (address & !3), plane 1");
        assert_eq!(vga.cpu_read(5), 0xAB);
        // Mode X: unchained, plane 1 selected for reading sees the same byte at offset 4.
        set(&mut vga, 0x3C4, 4, 0x06);
        set(&mut vga, 0x3CE, 4, 1);
        assert_eq!(vga.cpu_read(4), 0xAB);
    }

    #[test]
    fn map_mask_selects_planes() {
        let mut vga = planar();
        set(&mut vga, 0x3C4, 2, 0x05);
        vga.cpu_write(10, 0x5A);
        assert_eq!(vga.planes(10), 0x005A_005A);
    }

    #[test]
    fn set_reset_and_bit_mask() {
        let mut vga = planar();
        set(&mut vga, 0x3CE, 0, 0x0C); // set/reset color 12
        set(&mut vga, 0x3CE, 1, 0x0F); // enabled on all planes
        set(&mut vga, 0x3CE, 8, 0xF0); // only the left four pixels
        vga.cpu_read(0); // latch the (zero) background
        vga.cpu_write(0, 0x00);
        assert_eq!(vga.planes(0), 0xF0F0_0000, "planes 2 and 3 set in the masked bits");
    }

    #[test]
    fn write_mode_1_copies_latches() {
        let mut vga = planar();
        vga.set_planes(100, 0x1122_3344);
        set(&mut vga, 0x3CE, 5, 0x01);
        vga.cpu_read(100);
        vga.cpu_write(200, 0x00);
        assert_eq!(vga.planes(200), 0x1122_3344);
    }

    #[test]
    fn write_mode_2_and_xor() {
        let mut vga = planar();
        set(&mut vga, 0x3CE, 5, 0x02);
        set(&mut vga, 0x3CE, 8, 0x81);
        vga.cpu_read(0);
        vga.cpu_write(0, 0x05); // color 5 at pixels 0 and 7
        assert_eq!(vga.planes(0), 0x0081_0081);
        set(&mut vga, 0x3CE, 3, 0x18); // XOR
        set(&mut vga, 0x3CE, 8, 0x01);
        vga.cpu_read(0);
        vga.cpu_write(0, 0x0F); // pixel 7 becomes 5 ^ 15 = 10
        assert_eq!(vga.planes(0), 0x0180_0180);
    }

    #[test]
    fn write_mode_3_uses_rotated_data_as_mask() {
        let mut vga = planar();
        set(&mut vga, 0x3CE, 5, 0x03);
        set(&mut vga, 0x3CE, 0, 0x02); // color 2
        set(&mut vga, 0x3CE, 3, 0x01); // rotate right by 1
        vga.cpu_read(0);
        vga.cpu_write(0, 0x03); // rotated: 0x81
        assert_eq!(vga.planes(0), 0x0000_8100);
    }

    #[test]
    fn read_mode_1_compares_colors() {
        let mut vga = planar();
        // Pixels: bit 7 color 3, bit 6 color 1, the rest 0.
        vga.set_planes(0, 0x0000_80C0);
        set(&mut vga, 0x3CE, 5, 0x08);
        set(&mut vga, 0x3CE, 2, 0x03);
        set(&mut vga, 0x3CE, 7, 0x0F);
        assert_eq!(vga.cpu_read(0), 0x80);
        set(&mut vga, 0x3CE, 7, 0x01); // only plane 0 matters: colors 1 and 3 both match
        assert_eq!(vga.cpu_read(0), 0xC0);
    }

    #[test]
    fn crtc_write_protect_and_mono_ports() {
        let mut vga = Vga::new();
        set(&mut vga, 0x3D4, 0x11, 0x80);
        set(&mut vga, 0x3D4, 0x01, 0x55);
        assert_eq!(vga.crtc[1], 0);
        set(&mut vga, 0x3D4, 0x07, 0xFF);
        assert_eq!(vga.crtc[7], 0x10, "only line compare bit 8 gets through");
        // Color CRTC address: 3B4h is not decoded.
        set(&mut vga, 0x3B4, 0x0C, 0x12);
        assert_eq!(vga.crtc[0x0C], 0);
        vga.write(0x3C2, 0x66);
        set(&mut vga, 0x3B4, 0x0C, 0x12);
        assert_eq!(vga.crtc[0x0C], 0x12);
    }
}
