//! Snapshots of a session's run (DESIGN.md §8, D51), kept in the cache so that a question about a late
//! tick starts near it instead of from power-on. `rrc record` saves one at the first tick after every
//! 10 s of emulated time (`RRC_SNAPSHOT_MS` changes that); `rrc watch` and `rrc inputs` start from
//! the last one before the tick they begin at.
//!
//! They are found by what makes the run and its ticks the same: Clockwork's program, the game's
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

/// What makes a session's run the same: Clockwork's program, the game's files, the session's input
/// and options (as a hash).
pub fn run_identity(repo: &Repo, session: &Session) -> Result<String, String> {
    let exe = sha256_file(&clockwork::find(&repo.local)?)?;
    let files = crate::game::hash_folder(repo.game_dir()?, &repo.state()?)?;
    let input = |name: &Option<String>| -> Result<String, String> {
        match name {
            Some(name) => sha256_file(&repo.sessions().join(name)),
            None => Ok(String::new()),
        }
    };
    let original = &repo.settings.original;
    let identity = json!({
        "clockwork": exe, "files": files, "program": original.program, "options": original.options,
        "session": session.options, "inputs": input(&session.inputs)?, "replay": input(&session.replay)?,
    });
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
        let file = repo.state()?.join("snapshot.tmp");
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
    let original = &repo.settings.original;
    let mut argv = vec![repo.game_dir()?.to_string_lossy().into_owned(), original.program.clone()];
    argv.extend(original.options.iter().cloned());
    // The session's own options, but not its input: the snapshot has what is still to come.
    argv.extend(session.options.iter().cloned());
    let file = cache.path(&taken.hash);
    let mut loaded = clockwork.ask("load_snapshot", json!({ "argv": argv, "file": file }))?;
    if launched_16bit(repo) {
        clockwork.rebase(&mut loaded, placement(repo, session)?.image)?;
    }
    Ok((clockwork, loaded))
}

/// Whether the game's program is an LE executable (a flat program under a DOS extender), whose IDs
/// name its objects (`GAME.EXE#2+00010`).
pub fn is_le(repo: &Repo) -> bool {
    let Ok(dir) = repo.game_dir() else { return false };
    let Ok(file) = std::fs::read(dir.join(repo.module_file())) else { return false };
    let header = file.get(0x3C..0x40).map_or(0, |h| u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as usize);
    file.starts_with(b"MZ") && file.get(header..header + 2) == Some(b"LE")
}

/// Whether a launcher starts the game's program, a 16-bit one: its image is wherever the session's
/// run loads it ([`placement`]), and Clockwork counts `+OFF` from there ([`Clockwork::session`]).
pub fn launched_16bit(repo: &Repo) -> bool {
    repo.module() != repo.program_module() && !is_le(repo)
}

/// Where a session's run puts the game's program: its load image (linear base, size) and, for an LE
/// program, its objects (linear base, size and LE flags: 4 executable; in the LE's order).
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    pub image: (u32, u32),
    pub objects: Vec<(u32, u32, u32)>,
}

/// Where the session's run puts the game's program (D55, D56): found once, by running the session until
/// the program runs (and its extender has placed its objects), and kept by the run
/// (`index/places/<run>`). Clockwork is deterministic, so every run of the session puts it there. The
/// program is found by name, running or having started the one that runs (an extender's loader), so
/// another program a launcher runs first doesn't count (Daggerfall's runs REPORT.EXE before
/// FALL.EXE).
pub fn placement(repo: &Repo, session: &Session) -> Result<Placement, String> {
    let cache = Cache::open(&repo.local)?;
    let key = sha256(format!("{}\n{}", run_identity(repo, session)?, repo.module_file()).as_bytes());
    let parse = |text: &str| -> Option<Placement> {
        let hex = |t: &str| u32::from_str_radix(t, 16).ok();
        let mut placement = Placement { image: (0, 0), objects: Vec::new() };
        for line in text.lines() {
            let (what, fields) = line.split_once(' ')?;
            let mut fields = fields.split(':').map(hex);
            match what {
                "image" => placement.image = (fields.next()??, fields.next()??),
                "object" => placement.objects.push((fields.next()??, fields.next()??, fields.next()??)),
                _ => return None,
            }
        }
        Some(placement)
    };
    if let Some(placement) = cache.recall("places", &key).as_deref().and_then(parse) {
        return Ok(placement);
    }
    let (mut clockwork, _) = Clockwork::original(repo, &session.options(repo))?;
    let (module, le) = (repo.module(), is_le(repo));
    let length_ms = (session.seconds * 1000.0) as u64;
    let hex = |v: &Value| v.as_str().unwrap_or("").trim_start_matches('@').to_owned();
    let mut ms = 0;
    loop {
        ms = (ms + 50).min(length_ms);
        let ran = clockwork.ask("run", json!({ "until_ms": ms }))?;
        let answer = clockwork.ask("objects", json!({ "program": module }))?;
        let objects = answer["objects"].as_array();
        if !answer["program"].is_null() && (!le || objects.is_some()) {
            let mut text =
                format!("image {}:{:X}\n", hex(&answer["image"]), answer["image_size"].as_u64().unwrap_or(0));
            for o in objects.into_iter().flatten() {
                let (size, flags) = (o["size"].as_u64().unwrap_or(0), o["flags"].as_u64().unwrap_or(0));
                text.push_str(&format!("object {}:{size:X}:{flags:X}\n", hex(&o["at"])));
            }
            cache.remember("places", &key, &text)?;
            return parse(&text).ok_or_else(|| "Clockwork's answer can't be read".into());
        }
        if ms >= length_ms || ran["stop"].as_str() != Some("time limit reached") {
            return Err(match le {
                true => {
                    format!("{module} places no LE objects in this session: IDs with #N are an LE program's objects")
                }
                false => format!("{module} doesn't run in this session"),
            });
        }
    }
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

/// The key of a whole-session answer: the command and its arguments, the session's run, and what it
/// reads: the knowledge files and the analysis.
pub fn result_key(repo: &Repo, session: &Session, command: &[String]) -> Result<String, String> {
    let mut parts = vec![env!("CARGO_PKG_VERSION").to_owned(), command.join("\u{1}"), run_identity(repo, session)?];
    for file in ["functions.tsv", "data.tsv", "types.toml", "ticks.toml"] {
        let path = repo.knowledge().join(file);
        parts.push(if path.is_file() { sha256_file(&path)? } else { String::new() });
    }
    let index = repo.root.join(crate::repo::STATE).join("analysis.toml");
    parts.push(if index.is_file() { sha256_file(&index)? } else { String::new() });
    Ok(sha256(parts.join("\n").as_bytes()))
}
