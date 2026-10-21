//! Stable IDs of a program's functions and data (DESIGN.md §5, decision D53).
//!
//! Canonical text form: `<MODULE>[#<n>]+<offset>`, for example `GAME.EXE+0A4D2` or
//! `GAME.EXE#3+00120`. `#n` is a part of the module: an overlay of an MZ program, or an object of an
//! LE/LX program. The offset is relative to the start of the module's (unpacked) load image or of
//! the part, never a runtime `segment:offset`: load segments and where parts are placed vary between
//! runs. The module `MEM` is memory outside any program's image (the interrupt vectors, the BIOS's
//! data, other programs' memory), its offset the linear address: `MEM+0046C` is the BIOS's timer
//! count (D53). Tested against `testdata/function_ids.tsv`, which every implementation of the
//! format shares.

use std::fmt;
use std::str::FromStr;

const MODULE_MAX_LEN: usize = 64;
const OFFSET_MIN_DIGITS: usize = 5;
const OFFSET_MAX_DIGITS: usize = 8;
const MODULE_PUNCTUATION: &str = "._-~!$%&()@^{}'";

/// The module of memory outside any program's image, by linear address (D53).
pub const MEMORY: &str = "MEM";

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id {
    module: String,
    part: Option<u16>,
    offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseIdError {
    input: String,
    reason: &'static str,
}

impl fmt::Display for ParseIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid id {:?}: {}", self.input, self.reason)
    }
}

impl std::error::Error for ParseIdError {}

impl Id {
    /// Builds an ID from parts. `module` must be an uppercase DOS-style file name.
    pub fn new(module: impl Into<String>, part: Option<u16>, offset: u32) -> Result<Self, ParseIdError> {
        let module = module.into();
        if let Err(reason) = validate_module(&module) {
            return Err(ParseIdError { input: module, reason });
        }
        if module == MEMORY && part.is_some() {
            return Err(ParseIdError { input: module, reason: "MEM has no parts" });
        }
        Ok(Self { module, part, offset })
    }

    /// The byte at a linear address outside any program's image (`MEM+...`).
    pub fn memory(linear: u32) -> Self {
        Self { module: MEMORY.into(), part: None, offset: linear }
    }

    /// Whether it names memory outside the programs' images.
    pub fn is_memory(&self) -> bool {
        self.module == MEMORY
    }

    pub fn module(&self) -> &str {
        &self.module
    }

    /// The overlay (MZ) or object (LE/LX), if any.
    pub fn part(&self) -> Option<u16> {
        self.part
    }

    /// Offset into the module's load image (or into the part, if any).
    pub fn offset(&self) -> u32 {
        self.offset
    }
}

fn validate_module(module: &str) -> Result<(), &'static str> {
    if module.is_empty() {
        return Err("empty module name");
    }
    if module.len() > MODULE_MAX_LEN {
        return Err("module name longer than 64 characters");
    }
    let valid = module.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || MODULE_PUNCTUATION.contains(c));
    if !valid {
        return Err("module name must be an uppercase DOS file name");
    }
    Ok(())
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.module)?;
        if let Some(part) = self.part {
            write!(f, "#{part}")?;
        }
        write!(f, "+{:05X}", self.offset)
    }
}

impl FromStr for Id {
    type Err = ParseIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = |reason| ParseIdError { input: s.to_owned(), reason };

        let (head, offset_text) = s.rsplit_once('+').ok_or_else(|| err("missing '+<offset>'"))?;
        let (module, part) = match head.split_once('#') {
            Some((module, part)) => {
                let part = part.parse::<u16>().map_err(|_| err("bad part number"))?;
                (module, Some(part))
            }
            None => (head, None),
        };

        let digits_ok = (OFFSET_MIN_DIGITS..=OFFSET_MAX_DIGITS).contains(&offset_text.len())
            && offset_text.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b));
        if !digits_ok {
            return Err(err("offset must be 5 to 8 uppercase hex digits"));
        }
        let offset = u32::from_str_radix(offset_text, 16).map_err(|_| err("bad offset"))?;

        let id = Id::new(module, part, offset).map_err(|e| err(e.reason))?;
        // Rejects leading zeros beyond 5 digits, "#03", "#+3" and similar non-canonical spellings.
        if id.to_string() != s {
            return Err(err("not in canonical form"));
        }
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTORS: &str = include_str!("../../testdata/function_ids.tsv");

    #[test]
    fn shared_vectors() {
        let mut checked = 0;
        for line in VECTORS.lines().map(|l| l.trim_end_matches('\r')) {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            assert_eq!(cols.len(), 5, "malformed vector line: {line:?}");
            let (expect, text) = (cols[0], cols[1]);
            let parsed = text.parse::<Id>();
            match expect {
                "ok" => {
                    let id = parsed.unwrap_or_else(|e| panic!("{e}"));
                    assert_eq!(id.module(), cols[2]);
                    let part = (cols[3] != "-").then(|| cols[3].parse::<u16>().unwrap());
                    assert_eq!(id.part(), part);
                    assert_eq!(id.offset(), u32::from_str_radix(cols[4], 16).unwrap());
                    assert_eq!(id.to_string(), text);
                }
                "err" => assert!(parsed.is_err(), "{text:?} should be rejected"),
                other => panic!("unknown expectation {other:?}"),
            }
            checked += 1;
        }
        assert!(checked >= 10, "too few vectors read: {checked}");
    }

    #[test]
    fn new_rejects_bad_modules() {
        assert!(Id::new("game.exe", None, 0).is_err());
        assert!(Id::new("", None, 0).is_err());
        assert!(Id::new("A".repeat(65), None, 0).is_err());
        assert_eq!(Id::new("GAME.EXE", Some(2), 0x120).unwrap().to_string(), "GAME.EXE#2+00120");
    }
}
