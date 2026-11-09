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
    // Recorded play: as far as the session runs (D124).
    let replay = match &session.replay {
        Some(name) => crate::play::identity(&repo.sessions().join(name), session.seconds)?,
        None => String::new(),
    };
    let original = &repo.settings.original;
    let mut identity = json!({
        "clockwork": exe, "files": files, "program": original.program, "options": original.options,
        "session": session.options, "inputs": input(&session.inputs)?, "replay": replay,
    });
    // The session and tick it starts from (D118): that run's, at the tick's cycle where it is kept (D128: a tick's
    // number moves when sync points are added).
    if let Some(from) = &session.from {
        let base = Session::load(repo, &from.session)?;
        identity["from"] = match from.cycle {
            Some(cycle) => json!({"run": run_identity(repo, &base)?, "cycle": cycle}),
            None => json!({"run": run_identity(repo, &base)?, "tick": from.tick}),
        };
    }
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

/// What nothing had written where a based session starts (D150), as a file for Clockwork's `uninit`: what its base's run
/// hadn't by then, going back to power-on through its own base. Kept for the base's run and the cycle. None for a session
/// from power-on: Clockwork counts what a program finds when it starts.
pub fn unwritten(repo: &Repo, session: &Session) -> Result<Option<std::path::PathBuf>, String> {
    let (Some(start), Some(from)) = (start(repo, session)?, &session.from) else { return Ok(None) };
    let base = Session::load(repo, &from.session)?;
    let cache = Cache::open(&repo.local)?;
    let key = sha256(format!("{}\n{}", run_identity(repo, &base)?, start.cycle).as_bytes());
    if let Some(hash) = cache.recall("unwritten", &key).filter(|hash| cache.has(hash)) {
        return Ok(Some(cache.path(&hash)));
    }
    let inherited = unwritten(repo, &base)?;
    let (mut clockwork, _) = Clockwork::session(repo, &base)?;
    clockwork.ask("uninit", track_from(inherited.as_deref()))?;
    let now = clockwork::number(&clockwork.ask("regs", Value::Null)?, "cycles")?;
    if start.cycle > now {
        clockwork.ask("run", json!({ "cycles": start.cycle - now }))?;
    }
    let file = repo.state()?.join(format!("unwritten-{}.tmp", std::process::id()));
    clockwork.ask("uninit_unwritten", json!({ "file": file }))?;
    let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let _ = std::fs::remove_file(&file);
    let hash = cache.put(&bytes)?;
    cache.remember("unwritten", &key, &hash)?;
    Ok(Some(cache.path(&hash)))
}

/// `uninit`'s request for a run that starts where `unwritten` was kept, or from power-on.
pub fn track_from(unwritten: Option<&std::path::Path>) -> Value {
    match unwritten {
        Some(file) => json!({ "unwritten": file }),
        None => json!({}),
    }
}

/// How long a whole run of the session is from where it starts: its length less the time from power-on it starts at
/// (D118, D148). A `run` is for that long, not until that time.
pub fn to_run(repo: &Repo, session: &Session) -> Result<u64, String> {
    let start = based(repo, session)?.map_or(0, |(_, ms)| ms);
    Ok(((session.seconds * 1000.0) as u64).saturating_sub(start))
}

/// Where a session based on another starts (D118): its base's snapshot at the base's tick, in the cache, and its
/// time from power-on (ms). None for a session from power-on.
pub fn based(repo: &Repo, session: &Session) -> Result<Option<(String, u64)>, String> {
    Ok(start(repo, session)?.map(|s| (s.hash, s.ms)))
}

/// A based session's start: its base's snapshot where the base tick's pass begins (`hash`: the hit before it), its
/// time from power-on, the tick as the base's sync points number it now, and the cycle of that hit before it.
#[derive(Debug, Clone, PartialEq)]
pub struct Start {
    pub hash: String,
    pub ms: u64,
    pub tick: u64,
    pub cycle: u64,
}

