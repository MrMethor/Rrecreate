//! `rrc files SESSION` (D78): the files the original reads and writes in a session, and for each read
//! or write where in the file, how much, where in memory the bytes went or came from, and the code that
//! made it. A structure in memory leads to the file it came from and the function that reads that
//! file; a save routine's writes show which memory each part of a save file is. The session runs once
//! with Clockwork's file tracing; what it found is kept by the run, so asking again, about one file or
//! all, runs nothing. An access to an archive the formats know is named by its member (D112), and a
//! member's name picks its accesses as a file's does.

use std::collections::BTreeMap;
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::analysis::Analysis;
use crate::args::Args;
use crate::cache::Cache;
use crate::clockwork::{self, Clockwork};
use crate::formats::{Formats, Kind, Member};
use crate::knowledge::Knowledge;
use crate::places::Places;
use crate::quirks::{When, code_text};
use crate::repo::Repo;
use crate::say;
use crate::session::Session;
use crate::watch::Names;

/// Lines shown for one file's accesses.
const SHOWN: usize = 40;

/// One access as Clockwork reported it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Access {
    pub(crate) cycle: u64,
    pub(crate) write: bool,
    pub(crate) file: String,
    pub(crate) offset: u32,
    pub(crate) len: u32,
    pub(crate) at: String,
    pub(crate) by: String,
    /// The functions it was made in, innermost first.
    #[serde(default)]
    pub(crate) chain: Vec<String>,
}

pub(crate) fn files(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--file", "--lines"], &[])?;
    let [name] = a.positionals(1, 1, "files SESSION [--file NAME] [--lines N]")? else { unreachable!() };
    let shown: usize = a.value("--lines").map_or(Ok(SHOWN), str::parse).map_err(|_| "--lines is a count")?;
    let session = Session::load(repo, name)?;
    let (accesses, ms_per_cycle) = traced(repo, &session)?;
    let k = Knowledge::load(&repo.knowledge())?;
    let names = Names::new(&k, repo, &session)?;
    let places = Places::new(repo, &session);
    let analysis = Analysis::load(repo)?;
    let code = |address: &str| code_text(&places, &k, analysis.as_ref(), address);
    let made = |x: &Access| made_by(x, &names, &code);
    let when = When::new(repo, &session);
    let archives = archives(repo, &k, &accesses);
    let member = |x: &Access| member_of(&archives, x);
    let time = |cycle: u64| {
        let ticks = when.text(cycle);
        let seconds = cycle as f64 * ms_per_cycle / 1000.0;
        if ticks.starts_with("at cycle") { format!("at {seconds:.1} s") } else { format!("{ticks} ({seconds:.1} s)") }
    };
    match a.value("--file") {
        Some(file) => {
            let file = file.to_uppercase();
            // A file, or an archive's member (`MEMBER` or `FILE/MEMBER`).
            let (in_file, in_member) = match file.split_once('/') {
                Some((f, m)) => (f.to_owned(), Some(m.to_owned())),
                None => (file.clone(), None),
            };
            let mut mine: Vec<&Access> = accesses
                .iter()
                .filter(|x| {
                    x.file.to_uppercase().ends_with(&in_file)
                        && in_member
                            .as_ref()
                            .is_none_or(|m| member(x).is_some_and(|(n, _)| n.name.to_uppercase() == *m))
                })
                .collect();
            if mine.is_empty() && in_member.is_none() {
                mine =
                    accesses.iter().filter(|x| member(x).is_some_and(|(n, _)| n.name.to_uppercase() == file)).collect();
            }
            if mine.is_empty() {
                return Err(format!("{name} reads or writes no file or archive member named {file}"));
            }
            let what = match member(mine[0])
                .filter(|_| in_member.is_some() || !mine[0].file.to_uppercase().ends_with(&file))
            {
                Some((m, _)) => format!("{}/{}", mine[0].file, m.name),
                None => mine[0].file.clone(),
            };
            say("file", format!("{what}: {} accesses", mine.len()));
            for x in mine.iter().take(shown) {
                let (verb, arrow) = if x.write { ("wrote", "from") } else { ("read", "to") };
                let named = match member(x) {
                    Some((m, 0)) => format!(" ({})", m.name),
                    Some((m, within)) => format!(" ({} +{within:X})", m.name),
                    None => String::new(),
                };
                say(
                    verb,
                    format!(
                        "+{:X}{named}, {} bytes, {arrow} {} by {}, {}",
                        x.offset,
                        x.len,
                        names.data(&x.at, x.len),
                        made(x),
                        time(x.cycle)
                    ),
                );
            }
            if mine.len() > shown {
                say("...", format!("{} more (--lines N)", mine.len() - shown));
            }
        }
        None => {
            // By file and direction, in the order first touched.
            let mut by_file: Vec<((String, bool), Vec<&Access>)> = Vec::new();
            for x in &accesses {
                let key = (x.file.clone(), x.write);
                match by_file.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, list)) => list.push(x),
                    None => by_file.push((key, vec![x])),
                }
            }
            let read = by_file.iter().filter(|((_, write), _)| !write).count();
            say("files", format!("{name}: {read} files read, {} written", by_file.len() - read));
            for ((file, write), list) in by_file.iter().take(shown) {
                let bytes: u64 = list.iter().map(|x| u64::from(x.len)).sum();
                let mut callers: BTreeMap<String, usize> = BTreeMap::new();
                for x in list {
                    *callers.entry(made(x)).or_default() += 1;
                }
                let mut callers: Vec<(String, usize)> = callers.into_iter().collect();
                callers.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
                let by: Vec<String> = callers.iter().take(2).map(|(made, _)| made.clone()).collect();
                let members: std::collections::BTreeSet<&str> =
                    list.iter().filter_map(|x| member(x)).map(|(m, _)| m.name.as_str()).collect();
                let members = match members.len() {
                    0 => String::new(),
                    n => format!(", {n} members (--file {file} names them)"),
                };
                say(
                    if *write { "wrote" } else { "read" },
                    format!(
                        "{file}: {} times, {bytes} bytes{members}, by {}{}, first {}",
                        list.len(),
                        by.join(", "),
                        if callers.len() > 2 { format!(" and {} more", callers.len() - 2) } else { String::new() },
                        time(list[0].cycle)
                    ),
                );
            }
            if by_file.len() > shown {
                say("...", format!("{} more files (--lines N)", by_file.len() - shown));
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The archives among the files the session touched, by their names as accessed: their members, as the
/// formats lay them out in the game's copy (none without the game, or for a file no format makes an
/// archive).
fn archives(repo: &Repo, k: &Knowledge, accesses: &[Access]) -> BTreeMap<String, Vec<Member>> {
    let (Ok(formats), Ok(game)) = (Formats::load(&repo.knowledge()), repo.game_dir()) else { return BTreeMap::new() };
    let mut archives = BTreeMap::new();
    for x in accesses {
        if archives.contains_key(&x.file) {
            continue;
        }
        let members = rrc_runtime::original::file_in(game, &x.file)
            .ok()
            .and_then(|bytes| {
                let f = formats.of(&x.file, bytes.len()).filter(|f| f.kind == Kind::Archive)?;
                formats.members(f, k, &bytes).ok()
            })
            .unwrap_or_default();
        archives.insert(x.file.clone(), members);
    }
    archives
}

/// The archive member an access starts in, and where in it.
fn member_of<'a>(archives: &'a BTreeMap<String, Vec<Member>>, x: &Access) -> Option<(&'a Member, usize)> {
    let at = x.offset as usize;
    let members = archives.get(&x.file)?;
    let m = members.iter().find(|m| m.offset <= at && at < m.offset + m.packed)?;
    Some((m, at - m.offset))
}

