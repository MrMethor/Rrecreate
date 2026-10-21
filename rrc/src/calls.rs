//! `rrc calls SESSION`: how often the session calls each function, grouped by count. Functions that
//! one loop calls once a pass are called equally often, so a large group is a loop, and a function
//! in it that waits for the timer is where a tick ends: a sync point (DESIGN.md §10, step 3).
//!
//! The counts are split into phases: before the first tick, then each run of ticks at one sync point
//! (a loop: the menus, play), to the next phase's first tick or the session's end. With a function,
//! it says how often each phase called it.
//!
//! Clockwork's call tracker counts the calls, in real and protected mode alike; those to code outside
//! the program (an extender's, drivers') are counted, not listed.

use std::collections::BTreeMap;
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::analysis::Analysis;
use crate::args::Args;
use crate::clockwork::Clockwork;
use crate::knowledge::Knowledge;
use crate::repo::Repo;
use crate::say;
use crate::session::Session;
use crate::trace::{Places, begin, sync_points};
use crate::watch::Names;

/// A stretch of the session and the calls made in it, by target (Clockwork's `+OFF` or `@LINEAR`).
struct Phase {
    label: String,
    counts: BTreeMap<String, u64>,
}

pub(crate) fn calls(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--min", "--top"], &[])?;
    let positionals = a.positionals(1, 2, "calls SESSION [ID|NAME] [--min N] [--top N]")?;
    let session = Session::load(repo, &positionals[0])?;
    let key = crate::snapshots::result_key(repo, &session, &[&["calls".to_owned()], args].concat())?;
    crate::snapshots::kept(repo, &key, || count_calls(repo, &a, positionals, &session))
}

fn count_calls(repo: &Repo, a: &Args, positionals: &[String], session: &Session) -> Result<ExitCode, String> {
    let top: usize = a.value("--top").map_or(Ok(5), str::parse).map_err(|_| "--top is a count")?;
    let min: u64 = a.value("--min").map_or(Ok(100), str::parse).map_err(|_| "--min is a count")?;
    let name = &positionals[0];
    let k = Knowledge::load(&repo.knowledge())?;
    let names = Names::new(&k, repo, session)?;
    let function_name = |target: &str| names.function(target);
    let mut phases = phases(repo, session, &k)?;
    if let Some(what) = positionals.get(1) {
        let analysis = Analysis::load(repo)?;
        let id = crate::naming::resolve(repo, &k, analysis.as_ref(), what)?;
        let target = Places::new(repo, session).address(&id.to_string())?;
        let total: u64 = phases.iter().map(|p| p.counts.get(&target).copied().unwrap_or(0)).sum();
        let named = function_name(&target);
        let named = if named == id.to_string() { named } else { format!("{named} ({id})") };
        say("calls", format!("{named}: {total} in {name}"));
        for phase in &phases {
            say("phase", format!("{}: {}", phase.label, phase.counts.get(&target).copied().unwrap_or(0)));
        }
        return Ok(ExitCode::SUCCESS);
    }
    // The program's functions; an extender's and drivers' are only counted.
    let called: std::collections::BTreeSet<String> = phases.iter().flat_map(|p| p.counts.keys().cloned()).collect();
    let outside = called.iter().filter(|target| !names.in_program(target)).count();
    for phase in &mut phases {
        phase.counts.retain(|target, _| names.in_program(target));
    }
    let elsewhere =
        if outside > 0 { format!("; {outside} outside it (an extender's, drivers')") } else { String::new() };
    let (functions, module, n) = (called.len() - outside, repo.module(), phases.len());
    say("called", format!("{functions} functions of {module} in {name}, in {n} phases{elsewhere}"));
    for phase in &phases {
        say("phase", format!("{}: {} functions", phase.label, phase.counts.len()));
        let mut groups: BTreeMap<u64, Vec<&String>> = BTreeMap::new();
        for (target, &n) in &phase.counts {
            groups.entry(n).or_default().push(target);
        }
        let mut groups: Vec<(u64, Vec<&String>)> = groups.into_iter().filter(|(n, _)| *n >= min).collect();
        groups.sort_by_key(|(n, fs)| (std::cmp::Reverse(fs.len()), std::cmp::Reverse(*n)));
        for (n, functions) in groups.iter().take(top) {
            let names: Vec<String> = functions.iter().map(|t| function_name(t)).collect();
            say("  group", format!("{n}x, {} functions: {}", functions.len(), names.join(", ")));
        }
        if groups.len() > top {
            say("  ...", format!("{} more groups (--top N shows more, --min N fewer)", groups.len() - top));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The session's calls, phase by phase.
fn phases(repo: &Repo, session: &Session, k: &Knowledge) -> Result<Vec<Phase>, String> {
    let syncs = sync_points(k)?;
    let (mut clockwork, _, names) = begin(repo, session, &syncs)?;
    // The call tracker counts calls in 32-bit code too (coverage stays in real mode).
    clockwork.ask("calls", json!({"on": true, "count": true}))?;
    let length_ms = (session.seconds * 1000.0) as u64;
    let mut phases = Vec::new();
    let mut before = BTreeMap::new();
    let span = |from: u64, to: u64| format!("{:.1}-{:.1} s", from as f64 / 1000.0, to as f64 / 1000.0);
    // The phase running: its sync point, first and last tick, and when it began (ms).
    let mut current: Option<(String, u64, u64, u64)> = None;
    let mut tick = 0;
    loop {
        let ran = if syncs.is_empty() {
            clockwork.ask("run", json!({ "until_ms": length_ms }))?
        } else {
            clockwork.ask("run", json!({"sync": "next", "until_ms": length_ms}))?
        };
        let ms = ran["ms"].as_u64().unwrap_or(length_ms);
        let Some(at) = ran["sync"]["at"].as_str() else {
            let label = match current {
                None if syncs.is_empty() => "the whole session".to_owned(),
                None => "the whole session (no tick)".to_owned(),
                Some((sync, first, last, from)) => {
                    format!("{sync}, ticks {first}-{last} to the end ({})", span(from, ms))
                }
            };
            phases.push(Phase { label, counts: since(&mut clockwork, &mut before)? });
            return Ok(phases);
        };
        tick += 1;
        let sync = names.get(at).cloned().unwrap_or_else(|| at.to_owned());
        match &mut current {
            Some((s, _, last, _)) if *s == sync => *last = tick,
            _ => {
                let label = match current.take() {
                    None => format!("before tick 1 ({})", span(0, ms)),
                    Some((s, first, last, from)) => {
                        format!("{s}, ticks {first}-{last} to tick {tick} ({})", span(from, ms))
                    }
                };
                phases.push(Phase { label, counts: since(&mut clockwork, &mut before)? });
                current = Some((sync, tick, tick, ms));
            }
        }
    }
}

/// The calls made since `before` (the counts then), which become the counts now.
fn since(clockwork: &mut Clockwork, before: &mut BTreeMap<String, u64>) -> Result<BTreeMap<String, u64>, String> {
    let answer = clockwork.ask("call_counts", Value::Null)?;
    let now: BTreeMap<String, u64> = answer["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| Some((c[0].as_str()?.to_owned(), c[1].as_u64()?)))
        .collect();
    let made = now
        .iter()
        .map(|(target, &n)| (target.clone(), n - before.get(target).copied().unwrap_or(0)))
        .filter(|(_, n)| *n > 0)
        .collect();
    *before = now;
    Ok(made)
}
