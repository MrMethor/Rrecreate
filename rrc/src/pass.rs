//! `rrc pass SESSION --tick N` (D83): what one pass of the game's loop runs, in order: the calls made
//! between tick N-1's sync point and tick N's, as a tree, each function by its name. A port's step is
//! that pass, so the tree is its outline; `--in FUNCTION` shows what a function the pass calls runs.
//! Clockwork's call tracker logs the calls (protocol revision 7); hardware interrupts' handlers run
//! between them, and are left out unless asked for.

use std::process::ExitCode;

use serde_json::{Value, json};

use crate::analysis::Analysis;
use crate::args::Args;
use crate::knowledge::Knowledge;
use crate::repo::Repo;
use crate::say;
use crate::session::Session;
use crate::trace::sync_points;
use crate::watch::Names;

const USAGE: &str = "pass SESSION --tick N [--depth N] [--in ID|NAME] [--interrupts] [--lines N]";
/// Levels of calls shown, and lines.
const DEPTH: usize = 2;
const LINES: usize = 150;
/// Most calls logged in a pass.
const LIMIT: u64 = 1_000_000;

/// One call, and the calls it made.
#[derive(Debug, Clone, PartialEq)]
struct Call {
    site: String,
    target: String,
    /// `call`, `int` or `irq`.
    kind: String,
    calls: Vec<Call>,
}

impl Call {
    fn count(&self) -> usize {
        1 + self.calls.iter().map(Call::count).sum::<usize>()
    }
}

pub(crate) fn pass(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--tick", "--depth", "--in", "--lines"], &["--interrupts"])?;
    let [name] = a.positionals(1, 1, USAGE)? else { unreachable!() };
    let number = |flag: &str, default: usize| -> Result<usize, String> {
        a.value(flag).map_or(Ok(default), str::parse).map_err(|_| format!("{flag} is a number"))
    };
    let tick = number("--tick", 0)? as u64;
    if tick < 2 {
        return Err(format!("--tick is 2 or more: tick 1's pass begins before the first sync point ({USAGE})"));
    }
    let (depth, lines) = (number("--depth", DEPTH)?, number("--lines", LINES)?);
    let session = Session::load(repo, name)?;
    let k = Knowledge::load(&repo.knowledge())?;
    let syncs = sync_points(&k)?;
    if syncs.is_empty() {
        return Err("rrc pass needs sync points (knowledge/ticks.toml)".into());
    }
    let within = match a.value("--in") {
        Some(what) => Some(crate::naming::resolve(repo, &k, Analysis::load(repo)?.as_ref(), what)?),
        None => None,
    };
    let length_ms = (session.seconds * 1000.0) as u64;
    let (mut clockwork, _, from) = crate::watch::open(repo, &session, &syncs, tick)?;
    clockwork.ask("calls", json!({"on": true}))?;
    let ms = crate::watch::to_tick(&mut clockwork, tick, from.as_ref(), length_ms)?;
    clockwork.ask("calls", json!({"on": true, "log": true, "limit": LIMIT}))?;
    let ran = clockwork.ask("run", json!({"sync": "next", "max_ms": length_ms.saturating_sub(ms)}))?;
    if ran.get("sync").is_none() {
        return Err(format!("the session ends before tick {tick}"));
    }
    let log = clockwork.ask("call_log", Value::Null)?;
    let names = Names::new(&k, repo, &session)?;
    let entries: Vec<(i64, String, String, String)> = log["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            let text = |i: usize| c[i].as_str().unwrap_or("?").to_owned();
            (c[0].as_i64().unwrap_or(0), text(1), text(2), text(3))
        })
        .collect();
    let base = log["start"].as_array().map_or(0, Vec::len) as i64;
    let mut calls = tree(&entries, base);
    let interrupts = calls.iter().filter(|c| c.kind == "irq").map(Call::count).sum::<usize>();
    if !a.has("--interrupts") {
        drop_interrupts(&mut calls);
    }
    let running = log["start"][0].as_str().map_or_else(|| "?".to_owned(), |f| names.function(f));
    let total: usize = calls.iter().map(Call::count).sum();
    let dropped = log["dropped"].as_u64().unwrap_or(0);
    say(
        "pass",
        format!(
            "tick {tick}, in {running}: {total} calls{}{}",
            if interrupts > 0 && !a.has("--interrupts") {
                format!(" ({interrupts} in interrupt handlers left out: --interrupts)")
            } else {
                String::new()
            },
            if dropped > 0 { format!(", and {dropped} more not logged") } else { String::new() }
        ),
    );
    let shown: Vec<Call> = match &within {
        Some(id) => {
            let mut found = Vec::new();
            find(&calls, &|c: &Call| names.id(&c.target).as_ref() == Some(id), &mut found);
            if found.is_empty() {
                return Err(format!("the pass of tick {tick} doesn't call {id}"));
            }
            say("in", format!("{} ({id}): called {} times", names.function(&found[0].target), found.len()));
            found
        }
        None => calls,
    };
    let mut out = Vec::new();
    render(&shown, &names, 0, depth, &mut out);
    for line in out.iter().take(lines) {
        println!("{line}");
    }
    if out.len() > lines {
        say("...", format!("{} more lines (--lines N, --depth N, --in FUNCTION)", out.len() - lines));
    }
    Ok(ExitCode::SUCCESS)
}

