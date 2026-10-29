//! The compressions DOS games commonly used (M5, D101, D104), as the game's code decodes them: a port decodes
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

/// LZHUF (Haruyasu Yoshizaki's lzhuf.c, 1988, after Okumura's LZSS and adaptive Huffman coding), as
/// LHarc and many games of the era had it: LZSS with a ring of 4096 bytes, spaces below 4036 where writing
/// begins, matches of 3 to 60; the literals and the lengths are one adaptive Huffman code of 314 symbols,
/// and a match's place in the ring is its upper 6 bits in a fixed code and its lower 6 as they are. Bits are
/// read from each byte's top. Up to `size` bytes, or as many as `packed` gives.
pub fn lzhuf(packed: &[u8], size: usize) -> Vec<u8> {
    let mut bits = Bits { bytes: packed, at: 0, buffer: 0, held: 0 };
    let mut tree = Tree::new();
    let (code, length) = position_code();
    let mut ring = vec![0u8; huff::N];
    ring[..huff::N - huff::F].fill(b' ');
    let mut r = huff::N - huff::F;
    let mut out = Vec::with_capacity(size);
    let put = |c: u8, ring: &mut Vec<u8>, r: &mut usize, out: &mut Vec<u8>| {
        if out.len() < size {
            out.push(c);
        }
        ring[*r] = c;
        *r = (*r + 1) & (huff::N - 1);
    };
    // Past its end the input reads as zeros, as lzhuf.c's does: a few bytes more than it has at most.
    while out.len() < size && bits.at <= packed.len() + 2 {
        let c = tree.decode(&mut bits);
        if c < 256 {
            put(c as u8, &mut ring, &mut r, &mut out);
        } else {
            let first = bits.byte();
            let mut low = first;
            for _ in 2..length[first] {
                low = (low << 1) | bits.bit();
            }
            let place = usize::from(code[first]) << 6 | (low & 0x3F);
            let from = r.wrapping_sub(place).wrapping_sub(1) & (huff::N - 1);
            for k in 0..c - 255 + huff::THRESHOLD {
                put(ring[(from + k) & (huff::N - 1)], &mut ring, &mut r, &mut out);
            }
        }
        if out.len() >= size {
            break;
        }
    }
    out
}

/// LZHUF's sizes: the ring, the longest match, the shortest less one; the symbols (256 literals and the
/// lengths), the tree's nodes and its root.
mod huff {
    pub const N: usize = 4096;
    pub const F: usize = 60;
    pub const THRESHOLD: usize = 2;
    pub const SYMBOLS: usize = 256 - THRESHOLD + F;
    pub const T: usize = SYMBOLS * 2 - 1;
    pub const ROOT: usize = T - 1;
    pub const MAX_FREQ: u32 = 0x8000;
}

/// The fixed code of a place's upper 6 bits, by the byte that starts it: the value and the code's length (in
/// order of value, 1 code of 3 bits, 3 of 4, 8 of 5, 12 of 6, 24 of 7 and 16 of 8).
fn position_code() -> ([u8; 256], [usize; 256]) {
    let (mut code, mut length) = ([0u8; 256], [0usize; 256]);
    let (mut byte, mut value) = (0, 0u8);
    for (bits, count) in [(3, 1), (4, 3), (5, 8), (6, 12), (7, 24), (8, 16)] {
        for _ in 0..count {
            for _ in 0..1 << (8 - bits) {
                code[byte] = value;
                length[byte] = bits;
                byte += 1;
            }
            value += 1;
        }
    }
    (code, length)
}

/// Bits from the top of each byte, through a 16-bit window as lzhuf.c reads them.
struct Bits<'a> {
    bytes: &'a [u8],
    at: usize,
    buffer: u32,
    held: u32,
}

impl Bits<'_> {
    fn fill(&mut self) {
        while self.held <= 8 {
            let b = self.bytes.get(self.at).copied().unwrap_or(0);
            self.at += 1;
            self.buffer |= u32::from(b) << (8 - self.held);
            self.held += 8;
        }
    }

    fn bit(&mut self) -> usize {
        self.fill();
        let bit = (self.buffer >> 15) & 1;
        self.buffer = (self.buffer << 1) & 0xFFFF;
        self.held -= 1;
        bit as usize
    }

    fn byte(&mut self) -> usize {
        self.fill();
        let byte = (self.buffer >> 8) & 0xFF;
        self.buffer = (self.buffer << 8) & 0xFFFF;
        self.held -= 8;
        byte as usize
    }
}

/// LZHUF's adaptive Huffman tree: nodes by frequency, the leaves `T + symbol`, siblings side by side.
struct Tree {
    freq: Vec<u32>,
    parent: Vec<usize>,
    son: Vec<usize>,
}

impl Tree {
    fn new() -> Tree {
        use huff::*;
        let mut t = Tree { freq: vec![0; T + 1], parent: vec![0; T + SYMBOLS], son: vec![0; T] };
        for i in 0..SYMBOLS {
            t.freq[i] = 1;
            t.son[i] = i + T;
            t.parent[i + T] = i;
        }
        let (mut i, mut j) = (0, SYMBOLS);
        while j <= ROOT {
            t.freq[j] = t.freq[i] + t.freq[i + 1];
            t.son[j] = i;
            t.parent[i] = j;
            t.parent[i + 1] = j;
            i += 2;
            j += 1;
        }
        t.freq[T] = 0xFFFF;
        t.parent[ROOT] = 0;
        t
    }

