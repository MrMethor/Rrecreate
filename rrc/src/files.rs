//! `rrc files SESSION` (D78): the files the original reads and writes in a session, and for each read
//! or write where in the file, how much, where in memory the bytes went or came from, and the code that
//! made it. A structure in memory leads to the file it came from and the function that reads that
//! file; a save routine's writes show which memory each part of a save file is. The session runs once
//! with Clockwork's file tracing; what it found is kept by the run, so asking again, about one file or
//! all, runs nothing.

use std::collections::BTreeMap;
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::analysis::Analysis;
use crate::args::Args;
use crate::cache::Cache;
use crate::clockwork::{self, Clockwork};
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
struct Access {
    cycle: u64,
    write: bool,
    file: String,
    offset: u32,
    len: u32,
    at: String,
    by: String,
    /// The functions it was made in, innermost first.
    #[serde(default)]
    chain: Vec<String>,
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
    // The code and the functions it was called from, as names where the knowledge has them.
    let made = |x: &Access| {
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
    };
    let when = When::new(repo, &session);
    let time = |cycle: u64| {
        let ticks = when.text(cycle);
        let seconds = cycle as f64 * ms_per_cycle / 1000.0;
        if ticks.starts_with("at cycle") { format!("at {seconds:.1} s") } else { format!("{ticks} ({seconds:.1} s)") }
    };
    match a.value("--file") {
        Some(file) => {
            let file = file.to_uppercase();
            let mine: Vec<&Access> = accesses.iter().filter(|x| x.file.to_uppercase().ends_with(&file)).collect();
            if mine.is_empty() {
                return Err(format!("{name} reads or writes no file named {file}"));
            }
            say("file", format!("{}: {} accesses", mine[0].file, mine.len()));
            for x in mine.iter().take(shown) {
                let (verb, arrow) = if x.write { ("wrote", "from") } else { ("read", "to") };
                say(
                    verb,
                    format!(
                        "+{:X}, {} bytes, {arrow} {} by {}, {}",
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
                say(
                    if *write { "wrote" } else { "read" },
                    format!(
                        "{file}: {} times, {bytes} bytes, by {}{}, first {}",
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

/// The session's file accesses, and emulated milliseconds per cycle: from the cache, or a run of the
/// session with file tracing (kept by the run).
fn traced(repo: &Repo, session: &Session) -> Result<(Vec<Access>, f64), String> {
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
    let ran = clockwork.ask("run", json!({ "ms": (session.seconds * 1000.0) as u64 }))?;
    let found = clockwork.ask("files_end", Value::Null)?;
    let accesses: Vec<Access> = serde_json::from_value(found["accesses"].clone()).map_err(|e| e.to_string())?;
    let (cycles, ms) = (clockwork::number(&ran, "cycles")?, clockwork::number(&ran, "ms")?);
    let ms_per_cycle = if cycles == 0 { 0.0 } else { ms as f64 / cycles as f64 };
    let kept = json!({ "accesses": accesses, "ms_per_cycle": ms_per_cycle });
    cache.remember("results", &key, &kept.to_string())?;
    Ok((accesses, ms_per_cycle))
}
