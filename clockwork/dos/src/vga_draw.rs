//! Turning VGA registers and memory into a picture.
//!
//! Ported from DOSBox Staging `src/hardware/video/vga_draw.cpp` and the mode decisions in
//! `vga.cpp` (GPL-2.0-or-later). The display is rebuilt from the CRTC registers on every call:
//! start address, offset (row pitch), byte panning, pel panning, line compare (split screen),
//! character height and scan doubling, so page flipping, smooth scrolling and Mode X resolutions
//! show as they would on the card. Not modelled: per-scanline register changes within a frame,
//! cursor blinking and character blinking (blinking text is always shown).
//!
//! SVGA modes follow DOSBox's `setup_drawing` for the S3: the extended overflow, offset and
//! start address bits widen the CRTC values, 256-color modes show 8 pixels per character clock
//! in enhanced mode (CR3A bit 4) and 4 otherwise, high-color modes 4 and true-color modes 8.

use crate::vga::{DisplayKind, FONT_BLOCKS, Vga, chained_index};
use machine::hash::Fnv64;
use std::hash::Hasher;

/// A rendered screen, row by row: one DAC index per pixel in `pixels`, or for the direct-color
/// SVGA modes three bytes (red, green, blue) per pixel in `rgb`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    pub rgb: Vec<u8>,
}

impl Picture {
    fn indexed(width: usize, height: usize, pixels: Vec<u8>) -> Self {
        Self { width, height, pixels, rgb: Vec::new() }
    }
}

/// Bytes of B0000-BFFFF the renderer reads text and CGA screens from.
pub const LOW_RAM_SIZE: usize = 0x1_0000;

impl Vga {
    fn vertical_display_end(&self) -> u32 {
        let crtc7 = u32::from(self.crtc[7]);
        let bit10 = u32::from(self.s3.ver_overflow & 0x02) << 9;
        (u32::from(self.crtc[0x12]) | ((crtc7 & 0x02) << 7) | ((crtc7 & 0x40) << 3) | bit10) + 1
    }

    fn line_compare(&self) -> u32 {
        u32::from(self.crtc[0x18])
            | (u32::from(self.crtc[7] & 0x10) << 4)
            | (u32::from(self.crtc[9] & 0x40) << 3)
            | (u32::from(self.s3.ver_overflow & 0x40) << 4)
    }

    /// CRTC start address (registers 0Ch-0Dh and the S3's bits 16-20), in CRTC address units.
    pub fn start_address(&self) -> u32 {
        (u32::from(self.s3.start_high) << 16) | (u32::from(self.crtc[0x0C]) << 8) | u32::from(self.crtc[0x0D])
    }

    /// The offset register with the S3's bits 8-9 (DOSBox `scan_len`).
    pub fn scan_length(&self) -> u32 {
        u32::from(self.crtc[0x13]) | (u32::from(self.s3.scan_high) << 8)
    }

    /// Address units from one character row to the next (offset register × 2).
    fn row_offset(&self) -> u32 {
        self.scan_length() * 2
    }

    /// Character clocks of display per line (horizontal display end + 1, with the S3's bit 8).
    pub(crate) fn char_clocks(&self) -> usize {
        (usize::from(self.crtc[1]) | (usize::from(self.s3.hor_overflow & 0x02) << 7)) + 1
    }

    fn scan_doubling(&self) -> u32 {
        if self.crtc[9] & 0x80 != 0 { 2 } else { 1 }
    }

    /// Scan lines per character row (maximum scan line + 1).
    fn char_height(&self) -> u32 {
        u32::from(self.crtc[9] & 0x1F) + 1
    }

    fn bytes_skip(&self) -> u32 {
        u32::from(self.crtc[8] >> 5) & 3
    }

    /// Bytes of CPU memory per CRTC address unit in the CGA modes: 2 in word mode, 1 in byte mode.
    fn cga_unit(&self) -> u32 {
        if self.crtc[0x17] & 0x40 == 0 { 2 } else { 1 }
    }