/// Where a session based on another starts: by the base tick's cycle where the session keeps it (D128), so that
/// sync points added to the base, which number its ticks otherwise, don't move it; else by the tick's number. Kept
/// for the base's run and sync points.
pub fn start(repo: &Repo, session: &Session) -> Result<Option<Start>, String> {
    let Some(from) = &session.from else { return Ok(None) };
    let base = Session::load(repo, &from.session)?;
    let k = crate::knowledge::Knowledge::load(&repo.knowledge())?;
    let syncs = crate::trace::sync_points(&k)?;
    let key = based_key(repo, &base, &syncs, from)?;
    let cache = Cache::open(&repo.local)?;
    if let Some(kept) = kept_based(&cache, &key) {
        return Ok(Some(kept));
    }
    let length_ms = (base.seconds * 1000.0) as u64;
    let (mut clockwork, tick, cycle, ms) = match from.cycle {
        Some(cycle) => {
            // The cycle is the base's hit before its tick (where the tick's pass begins): from the last snapshot at
            // it or before, hit by hit to it; the tick is the next.
            let cache_index = cache.recall("snapshots", &key_of(repo, &base, &syncs)?);
            let index: Index = cache_index.and_then(|t| toml::from_str(&t).ok()).unwrap_or_default();
            let taken = index.snapshots.into_iter().rfind(|t| t.cycle <= cycle && cache.has(&t.hash));
            let (mut clockwork, mut at) = match &taken {
                Some(t) => (load(repo, &base, t)?.0, (t.tick, t.cycle, t.ms)),
                None => (crate::trace::begin(repo, &base, &syncs)?.0, (0, 0, 0)),
            };
            while at.1 < cycle {
                let ran = clockwork.ask("run", json!({"sync": "next", "until_ms": length_ms}))?;
                if ran.get("sync").is_none() {
                    return Err(format!("{}'s run ends before its tick at cycle {cycle}", from.session));
                }
                at = (at.0 + 1, clockwork::number(&ran, "cycles")?, clockwork::number(&ran, "ms")?);
            }
            if at.1 != cycle {
                return Err(format!(
                    "{}'s run has no tick at cycle {cycle} (its tick {} was there): its run changed (its input, the \
                     game or Clockwork), not only its sync points",
                    from.session, from.tick
                ));
            }
            (clockwork, at.0 + 1, at.1, at.2)
        }
        None => {
            let (mut clockwork, _, taken) = crate::watch::open(repo, &base, &syncs, from.tick)?;
            let ms = crate::watch::to_tick(&mut clockwork, from.tick, taken.as_ref(), length_ms)
                .map_err(|e| format!("{}'s tick {}: {e}", from.session, from.tick))?;
            let cycle = clockwork::number(&clockwork.ask("regs", Value::Null)?, "cycles")?;
            (clockwork, from.tick, cycle, ms)
        }
    };
    let file = repo.state()?.join(format!("snapshot-{}.tmp", std::process::id()));
    clockwork.ask("save", json!({ "file": file }))?;
    let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let _ = std::fs::remove_file(&file);
    let hash = cache.put(&bytes)?;
    cache.remember("based", &key, &format!("{hash} {ms} {tick} {cycle}"))?;
    Ok(Some(Start { hash, ms, tick, cycle }))
}

/// A based session's base tick settled by its cycle (D128): the tick's number as the base's sync points number it
/// now, and the cycle kept where it wasn't. What changed, to say, if anything did.
pub fn settle(repo: &Repo, session: &mut Session) -> Result<Option<String>, String> {
    let Some(start) = start(repo, session)? else { return Ok(None) };
    let from = session.from.as_mut().expect("a based session");
    let said = match (from.cycle, from.tick == start.tick) {
        (Some(_), true) => None,
        (Some(_), false) => Some(format!(
            "{}'s tick {} is its tick {} now: its sync points number its ticks so (the same moment, its cycle {})",
            from.session, from.tick, start.tick, start.cycle
        )),
        (None, _) => Some(format!("{}'s tick {}: kept by its cycle too, {}", from.session, start.tick, start.cycle)),
    };
    (from.tick, from.cycle) = (start.tick, Some(start.cycle));
    Ok(said)
}

/// Whether where `session` starts is kept already: [`based`] then needs no run (`rrc play` says when it does).
pub fn based_kept(repo: &Repo, session: &Session) -> Result<bool, String> {
    let Some(from) = &session.from else { return Ok(true) };
    let base = Session::load(repo, &from.session)?;
    let syncs = crate::trace::sync_points(&crate::knowledge::Knowledge::load(&repo.knowledge())?)?;
    Ok(kept_based(&Cache::open(&repo.local)?, &based_key(repo, &base, &syncs, from)?).is_some())
}

/// The key a base's snapshot is kept under: its run and sync points, and the cycle of the base tick where it is
/// kept, else its number.
fn based_key(repo: &Repo, base: &Session, syncs: &[SyncPoint], from: &crate::session::Base) -> Result<String, String> {
    let place = match from.cycle {
        Some(cycle) => format!("based 2\ncycle {cycle}"),
        None => format!("based 1\n{}", from.tick),
    };
    Ok(sha256(format!("{place}\n{}", key(repo, base, syncs)?).as_bytes()))
}

/// The start kept under `key`, if its snapshot is in the cache (a kept one without its tick and cycle isn't one).
fn kept_based(cache: &Cache, key: &str) -> Option<Start> {
    cache.recall("based", key).and_then(|text| {
        let mut parts = text.split_whitespace();
        let hash = parts.next()?.to_owned();
        let (ms, tick, cycle) = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
        cache.has(&hash).then_some(Start { hash, ms, tick, cycle })
    })
}

/// The key of a session's snapshots ([`key`]), for the snapshots' index.
fn key_of(repo: &Repo, session: &Session, syncs: &[SyncPoint]) -> Result<String, String> {
    key(repo, session, syncs)
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
