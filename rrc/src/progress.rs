//! `rrc progress` (DESIGN.md §7, D117): how far the port is, as the sessions show it. Code: the program's
//! functions the original ran in the sessions, those it ran in the passes the port matches (as `rrc compare`
//! last left each session: from its start or transplant to the last tick that matched), and of those, the
//! ones the port's code names (DESIGN.md §6.6). State: of the memory a recording keeps at every tick (D92,
//! the data segment), the bytes that change in the session's ticks, and how many of them the mapping maps
//! (with the presentation's own, D110), ignores, or neither; the largest left, by name.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use ids::Id;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::analysis::Analysis;
use crate::args::Args;
use crate::cache::Cache;
use crate::knowledge::Knowledge;
use crate::mapping::Mapping;
use crate::places::Places;
use crate::repo::Repo;
use crate::say;
use crate::session::Session;
use crate::trace::{begin, sync_points};
use crate::watch::Names;

const USAGE: &str = "progress [--lines N]";
/// What is left, listed at most.
const SHOWN: usize = 6;
/// Where `rrc compare` keeps what it found, in its work folder.
const OUTCOME: &str = "outcome.json";
/// Bytes that don't change, this many or fewer between two that do, join them in one place left.
const GAP: usize = 4;

/// What `rrc compare` last found for a session (D117): the recording it compared, the tick it started from
/// (0: the port's own start), the ticks it compared, the last that matched (state, screen and sound), the
/// fields it fed, and what differed after.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Outcome {
    pub recorded: String,
    pub from: u64,
    pub first: u64,
    pub last: u64,
    pub matched: u64,
    pub ticks: u64,
    pub fed: Vec<String>,
    pub differs: Option<String>,
}

