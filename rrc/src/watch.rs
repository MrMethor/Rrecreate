//! Who touches memory, through Clockwork's watches (D8):
//!
//! - `rrc watch SESSION ID`: each write (and read) of an item during a session or some of its ticks,
//!   with the instruction and the call chain, named from the knowledge.
//! - `rrc inputs SESSION`: the boundary values' candidates: memory the program reads that interrupt
//!   handlers (the keyboard's, the timer's) or host services write. A summary watch over the data
//!   segment finds them without anyone knowing their names.
//! - `rrc ports SESSION`: the instructions that read and write each port (the display's retrace, a
//!   sound card), from the same summary watch.

use std::process::ExitCode;

use ids::Id;
use serde_json::{Value, json};

use crate::analysis::Analysis;
use crate::args::Args;
use crate::clockwork::{self, Address, Clockwork};
use crate::knowledge::{Knowledge, parse_id};
use crate::places::Places;
use crate::repo::Repo;
use crate::say;
use crate::session::Session;
use crate::snapshots::{self, Taken};
use crate::trace::{SyncPoint, begin, sync_points};

/// Calls shown of a chain, innermost first.
const CHAIN_SHOWN: usize = 3;
/// Lines of accesses shown.
const ACCESSES_SHOWN: usize = 40;
/// The BIOS's data area (256 bytes), which `rrc inputs` scans too.
const BIOS_DATA: u32 = 0x400;

/// Clockwork's addresses (`+OFF` in the image, `@LINEAR` elsewhere) with the knowledge's names.
pub(crate) struct Names<'a> {
    k: &'a Knowledge,
    places: Places<'a>,
}

