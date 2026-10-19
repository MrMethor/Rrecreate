//! Stable function identifiers.
//!
//! Canonical text form: `<MODULE>[#<overlay>]+<offset>`, for example `GAME.EXE+0A4D2` or
//! `GAME.EXE#3+00120`. The offset is relative to the start of the module's (unpacked) load
//! image, never a runtime `segment:offset`: load segments and overlay placement vary
//! between runs. Tested against `testdata/function_ids.tsv`, which every implementation of the
//! format shares.

use std::fmt;
use std::str::FromStr;

const MODULE_MAX_LEN: usize = 64;
const OFFSET_MIN_DIGITS: usize = 5;
const OFFSET_MAX_DIGITS: usize = 8;
const MODULE_PUNCTUATION: &str = "._-~!$%&()@^{}'";

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FunctionId {
    module: String,
    overlay: Option<u16>,
    offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseFunctionIdError {
    input: String,
    reason: &'static str,
}

impl fmt::Display for ParseFunctionIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid function id {:?}: {}", self.input, self.reason)
    }
}

impl std::error::Error for ParseFunctionIdError {}

impl FunctionId {
    /// Builds an ID from parts. `module` must be an uppercase DOS-style file name.
    pub fn new(module: impl Into<String>, overlay: Option<u16>, offset: u32) -> Result<Self, ParseFunctionIdError> {
        let module = module.into();
        if let Err(reason) = validate_module(&module) {
            return Err(ParseFunctionIdError { input: module, reason });
        }
        Ok(Self { module, overlay, offset })
    }

    pub fn module(&self) -> &str {
        &self.module
    }

    pub fn overlay(&self) -> Option<u16> {
        self.overlay
    }

    /// Offset into the module's load image (or into the overlay, if any).
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

impl fmt::Display for FunctionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.module)?;
        if let Some(overlay) = self.overlay {
            write!(f, "#{overlay}")?;
        }
        write!(f, "+{:05X}", self.offset)
    }
}

impl FromStr for FunctionId {
    type Err = ParseFunctionIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = |reason| ParseFunctionIdError { input: s.to_owned(), reason };

        let (head, offset_text) = s.rsplit_once('+').ok_or_else(|| err("missing '+<offset>'"))?;
        let (module, overlay) = match head.split_once('#') {
            Some((module, overlay)) => {
                let overlay = overlay.parse::<u16>().map_err(|_| err("bad overlay number"))?;
                (module, Some(overlay))
            }
            None => (head, None),
        };

        let digits_ok = (OFFSET_MIN_DIGITS..=OFFSET_MAX_DIGITS).contains(&offset_text.len())
            && offset_text.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b));
        if !digits_ok {
            return Err(err("offset must be 5 to 8 uppercase hex digits"));
        }
        let offset = u32::from_str_radix(offset_text, 16).map_err(|_| err("bad offset"))?;

        let id = FunctionId::new(module, overlay, offset).map_err(|e| err(e.reason))?;
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

    const VECTORS: &str = include_str!("../../../testdata/function_ids.tsv");

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
            let parsed = text.parse::<FunctionId>();
            match expect {
                "ok" => {
                    let id = parsed.unwrap_or_else(|e| panic!("{e}"));
                    assert_eq!(id.module(), cols[2]);
                    let overlay = (cols[3] != "-").then(|| cols[3].parse::<u16>().unwrap());
                    assert_eq!(id.overlay(), overlay);
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
        assert!(FunctionId::new("game.exe", None, 0).is_err());
        assert!(FunctionId::new("", None, 0).is_err());
        assert!(FunctionId::new("A".repeat(65), None, 0).is_err());
        assert_eq!(FunctionId::new("GAME.EXE", Some(2), 0x120).unwrap().to_string(), "GAME.EXE#2+00120");
    }
}