/// The code that made an access and the functions it was called from, as names where the knowledge has
/// them.
pub(crate) fn made_by(x: &Access, names: &Names, code: &dyn Fn(&str) -> String) -> String {
    // The program's own functions (not the system's call stub), each once, past the innermost.
    let mut callers: Vec<String> = Vec::new();
    for entry in x.chain.iter().filter(|entry| names.in_program(entry)).skip(1) {
        let name = names.function(entry);
        if callers.last() != Some(&name) {
            callers.push(name);
        }
    }
    callers.truncate(3);
    if callers.is_empty() { code(&x.by) } else { format!("{} < {}", code(&x.by), callers.join(" < ")) }
}

/// The session's file accesses, and emulated milliseconds per cycle: from the cache, or a run of the
/// session with file tracing (kept by the run).
pub(crate) fn traced(repo: &Repo, session: &Session) -> Result<(Vec<Access>, f64), String> {
    let cache = Cache::open(&repo.local)?;
    let key = crate::snapshots::run_key(repo, session, "files 3")?;
    if let Some(text) = cache.recall("results", &key)
        && let Ok(kept) = serde_json::from_str::<Value>(&text)
        && let Ok(accesses) = serde_json::from_value::<Vec<Access>>(kept["accesses"].clone())
    {
        return Ok((accesses, kept["ms_per_cycle"].as_f64().unwrap_or(0.0)));
    }
    let (mut clockwork, _) = Clockwork::session(repo, session)?;
    // Calls tracked from power-on, so each access has its whole chain.
    clockwork.ask("calls", json!({"on": true}))?;
    clockwork.ask("files", json!({}))?;
    let ran = clockwork.ask("run", json!({ "ms": crate::snapshots::to_run(repo, session)? }))?;
    let found = clockwork.ask("files_end", Value::Null)?;
    let accesses: Vec<Access> = serde_json::from_value(found["accesses"].clone()).map_err(|e| e.to_string())?;
    let (cycles, ms) = (clockwork::number(&ran, "cycles")?, clockwork::number(&ran, "ms")?);
    let ms_per_cycle = if cycles == 0 { 0.0 } else { ms as f64 / cycles as f64 };
    let kept = json!({ "accesses": accesses, "ms_per_cycle": ms_per_cycle });
    cache.remember("results", &key, &kept.to_string())?;
    Ok((accesses, ms_per_cycle))
}
