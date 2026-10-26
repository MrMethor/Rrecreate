//! `rrc compare SESSION` (DESIGN.md §7, §8, D61-D63): the port against the original, tick by tick.
//!
//! rrc writes the session's inputs (`rrc-inputs`) from the original's trace, runs the port (`[port]
//! command` in rrc.toml) with them, from its start state, a transplant of the original's mapped values
//! at a tick, or its own state kept at a paired checkpoint, and reads the port's trace back. Every
//! mapped field (`mapping.toml`) is compared at every tick, as values of the knowledge's types. At the
//! first tick where one differs it says which, with both values, and who wrote the original's value in
//! the pass before (a watch from the session's snapshots). Comparing needs no emulator: the original's
//! trace is in the cache (D26); only the who-wrote and a paired checkpoint's snapshot run Clockwork.
//!
//! A scenario session's edits (D64) reach the port with the inputs: at an edit's tick, the mapped
//! fields it wrote, set to the original's values there.

use std::collections::BTreeMap;
use std::process::{Command, ExitCode};

use ids::Id;
use serde_json::{Value, json};

use crate::args::Args;
use crate::cache::Cache;
use crate::knowledge::{Knowledge, Types};
use crate::mapping::{MAPPING, Mapped, Mapping};
use crate::repo::Repo;
use crate::say;
use crate::session::{Pair, Session};
use crate::trace::Trace;
use crate::values::{at_path, decode, differences};

/// Fields shown of a divergence.
const SHOWN: usize = 8;

