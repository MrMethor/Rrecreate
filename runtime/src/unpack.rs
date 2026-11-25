//! A packed program's load image, unpacked by the port itself (D226): a port played on its own has no Clockwork to
//! unpack the game's program as `rrc` has it done (D224), so it undoes the packer's compression as the packer's own
//! decompressor does.
//!
//! PKLITE 1.12 with its "extra" and "large" compression, as its decompressor in the packed program does it (read in
//! Arena's ACD.EXE, its stub at the load image's start). The compressed data starts at the load image's offset 290h,
//! a stream of 16-bit little-endian words read a bit at a time from their lowest bit; a word is read as soon as the
//! last bit of the one before is taken. A 0 bit is a literal: the next byte of the stream, XORed with the count of
//! bits left in the current word ("extra" compression). A 1 bit is a copy: its length by a code, 25 and up by a
//! byte, and its distance's high byte by a code (none for a length of 2), its low byte by a byte. The length code's
//! byte FEh is a segment's end (nothing copied), FFh the data's end. Then the relocations, in groups (a count, its
//! offsets, from a segment 0FFFh paragraphs on from the last group's), and the program's SS, SP, CS and IP.
//!
//! Its result is Clockwork's (`clockwork unpack` runs the decompressor) byte for byte: on Arena's ACD.EXE the
//! 305,520 bytes of the load image are the same. Another packer, or another PKLITE, is said, never guessed.

/// The PKLITE signature an MZ program packed by it carries after its header's words.
const SIGNATURE: &[u8] = b"PK";

/// A packed program's header: PKLITE's version (minor, then major with the flags) at 1Ch.
const VERSION: usize = 0x1C;
/// 1.12, extra compression (10h) and large (20h).
const KNOWN: [u8; 2] = [0x0C, 0x31];

/// Where the compressed data starts in the load image.
const DATA: usize = 0x290;

/// The decoder's tables in the decompressor (PKLITE 1.12's large model): lengths by code, at its three levels, and the
/// distance's high byte by code. Indexed as the decompressor indexes them, from its bits.
const LENGTHS_1: [u8; 13] = [4, 0, 5, 6, 7, 0, 0, 0, 0, 0, 8, 9, 10];
const LENGTHS_2: [u8; 13] = [0, 0, 11, 12, 25, 0, 0, 0, 0, 0, 13, 14, 15];
const LENGTHS_3: [u8; 16] = [0, 0, 16, 17, 18, 0, 0, 0, 0, 0, 19, 20, 21, 22, 23, 24];
const HIGH: [u8; 23] = [1, 2, 0, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 7, 8, 9, 10, 11, 12, 13];

/// A program unpacked: its load image and its relocations (segment:offset in the image, as an MZ header lists them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unpacked {
    pub image: Vec<u8>,
    pub relocations: Vec<(u16, u16)>,
    /// SS, SP, CS and IP, the segments from the image's start.
    pub registers: [u16; 4],
}

/// Whether the MZ program `file` is packed by PKLITE.
pub fn is_pklite(file: &[u8]) -> bool {
    file.starts_with(b"MZ") && file.get(0x1E..0x20) == Some(SIGNATURE)
}

/// The MZ program `file` unpacked, if PKLITE packed it (none if it isn't packed so).
pub fn pklite(file: &[u8]) -> Result<Option<Unpacked>, String> {
    if !is_pklite(file) {
        return Ok(None);
    }
    let version = [file[VERSION], file[VERSION + 1]];
    if version != KNOWN {
        return Err(format!(
            "packed by PKLITE {}.{:02} (flags {:02X}h): only 1.12 with extra and large compression is unpacked",
            version[1] & 0x0F,
            version[0],
            version[1] & 0xF0
        ));
    }
    let header = usize::from(u16::from_le_bytes([file[8], file[9]])) * 16;
    let data = file.get(header + DATA..).ok_or("the packed program is shorter than its decompressor")?;
    Decoder::new(data)?.run().map(Some)
}