impl<'a> Names<'a> {
    pub(crate) fn new(k: &'a Knowledge, repo: &'a Repo, session: &'a Session) -> Result<Names<'a>, String> {
        let places = Places::new(repo, session);
        places.objects()?;
        Ok(Names { k, places })
    }

    pub(crate) fn id(&self, address: &str) -> Option<Id> {
        self.places.id(address)
    }

    /// Whether `address` is the program's own ([`Places::in_program`]).
    pub(crate) fn in_program(&self, address: &str) -> bool {
        self.places.in_program(address)
    }

    /// A function's name, or its address.
    pub(crate) fn function(&self, address: &str) -> String {
        let id = self.id(address);
        id.as_ref().and_then(|id| self.k.functions.get(id)).map_or_else(|| self.plain(address), |d| d.name.clone())
    }

    /// The bytes `hex` at `address` as their named data's type reads them, if they are one primitive
    /// element or field of it (a `u16` whole, an element of a `u8[256]`, a structure's `i16`).
    fn typed(&self, address: &str, hex: &str) -> Option<String> {
        let (d, offset) = self.datum(address)?;
        let (_, leaf, 0) = crate::values::place(&d.ty, offset, &self.k.types)? else { return None };
        let width = crate::values::primitive(&leaf)?;
        (hex.len() == 2 * width as usize).then(|| crate::trace::value_text(Some(hex), &leaf))
    }

    /// The name of the data `len` bytes at `address` are (0: as long as they are): a data item's
    /// name, or the element or field inside one its type says (`enemies[1].hits`, `name[+N]` without
    /// a type or for part of an element, `name[+0]` its first bytes); else the address.
    pub(crate) fn data(&self, address: &str, len: u32) -> String {
        let Some(id) = self.id(address) else { return address.to_owned() };
        let Some((d, offset)) = self.datum(address) else { return id.to_string() };
        let name = &d.decision.name;
        if offset == 0 && (len == 0 || d.size.is_none_or(|size| len >= size)) {
            return name.clone();
        }
        match crate::values::place(&d.ty, offset, &self.k.types) {
            Some((path, _, 0)) if !path.is_empty() => format!("{name}{path}"),
            Some((path, _, inner)) if !path.is_empty() => format!("{name}{path}+{inner}"),
            Some((_, leaf, 0)) if len == 0 || crate::values::primitive(&leaf).is_none_or(|width| len >= width) => {
                name.clone()
            }
            _ => format!("{name}[+{offset}]"),
        }
    }

    /// The named data `address` is in, and the offset in it.
    fn datum(&self, address: &str) -> Option<(&crate::knowledge::Datum, u32)> {
        let id = self.id(address)?;
        let (start, d) =
            self.k.data.range(..=id.clone()).next_back().filter(|(start, _)| start.module() == id.module())?;
        let offset = id.offset() - start.offset();
        (offset == 0 || d.size.is_some_and(|size| offset < size)).then_some((d, offset))
    }

    /// An ID for an address in the image or outside it, the address otherwise (paged memory).
    pub(crate) fn plain(&self, address: &str) -> String {
        self.id(address).map_or_else(|| address.to_owned(), |id| id.to_string())
    }
}

pub(crate) fn watch(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    watched(repo, args).map(|_| ExitCode::SUCCESS)
}

/// The last write a watch saw: its instruction, and the call chain it was made in, as the functions'
/// IDs, innermost first.
#[derive(Debug, Default)]
pub(crate) struct Writer {
    pub instruction: Option<Id>,
    pub chain: Vec<Id>,
    /// When it wrote, and, if it copied the bytes (D85), where from (as Clockwork names it).
    pub cycle: u64,
    pub copied: Option<String>,
}

/// Does what `rrc watch` does; and says who wrote last (nothing, if nothing wrote), or with `--before
/// CYCLE` (for rrc's own use), last before that cycle. `--quiet` (rrc's own too) says nothing.
pub(crate) fn watched(repo: &Repo, args: &[String]) -> Result<Writer, String> {
    let a = Args::parse(
        args,
        &["--len", "--tick", "--ticks", "--limit", "--lines", "--before"],
        &["--reads", "--cycles", "--quiet"],
    )?;
    let say = |label: &str, text: &dyn std::fmt::Display| {
        if !a.has("--quiet") {
            say(label, text);
        }
    };
    let before: u64 = a.value("--before").map_or(Ok(u64::MAX), str::parse).map_err(|_| "--before is a cycle")?;
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
    let at = Places::new(repo, &session).address(&id.to_string())?;
    clockwork.ask("watch", json!({"ranges": [{"at": at, "len": len}], "reads": a.has("--reads"), "limit": limit}))?;
    // Tick by tick, keeping where each ends.
    let mut ends = Vec::new();
    if syncs.is_empty() {
        clockwork.ask("run", json!({"ms": length_ms}))?;
    } else {
        while ms < length_ms && count.is_none_or(|n| (ends.len() as u64) < n) {
            let ran = clockwork.ask("run", json!({"sync": "next", "max_ms": length_ms - ms}))?;
            ms = clockwork::number(&ran, "ms")?;
            if ran.get("sync").is_none() {
                break;
            }
            ends.push(clockwork::number(&ran, "cycles")?);
        }
    }
    let watched = clockwork.ask("watch_end", Value::Null)?;
    let names = Names::new(&k, repo, &session)?;
    let span = if syncs.is_empty() { "the whole session".to_owned() } else { ticks_text(first, ends.len() as u64) };
    say("watch", &format!("{} ({id}, {len} bytes), {span}", names.data(&at, len)));
    // Each tick's accesses, those alike counted once (with --cycles, each on its own line). What runs
    // before the first sync point hit (loading, unpacking, the first screens) is "start".
    let mut lines: Vec<(String, String, usize)> = Vec::new();
    let accesses = group(watched["hits"].as_array().cloned().unwrap_or_default());
    for access in &accesses {
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
        say(tick, &if *n > 1 { format!("{text} ({n} times)") } else { text.clone() });
    }
    if lines.len() > lines_shown {
        say("...", &format!("{} more lines (--lines N shows more)", lines.len() - lines_shown));
    }
    if lines.is_empty() {
        say(
            "none",
            &if a.has("--reads") { "nothing read or wrote it" } else { "nothing wrote it (--reads for reads too)" },
        );
    }
    let dropped = watched["dropped"].as_u64().unwrap_or(0);
    if dropped > 0 {
        say(
            "dropped",
            &format!("{dropped} accesses past the limit of {limit} (--limit N): the counts above stop there"),
        );
    }
    let last = accesses.iter().rev().find(|access| access.kind == "write" && access.cycle < before);
    let Some(last) = last else { return Ok(Writer::default()) };
    let frames = last.chain.as_array().cloned().unwrap_or_default();
    Ok(Writer {
        instruction: last.by["instruction"].as_str().and_then(|at| names.id(at)),
        chain: frames.iter().filter_map(|f| f["entry"].as_str().and_then(|entry| names.id(entry))).collect(),
        cycle: last.cycle,
        copied: last.from.clone(),
    })
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
pub(crate) fn open(
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
pub(crate) fn to_tick(clockwork: &mut Clockwork, n: u64, from: Option<&Taken>, length_ms: u64) -> Result<u64, String> {
    let (done, ms) = from.map_or((0, 0), |t| (t.tick, t.ms));
    if n <= done + 1 {
        return Ok(ms);
    }
    let ran = clockwork.ask("run", json!({"sync": "next", "n": n - 1 - done, "until_ms": length_ms}))?;
    if ran.get("sync").is_none() {
        return Err(format!("the session ends before tick {n}"));
    }
    clockwork::number(&ran, "ms")
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
    /// Where a write copied its bytes from (D85).
    from: Option<String>,
}

impl Access {
    fn describe(&self, names: &Names) -> String {
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02X}")).collect::<String>();
        // As the named data's type reads it, where it is one of its elements; else a byte, word or
        // doubleword as the number the instruction wrote (little-endian: bytes E9 57 are 57E9h), and
        // other lengths as bytes in memory order.
        let value = |bytes: &[u8]| {
            names.typed(&self.at, &hex(bytes)).unwrap_or_else(|| match bytes.len() {
                1 | 2 | 4 => format!("{}h", bytes.iter().rev().map(|b| format!("{b:02X}")).collect::<String>()),
                _ => bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "),
            })
        };
        let was = if self.kind == "write" { format!(" (was {})", value(&self.old)) } else { String::new() };
        let by = if let Some(at) = self.by["instruction"].as_str() {
            names.plain(at)
        } else if let Some(service) = self.by["service"].as_str() {
            format!("a host service ({service}h, AX={})", self.by["ax"].as_str().unwrap_or("?"))
        } else if self.by["edit"].as_bool() == Some(true) {
            "the session's edit".to_owned()
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
        let mut chain = if chain.is_empty() { String::new() } else { format!(" in {}", chain.join(" < ")) };
        if let Some(from) = &self.from {
            chain.push_str(&format!(", copied from {}", names.data(from, self.value.len() as u32)));
        }
        format!(
            "{} {} = {}{was} by {by}{chain}",
            self.kind,
            names.data(&self.at, self.value.len() as u32),
            value(&self.value)
        )
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
            from: hit["from"].as_str().map(str::to_owned),
        });
    }
    accesses
}

pub(crate) fn inputs(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--tick", "--ticks", "--at", "--len", "--from-ms", "--to-ms"], &[])?;
    let usage = "inputs SESSION [--tick N] [--ticks N | --from-ms MS [--to-ms MS]] [--at ID|SEG:OFF --len N]";
    let [name] = a.positionals(1, 1, usage)? else { unreachable!() };
    let session = Session::load(repo, name)?;
    let key = snapshots::result_key(repo, &session, &[&["inputs".to_owned()], args].concat())?;
    snapshots::kept(repo, &key, || scan_inputs(repo, &a, &session))
}

fn scan_inputs(repo: &Repo, a: &Args, session: &Session) -> Result<ExitCode, String> {
    let k = Knowledge::load(&repo.knowledge())?;
    let syncs = sync_points(&k)?;
    let length_ms = (session.seconds * 1000.0) as u64;
    let span_ms = ms_span(a, length_ms)?;
    if span_ms.is_none() && syncs.is_empty() {
        return Err("rrc inputs scans from a tick on: declare a sync point first (knowledge/ticks.toml), or scan \
                    a time: --from-ms MS"
            .into());
    }
    let (first, count) = tick_span(a)?;
    let first = first.max(2);
    let (mut clockwork, loaded, ms) = match span_ms {
        Some((from, _)) => {
            let (mut clockwork, loaded, _) = begin(repo, session, &syncs)?;
            clockwork.ask("run", json!({ "until_ms": from }))?;
            (clockwork, loaded, from)
        }
        None => {
            let (mut clockwork, loaded, from) = open(repo, session, &syncs, first)?;
            let ms = to_tick(&mut clockwork, first, from.as_ref(), length_ms)?;
            (clockwork, loaded, ms)
        }
    };
    // The data segment the loop runs with (an LE program's data objects) and the BIOS's data (its
    // timer count, the keyboard's flags and buffer: games read them), unless asked for another range.
    let bios = (format!("@{BIOS_DATA:X}"), 0x100);
    let ranges: Vec<(String, u32)> = match a.value("--at") {
        Some(at) => {
            let analysis = Analysis::load(repo)?;
            let id = crate::naming::resolve(repo, &k, analysis.as_ref(), at)?;
            let len: u32 = a.required("--len")?.parse().map_err(|_| "--len is a number of bytes")?;
            vec![(Places::new(repo, session).address(&id.to_string())?, len)]
        }
        None if crate::places::is_le(repo) => {
            let objects = Places::new(repo, session).objects()?.to_vec();
            let data = objects.iter().filter(|&&(_, _, flags)| flags & 4 == 0);
            data.map(|&(base, size, _)| (format!("@{base:X}"), size)).chain([bios]).collect()
        }
        None => {
            // DS's segment, as an offset in the image when it is in it (so the output names it by ID).
            let regs = clockwork.ask("regs", Value::Null)?;
            let ds = clockwork::number(&regs, "ds")? as u32;
            let data = match (ds * 16).checked_sub(clockwork::linear(&loaded, "image")?) {
                Some(offset) => (format!("+{offset:05X}"), 0x1_0000),
                None => (format!("{ds:04X}:0000"), 0x1_0000),
            };
            vec![data, bios]
        }
    };
    let request: Vec<Value> = ranges.iter().map(|(at, len)| json!({"at": at, "len": len})).collect();
    clockwork.ask("watch", json!({"ranges": request, "reads": true, "summary": true}))?;
    let ran = match (span_ms, count) {
        (Some((_, to)), _) => clockwork.ask("run", json!({ "until_ms": to }))?,
        (None, Some(n)) => clockwork.ask("run", json!({"sync": "next", "n": n, "max_ms": length_ms - ms}))?,
        (None, None) => clockwork.ask("run", json!({"ms": length_ms - ms}))?,
    };
    let (start, last) = match span_ms {
        Some((from, to)) => (format!("{from} ms"), format!("{to} ms")),
        None => (
            format!("tick {first}"),
            ran["sync"]["hit"].as_u64().map_or_else(|| "the session's end".to_owned(), |hit| format!("tick {hit}")),
        ),
    };
    let watched = clockwork.ask("watch_end", Value::Null)?;
    let bytes = watched["bytes"].as_array().cloned().unwrap_or_default();
    let vectors = interrupt_vectors(&mut clockwork)?;
    let analysis = Analysis::load(repo)?;
    let image = clockwork::linear(&loaded, "image")?;
    let names = Names::new(&k, repo, session)?;
    let scanned: Vec<String> = ranges.iter().map(|(at, len)| format!("{}, {len} bytes", names.plain(at))).collect();
    say("scanned", format!("{}, from {start} to {last}", scanned.join("; ")));
    let candidates = candidates(&bytes);
    let mut writers: Vec<&Option<String>> = candidates.iter().map(|c| &c.writer).collect();
    writers.dedup();
    for writer in writers {
        let label = match writer {
            Some(entry) => {
                let linear = Address::parse(entry).and_then(|a| a.linear(image));
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
    // A declared input it is in, else named data.
    let within = |id: &Id, start: &Id, size: u32| {
        start.module() == id.module() && start.offset() <= id.offset() && id.offset() + c.len <= start.offset() + size
    };
    let input = id.as_ref().and_then(|id| {
        let i = k.ticks.inputs.iter().find(|i| parse_id(&i.at).is_ok_and(|start| within(id, &start, i.size)))?;
        Some(format!("{} {} (an input, {})", i.at, i.name, bytes(i.size)))
    });
    let item = input.or_else(|| {
        let id = id.as_ref()?;
        let (start, datum) = k.data.range(..=id.clone()).next_back()?;
        let size = datum.size?;
        within(id, start, size).then(|| format!("{start} {} ({})", datum.decision.name, bytes(size)))
    });
    let place = id.as_ref().map_or_else(|| c.at.clone(), ToString::to_string);
    let mut text = match item {
        Some(item) => format!("{item}: {} written", bytes(c.touched)),
        None if c.touched == c.len => format!("{place}, {}", bytes(c.len)),
        None => format!("{place}, {} of the {} from here written", c.touched, bytes(c.len)),
    };
    if let Some(by) = c.by.as_ref().and_then(|by| names.id(by)) {
        text += &format!(" by {by}");
        // Where Ghidra's reference can be what was written: the array's start, at most a segment before.
        // A reference elsewhere is Ghidra's guess of a segment register the handler sets itself.
        let written = id.as_ref();
        let near = |to: &Id| {
            written.is_some_and(|w| {
                to.module() == w.module()
                    && to.part() == w.part()
                    && to.offset() <= w.offset()
                    && w.offset() - to.offset() < 0x1_0000
            })
        };
        let refers: Vec<String> = analysis
            .into_iter()
            .flat_map(|a| a.refs.iter())
            .filter(|(from, to, _)| *from == by && near(to))
            .map(|(_, to, _)| names.data(&format!("+{:05X}", to.offset()), 0))
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
        ("ports", "201", "port 201h (the joystick)", "declare it as device = \"joystick\" (rrc ports finds where)"),
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

/// `--from-ms MS [--to-ms MS]`: a span of the session in ms, if given (to its end by default).
fn ms_span(a: &Args, length_ms: u64) -> Result<Option<(u64, u64)>, String> {
    let ms =
        |key: &str| a.value(key).map(|v| v.parse::<u64>().map_err(|_| format!("{key} is a number of ms"))).transpose();
    let (from, to) = (ms("--from-ms")?, ms("--to-ms")?);
    let Some(from) = from else {
        return match to {
            Some(_) => Err("--to-ms goes with --from-ms".into()),
            None => Ok(None),
        };
    };
    if a.value("--tick").is_some() || a.value("--ticks").is_some() {
        return Err("a span of ticks or of ms, not both".into());
    }
    let to = to.unwrap_or(length_ms).min(length_ms);
    if from >= to {
        return Err(format!("nothing from {from} ms to {to} ms (the session is {length_ms} ms)"));
    }
    Ok(Some((from, to)))
}

/// `rrc ports SESSION [PORT,...] [--tick N] [--ticks N]`.
pub(crate) fn ports(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--tick", "--ticks"], &[])?;
    let positionals = a.positionals(1, 2, "ports SESSION [PORT,...] [--tick N] [--ticks N]")?;
    let session = Session::load(repo, &positionals[0])?;
    let mut wanted = Vec::new();
    for port in positionals.get(1).into_iter().flat_map(|p| p.split(',')) {
        let hex = port.trim().trim_end_matches(['h', 'H']);
        wanted.push(u16::from_str_radix(hex, 16).map_err(|_| format!("{port:?}: a port in hex (3DA)"))?);
    }
    let key = snapshots::result_key(repo, &session, &[&["ports".to_owned()], args].concat())?;
    snapshots::kept(repo, &key, || scan_ports(repo, &a, &session, &wanted))
}

/// Instructions listed per port and kind of use.
const USES_SHOWN: usize = 6;

fn scan_ports(repo: &Repo, a: &Args, session: &Session, wanted: &[u16]) -> Result<ExitCode, String> {
    let k = Knowledge::load(&repo.knowledge())?;
    let syncs = sync_points(&k)?;
    if syncs.is_empty() && a.value("--tick").is_some() {
        return Err("--tick needs sync points (knowledge/ticks.toml)".into());
    }
    let (first, count) = tick_span(a)?;
    let length_ms = (session.seconds * 1000.0) as u64;
    let (mut clockwork, _, from) = open(repo, session, &syncs, first)?;
    let ms = to_tick(&mut clockwork, first, from.as_ref(), length_ms)?;
    clockwork.ask("watch", json!({"ranges": [], "summary": true}))?;
    let ran = match count {
        Some(n) if !syncs.is_empty() => {
            clockwork.ask("run", json!({"sync": "next", "n": n, "max_ms": length_ms - ms}))?
        }
        _ => clockwork.ask("run", json!({"ms": length_ms - ms}))?,
    };
    let watched = clockwork.ask("watch_end", Value::Null)?;
    let names = Names::new(&k, repo, session)?;
    let start = if syncs.is_empty() || first == 1 { "the start".to_owned() } else { format!("tick {first}") };
    let end = ran["sync"]["hit"].as_u64().map_or_else(|| "the session's end".to_owned(), |hit| format!("tick {hit}"));
    say("scanned", format!("from {start} to {end}"));
    // By port: the program's reads and writes by instruction, and interrupt handlers' counted apart.
    struct Uses {
        reads: Vec<(String, u64)>,
        writes: Vec<(String, u64)>,
        in_interrupts: (u64, u64),
    }
    let mut ports: std::collections::BTreeMap<u16, Uses> = std::collections::BTreeMap::new();
    for u in watched["port_uses"].as_array().into_iter().flatten() {
        let Some(port) = u["port"].as_str().and_then(|p| u16::from_str_radix(p, 16).ok()) else { continue };
        if !wanted.is_empty() && !wanted.contains(&port) {
            continue;
        }
        let n = u["n"].as_u64().unwrap_or(0);
        let write = u["write"].as_bool().unwrap_or(false);
        let entry = ports.entry(port).or_insert(Uses { reads: Vec::new(), writes: Vec::new(), in_interrupts: (0, 0) });
        if u["interrupt"].as_bool().unwrap_or(false) {
            if write { entry.in_interrupts.1 += n } else { entry.in_interrupts.0 += n }
        } else {
            let at = names.plain(u["at"].as_str().unwrap_or("?"));
            if write { entry.writes.push((at, n)) } else { entry.reads.push((at, n)) }
        }
    }
    let listed = |uses: &mut Vec<(String, u64)>| {
        uses.sort_by_key(|(at, n)| (std::cmp::Reverse(*n), at.clone()));
        let mut list: Vec<String> = uses.iter().take(USES_SHOWN).map(|(at, n)| format!("{at} ({n})")).collect();
        if uses.len() > USES_SHOWN {
            list.push(format!("{} more", uses.len() - USES_SHOWN));
        }
        list.join(", ")
    };
    for (port, mut uses) in ports {
        let mut parts = Vec::new();
        if !uses.reads.is_empty() {
            parts.push(format!("read by {}", listed(&mut uses.reads)));
        }
        if !uses.writes.is_empty() {
            parts.push(format!("written by {}", listed(&mut uses.writes)));
        }
        match uses.in_interrupts {
            (0, 0) => {}
            (r, w) => parts.push(format!("in interrupt handlers: {r} reads, {w} writes")),
        }
        say(&format!("{port:X}h"), parts.join("; "));
    }
    if watched["port_uses"].as_array().is_none_or(|u| u.is_empty()) {
        say("none", "no port was read or written");
    }
    Ok(ExitCode::SUCCESS)
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
        let Some(at) = run["at"].as_str().and_then(Address::parse) else { continue };
        let start = at.number();
        let len = run["len"].as_u64().unwrap_or(0) as u32;
        let written = run["written"].as_bool().unwrap_or(false);
        let by = run["interrupt_write_by"].as_str().map(str::to_owned);
        // Runs join only addresses of one kind (offsets in the image, linear, physical).
        let same_kind = |c: &Candidate| Address::parse(&c.at).is_some_and(|a| a.with(0) == at.with(0));
        let group = out.iter_mut().rev().find(|(s, c)| {
            c.writer == writer
                && same_kind(c)
                && if by.is_some() { c.by == by } else { c.by.is_none() && start <= s + c.len + 16 }
        });
        match group {
            Some((s, c)) => {
                c.len = start + len - *s;
                c.touched += len;
                c.also_written |= written;
            }
            None => {
                let at = at.with(start).to_string();
                out.push((start, Candidate { at, len, touched: len, writer, by, also_written: written }));
            }
        }
    }
    out.sort_by(|a, b| (&a.1.writer, a.0).cmp(&(&b.1.writer, b.0)));
    out.into_iter().map(|(_, c)| c).collect()
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