pub(crate) fn compare(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--transplant", "--resume", "--to", "--pair"], &[])?;
    let usage = "compare SESSION [--transplant TICK | --resume TICK] [--to TICK] [--pair TICK]";
    let [name] = a.positionals(1, 1, usage)? else { unreachable!() };
    let tick = |flag: &str| -> Result<Option<u64>, String> {
        a.value(flag)
            .map(|t| t.parse::<u64>().ok().filter(|&t| t > 0).ok_or(format!("{flag} is a tick number")))
            .transpose()
    };
    let (transplant, resume, to, pair) = (tick("--transplant")?, tick("--resume")?, tick("--to")?, tick("--pair")?);
    if transplant.is_some() && resume.is_some() {
        return Err("--transplant or --resume, not both".into());
    }
    let mut session = Session::load(repo, name)?;
    let recorded = session.trace.clone().ok_or_else(|| format!("{name} isn't recorded: rrc record {name}"))?;
    let k = Knowledge::load(&repo.knowledge())?;
    let mapped = Mapping::load(repo)?.resolve(&k)?;
    if mapped.is_empty() {
        return Err(format!(
            "nothing is mapped: {MAPPING} names the port's fields and the original's items (docs/formats.md)"
        ));
    }
    let cache = Cache::open(&repo.local)?;
    let trace = Trace::parse(&String::from_utf8_lossy(&cache.get(&recorded.hash)?))?;
    let original = original_values(&trace, &mapped, &k.types, name)?;
    let ticks = trace.ticks.len() as u64;
    for t in [transplant, resume, to, pair].into_iter().flatten() {
        if t > ticks {
            return Err(format!("{name} has {ticks} ticks, not {t}"));
        }
    }

    let work = repo.state()?.join("compare").join(name.as_str());
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let file = |f: &str| work.join(f).to_string_lossy().into_owned();
    let after = transplant.or(resume).unwrap_or(0);
    let sets = edit_sets(&trace, &mapped, &original, &k.types, after)?;
    write_inputs(&trace, &sets, &file("inputs.jsonl"))?;
    let mut port_args =
        vec!["run".to_owned(), "--inputs".into(), file("inputs.jsonl"), "--out".into(), file("port.jsonl")];
    if let Some(t) = transplant {
        write_start(&trace.header.session, &mapped, &original, t, &file("start.json"))?;
        port_args.extend(["--transplant".into(), file("start.json")]);
    }
    if let Some(t) = resume {
        let kept = session.pairs.iter().find(|p| p.tick == t).ok_or_else(|| {
            let ticks: Vec<String> = session.pairs.iter().map(|p| p.tick.to_string()).collect();
            format!(
                "{name} has no paired checkpoint at tick {t} (it has: {})",
                if ticks.is_empty() { "none".into() } else { ticks.join(", ") }
            )
        })?;
        std::fs::write(file("resume.json"), cache.get(&kept.port)?).map_err(|e| e.to_string())?;
        port_args.extend(["--resume".into(), file("resume.json")]);
    }
    if let Some(t) = pair {
        let _ = std::fs::remove_file(file("pair.json"));
        port_args.extend(["--save-at".into(), t.to_string(), "--save".into(), file("pair.json")]);
    }
    if let Some(t) = to {
        port_args.extend(["--to".into(), t.to_string()]);
    }
    run_port(repo, &port_args)?;

    // Tick by tick, the port's state against the original's.
    let port_trace = std::fs::read_to_string(file("port.jsonl")).map_err(|e| format!("the port's trace: {e}"))?;
    let mut lines = port_trace.lines().filter(|l| !l.trim().is_empty());
    let header: Value =
        serde_json::from_str(lines.next().ok_or("the port wrote an empty trace")?).map_err(|e| e.to_string())?;
    if header["format"] != "rrc-port-trace" || header["version"] != 1 {
        return Err(format!(
            "the port's trace isn't rrc-port-trace version 1 ({} {})",
            header["format"], header["version"]
        ));
    }
    let mut state = json!({});
    let (mut first, mut last, mut diverged) = (None, 0, None);
    for line in lines {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("the port's trace: {e}"))?;
        let Some(t) = v["tick"].as_u64() else { continue };
        for (key, value) in v["state"].as_object().into_iter().flatten() {
            state[key] = value.clone();
        }
        if t == 0 || t > ticks {
            return Err(format!("the port's trace has tick {t}; the session has 1 to {ticks}"));
        }
        first.get_or_insert(t);
        last = t;
        let mut found = Vec::new();
        for (m, values) in mapped.iter().zip(&original) {
            let theirs = &values[(t - 1) as usize];
            match at_path(&state, &m.field) {
                Some(ours) => differences(&m.field, theirs, ours, SHOWN, &mut found),
                None => found.push((m.field.clone(), theirs.clone(), Value::Null)),
            }
        }
        if !found.is_empty() {
            diverged = Some((t, found));
            break;
        }
    }
    let first = first.ok_or("the port ran no tick")?;
    let bytes: u32 = mapped.iter().map(|m| m.size).sum();
    say("compare", format!("{name}, ticks {first}-{last} of {ticks}: {} fields mapped ({bytes} bytes)", mapped.len()));
    let code = match diverged {
        None => {
            say("match", format!("every mapped field at all {} ticks", last - first + 1));
            ExitCode::SUCCESS
        }
        Some((t, found)) => {
            report(repo, name, &trace, &mapped, &k.types, t, first, transplant.is_some(), &found)?;
            ExitCode::from(1)
        }
    };
    if let Some(t) = pair {
        keep_pair(repo, &mut session, name, &k, t, &file("pair.json"), &cache)?;
    }
    unmapped(&k, &mapped, repo)?;
    Ok(code)
}

/// Each mapped field's value at each tick of the original's trace.
fn original_values(trace: &Trace, mapped: &[Mapped], types: &Types, name: &str) -> Result<Vec<Vec<Value>>, String> {
    let mut all = Vec::new();
    for m in mapped {
        let at = m.id.to_string();
        let column = trace
            .header
            .state
            .iter()
            .position(|item| item.at == at && item.size == m.size)
            .ok_or_else(|| format!("the trace of {name} lacks {} ({at}): rrc record {name}", m.field))?;
        let mut last: Option<(Option<String>, Value)> = None;
        let mut values = Vec::with_capacity(trace.ticks.len());
        for hex in trace.values(column) {
            let value = match &last {
                Some((h, v)) if *h == hex => v.clone(),
                _ => match &hex {
                    Some(h) => decode(&unhex(h)?, &m.ty, types).map_err(|e| format!("{}: {e}", m.field))?,
                    None => Value::Null,
                },
            };
            last = Some((hex, value.clone()));
            values.push(value);
        }
        all.push(values);
    }
    Ok(all)
}