impl Outcome {
    pub(crate) fn save(&self, work: &Path) -> Result<(), String> {
        let path = work.join(OUTCOME);
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn load(repo: &Repo, name: &str) -> Result<Option<Outcome>, String> {
        let path = repo.state()?.join("compare").join(name).join(OUTCOME);
        let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
        serde_json::from_str(&text).map(Some).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The passes that match: those to the ticks after its start, to the last that matched.
    fn matches(&self, (first, last): (u64, u64)) -> bool {
        last > self.from && first <= self.matched
    }
}

/// The functions a session's run called, by Clockwork's address: the passes they were called in (pass 1 is
/// to the first tick; a session of n ticks has n + 1, the last to its end) as runs from one to another, and
/// how often in all.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Called {
    passes: BTreeMap<String, Vec<(u64, u64)>>,
    counts: BTreeMap<String, u64>,
}

/// A function the sessions ran: its name, how often, whether in a pass that matches.
struct Ran {
    name: String,
    calls: u64,
    matched: bool,
}

/// A session's state: the bytes that change, those mapped, ignored and neither; the places left (where,
/// how many bytes change there), and the fields mapped through pointers.
struct State {
    changing: usize,
    mapped: usize,
    ignored: usize,
    left: Vec<(String, usize)>,
    through: usize,
}

pub(crate) fn progress(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--lines"], &[])?;
    a.positionals(0, 0, USAGE)?;
    let shown: usize = a.value("--lines").map_or(Ok(SHOWN), str::parse).map_err(|_| "--lines is a count")?;
    let k = Knowledge::load(&repo.knowledge())?;
    let module = repo.module();
    let analysis = Analysis::load(repo)?;
    let mut ran: BTreeMap<String, Ran> = BTreeMap::new();
    // Called code the analysis has no function for: a driver or overlay loaded at run time into the program's
    // memory, not the program's code.
    let mut loaded: BTreeMap<String, u64> = BTreeMap::new();
    let mut states = Vec::new();
    for name in crate::session::names(repo)? {
        let session = Session::load(repo, &name)?;
        let Some(recorded) = &session.trace else {
            say("session", format!("{name}: not recorded (rrc record {name})"));
            continue;
        };
        let outcome = Outcome::load(repo, &name)?.filter(|o| o.recorded == recorded.hash);
        match &outcome {
            None => say("session", format!("{name}: not compared since it was recorded (rrc compare {name})")),
            Some(o) => say("session", format!("{name}: {}", outcome_text(o))),
        }
        let called = called(repo, &session, &k)?;
        let names = Names::new(&k, repo, &session)?;
        for (target, passes) in &called.passes {
            let Some(id) = names.id(target).filter(|_| names.in_program(target)) else { continue };
            if analysis.as_ref().is_some_and(|a| !a.functions.contains_key(&id)) {
                *loaded.entry(id.to_string()).or_default() += called.counts.get(target).copied().unwrap_or(0);
                continue;
            }
            let matched = outcome.as_ref().is_some_and(|o| passes.iter().any(|&p| o.matches(p)));
            let r =
                ran.entry(id.to_string()).or_insert_with(|| Ran { name: names.function(target), calls: 0, matched });
            r.calls += called.counts.get(target).copied().unwrap_or(0);
            r.matched |= matched;
        }
        if let Some(state) = state(repo, &session, &k, &names, analysis.as_ref())? {
            states.push((name, state));
        }
    }
    if ran.is_empty() {
        say("code", "no session ran the program's functions: rrc session, then rrc record");
        return Ok(ExitCode::SUCCESS);
    }

    // The code: all the functions run, those in passes that match, and those of them the port names; by
    // their bytes where the analysis has them.
    let named = crate::sources::ids(&repo.root, &module);
    let size = |id: &str| {
        let id = crate::knowledge::parse_id(id).ok()?;
        analysis.as_ref()?.functions.get(&id).map(|f| u64::from(f.size))
    };
    let measure = |ids: &mut dyn Iterator<Item = &String>| {
        ids.fold((0u64, 0u64), |(n, bytes), id| (n + 1, bytes + size(id).unwrap_or(0)))
    };
    let all = measure(&mut ran.keys());
    let matching = measure(&mut ran.iter().filter(|(_, r)| r.matched).map(|(id, _)| id));
    let ported = measure(&mut ran.iter().filter(|(id, r)| r.matched && named.contains(*id)).map(|(id, _)| id));
    let share = |(n, bytes): (u64, u64)| match (bytes * 100).checked_div(all.1) {
        Some(percent) => format!("{n} ({}, {percent}%)", kb(bytes)),
        None => format!("{n} ({}%)", n * 100 / all.0.max(1)),
    };
    let total = if all.1 > 0 { format!(" ({})", kb(all.1)) } else { String::new() };
    say(
        "code",
        format!(
            "{module} ran {} functions{total} in the sessions; {} in passes that match, and the port names {} of those",
            all.0,
            share(matching),
            share(ported)
        ),
    );
    if !loaded.is_empty() {
        let calls: u64 = loaded.values().sum();
        say(
            "loaded",
            format!(
                "{} places called that the analysis has no function for ({calls} calls: code loaded at run time), left out",
                loaded.len()
            ),
        );
    }
    // What ran in passes that match but no code of the port names: inlined helpers, library code, or a part
    // fed or not ported; the most called first.
    let mut unnamed: Vec<(&String, &Ran)> = ran.iter().filter(|(id, r)| r.matched && !named.contains(*id)).collect();
    unnamed.sort_by_key(|(id, r)| (std::cmp::Reverse(r.calls), (*id).clone()));
    for (id, r) in unnamed.iter().take(shown) {
        let name = if r.name == **id { r.name.clone() } else { format!("{} ({id})", r.name) };
        say("unnamed", format!("{name}: {} calls in the sessions, in passes that match", r.calls));
    }
    if unnamed.len() > shown {
        say("...", format!("{} more the port doesn't name (--lines N)", unnamed.len() - shown));
    }
    let unmatched = ran.values().filter(|r| !r.matched).count();
    if unmatched > 0 {
        say("unmatched", format!("{unmatched} functions ran only in passes no comparison matches"));
    }

    // The state, session by session.
    for (name, s) in &states {
        let percent = |n: usize| n * 100 / s.changing.max(1);
        let neither = s.changing - s.mapped - s.ignored;
        say(
            "state",
            format!(
                "{name}: {} bytes kept at every tick change; mapped {} ({}%), ignored {} ({}%), neither {} ({}%)",
                s.changing,
                s.mapped,
                percent(s.mapped),
                s.ignored,
                percent(s.ignored),
                neither,
                percent(neither)
            ),
        );
        for (place, n) in s.left.iter().take(shown) {
            say("unmapped", format!("{place}: {n} change"));
        }
        if s.left.len() > shown {
            say("...", format!("{} more places (--lines N)", s.left.len() - shown));
        }
        if s.through > 0 {
            say("through", format!("{name}: {} fields mapped through pointers point outside what is kept", s.through));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// What a comparison found, in words.
fn outcome_text(o: &Outcome) -> String {
    let start =
        if o.from == 0 { "from the port's start".to_owned() } else { format!("from a transplant at tick {}", o.from) };
    let fed = if o.fed.is_empty() { "no feed".to_owned() } else { format!("fed {}", o.fed.join(", ")) };
    let to = if o.last < o.ticks { format!(" (compared to tick {})", o.last) } else { String::new() };
    match &o.differs {
        None => format!("ticks {}-{} of {} match, {start}, {fed}{to}", o.first, o.matched, o.ticks),
        Some(what) if o.matched < o.first => {
            format!("its {what} differs at tick {}, {start}, {fed}", o.matched + 1)
        }
        Some(what) => {
            format!("ticks {}-{} of {} match, then its {what} differs, {start}, {fed}", o.first, o.matched, o.ticks)
        }
    }
}

/// Bytes as kilobytes.
fn kb(bytes: u64) -> String {
    format!("{:.1} KB", bytes as f64 / 1024.0)
}

/// The functions the session's run calls, pass by pass: Clockwork's call tracker counted at each tick, kept
/// for the same run and sync points.
fn called(repo: &Repo, session: &Session, k: &Knowledge) -> Result<Called, String> {
    let syncs = sync_points(k)?;
    let key = crate::snapshots::run_key(repo, session, &format!("calls by pass 1 {}", json!(syncs)))?;
    let cache = Cache::open(&repo.local)?;
    if let Some(called) = cache.recall("progress", &key).and_then(|text| serde_json::from_str(&text).ok()) {
        return Ok(called);
    }
    let (mut clockwork, _, _) = begin(repo, session, &syncs)?;
    clockwork.ask("calls", json!({"on": true, "count": true}))?;
    let length_ms = (session.seconds * 1000.0) as u64;
    let mut before = BTreeMap::new();
    let mut called = Called::default();
    for pass in 1.. {
        let ran = if syncs.is_empty() {
            clockwork.ask("run", json!({ "until_ms": length_ms }))?
        } else {
            clockwork.ask("run", json!({"sync": "next", "until_ms": length_ms}))?
        };
        for (target, n) in crate::calls::since(&mut clockwork, &mut before)? {
            *called.counts.entry(target.clone()).or_default() += n;
            let runs = called.passes.entry(target).or_default();
            match runs.last_mut() {
                Some((_, last)) if *last + 1 == pass => *last = pass,
                _ => runs.push((pass, pass)),
            }
        }
        if ran["sync"]["at"].as_str().is_none() {
            break;
        }
    }
    let text = serde_json::to_string(&called).map_err(|e| e.to_string())?;
    cache.remember("progress", &key, &text)?;
    Ok(called)
}

/// The session's state as its recording kept it at every tick (D92), against the mapping; none if nothing
/// was kept.
fn state(
    repo: &Repo,
    session: &Session,
    k: &Knowledge,
    names: &Names,
    analysis: Option<&Analysis>,
) -> Result<Option<State>, String> {
    let Some(recorded) = &session.trace else { return Ok(None) };
    let Some(memory) = &recorded.memory else { return Ok(None) };
    let syncs = sync_points(k)?;
    let key = crate::memory::key(repo, session, &syncs, "data")?;
    let span = crate::memory::kept_by(repo, &key, memory)?.filter(|s| s.first == 1 && recorded.ticks <= s.last);
    let Some(span) = span else { return Ok(None) };
    let changing = crate::memory::changing(repo, &span)?;
    let places = Places::new(repo, session);
    let mut bases = Vec::new();
    for (at, _) in &span.ranges {
        bases.push(places.linear(at)?);
    }
    // What covers each byte kept: 1 ignored, 2 mapped.
    let mut covered: Vec<Vec<u8>> = changing.iter().map(|c| vec![0; c.len()]).collect();
    let mut cover = |linear: u32, size: u32, how: u8| {
        for (r, &base) in bases.iter().enumerate() {
            let len = covered[r].len() as u32;
            let (from, to) = (linear.max(base), linear.saturating_add(size).min(base + len));
            if from < to {
                for c in &mut covered[r][(from - base) as usize..(to - base) as usize] {
                    *c = (*c).max(how);
                }
            }
        }
    };
    let linear = |id: &Id| places.linear(&places.address(&id.to_string())?);
    let mapping = Mapping::load(repo)?;
    // A field through a pointer covers each place the pointer pointed to at a tick (D72), as the trace has it;
    // those outside what is kept are counted apart.
    let trace = crate::trace::Trace::parse(&String::from_utf8_lossy(&Cache::open(&repo.local)?.get(&recorded.hash)?))?;
    let mut through = 0;
    for m in mapping.resolve(k)?.iter().chain(&mapping.resolve_present(k)?) {
        let Some(via) = &m.via else {
            cover(linear(&m.id)?, m.size, 2);
            continue;
        };
        // Through more pointers, or a list (D122): not placed here.
        if !via.then.is_empty() || via.next.is_some() {
            through += 1;
            continue;
        }
        let at = m.id.to_string();
        let pointer =
            trace.header.state.iter().position(|i| i.via.is_none() && i.at == at && i.size == via.pointer.size());
        let mut targets: Vec<u32> = Vec::new();
        for hex in pointer.map(|i| trace.values(i)).unwrap_or_default().into_iter().flatten() {
            let target = match via.pointer.target(&crate::trace::unhex(&hex), via.offset) {
                Some(crate::values::Target::Id(id)) => linear(&id)?,
                Some(crate::values::Target::Linear(at)) => at,
                None => continue,
            };
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        let kept = |at: u32| bases.iter().zip(&changing).any(|(&base, c)| at >= base && at < base + c.len() as u32);
        if targets.is_empty() || !targets.iter().all(|&at| kept(at)) {
            through += 1;
        }
        for at in targets {
            cover(at, m.size, 2);
        }
    }
    for (id, size) in mapping.ignored(k)? {
        cover(linear(&id)?, size, 1);
    }
    let (mut state, mut left) = (State { changing: 0, mapped: 0, ignored: 0, left: Vec::new(), through }, Vec::new());
    for (r, changing) in changing.iter().enumerate() {
        // The places left: changing bytes nothing covers, those close together as one.
        let mut place: Option<(usize, usize, usize)> = None;
        for (i, &changes) in changing.iter().enumerate() {
            let how = covered[r][i];
            if changes {
                state.changing += 1;
                match how {
                    2 => state.mapped += 1,
                    1 => state.ignored += 1,
                    _ => {}
                }
            }
            match (&mut place, changes && how == 0) {
                (Some((_, last, n)), true) => {
                    *last = i;
                    *n += 1;
                }
                (None, true) => place = Some((i, i, 1)),
                (Some((_, last, _)), false) if how != 0 || i - *last > GAP => {
                    left.push((r, place.take().expect("a place")));
                }
                _ => {}
            }
        }
        if let Some(p) = place {
            left.push((r, p));
        }
    }
    left.sort_by_key(|(r, (from, _, n))| (std::cmp::Reverse(*n), *r, *from));
    for (r, (from, last, n)) in left {
        let linear = bases[r] + from as u32;
        let id = places.linear_id(linear)?;
        let address = places.address(&id.to_string())?;
        let len = (last - from + 1) as u32;
        let named = names.data(&address, len);
        // Where it is as the data segment has it (D90), and its name if it has one.
        let ds = analysis.and_then(|a| a.ds_offset(&id)).map(|at| format!("DS:{at:04X}"));
        let bytes = if len == 1 { "1 byte".to_owned() } else { format!("{len} bytes") };
        let place = match (ds, named == id.to_string()) {
            (Some(ds), true) => format!("{ds} ({id}), {bytes}"),
            (Some(ds), false) => format!("{ds} {named} ({id}), {bytes}"),
            (None, true) => format!("{id}, {bytes}"),
            (None, false) => format!("{named} ({id}), {bytes}"),
        };
        state.left.push((place, n));
    }
    Ok(Some(state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_passes_that_match_are_after_the_start_to_the_last_that_matched() {
        let o = Outcome {
            recorded: String::new(),
            from: 1,
            first: 1,
            last: 650,
            matched: 279,
            ticks: 650,
            fed: Vec::new(),
            differs: Some("state".into()),
        };
        assert!(!o.matches((1, 1)), "the pass to the transplant's tick is the original's");
        assert!(o.matches((1, 2)) && o.matches((279, 300)));
        assert!(!o.matches((280, 651)), "the pass to the tick that differs");
        assert_eq!(
            outcome_text(&o),
            "ticks 1-279 of 650 match, then its state differs, from a transplant at tick 1, no feed"
        );
        let all = Outcome { matched: 650, differs: None, from: 0, fed: vec!["level.flats".into()], ..o };
        assert!(all.matches((1, 1)), "from the port's start, its first pass is its own");
        assert_eq!(outcome_text(&all), "ticks 1-650 of 650 match, from the port's start, fed level.flats");
    }
}
