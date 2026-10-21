//! Frames as pictures (D25): what the screen shows, as indexed pixels and the palette (RGB in
//! direct-color modes), so a port's frames can be compared with the original's. A frame's picture is
//! a small file, deflated (raw deflate):
//!
//! - `FRM1`, then the width and the height (16-bit, little-endian), then the kind: 0 indexed, 1 RGB;
//! - indexed: the palette, 256 × R, G, B as the DAC holds them (0-63), through the pel mask (entry
//!   `i` is the DAC's `i & mask`), then a byte a pixel;
//! - RGB: 3 bytes a pixel.
//!
//! Pixels go row by row from the top left. Text modes are drawn with their font, as the screen shows
//! them. Records, not state: the machine renders them only when asked
//! ([`crate::Machine::record_frame_images`]), and only when the picture changed.

use std::io::Write;

use crate::vga::Vga;

/// The pictures of the frames shown since they were last taken.
#[derive(Debug, Clone, Default)]
pub struct FrameImages {
    /// The last frame's fingerprint: a frame showing the same picture isn't rendered again.
    last: Option<u64>,
    /// (frame number, picture).
    pub pending: Vec<(u64, Vec<u8>)>,
}

impl FrameImages {
    /// Frame `number` with fingerprint `hash`: its picture, if it differs from the frame before.
    pub(crate) fn frame(&mut self, number: u64, hash: u64, vga: &Vga, low_ram: &[u8]) {
        if self.last != Some(hash) {
            self.last = Some(hash);
            self.pending.push((number, encode(vga, low_ram)));
        }
    }
}

/// The screen's picture, encoded as the module says.
pub fn encode(vga: &Vga, low_ram: &[u8]) -> Vec<u8> {
    let picture = vga.picture(low_ram);
    let mut raw = Vec::with_capacity(8 + 768 + picture.pixels.len().max(picture.rgb.len()));
    raw.extend_from_slice(b"FRM1");
    raw.extend_from_slice(&(picture.width as u16).to_le_bytes());
    raw.extend_from_slice(&(picture.height as u16).to_le_bytes());
    if picture.rgb.is_empty() {
        raw.push(0);
        for i in 0..256 {
            raw.extend_from_slice(&vga.dac[i & usize::from(vga.pel_mask)]);
        }
        raw.extend_from_slice(&picture.pixels);
    } else {
        raw.push(1);
        raw.extend_from_slice(&picture.rgb);
    }
    let mut deflate = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    deflate.write_all(&raw).expect("writing to memory");
    deflate.finish().expect("writing to memory")
}