/// The fields the port gets set by the scenario's edits after tick `after` (D64), by tick: the mapped
/// leaves each edit wrote, with the original's values at its tick. An edit of memory nothing maps is
/// said: the port can't get it.
fn edit_sets(
    trace: &Trace,
    mapped: &[Mapped],
    original: &[Vec<Value>],
    types: &Types,
    after: u64,
) -> Result<BTreeMap<u64, serde_json::Map<String, Value>>, String> {
    let mut sets: BTreeMap<u64, serde_json::Map<String, Value>> = BTreeMap::new();
    for edit in &trace.header.edits {
        let (Some(tick), Some(at), Some(hex)) = (edit["tick"].as_u64(), edit["at"].as_str(), edit["hex"].as_str())
        else {
            continue;
        };
        if tick <= after {
            continue;
        }
        let id = crate::knowledge::parse_id(at)?;
        let (start, end) = (id.offset(), id.offset() + hex.len() as u32 / 2);
        let mut covered = 0;
        for (m, values) in mapped.iter().zip(original) {
            if (m.id.module(), m.id.part()) != (id.module(), id.part()) {
                continue;
            }
            let (mut offset, to) = (start.max(m.id.offset()), end.min(m.id.offset() + m.size));
            while offset < to {
                let Some((path, leaf, inner)) = crate::values::place(&m.ty, offset - m.id.offset(), types) else {
                    break;
                };
                let width = crate::values::size(&leaf, types)? - inner;
                let value = crate::values::at_leaf(&values[(tick - 1) as usize], &path).cloned().unwrap_or_default();
                sets.entry(tick).or_default().insert(format!("{}{path}", m.field), value);
                covered += width.min(to - offset);
                offset += width;
            }
        }
        if covered < end - start {
            say(
                "edit",
                format!(
                    "tick {tick}: {}: memory nothing maps, which the port can't get",
                    crate::trace::edit_text(edit)
                ),
            );
        }
    }
    Ok(sets)
}

/// The session's inputs, tick by tick (`rrc-inputs`): from the trace, what the port gets; and the
/// fields the scenario's edits set (`set`).
fn write_inputs(trace: &Trace, sets: &BTreeMap<u64, serde_json::Map<String, Value>>, path: &str) -> Result<(), String> {
    let inputs: Vec<Value> = trace
        .header
        .inputs
        .iter()
        .map(|i| {
            if i.device.is_empty() {
                json!({"name": i.name, "size": i.size})
            } else {
                json!({"name": i.name, "size": i.size, "device": i.device})
            }
        })
        .collect();
    let mut text =
        json!({"format": "rrc-inputs", "version": 1, "session": trace.header.session, "inputs": inputs}).to_string();
    text.push('\n');
    for t in &trace.ticks {
        let read: Vec<Value> = t.inputs.iter().filter_map(|(i, v)| v.as_ref().map(|v| json!([i, v]))).collect();
        let mut line = json!({"tick": t.tick, "sync": t.sync, "inputs": read});
        if let Some(set) = sets.get(&t.tick) {
            line["set"] = json!(set);
        }
        text.push_str(&line.to_string());
        text.push('\n');
    }
    text.push_str(&json!({"end": trace.end.end, "ticks": trace.ticks.len()}).to_string());
    text.push('\n');
    std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))
}

/// A transplant (`rrc-state`): the mapped fields' values at `tick`.
fn write_start(session: &str, mapped: &[Mapped], original: &[Vec<Value>], tick: u64, path: &str) -> Result<(), String> {
    let state: serde_json::Map<String, Value> =
        mapped.iter().zip(original).map(|(m, values)| (m.field.clone(), values[(tick - 1) as usize].clone())).collect();
    let file = json!({"format": "rrc-state", "version": 1, "session": session, "tick": tick, "state": state});
    std::fs::write(path, format!("{file}\n")).map_err(|e| format!("{path}: {e}"))
}

