//! Traces (DESIGN.md §9, D45): a session run through Clockwork, read at every tick. A tick ends when
//! the original reaches a sync point; there the trace takes the boundary values, the named data
//! that changed since the last tick, and the frames and sound events of the tick.
//!
//! The file is JSON lines (`docs/formats.md`): a header naming the items, a line per tick, and a
//! last line saying how the run ended. Values are the original's bytes in hex, as memory holds them.

use std::collections::BTreeMap;
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::args::Args;
use crate::cache::Cache;
use crate::clockwork::{self, Clockwork};
use crate::knowledge::{Knowledge, parse_id};
use crate::repo::Repo;
use crate::say;
use crate::session::{Recorded, Session};

pub const FORMAT: &str = "rrc-trace";
pub const VERSION: u32 = 1;

/// A memory item a trace follows.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Item {
    pub name: String,
    /// Its ID.
    pub at: String,
    pub size: u32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SyncPoint {
    pub name: String,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Header {
    pub format: String,
    pub version: u32,
    pub game: String,
    pub session: String,
    pub program: String,
    pub clockwork: String,
    pub syncs: Vec<SyncPoint>,
    /// Boundary values: every tick has all of them.
    pub inputs: Vec<Item>,
    /// Named data: a tick has those that changed since the tick before (the first, all).
    pub state: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tick {
    /// From 1.
    pub tick: u64,
    /// The sync point that ended it.
    pub sync: String,
    pub cycle: u64,
    /// Each input's value (null where memory isn't mapped).
    pub inputs: Vec<Option<String>>,
    /// (index in the header's `state`, value) for each item that changed.
    pub state: Vec<(usize, Option<String>)>,
    /// The frames shown during the tick: [number, cycle, video mode, hash].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<Value>,
    /// Sound events during the tick, by device (only those with events).
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub sound: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct End {
    /// Why the run stopped.
    pub end: String,
    pub cycles: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Trace {
    pub header: Header,
    pub ticks: Vec<Tick>,
    pub end: End,
}

impl Trace {
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        let mut line = |value: Value| {
            text.push_str(&value.to_string());
            text.push('\n');
        };
        line(json!(self.header));
        for tick in &self.ticks {
            line(json!(tick));
        }
        line(json!(self.end));
        text
    }

    pub fn parse(text: &str) -> Result<Trace, String> {
        let mut lines = text.lines().filter(|l| !l.is_empty());
        let header: Header = serde_json::from_str(lines.next().ok_or("an empty trace")?)
            .map_err(|e| format!("the trace's header: {e}"))?;
        if header.format != FORMAT || header.version != VERSION {
            return Err(format!("not a trace this rrc reads ({} {})", header.format, header.version));
        }
        let rest: Vec<&str> = lines.collect();
        let (last, ticks) = rest.split_last().ok_or("the trace has no end")?;
        let ticks = ticks
            .iter()
            .map(|l| serde_json::from_str(l).map_err(|e| format!("a tick: {e}")))
            .collect::<Result<_, _>>()?;
        let end = serde_json::from_str(last).map_err(|e| format!("the trace's end: {e}"))?;
        Ok(Trace { header, ticks, end })
    }

    /// The value of state item `index` at each tick, from the changes.
    pub fn values(&self, index: usize) -> Vec<Option<String>> {
        let mut value = None;
        self.ticks
            .iter()
            .map(|tick| {
                if let Some((_, v)) = tick.state.iter().find(|(i, _)| *i == index) {
                    value = v.clone();
                }
                value.clone()
            })
            .collect()
    }
}

/// `+OFF` for an ID in the program (Clockwork's address for an image offset).
pub(crate) fn image_address(module: &str, id: &str) -> Result<String, String> {
    let id = parse_id(id)?;
    if id.module() != module || id.part().is_some() {
        return Err(format!("{id}: only the program's own image ({module}) can be traced so far"));
    }
    Ok(format!("+{:05X}", id.offset()))
}

pub(crate) fn record(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &[], &[])?;
    let [name] = a.positionals(1, 1, "record SESSION")? else { unreachable!() };
    let mut session = Session::load(repo, name)?;
    let k = Knowledge::load(&repo.knowledge())?;
    if k.ticks.syncs.is_empty() {
        return Err("no sync points in knowledge/ticks.toml: find where a tick ends first (docs/formats.md)".into());
    }
    let module = repo.module();
    let inputs: Vec<Item> =
        k.ticks.inputs.iter().map(|i| Item { name: i.name.clone(), at: i.at.clone(), size: i.size }).collect();
    let state: Vec<Item> = k
        .data
        .iter()
        .filter(|(id, d)| id.module() == module && id.part().is_none() && d.size.is_some())
        .map(|(id, d)| Item { name: d.decision.name.clone(), at: id.to_string(), size: d.size.unwrap_or(0) })
        .collect();
    let exe = clockwork::find(&repo.local)?;
    let header = Header {
        format: FORMAT.into(),
        version: VERSION,
        game: repo.settings.game.name.clone(),
        session: name.clone(),
        program: module.clone(),
        clockwork: clockwork::version(&exe).unwrap_or_default(),
        syncs: sync_points(&k),
        inputs,
        state,
    };
    let trace = run(repo, &session, header, &module)?;
    let cache = Cache::open(&repo.local)?;
    let text = trace.to_text();
    let hash = cache.put(text.as_bytes())?;
    let ticks = trace.ticks.len() as u64;
    let before =
        session.trace.replace(Recorded { hash: hash.clone(), ticks, clockwork: trace.header.clockwork.clone() });
    session.save(repo, name)?;
    say("trace", format!("{ticks} ticks, {} KB: {}", text.len() / 1024, trace.end.end));
    match before {
        Some(b) if b.hash == hash => say("same", "as the trace recorded before"),
        Some(b) => say("differs", format!("from the trace recorded before ({}, {} ticks)", b.hash, b.ticks)),
        None => {}
    }
    say("hash", hash);
    Ok(ExitCode::SUCCESS)
}

/// Runs the session to its end, taking a tick at each sync point hit.
/// The session's original in a fresh Clockwork with the sync points set: Clockwork, what `load`
/// answered (the cycle it starts at, where the image is), and the sync points' names by Clockwork's
/// address (`+OFF`).
pub(crate) fn begin(
    repo: &Repo,
    session: &Session,
    syncs: &[SyncPoint],
) -> Result<(Clockwork, Value, BTreeMap<String, String>), String> {
    let (mut clockwork, loaded) = Clockwork::original(repo, &session.options(repo))?;
    let mut names = BTreeMap::new();
    for s in syncs {
        let at = image_address(&repo.module(), &s.at)?;
        clockwork.ask("sync", json!({ "at": at }))?;
        names.insert(at, s.name.clone());
    }
    Ok((clockwork, loaded, names))
}

/// The knowledge's sync points as a trace names them.
pub(crate) fn sync_points(k: &Knowledge) -> Vec<SyncPoint> {
    k.ticks.syncs.iter().map(|s| SyncPoint { name: s.name.clone(), at: s.at.clone() }).collect()
}

fn run(repo: &Repo, session: &Session, header: Header, module: &str) -> Result<Trace, String> {
    let (mut clockwork, loaded, syncs) = begin(repo, session, &header.syncs)?;
    let start = loaded["cycles"].as_u64().unwrap_or(0);
    let mut ranges = Vec::new();
    for item in header.inputs.iter().chain(&header.state) {
        ranges.push(json!({"at": image_address(module, &item.at)?, "len": item.size}));
    }
    let ranges = json!({ "ranges": ranges });
    let length_ms = (session.seconds * 1000.0) as u64;
    let (mut ms, mut last_cycle) = (0, start);
    let mut previous: Vec<Option<String>> = vec![None; header.state.len()];
    let mut ticks = Vec::new();
    let end = loop {
        if ms >= length_ms {
            break End { end: "the session's end".into(), cycles: last_cycle };
        }
        let request = json!({"from": last_cycle, "ranges": ranges["ranges"], "max_ms": length_ms - ms});
        let ran = clockwork.ask("tick", request)?;
        ms = ran["ms"].as_u64().unwrap_or(length_ms);
        let cycle = ran["cycles"].as_u64().unwrap_or(0);
        let Some(at) = ran["sync"]["at"].as_str() else {
            let stop = ran["stop"].as_str().unwrap_or("?");
            let stop = if stop == "time limit reached" { "the session's end" } else { stop };
            break End { end: stop.into(), cycles: cycle };
        };
        let values: Vec<Option<String>> =
            ran["data"].as_array().into_iter().flatten().map(|v| v.as_str().map(str::to_owned)).collect();
        let (inputs, state) = values.split_at(header.inputs.len().min(values.len()));
        let changed = state
            .iter()
            .enumerate()
            .filter(|(i, v)| ticks.is_empty() || previous[*i] != **v)
            .map(|(i, v)| (i, v.clone()))
            .collect();
        previous = state.to_vec();
        let frames = ran["frames"].as_array().cloned().unwrap_or_default();
        let mut sound = ran["sound"].as_object().cloned().unwrap_or_default();
        sound.retain(|_, v| v.as_array().is_some_and(|a| !a.is_empty()));
        ticks.push(Tick {
            tick: ticks.len() as u64 + 1,
            sync: syncs.get(at).cloned().unwrap_or_else(|| at.to_owned()),
            cycle,
            inputs: inputs.to_vec(),
            state: changed,
            frames,
            sound,
        });
        last_cycle = cycle;
    };
    Ok(Trace { header, ticks, end })
}

pub(crate) fn show(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--tick", "--item"], &[])?;
    let [name] = a.positionals(1, 1, "trace SESSION [--tick N] [--item NAME]")? else { unreachable!() };
    let session = Session::load(repo, name)?;
    let recorded = session.trace.ok_or_else(|| format!("{name} isn't recorded: rrc record {name}"))?;
    let bytes = Cache::open(&repo.local)?.get(&recorded.hash)?;
    let trace = Trace::parse(&String::from_utf8_lossy(&bytes))?;
    let h = &trace.header;
    if let Some(item) = a.value("--item") {
        return show_item(&trace, item);
    }
    if let Some(n) = a.value("--tick") {
        let n: u64 = n.parse().map_err(|_| "--tick is a number")?;
        let tick = trace
            .ticks
            .iter()
            .find(|t| t.tick == n)
            .ok_or_else(|| format!("no tick {n} (1 to {})", trace.ticks.len()))?;
        say("tick", format!("{} at cycle {}, sync point {}", tick.tick, tick.cycle, tick.sync));
        for (item, value) in h.inputs.iter().zip(&tick.inputs) {
            say("input", format!("{} = {}", item.name, short(value.as_deref().unwrap_or("unmapped"))));
        }
        for (i, value) in &tick.state {
            say("changed", format!("{} = {}", h.state[*i].name, short(value.as_deref().unwrap_or("unmapped"))));
        }
        say("frames", tick.frames.len());
        for (device, events) in &tick.sound {
            say("sound", format!("{device}: {} events", events.as_array().map_or(0, Vec::len)));
        }
        return Ok(ExitCode::SUCCESS);
    }
    say(
        "trace",
        format!("{} ticks of {}, then {} (cycle {})", trace.ticks.len(), h.session, trace.end.end, trace.end.cycles),
    );
    let mut by_sync: BTreeMap<&str, usize> = BTreeMap::new();
    for tick in &trace.ticks {
        *by_sync.entry(&tick.sync).or_default() += 1;
    }
    let syncs: Vec<String> = by_sync.iter().map(|(s, n)| format!("{s} {n}")).collect();
    say("syncs", syncs.join(", "));
    let mut changes = vec![0usize; h.state.len()];
    for tick in trace.ticks.iter().skip(1) {
        for (i, _) in &tick.state {
            changes[*i] += 1;
        }
    }
    let mut most: Vec<(usize, &str)> = changes.iter().zip(&h.state).map(|(n, item)| (*n, item.name.as_str())).collect();
    most.sort_by_key(|m| std::cmp::Reverse(m.0));
    let most: Vec<String> = most.iter().take(8).filter(|m| m.0 > 0).map(|(n, name)| format!("{name} {n}")).collect();
    say("inputs", h.inputs.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", "));
    say("state", format!("{} items; changed most: {}", h.state.len(), most.join(", ")));
    say("frames", trace.ticks.iter().map(|t| t.frames.len()).sum::<usize>());
    Ok(ExitCode::SUCCESS)
}

/// An item's value over the ticks: each tick it changed (an input: each tick).
fn show_item(trace: &Trace, name: &str) -> Result<ExitCode, String> {
    let h = &trace.header;
    let values: Vec<Option<String>> = if let Some(i) = h.inputs.iter().position(|i| i.name == name) {
        trace.ticks.iter().map(|t| t.inputs.get(i).cloned().flatten()).collect()
    } else if let Some(i) = h.state.iter().position(|i| i.name == name) {
        trace.values(i)
    } else {
        return Err(format!("the trace has no {name}"));
    };
    let mut shown = 0;
    let mut last: Option<&Option<String>> = None;
    for (tick, value) in trace.ticks.iter().zip(&values) {
        if last == Some(value) {
            continue;
        }
        last = Some(value);
        shown += 1;
        if shown <= 40 {
            say(&tick.tick.to_string(), short(value.as_deref().unwrap_or("unmapped")));
        }
    }
    if shown > 40 {
        say("...", format!("{} more changes", shown - 40));
    }
    Ok(ExitCode::SUCCESS)
}

/// A value, shortened for a line of output.
fn short(hex: &str) -> String {
    if hex.len() <= 64 { hex.to_owned() } else { format!("{}… ({} bytes)", &hex[..64], hex.len() / 2) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trace_reads_back_as_written() {
        let item = |name: &str, at: &str| Item { name: name.into(), at: at.into(), size: 2 };
        let header = Header {
            format: FORMAT.into(),
            version: VERSION,
            game: "game".into(),
            session: "walk".into(),
            program: "GAME.EXE".into(),
            clockwork: "0.1.0".into(),
            syncs: vec![SyncPoint { name: "play".into(), at: "GAME.EXE+00100".into() }],
            inputs: vec![item("elapsed", "GAME.EXE+20000")],
            state: vec![item("x", "GAME.EXE+20002"), item("y", "GAME.EXE+20004")],
        };
        let tick = |n: u64, state: Vec<(usize, Option<String>)>| Tick {
            tick: n,
            sync: "play".into(),
            cycle: n * 1000,
            inputs: vec![Some("0100".into())],
            state,
            frames: vec![],
            sound: serde_json::Map::new(),
        };
        let trace = Trace {
            header,
            ticks: vec![
                tick(1, vec![(0, Some("0000".into())), (1, Some("0500".into()))]),
                tick(2, vec![(0, Some("0100".into()))]),
                tick(3, vec![]),
            ],
            end: End { end: "the session's end".into(), cycles: 4000 },
        };
        let text = trace.to_text();
        assert_eq!(text.lines().count(), 5);
        assert!(!text.lines().nth(3).unwrap().contains("frames"), "empty lists are left out");
        assert_eq!(Trace::parse(&text).unwrap(), trace);
        let x: Vec<Option<String>> = trace.values(0);
        assert_eq!(x, [Some("0000".into()), Some("0100".into()), Some("0100".into())]);
        assert_eq!(image_address("GAME.EXE", "GAME.EXE+20002").unwrap(), "+20002");
        assert!(image_address("GAME.EXE", "OTHER.EXE+20002").is_err());
    }
}
