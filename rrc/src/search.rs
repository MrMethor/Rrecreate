//! `rrc search SESSION CONDITION...` (D75): the original's memory found by how it behaves over the
//! session's ticks, as memory scanners find a game's values: what changed while the player walked, stayed
//! the same while they stood, grew, shrank, or held a value. Every place in the data segment (an LE
//! program's data objects, or the range given) is read as the type at the ticks the conditions name;
//! those that meet them all are the candidates for the state the agent is looking for.

use std::collections::BTreeMap;
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::args::Args;
use crate::clockwork;
use crate::knowledge::Knowledge;
use crate::places::Places;
use crate::repo::Repo;
use crate::say;
use crate::session::Session;
use crate::trace::{sync_points, unhex};
use crate::watch::{Names, open};

/// Candidates shown.
const SHOWN: usize = 20;
/// Ticks a `same` condition may span (each is read whole).
const MAX_SPAN: u64 = 200;

/// A condition on a place's values, at ticks.
#[derive(Debug, Clone, PartialEq)]
enum Condition {
    Changed(u64, u64),
    /// The same at every tick from the first to the second.
    Same(u64, u64),
    More(u64, u64),
    Less(u64, u64),
    Is(u64, i64),
}

impl Condition {
    fn parse(text: &str) -> Result<Condition, String> {
        let wrong =
            || format!("{text:?}: a condition is changed:A-B, same:A-B, more:A-B, less:A-B or is:T=V (ticks A, B, T)");
        let (kind, rest) = text.split_once(':').ok_or_else(wrong)?;
        let tick = |t: &str| t.parse::<u64>().ok().filter(|&t| t > 0).ok_or_else(wrong);
        if kind == "is" {
            let (t, v) = rest.split_once('=').ok_or_else(wrong)?;
            return Ok(Condition::Is(tick(t)?, v.parse().map_err(|_| wrong())?));
        }
        let (a, b) = rest.split_once('-').ok_or_else(wrong)?;
        let (a, b) = (tick(a)?, tick(b)?);
        if b <= a {
            return Err(format!("{text:?}: the second tick comes after the first"));
        }
        match kind {
            "changed" => Ok(Condition::Changed(a, b)),
            "same" if b - a > MAX_SPAN => Err(format!("{text:?}: same spans at most {MAX_SPAN} ticks")),
            "same" => Ok(Condition::Same(a, b)),
            "more" => Ok(Condition::More(a, b)),
            "less" => Ok(Condition::Less(a, b)),
            _ => Err(wrong()),
        }
    }

    /// The ticks it reads.
    fn ticks(&self) -> Vec<u64> {
        match *self {
            Condition::Same(a, b) => (a..=b).collect(),
            Condition::Changed(a, b) | Condition::More(a, b) | Condition::Less(a, b) => vec![a, b],
            Condition::Is(t, _) => vec![t],
        }
    }

    /// Whether the value at each tick (`at`) meets it.
    fn holds(&self, at: &dyn Fn(u64) -> i64) -> bool {
        match *self {
            Condition::Changed(a, b) => at(a) != at(b),
            Condition::Same(a, b) => (a + 1..=b).all(|t| at(t) == at(a)),
            Condition::More(a, b) => at(b) > at(a),
            Condition::Less(a, b) => at(b) < at(a),
            Condition::Is(t, v) => at(t) == v,
        }
    }
}

/// A number type: size, signed.
fn number_type(ty: &str) -> Result<(usize, bool), String> {
    match ty {
        "u8" => Ok((1, false)),
        "i8" => Ok((1, true)),
        "u16" => Ok((2, false)),
        "i16" => Ok((2, true)),
        "u32" => Ok((4, false)),
        "i32" => Ok((4, true)),
        _ => Err(format!("--type {ty}: u8, i8, u16, i16, u32 or i32")),
    }
}

fn value(bytes: &[u8], at: usize, (size, signed): (usize, bool)) -> i64 {
    let raw = bytes[at..at + size].iter().rev().fold(0u64, |v, &b| (v << 8) | u64::from(b));
    let shift = 64 - 8 * size as u32;
    if signed { ((raw << shift) as i64) >> shift } else { raw as i64 }
}