/// Builds the port if rrc.toml says how (`[port] build`), then runs it (`[port] command`) in the port
/// repository.
fn run_port(repo: &Repo, args: &[String]) -> Result<(), String> {
    let port = &repo.settings.port;
    let run = |what: &str, command: &[String], args: &[String]| -> Result<(), String> {
        let (program, rest) = command.split_first().ok_or(format!("[port] {what} is empty"))?;
        let output = Command::new(program)
            .args(rest)
            .args(args)
            .current_dir(&repo.root)
            .output()
            .map_err(|e| format!("[port] {what} ({program}): {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(8).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        Err(format!("[port] {what} failed ({}):\n{}", output.status, tail.join("\n")))
    };
    if !port.build.is_empty() {
        run("build", &port.build, &[])?;
    }
    if port.command.is_empty() {
        return Err("rrc.toml has no [port] command: the port's program, which rrc runs (docs/formats.md)".into());
    }
    run("command", &port.command, args)
}

/// The first divergence: the fields, the tick's late inputs, and who wrote the original's value.
#[allow(clippy::too_many_arguments)]
fn report(
    repo: &Repo,
    name: &str,
    trace: &Trace,
    mapped: &[Mapped],
    types: &Types,
    t: u64,
    first: u64,
    transplanted: bool,
    found: &[(String, Value, Value)],
) -> Result<(), String> {
    let tick = &trace.ticks[(t - 1) as usize];
    say(
        "diverges",
        format!("at tick {t} ({}): {} field{} differ", tick.sync, found.len(), if found.len() == 1 { "" } else { "s" }),
    );
    for (path, theirs, ours) in found {
        let ours = if ours.is_null() { "missing".to_owned() } else { ours.to_string() };
        say("  field", format!("{path}: original {theirs}, port {ours}"));
    }
    // A divergence after a scenario's edit is suspected of the edit first (D59): the state it made may
    // be one play can't reach.
    let mut edits = trace.header.edits.iter().filter(|e| e["tick"].as_u64().is_some_and(|e| e <= t));
    if let Some(edit) = edits.next_back() {
        let at = edit["tick"].as_u64().unwrap_or(0);
        let when = if at == t { "at this tick".to_owned() } else { format!("the last before it, at tick {at}") };
        let text = crate::trace::edit_text(edit);
        say("edit", format!("{when}: {text}; an edited state may be one play can't reach: suspect the edit first"));
    }
    if t == first {
        say(
            "start",
            if transplanted {
                "at the transplant's tick: a field the mapping misses, or the port's transplant() builds it otherwise"
            } else {
                "at the port's first tick: its start() isn't the original's state at its first sync point hit"
            },
        );
        return Ok(());
    }
    let before = &trace.ticks[(t - 2) as usize];
    if !before.late.is_empty() {
        say(
            "late",
            format!(
                "tick {}'s pass read inputs that changed after its sync point (D60): the port got other values",
                t - 1
            ),
        );
    }
    // Who wrote the first differing value: in the pass from tick t-1's sync point to tick t's, which a
    // watch numbers t.
    let (path, _, _) = &found[0];
    let Some((m, offset, size)) = mapped.iter().find_map(|m| locate(m, path, types).map(|(o, s)| (m, o, s))) else {
        return Ok(());
    };
    let id = Id::new(m.id.module(), m.id.part(), m.id.offset() + offset).map_err(|e| e.to_string())?;
    say("wrote", format!("{path} ({id}), between the sync points of ticks {} and {t}:", t - 1));
    let args: Vec<String> =
        [name, &id.to_string(), "--len", &size.to_string(), "--tick", &t.to_string(), "--ticks", "1", "--lines", "6"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    crate::watch::watch(repo, &args).map(|_| ())
}

/// Where a differing path (`enemies[3].x`) is in its mapped item: its offset and size.
fn locate(m: &Mapped, path: &str, types: &Types) -> Option<(u32, u32)> {
    let rest = path.strip_prefix(m.field.as_str())?;
    if !rest.is_empty() && !rest.starts_with(['[', '.']) {
        return None;
    }
    let (offset, ty) = crate::values::locate(&m.ty, rest, types)?;
    Some((offset, crate::values::size(&ty, types).ok()?))
}

/// Keeps a paired checkpoint (D63): the port's own state at tick `t` and the original's snapshot there.
fn keep_pair(
    repo: &Repo,
    session: &mut Session,
    name: &str,
    k: &Knowledge,
    t: u64,
    saved: &str,
    cache: &Cache,
) -> Result<(), String> {
    let port = match std::fs::read(saved) {
        Ok(bytes) => cache.put(&bytes)?,
        Err(_) => {
            say("pair", format!("not kept: the port didn't reach tick {t}"));
            return Ok(());
        }
    };
    let snapshot = crate::snapshots::at_tick(repo, session, k, t)?;
    session.pairs.retain(|p| p.tick != t);
    session.pairs.push(Pair { tick: t, port, snapshot });
    session.pairs.sort_by_key(|p| p.tick);
    session.save(repo, name)?;
    say("pair", format!("kept at tick {t}: rrc compare {name} --resume {t} starts the port there"));
    Ok(())
}

/// Says which named data the mapping neither maps nor ignores: the state not compared yet.
fn unmapped(k: &Knowledge, mapped: &[Mapped], repo: &Repo) -> Result<(), String> {
    let ignored = Mapping::load(repo)?.ignored(k)?;
    let covered = |id: &Id, size: u32| {
        let inside = |at: &Id, n: u32| {
            at.module() == id.module()
                && at.part() == id.part()
                && at.offset() <= id.offset()
                && id.offset() + size <= at.offset() + n
        };
        mapped.iter().any(|m| inside(&m.id, m.size)) || ignored.iter().any(|(at, n)| inside(at, *n))
    };
    let inputs: Vec<String> = k.ticks.inputs.iter().map(|i| i.at.clone()).collect();
    let left: Vec<&str> = k
        .data
        .iter()
        .filter(|(id, d)| d.size.is_some_and(|size| !covered(id, size)) && !inputs.contains(&id.to_string()))
        .map(|(_, d)| d.decision.name.as_str())
        .collect();
    if !left.is_empty() {
        let shown: Vec<&str> = left.iter().take(10).copied().collect();
        let more = if left.len() > 10 { format!(" and {} more", left.len() - 10) } else { String::new() };
        say("unmapped", format!("{} named data not mapped or ignored: {}{more}", left.len(), shown.join(", ")));
    }
    Ok(())
}

fn unhex(text: &str) -> Result<Vec<u8>, String> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).map_err(|_| format!("bad hex {text:?}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::{Field, Struct};

    #[test]
    fn a_differing_path_is_located_in_its_item() {
        let field = |offset, size, name: &str, ty: &str| Field { offset, size, name: name.into(), ty: ty.into() };
        let types = Types {
            structs: vec![Struct {
                name: "enemy".into(),
                size: 10,
                fields: vec![field(0, 2, "x", "i16"), field(2, 2, "y", "i16"), field(8, 1, "alive", "u8")],
                provenance: String::new(),
            }],
            enums: vec![],
        };
        let m =
            Mapped { field: "enemies".into(), id: "GAME.EXE+00100".parse().unwrap(), ty: "enemy[6]".into(), size: 60 };
        assert_eq!(locate(&m, "enemies[3].y", &types), Some((32, 2)));
        assert_eq!(locate(&m, "enemies[1]", &types), Some((10, 10)));
        assert_eq!(locate(&m, "enemies", &types), Some((0, 60)));
        assert_eq!(locate(&m, "enemiesx", &types), None);
        let score =
            Mapped { field: "player.score".into(), id: "GAME.EXE+00200".parse().unwrap(), ty: "u16".into(), size: 2 };
        assert_eq!(locate(&score, "player.score", &types), Some((0, 2)));
    }
}
