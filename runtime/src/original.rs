//! The person's copy of the game, for what a port loads from it instead of copying it into its source
//! (D82): the original program's static data (tables its code reads, such as a sine table) and the
//! game's files. `rrc` gives a port both when it runs it: `RRC_PROGRAM`, the program as the analysis
//! unpacked it, and `RRC_GAME`, the game's folder.

use std::path::PathBuf;
use std::sync::OnceLock;

/// The original program's load image: what its IDs' offsets count in (`ACD.EXE+46A3C` is offset
/// `0x46A3C`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    image: Vec<u8>,
}

impl Program {
    /// An MZ program's load image: the file past its header, as long as the header says.
    pub fn from_mz(bytes: &[u8]) -> Result<Program, String> {
        let word = |at: usize| bytes.get(at..at + 2).map(|w| usize::from(u16::from_le_bytes([w[0], w[1]])));
        if !(bytes.starts_with(b"MZ") || bytes.starts_with(b"ZM")) {
            return Err("not an MZ program".into());
        }
        let (last, pages, header) = (word(2).unwrap_or(0), word(4).unwrap_or(0), word(8).unwrap_or(0) * 16);
        let size = if last == 0 { pages * 512 } else { pages.saturating_sub(1) * 512 + last };
        let image = bytes.get(header..size.min(bytes.len())).ok_or("the program is shorter than its header")?;
        Ok(Program { image: image.to_vec() })
    }

    /// `len` bytes at `offset`. Panics past the image: static data is in it.
    pub fn bytes(&self, offset: u32, len: usize) -> &[u8] {
        let start = offset as usize;
        self.image.get(start..start + len).unwrap_or_else(|| {
            panic!("+{offset:05X}, {len} bytes: past the program's image ({} bytes)", self.image.len())
        })
    }

    /// `n` words at `offset`.
    pub fn u16s(&self, offset: u32, n: usize) -> Vec<u16> {
        self.bytes(offset, 2 * n).chunks_exact(2).map(|w| u16::from_le_bytes([w[0], w[1]])).collect()
    }

    /// `n` signed words at `offset`.
    pub fn i16s(&self, offset: u32, n: usize) -> Vec<i16> {
        self.bytes(offset, 2 * n).chunks_exact(2).map(|w| i16::from_le_bytes([w[0], w[1]])).collect()
    }
}

/// The original program, as `rrc` gave it (`RRC_PROGRAM`). Panics, saying why, when there is none: a
/// port that reads it runs through `rrc`, after `rrc analyze`.
pub fn program() -> &'static Program {
    static PROGRAM: OnceLock<Result<Program, String>> = OnceLock::new();
    let loaded = PROGRAM.get_or_init(|| {
        let path = std::env::var_os("RRC_PROGRAM")
            .ok_or("the port reads the original program (RRC_PROGRAM): run it through rrc, after rrc analyze")?;
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", PathBuf::from(&path).display()))?;
        Program::from_mz(&bytes)
    });
    match loaded {
        Ok(program) => program,
        Err(e) => panic!("{e}"),
    }
}

/// The game's folder (`RRC_GAME`), if `rrc` gave one.
pub fn game_dir() -> Option<PathBuf> {
    std::env::var_os("RRC_GAME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_mz_program_s_image_is_past_its_header() {
        // Two paragraphs of header, 4 bytes of image: 36 bytes, 1 page whose last has 36.
        let mut mz = vec![0u8; 32];
        mz[..2].copy_from_slice(b"MZ");
        mz[2..4].copy_from_slice(&36u16.to_le_bytes());
        mz[4..6].copy_from_slice(&1u16.to_le_bytes());
        mz[8..10].copy_from_slice(&2u16.to_le_bytes());
        mz.extend([0x92, 0x01, 0xFE, 0xFF, 0xAA]);
        let program = Program::from_mz(&mz).unwrap();
        assert_eq!(program.u16s(0, 2), [0x0192, 0xFFFE]);
        assert_eq!(program.i16s(2, 1), [-2], "the byte past the header's length isn't the image's");
        assert!(Program::from_mz(b"PK").is_err());
    }
}
