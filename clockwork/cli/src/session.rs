//! Recorded input sessions: timed input events that replay
//! bit-identically because time is virtual.
//!
//! Text format:
//! ```text
//! # clockwork session v1
//! program GAME.EXE
//! args -level 3
//! ips 8000000
//! 1234567 K 1E
//! 1250000 M 320 100 1
//! 1300000 J 0 -32767 0 1
//! ```
//! Data lines are `<cycle> K <scancode in hex>`, `<cycle> M <x> <y> <buttons>` (mouse position
//! in the driver's coordinates; buttons: bit 0 left, 1 right, 2 middle) or `<cycle> J <stick>
//! <x> <y> <buttons>` (joystick 0 or 1; position -32767..=32767; buttons: bit 0 button 1, bit 1
//! button 2). The older form `<cycle> <scancode>` is still read as a key.

use std::fmt::Write as _;
use std::path::Path;

use dos::Input;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    pub program: String,
    pub args: String,
    pub instructions_per_second: u64,
    pub inputs: Vec<(u64, Input)>,
}

const MAGIC: &str = "# clockwork session v1";

impl Session {
    pub fn to_text(&self) -> String {
        let mut out =
            format!("{MAGIC}\nprogram {}\nargs {}\nips {}\n", self.program, self.args, self.instructions_per_second);
        for (cycle, input) in &self.inputs {
            match input {
                Input::Key(code) => writeln!(out, "{cycle} K {code:02X}"),
                Input::Mouse { x, y, buttons } => writeln!(out, "{cycle} M {x} {y} {buttons}"),
                Input::Joystick { stick, x, y, buttons } => writeln!(out, "{cycle} J {stick} {x} {y} {buttons}"),
                Input::MouseMotion { dx, dy } => writeln!(out, "{cycle} R {dx} {dy}"),
            }
            .expect("writing to a String");
        }
        out
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut lines = text.lines();
        if !matches!(lines.next().map(str::trim), Some(MAGIC)) {
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
                    let cycle = cycle.parse().map_err(|_| format!("line {number}: expected '<cycle> K|M ...'"))?;
                    let input = parse_input(value).ok_or_else(|| format!("line {number}: bad input {value:?}"))?;
                    session.inputs.push((cycle, input));
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

fn parse_input(text: &str) -> Option<Input> {
    let fields: Vec<&str> = text.split_whitespace().collect();
    match fields.as_slice() {
        ["K", code] | [code] => u8::from_str_radix(code, 16).ok().map(Input::Key),
        ["M", x, y, buttons] => {
            Some(Input::Mouse { x: x.parse().ok()?, y: y.parse().ok()?, buttons: buttons.parse().ok()? })
        }
        ["R", dx, dy] => Some(Input::MouseMotion { dx: dx.parse().ok()?, dy: dy.parse().ok()? }),
        ["J", stick, x, y, buttons] => Some(Input::Joystick {
            stick: stick.parse().ok().filter(|&s: &u8| s < 2)?,
            x: x.parse().ok()?,
            y: y.parse().ok()?,
            buttons: buttons.parse().ok()?,
        }),
        _ => None,
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
            inputs: vec![
                (100, Input::Key(0x1E)),
                (180, Input::Mouse { x: 320, y: -1, buttons: 1 }),
                (200, Input::Joystick { stick: 1, x: -32767, y: 400, buttons: 2 }),
                (210, Input::MouseMotion { dx: -900, dy: 3 }),
                (250, Input::Key(0x9E)),
            ],
        };
        assert_eq!(Session::parse(&session.to_text()), Ok(session));
        assert!(Session::parse("hello").is_err());
    }

    #[test]
    fn reads_the_old_key_format() {
        let session = Session::parse("# clockwork session v1\nprogram A.EXE\nips 8000000\n100 1E\n").unwrap();
        assert_eq!(session.inputs, vec![(100, Input::Key(0x1E))]);
    }
}