pub(crate) fn search(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--type", "--at", "--len", "--lines", "--step"], &[])?;
    let usage = "search SESSION CONDITION... [--type T] [--step N] [--at ID --len N] [--lines N]";
    let positional = a.positionals(2, usize::MAX, usage)?;
    let name = &positional[0];
    let conditions: Vec<Condition> = positional[1..].iter().map(|c| Condition::parse(c)).collect::<Result<_, _>>()?;
    let ty = a.value("--type").unwrap_or("u16");
    let number = number_type(ty)?;
    let step: usize = a.value("--step").map_or(Ok(1), str::parse).map_err(|_| "--step is a number of bytes")?;
    let shown: usize = a.value("--lines").map_or(Ok(SHOWN), str::parse).map_err(|_| "--lines is a count")?;
    let mut ticks: Vec<u64> = conditions.iter().flat_map(Condition::ticks).collect();
    ticks.sort_unstable();
    ticks.dedup();

    let session = Session::load(repo, name)?;
    let k = Knowledge::load(&repo.knowledge())?;
    let syncs = sync_points(&k)?;
    if syncs.is_empty() {
        return Err("rrc search reads memory at ticks: declare a sync point first (knowledge/ticks.toml)".into());
    }
    // Each range's bytes at each tick: kept from an earlier search, or read now (and kept, D76).
    let what = match a.value("--at") {
        Some(at) => format!("{at} {}", a.value("--len").unwrap_or_default()),
        None => "data".to_owned(),
    };
    let key = crate::memory::key(repo, &session, &syncs, &what)?;
    let (first, last) = (ticks[0], ticks[ticks.len() - 1]);
    let (ranges, memory) = match crate::memory::kept(repo, &key, first, last)? {
        Some(span) => {
            say(
                "memory",
                format!("ticks {}-{}, kept (by the recording or a search): no emulator", span.first, span.last),
            );
            (span.ranges.clone(), crate::memory::load(repo, &span, &ticks)?)
        }
        None => read(repo, &a, &k, &session, &ticks, &key)?,
    };

    let names = Names::new(&k, repo, &session)?;
    let mut found: Vec<(String, Vec<(u64, i64)>)> = Vec::new();
    let mut total = 0;
    for (r, (at, len)) in ranges.iter().enumerate() {
        let complete = memory.values().all(|m| m[r].len() == *len as usize);
        if !complete {
            continue;
        }
        let base = clockwork::Address::parse(at);
        for offset in (0..(*len as usize).saturating_sub(number.0 - 1)).step_by(step.max(1)) {
            let at_tick = |t: u64| value(&memory[&t][r], offset, number);
            if conditions.iter().all(|c| c.holds(&at_tick)) {
                total += 1;
                if found.len() < shown {
                    let address = base
                        .map_or_else(|| format!("{at}+{offset:X}"), |b| b.with(b.number() + offset as u32).to_string());
                    let shown_ticks = conditions.iter().flat_map(|c| match *c {
                        Condition::Same(a, b) => vec![a, b],
                        _ => c.ticks(),
                    });
                    let mut values: Vec<(u64, i64)> = shown_ticks.map(|t| (t, at_tick(t))).collect();
                    values.sort_unstable();
                    values.dedup();
                    found.push((names.data(&address, number.0 as u32), values));
                }
            }
        }
    }
    let scanned: Vec<String> = ranges.iter().map(|(at, len)| format!("{} ({len} bytes)", names.plain(at))).collect();
    say("search", format!("{name}, {} as {ty}: {total} places meet {}", scanned.join(", "), positional[1..].join(" ")));
    for (place, values) in &found {
        let values: Vec<String> = values.iter().map(|(t, v)| format!("{t}: {v}")).collect();
        say("found", format!("{place}  {}", values.join(", ")));
    }
    if total > found.len() {
        say("...", format!("{} more: add a condition, or --lines N", total - found.len()));
    }
    Ok(ExitCode::SUCCESS)
}

