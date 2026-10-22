//! Clockwork as a child process, driven through `clockwork serve` (`clockwork/cli/src/serve.rs`): a
//! JSON request per line on its input, an answer per line on its output.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

use crate::repo::{Local, Repo};
use crate::session::Session;

pub struct Clockwork {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

impl Clockwork {
    pub fn start(exe: &Path) -> Result<Clockwork, String> {
        let mut child = Command::new(exe)
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Its warnings (file names that aren't 8.3) would drown rrc's short output; errors
            // come back in the answers.
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{}: {e}", exe.display()))?;
        let input = child.stdin.take().expect("a piped input");
        let output = BufReader::new(child.stdout.take().expect("a piped output"));
        Ok(Clockwork { child, input, output, next_id: 1 })
    }

    /// Sends `op` with the fields of `args` (an object, or null) and returns the answer.
    pub fn ask(&mut self, op: &str, args: Value) -> Result<Value, String> {
        let mut request = if args.is_null() { json!({}) } else { args };
        request["op"] = json!(op);
        request["id"] = json!(self.next_id);
        self.next_id += 1;
        writeln!(self.input, "{request}").and_then(|()| self.input.flush()).map_err(|e| format!("clockwork: {e}"))?;
        let mut line = String::new();
        let n = self.output.read_line(&mut line).map_err(|e| format!("clockwork: {e}"))?;
        if n == 0 {
            return Err(format!("clockwork stopped during {op}"));
        }
        let answer: Value = serde_json::from_str(&line).map_err(|e| format!("clockwork answered {line:?}: {e}"))?;
        match answer.get("error").and_then(Value::as_str) {
            Some(error) => Err(format!("clockwork {op}: {error}")),
            None => Ok(answer),
        }
    }

    /// Starts the port's original program in a fresh Clockwork, with `options` added to the
    /// port's own (`clockwork run` options); also returns what `load` answered (where the image is,
    /// its size).
    pub fn original(repo: &Repo, options: &[String]) -> Result<(Clockwork, Value), String> {
        let mut clockwork = Clockwork::start(&find(&repo.local)?)?;
        let loaded = clockwork.ask("load", json!({ "argv": argv(repo, options)? }))?;
        Ok((clockwork, loaded))
    }

    /// The session's run of the original, as [`Clockwork::original`] starts it; when a launcher starts
    /// a 16-bit game, `+OFF` counts from where the run will load the game (D57), and `loaded` says so.
    pub fn session(repo: &Repo, session: &Session) -> Result<(Clockwork, Value), String> {
        let (mut clockwork, mut loaded) = Clockwork::original(repo, &session.options(repo))?;
        crate::places::prepare(&mut clockwork, &mut loaded, repo, session)?;
        Ok((clockwork, loaded))
    }

    /// Counts `+OFF` from `image` (linear base, size) from now on, and puts it in `loaded` (what
    /// loading answered).
    pub fn rebase(&mut self, loaded: &mut Value, (base, size): (u32, u32)) -> Result<(), String> {
        self.ask("rebase", json!({ "at": format!("@{base:X}"), "len": size }))?;
        loaded["image"] = json!(format!("@{base:X}"));
        loaded["image_size"] = json!(size);
        Ok(())
    }
}

/// An address as Clockwork writes it: `+OFF` (in the image `+OFF` counts from), `@LINEAR`, or `%PHYS`
/// (physical memory, named so while paging is on). Hex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Address {
    Image(u32),
    Linear(u32),
    Physical(u32),
}

impl Address {
    pub fn parse(text: &str) -> Option<Address> {
        let (kind, number) = text.split_at_checked(1)?;
        let number = u32::from_str_radix(number, 16).ok()?;
        match kind {
            "+" => Some(Address::Image(number)),
            "@" => Some(Address::Linear(number)),
            "%" => Some(Address::Physical(number)),
            _ => None,
        }
    }

    /// Its number, whatever it counts in.
    pub fn number(self) -> u32 {
        match self {
            Address::Image(n) | Address::Linear(n) | Address::Physical(n) => n,
        }
    }

    /// Its linear address, given where `+OFF` counts from; none for physical memory.
    pub fn linear(self, image: u32) -> Option<u32> {
        match self {
            Address::Image(offset) => Some(image.wrapping_add(offset)),
            Address::Linear(linear) => Some(linear),
            Address::Physical(_) => None,
        }
    }

    /// The same kind of address at `number`.
    pub fn with(self, number: u32) -> Address {
        match self {
            Address::Image(_) => Address::Image(number),
            Address::Linear(_) => Address::Linear(number),
            Address::Physical(_) => Address::Physical(number),
        }
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Address::Image(offset) => write!(f, "+{offset:05X}"),
            Address::Linear(linear) => write!(f, "@{linear:X}"),
            Address::Physical(physical) => write!(f, "%{physical:X}"),
        }
    }
}

/// What `load` and `load_snapshot` take: the game's folder, its program, the port's options, then
/// `options` (`clockwork run` options).
pub fn argv(repo: &Repo, options: &[String]) -> Result<Vec<String>, String> {
    let original = &repo.settings.original;
    let mut argv = vec![repo.game_dir()?.to_string_lossy().into_owned(), original.program.clone()];
    argv.extend(original.options.iter().cloned());
    argv.extend(options.iter().cloned());
    Ok(argv)
}

/// A number in Clockwork's answer: one without it is a mismatch with the protocol, not a zero.
pub fn number(answer: &Value, key: &str) -> Result<u64, String> {
    answer[key].as_u64().ok_or_else(|| format!("clockwork answered without a number {key:?}: {answer}"))
}

/// A linear address (`@HEX`) in Clockwork's answer.
pub fn linear(answer: &Value, key: &str) -> Result<u32, String> {
    match answer[key].as_str().and_then(Address::parse) {
        Some(Address::Linear(linear)) => Ok(linear),
        _ => Err(format!("clockwork answered without a linear address {key:?}: {answer}")),
    }
}

impl Drop for Clockwork {
    fn drop(&mut self) {
        let _ = writeln!(self.input, "{}", json!({"op": "quit"}));
        let _ = self.input.flush();
        let _ = self.child.wait();
    }
}

/// The Clockwork program: `RRC_CLOCKWORK`, `tools.clockwork` in `rrc.local.toml`, next to `rrc`
/// itself (both are built into the same folder), or on the `PATH`.
pub fn find(local: &Local) -> Result<PathBuf, String> {
    let name = if cfg!(windows) { "clockwork.exe" } else { "clockwork" };
    let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join(name)));
    let on_path = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).map(|dir| dir.join(name)).collect::<Vec<_>>())
        .unwrap_or_default();
    std::env::var_os("RRC_CLOCKWORK")
        .map(PathBuf::from)
        .into_iter()
        .chain(local.tools.clockwork.clone())
        .chain(beside)
        .chain(on_path)
        .find(|p| p.is_file())
        .ok_or_else(|| "Clockwork not found: build it (cargo build --release -p clockwork) or set RRC_CLOCKWORK".into())
}

/// `clockwork --version`.
pub fn version(exe: &Path) -> Option<String> {
    let output = Command::new(exe).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.trim().trim_start_matches("clockwork ").to_owned())
}
