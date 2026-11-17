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
//!
//! Written as it is played (`play --record`, [`Recording`]), the file has `# at <cycle>` every 100 ms of
//! emulated time: every input up to that cycle is in the file above it; `# end <cycle>` when the play
//! stopped; `# clockwork <source>` after the header, the source hash of the Clockwork that played it; and
//! `# hash <cycle> <state hash>` when the play starts and every 30 s of emulated time after, the machine's
//! state at that cycle before the input at it, so that a tool following the play proves its run the
//! window's (M6.5, D189). All are comments to a reader of the session.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

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
    #[cfg(test)]
    pub fn to_text(&self) -> String {
        let mut out = header(&self.program, &self.args, self.instructions_per_second);
        for (cycle, input) in &self.inputs {
            out.push_str(&line(*cycle, input));
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
                _ => session.inputs.push(event(line).map_err(|e| format!("line {number}: {e}"))?),
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
}

/// One input line of a session, `<cycle> K|M|R|J ...`: when, and what.
pub fn event(line: &str) -> Result<(u64, Input), String> {
    let (cycle, value) = line.trim().split_once(' ').unwrap_or((line, ""));
    let cycle = cycle.parse().map_err(|_| format!("expected '<cycle> K|M ...', not {line:?}"))?;
    let input = parse_input(value).ok_or_else(|| format!("bad input {value:?}"))?;
    Ok((cycle, input))
}

/// The file's first lines: the magic line, the program, its arguments and the speed.
fn header(program: &str, args: &str, ips: u64) -> String {
    format!("{MAGIC}\nprogram {program}\nargs {args}\nips {ips}\n")
}

/// An input's line.
fn line(cycle: u64, input: &Input) -> String {
    let mut out = String::new();
    match input {
        Input::Key(code) => writeln!(out, "{cycle} K {code:02X}"),
        Input::Mouse { x, y, buttons } => writeln!(out, "{cycle} M {x} {y} {buttons}"),
        Input::Joystick { stick, x, y, buttons } => writeln!(out, "{cycle} J {stick} {x} {y} {buttons}"),
        Input::MouseMotion { dx, dy } => writeln!(out, "{cycle} R {dx} {dy}"),
    }
    .expect("writing to a String");
    out
}

/// Emulated time between `# at` marks: how far behind the window a follower may be held (M6.5, D189).
pub const MARK_MS: u64 = 100;

/// Emulated time between `# hash` lines: a state hash costs the window about 9 ms (M6.5's step 0).
pub const HASH_SECONDS: u64 = 30;

/// A session written while it is played, so that a tool can follow it: its inputs as they come,
/// `# at <cycle>` every [`MARK_MS`] once every input up to that cycle is written (flushed then), and
/// `# hash <cycle> <state hash>` at the start and every [`HASH_SECONDS`].
pub struct Recording {
    file: std::io::BufWriter<std::fs::File>,
    path: PathBuf,
    /// Cycles between marks, and the cycle the next is due at; the same for state hashes.
    every: u64,
    next: u64,
    hash_every: u64,
    next_hash: u64,
    inputs: usize,
}

impl Recording {
    /// Starts the file at `path` for a play at `ips` from cycle `now`, by the Clockwork whose source hash is
    /// `source`.
    pub fn create(
        path: &Path,
        program: &str,
        args: &str,
        ips: u64,
        now: u64,
        source: &str,
    ) -> Result<Recording, String> {
        let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file = std::io::BufWriter::new(file);
        let (every, hash_every) = ((ips * MARK_MS / 1000).max(1), (ips * HASH_SECONDS).max(1));
        let mut recording =
            Recording { file, path: path.to_owned(), every, next: now, hash_every, next_hash: now, inputs: 0 };
        recording.write(&header(program, args, ips))?;
        recording.write(&format!("# clockwork {source}\n"))?;
        Ok(recording)
    }

    /// State hashes every `cycles` instead of every [`HASH_SECONDS`] (`play --hash-every`).
    pub fn hash_every(&mut self, cycles: u64) {
        self.hash_every = cycles.max(1);
    }

    /// Whether a state hash is due at `cycle`: then [`Recording::hash`] the machine's, before the input at it.
    pub fn hash_due(&self, cycle: u64) -> bool {
        cycle >= self.next_hash
    }

    /// The machine's state hash at `cycle`, before the input at it.
    pub fn hash(&mut self, cycle: u64, state: u64) -> Result<(), String> {
        self.next_hash = cycle + self.hash_every;
        self.write(&format!("# hash {cycle} {state:016x}\n"))
    }

    /// An input given at `cycle`.
    pub fn input(&mut self, cycle: u64, input: &Input) -> Result<(), String> {
        self.inputs += 1;
        self.write(&line(cycle, input))
    }

    /// Every input up to `cycle` is written: a mark if one is due.
    pub fn at(&mut self, cycle: u64) -> Result<(), String> {
        if cycle < self.next {
            return Ok(());
        }
        self.next = cycle + self.every;
        self.write(&format!("# at {cycle}\n"))?;
        self.file.flush().map_err(|e| format!("{}: {e}", self.path.display()))
    }

    /// The play stopped at `cycle`: the last mark. How many inputs it recorded.
    pub fn end(mut self, cycle: u64) -> Result<usize, String> {
        self.write(&format!("# end {cycle}\n"))?;
        self.file.flush().map_err(|e| format!("{}: {e}", self.path.display()))?;
        Ok(self.inputs)
    }

    fn write(&mut self, text: &str) -> Result<(), String> {
        self.file.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", self.path.display()))
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
    fn a_recording_is_a_session_with_its_marks() {
        let path = std::env::temp_dir().join(format!("clockwork-recording-{}.txt", std::process::id()));
        let mut recording = Recording::create(&path, "GAME.EXE", "", 1000, 5000, "abc").unwrap();
        assert!(recording.hash_due(5000), "a hash when the play starts");
        recording.hash(5000, 0x1234).unwrap();
        recording.at(5000).unwrap();
        recording.input(5050, &Input::Key(0x1E)).unwrap();
        recording.at(5050).unwrap();
        recording.input(5100, &Input::Key(0x9E)).unwrap();
        recording.at(5100).unwrap();
        assert!(!recording.hash_due(34_999) && recording.hash_due(35_000), "then every 30 s");
        recording.input(6100, &Input::Mouse { x: 3, y: 4, buttons: 1 }).unwrap();
        recording.at(6100).unwrap();
        // Read while it is played: up to the last mark.
        let text = std::fs::read_to_string(&path).unwrap();
        let marks: Vec<&str> = text.lines().filter(|l| l.starts_with("# at")).collect();
        assert_eq!(marks, ["# at 5000", "# at 5100", "# at 6100"], "one every 100 ms (100 cycles here)");
        assert!(text.contains("ips 1000\n# clockwork abc\n# hash 5000 0000000000001234\n# at 5000\n"), "{text}");
        assert_eq!(recording.end(6150).unwrap(), 3);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with("6100 M 3 4 1\n# at 6100\n# end 6150\n"), "{text}");
        let session = Session::load(&path).unwrap();
        let mouse = Input::Mouse { x: 3, y: 4, buttons: 1 };
        assert_eq!(session.inputs, vec![(5050, Input::Key(0x1E)), (5100, Input::Key(0x9E)), (6100, mouse)]);
        assert_eq!(event("6100 M 3 4 1"), Ok((6100, mouse)));
        assert!(event("6100 Q").is_err() && event("# at 5").is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn reads_the_old_key_format() {
        let session = Session::parse("# clockwork session v1\nprogram A.EXE\nips 8000000\n100 1E\n").unwrap();
        assert_eq!(session.inputs, vec![(100, Input::Key(0x1E))]);
    }
}