    /// The tree made again from its leaves, their frequencies halved.
    fn rebuild(&mut self) {
        use huff::*;
        let mut j = 0;
        for i in 0..T {
            if self.son[i] >= T {
                self.freq[j] = self.freq[i].div_ceil(2);
                self.son[j] = self.son[i];
                j += 1;
            }
        }
        let (mut i, mut j) = (0, SYMBOLS);
        while j < T {
            let f = self.freq[i] + self.freq[i + 1];
            self.freq[j] = f;
            let mut k = j - 1;
            while f < self.freq[k] {
                k -= 1;
            }
            k += 1;
            self.freq.copy_within(k..j, k + 1);
            self.freq[k] = f;
            self.son.copy_within(k..j, k + 1);
            self.son[k] = i;
            i += 2;
            j += 1;
        }
        for i in 0..T {
            let k = self.son[i];
            self.parent[k] = i;
            if k < T {
                self.parent[k + 1] = i;
            }
        }
    }

    /// A symbol counted once more, the nodes on its way to the root kept in order of frequency.
    fn update(&mut self, symbol: usize) {
        use huff::*;
        if self.freq[ROOT] == MAX_FREQ {
            self.rebuild();
        }
        let mut c = self.parent[symbol + T];
        loop {
            self.freq[c] += 1;
            let k = self.freq[c];
            let mut l = c + 1;
            if k > self.freq[l] {
                while k > self.freq[l + 1] {
                    l += 1;
                }
                self.freq[c] = self.freq[l];
                self.freq[l] = k;
                let i = self.son[c];
                self.parent[i] = l;
                if i < T {
                    self.parent[i + 1] = l;
                }
                let j = self.son[l];
                self.son[l] = i;
                self.parent[j] = c;
                if j < T {
                    self.parent[j + 1] = c;
                }
                self.son[c] = j;
                c = l;
            }
            c = self.parent[c];
            if c == 0 {
                break;
            }
        }
    }

    /// From the root to a leaf, a bit a step: 0 the first of the two sons, 1 the second.
    fn decode(&mut self, bits: &mut Bits) -> usize {
        let mut c = self.son[huff::ROOT];
        while c < huff::T {
            c = self.son[c + bits.bit()];
        }
        let symbol = c - huff::T;
        self.update(symbol);
        symbol
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LZHUF's encoder, for the tests: literals and matches (how far back from the next byte less one, and
    /// how long) as lzhuf.c writes them.
    fn lzhuf_encoded(items: &[(usize, usize)]) -> Vec<u8> {
        let mut tree = Tree::new();
        let (code, length) = position_code();
        let mut bits: Vec<bool> = Vec::new();
        let put = |value: usize, n: usize, bits: &mut Vec<bool>| {
            for k in (0..n).rev() {
                bits.push(value >> k & 1 == 1);
            }
        };
        for &(symbol, place) in items {
            // The way from the leaf to the root, read backwards.
            let mut way = Vec::new();
            let mut k = tree.parent[symbol + huff::T];
            loop {
                way.push(k & 1 == 1);
                k = tree.parent[k];
                if k == huff::ROOT {
                    break;
                }
            }
            bits.extend(way.iter().rev());
            tree.update(symbol);
            if symbol >= 256 {
                let upper = place >> 6;
                let first = code.iter().position(|&v| usize::from(v) == upper).unwrap();
                put(first >> (8 - length[first]), length[first], &mut bits);
                put(place & 0x3F, 6, &mut bits);
            }
        }
        bits.chunks(8)
            .map(|byte| byte.iter().enumerate().fold(0u8, |b, (i, &bit)| b | u8::from(bit) << (7 - i)))
            .collect()
    }

    #[test]
    fn lzhuf_decodes_what_its_encoder_wrote() {
        // A, B, C; a match of 5 two back (B C B C B); 4 of the ring's spaces; Z.
        let match_of = |n: usize| 255 + n - huff::THRESHOLD;
        let packed = lzhuf_encoded(&[
            (usize::from(b'A'), 0),
            (usize::from(b'B'), 0),
            (usize::from(b'C'), 0),
            (match_of(5), 1),
            (match_of(4), 100),
            (usize::from(b'Z'), 0),
        ]);
        assert_eq!(lzhuf(&packed, 13), b"ABCBCBCB    Z");
        assert_eq!(lzhuf(&packed, 5), b"ABCBC", "stopped at the size");
        // Many of one byte: the tree is made again when the root's count reaches its most.
        let many: Vec<(usize, usize)> = (0..0x9000).map(|i| (usize::from(b'a') + i % 3, 0)).collect();
        let expected: Vec<u8> = (0..0x9000).map(|i| b'a' + (i % 3) as u8).collect();
        assert_eq!(lzhuf(&lzhuf_encoded(&many), expected.len()), expected);
    }

    #[test]
    fn the_place_code_covers_every_byte() {
        let (code, length) = position_code();
        assert_eq!((code[0], length[0], code[0x1F]), (0, 3, 0));
        assert_eq!((code[0x20], length[0x20], code[0x50], length[0x50]), (1, 4, 4, 5));
        assert_eq!((code[0xFF], length[0xFF]), (63, 8));
    }

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
