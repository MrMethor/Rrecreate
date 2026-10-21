//! Who touches memory, through Clockwork's watches (D8):
//!
//! - `rrc watch SESSION ID`: each write (and read) of an item during a session or some of its ticks,
//!   with the instruction and the call chain, named from the knowledge.
//! - `rrc inputs SESSION`: the boundary values' candidates: memory the program reads that interrupt
//!   handlers (the keyboard's, the timer's) or host services write. A summary watch over the data
//!   segment finds them without anyone knowing their names.

use std::process::ExitCode;

use ids::Id;
use serde_json::{Value, json};

use crate::analysis::Analysis;
use crate::args::Args;
use crate::clockwork::Clockwork;
use crate::knowledge::Knowledge;
use crate::repo::Repo;
use crate::say;
use crate::session::Session;
use crate::snapshots::{self, Taken};
use crate::trace::{SyncPoint, begin, image_address, sync_points};

/// Calls shown of a chain, innermost first.
const CHAIN_SHOWN: usize = 3;
/// Lines of accesses shown.
const ACCESSES_SHOWN: usize = 40;

/// Clockwork's addresses (`+OFF` in the image, `@LINEAR` elsewhere) with the knowledge's names.
struct Names<'a> {
    k: &'a Knowledge,
    module: String,
}

impl Names<'_> {
    fn id(&self, address: &str) -> Option<Id> {
        let offset = u32::from_str_radix(address.strip_prefix('+')?, 16).ok()?;
        Id::new(&self.module, None, offset).ok()
    }

    /// A function's name, or its address.
    fn function(&self, address: &str) -> String {
        let id = self.id(address);
        id.as_ref().and_then(|id| self.k.functions.get(id)).map_or_else(|| self.plain(address), |d| d.name.clone())
    }

    /// A data item's name (`name[+N]` inside one), or the address.
    fn data(&self, address: &str) -> String {
        let Some(id) = self.id(address) else { return address.to_owned() };
        match self.k.data.range(..=id.clone()).next_back() {
            Some((start, d)) if *start == id => d.decision.name.clone(),
            Some((start, d)) if d.size.is_some_and(|size| id.offset() - start.offset() < size) => {
                format!("{}[+{}]", d.decision.name, id.offset() - start.offset())
            }
            _ => id.to_string(),
        }
    }

    /// An ID for an image address, the address otherwise.
    fn plain(&self, address: &str) -> String {
        self.id(address).map_or_else(|| address.to_owned(), |id| id.to_string())
    }
}

