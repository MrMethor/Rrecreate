//! Autodesk's FLI and FLC animations (the format's published chunks, as DOS games played them: Arena's cinematics,
//! Wolf3D's sequels' intros): a header, then frames, each a set of chunks that change the picture or the palette
//! from the frame before; after the last, a ring frame that leads back to the first's picture, so an animation
//! loops from its second frame on. A port draws a cinematic with this, its own drawing over it.

/// An animation: its picture's size, its frames' count, its speed, and where each frame (and the ring frame) is.
#[derive(Debug, Clone)]
pub struct Flc {
    data: Vec<u8>,
    pub width: usize,
    pub height: usize,
    /// The frames, not counting the ring frame.
    pub frames: usize,
    /// Each frame's time: milliseconds (FLC), or 70ths of a second (FLI).
    pub speed: u32,
    /// FLI (AF11h) rather than FLC (AF12h).
    pub fli: bool,
    offsets: Vec<usize>,
}

fn u16_at(d: &[u8], at: usize) -> Option<usize> {
    Some(usize::from(u16::from_le_bytes([*d.get(at)?, *d.get(at + 1)?])))
}

fn u32_at(d: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes([*d.get(at)?, *d.get(at + 1)?, *d.get(at + 2)?, *d.get(at + 3)?]) as usize)
}

/// Chunks' types.
pub const COLOR_256: usize = 4;
pub const SS2: usize = 7;
pub const COLOR_64: usize = 11;
pub const LC: usize = 12;
pub const BLACK: usize = 13;
pub const BRUN: usize = 15;
pub const COPY: usize = 16;
pub const PSTAMP: usize = 18;
/// A frame's chunk, and a prefix chunk (FLC's, which a player skips).
const FRAME: usize = 0xF1FA;
const PREFIX: usize = 0xF100;

impl Flc {
    /// An FLI or FLC file's animation: its frames found (the ring frame after the last).
    pub fn parse(data: Vec<u8>) -> Result<Flc, String> {
        let wrong = |what: &str| format!("an FLC: {what}");
        let magic = u16_at(&data, 4).ok_or_else(|| wrong("shorter than its header"))?;
        let fli = match magic {
            0xAF11 => true,
            0xAF12 => false,
            _ => return Err(wrong("not an FLI (AF11h) or an FLC (AF12h)")),
        };
        let frames = u16_at(&data, 6).ok_or_else(|| wrong("its header"))?;
        let (width, height) = (u16_at(&data, 8).unwrap_or(320), u16_at(&data, 10).unwrap_or(200));
        let speed = if fli { u16_at(&data, 16).unwrap_or(0) as u32 } else { u32_at(&data, 16).unwrap_or(0) as u32 };
        // FLC: the first frame where its header says (oframe1), else after the 128 bytes of the header.
        let mut at = if fli { 128 } else { u32_at(&data, 80).filter(|&o| o != 0).unwrap_or(128) };
        let mut offsets = Vec::new();
        while offsets.len() <= frames {
            let size = u32_at(&data, at).ok_or_else(|| wrong("a frame past its end"))?;
            let kind = u16_at(&data, at + 4).ok_or_else(|| wrong("a frame past its end"))?;
            if size < 6 {
                return Err(wrong("a frame of less than its header"));
            }
            if kind == FRAME {
                offsets.push(at);
            } else if kind != PREFIX {
                return Err(wrong(&format!("a chunk {kind:04X}h where a frame is")));
            }
            at += size;
        }
        Ok(Flc { data, width, height, frames, speed, fli, offsets })
    }

    /// Frame `n`'s chunks, each its type and its body: for a game's own player, which decodes some its own way.
    pub fn chunks(&self, n: usize) -> Result<Vec<(usize, &[u8])>, String> {
        let at = *self.offsets.get(n).ok_or_else(|| format!("an FLC has no frame {n}"))?;
        let d = &self.data;
        let wrong = || format!("an FLC's frame {n}: a chunk past its end");
        let count = u16_at(d, at + 6).ok_or_else(wrong)?;
        let mut c = at + 16;
        let mut chunks = Vec::with_capacity(count);
        for _ in 0..count {
            let size = u32_at(d, c).ok_or_else(wrong)?;
            let kind = u16_at(d, c + 4).ok_or_else(wrong)?;
            chunks.push((kind, d.get(c + 6..c + size.max(6)).ok_or_else(wrong)?));
            c += size;
        }
        Ok(chunks)
    }

