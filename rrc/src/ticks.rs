//! `rrc sync` and `rrc input`: the sync points and boundary values in `knowledge/ticks.toml`
//! (`docs/formats.md`), with who decided them and from what, like every other knowledge item.

use std::process::ExitCode;

use crate::args::Args;
use crate::knowledge::{DEVICES, Input, Knowledge, Sync};
use crate::naming::{DECISION_OPTIONS, decision};
use crate::repo::Repo;
use crate::say;

/// `rrc sync [ID NAME [--when-changed A,B] [--note TEXT] ...] | [NAME --remove]`.
pub(crate) fn sync(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let options = [DECISION_OPTIONS.as_slice(), &["--when-changed", "--note"]].concat();
    let a = Args::parse(args, &options, &["--remove"])?;
    let usage = "sync [ID NAME [--when-changed A,B] [--note TEXT] [--evidence TEXT] [--confidence X] [--by WHO]] \
                 [NAME --remove]";
    let _lock = crate::knowledge::EditLock::take(&repo.state()?)?;
    let mut k = Knowledge::load(&repo.knowledge())?;
    if a.has("--remove") {
        let [name] = a.positionals(1, 1, usage)? else { unreachable!() };
        let before = k.ticks.syncs.len();
        k.ticks.syncs.retain(|s| &s.name != name);
        if k.ticks.syncs.len() == before {
            return Err(format!("no sync point {name}"));
        }
        k.save(&repo.knowledge())?;
        say("removed", name);
        return Ok(ExitCode::SUCCESS);
    }
    let positionals = a.positionals(0, 2, usage)?;
    let [id, name] = positionals else {
        if !positionals.is_empty() {
            return Err(format!("usage: rrc {usage}"));
        }
        for s in &k.ticks.syncs {
            say("sync", sync_text(s));
        }
        if k.ticks.syncs.is_empty() {
            say("none", "no sync points yet: rrc calls SESSION finds the loops (docs/porting.md)");
        }
        return Ok(ExitCode::SUCCESS);
    };
    let id = crate::naming::place(repo, id)?;
    let d = decision(&a, name)?;
    let when_changed = list(a.value("--when-changed"));
    for item in &when_changed {
        k.gate_item(item)?;
    }
    let sync = Sync {
        name: name.clone(),
        at: id.to_string(),
        when_changed,
        provenance: d.provenance,
        confidence: d.confidence,
        evidence: d.evidence,
        note: a.value("--note").unwrap_or_default().to_owned(),
    };
    say("sync", sync_text(&sync));
    match k.ticks.syncs.iter_mut().find(|s| s.name == *name) {
        Some(old) => {
            say("was", sync_text(old));
            *old = sync;
        }
        None => k.ticks.syncs.push(sync),
    }
    k.save(&repo.knowledge())?;
    Ok(ExitCode::SUCCESS)
}

