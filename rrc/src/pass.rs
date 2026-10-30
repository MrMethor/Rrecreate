//! `rrc pass SESSION --tick N` (D83): what one pass of the game's loop runs, in order: the calls made
//! between tick N-1's sync point and tick N's, as a tree, each function by its name. A port's step is
//! that pass, so the tree is its outline; `--in FUNCTION` shows what a function the pass calls runs, and
//! `--functions` every function that runs there, at any depth, with how many times: what porting it takes.
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

const USAGE: &str = "pass SESSION --tick N [--depth N] [--in ID|NAME] [--functions] [--interrupts] [--lines N]";
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
    let a = Args::parse(args, &["--tick", "--depth", "--in", "--lines"], &["--interrupts", "--functions"])?;
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
    if a.has("--functions") {
        let called = functions(&shown);
        say("functions", format!("{} called beneath it", called.len()));
        for (target, n) in called.iter().take(lines) {
            say("  call", format!("{} ×{n}", names.function(target)));
        }
        if called.len() > lines {
            say("...", format!("{} more (--lines N)", called.len() - lines));
        }
        return Ok(ExitCode::SUCCESS);
    }
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

/// Every function the calls beneath `calls` call, at any depth, with how many times, in the order first
/// called.
fn functions(calls: &[Call]) -> Vec<(String, usize)> {
    fn gather(calls: &[Call], called: &mut Vec<(String, usize)>) {
        for c in calls {
            match called.iter_mut().find(|(t, _)| *t == c.target) {
                Some((_, n)) => *n += 1,
                None => called.push((c.target.clone(), 1)),
            }
            gather(&c.calls, called);
        }
    }
    let mut called = Vec::new();
    for c in calls {
        gather(&c.calls, &mut called);
    }
    called
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
/// same call made again in a row, with the same calls shown under it, once with a count; a loop's
/// calls made again in a row (up to `PERIOD` calls a round), once with a line counting the rounds; and
/// under the last level shown, how many calls are below.
fn render(calls: &[Call], names: &Names, level: usize, depth: usize, out: &mut Vec<String>) {
    let below = |c: &Call| -> Vec<String> {
        let mut lines = Vec::new();
        if level + 1 < depth {
            render(&c.calls, names, level + 1, depth, &mut lines);
        }
        lines
    };
    // Each call with its lines, the same call made again in a row as one.
    let mut shown: Vec<Vec<String>> = Vec::new();
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
        let hidden = match deeper {
            _ if level + 1 < depth => String::new(),
            0 => String::new(),
            1 => ", 1 call below".to_owned(),
            n => format!(", {n} calls below"),
        };
        let mut all = vec![format!("{}{what}{repeated}  (at {}{hidden})", "  ".repeat(level), names.plain(&call.site))];
        all.extend(lines);
        shown.push(all);
        i += times;
    }
    let mut i = 0;
    while i < shown.len() {
        let (round, rounds) = rounds(&shown[i..]);
        for lines in &shown[i..i + round] {
            out.extend(lines.iter().cloned());
        }
        if rounds > 1 {
            out.push(format!("{}the {round} calls above ×{rounds}", "  ".repeat(level)));
        }
        i += round * rounds;
    }
}

/// Most calls a round of a loop `render` finds.
const PERIOD: usize = 16;

/// The round that starts `items` and how many times it is made in a row: the one that covers the most
/// items (the shortest of those), or the first item once.
fn rounds<T: PartialEq>(items: &[T]) -> (usize, usize) {
    let mut best = (1, 1);
    for round in 2..=PERIOD.min(items.len() / 2) {
        let mut times = 1;
        while items.len() >= round * (times + 1) && items[round * times..round * (times + 1)] == items[..round] {
            times += 1;
        }
        if times > 1 && round * times > best.0 * best.1 {
            best = (round, times);
        }
    }
    best
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
    fn the_functions_beneath_a_call_are_counted_at_any_depth() {
        let calls = tree(&[entry(0, "+0100"), entry(1, "+0200"), entry(2, "+0300"), entry(1, "+0300")], 0);
        assert_eq!(functions(&calls), [("+0200".to_owned(), 1), ("+0300".to_owned(), 2)]);
    }

    #[test]
    fn a_loop_s_rounds_are_counted() {
        assert_eq!(rounds(&["a", "b", "a", "b", "a", "b", "c"]), (2, 3));
        assert_eq!(rounds(&["a", "b", "c", "a", "b"]), (1, 1), "a round not made twice");
        assert_eq!(rounds(&["a", "b", "a", "b", "c", "a", "b", "c"]), (2, 2), "from the first item");
        assert_eq!(rounds(&["a", "a", "b", "a", "a", "b", "a", "a", "b"]), (3, 3));
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
