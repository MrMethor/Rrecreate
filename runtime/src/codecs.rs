//! The compressions DOS games commonly used (M5, D101), as the game's code decodes them: a port decodes
//! the game's files with these, and `rrc asset` extracts with the same. A game's own compression is the
//! port's to write, as the rest of its code.

/// PCX's run-length code: a byte with its top two bits set repeats the next byte its low six times; any
/// other byte is itself. Up to `size` bytes, or as many as `packed` gives.
pub fn runs(packed: &[u8], size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    let mut i = 0;
    while i < packed.len() && out.len() < size {
        let b = packed[i];
        i += 1;
        if b & 0xC0 == 0xC0 {
            let Some(&value) = packed.get(i) else { break };
            i += 1;
            let n = usize::from(b & 0x3F).min(size - out.len());
            out.extend(std::iter::repeat_n(value, n));
        } else {
            out.push(b);
        }
    }
    out
}

/// An LZSS variant: a ring of `ring` bytes (a power of two) filled with `fill` below `start`, where
/// writing begins, and matches of `shortest` bytes and more. A flag byte comes before each eight items,
/// its low bit first: 1 a literal byte, 0 a match of two bytes, the low 8 bits of its place in the ring,
/// then the place's high 4 bits and the length less `shortest` in 4 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lzss {
    pub ring: usize,
    pub fill: u8,
    pub start: usize,
    pub shortest: usize,
}

impl Lzss {
    /// Haruhiko Okumura's lzss.c (1989), which much of the era's LZSS came from: a 4096-byte ring of
    /// spaces, writing from 4078, matches of 3 to 18. (Its ring above 4078 isn't filled; this fills it with
    /// zeros, which an encoder that matches only what it wrote never reads.)
    pub const OKUMURA: Lzss = Lzss { ring: 4096, fill: b' ', start: 4096 - 18, shortest: 3 };

    /// Up to `size` bytes, or as many as `packed` gives.
    pub fn decode(&self, packed: &[u8], size: usize) -> Vec<u8> {
        let mask = self.ring - 1;
        let mut ring = vec![0u8; self.ring];
        ring[..self.start].fill(self.fill);
        let mut out = Vec::with_capacity(size);
        let (mut r, mut i, mut flags) = (self.start, 0, 0u16);
        let put = |c: u8, ring: &mut Vec<u8>, r: &mut usize, out: &mut Vec<u8>| {
            if out.len() < size {
                out.push(c);
            }
            ring[*r] = c;
            *r = (*r + 1) & mask;
        };
        loop {
            flags >>= 1;
            if flags & 0x100 == 0 {
                let Some(&b) = packed.get(i) else { break };
                i += 1;
                flags = u16::from(b) | 0xFF00;
            }
            if flags & 1 != 0 {
                let Some(&c) = packed.get(i) else { break };
                i += 1;
                put(c, &mut ring, &mut r, &mut out);
            } else {
                let (Some(&low), Some(&high)) = (packed.get(i), packed.get(i + 1)) else { break };
                i += 2;
                let place = usize::from(low) | usize::from(high & 0xF0) << 4;
                let length = usize::from(high & 0x0F) + self.shortest;
                for k in 0..length {
                    let c = ring[(place + k) & mask];
                    put(c, &mut ring, &mut r, &mut out);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_repeat_and_pass_bytes_through() {
        assert_eq!(runs(&[0xC3, 7, 1, 0xC1, 0xC5], 10), [7, 7, 7, 1, 0xC5]);
        assert_eq!(runs(&[0xFF, 9], 4), [9; 4], "stopped at the size");
        assert_eq!(runs(&[1, 0xC2], 4), [1], "a run cut short");
    }

    #[test]
    fn lzss_copies_from_the_ring_and_its_spaces() {
        // Literals A, B; a match of 3 from the start (4078): A B A; a match of 3 of the ring's spaces.
        let packed = [0b0000_0011, b'A', b'B', 0xEE, 0xF0, 0x00, 0x00];
        assert_eq!(Lzss::OKUMURA.decode(&packed, 8), b"ABABA   ");
        assert_eq!(Lzss::OKUMURA.decode(&packed, 4), b"ABAB", "stopped at the size");
    }
}
