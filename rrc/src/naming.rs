//! The knowledge, item by item: `rrc check`, `rrc name`, `rrc data`, `rrc show`.

use std::process::ExitCode;

use ids::Id;

use crate::analysis::Analysis;
use crate::args::Args;
use crate::clockwork::Clockwork;
use crate::knowledge::{Datum, Decision, Knowledge, is_name, parse_id};
use crate::repo::Repo;
use crate::say;

pub(crate) fn check(repo: &Repo) -> Result<ExitCode, String> {
    let k = Knowledge::load(&repo.knowledge())?;
    let image_size = image_size(repo);
    let ticks = &k.ticks;
    say(
        "knowledge",
        format!(
            "{} functions, {} data, {} types, {} sync points, {} inputs",
            k.functions.len(),
            k.data.len(),
            k.types.structs.len() + k.types.enums.len(),
            ticks.syncs.len(),
            ticks.inputs.len()
        ),
    );
    match &image_size {
        Ok(size) => say("image", format!("{size} bytes")),
        Err(e) => say("image", format!("unknown, so not checked against it: {e}")),
    }
    let problems = k.problems(&repo.module(), image_size.ok());
    for problem in &problems {
        say("problem", problem);
    }
    Ok(if problems.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

/// The size of the program's image, from Clockwork loading it.
fn image_size(repo: &Repo) -> Result<u32, String> {
    let (_, loaded) = Clockwork::original(repo, &[])?;
    loaded
        .get("image_size")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as u32)
        .ok_or("Clockwork gave no image size".into())
}

/// Who decides: `--by`, else `RRC_BY`, else `agent`.
fn provenance(a: &Args) -> String {
    a.value("--by").map(str::to_owned).or_else(|| std::env::var("RRC_BY").ok()).unwrap_or_else(|| "agent".into())
}

fn decision(a: &Args, name: &str) -> Result<Decision, String> {
    if !is_name(name) {
        return Err(format!("{name:?} isn't a name (letters, digits, _, @ and $, not starting with a digit)"));
    }
    let confidence = match a.value("--confidence") {
        Some(c) => Some(c.parse::<f64>().ok().filter(|c| (0.0..=1.0).contains(c)).ok_or("--confidence is 0 to 1")?),
        None => None,
    };
    Ok(Decision {
        name: name.to_owned(),
        provenance: provenance(a),
        confidence,
        evidence: a.value("--evidence").unwrap_or_default().to_owned(),
    })
}

/// Fails if `name` names an item other than `id`.
fn unique(k: &Knowledge, id: &Id, name: &str) -> Result<(), String> {
    match k.named(name) {
        Some(other) if other != id => Err(format!("{name} names {other} already")),
        _ => Ok(()),
    }
}

const DECISION_OPTIONS: [&str; 3] = ["--by", "--confidence", "--evidence"];

pub(crate) fn name(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &DECISION_OPTIONS, &[])?;
    let [id, name] = a.positionals(2, 2, "name ID NAME [--evidence TEXT] [--confidence X] [--by WHO]")? else {
        unreachable!()
    };
    let id = parse_id(id)?;
    let mut k = Knowledge::load(&repo.knowledge())?;
    if k.data.contains_key(&id) {
        return Err(format!("{id} is data: rrc data"));
    }
    unique(&k, &id, name)?;
    let old = k.functions.insert(id.clone(), decision(&a, name)?);
    k.save(&repo.knowledge())?;
    say("function", format!("{id} {name}{}", old.map(|d| format!(" (was {})", d.name)).unwrap_or_default()));
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn data(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let options = [DECISION_OPTIONS.as_slice(), &["--size", "--type"]].concat();
    let a = Args::parse(args, &options, &[])?;
    let usage = "data ID NAME [--size N] [--type T] [--evidence TEXT] [--confidence X] [--by WHO]";
    let [id, name] = a.positionals(2, 2, usage)? else { unreachable!() };
    let id = parse_id(id)?;
    let mut k = Knowledge::load(&repo.knowledge())?;
    if k.functions.contains_key(&id) {
        return Err(format!("{id} is a function: rrc name"));
    }
    unique(&k, &id, name)?;
    let size = match a.value("--size") {
        Some(n) => Some(n.parse::<u32>().ok().filter(|&n| n > 0).ok_or("--size is a number of bytes")?),
        None => k.data.get(&id).and_then(|d| d.size),
    };
    let ty = a.value("--type").map(str::to_owned).or_else(|| k.data.get(&id).map(|d| d.ty.clone())).unwrap_or_default();
    let old = k.data.insert(id.clone(), Datum { decision: decision(&a, name)?, size, ty });
    k.save(&repo.knowledge())?;
    say("data", format!("{id} {name}{}", old.map(|d| format!(" (was {})", d.decision.name)).unwrap_or_default()));
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn show(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &[], &["--asm", "--c"])?;
    let [what] = a.positionals(1, 1, "show ID|NAME [--asm] [--c]")? else { unreachable!() };
    let k = Knowledge::load(&repo.knowledge())?;
    let id = match parse_id(what) {
        Ok(id) => id,
        Err(_) => k.named(what).cloned().ok_or_else(|| format!("nothing is named {what:?}"))?,
    };
    let about = |d: &Decision| {
        let confidence = d.confidence.map(|c| format!(", {c}")).unwrap_or_default();
        format!("{id} {} ({}{confidence})", d.name, d.provenance)
    };
    if let Some(d) = k.functions.get(&id) {
        say("function", about(d));
        if !d.evidence.is_empty() {
            say("evidence", &d.evidence);
        }
    } else if let Some(datum) = k.data.get(&id) {
        say("data", about(&datum.decision));
        let size = datum.size.map(|s| format!("{s} bytes")).unwrap_or_else(|| "size unknown".into());
        say("layout", if datum.ty.is_empty() { size } else { format!("{size}, {}", datum.ty) });
        if !datum.decision.evidence.is_empty() {
            say("evidence", &datum.decision.evidence);
        }
    } else {
        say("unnamed", &id);
    }
    let text = id.to_string();
    for s in k.ticks.syncs.iter().filter(|s| s.at == text) {
        say("sync", format!("{}{}", s.name, note(&s.note)));
    }
    for i in k.ticks.inputs.iter().filter(|i| i.at == text) {
        say("input", format!("{}, {} bytes{}", i.name, i.size, note(&i.note)));
    }
    if let Some(analysis) = Analysis::load(repo)? {
        show_analysis(&k, &analysis, &id, a.has("--asm"), a.has("--c"));
    }
    Ok(ExitCode::SUCCESS)
}

/// What Ghidra found about `id`: the function there, its calls and callers, its code.
fn show_analysis(k: &Knowledge, analysis: &Analysis, id: &Id, asm: bool, c: bool) {
    let Some(f) = analysis.functions.get(id) else { return };
    let library = f.library.as_ref().map(|(name, pack)| format!(", {name} in {pack}")).unwrap_or_default();
    say("ghidra", format!("{}, {} bytes{library}", f.name, f.size));
    let named = |id: &Id| k.functions.get(id).map_or_else(|| id.to_string(), |d| d.name.clone());
    let mut calls: Vec<String> = analysis.calls.iter().filter(|c| &c.0 == id).map(|c| named(&c.2)).collect();
    let mut callers: Vec<String> = analysis.calls.iter().filter(|c| &c.2 == id).map(|c| named(&c.0)).collect();
    for (label, list) in [("calls", &mut calls), ("callers", &mut callers)] {
        list.sort();
        list.dedup();
        if !list.is_empty() {
            say(label, list.join(", "));
        }
    }
    if let Some((disassembly, decompiled)) = analysis.code.get(id) {
        if asm {
            print!("{disassembly}");
        }
        if c {
            println!("{}", decompiled.as_deref().unwrap_or("(Ghidra didn't decompile it)"));
        }
    }
}

fn note(text: &str) -> String {
    if text.is_empty() { String::new() } else { format!(": {text}") }
}
