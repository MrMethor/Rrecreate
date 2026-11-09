//! What the port's code names that no session runs (D151). A comparison only shows what a session reaches, so the port's
//! code for the rest has no check but a reading against the original's. `rrc suspects` keeps what each session's run
//! executed (it runs them anyway); this takes the places in the original's code that the port's code names (`sources`)
//! and lists those no run executed, by function: each wants a reading against the original's code, or a session.

use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;

use serde_json::Value;

use crate::analysis::Analysis;
use crate::args::Args;
use crate::cache::Cache;
use crate::knowledge::{Knowledge, parse_id};
use crate::places::Places;
use crate::repo::Repo;
use crate::say;
use crate::session::Session;

/// The functions shown; the whole list goes to a file.
const SHOWN: usize = 20;

/// Keeps what a session's run executed (`coverage_end`'s answer), by what makes the run the same.
pub(crate) fn keep(repo: &Repo, session: &Session, places: &Places, coverage: &Value) -> Result<(), String> {
    let module = repo.module();
    let ran: BTreeSet<String> = coverage["executed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|address| places.id(address))
        .filter(|id| id.module() == module)
        .map(|id| id.to_string())
        .collect();
    let text: Vec<String> = ran.into_iter().collect();
    let run = crate::snapshots::run_identity(repo, session)?;
    Cache::open(&repo.local)?.remember("coverage", &run, &text.join("\n"))
}

/// `rrc unrun`.
pub(crate) fn unrun(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--lines"], &[])?;
    a.positionals(0, 0, "unrun [--lines N]")?;
    let shown: usize = a.value("--lines").map_or(Ok(SHOWN), str::parse).map_err(|_| "--lines is a count")?;
    let cache = Cache::open(&repo.local)?;
    let (mut ran, mut sessions, mut missing) = (BTreeSet::new(), 0, Vec::new());
    for name in crate::session::names(repo)? {
        let run = crate::snapshots::run_identity(repo, &Session::load(repo, &name)?)?;
        match cache.recall("coverage", &run) {
            Some(text) => {
                ran.extend(text.lines().map(str::to_owned));
                sessions += 1;
            }
            None => missing.push(name),
        }
    }
    let analysis = Analysis::load(repo)?;
    let k = Knowledge::load(&repo.knowledge())?;
    // The places in the original's code that the port's code names, by the function each is in; what isn't in a
    // function is data, which nothing executes. With no analysis, each counts.
    let (mut named, mut left) = (0, 0);
    let mut by_function: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for text in crate::sources::ids(&repo.root, &repo.module()) {
        let id = parse_id(&text)?;
        let function = match &analysis {
            Some(analysis) => match analysis.containing(&id) {
                Some((entry, _)) => k.functions.get(entry).map_or_else(|| entry.to_string(), |d| d.name.clone()),
                None => continue,
            },
            None => String::new(),
        };
        named += 1;
        if !ran.contains(&text) {
            left += 1;
            by_function.entry(function).or_default().push(text);
        }
    }
    say("sessions", format!("{sessions} looked through"));
    if !missing.is_empty() {
        say(
            "missing",
            format!("{} not looked through, so not counted: rrc suspects {} first", missing.len(), missing[0]),
        );
    }
    say(
        "unrun",
        format!(
            "{left} of {named} places in the original's code that the port's code names: no session ran them, in {} functions",
            by_function.len()
        ),
    );
    let mut most: Vec<(&String, &Vec<String>)> = by_function.iter().collect();
    most.sort_by_key(|(_, places)| std::cmp::Reverse(places.len()));
    for (function, places) in most.iter().take(shown) {
        say("function", format!("{function}: {}, first {}", places.len(), places[0]));
    }
    // The whole list, each with the port's code that names it.
    let mut listing = String::new();
    for (function, places) in &by_function {
        for place in places {
            let short = place.split_once('+').map(|(_, offset)| format!("+{offset}"));
            let sites = crate::sources::naming(&repo.root, place, short.as_deref(), 3);
            let sites: Vec<String> = sites.iter().map(|(file, line)| format!("{file}:{line}")).collect();
            listing.push_str(&format!("{place}\t{function}\t{}\n", sites.join(" ")));
        }
    }
    let file = repo.state()?.join("unrun.tsv");
    std::fs::write(&file, listing).map_err(|e| format!("{}: {e}", file.display()))?;
    say("list", crate::path_text(&file));
    Ok(ExitCode::SUCCESS)
}