    /// Frame `n`'s changes made to `pixels` (`width` × `height`, row by row) and `palette` (256 × R, G, B, the
    /// DAC's 0 to 63), as the format says: `n` from 0, the ring frame `frames`. Whether the palette changed.
    pub fn apply(&self, n: usize, pixels: &mut [u8], palette: &mut [u8]) -> Result<bool, String> {
        let mut palette_changed = false;
        for (kind, body) in self.chunks(n)? {
            match kind {
                COLOR_256 | COLOR_64 => {
                    colours(body, palette, kind == COLOR_256 && !self.fli);
                    palette_changed = true;
                }
                BRUN => self.byte_run(body, pixels),
                SS2 => self.word_delta(body, pixels),
                LC => self.line_delta(body, pixels),
                BLACK => pixels.fill(0),
                COPY => {
                    let n = pixels.len().min(body.len());
                    pixels[..n].copy_from_slice(&body[..n]);
                }
                PSTAMP => {}
                _ => return Err(format!("an FLC's frame {n}: a chunk of type {kind} not known")),
            }
        }
        Ok(palette_changed)
    }

    /// BYTE_RUN: each line its packets (the count byte unused), a positive count a byte repeated, a negative one
    /// bytes copied, to the line's width.
    pub fn byte_run(&self, body: &[u8], pixels: &mut [u8]) {
        let mut at = 0;
        for y in 0..self.height {
            at += 1;
            let mut x = 0;
            while x < self.width {
                let Some(&count) = body.get(at) else { return };
                at += 1;
                let count = count as i8;
                if count >= 0 {
                    let value = body.get(at).copied().unwrap_or(0);
                    at += 1;
                    for _ in 0..count {
                        put(pixels, self.width, x, y, value);
                        x += 1;
                    }
                } else {
                    for _ in 0..(-i16::from(count)) {
                        put(pixels, self.width, x, y, body.get(at).copied().unwrap_or(0));
                        at += 1;
                        x += 1;
                    }
                }
            }
        }
    }

    /// DELTA_FLC (SS2): its lines' count, then for each line its words of options (a count of lines to skip, the
    /// line's last byte, or the packets' count), then the packets: columns to skip, then a positive count of words
    /// copied, or a negative one of a word repeated.
    fn word_delta(&self, body: &[u8], pixels: &mut [u8]) {
        let Some(lines) = u16_at(body, 0) else { return };
        let mut at = 2;
        let mut y = 0usize;
        for _ in 0..lines {
            let packets = loop {
                let Some(word) = u16_at(body, at) else { return };
                at += 2;
                match word >> 14 {
                    0b11 => y += 0x10000 - word,
                    0b10 => put(pixels, self.width, self.width - 1, y, word as u8),
                    _ => break word,
                }
            };
            let mut x = 0;
            for _ in 0..packets {
                x += usize::from(body.get(at).copied().unwrap_or(0));
                let count = body.get(at + 1).copied().unwrap_or(0) as i8;
                at += 2;
                if count >= 0 {
                    for _ in 0..count {
                        put(pixels, self.width, x, y, body.get(at).copied().unwrap_or(0));
                        put(pixels, self.width, x + 1, y, body.get(at + 1).copied().unwrap_or(0));
                        at += 2;
                        x += 2;
                    }
                } else {
                    let (a, b) = (body.get(at).copied().unwrap_or(0), body.get(at + 1).copied().unwrap_or(0));
                    at += 2;
                    for _ in 0..(-i16::from(count)) {
                        put(pixels, self.width, x, y, a);
                        put(pixels, self.width, x + 1, y, b);
                        x += 2;
                    }
                }
            }
            y += 1;
        }
    }

    /// DELTA_FLI (LC): the lines to skip and the lines changed, then for each its packets: columns to skip, then a
    /// positive count of bytes copied, or a negative one of a byte repeated.
    pub fn line_delta(&self, body: &[u8], pixels: &mut [u8]) {
        let (Some(skip), Some(lines)) = (u16_at(body, 0), u16_at(body, 2)) else { return };
        let mut at = 4;
        for y in skip..skip + lines {
            let packets = body.get(at).copied().unwrap_or(0);
            at += 1;
            let mut x = 0;
            for _ in 0..packets {
                x += usize::from(body.get(at).copied().unwrap_or(0));
                let count = body.get(at + 1).copied().unwrap_or(0) as i8;
                at += 2;
                if count >= 0 {
                    for _ in 0..count {
                        put(pixels, self.width, x, y, body.get(at).copied().unwrap_or(0));
                        at += 1;
                        x += 1;
                    }
                } else {
                    let value = body.get(at).copied().unwrap_or(0);
                    at += 1;
                    for _ in 0..(-i16::from(count)) {
                        put(pixels, self.width, x, y, value);
                        x += 1;
                    }
                }
            }
        }
    }
}

