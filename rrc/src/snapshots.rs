//! Snapshots of a session's run (DESIGN.md §8, D51), kept in the cache so that a question about a late
//! tick starts near it instead of from power-on. `rrc record` saves one at the first tick after every
//! 10 s of emulated time (`RRC_SNAPSHOT_MS` changes that); `rrc watch` and `rrc inputs` start from
//! the last one before the tick they begin at.
//!
//! They are found by what makes the run and its ticks the same: Clockwork's sources (D76), the game's
//! files, the session's input and options, and the sync points. A snapshot holds the machine with
//! its sync points' counts and the input still to come, so a run from it goes on as the recording
//! did.
//!
//! Results of whole-session questions (`rrc calls`, `rrc inputs`) are kept the same way, by the run
//! and everything they read: asked again, they come from the cache.

use std::cell::RefCell;

use serde_json::{Value, json};

use crate::cache::{Cache, sha256, sha256_file};
use crate::clockwork::{self, Clockwork};
use crate::repo::Repo;
use crate::session::Session;
use crate::trace::SyncPoint;

/// Emulated time between snapshots: `RRC_SNAPSHOT_MS`, else 10 s.
pub fn spacing_ms() -> u64 {
    std::env::var("RRC_SNAPSHOT_MS").ok().and_then(|ms| ms.parse().ok()).filter(|&ms| ms > 0).unwrap_or(10_000)
}

/// A snapshot `rrc record` kept: at the stop of tick `tick` (cycle, ms), in the cache under `hash`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Taken {
    pub tick: u64,
    pub cycle: u64,
    pub ms: u64,
    pub hash: String,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Index {
    #[serde(default, rename = "snapshot")]
    snapshots: Vec<Taken>,
}

/// What makes a session's run the same: Clockwork's sources (not its program's bytes, which any
/// rebuild changes: D76), the game's files, the session's input and options (as a hash).
pub fn run_identity(repo: &Repo, session: &Session) -> Result<String, String> {
    let exe = clockwork::source(&clockwork::find(&repo.local)?)?;
    let files = crate::game::hash_folder(repo.game_dir()?, &repo.state()?)?;
    let input = |name: &Option<String>| -> Result<String, String> {
        match name {
            Some(name) => sha256_file(&repo.sessions().join(name)),
            None => Ok(String::new()),
        }
    };
    let original = &repo.settings.original;
    let mut identity = json!({
        "clockwork": exe, "files": files, "program": original.program, "options": original.options,
        "session": session.options, "inputs": input(&session.inputs)?, "replay": input(&session.replay)?,
    });
    // The scenario's edits, as the bytes they write (D64); none leaves the identity as before.
    if !session.edits.is_empty() {
        let k = crate::knowledge::Knowledge::load(&repo.knowledge())?;
        identity["edits"] = json!(crate::edits::listed(&crate::edits::resolve(&k, &session.edits)?));
    }
    Ok(sha256(identity.to_string().as_bytes()))
}

/// The key of a session's snapshots: its run, and the sync points that number its ticks.
fn key(repo: &Repo, session: &Session, syncs: &[SyncPoint]) -> Result<String, String> {
    Ok(sha256(format!("{}\n{}", run_identity(repo, session)?, json!(syncs)).as_bytes()))
}

/// Saves snapshots during a recording.
pub struct Saver {
    cache: Cache,
    key: String,
    taken: Vec<Taken>,
    next_ms: u64,
    file: std::path::PathBuf,
}

impl Saver {
    pub fn new(repo: &Repo, session: &Session, syncs: &[SyncPoint]) -> Result<Saver, String> {
        // This process's own: recordings of other sessions may run meanwhile.
        let file = repo.state()?.join(format!("snapshot-{}.tmp", std::process::id()));
        Ok(Saver {
            cache: Cache::open(&repo.local)?,
            key: key(repo, session, syncs)?,
            taken: Vec::new(),
            next_ms: spacing_ms(),
            file,
        })
    }

    /// At the stop of tick `tick`: a snapshot, if the time since the last one has come.
    pub fn tick(&mut self, clockwork: &mut Clockwork, tick: u64, cycle: u64, ms: u64) -> Result<(), String> {
        if ms < self.next_ms {
            return Ok(());
        }
        let spacing = spacing_ms();
        self.next_ms = (ms / spacing + 1) * spacing;
        clockwork.ask("save", json!({ "file": self.file }))?;
        let bytes = std::fs::read(&self.file).map_err(|e| format!("{}: {e}", self.file.display()))?;
        let _ = std::fs::remove_file(&self.file);
        self.taken.push(Taken { tick, cycle, ms, hash: self.cache.put(&bytes)? });
        Ok(())
    }

    /// Keeps the index of the snapshots taken; how many.
    pub fn finish(self) -> Result<usize, String> {
        let index = Index { snapshots: self.taken };
        self.cache.remember("snapshots", &self.key, &toml::to_string(&index).map_err(|e| e.to_string())?)?;
        Ok(index.snapshots.len())
    }
}