/// The log's calls as a tree: a call's depth, less `base` (the frames open when the log began), is its
/// level. A call deeper than the one before it by more than one (made inside a frame the log didn't
/// see begin) goes under the deepest there is.
fn tree(entries: &[(i64, String, String, String)], base: i64) -> Vec<Call> {
    let mut roots: Vec<Call> = Vec::new();
    for (depth, site, target, kind) in entries {
        let level = (depth - base).max(0) as usize;
        let call = Call { site: site.clone(), target: target.clone(), kind: kind.clone(), calls: Vec::new() };
        let mut siblings = &mut roots;
        for _ in 0..level {
            if siblings.is_empty() {
                break;
            }
            siblings = &mut siblings.last_mut().expect("not empty").calls;
        }
        siblings.push(call);
    }
    roots
}

/// Leaves out hardware interrupts and what their handlers called.
fn drop_interrupts(calls: &mut Vec<Call>) {
    calls.retain(|c| c.kind != "irq");
    for c in calls {
        drop_interrupts(&mut c.calls);
    }
}

/// The calls `matches` picks, outermost (not those inside one picked).
fn find(calls: &[Call], matches: &dyn Fn(&Call) -> bool, out: &mut Vec<Call>) {
    for c in calls {
        if matches(c) {
            out.push(c.clone());
        } else {
            find(&c.calls, matches, out);
        }
    }
}

/// Lines for `calls` at `level`, `depth` levels in all: a call by its target's name and its site; the
/// same call made again in a row, with the same calls shown under it, once with a count; and under the
/// last level shown, how many calls are below.
fn render(calls: &[Call], names: &Names, level: usize, depth: usize, out: &mut Vec<String>) {
    let below = |c: &Call| -> Vec<String> {
        let mut lines = Vec::new();
        if level + 1 < depth {
            render(&c.calls, names, level + 1, depth, &mut lines);
        }
        lines
    };
    let mut i = 0;
    while i < calls.len() {
        let call = &calls[i];
        let lines = below(call);
        let deeper: usize = call.calls.iter().map(Call::count).sum();
        let mut times = 1;
        while let Some(next) = calls.get(i + times)
            && next.site == call.site
            && next.target == call.target
            && below(next) == lines
        {
            times += 1;
        }
        let what = match call.kind.as_str() {
            "int" => format!("INT handler {}", names.function(&call.target)),
            "irq" => format!("interrupt {}", names.function(&call.target)),
            _ => names.function(&call.target),
        };
        let repeated = if times > 1 { format!(" ×{times}") } else { String::new() };
        let hidden = if level + 1 >= depth && deeper > 0 { format!(", {deeper} calls below") } else { String::new() };
        out.push(format!("{}{what}{repeated}  (at {}{hidden})", "  ".repeat(level), names.plain(&call.site)));
        out.extend(lines);
        i += times;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(depth: i64, target: &str) -> (i64, String, String, String) {
        (depth, format!("site-{target}"), target.to_owned(), "call".to_owned())
    }

    #[test]
    fn the_log_s_depths_make_the_tree() {
        let calls = tree(&[entry(3, "a"), entry(4, "b"), entry(5, "c"), entry(3, "d"), entry(7, "e")], 3);
        assert_eq!(calls.iter().map(|c| c.target.as_str()).collect::<Vec<_>>(), ["a", "d"]);
        assert_eq!(calls[0].calls[0].calls[0].target, "c");
        assert_eq!(calls[1].calls[0].target, "e", "deeper than the log saw begin: under the deepest");
        assert_eq!(calls[0].count(), 3);
    }

    #[test]
    fn interrupts_are_left_out_with_their_handlers_calls() {
        let mut irq = entry(4, "handler");
        irq.3 = "irq".into();
        let mut calls = tree(&[entry(3, "a"), irq, entry(5, "in_handler"), entry(4, "b")], 3);
        drop_interrupts(&mut calls);
        assert_eq!(calls[0].calls.iter().map(|c| c.target.as_str()).collect::<Vec<_>>(), ["b"]);
    }
}