/// A pixel, nothing past the picture.
fn put(pixels: &mut [u8], width: usize, x: usize, y: usize, value: u8) {
    if x < width {
        if let Some(p) = pixels.get_mut(y * width + x) {
            *p = value;
        }
    }
}

/// COLOR_256 or COLOR_64: packets of colours to skip and colours to set (a count of 0: 256), each R, G, B; an
/// FLC's COLOR_256 of 0 to 255 (`eight`: the DAC takes the top six bits), the others' of 0 to 63.
pub fn colours(body: &[u8], palette: &mut [u8], eight: bool) {
    let Some(packets) = u16_at(body, 0) else { return };
    let mut at = 2;
    let mut colour = 0usize;
    for _ in 0..packets {
        colour += usize::from(body.get(at).copied().unwrap_or(0));
        let count = match body.get(at + 1).copied().unwrap_or(0) {
            0 => 256,
            n => usize::from(n),
        };
        at += 2;
        for _ in 0..count {
            for k in 0..3 {
                let v = body.get(at).copied().unwrap_or(0);
                if let Some(p) = palette.get_mut(colour * 3 + k) {
                    *p = if eight { v >> 2 } else { v & 0x3F };
                }
                at += 1;
            }
            colour += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4 × 2 FLC: a first frame of a palette and a byte run, a second of a word delta, the ring frame empty.
    fn sample() -> Vec<u8> {
        let chunk = |kind: u16, body: &[u8]| -> Vec<u8> {
            let mut c = ((body.len() + 6) as u32).to_le_bytes().to_vec();
            c.extend(kind.to_le_bytes());
            c.extend(body);
            c
        };
        let frame = |chunks: Vec<Vec<u8>>| -> Vec<u8> {
            let body: Vec<u8> = chunks.concat();
            let mut f = ((body.len() + 16) as u32).to_le_bytes().to_vec();
            f.extend(0xF1FAu16.to_le_bytes());
            f.extend((chunks.len() as u16).to_le_bytes());
            f.extend([0; 8]);
            f.extend(body);
            f
        };
        // Colour 1 as 252, 128, 4 (the DAC's 63, 32, 1).
        let palette = chunk(4, &[1, 0, 1, 1, 252, 128, 4]);
        // Line 0: 4 × 1; line 1: copy 7, 8, then 2 × 9 (a line's first byte, its packets' count, unused).
        let brun = chunk(15, &[2, 4, 1, 3, 0xFE, 7, 8, 2, 9]);
        // One line changed: line 1 (one skipped), a packet at column 2 of one word 5, 6.
        let ss2 = chunk(7, &[1, 0, 0xFF, 0xFF, 1, 0, 2, 1, 5, 6]);
        let mut d = vec![0u8; 128];
        d[4..6].copy_from_slice(&0xAF12u16.to_le_bytes());
        d[6..8].copy_from_slice(&2u16.to_le_bytes());
        d[8..10].copy_from_slice(&4u16.to_le_bytes());
        d[10..12].copy_from_slice(&2u16.to_le_bytes());
        d[16..20].copy_from_slice(&70u32.to_le_bytes());
        d.extend(frame(vec![palette, brun]));
        d.extend(frame(vec![ss2]));
        d.extend(frame(vec![]));
        d
    }

    #[test]
    fn frames_change_the_picture_and_the_palette_in_turn() {
        let flc = Flc::parse(sample()).unwrap();
        assert_eq!((flc.width, flc.height, flc.frames, flc.speed), (4, 2, 2, 70));
        let (mut pixels, mut palette) = (vec![0u8; 8], vec![0u8; 768]);
        assert!(flc.apply(0, &mut pixels, &mut palette).unwrap(), "the first frame sets colours");
        assert_eq!(pixels, [1, 1, 1, 1, 7, 8, 9, 9]);
        assert_eq!(&palette[3..6], &[63, 32, 1]);
        assert!(!flc.apply(1, &mut pixels, &mut palette).unwrap());
        assert_eq!(pixels, [1, 1, 1, 1, 7, 8, 5, 6], "the second's word on its second line");
        flc.apply(2, &mut pixels, &mut palette).unwrap();
        assert!(flc.apply(3, &mut pixels, &mut palette).is_err(), "no frame past the ring frame");
    }
}