/// `rrc input [ID|DEVICE NAME [--size N] [--sync A,B] [--part TEXT --read-at A,B] [--note TEXT] ...] |
/// [NAME --remove]`.
pub(crate) fn input(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let options = [DECISION_OPTIONS.as_slice(), &["--size", "--sync", "--part", "--read-at", "--note"]].concat();
    let a = Args::parse(args, &options, &["--remove"])?;
    let usage = "input [ID|mouse|joystick NAME [--size N] [--sync A,B] [--part TEXT --read-at A,B] [--note TEXT] \
                 [--evidence TEXT] [--confidence X] [--by WHO]] [NAME --remove]";
    let _lock = crate::knowledge::EditLock::take(&repo.state()?)?;
    let mut k = Knowledge::load(&repo.knowledge())?;
    if a.has("--remove") {
        let [name] = a.positionals(1, 1, usage)? else { unreachable!() };
        let before = k.ticks.inputs.len();
        k.ticks.inputs.retain(|i| &i.name != name);
        if k.ticks.inputs.len() == before {
            return Err(format!("no input {name}"));
        }
        k.save(&repo.knowledge())?;
        say("removed", name);
        return Ok(ExitCode::SUCCESS);
    }
    let positionals = a.positionals(0, 2, usage)?;
    let [what, name] = positionals else {
        if !positionals.is_empty() {
            return Err(format!("usage: rrc {usage}"));
        }
        for i in &k.ticks.inputs {
            say("input", input_text(i));
        }
        if k.ticks.inputs.is_empty() {
            say("none", "no boundary values yet: rrc inputs SESSION finds candidates (docs/porting.md)");
        }
        return Ok(ExitCode::SUCCESS);
    };
    let d = decision(&a, name)?;
    let size = match a.value("--size") {
        Some(n) => n.parse::<u32>().ok().filter(|&n| n > 0).ok_or("--size is a number of bytes")?,
        None => 0,
    };
    let (at, device) = if DEVICES.iter().any(|(device, _)| device == what) {
        (String::new(), what.clone())
    } else {
        (crate::naming::place(repo, what)?.to_string(), String::new())
    };
    // Named data there gives its size.
    let size = match (size, k.data.iter().find(|(id, _)| id.to_string() == at)) {
        (0, Some((_, datum))) if device.is_empty() => datum.size.unwrap_or(0),
        (size, _) => size,
    };
    if device.is_empty() && size == 0 {
        return Err(format!("{at} isn't named data with a size: give --size N"));
    }
    let sync = list(a.value("--sync"));
    // A polling loop's gate names its inputs, so they come first: they may name its sync point before
    // it is declared (rrc check and rrc record say if it never is).
    for unknown in sync.iter().filter(|s| !k.ticks.syncs.iter().any(|t| t.name == **s)) {
        say("note", format!("no sync point {unknown} yet: declare it (rrc sync) before recording"));
    }
    // What a part not ported writes (D96), read where the ported code reads it.
    let part = a.value("--part").unwrap_or_default().to_owned();
    let read_at = list(a.value("--read-at"))
        .iter()
        .map(|id| crate::naming::place(repo, id).map(|id| id.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    if !read_at.is_empty() && part.is_empty() {
        return Err("--read-at goes with --part: what writes it, not ported (other inputs are read anywhere)".into());
    }
    if !part.is_empty() && !device.is_empty() {
        return Err("--part is for memory a part of the program writes, not a device".into());
    }
    let input = Input {
        name: name.clone(),
        at,
        device,
        size,
        sync,
        part,
        read_at,
        provenance: d.provenance,
        confidence: d.confidence,
        evidence: d.evidence,
        note: a.value("--note").unwrap_or_default().to_owned(),
    };
    say("input", input_text(&input));
    match k.ticks.inputs.iter_mut().find(|i| i.name == *name) {
        Some(old) => {
            say("was", input_text(old));
            *old = input;
        }
        None => k.ticks.inputs.push(input),
    }
    k.save(&repo.knowledge())?;
    Ok(ExitCode::SUCCESS)
}

/// A comma-separated list.
fn list(text: Option<&str>) -> Vec<String> {
    text.into_iter().flat_map(|t| t.split(',')).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

fn sync_text(s: &Sync) -> String {
    let gate = if s.when_changed.is_empty() {
        String::new()
    } else {
        format!(", a tick when {} changed", s.when_changed.join(", "))
    };
    format!("{} {}{gate} ({})", s.name, s.at, if s.provenance.is_empty() { "?" } else { &s.provenance })
}

fn input_text(i: &Input) -> String {
    let what = if i.device.is_empty() { format!("{}, {} bytes", i.at, i.size) } else { format!("the {}", i.device) };
    let at = if i.sync.is_empty() { String::new() } else { format!(", at {}", i.sync.join(", ")) };
    let part = match (i.part.is_empty(), i.read_at.is_empty()) {
        (true, _) => String::new(),
        (false, true) => format!(", written by {}", i.part),
        (false, false) => format!(", written by {}, read at {}", i.part, i.read_at.join(", ")),
    };
    format!("{} {what}{at}{part} ({})", i.name, if i.provenance.is_empty() { "?" } else { &i.provenance })
}