/// The bit stream and the bytes it is read beside.
struct Decoder<'a> {
    data: &'a [u8],
    at: usize,
    bits: u16,
    left: u8,
}

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8]) -> Result<Decoder<'a>, String> {
        let mut d = Decoder { data, at: 0, bits: 0, left: 16 };
        d.bits = d.word()?;
        Ok(d)
    }

    fn byte(&mut self) -> Result<u8, String> {
        let b = *self.data.get(self.at).ok_or("the compressed data ends early")?;
        self.at += 1;
        Ok(b)
    }

    fn word(&mut self) -> Result<u16, String> {
        Ok(u16::from(self.byte()?) | u16::from(self.byte()?) << 8)
    }

    /// The next bit; the next word read as soon as this one's last bit is taken.
    fn bit(&mut self) -> Result<u16, String> {
        let bit = self.bits & 1;
        self.bits >>= 1;
        self.left -= 1;
        if self.left == 0 {
            self.bits = self.word()?;
            self.left = 16;
        }
        Ok(bit)
    }

    /// `code` with the next bit after it.
    fn more(&mut self, code: u16) -> Result<u16, String> {
        Ok(code << 1 | self.bit()?)
    }

    /// A copy's length, by its code: 25 means a byte follows.
    fn length(&mut self) -> Result<u8, String> {
        let mut code = self.bit()?;
        code = self.more(code)?;
        if code >= 2 {
            return Ok(code as u8);
        }
        code = self.more(code)?;
        if code == 0 {
            return Ok(LENGTHS_1[0]);
        }
        code = self.more(code)?;
        if code < 5 {
            return Ok(LENGTHS_1[usize::from(code)]);
        }
        code = self.more(code)?;
        if code <= 12 {
            return Ok(LENGTHS_1[usize::from(code)]);
        }
        code = self.more(code & 3)?;
        if code < 5 {
            return Ok(LENGTHS_2[usize::from(code)]);
        }
        code = self.more(code)?;
        if code <= 12 {
            return Ok(LENGTHS_2[usize::from(code)]);
        }
        code = self.more(code & 3)?;
        if code >= 5 {
            code = self.more(code)?;
        }
        Ok(LENGTHS_3[usize::from(code)])
    }

    /// A copy's distance's high byte, by its code.
    fn high(&mut self) -> Result<u8, String> {
        if self.bit()? == 1 {
            return Ok(0);
        }
        let mut code = self.bit()?;
        code = self.more(code)?;
        code = self.more(code)?;
        if code < 2 {
            return Ok(HIGH[usize::from(code)]);
        }
        code = self.more(code)?;
        if code < 8 {
            return Ok(HIGH[usize::from(code)]);
        }
        code = self.more(code)?;
        if code < 0x17 {
            return Ok(HIGH[usize::from(code)]);
        }
        code = self.more(code)?;
        Ok((code & 0xDF) as u8)
    }

    fn run(mut self) -> Result<Unpacked, String> {
        let mut image = Vec::new();
        loop {
            if self.bit()? == 0 {
                let b = self.byte()?;
                image.push(b ^ self.left);
                continue;
            }
            let mut length = usize::from(self.length()?);
            if length == 25 {
                match self.byte()? {
                    0xFE => continue,
                    0xFF => break,
                    more => length = 25 + usize::from(more),
                }
            }
            let high = if length == 2 { 0 } else { self.high()? };
            let distance = usize::from(high) << 8 | usize::from(self.byte()?);
            let from =
                image.len().checked_sub(distance).filter(|_| distance > 0).ok_or("a copy from before the start")?;
            for i in 0..length {
                image.push(image[from + i]);
            }
        }
        let mut relocations = Vec::new();
        let mut segment = 0u16;
        loop {
            let count = self.word()?;
            if count == 0xFFFF {
                break;
            }
            for _ in 0..count {
                relocations.push((segment, self.word()?));
            }
            if count > 0 {
                segment = segment.wrapping_add(0x0FFF);
            }
        }
        let registers = [self.word()?, self.word()?, self.word()?, self.word()?];
        Ok(Unpacked { image, relocations, registers })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream written as the decompressor reads it: bits from each word's lowest, a word put where the decoder
    /// reads it (the first at once, each next as the last bit of the one before is taken), bytes between them.
    #[derive(Default)]
    struct Stream {
        out: Vec<u8>,
        word: Option<usize>,
        bits: u16,
        used: u8,
    }

    impl Stream {
        fn new() -> Stream {
            let mut s = Stream::default();
            s.open();
            s
        }

        fn open(&mut self) {
            self.word = Some(self.out.len());
            self.out.extend([0, 0]);
            self.bits = 0;
            self.used = 0;
        }

        fn bit(&mut self, b: u16) {
            self.bits |= b << self.used;
            self.used += 1;
            let at = self.word.unwrap();
            self.out[at..at + 2].copy_from_slice(&self.bits.to_le_bytes());
            if self.used == 16 {
                self.open();
            }
        }

        fn bits(&mut self, code: &str) {
            for c in code.chars() {
                self.bit(u16::from(c == '1'));
            }
        }

        /// The count of bits the decoder has left after the last one taken.
        fn left(&self) -> u8 {
            16 - self.used
        }

        fn literal(&mut self, b: u8) {
            self.bit(0);
            let key = self.left();
            self.out.push(b ^ key);
        }
    }

    fn packed(stream: &[u8]) -> Vec<u8> {
        let mut file = vec![0u8; 0x20 + DATA];
        file[..2].copy_from_slice(b"MZ");
        file[8] = 2; // a header of 2 paragraphs
        file[VERSION..VERSION + 2].copy_from_slice(&KNOWN);
        file[0x1E..0x20].copy_from_slice(SIGNATURE);
        file.extend_from_slice(stream);
        file
    }

    #[test]
    fn literals_copies_and_the_end_unpack_as_the_decompressor_reads_them() {
        let mut s = Stream::new();
        for b in *b"ABC" {
            s.literal(b);
        }
        // A copy of 3 (code 11) from 3 back: its high byte's code 1 (0), its low byte 3.
        s.bits("1");
        s.bits("11");
        s.bits("1");
        s.out.push(3);
        // A copy of 2 (code 10) from 1 back: no high byte.
        s.bits("1");
        s.bits("10");
        s.out.push(1);
        // A copy of 7 (code 0100) from 2 back, over what it copies.
        s.bits("1");
        s.bits("0100");
        s.bits("1");
        s.out.push(2);
        // A segment's end (code 25, 011100, then FEh), a literal, then the data's end (FFh).
        s.bits("1");
        s.bits("011100");
        s.out.push(0xFE);
        s.literal(b'Z');
        s.bits("1");
        s.bits("011100");
        s.out.push(0xFF);
        // Relocations: a group of one (0:0002), the end; then SS, SP, CS, IP.
        s.out.extend([1, 0, 2, 0, 0xFF, 0xFF, 0x10, 0, 0x80, 0, 0, 0, 0, 0]);
        let unpacked = pklite(&packed(&s.out)).unwrap().unwrap();
        assert_eq!(unpacked.image, [&b"ABCABC"[..], &[b'C'; 9], b"Z"].concat());
        assert_eq!(unpacked.relocations, [(0, 2)]);
        assert_eq!(unpacked.registers, [0x10, 0x80, 0, 0]);
    }

    /// A packed game's program against Clockwork's unpacking of it (`clockwork unpack`), when both are given
    /// (`RRC_TEST_PKLITE`, `RRC_TEST_UNPACKED`): the game's files are never in the repository (D33).
    #[test]
    fn a_game_s_program_unpacks_as_clockwork_unpacks_it() {
        let (Some(packed), Some(unpacked)) =
            (std::env::var_os("RRC_TEST_PKLITE"), std::env::var_os("RRC_TEST_UNPACKED"))
        else {
            return;
        };
        let ours = pklite(&std::fs::read(packed).unwrap()).unwrap().unwrap();
        let theirs = crate::original::Program::from_mz(&std::fs::read(unpacked).unwrap()).unwrap();
        assert_eq!(ours.image.len(), theirs.image().len());
        assert!(ours.image == theirs.image(), "the images differ");
    }

    #[test]
    fn a_program_not_packed_or_packed_otherwise_is_said() {
        assert_eq!(pklite(b"MZ plain program here........").unwrap(), None);
        let mut other = packed(&[0, 0]);
        other[VERSION] = 0x0E;
        assert!(pklite(&other).unwrap_err().contains("PKLITE 1.14"));
    }
}
