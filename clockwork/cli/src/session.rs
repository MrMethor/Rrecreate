//! Recorded input sessions (PROJECT.md §5.2, "Sessions"): timed scancodes that replay
//! bit-identically because time is virtual.
//!
//! Text format:
//! ```text
//! # rec-host session v1
//! program GAME.EXE
//! args -level 3
//! ips 8000000
//! 1234567 1E
//! 1250000 9E
//! ```
//! Each data line is `<cycle> <scancode in hex>`.

use std::fmt::Write as _;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    pub program: String,
    pub args: String,
    pub instructions_per_second: u64,
    pub inputs: Vec<(u64, u8)>,
}

const MAGIC: &str = "# rec-host session v1";

impl Session {
    pub fn to_text(&self) -> String {
        let mut out =
            format!("{MAGIC}\nprogram {}\nargs {}\nips {}\n", self.program, self.args, self.instructions_per_second);
        for (cycle, code) in &self.inputs {
            writeln!(out, "{cycle} {code:02X}").expect("writing to a String");
        }
        out
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut lines = text.lines();
        if lines.next().map(str::trim) != Some(MAGIC) {
            return Err(format!("not a session file (first line must be {MAGIC:?})"));
        }
        let mut session = Session::default();
        for (number, line) in lines.enumerate().map(|(i, l)| (i + 2, l.trim())) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            match key {
                "program" => session.program = value.to_owned(),
                "args" => session.args = value.to_owned(),
                "ips" => {
                    session.instructions_per_second = value.parse().map_err(|_| format!("line {number}: bad ips"))?
                }
                cycle => {
                    let cycle = cycle.parse().map_err(|_| format!("line {number}: expected '<cycle> <scancode>'"))?;
                    let code =
                        u8::from_str_radix(value, 16).map_err(|_| format!("line {number}: bad scancode {value:?}"))?;
                    session.inputs.push((cycle, code));
                }
            }
        }
        if session.program.is_empty() || session.instructions_per_second == 0 {
            return Err("session file needs 'program' and 'ips' lines".into());
        }
        Ok(session)
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        std::fs::write(path, self.to_text()).map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let session = Session {
            program: "GAME.EXE".into(),
            args: "-nosound".into(),
            instructions_per_second: 8_000_000,
            inputs: vec![(100, 0x1E), (250, 0x9E)],
        };
        assert_eq!(Session::parse(&session.to_text()), Ok(session));
        assert!(Session::parse("hello").is_err());
    }
}