/// The ranges a search reads (the data segment at the first tick, an LE program's data objects, or the
/// range asked for) and their bytes at each tick it needs, read from Clockwork: every tick from the first
/// to the last, kept for later searches, unless that is more than [`crate::memory::MAX_KEPT`] bytes.
#[allow(clippy::type_complexity)]
fn read(
    repo: &Repo,
    a: &Args,
    k: &Knowledge,
    session: &Session,
    ticks: &[u64],
    key: &str,
) -> Result<(Vec<(String, u32)>, BTreeMap<u64, Vec<Vec<u8>>>), String> {
    let syncs = sync_points(k)?;
    let length_ms = (session.seconds * 1000.0) as u64;
    let (first, last) = (ticks[0], ticks[ticks.len() - 1]);
    // Tick t's state is memory at its sync point, where tick t+1's pass starts.
    let (mut clockwork, loaded, from) = open(repo, session, &syncs, first + 1)?;
    crate::watch::to_tick(&mut clockwork, first + 1, from.as_ref(), length_ms)?;
    let places = Places::new(repo, session);
    let ranges: Vec<(String, u32)> = match a.value("--at") {
        Some(at) => {
            let analysis = crate::analysis::Analysis::load(repo)?;
            let id = crate::naming::resolve(repo, k, analysis.as_ref(), at)?;
            let len: u32 = a.required("--len")?.parse().map_err(|_| "--len is a number of bytes")?;
            vec![(places.address(&id.to_string())?, len)]
        }
        None if crate::places::is_le(repo) => {
            let objects = places.objects()?.to_vec();
            let data = objects.iter().filter(|&&(_, _, flags)| flags & 4 == 0);
            data.map(|&(base, size, _)| (format!("@{base:X}"), size)).collect()
        }
        None => {
            // DS's segment at the first tick, as an offset in the image when it is in it.
            let regs = clockwork.ask("regs", Value::Null)?;
            let ds = clockwork::number(&regs, "ds")? as u32;
            match (ds * 16).checked_sub(clockwork::linear(&loaded, "image")?) {
                Some(offset) => vec![(format!("+{offset:05X}"), 0x1_0000)],
                None => vec![(format!("{ds:04X}:0000"), 0x1_0000)],
            }
        }
    };
    let request: Vec<Value> = ranges.iter().map(|(at, len)| json!({"at": at, "len": len})).collect();
    let bytes: u64 = ranges.iter().map(|(_, len)| u64::from(*len)).sum::<u64>() * (last - first + 1);
    let keep = bytes <= crate::memory::MAX_KEPT;
    let read: Vec<u64> = if keep { (first..=last).collect() } else { ticks.to_vec() };
    let mut span: Vec<Vec<Option<Vec<u8>>>> = Vec::new();
    let mut memory: BTreeMap<u64, Vec<Vec<u8>>> = BTreeMap::new();
    let mut now = first;
    for t in read {
        if t > now {
            let ran = clockwork.ask("run", json!({"sync": "next", "n": t - now, "max_ms": length_ms}))?;
            if ran.get("sync").is_none() {
                return Err(format!("the session ends before tick {t}"));
            }
            now = t;
        }
        let answer = clockwork.ask("read", json!({ "ranges": request }))?;
        let tick: Vec<Option<Vec<u8>>> =
            answer["data"].as_array().into_iter().flatten().map(|v| v.as_str().map(unhex)).collect();
        if ticks.contains(&t) {
            memory.insert(t, tick.iter().map(|m| m.clone().unwrap_or_default()).collect());
        }
        if keep {
            span.push(tick);
        }
    }
    if keep {
        crate::memory::keep(repo, key, first, &ranges, &span)?;
        say("memory", format!("ticks {first}-{last}, read and kept: searches within them run no emulator"));
    }
    Ok((ranges, memory))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditions_read_and_hold() {
        assert_eq!(Condition::parse("changed:3-7"), Ok(Condition::Changed(3, 7)));
        assert_eq!(Condition::parse("is:4=-2"), Ok(Condition::Is(4, -2)));
        assert_eq!(Condition::parse("same:2-5").unwrap().ticks(), [2, 3, 4, 5]);
        assert!(Condition::parse("more:7-3").is_err() && Condition::parse("bigger:1-2").is_err());
        assert!(Condition::parse("same:1-500").unwrap_err().contains("at most 200"));
        let values = |t: u64| [0, 5, 5, 5, 9][t as usize - 1];
        assert!(Condition::Same(2, 4).holds(&values) && !Condition::Same(1, 2).holds(&values));
        assert!(Condition::More(1, 5).holds(&values) && !Condition::Less(1, 5).holds(&values));
        assert!(Condition::Changed(4, 5).holds(&values) && Condition::Is(5, 9).holds(&values));
        assert_eq!(value(&[0xFE, 0xFF, 0x01], 0, (2, true)), -2);
        assert_eq!(value(&[0xFE, 0xFF, 0x01], 1, (2, false)), 0x01FF);
    }
}