pub(crate) fn watch(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--len", "--tick", "--ticks", "--limit", "--lines"], &["--reads", "--cycles"])?;
    let usage = "watch SESSION ID|NAME|SEG:OFF [--len N] [--tick N] [--ticks N] [--reads] [--cycles] [--limit N] \
                 [--lines N]";
    let limit: u64 = a.value("--limit").map_or(Ok(10_000), str::parse).map_err(|_| "--limit is a count")?;
    let lines_shown: usize =
        a.value("--lines").map_or(Ok(ACCESSES_SHOWN), str::parse).map_err(|_| "--lines is a count")?;
    let [name, what] = a.positionals(2, 2, usage)? else { unreachable!() };
    let session = Session::load(repo, name)?;
    let k = Knowledge::load(&repo.knowledge())?;
    let analysis = Analysis::load(repo)?;
    let id = crate::naming::resolve(repo, &k, analysis.as_ref(), what)?;
    let len: u32 = match a.value("--len") {
        Some(n) => n.parse().map_err(|_| "--len is a number of bytes")?,
        None => k.data.get(&id).and_then(|d| d.size).unwrap_or(2),
    };
    let (first, count) = tick_span(&a)?;
    let syncs = sync_points(&k)?;
    if syncs.is_empty() && a.value("--tick").is_some() {
        return Err("--tick needs sync points (knowledge/ticks.toml)".into());
    }
    let length_ms = (session.seconds * 1000.0) as u64;
    let (mut clockwork, _, from) = open(repo, &session, &syncs, first)?;
    // Tracked from the start (or the snapshot), so each access has its call chain from there.
    clockwork.ask("calls", json!({"on": true}))?;
    let mut ms = to_tick(&mut clockwork, first, from.as_ref(), length_ms)?;
    let at = image_address(&repo.module(), &id.to_string())?;
    clockwork.ask("watch", json!({"ranges": [{"at": at, "len": len}], "reads": a.has("--reads"), "limit": limit}))?;
    // Tick by tick, keeping where each ends.
    let mut ends = Vec::new();
    if syncs.is_empty() {
        clockwork.ask("run", json!({"ms": length_ms}))?;
    } else {
        while ms < length_ms && count.is_none_or(|n| (ends.len() as u64) < n) {
            let ran = clockwork.ask("run", json!({"sync": "next", "max_ms": length_ms - ms}))?;
            ms = ran["ms"].as_u64().unwrap_or(length_ms);
            if ran.get("sync").is_none() {
                break;
            }
            ends.push(ran["cycles"].as_u64().unwrap_or(0));
        }
    }
    let watched = clockwork.ask("watch_end", Value::Null)?;
    let names = Names { k: &k, module: repo.module() };
    let span = if syncs.is_empty() { "the whole session".to_owned() } else { ticks_text(first, ends.len() as u64) };
    say("watch", format!("{} ({id}, {len} bytes), {span}", names.data(&at)));
    // Each tick's accesses, those alike counted once (with --cycles, each on its own line). What runs
    // before the first sync point hit (loading, unpacking, the first screens) is "start".
    let mut lines: Vec<(String, String, usize)> = Vec::new();
    for access in group(watched["hits"].as_array().cloned().unwrap_or_default()) {
        let tick = match ends.iter().position(|&end| access.cycle <= end) {
            Some(0) if first == 1 => "start".to_owned(),
            Some(i) => (first + i as u64).to_string(),
            None => "-".to_owned(),
        };
        let mut text = access.describe(&names);
        if a.has("--cycles") {
            text = format!("{text}, cycle {}", access.cycle);
        }
        match lines.iter_mut().find(|(t, line, _)| *t == tick && *line == text) {
            Some((_, _, n)) => *n += 1,
            None => lines.push((tick, text, 1)),
        }
    }
    for (tick, text, n) in lines.iter().take(lines_shown) {
        say(tick, if *n > 1 { format!("{text} ({n} times)") } else { text.clone() });
    }
    if lines.len() > lines_shown {
        say("...", format!("{} more lines (--lines N shows more)", lines.len() - lines_shown));
    }
    if lines.is_empty() {
        say(
            "none",
            if a.has("--reads") { "nothing read or wrote it" } else { "nothing wrote it (--reads for reads too)" },
        );
    }
    let dropped = watched["dropped"].as_u64().unwrap_or(0);
    if dropped > 0 {
        say(
            "dropped",
            format!("{dropped} accesses past the limit of {limit} (--limit N): the counts above stop there"),
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// `--tick N` (default 1) and `--ticks N` (default: to the session's end).
fn tick_span(a: &Args) -> Result<(u64, Option<u64>), String> {
    let first: u64 = a.value("--tick").map_or(Ok(1), str::parse).map_err(|_| "--tick is a number")?;
    let count = a.value("--ticks").map(str::parse::<u64>).transpose().map_err(|_| "--ticks is a number")?;
    if first == 0 || count == Some(0) {
        return Err("ticks count from 1".into());
    }
    Ok((first, count))
}

fn ticks_text(first: u64, count: u64) -> String {
    match count {
        0 => format!("no tick from {first} on"),
        1 => format!("tick {first}"),
        n => format!("ticks {first}-{}", first + n - 1),
    }
}

/// Runs to where tick `n` starts (the end of tick n-1); returns the session's milliseconds then.
/// Clockwork with the session and its sync points: from the last snapshot `rrc record` kept before
/// tick `first`, if there is one (which it says), else from the start; what loading answered.
fn open(
    repo: &Repo,
    session: &Session,
    syncs: &[SyncPoint],
    first: u64,
) -> Result<(Clockwork, Value, Option<Taken>), String> {
    if first > 1
        && let Some(taken) = snapshots::before(repo, session, syncs, first)?
    {
        let (clockwork, loaded) = snapshots::load(repo, session, &taken)?;
        say("start", format!("from the snapshot at tick {} ({:.1} s)", taken.tick, taken.ms as f64 / 1000.0));
        return Ok((clockwork, loaded, Some(taken)));
    }
    let (clockwork, loaded, _) = begin(repo, session, syncs)?;
    Ok((clockwork, loaded, None))
}

/// Runs to the end of tick `n - 1`, where tick `n` begins (from the snapshot `from`, if one was
/// loaded); the ms then.
fn to_tick(clockwork: &mut Clockwork, n: u64, from: Option<&Taken>, length_ms: u64) -> Result<u64, String> {
    let (done, ms) = from.map_or((0, 0), |t| (t.tick, t.ms));
    if n <= done + 1 {
        return Ok(ms);
    }
    let ran = clockwork.ask("run", json!({"sync": "next", "n": n - 1 - done, "until_ms": length_ms}))?;
    if ran.get("sync").is_none() {
        return Err(format!("the session ends before tick {n}"));
    }
    Ok(ran["ms"].as_u64().unwrap_or(0))
}

/// One access: the bytes one instruction (or service, or device) read or wrote at once.
struct Access {
    cycle: u64,
    kind: String,
    at: String,
    value: Vec<u8>,
    old: Vec<u8>,
    by: Value,
    chain: Value,
}

impl Access {
    fn describe(&self, names: &Names) -> String {
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02X}")).collect::<String>();
        let was = if self.kind == "write" { format!(" (was {})", hex(&self.old)) } else { String::new() };
        let by = if let Some(at) = self.by["instruction"].as_str() {
            names.plain(at)
        } else if let Some(service) = self.by["service"].as_str() {
            format!("a host service ({service}h, AX={})", self.by["ax"].as_str().unwrap_or("?"))
        } else {
            "a device".to_owned()
        };
        let frames = self.chain.as_array().cloned().unwrap_or_default();
        let mut chain: Vec<String> = frames
            .iter()
            .take(CHAIN_SHOWN)
            .map(|f| {
                let entry = names.function(f["entry"].as_str().unwrap_or("?"));
                if f["interrupt"].as_bool().unwrap_or(false) { format!("interrupt {entry}") } else { entry }
            })
            .collect();
        if frames.len() > CHAIN_SHOWN {
            chain.push("…".into());
        }
        let chain = if chain.is_empty() { String::new() } else { format!(" in {}", chain.join(" < ")) };
        format!("{} {} = {}{was} by {by}{chain}", self.kind, names.data(&self.at), hex(&self.value))
    }
}

/// Byte hits as accesses: consecutive bytes the same thing touched in the same cycle.
fn group(hits: Vec<Value>) -> Vec<Access> {
    let mut accesses: Vec<Access> = Vec::new();
    let byte = |v: &Value| v.as_u64().unwrap_or(0) as u8;
    for hit in hits {
        let cycle = hit["cycle"].as_u64().unwrap_or(0);
        let kind = hit["kind"].as_str().unwrap_or("?").to_owned();
        if let Some(last) = accesses.last_mut()
            && last.cycle == cycle
            && last.kind == kind
            && last.by == hit["by"]
        {
            last.value.push(byte(&hit["value"]));
            last.old.push(byte(&hit["old"]));
            continue;
        }
        accesses.push(Access {
            cycle,
            kind,
            at: hit["at"].as_str().unwrap_or("?").to_owned(),
            value: vec![byte(&hit["value"])],
            old: vec![byte(&hit["old"])],
            by: hit["by"].clone(),
            chain: hit["chain"].clone(),
        });
    }
    accesses
}

pub(crate) fn inputs(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--tick", "--ticks", "--at", "--len"], &[])?;
    let usage = "inputs SESSION [--tick N] [--ticks N] [--at ID|SEG:OFF --len N]";
    let [name] = a.positionals(1, 1, usage)? else { unreachable!() };
    let session = Session::load(repo, name)?;
    let key = snapshots::result_key(repo, &session, &[&["inputs".to_owned()], args].concat())?;
    snapshots::kept(repo, &key, || scan_inputs(repo, &a, &session))
}

fn scan_inputs(repo: &Repo, a: &Args, session: &Session) -> Result<ExitCode, String> {
    let k = Knowledge::load(&repo.knowledge())?;
    let syncs = sync_points(&k)?;
    if syncs.is_empty() {
        return Err("rrc inputs scans from a tick on: declare a sync point first (knowledge/ticks.toml)".into());
    }
    let (first, count) = tick_span(a)?;
    let first = first.max(2);
    let length_ms = (session.seconds * 1000.0) as u64;
    let (mut clockwork, loaded, from) = open(repo, session, &syncs, first)?;
    let ms = to_tick(&mut clockwork, first, from.as_ref(), length_ms)?;
    // The data segment the loop runs with, unless asked for another range.
    let (at, len) = match a.value("--at") {
        Some(at) => {
            let analysis = Analysis::load(repo)?;
            let id = crate::naming::resolve(repo, &k, analysis.as_ref(), at)?;
            let len: u32 = a.required("--len")?.parse().map_err(|_| "--len is a number of bytes")?;
            (image_address(&repo.module(), &id.to_string())?, len)
        }
        None => {
            let regs = clockwork.ask("regs", Value::Null)?;
            (format!("{:04X}:0000", regs["ds"].as_u64().unwrap_or(0)), 0x1_0000)
        }
    };
    clockwork.ask("watch", json!({"ranges": [{"at": at, "len": len}], "reads": true, "summary": true}))?;
    let ran = match count {
        Some(n) => clockwork.ask("run", json!({"sync": "next", "n": n, "max_ms": length_ms - ms}))?,
        None => clockwork.ask("run", json!({"ms": length_ms - ms}))?,
    };
    let last = ran["sync"]["hit"].as_u64().map_or_else(|| "the session's end".to_owned(), |hit| format!("tick {hit}"));
    let watched = clockwork.ask("watch_end", Value::Null)?;
    let bytes = watched["bytes"].as_array().cloned().unwrap_or_default();
    let vectors = interrupt_vectors(&mut clockwork)?;
    let analysis = Analysis::load(repo)?;
    let image = loaded["image"].as_str().and_then(|i| u32::from_str_radix(i.trim_start_matches('@'), 16).ok());
    let names = Names { k: &k, module: repo.module() };
    say("scanned", format!("{at}, {len} bytes, from tick {first} to {last}"));
    let candidates = candidates(&bytes);
    let mut writers: Vec<&Option<String>> = candidates.iter().map(|c| &c.writer).collect();
    writers.dedup();
    for writer in writers {
        let label = match writer {
            Some(entry) => {
                let linear = image.zip(names.id(entry)).map(|(base, id)| base + id.offset());
                let vector = linear.and_then(|l| vectors.iter().position(|&v| v == l));
                let what = vector.map_or_else(|| "an interrupt handler".to_owned(), interrupt_name);
                format!("written by {what}, {}", names.function(entry))
            }
            None => "written by host services".to_owned(),
        };
        say("writer", label);
        for c in candidates.iter().filter(|c| &c.writer == writer) {
            say("  input?", candidate_text(c, &names, &k, analysis.as_ref()));
        }
    }
    if candidates.is_empty() {
        say("none", "nothing the program read there was written by an interrupt handler or a host service");
    }
    for line in unseen(&watched) {
        say("unseen", line);
    }
    Ok(ExitCode::SUCCESS)
}

/// A candidate as a line: the named item it is in, or its bytes; who wrote them; and what Ghidra
/// says the writing instruction refers to (an array's start, for a store indexed by a key).
fn candidate_text(c: &Candidate, names: &Names, k: &Knowledge, analysis: Option<&Analysis>) -> String {
    let bytes = |n: u32| if n == 1 { "1 byte".to_owned() } else { format!("{n} bytes") };
    let id = names.id(&c.at);
    let item = id.as_ref().and_then(|id| {
        let (start, datum) = k.data.range(..=id.clone()).next_back()?;
        let size = datum.size?;
        (start.module() == id.module() && id.offset() + c.len <= start.offset() + size)
            .then(|| format!("{start} {} ({})", datum.decision.name, bytes(size)))
    });
    let place = id.as_ref().map_or_else(|| c.at.clone(), ToString::to_string);
    let mut text = match item {
        Some(item) => format!("{item}: {} written", bytes(c.touched)),
        None if c.touched == c.len => format!("{place}, {}", bytes(c.len)),
        None => format!("{place}, {} of the {} from here written", c.touched, bytes(c.len)),
    };
    if let Some(by) = c.by.as_ref().and_then(|by| names.id(by)) {
        text += &format!(" by {by}");
        let refers: Vec<String> = analysis
            .into_iter()
            .flat_map(|a| a.refs.iter())
            .filter(|(from, _, _)| *from == by)
            .map(|(_, to, _)| names.data(&format!("+{:05X}", to.offset())))
            .collect();
        if !refers.is_empty() {
            text += &format!(" (Ghidra: it refers to {})", refers.join(", "));
        }
    }
    if c.also_written {
        text += "; the program writes it too";
    }
    text
}

/// What the scan can't see: input that comes back in registers from a service or a port.
fn unseen(watched: &Value) -> Vec<String> {
    let count = |kind: &str, key: &str| watched[kind][key].as_u64().unwrap_or(0);
    let mut lines = Vec::new();
    let sources = [
        ("services", "33", "INT 33h (the mouse driver)", "declare it as device = \"mouse\""),
        ("ports", "201", "port 201h (the joystick)", "declare it as device = \"joystick\""),
        ("services", "16", "INT 16h (the BIOS keyboard)", "look for where the program keeps the keys"),
    ];
    for (kind, key, what, then) in sources {
        let n = count(kind, key);
        if n > 0 {
            let verb = if kind == "ports" { "read" } else { "called" };
            lines.push(format!("{what} {verb} {n} times: what it returns stays in registers, unscanned; {then}"));
        }
    }
    lines
}

/// The real-mode interrupt vectors, as linear addresses.
fn interrupt_vectors(clockwork: &mut Clockwork) -> Result<Vec<u32>, String> {
    let read = clockwork.ask("read", json!({"ranges": [{"at": "@0", "len": 1024}]}))?;
    let hex = read["data"][0].as_str().unwrap_or_default();
    let bytes: Vec<u8> =
        (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect();
    Ok(bytes
        .chunks(4)
        .map(|v| {
            let word = |i: usize| u32::from(u16::from_le_bytes([v[i], v[i + 1]]));
            (word(2) << 4) + word(0)
        })
        .collect())
}

fn interrupt_name(vector: usize) -> String {
    match vector {
        0x08 => "INT 08h (the timer, IRQ 0)".into(),
        0x09 => "INT 09h (the keyboard, IRQ 1)".into(),
        0x1C => "INT 1Ch (the timer's hook)".into(),
        0x0A..=0x0F => format!("INT {vector:02X}h (IRQ {})", vector - 8),
        0x70..=0x77 => format!("INT {vector:02X}h (IRQ {})", vector - 0x68),
        _ => format!("INT {vector:02X}h"),
    }
}

/// Memory the program reads that an interrupt handler or a host service writes.
#[derive(Debug, Clone, PartialEq)]
struct Candidate {
    /// Its first byte.
    at: String,
    /// From the first byte to the last.
    len: u32,
    /// The bytes written in that span.
    touched: u32,
    /// The interrupt handler's entry; `None`: a host service.
    writer: Option<String>,
    /// The handler's instruction that wrote it.
    by: Option<String>,
    /// The program's own code writes it too (clearing a key-state array, adjusting a counter).
    also_written: bool,
}

/// The candidates among a summary watch's runs, by writer. A handler's bytes are grouped by the
/// instruction that wrote them, so the elements of an array one store fills are one candidate
/// however far apart (the keys pressed); other runs of the same writer up to 16 bytes apart are
/// joined.
fn candidates(runs: &[Value]) -> Vec<Candidate> {
    let mut out: Vec<(u32, Candidate)> = Vec::new();
    for run in runs {
        let writer = run["interrupt_writer"].as_str().map(str::to_owned);
        if !run["read"].as_bool().unwrap_or(false) || (writer.is_none() && !run["host"].as_bool().unwrap_or(false)) {
            continue;
        }
        let Some((start, prefix)) = address(run["at"].as_str().unwrap_or("")) else { continue };
        let len = run["len"].as_u64().unwrap_or(0) as u32;
        let written = run["written"].as_bool().unwrap_or(false);
        let by = run["interrupt_write_by"].as_str().map(str::to_owned);
        let group = out.iter_mut().rev().find(|(s, c)| {
            c.writer == writer && if by.is_some() { c.by == by } else { c.by.is_none() && start <= s + c.len + 16 }
        });
        match group {
            Some((s, c)) => {
                c.len = start + len - *s;
                c.touched += len;
                c.also_written |= written;
            }
            None => {
                let at = format!("{prefix}{start:05X}");
                out.push((start, Candidate { at, len, touched: len, writer, by, also_written: written }));
            }
        }
    }
    out.sort_by(|a, b| (&a.1.writer, a.0).cmp(&(&b.1.writer, b.0)));
    out.into_iter().map(|(_, c)| c).collect()
}

/// A Clockwork address's number and its prefix (`+`, `@` or `%`).
fn address(text: &str) -> Option<(u32, String)> {
    let split = text.find(|c: char| c.is_ascii_hexdigit())?;
    let (prefix, number) = text.split_at(split);
    Some((u32::from_str_radix(number, 16).ok()?, prefix.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_are_what_the_program_reads_and_the_outside_writes() {
        let run = |at: &str, len: u32, read: bool, written: bool, writer: Option<&str>| {
            json!({"at": at, "len": len, "read": read, "read_in_interrupt": false, "written": written,
                   "interrupt_writer": writer, "host": false})
        };
        let runs = [
            run("+40AA3", 1, true, false, Some("+0EB0D")),
            run("+40AA4", 3, true, false, None),
            run("+40AB0", 2, true, true, Some("+0EB0D")),
            run("+40D50", 4, true, true, Some("+0EA40")),
            run("+41000", 2, true, true, None),
            run("+42000", 2, false, false, Some("+0EA40")),
        ];
        let found = candidates(&runs);
        let candidate = |at: &str, len, touched, writer: &str, also_written| Candidate {
            at: at.into(),
            len,
            touched,
            writer: Some(writer.into()),
            by: None,
            also_written,
        };
        assert_eq!(
            found,
            [candidate("+40D50", 4, 4, "+0EA40", true), candidate("+40AA3", 15, 3, "+0EB0D", true)],
            "an array's elements joined; not what only the program writes, nor what it doesn't read"
        );
        // Bytes one instruction wrote, however far apart, are one candidate; another's aren't in it.
        let by = |at: &str, len: u32, instruction: &str| {
            json!({"at": at, "len": len, "read": true, "written": false, "interrupt_writer": "+0EB0D",
                   "interrupt_write_by": instruction, "host": false})
        };
        let runs = [by("+40B15", 2, "+0EB40"), by("+40B20", 1, "+0EB50"), by("+40B33", 12, "+0EB40")];
        let found = candidates(&runs);
        assert_eq!((found.len(), found[0].len, found[0].touched), (2, 0x2A, 14), "{found:?}");
        assert_eq!(found[0].by.as_deref(), Some("+0EB40"));
        assert_eq!(interrupt_name(9), "INT 09h (the keyboard, IRQ 1)");
    }
}