/// The last snapshot of the session (with these sync points) before tick `tick`, if one was kept.
pub fn before(repo: &Repo, session: &Session, syncs: &[SyncPoint], tick: u64) -> Result<Option<Taken>, String> {
    let cache = Cache::open(&repo.local)?;
    let Some(text) = cache.recall("snapshots", &key(repo, session, syncs)?) else { return Ok(None) };
    let index: Index = toml::from_str(&text).unwrap_or_default();
    Ok(index.snapshots.into_iter().rfind(|t| t.tick < tick && cache.has(&t.hash)))
}

/// Clockwork with the session loaded from `taken` (its sync points and the input still to come are
/// in the snapshot); what `load_snapshot` answered.
pub fn load(repo: &Repo, session: &Session, taken: &Taken) -> Result<(Clockwork, Value), String> {
    let cache = Cache::open(&repo.local)?;
    let mut clockwork = Clockwork::start(&clockwork::find(&repo.local)?)?;
    // The session's own options, but not its input: the snapshot has what is still to come.
    let argv = clockwork::argv(repo, &session.options)?;
    let file = cache.path(&taken.hash);
    let mut loaded = clockwork.ask("load_snapshot", json!({ "argv": argv, "file": file }))?;
    crate::places::prepare(&mut clockwork, &mut loaded, repo, session)?;
    Ok((clockwork, loaded))
}

/// A snapshot of the session's original at the end of tick `tick`, in the cache (a paired checkpoint's,
/// D63): from the last one `rrc record` kept before, run on to there.
pub fn at_tick(repo: &Repo, session: &Session, k: &crate::knowledge::Knowledge, tick: u64) -> Result<String, String> {
    let syncs = crate::trace::sync_points(k)?;
    let (mut clockwork, _, from) = crate::watch::open(repo, session, &syncs, tick + 1)?;
    crate::watch::to_tick(&mut clockwork, tick + 1, from.as_ref(), (session.seconds * 1000.0) as u64)?;
    let file = repo.state()?.join(format!("snapshot-{}.tmp", std::process::id()));
    clockwork.ask("save", json!({ "file": file }))?;
    let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let _ = std::fs::remove_file(&file);
    Cache::open(&repo.local)?.put(&bytes)
}

thread_local! {
    /// What `say` printed while a result is being kept.
    static CAPTURE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Keeps a line of output for a result being kept ([`kept`]).
pub fn captured(line: &str) {
    CAPTURE.with(|c| {
        if let Some(text) = c.borrow_mut().as_mut() {
            text.push_str(line);
            text.push('\n');
        }
    });
}

/// Runs `question` (which prints its answer), or prints the answer kept from an earlier run with the
/// same `key`; keeps a new answer under it.
pub fn kept(
    repo: &Repo,
    key: &str,
    question: impl FnOnce() -> Result<std::process::ExitCode, String>,
) -> Result<std::process::ExitCode, String> {
    let cache = Cache::open(&repo.local)?;
    if let Some(text) = cache.recall("results", key) {
        print!("{text}");
        crate::say("kept", "this answer from an earlier run of the same session and knowledge");
        return Ok(std::process::ExitCode::SUCCESS);
    }
    CAPTURE.with(|c| *c.borrow_mut() = Some(String::new()));
    let result = question();
    let text = CAPTURE.with(|c| c.borrow_mut().take()).unwrap_or_default();
    if result.is_ok() {
        cache.remember("results", key, &text)?;
    }
    result
}

/// The key of what a session's run gives Clockwork's probes, kept as it came (contract inference, file
/// tracing): the run and `what` was asked (a version, when what is kept changes). Neither rrc's build nor
/// the knowledge changes a run: a rebuilt rrc or a new name doesn't run the session again (D92).
pub fn run_key(repo: &Repo, session: &Session, what: &str) -> Result<String, String> {
    Ok(sha256(
        format!(
            "{what}
{}",
            run_identity(repo, session)?
        )
        .as_bytes(),
    ))
}

/// The key of a whole-session answer: the build of `rrc` that gave it, the command and its arguments,
/// the session's run, and what it reads: the knowledge files and the analysis.
pub fn result_key(repo: &Repo, session: &Session, command: &[String]) -> Result<String, String> {
    let mut parts = vec![crate::cache::own_build()?, command.join("\u{1}"), run_identity(repo, session)?];
    for file in ["functions.tsv", "data.tsv", "types.toml", "ticks.toml"] {
        let path = repo.knowledge().join(file);
        parts.push(if path.is_file() { sha256_file(&path)? } else { String::new() });
    }
    let index = repo.root.join(crate::repo::STATE).join("analysis.toml");
    parts.push(if index.is_file() { sha256_file(&index)? } else { String::new() });
    Ok(sha256(parts.join("\n").as_bytes()))
}
