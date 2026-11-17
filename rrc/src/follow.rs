//! A recorded play followed as it is written (M6.5's step 2, D189): `rrc record SESSION --follow` records a session
//! whose play file (`clockwork play --record`) is still being written. Its input is given to Clockwork as the file's
//! marks say it is all there (`# at CYCLE`), the run is never let past the last mark (input after it may still come,
//! and a run without it would quietly be another game), and each state hash the window wrote (`# hash CYCLE HASH`,
//! its machine's state there before the input at it) is compared with the follower's at that cycle: the follower's
//! run is proved the window's as it goes, not assumed (D9).
//!
//! The input at a hash's cycle and after is given only once that hash is checked: input queued ahead changes when
//! Clockwork next services its devices, which its state hash covers.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::clockwork::Clockwork;

/// How often the file is read again while the run waits for the window.
const POLL: Duration = Duration::from_millis(20);

/// A play file followed as it is written.
pub(crate) struct Follow {
    path: PathBuf,
    /// The bytes read so far: whole lines, up to a mark.
    read: usize,
    /// The play's speed, from its header (0 until read).
    pub ips: u64,
    /// The Clockwork that played it (its source hash), if the file says.
    pub source: Option<String>,
    /// Every input up to this cycle is in; where the play ended.
    mark: Option<u64>,
    pub ended: Option<u64>,
    /// Inputs read and not given yet, in order: `<cycle> K 1E` lines as the file has them.
    waiting: VecDeque<(u64, String)>,
    /// The window's state hashes not checked yet: the cycle, the hash.
    hashes: VecDeque<(u64, String)>,
    /// Inputs before this cycle aren't the run's (a session from a snapshot: where its run starts).
    from: u64,
    /// Marks read and state hashes checked so far.
    pub marks: u64,
    pub checked: u64,
}

/// What the run may do next.
pub(crate) enum Next {
    /// Run, but not past this cycle.
    To(u64),
    /// The play ended and every input is given: run to the session's end.
    End,
}

impl Follow {
    /// The play in `path`, for a run that starts at cycle `from`.
    pub fn new(path: &Path, from: u64) -> Follow {
        Follow {
            path: path.to_owned(),
            read: 0,
            ips: 0,
            source: None,
            mark: None,
            ended: None,
            waiting: VecDeque::new(),
            hashes: VecDeque::new(),
            from,
            marks: 0,
            checked: 0,
        }
    }

    /// The run starts at cycle `cycle`: input before it isn't the run's.
    pub fn starting_at(&mut self, cycle: u64) {
        self.from = cycle;
        self.waiting.retain(|&(at, _)| at >= cycle);
    }

    /// Waits until the file has its header and a first mark: then its speed and its Clockwork are known.
    pub fn start(&mut self) -> Result<(), String> {
        loop {
            self.read()?;
            if self.mark.is_some() {
                return if self.ips == 0 { Err(format!("{}: no ips line", self.path.display())) } else { Ok(()) };
            }
            std::thread::sleep(POLL);
        }
    }

    /// Reads what the file has gained: its whole lines up to its last mark (a line after it may be half written).
    fn read(&mut self) -> Result<(), String> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("{}: {e}", self.path.display())),
        };
        let new = String::from_utf8_lossy(bytes.get(self.read..).unwrap_or_default()).into_owned();
        // Up to the end of the last whole mark line.
        let mut upto = 0;
        let mut at = 0;
        for line in new.split_inclusive('\n') {
            at += line.len();
            let trimmed = line.trim();
            if line.ends_with('\n') && (trimmed.starts_with("# at ") || trimmed.starts_with("# end ")) {
                upto = at;
            }
        }
        for line in new[..upto].lines().map(str::trim) {
            let number =
                |text: &str| text.trim().parse::<u64>().map_err(|_| format!("{}: {line:?}", self.path.display()));
            if let Some(cycle) = line.strip_prefix("# at ") {
                self.mark = Some(number(cycle)?);
                self.marks += 1;
            } else if let Some(cycle) = line.strip_prefix("# end ") {
                let cycle = number(cycle)?;
                (self.mark, self.ended) = (Some(cycle), Some(cycle));
            } else if let Some(hash) = line.strip_prefix("# hash ") {
                let (cycle, hash) = hash.split_once(' ').ok_or_else(|| format!("{}: {line:?}", self.path.display()))?;
                self.hashes.push_back((number(cycle)?, hash.trim().to_owned()));
            } else if line == "# clockwork session v1" {
                // The file's first line, not its Clockwork.
            } else if let Some(source) = line.strip_prefix("# clockwork ") {
                self.source = Some(source.trim().to_owned());
            } else if let Some(ips) = line.strip_prefix("ips ") {
                self.ips = number(ips)?;
            } else if let Some(cycle) = line.split(' ').next().and_then(|c| c.parse::<u64>().ok()) {
                if cycle >= self.from {
                    self.waiting.push_back((cycle, line.to_owned()));
                }
            }
        }
        self.read += upto;
        Ok(())
    }

    /// The run is at cycle `now`: the window's state hash there, if it took one, is compared with the follower's
    /// (and at the session's end, the last).
    pub fn check(&mut self, clockwork: &mut Clockwork, now: u64) -> Result<(), String> {
        self.read()?;
        while let Some((cycle, hash)) = self.hashes.front().cloned() {
            if cycle > now {
                return Ok(());
            }
            if cycle < now {
                return Err(format!("the play's state hash at cycle {cycle} is behind the run (at {now})"));
            }
            let ours = clockwork.ask("hash", Value::Null)?;
            let ours = ours["state_hash"].as_str().unwrap_or_default();
            if ours != hash {
                let at = if self.ips > 0 { now as f64 / self.ips as f64 } else { 0.0 };
                return Err(format!(
                    "the follower's run left the window's at {}:{:04.1} of play (cycle {now}): its state there is {ours}, \
                     the window's {hash}. A Clockwork bug (D9): the play file is kept, and replays",
                    (at / 60.0) as u64,
                    at % 60.0
                ));
            }
            self.hashes.pop_front();
            self.checked += 1;
        }
        Ok(())
    }

    /// The run is at cycle `now`: checks a state hash the window took there, gives Clockwork the input that comes
    /// before the next hash's cycle or the last mark, and says how far the run may go; waits for the window while
    /// the run is at the last mark.
    pub fn next(&mut self, clockwork: &mut Clockwork, now: u64) -> Result<Next, String> {
        loop {
            self.check(clockwork, now)?;
            let Some(mark) = self.mark else {
                std::thread::sleep(POLL);
                continue;
            };
            // Not past the next state hash: the input at its cycle comes once it is checked.
            let (bound, before) = match self.hashes.front() {
                Some(&(cycle, _)) if cycle <= mark => (cycle, cycle),
                _ => (mark, mark.saturating_add(1)),
            };
            let mut events = Vec::new();
            while let Some((cycle, _)) = self.waiting.front()
                && *cycle < before
            {
                events.push(self.waiting.pop_front().expect("there is one").1);
            }
            if !events.is_empty() {
                clockwork.ask("input", json!({ "events": events }))?;
            }
            if bound > now {
                return Ok(Next::To(bound));
            }
            if self.ended.is_some() && self.hashes.is_empty() && self.waiting.is_empty() {
                return Ok(Next::End);
            }
            std::thread::sleep(POLL);
        }
    }
}