    /// A 4-bit color through the attribute controller: color plane enable, palette registers,
    /// and the color select bits.
    pub fn attribute_color(&self, color: u8) -> u8 {
        let mut value = self.attr[usize::from(color & self.attr[0x12] & 0x0F)] & 0x3F;
        let select = self.attr[0x14];
        if self.attr[0x10] & 0x80 != 0 {
            value = (value & 0x0F) | ((select & 0x03) << 4);
        }
        value | ((select & 0x0C) << 4)
    }

    /// The memory row a display row comes from, and whether it is past the split line (line
    /// compare restarts the display at address 0).
    fn row_address(&self, row: u32, scanlines_per_row: u32) -> (u32, bool) {
        let split = (self.line_compare() + 1) / scanlines_per_row.max(1);
        if row >= split {
            ((row - split) * self.row_offset(), true)
        } else {
            (self.start_address() + row * self.row_offset(), false)
        }
    }

    fn pel_panning(&self, after_split: bool) -> u32 {
        if after_split && self.attr[0x10] & 0x20 != 0 {
            return 0;
        }
        u32::from(self.attr[0x13] & 0x07)
    }

    /// Renders the screen. `low_ram` is memory B0000-BFFFF, where text and CGA screens are.
    pub fn picture(&self, low_ram: &[u8]) -> Picture {
        match self.display_kind() {
            DisplayKind::Text => self.text_picture(low_ram),
            DisplayKind::Vga256 => self.vga256_picture(),
            DisplayKind::Ega | DisplayKind::Svga16 => self.ega_picture(),
            DisplayKind::Cga4 => self.cga_picture(low_ram, 2),
            DisplayKind::Cga2 => self.cga_picture(low_ram, 1),
            DisplayKind::Svga256 => self.svga256_picture(),
            kind => self.direct_color_picture(kind),
        }
    }

    /// The screen as RGB bytes.
    pub fn rgb(&self, low_ram: &[u8]) -> (usize, usize, Vec<u8>) {
        let picture = self.picture(low_ram);
        if !picture.rgb.is_empty() {
            return (picture.width, picture.height, picture.rgb);
        }
        let expand = |c: u8| (c << 2) | (c >> 4);
        let palette: Vec<[u8; 3]> = (0..256).map(|i| self.dac[i & usize::from(self.pel_mask)].map(expand)).collect();
        let rgb = picture.pixels.iter().flat_map(|&p| palette[usize::from(p)]).collect();
        (picture.width, picture.height, rgb)
    }

    fn graphics_rows(&self) -> u32 {
        (self.vertical_display_end() / (self.scan_doubling() * self.char_height())).max(1)
    }

    fn vga256_picture(&self) -> Picture {
        let width = self.char_clocks() * 4;
        let height = self.graphics_rows();
        // Chain-4 with doubleword addressing displays the CPU's linear view (DOSBox's
        // compatible_chain4 fast path); otherwise the CRTC walks interleaved memory, four pixels
        // (one per plane) per address.
        let chained = self.seq[4] & 0x08 != 0 && self.crtc[0x14] & 0x40 != 0;
        let per_row = self.scan_doubling() * self.char_height();
        let wrap = self.wrap();
        let mut pixels = Vec::with_capacity(width * height as usize);
        for row in 0..height {
            let (base, split) = self.row_address(row, per_row);
            let address = (base + self.bytes_skip()) * 4 + self.pel_panning(split) / 2;
            let start = address as usize;
            if chained && start & 3 == 0 && start + width <= 0x1_0000 {
                // Aligned and without wrapping: four pixels per plane offset.
                for group in (start..start + width).step_by(4) {
                    let at = group << 2;
                    pixels.extend_from_slice(&self.vram[at..at + 4]);
                }
            } else if !chained && start + width <= wrap {
                pixels.extend_from_slice(&self.vram[start..start + width]);
            } else {
                for x in 0..width as u32 {
                    let at = address + x;
                    pixels.push(if chained {
                        self.vram[chained_index(at & 0xFFFF)]
                    } else {
                        self.vram[at as usize & (wrap - 1)]
                    });
                }
            }
        }
        Picture::indexed(width, height as usize, pixels)
    }

