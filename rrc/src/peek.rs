//! `rrc peek SESSION --tick N WHAT...` (D87): memory at a tick, named or not, without naming it and
//! recording again: the session runs from the last snapshot before tick N to its sync point (the state at
//! tick N), and each item is read there, as its type reads it, else as a number or bytes.

use std::process::ExitCode;

use serde_json::json;

use crate::analysis::Analysis;
use crate::args::Args;
use crate::knowledge::Knowledge;
use crate::places::Places;
use crate::repo::Repo;
use crate::say;
use crate::session::Session;
use crate::trace::sync_points;
use crate::watch::Names;

const USAGE: &str = "peek SESSION --tick N ID|NAME|SEG:OFF... [--len N]";

pub(crate) fn peek(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--tick", "--len"], &[])?;
    let positionals = a.positionals(2, usize::MAX, USAGE)?;
    let tick: u64 = a.value("--tick").ok_or(USAGE)?.parse().map_err(|_| "--tick is a number")?;
    let len: Option<u32> = a.value("--len").map(str::parse).transpose().map_err(|_| "--len is a number of bytes")?;
    let session = Session::load(repo, &positionals[0])?;
    let k = Knowledge::load(&repo.knowledge())?;
    let analysis = Analysis::load(repo)?;
    let syncs = sync_points(&k)?;
    if syncs.is_empty() || tick == 0 {
        return Err("rrc peek needs sync points and a tick from 1".into());
    }
    let places = Places::new(repo, &session);
    let mut items = Vec::new();
    for what in &positionals[1..] {
        let id = crate::naming::resolve(repo, &k, analysis.as_ref(), what)?;
        let size = len.or_else(|| k.data.get(&id).and_then(|d| d.size)).unwrap_or(2);
        items.push((id.clone(), places.address(&id.to_string())?, size));
    }
    let length_ms = (session.seconds * 1000.0) as u64;
    let (mut clockwork, _, from) = crate::watch::open(repo, &session, &syncs, tick + 1)?;
    crate::watch::to_tick(&mut clockwork, tick + 1, from.as_ref(), length_ms)?;
    let ranges: Vec<_> = items.iter().map(|(_, at, size)| json!({"at": at, "len": size})).collect();
    let read = clockwork.ask("read", json!({ "ranges": ranges }))?;
    let names = Names::new(&k, repo, &session)?;
    say("tick", format!("{tick}: the state at its sync point"));
    for ((id, at, size), hex) in items.iter().zip(read["data"].as_array().into_iter().flatten()) {
        let name = names.data(at, *size);
        match hex.as_str() {
            Some(hex) => say("value", format!("{name} = {}", value(&k, id, *size, hex))),
            None => say("value", format!("{name}: not in memory")),
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// As the item's type reads it, if it is a whole named item with one; else a byte, word or doubleword
/// as a number (decimal, and hex), else bytes in memory order.
fn value(k: &Knowledge, id: &ids::Id, size: u32, hex: &str) -> String {
    if let Some(d) = k.data.get(id).filter(|d| !d.ty.is_empty() && d.size == Some(size)) {
        return crate::trace::value_text(Some(hex), &d.ty);
    }
    let bytes: Vec<u8> =
        (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect();
    match bytes.len() {
        1 | 2 | 4 => {
            let n = bytes.iter().rev().fold(0u64, |v, &b| v << 8 | u64::from(b));
            let hex: String = bytes.iter().rev().map(|b| format!("{b:02X}")).collect();
            format!("{n} ({hex}h)")
        }
        _ => bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "),
    }
}