    /// Byte address in video memory of each displayed row of an SVGA mode: the start address
    /// and the offset register count 4 bytes, the pel panning register bytes (pixel pairs in
    /// 256-color modes).
    fn svga_rows(&self, height: u32, pel_shift: u32) -> impl Iterator<Item = usize> + '_ {
        let per_row = self.scan_doubling() * self.char_height();
        (0..height).map(move |row| {
            let (base, split) = self.row_address(row, per_row);
            ((base + self.bytes_skip()) * 4 + (self.pel_panning(split) >> pel_shift)) as usize
        })
    }

    fn svga256_picture(&self) -> Picture {
        let clocks = self.char_clocks();
        let pixel_doubling = self.crtc[0x17] & 0x08 != 0;
        let width = if pixel_doubling || self.s3.reg_3a & 0x10 == 0 { clocks * 4 } else { clocks * 8 };
        let height = self.graphics_rows();
        let wrap = self.wrap();
        let mut pixels = Vec::with_capacity(width * height as usize);
        for start in self.svga_rows(height, 1) {
            if start + width <= wrap {
                pixels.extend_from_slice(&self.vram[start..start + width]);
            } else {
                pixels.extend((0..width).map(|x| self.vram[(start + x) & (wrap - 1)]));
            }
        }
        Picture::indexed(width, height as usize, pixels)
    }

    /// 15-, 16-, 24- and 32-bit pixels, converted to 8-bit red, green and blue.
    fn direct_color_picture(&self, kind: DisplayKind) -> Picture {
        let clocks = self.char_clocks();
        let (width, bytes) = match kind {
            DisplayKind::HiColor15 | DisplayKind::HiColor16 => (clocks * 4, 2),
            DisplayKind::TrueColor24 => (clocks * 8, 3),
            _ => (clocks * 8, 4),
        };
        let height = self.graphics_rows();
        let mask = self.wrap() - 1;
        let five = |v: u16| ((v << 3) | (v >> 2)) as u8;
        let mut rgb = Vec::with_capacity(width * height as usize * 3);
        for start in self.svga_rows(height, 0) {
            for x in 0..width {
                let at = start + x * bytes;
                let byte = |i: usize| self.vram[(at + i) & mask];
                let word = u16::from_le_bytes([byte(0), byte(1)]);
                rgb.extend_from_slice(&match kind {
                    DisplayKind::HiColor15 => [five((word >> 10) & 0x1F), five((word >> 5) & 0x1F), five(word & 0x1F)],
                    DisplayKind::HiColor16 => {
                        let green = (word >> 5) & 0x3F;
                        [five(word >> 11), ((green << 2) | (green >> 4)) as u8, five(word & 0x1F)]
                    }
                    _ => [byte(2), byte(1), byte(0)],
                });
            }
        }
        Picture { width, height: height as usize, pixels: Vec::new(), rgb }
    }

    fn ega_picture(&self) -> Picture {
        let width = self.char_clocks() * 8;
        let height = self.graphics_rows();
        let colors: [u8; 16] = std::array::from_fn(|c| self.attribute_color(c as u8));
        let per_row = self.scan_doubling() * self.char_height();
        let mut pixels = Vec::with_capacity(width * height as usize);
        for row in 0..height {
            let (base, split) = self.row_address(row, per_row);
            let first = (base + self.bytes_skip()) * 8 + self.pel_panning(split);
            for x in 0..width as u32 {
                let at = first + x;
                let planes = self.planes(at >> 3);
                let bit = 7 - (at & 7);
                let color = ((planes >> bit) & 1)
                    | ((planes >> (bit + 7)) & 2)
                    | ((planes >> (bit + 14)) & 4)
                    | ((planes >> (bit + 21)) & 8);
                pixels.push(colors[color as usize]);
            }
        }
        Picture::indexed(width, height as usize, pixels)
    }

    /// CGA graphics from RAM at B8000: `bits` per pixel (2 or 1), scan lines of a character row
    /// interleaved in 8 KB banks.
    fn cga_picture(&self, low_ram: &[u8], bits: u32) -> Picture {
        let width = self.char_clocks() * 8;
        let lines = (self.vertical_display_end() / self.scan_doubling()).max(1);
        let banks = self.char_height();
        let colors: [u8; 4] = std::array::from_fn(|c| self.attribute_color(c as u8));
        let (unit, pitch) = (self.cga_unit(), self.row_offset() * self.cga_unit());
        let per_byte = 8 / bits;
        let mut pixels = Vec::with_capacity(width * lines as usize);
        for y in 0..lines {
            let address = self.start_address() * unit + (y / banks) * pitch + (y % banks) * 0x2000;
            for x in 0..width as u32 {
                let byte = low_ram[0x8000 + ((address + x / per_byte) as usize & 0x7FFF)];
                let shift = 8 - bits * (x % per_byte + 1);
                pixels.push(colors[usize::from((byte >> shift) & ((1 << bits) - 1) as u8)]);
            }
        }
        Picture::indexed(width, lines as usize, pixels)
    }

    /// Offset of the text screen in `low_ram`: B0000 for memory map 2, B8000 otherwise.
    fn text_window(&self) -> usize {
        if self.gc[6] & 0x0C == 0x08 { 0 } else { 0x8000 }
    }

    fn text_geometry(&self) -> (usize, usize, u32, usize) {
        let columns = self.char_clocks();
        let height = self.char_height();
        let rows = (self.vertical_display_end() / self.scan_doubling() / height).max(1) as usize;
        let dot = if self.seq[1] & 0x01 != 0 { 8 } else { 9 };
        (columns, rows, height, dot)
    }

    /// The (character, attribute) cells displayed in a text mode, row by row.
    pub(crate) fn text_cells<'a>(&self, low_ram: &'a [u8]) -> impl Iterator<Item = (u8, u8)> + 'a {
        let (columns, rows, _, _) = self.text_geometry();
        let (window, start, pitch) = (self.text_window(), self.start_address(), self.row_offset());
        (0..rows * columns).map(move |i| {
            let cell = start + (i / columns) as u32 * pitch + (i % columns) as u32;
            let at = window + ((cell as usize * 2) & 0x7FFF);
            (low_ram[at], low_ram[at + 1])
        })
    }

    /// Font blocks for attribute bit 3 clear (map B) and set (map A).
    fn font_maps(&self) -> [u32; 2] {
        let s = self.seq[3];
        let a = (((s >> 5) & 1) << 2) | ((s >> 2) & 3);
        let b = (((s >> 4) & 1) << 2) | (s & 3);
        [FONT_BLOCKS[usize::from(b)], FONT_BLOCKS[usize::from(a)]]
    }

    fn text_picture(&self, low_ram: &[u8]) -> Picture {
        let (columns, rows, char_height, dot) = self.text_geometry();
        let (width, height) = (columns * dot, rows * char_height as usize);
        let colors: [u8; 16] = std::array::from_fn(|c| self.attribute_color(c as u8));
        let blink = self.attr[0x10] & 0x08 != 0;
        let line_graphics = self.attr[0x10] & 0x04 != 0;
        let maps = self.font_maps();
        let cursor_on = self.crtc[0x0A] & 0x20 == 0;
        let cursor = (u32::from(self.crtc[0x0E]) << 8) | u32::from(self.crtc[0x0F]);
        let cursor_lines = u32::from(self.crtc[0x0A] & 0x1F)..=u32::from(self.crtc[0x0B] & 0x1F);
        let (start, pitch) = (self.start_address(), self.row_offset());
        let mut pixels = vec![0u8; width * height];
        for (i, (ch, attribute)) in self.text_cells(low_ram).enumerate() {
            let (row, column) = (i / columns, i % columns);
            let foreground = colors[usize::from(attribute & 0x0F)];
            let background = colors[usize::from(if blink { (attribute >> 4) & 7 } else { attribute >> 4 })];
            let glyph = maps[usize::from(attribute >> 3) & 1] + u32::from(ch) * 32;
            let has_cursor = cursor_on && cursor == start + row as u32 * pitch + column as u32;
            for line in 0..char_height {
                let bits = self.font_byte(glyph + line);
                let ninth = dot == 9 && line_graphics && (0xC0..=0xDF).contains(&ch) && bits & 1 != 0;
                let solid = has_cursor && cursor_lines.contains(&line);
                let at = (row * char_height as usize + line as usize) * width + column * dot;
                for x in 0..dot {
                    let on = solid || if x < 8 { bits & (0x80 >> x) != 0 } else { ninth };
                    pixels[at + x] = if on { foreground } else { background };
                }
            }
        }
        Picture::indexed(width, height, pixels)
    }

    /// A fingerprint of what is displayed, for frame comparison. Graphics in video memory are
    /// re-rendered only after something changed; text and CGA screens (in RAM) are hashed from
    /// their cells or pixels each time.
    pub(crate) fn fingerprint(&mut self, low_ram: &[u8]) -> u64 {
        let kind = self.display_kind();
        if self.changed {
            self.changed = false;
            let mut h = Fnv64::default();
            h.write(self.dac.as_flattened());
            h.write(&[self.pel_mask]);
            h.write(&self.attr);
            h.write(&self.seq);
            h.write(&self.crtc);
            h.write(&self.gc);
            match kind {
                DisplayKind::Text => {
                    for base in self.font_maps() {
                        let font: Vec<u8> = (0..256 * 32).map(|i| self.font_byte(base + i)).collect();
                        h.write(&font);
                    }
                }
                DisplayKind::Cga4 | DisplayKind::Cga2 => {}
                _ => {
                    let picture = self.picture(low_ram);
                    h.write(&picture.pixels);
                    h.write(&picture.rgb);
                }
            }
            self.display_hash = h.finish();
        }
        let mut h = Fnv64::default();
        h.write(&self.display_hash.to_le_bytes());
        match kind {
            DisplayKind::Text => {
                let cells: Vec<u8> = self.text_cells(low_ram).flat_map(|(c, a)| [c, a]).collect();
                h.write(&cells);
            }
            DisplayKind::Cga4 | DisplayKind::Cga2 => h.write(&self.picture(low_ram).pixels),
            _ => {}
        }
        h.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vga_params::VGA_PARAMETERS;

    fn vga_in(entry: usize) -> Vga {
        let mut vga = Vga::new();
        vga.load_parameters(&VGA_PARAMETERS[entry]);
        vga
    }

    #[test]
    fn mode_geometry_from_registers() {
        let size = |entry: usize, kind: DisplayKind| {
            let vga = vga_in(entry);
            assert_eq!(vga.display_kind(), kind, "entry {entry:02X}h");
            let p = vga.picture(&[0; LOW_RAM_SIZE]);
            (p.width, p.height)
        };
        assert_eq!(size(0x18, DisplayKind::Text), (720, 400), "mode 3: 80×25, 9×16 cells");
        assert_eq!(size(0x17, DisplayKind::Text), (360, 400), "mode 1: 40×25");
        assert_eq!(size(0x04, DisplayKind::Cga4), (320, 200));
        assert_eq!(size(0x06, DisplayKind::Cga2), (640, 200));
        assert_eq!(size(0x0D, DisplayKind::Ega), (320, 200));
        assert_eq!(size(0x0E, DisplayKind::Ega), (640, 200));
        assert_eq!(size(0x12, DisplayKind::Ega), (640, 350), "mode 10h");
        assert_eq!(size(0x1B, DisplayKind::Ega), (640, 480), "mode 12h");
        assert_eq!(size(0x1C, DisplayKind::Vga256), (320, 200), "mode 13h");
    }

    #[test]
    fn mode_x_320x240_with_page_flip() {
        let mut vga = vga_in(0x1C);
        // The classic Mode X setup: chain-4 off, byte mode, 480 lines.
        vga.seq[4] = 0x06;
        vga.crtc[0x14] = 0x00;
        vga.crtc[0x17] = 0xE3;
        vga.crtc[0x06] = 0x0D;
        vga.crtc[0x07] = 0x3E;
        vga.crtc[0x12] = 0xDF;
        vga.seq[2] = 0x02; // plane 1
        vga.cpu_write(80 * 239 + 2, 7); // x = 2*4 + 1 = 9, last row of page 0
        vga.cpu_write(19200 + 1, 9); // page 2 starts at 19200: x = 5, y = 0
        let p = vga.picture(&[0; LOW_RAM_SIZE]);
        assert_eq!((p.width, p.height), (320, 240));
        assert_eq!(p.pixels[239 * 320 + 9], 7);
        vga.crtc[0x0C] = (19200 >> 8) as u8;
        vga.crtc[0x0D] = 19200u32 as u8;
        assert_eq!(vga.picture(&[0; LOW_RAM_SIZE]).pixels[5], 9);
    }

    #[test]
    fn planar_pixels_through_the_attribute_palette() {
        let mut vga = vga_in(0x1B); // mode 12h
        vga.seq[2] = 0x0F;
        vga.gc[5] = 0x02; // write mode 2
        vga.gc[8] = 0x40; // pixel 1
        vga.cpu_read(80);
        vga.cpu_write(80, 0x06); // row 1, x = 1: color 6
        let p = vga.picture(&[0; LOW_RAM_SIZE]);
        assert_eq!(p.pixels[640 + 1], 0x14, "color 6 maps to DAC 14h (brown)");
        assert_eq!(p.pixels[640], 0);
        // Line compare: rows from 100 on show memory from address 0.
        vga.crtc[0x18] = 99;
        vga.crtc[7] &= !0x10;
        vga.crtc[9] &= !0x40;
        assert_eq!(vga.picture(&[0; LOW_RAM_SIZE]).pixels[101 * 640 + 1], 0x14);
        // Pel panning by one pixel moves the pixel left.
        vga.crtc[0x18] = 0xFF;
        vga.crtc[7] |= 0x10;
        vga.crtc[9] |= 0x40;
        vga.attr[0x13] = 1;
        assert_eq!(vga.picture(&[0; LOW_RAM_SIZE]).pixels[640], 0x14);
    }

    #[test]
    fn text_uses_the_plane_2_font() {
        let mut vga = vga_in(0x18);
        let mut glyph = [0u8; 16];
        glyph[3] = 0x81;
        vga.load_font(0, b'A' as usize, 16, &glyph);
        let mut ram = vec![0u8; LOW_RAM_SIZE];
        ram[0x8000] = b'A';
        ram[0x8001] = 0x1E; // yellow on blue
        vga.crtc[0x0A] = 0x20; // cursor off
        let p = vga.picture(&ram);
        assert_eq!(p.pixels[3 * 720], 0x3E, "yellow: attribute register 14 = 3Eh");
        assert_eq!(p.pixels[3 * 720 + 7], 0x3E);
        assert_eq!(p.pixels[3 * 720 + 1], 0x01, "blue background");
        assert_eq!(p.pixels[3 * 720 + 8], 0x01, "9th column of a letter is background");
    }

    #[test]
    fn cga4_interleaved_banks() {
        let vga = vga_in(0x04);
        let mut ram = vec![0u8; LOW_RAM_SIZE];
        ram[0x8000] = 0b1100_0000; // (0, 0) color 3
        ram[0x8000 + 0x2000 + 1] = 0b0000_0001; // (7, 1) color 1
        let p = vga.picture(&ram);
        assert_eq!(p.pixels[0], vga.attribute_color(3));
        assert_eq!(p.pixels[320 + 7], vga.attribute_color(1));
        assert_eq!(vga.attribute_color(3), 0x17);
    }
}
