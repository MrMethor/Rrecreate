//! Traces (docs/design.md §9, D54): a session run through Clockwork, read at every tick. A tick ends when
//! the original reaches a sync point; there the trace takes the boundary values and the named data
//! that changed since they were last read, and the frames and sound events of the tick.
//!
//! The file is JSON lines (`docs/formats.md`): a header naming the items, a line per tick, and a
//! last line saying how the run ended, with what it did after the last tick. Values are the
//! original's bytes in hex, as memory holds them.
//!
//! The run's hash covers what the machine did (frames, sound, how and when it ended, its state
//! then), not what the knowledge asked to read: the same run traced with other names, items or sync
//! points has the same run hash and another trace hash.

use std::collections::BTreeMap;
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::args::Args;
use crate::cache::Cache;
use crate::clockwork::{self, Clockwork};
use crate::knowledge::Knowledge;
use crate::places::Places;
use crate::repo::Repo;
use crate::say;
use crate::session::{Recorded, Session};
use crate::values::{PointerKind, Target};

pub const FORMAT: &str = "rrc-trace";
pub const VERSION: u32 = 12;
/// The oldest version this rrc reads: version 6 only added items read through pointers (D72).
const OLDEST: u32 = 5;
/// The first version whose run hash is the one [`Trace::run_hash`] computes.
const RUN_HASH_SINCE: u32 = 3;

/// A memory item a trace follows, or an input device's state.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Item {
    pub name: String,
    /// Its ID; or
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at: String,
    /// the device (D49): `mouse` or `joystick`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device: String,
    pub size: u32,
    /// An input read only at these sync points (else at all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sync: Vec<String>,
    /// An item read where a pointer points at each tick (D72), not at `at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<ItemVia>,
    /// An input a part of the program not ported writes (D96), and the instructions whose reads of it
    /// are kept (none: all).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub part: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_at: Vec<String>,
}

/// Where an item read through a pointer is: the pointer's ID and how it points, and how far past
/// where it points.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ItemVia {
    pub at: String,
    /// `near` (an offset of `size` bytes from `base`), `far` (offset, segment: 4 bytes) or `segment`
    /// (a segment alone: 2 bytes).
    pub pointer: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base: String,
    pub size: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: u32,
    /// The pointers found through it one after another (D122).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub then: Vec<ItemStep>,
    /// A list's (D122): where in a node its pointer to the next is, and how many nodes at most; the item's
    /// value is its nodes one after another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<(u32, u32)>,
}

/// A pointer found through another (D122): how far past where the last points it is, and how it points.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ItemStep {
    pub offset: u32,
    pub pointer: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base: String,
    pub size: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// How a pointer points, as a trace writes it: `near` (and its base), `far` or `segment`.
fn pointer_text(p: &PointerKind) -> (&'static str, String) {
    match p {
        PointerKind::Near { base, .. } => ("near", base.to_string()),
        PointerKind::Far => ("far", String::new()),
        PointerKind::Segment => ("segment", String::new()),
    }
}

fn pointer_kind(pointer: &str, base: &str, size: u32) -> Result<PointerKind, String> {
    match pointer {
        "near" => Ok(PointerKind::Near { base: crate::knowledge::parse_id(base)?, size }),
        "far" => Ok(PointerKind::Far),
        "segment" => Ok(PointerKind::Segment),
        other => Err(format!("a trace's pointer is near or far, not {other:?}")),
    }
}

impl ItemVia {
    fn of(m: &crate::mapping::Mapped, via: &crate::mapping::Via) -> ItemVia {
        let (pointer, base) = pointer_text(&via.pointer);
        let then = via
            .then
            .iter()
            .map(|(offset, p)| {
                let (pointer, base) = pointer_text(p);
                ItemStep { offset: *offset, pointer: pointer.into(), base, size: p.size() }
            })
            .collect();
        ItemVia {
            at: m.id.to_string(),
            pointer: pointer.into(),
            base,
            size: via.pointer.size(),
            offset: via.offset,
            then,
            next: via.next,
        }
    }

    pub fn kind(&self) -> Result<PointerKind, String> {
        pointer_kind(&self.pointer, &self.base, self.size)
    }

    /// The pointers found through it, with how far past where the last points each is.
    fn steps(&self) -> Result<Steps, String> {
        self.then.iter().map(|s| Ok((s.offset, pointer_kind(&s.pointer, &s.base, s.size)?))).collect()
    }
}

impl Item {
    fn from_input(i: &crate::knowledge::Input) -> Item {
        let (name, at, device) = (i.name.clone(), i.at.clone(), i.device.clone());
        let (part, read_at) = (i.part.clone(), i.read_at.clone());
        Item { name, at, device, size: i.bytes(), sync: i.sync.clone(), via: None, part, read_at }
    }

    /// Whether an input is read at sync point `sync`.
    pub fn read_at(&self, sync: &str) -> bool {
        self.sync.is_empty() || self.sync.iter().any(|s| s == sync)
    }

    /// What Clockwork's `read` and a gate take for it.
    fn request(&self, places: &Places) -> Result<Value, String> {
        if !self.part.is_empty() {
            let read_at: Vec<String> = self.read_at.iter().map(|id| places.address(id)).collect::<Result<_, _>>()?;
            Ok(json!({"at": places.address(&self.at)?, "len": self.size, "follow_writes": false, "read_at": read_at}))
        } else if self.device.is_empty() {
            Ok(json!({"at": places.address(&self.at)?, "len": self.size}))
        } else {
            Ok(json!({ "device": self.device }))
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SyncPoint {
    pub name: String,
    pub at: String,
    /// Its gate (D48): a hit is a tick only when one of these changed since its last tick.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when_changed: Vec<Item>,
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
    /// Boundary values: a tick has those it reads that changed since they were last read.
    pub inputs: Vec<Item>,
    /// Named data (but the inputs): a tick has those that changed since the tick before (the first,
    /// all).
    pub state: Vec<Item>,
    /// The scenario's edits (D64): `{"tick", "at" (an ID), "hex", "edit" (as the session has it)}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<Value>,
    /// Calls into ported code from a part not ported (D98): `{"name", "at", "registers"}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookItem>,
    /// The start-up's pass, from power-on to the first tick (D134): tick 0, its sync point `power-on`, every input
    /// as it was at power-on and the reads that saw one otherwise. None in traces before version 12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_on: Option<Box<Tick>>,
}

/// A hook a trace follows (D98): its name, its entry's ID, the registers it takes, and whether its runs from
/// interrupt handlers count too (D174).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HookItem {
    pub name: String,
    pub at: String,
    pub registers: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interrupts: bool,
    /// The memory each call keeps (D235): `SEGMENT:OFFSET:LENGTH`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub memory: String,
}

/// A register's value in Clockwork's `entries_end` answer: `ax` is `eax`'s low word, `ah` its second byte.
fn register(regs: &Value, name: &str) -> Option<u32> {
    let full = |n: &str| regs[n].as_u64().map(|v| v as u32);
    let word = |n: &str| full(n).map(|v| v & 0xFFFF);
    Some(match name {
        "ax" | "bx" | "cx" | "dx" | "si" | "di" | "bp" | "sp" => word(&format!("e{name}"))?,
        "al" | "bl" | "cl" | "dl" => full(&format!("e{}x", &name[..1]))? & 0xFF,
        "ah" | "bh" | "ch" | "dh" => full(&format!("e{}x", &name[..1]))? >> 8 & 0xFF,
        "eflags" => full("flags")?,
        "ip" => full("eip")? & 0xFFFF,
        other => full(other)?,
    })
}

/// A read of a tick's pass that saw an input otherwise than the tick has, or than the same instruction's
/// read before it in the pass (D93): `[input, ID, value, read, bytes, round]`. Version 5 and 6 traces have no
/// `read`: one value an instruction, its first read's; versions before 8 no `bytes`; before 11 no `round` (nor
/// does a read in a pass's first round have one).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "Vec<Value>", into = "Vec<Value>")]
pub struct LateRead {
    /// The input (its index in the header's).
    pub input: usize,
    /// The instruction that read it (for a driver, the one its call returns to).
    pub at: String,
    /// What it read: the tick's value with the bytes that read saw (none: memory not mapped).
    pub value: Option<String>,
    /// Which of that instruction's reads of the input in its round of the pass it was, from 1.
    pub read: u32,
    /// The bytes it saw that differ from what the program should have read there, `[offset, value]`: the
    /// tick's value with the program's own writes since (D97); none in traces before version 8.
    pub bytes: Option<Vec<(u32, u8)>>,
    /// The round of the pass it was in: the hits of a gated wait the pass went round before it, none counted
    /// (D130). Its reads are numbered within it.
    pub round: u32,
}

impl TryFrom<Vec<Value>> for LateRead {
    type Error = String;

    fn try_from(v: Vec<Value>) -> Result<LateRead, String> {
        let wrong = || format!("a late read is [input, ID, value, read]: {v:?}");
        let input = v.first().and_then(Value::as_u64).ok_or_else(wrong)? as usize;
        let at = v.get(1).and_then(Value::as_str).ok_or_else(wrong)?.to_owned();
        let value = v.get(2).and_then(Value::as_str).map(str::to_owned);
        let read = v.get(3).map_or(Some(1), Value::as_u64).ok_or_else(wrong)? as u32;
        let bytes = match v.get(4).filter(|list| !list.is_null()) {
            Some(list) => Some(byte_pairs(list).ok_or_else(wrong)?),
            None => None,
        };
        let round = v.get(5).map_or(Some(0), Value::as_u64).ok_or_else(wrong)? as u32;
        Ok(LateRead { input, at, value, read, bytes, round })
    }
}

impl From<LateRead> for Vec<Value> {
    fn from(l: LateRead) -> Vec<Value> {
        let mut v = vec![json!(l.input), json!(l.at), json!(l.value), json!(l.read)];
        if l.bytes.is_some() || l.round != 0 {
            v.push(json!(l.bytes));
        }
        if l.round != 0 {
            v.push(json!(l.round));
        }
        v
    }
}

/// `[[offset, value], ...]`.
pub fn byte_pairs(list: &Value) -> Option<Vec<(u32, u8)>> {
    list.as_array()?
        .iter()
        .map(|pair| Some((u32::try_from(pair.get(0)?.as_u64()?).ok()?, u8::try_from(pair.get(1)?.as_u64()?).ok()?)))
        .collect()
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tick {
    /// From 1.
    pub tick: u64,
    /// The sync point that ended it.
    pub sync: String,
    pub cycle: u64,
    /// At a gated sync point: the hits its gate passed over since its last tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<u64>,
    /// (index in the header's `inputs`, value) for each input the tick reads whose value changed
    /// since it was last read (null where memory isn't mapped).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<(usize, Option<String>)>,
    /// (index in the header's `state`, value) for each item that changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state: Vec<(usize, Option<String>)>,
    /// The reads of the tick's pass that saw an input otherwise than the tick has, or than the same
    /// instruction's read before (D60, D68, D93).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub late: Vec<LateRead>,
    /// The hooks' calls in the tick's pass, in order (D98): (index in the header's `hooks`, registers).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<(usize, Vec<u32>)>,
    /// The memory the calls whose hook keeps some kept (D235): (index in the tick's `hooks`, the bytes in hex).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hook_memory: Vec<(usize, String)>,
    /// The frames shown during the tick: [number, cycle, video mode, fingerprint, picture (its hash in
    /// the cache)].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<Value>,
    /// Sound events during the tick, by device (only those with events).
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub sound: serde_json::Map<String, Value>,
    /// The screen at the sync point (its picture's hash in the cache), when it changed since the tick
    /// before (D107; version 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct End {
    /// Why the run stopped.
    pub end: String,
    pub cycles: u64,
    /// The frames and sound after the last tick, as a tick has them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<Value>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub sound: serde_json::Map<String, Value>,
    /// Clockwork's hash of the machine's state then.
    pub state_hash: String,
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
        if header.format != FORMAT || !(OLDEST..=VERSION).contains(&header.version) {
            return Err(format!(
                "not a trace this rrc reads ({} {}; it reads versions {OLDEST} to {VERSION}): rrc record again",
                header.format, header.version
            ));
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
        carried(&self.ticks, index, |tick| &tick.state)
    }

    /// The value of input `index` at each tick: the last value read (`None` before the first, or
    /// where memory isn't mapped).
    pub fn input_values(&self, index: usize) -> Vec<Option<String>> {
        carried(&self.ticks, index, |tick| &tick.inputs)
    }

    /// The run's hash: of the frames and sound from the start to the end, how and when the run
    /// ended and the machine's state then; not of what the knowledge asked to read.
    pub fn run_hash(&self) -> String {
        let mut frames = Vec::new();
        let mut sound: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let spans = self.ticks.iter().map(|t| (&t.frames, &t.sound)).chain([(&self.end.frames, &self.end.sound)]);
        for (f, s) in spans {
            frames.extend(f.iter().cloned());
            for (device, events) in s {
                sound.entry(device.clone()).or_default().extend(events.as_array().into_iter().flatten().cloned());
            }
        }
        // Keys in sorted order, as docs/formats.md gives the text that is hashed.
        let run = json!({"cycles": self.end.cycles, "end": self.end.end, "frames": frames, "sound": sound,
                         "state_hash": self.end.state_hash});
        crate::cache::sha256(run.to_string().as_bytes())
    }
}

/// An item's value at each tick, from the ticks' changes (`changes` picks a tick's list).
fn carried(
    ticks: &[Tick],
    index: usize,
    changes: impl Fn(&Tick) -> &Vec<(usize, Option<String>)>,
) -> Vec<Option<String>> {
    let mut value = None;
    ticks
        .iter()
        .map(|tick| {
            if let Some((_, v)) = changes(tick).iter().find(|(i, _)| *i == index) {
                value = v.clone();
            }
            value.clone()
        })
        .collect()
}

pub(crate) fn record(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &[], &["--run", "--folder", "--rebase", "--follow"])?;
    let usage = "record SESSION [--run] [--folder] [--rebase] [--follow]";
    let [name] = a.positionals(1, 1, usage)? else { unreachable!() };
    let mut session = Session::load(repo, name)?;
    // The game's folder it runs on (D170): kept at its first recording, and taken again when --folder asks.
    match &session.from {
        Some(from) if a.has("--folder") => {
            let mut root = from.session.clone();
            while let Some(base) = Session::load(repo, &root)?.from {
                root = base.session;
            }
            return Err(format!(
                "{name} runs on the folder of the session its bases start from: rrc record {root} --folder"
            ));
        }
        Some(_) => {}
        None if session.files.is_empty() || a.has("--folder") => {
            session.files = crate::folder::take(repo)?;
            let files = session.files.keys().filter(|p| !p.ends_with('/')).count();
            say("folder", format!("the game's folder as it is now, kept ({files} files): every run of {name} sees it"));
            session.save(repo, name)?;
        }
        None => {
            if let Ok(changed) = crate::folder::differences(repo, &session.files)
                && !changed.is_empty()
            {
                let more = if changed.len() > 3 { format!(" and {} more", changed.len() - 3) } else { String::new() };
                say(
                    "folder",
                    format!(
                        "as recorded; the game's folder differs now in {}{more}, which {name} doesn't see (--folder takes it as it is)",
                        changed[..changed.len().min(3)].join(", ")
                    ),
                );
            }
        }
    }
    // A based session's base tick, by its cycle (D128): its number now; by its number when the base's run changed.
    if a.has("--rebase") {
        let from = session.from.as_mut().ok_or(format!("{name} isn't based on another session: nothing to rebase"))?;
        from.cycle = None;
    }
    if let Some(said) = crate::snapshots::settle(repo, &mut session)? {
        say("from", said);
        session.save(repo, name)?;
    }
    let k = Knowledge::load(&repo.knowledge())?;
    let exe = clockwork::find(&repo.local)?;
    let header = header(repo, &k, name, &exe, crate::edits::listed(&crate::edits::resolve(&k, &session.edits)?))?;
    // A play still being written, followed (D189): from its first mark, by the Clockwork that played it.
    let mut follow = match a.has("--follow") {
        true => Some(follow_play(repo, &session, &exe, false)?),
        false => None,
    };
    // The run asked for: the last recording's only if its input, options and length are the same (D123); a
    // followed play's once it is written.
    let identity = |session: &Session| identity(repo, session);
    let asked = match follow {
        Some(_) => String::new(),
        None => identity(&session)?,
    };
    let same_run = follow.is_none() && session.trace.as_ref().is_some_and(|t| t.identity == asked);
    // Nothing to add: the trace recorded before has every item (D111); `--run` runs it again as a check.
    if !a.has("--run")
        && same_run
        && let Some(before) = &session.trace
        && recorded_header(&Cache::open(&repo.local)?, &before.hash)
            .is_some_and(|h| Header { power_on: None, ..h } == header)
    {
        say(
            "kept",
            "the trace recorded before has every item: nothing recorded (--run runs the session again, which checks its run is the same)",
        );
        say("hash", &before.hash);
        say("run", &before.run);
        return Ok(ExitCode::SUCCESS);
    }
    // Only items added since the last recording, all in its kept memory: no run (D92).
    let kept = if a.has("--run") || follow.is_some() {
        Err(String::new())
    } else if let Some(before) = session.trace.as_ref().filter(|_| !same_run) {
        Err(match before.identity.is_empty() {
            true => "its last recording didn't keep what its run was asked to be".into(),
            false => "its input, options or length changed since its last recording".into(),
        })
    } else {
        from_kept(repo, &session, &header)?
    };
    let (trace, snapshots, memory) = match kept {
        Ok(trace) => (TraceFile::write(repo, &trace)?, None, session.trace.as_ref().and_then(|t| t.memory.clone())),
        Err(why) => {
            if !why.is_empty() {
                say("runs", format!("the session: {why}"));
            }
            let (trace, snapshots, memory) = run(repo, &session, header, follow.as_mut())?;
            (trace, Some(snapshots), memory)
        }
    };
    let cache = Cache::open(&repo.local)?;
    let (hash, run, ticks, late) = (trace.hash.clone(), trace.run.clone(), trace.ticks, trace.late.clone());
    let clockwork = trace.header.clockwork.clone();
    let identity = match follow {
        Some(_) => identity(&session)?,
        None => asked,
    };
    let recorded = Recorded { hash: hash.clone(), run: run.clone(), ticks, clockwork, memory, identity };
    let before = session.trace.replace(recorded);
    // Paired checkpoints are of the run they were taken in (D63).
    let pairs = session.pairs.len();
    if pairs > 0 && before.as_ref().is_none_or(|b| b.run != run) {
        session.pairs.clear();
    }
    session.save(repo, name)?;
    say("trace", format!("{ticks} ticks, {} KB: {}", trace.bytes / 1024, trace.end));
    if let Some(f) = &follow {
        let hashes = match f.checked {
            1 => "1 state hash".to_owned(),
            n => format!("{n} state hashes"),
        };
        say(
            "followed",
            format!("the play as it was written, to its {} marks; {hashes} of the window's, each the same", f.marks),
        );
    }
    if !session.edits.is_empty() {
        say("edits", format!("{}, the scenario's (rrc trace {name} lists them)", session.edits.len()));
    }
    if let Some(late) = late {
        say("late", late);
    }
    match snapshots {
        Some(snapshots) => say(
            "snapshots",
            format!(
                "{snapshots}, one every {} s, for rrc watch and rrc inputs",
                crate::snapshots::spacing_ms() as f64 / 1000.0
            ),
        ),
        None => say("from", "the run recorded before and the memory it kept: no emulator (--run runs the session)"),
    }
    match before {
        Some(b) if b.hash == hash => say("same", "as the trace recorded before"),
        Some(b) if b.run == run => {
            let header = recorded_header(&cache, &b.hash);
            let why = match header {
                Some(h) if h.version != VERSION => "it was in an older format",
                Some(h) if h == trace.header => "what its passes read late differs (another Clockwork)",
                _ => "the knowledge changed",
            };
            say("same", format!("run as before; the trace differs ({}, {} ticks): {why}", b.hash, b.ticks))
        }
        Some(b) if b.run.is_empty() => say("differs", "from the trace recorded before, which has no run hash"),
        Some(b) if recorded_header(&cache, &b.hash).is_some_and(|h| h.version < RUN_HASH_SINCE) => say(
            "differs",
            format!("from the trace recorded before, in an older format: its run hash isn't comparable ({})", b.run),
        ),
        Some(b) => say(
            "differs",
            format!(
                "the run differs from the one recorded before ({}): the session, the game or Clockwork changed, \
                 or a run isn't deterministic",
                b.run
            ),
        ),
        None => {}
    }
    if pairs > 0 && session.pairs.is_empty() {
        say(
            "pairs",
            format!("{pairs} dropped: they were of the run before (rrc compare {name} --pair N keeps new ones)"),
        );
    }
    say("hash", hash);
    say("run", run);
    Ok(ExitCode::SUCCESS)
}

/// A trace's header for session `name`: what it follows from the knowledge (its sync points, inputs, state and hooks),
/// the Clockwork `exe` that records it, the scenario's `edits`.
pub(crate) fn header(
    repo: &Repo,
    k: &Knowledge,
    name: &str,
    exe: &std::path::Path,
    edits: Vec<Value>,
) -> Result<Header, String> {
    if k.ticks.syncs.is_empty() {
        return Err("no sync points in knowledge/ticks.toml: find where a tick ends first (docs/formats.md)".into());
    }
    let module = repo.module();
    let (syncs, inputs, state) = items(repo, k, &module)?;
    Ok(Header {
        format: FORMAT.into(),
        version: VERSION,
        game: repo.settings.game.name.clone(),
        session: name.to_owned(),
        program: module,
        clockwork: clockwork::version(exe).unwrap_or_default(),
        syncs,
        inputs,
        state,
        edits,
        hooks: hook_items(k),
        power_on: None,
    })
}

/// The hooks a trace follows, as the knowledge has them now (D98): a recording with others isn't current.
pub(crate) fn hook_items(k: &Knowledge) -> Vec<HookItem> {
    k.ticks
        .hooks
        .iter()
        .map(|h| HookItem {
            name: h.name.clone(),
            at: h.at.clone(),
            registers: h.registers.clone(),
            interrupts: h.interrupts,
            memory: h.memory.clone(),
        })
        .collect()
}

/// What a session's run was asked to be (D123): its run's identity and its length.
pub(crate) fn identity(repo: &Repo, session: &Session) -> Result<String, String> {
    let run = crate::snapshots::run_identity(repo, session)?;
    Ok(crate::cache::sha256(format!("{run}\n{}", session.seconds).as_bytes()))
}

/// Why an item, by what it is, is never in the memory a recording keeps (D92): read through a pointer, a device's,
/// or in expanded memory, whose pages Clockwork finds wherever the program has them (D172, D217).
fn not_kept(item: &Item) -> Option<&'static str> {
    if item.via.is_some() {
        Some("is read through a pointer")
    } else if !item.device.is_empty() {
        Some("is a device's")
    } else if crate::knowledge::parse_id(&item.at).is_ok_and(|id| id.is_expanded()) {
        Some("is in expanded memory")
    } else {
        None
    }
}

/// The sync points, inputs and state items of a trace.
pub(crate) type Items = (Vec<SyncPoint>, Vec<Item>, Vec<Item>);

/// What a trace follows, from the knowledge: the sync points, the boundary values, the named data
/// (but an item that is an input at every tick: it is written once, as the input) and the items the
/// state mapping maps that aren't named data of their size (D62); an item mapped via a pointer, read
/// where it points, with the pointer (D72).
pub(crate) fn items(repo: &Repo, k: &Knowledge, module: &str) -> Result<Items, String> {
    for i in &k.ticks.inputs {
        if let Some(sync) = i.sync.iter().find(|s| !k.ticks.syncs.iter().any(|t| t.name == **s)) {
            return Err(format!(
                "input {}: no sync point {sync} (rrc sync, or rrc input {} ... --sync)",
                i.name, i.name
            ));
        }
    }
    let inputs: Vec<Item> = k.ticks.inputs.iter().map(Item::from_input).collect();
    let is_input =
        |id: &str, size: Option<u32>| inputs.iter().any(|i| i.at == id && Some(i.size) == size && i.sync.is_empty());
    let state: Vec<Item> = k
        .data
        .iter()
        .filter(|(id, d)| (id.module() == module || id.is_memory() || id.is_expanded()) && d.size.is_some())
        .filter(|(id, d)| !is_input(&id.to_string(), d.size))
        .map(|(id, d)| Item {
            name: d.decision.name.clone(),
            at: id.to_string(),
            size: d.size.unwrap_or(0),
            ..Item::default()
        })
        .collect();
    let mut state = state;
    let mapped = crate::mapping::Mapping::load(repo)?.resolve(k)?;
    for m in &mapped {
        let (id, size) = m.covers();
        let at = id.to_string();
        if !state.iter().any(|item| item.at == at && item.size == size) {
            let name = m.via.as_ref().map_or_else(|| m.field.clone(), |via| via.name.clone());
            state.push(Item { name, at, size, ..Item::default() });
        }
    }
    // The pointer an array an item is an index into is found through (D178: a carried array's too).
    for into in mapped.iter().filter_map(|m| m.into.as_ref()) {
        let (at, size) = (into.base.to_string(), into.pointer.size());
        if !state.iter().any(|item| item.at == at && item.size == size) {
            state.push(Item { name: format!("{}'s pointer", into.field), at, size, ..Item::default() });
        }
    }
    for m in &mapped {
        if let Some(via) = &m.via {
            let (name, size) = (via.item_name(), m.size);
            if !state.iter().any(|item| item.name == name && item.size == size) {
                state.push(Item { name, size, via: Some(ItemVia::of(m, via)), ..Item::default() });
            }
        }
    }
    Ok((sync_points(k)?, inputs, state))
}

/// The header of the trace recorded under `hash`, if this machine's cache has it.
pub(crate) fn recorded_header(cache: &Cache, hash: &str) -> Option<Header> {
    // Its first line alone: `rrc status` asks this of every session.
    use std::io::BufRead;
    let mut first = String::new();
    std::io::BufReader::new(std::fs::File::open(cache.path(hash)).ok()?).read_line(&mut first).ok()?;
    serde_json::from_str(&first).ok()
}

/// The session's original in a fresh Clockwork with the sync points set: Clockwork, what `load`
/// answered (the cycle it starts at, where the image is), and the sync points' names by Clockwork's
/// address (`+OFF`).
pub(crate) fn begin(
    repo: &Repo,
    session: &Session,
    syncs: &[SyncPoint],
) -> Result<(Clockwork, Value, BTreeMap<String, String>), String> {
    let places = Places::new(repo, session);
    let mut names = BTreeMap::new();
    let mut requests = Vec::new();
    for s in syncs {
        let at = places.address(&s.at)?;
        // Hits count in the module only: a launcher's other programs may run code there (D56).
        let mut request = json!({ "at": at, "program": repo.module() });
        if !s.when_changed.is_empty() {
            let gate: Vec<Value> = s.when_changed.iter().map(|item| item.request(&places)).collect::<Result<_, _>>()?;
            request["gate"] = json!(gate);
        }
        requests.push(request);
        names.insert(at, s.name.clone());
    }
    // The scenario's edits, written when their ticks are counted (D64): Clockwork counts those before a
    // based session's first too (D118).
    let mut edits = Vec::new();
    if !session.edits.is_empty() {
        let k = Knowledge::load(&repo.knowledge())?;
        let before = session.base(repo)?.map_or(0, |(_, before)| before);
        for w in crate::edits::resolve(&k, &session.edits)? {
            let (at, hex) = (places.address(&w.id.to_string())?, crate::edits::hex(&w.bytes));
            edits.push(json!({"tick": w.tick + before, "at": at, "hex": hex}));
        }
    }
    // The places first: finding where the game's program goes may take a run of its own.
    let (mut clockwork, loaded) = Clockwork::session(repo, session)?;
    for request in requests {
        clockwork.ask("sync", request)?;
    }
    if !edits.is_empty() {
        clockwork.ask("edits", json!({ "edits": edits }))?;
    }
    Ok((clockwork, loaded, names))
}

/// The session's run as its recording ran it, for a question about the whole of it (D152): with the sync points set, so
/// that its edits are written at their ticks (D64).
pub(crate) fn begin_run(repo: &Repo, session: &Session) -> Result<(Clockwork, Value), String> {
    let k = Knowledge::load(&repo.knowledge())?;
    let (clockwork, loaded, _) = begin(repo, session, &sync_points(&k)?)?;
    Ok((clockwork, loaded))
}

/// The inputs at power-on as a recording of `session` reads them where its run begins (D134), without running it
/// (D225): the session loaded in Clockwork as a recording begins it, each of the knowledge's inputs read there. On
/// its recorded folder, or on the game's folder as it is when that one can't be built (no file of it is read
/// before the program runs); whether it was. By input, those with a value (hex).
pub(crate) fn power_on_inputs(
    repo: &Repo,
    k: &Knowledge,
    session: &Session,
) -> Result<(serde_json::Map<String, Value>, bool), String> {
    let mut on = session.clone();
    let as_it_is = !session.files.is_empty() && crate::folder::of(repo, session).is_err();
    if as_it_is {
        on.files.clear();
    }
    let (_, inputs, _) = items(repo, k, &repo.module())?;
    let (mut clockwork, _, _) = begin(repo, &on, &sync_points(k)?)?;
    let places = Places::new(repo, &on);
    let mut values = serde_json::Map::new();
    if !inputs.is_empty() {
        let request: Vec<Value> = inputs.iter().map(|item| item.request(&places)).collect::<Result<_, _>>()?;
        let read = clockwork.ask("read", json!({ "ranges": request }))?;
        for (item, value) in inputs.iter().zip(read["data"].as_array().into_iter().flatten()) {
            if let Some(hex) = value.as_str() {
                values.insert(item.name.clone(), json!(hex));
            }
        }
    }
    Ok((values, as_it_is))
}

/// Runs what [`begin_run`] began to the session's end, what `run` answered. Refused if the session is recorded and its state
/// there isn't the recording's: what a question found wouldn't be about the run the session has.
pub(crate) fn run_to_end(repo: &Repo, session: &Session, clockwork: &mut Clockwork) -> Result<Value, String> {
    let ran = clockwork.ask("run", json!({ "until_ms": (session.seconds * 1000.0) as u64 }))?;
    if let Some(recorded) = &session.trace {
        let cache = Cache::open(&repo.local)?;
        let trace = Trace::parse(&String::from_utf8_lossy(&cache.get(&recorded.hash)?))?;
        let now = clockwork.ask("hash", Value::Null)?;
        if now["state_hash"].as_str() != Some(trace.end.state_hash.as_str()) {
            return Err(format!(
                "this run ended in state {}, its recording in {}: not the run the session has (rrc record it?)",
                now["state_hash"], trace.end.state_hash
            ));
        }
    }
    Ok(ran)
}

/// The knowledge's sync points as a trace names them, their gates' names resolved.
pub(crate) fn sync_points(k: &Knowledge) -> Result<Vec<SyncPoint>, String> {
    k.ticks
        .syncs
        .iter()
        .map(|s| {
            let when_changed = s
                .when_changed
                .iter()
                .map(|name| Ok(Item::from_input(&k.gate_item(name).map_err(|e| format!("sync {}: {e}", s.name))?)))
                .collect::<Result<_, String>>()?;
            Ok(SyncPoint { name: s.name.clone(), at: s.at.clone(), when_changed })
        })
        .collect()
}

/// The trace of a recording that only added items to the last one's state, all in the memory kept with
/// it (D92): the last trace's ticks with the new items read from that memory. Else why the session runs
/// (empty: nothing recorded before): the run would differ (another Clockwork, sync points, inputs or
/// edits), an item isn't in the kept memory (a device, an item read through a pointer, expanded memory,
/// memory outside the data segment), or nothing was kept.
fn from_kept(repo: &Repo, session: &Session, header: &Header) -> Result<Result<Trace, String>, String> {
    let Some(before) = &session.trace else { return Ok(Err(String::new())) };
    let Some(memory) = &before.memory else { return Ok(Err("its last recording kept no memory".into())) };
    let cache = Cache::open(&repo.local)?;
    let old = cache.get(&before.hash).ok().and_then(|text| Trace::parse(&String::from_utf8_lossy(&text)).ok());
    let Some(old) = old else { return Ok(Err("its last trace isn't in the cache".into())) };
    // The same run: all but the state (and the run's own power-on pass) alike.
    let run = |h: &Header| Header { state: Vec::new(), power_on: None, ..h.clone() };
    if old.header.version != VERSION {
        return Ok(Err(format!("its last trace is of version {}", old.header.version)));
    }
    if run(&old.header) != run(header) {
        let what = if old.header.clockwork != header.clockwork {
            "another Clockwork"
        } else if old.header.syncs != header.syncs {
            "the sync points changed"
        } else if old.header.inputs != header.inputs {
            "the inputs changed"
        } else if old.header.edits != header.edits {
            "the edits changed"
        } else if old.header.hooks != header.hooks {
            "the hooks changed"
        } else {
            "another program or game"
        };
        return Ok(Err(format!("{what} since its last recording")));
    }
    // Nothing added (`record` stops before this unless `--run`, D111).
    if run(&old.header) == run(header) && old.header.state == header.state {
        return Ok(Err("nothing added since its last recording".into()));
    }
    let key = crate::memory::key(repo, session, &header.syncs, "data")?;
    let ticks = old.ticks.len() as u64;
    let span = crate::memory::kept_by(repo, &key, memory)?.filter(|s| s.first == 1 && ticks <= s.last);
    let Some(span) = span.filter(|_| ticks > 0) else {
        return Ok(Err("the memory its last recording kept isn't in the cache, or is of another run".into()));
    };
    // Each item: its index in the last trace's state, or its range in the kept memory and where in it.
    enum Source {
        Old(usize),
        Memory(usize, usize, usize),
    }
    let places = Places::new(repo, session);
    let mut ranges = Vec::new();
    for (at, len) in &span.ranges {
        ranges.push((places.linear(at)?, *len));
    }
    let mut sources = Vec::new();
    for item in &header.state {
        if let Some(i) = old.header.state.iter().position(|o| o == item) {
            sources.push(Source::Old(i));
            continue;
        }
        if let Some(why) = not_kept(item) {
            return Ok(Err(format!("{} {why}, not kept", item.name)));
        }
        let linear = places.linear(&places.address(&item.at)?)?;
        let range = ranges.iter().position(|&(start, len)| {
            linear >= start && u64::from(linear) + u64::from(item.size) <= u64::from(start) + u64::from(len)
        });
        let Some(r) = range else { return Ok(Err(format!("{} is outside the memory kept", item.name))) };
        sources.push(Source::Memory(r, (linear - ranges[r].0) as usize, item.size as usize));
    }
    let carried: Vec<Vec<Option<String>>> =
        (0..old.header.state.len()).map(|i| carried(&old.ticks, i, |t| &t.state)).collect();
    let all: Vec<u64> = (1..=ticks).collect();
    let kept = if sources.iter().any(|s| matches!(s, Source::Memory(..))) {
        crate::memory::load(repo, &span, &all)?
    } else {
        BTreeMap::new()
    };
    let mut new_ticks = Vec::with_capacity(old.ticks.len());
    let mut previous: Vec<Option<String>> = Vec::new();
    for (t, tick) in old.ticks.iter().enumerate() {
        let values: Vec<Option<String>> = sources
            .iter()
            .map(|s| match *s {
                Source::Old(i) => carried[i][t].clone(),
                Source::Memory(r, at, size) => {
                    let bytes = kept.get(&(t as u64 + 1)).and_then(|m| m.get(r)).and_then(|m| m.get(at..at + size));
                    bytes.map(crate::edits::hex)
                }
            })
            .collect();
        let state = values
            .iter()
            .enumerate()
            .filter(|(i, v)| t == 0 || previous[*i] != **v)
            .map(|(i, v)| (i, v.clone()))
            .collect();
        new_ticks.push(Tick { state, ..tick.clone() });
        previous = values;
    }
    Ok(Ok(Trace { header: Header { power_on: old.header.power_on, ..header.clone() }, ticks: new_ticks, end: old.end }))
}

/// The ranges kept with a trace (D92), as `rrc search` reads them: an LE program's data objects, or the
/// 16-bit program's data segment (DS at the first tick), as an offset in the image (`image`, linear)
/// where it is in it.
fn data_ranges(
    repo: &Repo,
    clockwork: &mut Clockwork,
    places: &Places,
    image: u32,
) -> Result<Vec<(String, u32)>, String> {
    if crate::places::is_le(repo) {
        let objects = places.objects()?.to_vec();
        let data = objects.iter().filter(|&&(_, _, flags)| flags & 4 == 0);
        return Ok(data.map(|&(base, size, _)| (format!("@{base:X}"), size)).collect());
    }
    let regs = clockwork.ask("regs", Value::Null)?;
    let ds = clockwork::number(&regs, "ds")? as u32 * 16;
    Ok(vec![match ds.checked_sub(image) {
        Some(offset) => (format!("+{offset:05X}"), 0x1_0000),
        None => (format!("@{ds:X}"), 0x1_0000),
    }])
}

/// A followed play's start (D189): the session's play file once it has its first mark, refused if another
/// Clockwork played it (unless `other_build`, D212) or if it is a part of a play (D124's, recorded whole).
pub(crate) fn follow_play(
    repo: &Repo,
    session: &Session,
    exe: &std::path::Path,
    other_build: bool,
) -> Result<crate::follow::Follow, String> {
    let replay = session.replay.as_ref().ok_or("--follow follows a recorded play: the session has no replay")?;
    if let Some(from) = &session.from
        && Session::load(repo, &from.session)?.replay.as_ref() == Some(replay)
    {
        return Err("a part of a play goes on from the part before's input: it is recorded whole, not followed".into());
    }
    let mut follow = crate::follow::Follow::new(&repo.sessions().join(replay), 0);
    say("follows", format!("sessions/{replay} as it is written"));
    follow.start()?;
    let ours = clockwork::source(exe)?;
    let ours = ours.strip_prefix("source ").unwrap_or(&ours);
    if let Some(theirs) = follow.source.as_ref().filter(|theirs| *theirs != ours) {
        if !other_build {
            return Err(format!(
                "the play was played by another Clockwork (source {theirs}), this one is {ours}: follow it with the \
                 same build, or with --other-build to have the play's state hashes prove the run the same"
            ));
        }
        // Each state hash the window took is checked as the run reaches it: a run that differs stops there.
        say(
            "build",
            format!("played by Clockwork {theirs}, followed by {ours}: the run proved the same by its hashes"),
        );
    }
    Ok(follow)
}

/// A session's run as it goes (D192): each tick once nothing more can come for it (its pass's late reads and hooks'
/// calls are found in the pass after it), the start-up's (tick 0) first, then the run's end. A followed play (D189)
/// gives its input as it is written, and the run is held at its last mark. What it holds doesn't grow with the run:
/// the ticks not final yet, and the data segment at each tick (D92) only while that is less than the cache keeps.
pub(crate) struct Recorder<'a> {
    /// The trace's header, but the start-up's pass, which is the first tick given.
    pub header: Header,
    repo: &'a Repo,
    session: &'a Session,
    follow: Option<&'a mut crate::follow::Follow>,
    clockwork: Clockwork,
    places: Places<'a>,
    /// Places' objects asked for (an LE program's), as the late reads' IDs need.
    objects: bool,
    syncs: BTreeMap<String, String>,
    saver: crate::snapshots::Saver,
    writer: crate::cache::Writer,
    /// The screens at sync points, kept at once: a comparison reads them as the ticks come (D192).
    cache: Cache,
    picture: Value,
    /// The cycle the run starts at, and the program's image (where the data segment is found from).
    start: u64,
    image: u32,
    ranges: Value,
    pointers: Pointers,
    items: usize,
    now: u64,
    data: Vec<(String, u32)>,
    memory: Vec<Vec<Option<Vec<u8>>>>,
    keeping: bool,
    length_ms: u64,
    last_cycle: u64,
    /// The values last written: an input's when it was last read, the state's at the tick before.
    last_input: Vec<Option<Option<String>>>,
    previous: Vec<Option<String>>,
    /// The ticks made (the start-up's not counted), and each input's value as of the last.
    made: u64,
    inputs_now: Vec<Option<String>>,
    /// The ticks made that more may still come for, oldest first.
    pending: std::collections::VecDeque<Pending>,
    /// For each input, the last tick made at one of its own sync points (D130: a source with some has its tick go on
    /// past the other points' hits, and its late reads come at its own).
    last_own: Vec<u64>,
    /// The cycle of the last tick given.
    given: Option<u64>,
    /// Hooks' calls past what Clockwork keeps.
    dropped: u64,
    /// Why and where the run stopped, once it did.
    stop: Option<(String, u64)>,
    /// A play followed to its end (`rrc play`, D192): the session as long as the play once its end is written, and
    /// how far the play and the run are.
    to_end: Option<std::sync::Arc<crate::live::Progress>>,
    /// The pictures between ticks kept in the cache, or their hashes alone (a play followed live, D196: the live
    /// comparison needs only the screens at sync points, and a recording of the session makes the rest again).
    pictures: bool,
}

/// A tick made and not final yet: each input's value at it (what a late read of one is read against), and its late
/// reads with what orders them.
struct Pending {
    tick: Tick,
    values: Vec<Option<String>>,
    late: Vec<((usize, u64), LateRead)>,
}

impl<'a> Recorder<'a> {
    pub(crate) fn start(
        repo: &'a Repo,
        session: &'a Session,
        header: Header,
        mut follow: Option<&'a mut crate::follow::Follow>,
    ) -> Result<Recorder<'a>, String> {
        // A followed play's input comes as it is written, not from its file at the start; its speed is the play's.
        let mut started = session.clone();
        if let Some(f) = &follow {
            started.replay = None;
            if f.ips != 8_000_000 && !started.options.iter().any(|o| o == "--ips") {
                started.options.extend(["--ips".to_owned(), f.ips.to_string()]);
            }
        }
        let (mut clockwork, loaded, syncs) = begin(repo, &started, &header.syncs)?;
        let places = Places::new(repo, session);
        let saver = crate::snapshots::Saver::new(repo, session, &header.syncs)?;
        // Tracked from power-on, so the snapshots give a watch from them whole call chains.
        clockwork.ask("calls", json!({"on": true}))?;
        // Inputs a pass reads after they changed since its sync point (D60).
        if !header.inputs.is_empty() {
            let mut sources = Vec::new();
            for item in &header.inputs {
                let mut source = item.request(&places)?;
                if !item.sync.is_empty() {
                    let at: Vec<&String> =
                        syncs.iter().filter(|(_, name)| item.sync.contains(name)).map(|(at, _)| at).collect();
                    source["sync"] = json!(at);
                }
                sources.push(source);
            }
            clockwork.ask("late_inputs", json!({ "sources": sources }))?;
        }
        // The inputs at power-on, where the start-up's pass begins (D134).
        let mut power_on = Vec::new();
        if !header.inputs.is_empty() {
            let request: Vec<Value> =
                header.inputs.iter().map(|item| item.request(&places)).collect::<Result<_, _>>()?;
            let read = clockwork.ask("read", json!({ "ranges": request }))?;
            power_on = read["data"].as_array().into_iter().flatten().map(|v| v.as_str().map(str::to_owned)).collect();
        }
        // Calls into ported code from a part not ported (D98).
        if !header.hooks.is_empty() {
            let at: Vec<String> = header.hooks.iter().map(|h| places.address(&h.at)).collect::<Result<_, _>>()?;
            let interrupts: Vec<&String> =
                header.hooks.iter().zip(&at).filter(|(h, _)| h.interrupts).map(|(_, a)| a).collect();
            // The memory a hook's calls keep (D235).
            let memory: Vec<Value> = header
                .hooks
                .iter()
                .zip(&at)
                .filter(|(h, _)| !h.memory.is_empty())
                .map(|(h, a)| json!({"at": a, "memory": h.memory}))
                .collect();
            clockwork.ask("entries", json!({ "at": at, "interrupts": interrupts, "memory": memory }))?;
        }
        // Each frame's picture, kept in the cache (D25).
        clockwork.ask("frame_images", json!({"on": true}))?;
        let writer = Cache::open(&repo.local)?.writer();
        let start = clockwork::number(&loaded, "cycles")?;
        let mut ranges = Vec::new();
        for item in header.inputs.iter().chain(header.state.iter().filter(|item| item.via.is_none())) {
            ranges.push(item.request(&places)?);
        }
        let pointers = Pointers::new(&header.state)?;
        let items = ranges.len();
        if let Some(f) = follow.as_mut() {
            f.starting_at(start);
        }
        // The start-up's pass (D134): tick 0, every input as it was at power-on.
        let start_up = Tick {
            tick: 0,
            sync: "power-on".into(),
            cycle: start,
            skipped: None,
            inputs: power_on.iter().cloned().enumerate().collect(),
            state: Vec::new(),
            late: Vec::new(),
            hooks: Vec::new(),
            hook_memory: Vec::new(),
            frames: Vec::new(),
            sound: serde_json::Map::new(),
            screen: None,
        };
        let inputs = header.inputs.len();
        let states = header.state.len();
        Ok(Recorder {
            repo,
            session,
            follow,
            image: clockwork::linear(&loaded, "image")?,
            clockwork,
            places,
            objects: false,
            syncs,
            saver,
            writer,
            cache: Cache::open(&repo.local)?,
            picture: Value::Null,
            start,
            ranges: json!({ "ranges": ranges }),
            pointers,
            items,
            now: start,
            data: Vec::new(),
            memory: Vec::new(),
            keeping: true,
            length_ms: (session.seconds * 1000.0) as u64,
            last_cycle: start,
            last_input: vec![None; inputs],
            previous: vec![None; states],
            made: 0,
            inputs_now: vec![None; inputs],
            pending: std::collections::VecDeque::from([Pending { tick: start_up, values: power_on, late: Vec::new() }]),
            last_own: vec![0; inputs],
            given: None,
            dropped: 0,
            stop: None,
            to_end: None,
            pictures: true,
            header,
        })
    }

    /// Follows the play to its end (`rrc play`, D192): the run never goes past the millisecond the last mark is in, so
    /// that it ends where the session, as long as the play, ends when it is recorded again (D111); `progress` says how
    /// far the play and the run are.
    pub(crate) fn until_play_ends(&mut self, progress: std::sync::Arc<crate::live::Progress>) {
        self.length_ms = u64::MAX / 2;
        // Its disk kept small (D196, the person's call): the pictures between ticks by their hashes alone, a snapshot a
        // minute of play.
        self.pictures = false;
        self.saver.every(60_000);
        // Nothing kept that grows with the play (D193): the data segment at every tick (D92) isn't, which a long play
        // would take to its limit only to drop.
        self.keeping = false;
        self.to_end = Some(progress);
        // The play's inputs, for the check of the port's input handling beside it (D197).
        if let Some(follow) = self.follow.as_mut() {
            follow.keep_given();
        }
    }

    /// The followed play's inputs given up to `cycle` (a tick's), each once.
    pub(crate) fn played_until(&mut self, cycle: u64) -> Vec<(u64, String)> {
        self.follow.as_mut().map(|f| f.given_until(cycle)).unwrap_or_default()
    }

    /// The next tick final, in order (the start-up's first); none once the run has ended and every tick is given.
    pub(crate) fn next(&mut self) -> Result<Option<Tick>, String> {
        loop {
            if self.ready() {
                let mut p = self.pending.pop_front().expect("a tick made");
                // Each source's reads in the order Clockwork answers them: a source with sync points of its own gives a
                // tick's reads over more than one take.
                p.late.sort_by_key(|(order, _)| *order);
                p.tick.late = p.late.into_iter().map(|(_, late)| late).collect();
                self.given = Some(p.tick.cycle);
                return Ok(Some(p.tick));
            }
            if self.stop.is_some() {
                return Ok(None);
            }
            self.step()?;
        }
    }

    /// What `read` reads of the machine now, with the tick it is at: once [`Recorder::next`] gave a tick, the run is at
    /// the sync point of the last tick made (D193: what a port started again there is given).
    pub(crate) fn at_made<T>(
        &mut self,
        read: impl FnOnce(&mut Clockwork, &Places, u64) -> Result<T, String>,
    ) -> Result<T, String> {
        if self.stop.is_some() || self.made == 0 {
            return Err("the run is at no tick".into());
        }
        read(&mut self.clockwork, &self.places, self.made)
    }

    /// Whether the oldest tick made is final: a later one was made, and each input with sync points of its own whose
    /// tick it began has begun another since; or the run ended.
    fn ready(&self) -> bool {
        let Some(p) = self.pending.front() else { return false };
        if self.stop.is_some() {
            return true;
        }
        let n = p.tick.tick;
        self.made > n
            && self
                .header
                .inputs
                .iter()
                .zip(&self.last_own)
                .all(|(item, &own)| item.sync.is_empty() || !(n == 0 || item.sync.contains(&p.tick.sync)) || own > n)
    }

    /// Runs to the next tick, or as far as the play is written: a tick made, and what the pass before it found given
    /// to the ticks it is of; at the run's end, the rest of what was found.
    fn step(&mut self) -> Result<(), String> {
        // A followed play: not past its last mark, nor a state hash of the window's not checked yet.
        let mut held = None;
        let mut until_ms = self.length_ms;
        let mut ips = 0;
        if let Some(f) = self.follow.as_deref_mut() {
            if let crate::follow::Next::To(bound) = f.next(&mut self.clockwork, self.now)? {
                held = Some(bound);
            }
            ips = f.ips;
            if let Some(progress) = &self.to_end
                && ips > 0
            {
                // The session as long as the play once its end is written (as `rrc play` saves it), and until then not
                // past the millisecond its last mark is in, which the session's end can't come before.
                if let Some(end) = f.ended {
                    self.length_ms = (crate::play::seconds(end, ips) * 1000.0) as u64;
                }
                until_ms = self.length_ms.min((u128::from(f.written()) * 1000 / u128::from(ips)) as u64);
                progress.written.store(f.written(), std::sync::atomic::Ordering::Relaxed);
            }
        }
        let mut asked = self.ranges["ranges"].as_array().cloned().unwrap_or_default();
        if self.keeping {
            asked.extend(self.data.iter().map(|(at, len)| json!({"at": at, "len": len})));
        }
        let mut request = json!({"from": self.last_cycle, "ranges": asked, "until_ms": until_ms, "screen": true});
        if let Some(bound) = held {
            request["until_cycle"] = json!(bound);
        }
        let ran = self.clockwork.ask("tick", request)?;
        let cycle = clockwork::number(&ran, "cycles")?;
        let moved = cycle != self.now;
        self.now = cycle;
        if let Some(progress) = &self.to_end {
            progress.now.store(cycle, std::sync::atomic::Ordering::Relaxed);
        }
        let Some(at) = ran["sync"]["at"].as_str() else {
            // Held where the play is written to, short of the session's end: on once more is written.
            let short = match &self.to_end {
                Some(_) => held.is_some() && cycle < (u128::from(self.length_ms) * u128::from(ips) / 1000) as u64,
                None => held == Some(cycle) && clockwork::number(&ran, "ms")? < self.length_ms,
            };
            if short {
                if !moved {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                return Ok(());
            }
            return self.end((ran["stop"].as_str().unwrap_or("?").to_owned(), cycle));
        };
        let sync = self.syncs.get(at).cloned().unwrap_or_else(|| at.to_owned());
        let mut values: Vec<Option<String>> =
            ran["data"].as_array().into_iter().flatten().map(|v| v.as_str().map(str::to_owned)).collect();
        if self.keeping {
            let kept: Vec<Option<Vec<u8>>> = if self.data.is_empty() {
                // The first tick says where the data segment is.
                self.data = data_ranges(self.repo, &mut self.clockwork, &self.places, self.image)?;
                let request: Vec<Value> = self.data.iter().map(|(at, len)| json!({"at": at, "len": len})).collect();
                let read = self.clockwork.ask("read", json!({ "ranges": request }))?;
                read["data"].as_array().into_iter().flatten().map(|v| v.as_str().map(unhex)).collect()
            } else {
                values.split_off(self.items.min(values.len())).iter().map(|v| v.as_deref().map(unhex)).collect()
            };
            self.memory.push(kept);
            let bytes = self.data.iter().map(|(_, len)| u64::from(*len)).sum::<u64>() * self.memory.len() as u64;
            if bytes > crate::memory::MAX_KEPT {
                (self.keeping, self.memory) = (false, Vec::new());
            }
        }
        let (inputs, direct) = values.split_at(self.header.inputs.len().min(values.len()));
        let state = self.pointers.read(&mut self.clockwork, &self.places, &self.header.state, direct)?;
        let mut read = Vec::new();
        for (i, value) in inputs.iter().enumerate() {
            if self.header.inputs[i].read_at(&sync) && self.last_input[i].as_ref() != Some(value) {
                self.last_input[i] = Some(value.clone());
                read.push((i, value.clone()));
            }
        }
        let first = self.made == 0;
        let changed = state
            .iter()
            .enumerate()
            .filter(|(i, v)| first || self.previous[*i] != **v)
            .map(|(i, v)| (i, v.clone()))
            .collect();
        self.previous = state;
        let frames = pictures(self.pictures.then_some(&self.writer), &ran["frames"], &ran["images"], &mut self.picture);
        let sound = sound_events(&ran["sound"]);
        let skipped = ran["sync"]["skipped"].as_u64();
        // The screen at the sync point, when it changed (D107).
        let screen = match ran["screen"].as_str() {
            Some(hex) => Some(self.cache.put(&unhex(hex))?),
            None => None,
        };
        self.made += 1;
        for (i, value) in &read {
            self.inputs_now[*i].clone_from(value);
        }
        for (i, item) in self.header.inputs.iter().enumerate() {
            if item.sync.contains(&sync) {
                self.last_own[i] = self.made;
            }
        }
        let tick = Tick {
            tick: self.made,
            sync,
            cycle,
            skipped,
            inputs: read,
            state: changed,
            late: Vec::new(),
            hooks: Vec::new(),
            hook_memory: Vec::new(),
            frames,
            sound,
            screen,
        };
        self.pending.push_back(Pending { tick, values: self.inputs_now.clone(), late: Vec::new() });
        self.saver.tick(&mut self.clockwork, self.made, cycle, clockwork::number(&ran, "ms")?)?;
        self.last_cycle = cycle;
        // What the passes before it found.
        if !self.header.inputs.is_empty() {
            let found = self.clockwork.ask("late_inputs_take", Value::Null)?;
            self.late(&found)?;
        }
        if !self.header.hooks.is_empty() {
            let found = self.clockwork.ask("entries_take", Value::Null)?;
            self.entries(&found)?;
        }
        Ok(())
    }

    /// The run stopped: the window's state hash where it ends (a followed play), and the rest of what was found.
    fn end(&mut self, stop: (String, u64)) -> Result<(), String> {
        if let Some(f) = self.follow.as_deref_mut() {
            f.check(&mut self.clockwork, stop.1)?;
        }
        if !self.header.inputs.is_empty() {
            let found = self.clockwork.ask("late_inputs_end", Value::Null)?;
            self.late(&found)?;
        }
        if !self.header.hooks.is_empty() {
            let found = self.clockwork.ask("entries_end", Value::Null)?;
            self.entries(&found)?;
        }
        self.stop = Some(stop);
        Ok(())
    }

    /// The tick not given yet that what was found at `cycle` is of (`of`), the latest such: none if it is of no tick;
    /// an error if it is of one given already (`given`), which went without it.
    fn tick_of(
        &mut self,
        cycle: u64,
        of: impl Fn(&Pending) -> bool,
        given: impl Fn(u64) -> bool,
    ) -> Result<Option<&mut Pending>, String> {
        match self.pending.iter().rposition(of) {
            Some(i) => Ok(self.pending.get_mut(i)),
            None if self.given.is_some_and(given) => Err(format!(
                "what the run found at cycle {cycle} came after its tick was given: a recording's bug (D192)"
            )),
            None => Ok(None),
        }
    }

    /// The IDs of an LE program's objects, before the first finding is named.
    fn named(&mut self) -> Result<(), String> {
        if !self.objects && !self.header.inputs.is_empty() {
            self.places.objects()?;
            self.objects = true;
        }
        Ok(())
    }

    /// Each late read (`late_inputs_take`, `late_inputs_end`) as the input's value at its tick with the bytes the read
    /// saw (D68), numbered among its instruction's reads in the pass (D93); the start-up's (its tick at power-on)
    /// power-on's.
    fn late(&mut self, found: &Value) -> Result<(), String> {
        let findings = found["late"].as_array().map(Vec::as_slice).unwrap_or_default();
        if findings.is_empty() {
            return Ok(());
        }
        self.named()?;
        for late in findings {
            let (Some(cycle), Some(input), Some(at)) =
                (late["tick"].as_u64(), late["source"].as_u64(), late["at"].as_str())
            else {
                continue;
            };
            let at = self.places.id(at).map_or_else(|| at.to_owned(), |id| id.to_string());
            let input = input as usize;
            let start = self.start;
            // The start-up's when it began at the start (D134), whatever tick has its cycle.
            let of = move |p: &Pending| if cycle == start { p.tick.tick == 0 } else { p.tick.cycle == cycle };
            let Some(p) = self.tick_of(cycle, of, |given| cycle <= given)? else { continue };
            let read = p.values.get(input).cloned().flatten().map(|hex| {
                let mut bytes: Vec<u8> =
                    (0..hex.len() / 2).filter_map(|k| u8::from_str_radix(&hex[2 * k..2 * k + 2], 16).ok()).collect();
                for pair in late["bytes"].as_array().into_iter().flatten() {
                    if let (Some(offset), Some(value)) = (pair[0].as_u64(), pair[1].as_u64())
                        && let Some(byte) = bytes.get_mut(offset as usize)
                    {
                        *byte = value as u8;
                    }
                }
                crate::edits::hex(&bytes)
            });
            let n = late["read"].as_u64().unwrap_or(1) as u32;
            let bytes = Some(byte_pairs(&late["bytes"]).unwrap_or_default());
            let round = late["round"].as_u64().unwrap_or(0) as u32;
            let order = (input, late["cycle"].as_u64().unwrap_or(0));
            p.late.push((order, LateRead { input, at, value: read, read: n, bytes, round }));
        }
        Ok(())
    }

    /// Each hook's call (`entries_take`, `entries_end`) in the pass of the tick before it: its registers. The start-up's
    /// pass, before the first tick, is power-on's (D175).
    fn entries(&mut self, found: &Value) -> Result<(), String> {
        self.dropped += found["dropped"].as_u64().unwrap_or(0);
        let entries = found["entries"].as_array().map(Vec::as_slice).unwrap_or_default();
        if entries.is_empty() {
            return Ok(());
        }
        self.named()?;
        for entry in entries {
            let (Some(cycle), Some(at)) = (entry["cycle"].as_u64(), entry["at"].as_str()) else { continue };
            let at = self.places.id(at).map_or_else(|| at.to_owned(), |id| id.to_string());
            let Some(hook) = self.header.hooks.iter().position(|h| h.at == at) else { continue };
            let values =
                self.header.hooks[hook].registers.iter().map(|r| register(&entry["regs"], r).unwrap_or(0)).collect();
            // The last tick made at or before it: ticks' cycles only grow.
            let Some(p) = self.tick_of(cycle, |p: &Pending| p.tick.cycle <= cycle, |given| given <= cycle)? else {
                continue;
            };
            if let Some(memory) = entry["memory"].as_str() {
                p.tick.hook_memory.push((p.tick.hooks.len(), memory.to_owned()));
            }
            p.tick.hooks.push((hook, values));
        }
        Ok(())
    }

    /// The run's end once every tick is given: what it did after the last tick and the machine's state then; how many
    /// snapshots it kept; the hash of the data segment kept at every tick (D92), unless that was more than the cache
    /// keeps.
    pub(crate) fn finish(mut self) -> Result<(End, usize, Option<String>), String> {
        let (stop, cycles) = self.stop.take().ok_or("the run goes on: its ticks aren't all given")?;
        let span = json!({ "from": self.last_cycle });
        let shown = self.clockwork.ask("frames", span.clone())?;
        let taken = self.clockwork.ask("take_frame_images", Value::Null)?;
        let writer = self.pictures.then_some(&self.writer);
        let frames = pictures(writer, &shown["frames"], &taken["images"], &mut self.picture);
        self.writer.finish()?;
        let sound = sound_events(&self.clockwork.ask("sound", span)?);
        let state_hash = self.clockwork.ask("hash", Value::Null)?["state_hash"].as_str().unwrap_or_default().to_owned();
        let end = if stop == "time limit reached" { "the session's end".to_owned() } else { stop };
        let kept = if self.keeping && !self.memory.is_empty() {
            let key = crate::memory::key(self.repo, self.session, &self.header.syncs, "data")?;
            Some(crate::memory::keep(self.repo, &key, 1, &self.data, &self.memory)?.hash)
        } else {
            None
        };
        if self.dropped > 0 {
            say("hooks", format!("{} calls past the limit not kept", self.dropped));
        }
        Ok((End { end, cycles, frames, sound, state_hash }, self.saver.finish()?, kept))
    }
}

/// The session run to its end, a tick at each sync point hit, its trace written as the ticks come ([`TraceFile`]); how
/// many snapshots it kept; and the hash of the data segment kept at every tick (D92), unless that was more than the
/// cache keeps. A followed play (D189) gives its input as it is written, and the run is held at its last mark.
fn run(
    repo: &Repo,
    session: &Session,
    header: Header,
    follow: Option<&mut crate::follow::Follow>,
) -> Result<(Written, usize, Option<String>), String> {
    let mut recorder = Recorder::start(repo, session, header, follow)?;
    let mut file = TraceFile::create(repo, recorder.header.clone())?;
    while let Some(tick) = recorder.next()? {
        file.tick(&tick)?;
    }
    let (end, snapshots, kept) = recorder.finish()?;
    Ok((file.end(&end, &Cache::open(&repo.local)?)?, snapshots, kept))
}

/// A trace as it was written: its hash (kept in the cache), its run's, its ticks, its size, how it ended, its header,
/// what its passes read late in a line.
pub(crate) struct Written {
    pub hash: String,
    pub run: String,
    pub ticks: u64,
    pub bytes: u64,
    pub end: String,
    pub header: Header,
    pub late: Option<String>,
}

/// A trace written as its ticks come (D192): to a file of its own, hashed as it grows, and moved into the cache at its
/// end. The run's hash takes every frame and sound event from the start to the end, which are written aside as they
/// come, each device's apart: nothing kept grows with the run.
pub(crate) struct TraceFile {
    dir: std::path::PathBuf,
    /// The trace's file, open until its end.
    out: Option<std::io::BufWriter<std::fs::File>>,
    hasher: sha2::Sha256,
    bytes: u64,
    header: Header,
    started: bool,
    ticks: u64,
    frames: Option<Aside>,
    sound: BTreeMap<String, Aside>,
    /// The ticks with late reads, and each input's reads by the instruction: in how many ticks, the first.
    late: (usize, LateReads),
}

/// Values written aside, `,` between them.
struct Aside {
    path: std::path::PathBuf,
    out: std::io::BufWriter<std::fs::File>,
    any: bool,
}

impl Aside {
    fn create(path: std::path::PathBuf) -> Result<Aside, String> {
        let file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Aside { out: std::io::BufWriter::new(file), path, any: false })
    }

    fn push(&mut self, value: &Value) -> Result<(), String> {
        use std::io::Write as _;
        let comma: &[u8] = if self.any { b"," } else { b"" };
        self.any = true;
        self.out
            .write_all(comma)
            .and_then(|()| self.out.write_all(value.to_string().as_bytes()))
            .map_err(|e| format!("{}: {e}", self.path.display()))
    }

    /// What was written, into `hasher`.
    fn hash_into(self, hasher: &mut sha2::Sha256) -> Result<(), String> {
        use sha2::Digest as _;
        use std::io::{Read as _, Write as _};
        let Aside { path, mut out, .. } = self;
        out.flush().map_err(|e| format!("{}: {e}", path.display()))?;
        drop(out);
        let mut file = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut buffer = vec![0; 1 << 16];
        loop {
            let n = file.read(&mut buffer).map_err(|e| format!("{}: {e}", path.display()))?;
            if n == 0 {
                return Ok(());
            }
            hasher.update(&buffer[..n]);
        }
    }
}

impl TraceFile {
    /// A trace with `header` (its `power_on` is the start-up's tick, the first given), in a folder of its own under
    /// the repository's state.
    pub(crate) fn create(repo: &Repo, header: Header) -> Result<TraceFile, String> {
        static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        TraceFile::create_in(&repo.state()?.join(format!("trace-{}-{n}", std::process::id())), header)
    }

    fn create_in(dir: &std::path::Path, header: Header) -> Result<TraceFile, String> {
        use sha2::Digest as _;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join("trace.jsonl");
        let file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(TraceFile {
            dir: dir.to_owned(),
            out: Some(std::io::BufWriter::new(file)),
            hasher: sha2::Sha256::new(),
            bytes: 0,
            header: Header { power_on: None, ..header },
            started: false,
            ticks: 0,
            frames: Some(Aside::create(dir.join("frames"))?),
            sound: BTreeMap::new(),
            late: (0, BTreeMap::new()),
        })
    }

    /// A trace made whole (from the memory a recording kept, D92), written as one made as it goes.
    pub(crate) fn write(repo: &Repo, trace: &Trace) -> Result<Written, String> {
        let mut file = TraceFile::create(repo, trace.header.clone())?;
        match &trace.header.power_on {
            Some(start_up) => file.tick(start_up)?,
            None => file.start()?,
        }
        for tick in &trace.ticks {
            file.tick(tick)?;
        }
        file.end(&trace.end, &Cache::open(&repo.local)?)
    }

    fn line(&mut self, value: &Value) -> Result<(), String> {
        use sha2::Digest as _;
        use std::io::Write as _;
        let mut text = value.to_string();
        text.push('\n');
        self.hasher.update(text.as_bytes());
        self.bytes += text.len() as u64;
        let out = self.out.as_mut().ok_or("the trace is written to its end")?;
        out.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", self.dir.display()))
    }

    /// The header, without a start-up's pass (an older trace's).
    fn start(&mut self) -> Result<(), String> {
        if !self.started {
            self.started = true;
            let header = json!(self.header);
            self.line(&header)?;
        }
        Ok(())
    }

    /// The next tick: the start-up's (tick 0) goes into the header.
    pub(crate) fn tick(&mut self, tick: &Tick) -> Result<(), String> {
        if !self.started && tick.tick == 0 {
            self.header.power_on = Some(Box::new(tick.clone()));
            return self.start();
        }
        self.start()?;
        self.line(&json!(tick))?;
        self.ticks += 1;
        self.aside(&tick.frames, &tick.sound)?;
        if !tick.late.is_empty() {
            self.late.0 += 1;
            for LateRead { input, at, .. } in &tick.late {
                self.late.1.entry((*input, at.clone())).or_insert((0, tick.tick)).0 += 1;
            }
        }
        Ok(())
    }

    fn aside(&mut self, frames: &[Value], sound: &serde_json::Map<String, Value>) -> Result<(), String> {
        if let Some(aside) = self.frames.as_mut() {
            for frame in frames {
                aside.push(frame)?;
            }
        }
        for (device, events) in sound {
            if !self.sound.contains_key(device) {
                let aside = Aside::create(self.dir.join(format!("sound-{}", self.sound.len())))?;
                self.sound.insert(device.clone(), aside);
            }
            let aside = self.sound.get_mut(device).expect("just made");
            for event in events.as_array().into_iter().flatten() {
                aside.push(event)?;
            }
        }
        Ok(())
    }

    /// The trace's end: the file kept in `cache` by its hash, the run's hash ([`Trace::run_hash`]'s, without the trace
    /// in memory).
    pub(crate) fn end(mut self, end: &End, cache: &Cache) -> Result<Written, String> {
        use sha2::Digest as _;
        use std::io::Write as _;
        self.start()?;
        self.line(&json!(end))?;
        self.aside(&end.frames, &end.sound)?;
        let mut out = self.out.take().ok_or("the trace is written to its end")?;
        out.flush().map_err(|e| format!("{}: {e}", self.dir.display()))?;
        drop(out);
        let hash = crate::cache::hex(&std::mem::take(&mut self.hasher).finalize());
        // The text `run_hash` hashes, its keys in order: the frames and each device's sound written aside.
        let text = |v: &str| serde_json::to_string(v).unwrap_or_default();
        let mut run = sha2::Sha256::new();
        run.update(format!("{{\"cycles\":{},\"end\":{},\"frames\":[", end.cycles, text(&end.end)));
        if let Some(frames) = self.frames.take() {
            frames.hash_into(&mut run)?;
        }
        run.update("],\"sound\":{");
        for (n, (device, aside)) in std::mem::take(&mut self.sound).into_iter().enumerate() {
            run.update(format!("{}{}:[", if n > 0 { "," } else { "" }, text(&device)));
            aside.hash_into(&mut run)?;
            run.update("]");
        }
        run.update(format!("}},\"state_hash\":{}}}", text(&end.state_hash)));
        let run = crate::cache::hex(&run.finalize());
        let reads = std::mem::take(&mut self.late.1);
        let late = late_line(self.late.0, reads, &self.header.inputs);
        cache.adopt(&self.dir.join("trace.jsonl"), &hash)?;
        Ok(Written {
            hash,
            run,
            ticks: self.ticks,
            bytes: self.bytes,
            end: end.end.clone(),
            header: self.header.clone(),
            late,
        })
    }
}

impl Drop for TraceFile {
    /// The folder is this trace's own: what is in it is kept elsewhere or wasn't finished.
    fn drop(&mut self) {
        // Closed first: an open file isn't removed on every system.
        (self.out, self.frames) = (None, None);
        self.sound.clear();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The items read through pointers (D72): each one's index in the state, where it is past where its
/// pointer points, how the pointer points, the pointer's index; and the pointers found through it, and a
/// list's next pointer and count (D122).
struct Pointers(Vec<Pointed>);

/// An item read through a pointer: its index in the state, where it is past where the last pointer points, how
/// its pointer points, the pointer's index; the pointers found through it, and a list's next pointer and count.
struct Pointed {
    item: usize,
    offset: u32,
    kind: PointerKind,
    pointer: usize,
    then: Steps,
    next: Option<(u32, u32)>,
}

/// Pointers found through another: how far past where the last points each is, and how it points.
type Steps = Vec<(u32, PointerKind)>;

impl Pointers {
    fn new(state: &[Item]) -> Result<Pointers, String> {
        let mut found = Vec::new();
        for (i, item) in state.iter().enumerate() {
            let Some(via) = &item.via else { continue };
            let pointer = state
                .iter()
                .position(|p| p.via.is_none() && p.at == via.at && p.size == via.size)
                .ok_or_else(|| format!("{}: its pointer {} isn't traced", item.name, via.at))?;
            let (offset, kind, then, next) = (via.offset, via.kind()?, via.steps()?, via.next);
            found.push(Pointed { item: i, offset, kind, pointer, then, next });
        }
        Ok(Pointers(found))
    }

    /// The state at a tick: `direct`, what Clockwork read of the items at IDs, with the items read
    /// where their pointers point (null for a null pointer).
    fn read(
        &self,
        clockwork: &mut Clockwork,
        places: &Places,
        state: &[Item],
        direct: &[Option<String>],
    ) -> Result<Vec<Option<String>>, String> {
        let mut values = direct.iter().cloned();
        let mut all: Vec<Option<String>> =
            state.iter().map(|item| if item.via.is_none() { values.next().flatten() } else { None }).collect();
        let (mut ranges, mut which) = (Vec::new(), Vec::new());
        for Pointed { item: i, offset, kind, pointer, then, next } in &self.0 {
            let bytes = all[*pointer].as_deref().map(unhex).unwrap_or_default();
            // Through more pointers, or a list (D122): followed read by read.
            if !then.is_empty() || next.is_some() {
                let size = state[*i].size;
                all[*i] = followed(clockwork, places, (kind, kind.target(&bytes, 0)), then, *offset, size, *next)?
                    .map(|bytes| crate::edits::hex(&bytes));
                continue;
            }
            let at = match kind.target(&bytes, *offset) {
                None => continue,
                Some(Target::Id(id)) => places.address(&id.to_string())?,
                Some(Target::Linear(linear)) => clockwork::Address::Linear(linear).to_string(),
            };
            ranges.push(json!({"at": at, "len": state[*i].size}));
            which.push(*i);
        }
        if !ranges.is_empty() {
            let read = clockwork.ask("read", json!({ "ranges": ranges }))?;
            for (i, value) in which.into_iter().zip(read["data"].as_array().into_iter().flatten()) {
                all[i] = value.as_str().map(str::to_owned);
            }
        }
        Ok(all)
    }
}

/// An item found through pointers (D122): from where the first, `pointer`, points (`first`), each pointer of `then` read
/// `offset` past where the last points; the item `offset` past where the last points, `size` bytes; or a
/// list's nodes, each's next pointer `next` bytes into it, to a null one or `count`. None where a pointer is
/// null (a list ends there) or memory isn't mapped.
#[allow(clippy::too_many_arguments)]
pub(crate) fn followed(
    clockwork: &mut Clockwork,
    places: &Places,
    (pointer, first): (&PointerKind, Option<Target>),
    then: &[(u32, PointerKind)],
    offset: u32,
    size: u32,
    next: Option<(u32, u32)>,
) -> Result<Option<Vec<u8>>, String> {
    let mut read = |target: &Target, past: u32, len: u32| -> Result<Option<Vec<u8>>, String> {
        let at = match target {
            Target::Id(id) => {
                let id = crate::values::moved(id, past).ok_or("a pointer past the address space")?;
                places.address(&id.to_string())?
            }
            Target::Linear(linear) => clockwork::Address::Linear(linear.wrapping_add(past)).to_string(),
        };
        let answer = clockwork.ask("read", json!({ "ranges": [{"at": at, "len": len}] }))?;
        Ok(answer["data"][0].as_str().map(unhex))
    };
    let Some(mut target) = first else { return Ok(None) };
    let mut kind: Option<&PointerKind> = None;
    for (past, step) in then {
        let Some(bytes) = read(&target, *past, step.size())? else { return Ok(None) };
        let Some(found) = step.target(&bytes, 0) else { return Ok(None) };
        (target, kind) = (found, Some(step));
    }
    let Some((at, count)) = next else { return read(&target, offset, size) };
    // A list's next pointers point as the last pointer followed did.
    let kind = kind.unwrap_or(pointer);
    let mut nodes = Vec::new();
    for _ in 0..count {
        let Some(node) = read(&target, offset, size)? else { break };
        let pointer = node.get(at as usize..(at + kind.size()) as usize).map(<[u8]>::to_vec);
        nodes.extend(node);
        match pointer.and_then(|p| kind.target(&p, 0)) {
            Some(found) => target = found,
            None => break,
        }
    }
    Ok(Some(nodes))
}

/// Hex as bytes (what isn't hex is left out).
pub(crate) fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect()
}

/// Clockwork's frames (`[number, cycle, mode, fingerprint]`) with their pictures' hashes: `images`,
/// the pictures rendered for some of them (`[[number, hex]]`), go into the cache; a frame without one
/// shows the picture before it (`last`).
fn pictures(cache: Option<&crate::cache::Writer>, frames: &Value, images: &Value, last: &mut Value) -> Vec<Value> {
    let mut rendered = BTreeMap::new();
    for image in images.as_array().into_iter().flatten() {
        let (Some(number), Some(hex)) = (image[0].as_u64(), image[1].as_str()) else { continue };
        let bytes: Vec<u8> =
            (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect();
        // Kept, or named by its hash alone (D196).
        let hash = match cache {
            Some(cache) => cache.put(bytes),
            None => crate::cache::sha256(&bytes),
        };
        rendered.insert(number, hash);
    }
    let mut out = Vec::new();
    for frame in frames.as_array().into_iter().flatten() {
        if let Some(hash) = frame[0].as_u64().and_then(|number| rendered.get(&number)) {
            *last = json!(hash);
        }
        let mut frame = frame.as_array().cloned().unwrap_or_default();
        frame.push(last.clone());
        out.push(Value::Array(frame));
    }
    out
}

/// Clockwork's `sound` answer as a trace has it: the devices with events.
fn sound_events(answer: &Value) -> serde_json::Map<String, Value> {
    let mut sound = answer.as_object().cloned().unwrap_or_default();
    sound.retain(|_, v| v.as_array().is_some_and(|a| !a.is_empty()));
    sound
}

/// A tick's sound events shown.
const SOUND_SHOWN: usize = 12;

/// A sound event without its time, as a port makes it (D108): an OPL write `A0=59`, the speaker `tone
/// 2281/1141` or `steady on`, a digitized sound `pcm 11025`, a MIDI byte.
fn sound_text(device: &str, event: &Value) -> String {
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    match device {
        "opl" => format!("{:X}={:02X}", n(&event[1]), n(&event[2])),
        "speaker" => match (event[1].get("Steady"), event[1].get("Periodic")) {
            (Some(on), _) => format!("steady {}", if on.as_bool() == Some(true) { "on" } else { "off" }),
            (_, Some(p)) => format!("tone {}/{}", n(&p["period"]), n(&p["high"])),
            _ => "one-shot".to_owned(),
        },
        "pcm" => format!("pcm {}{}", n(&event[1]), if event[2].as_bool() == Some(true) { " stereo" } else { "" }),
        "midi" => format!("{:02X}", n(&event[1])),
        _ => event.to_string(),
    }
}

pub(crate) fn show(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--tick", "--item", "--lines"], &["--path"])?;
    let usage = "trace SESSION [--tick N] [--item NAME [--lines N]] [--path]";
    let [name] = a.positionals(1, 1, usage)? else { unreachable!() };
    let session = Session::load(repo, name)?;
    let recorded = session.trace.ok_or_else(|| format!("{name} isn't recorded: rrc record {name}"))?;
    let cache = Cache::open(&repo.local)?;
    if a.has("--path") {
        println!("{}", crate::path_text(&cache.path(&recorded.hash)));
        return Ok(ExitCode::SUCCESS);
    }
    let trace = Trace::parse(&String::from_utf8_lossy(&cache.get(&recorded.hash)?))?;
    let h = &trace.header;
    // The types data.tsv gives the items, by ID.
    let k = Knowledge::load(&repo.knowledge())?;
    let types: BTreeMap<String, String> = k.data.iter().map(|(id, d)| (id.to_string(), d.ty.clone())).collect();
    let ty = |item: &Item| {
        if item.device.is_empty() { types.get(&item.at).cloned().unwrap_or_default() } else { item.device.clone() }
    };
    if let Some(item) = a.value("--item") {
        let lines: usize = a.value("--lines").map_or(Ok(40), str::parse).map_err(|_| "--lines is a count")?;
        // Named data that is an input is in the trace as the input.
        let as_input = k.named(item).and_then(|id| h.inputs.iter().find(|i| i.at == id.to_string()));
        let item = if h.state.iter().any(|i| i.name == item) { item } else { as_input.map_or(item, |i| &i.name) };
        return show_item(&trace, item, &ty, lines);
    }
    if let Some(n) = a.value("--tick") {
        let n: u64 = n.parse().map_err(|_| "--tick is a number")?;
        // Tick 0: the start-up's pass, from power-on to the first tick (D134): its inputs as at power-on, its late
        // reads and its hooks' calls (D175).
        if n == 0 {
            let tick = trace.header.power_on.as_deref().ok_or("no tick 0: a trace recorded before D134")?;
            say("tick", "0, power-on: the start-up's pass to the first tick");
            for (i, value) in &tick.inputs {
                let item = &h.inputs[*i];
                say("input", format!("{} = {}", item.name, value_text(value.as_deref(), &ty(item))));
            }
            for (n, (i, values)) in tick.hooks.iter().enumerate().take(HOOK_CALLS) {
                let hook = &h.hooks[*i];
                let args: Vec<String> = hook.registers.iter().zip(values).map(|(r, v)| format!("{r}={v:X}h")).collect();
                say("hook", format!("{} ({}): {}{}", hook.name, hook.at, args.join(", "), memory_text(tick, n)));
            }
            if tick.hooks.len() > HOOK_CALLS {
                say("...", format!("{} more calls through hooks in this pass", tick.hooks.len() - HOOK_CALLS));
            }
            return Ok(ExitCode::SUCCESS);
        }
        let tick = trace
            .ticks
            .iter()
            .find(|t| t.tick == n)
            .ok_or_else(|| format!("no tick {n} (1 to {})", trace.ticks.len()))?;
        let skipped = tick.skipped.map(|n| format!(", {n} hits skipped before it")).unwrap_or_default();
        say("tick", format!("{} at cycle {}, sync point {}{skipped}", tick.tick, tick.cycle, tick.sync));
        let index = (n - 1) as usize;
        for (i, item) in h.inputs.iter().enumerate().filter(|(_, item)| item.read_at(&tick.sync)) {
            let value = &trace.input_values(i)[index];
            say("input", format!("{} = {}", item.name, value_text(value.as_deref(), &ty(item))));
        }
        for (i, value) in &tick.state {
            say("changed", format!("{} = {}", h.state[*i].name, value_text(value.as_deref(), &ty(&h.state[*i]))));
        }
        for l in &tick.late {
            let item = &h.inputs[l.input];
            let value = value_text(l.value.as_deref(), &ty(item));
            let which = if l.read > 1 { format!(" (its read {} there)", l.read) } else { String::new() };
            say("late", format!("{}: {}{which} read {value}, changed after the sync point", item.name, l.at));
        }
        for (n, (i, values)) in tick.hooks.iter().enumerate().take(HOOK_CALLS) {
            let hook = &h.hooks[*i];
            let args: Vec<String> = hook.registers.iter().zip(values).map(|(r, v)| format!("{r}={v:X}h")).collect();
            say("hook", format!("{} ({}): {}{}", hook.name, hook.at, args.join(", "), memory_text(tick, n)));
        }
        if tick.hooks.len() > HOOK_CALLS {
            say("...", format!("{} more calls through hooks in this pass", tick.hooks.len() - HOOK_CALLS));
        }
        for edit in h.edits.iter().filter(|e| e["tick"].as_u64() == Some(n)) {
            say("edit", edit_text(edit));
        }
        say("frames", tick.frames.len());
        for (device, events) in &tick.sound {
            let events = events.as_array().cloned().unwrap_or_default();
            let shown: Vec<String> = events.iter().take(SOUND_SHOWN).map(|e| sound_text(device, e)).collect();
            let more = if events.len() > SOUND_SHOWN {
                format!(" and {} more", events.len() - SOUND_SHOWN)
            } else {
                String::new()
            };
            say("sound", format!("{device}: {}{more}", shown.join(" ")));
        }
        return Ok(ExitCode::SUCCESS);
    }
    say(
        "trace",
        format!("{} ticks of {}, then {} (cycle {})", trace.ticks.len(), h.session, trace.end.end, trace.end.cycles),
    );
    // Ticks and, at gated sync points, the hits skipped, by sync point.
    let mut by_sync: BTreeMap<&str, (usize, Option<u64>)> = BTreeMap::new();
    for tick in &trace.ticks {
        let (ticks, skipped) = by_sync.entry(&tick.sync).or_default();
        *ticks += 1;
        if let Some(n) = tick.skipped {
            *skipped = Some(skipped.unwrap_or(0) + n);
        }
    }
    let syncs: Vec<String> = by_sync
        .iter()
        .map(|(s, (n, skipped))| match skipped {
            Some(skipped) => format!("{s} {n} ({skipped} hits skipped)"),
            None => format!("{s} {n}"),
        })
        .collect();
    say("syncs", syncs.join(", "));
    if let Some(late) = late_text(&trace) {
        say("late", late);
    }
    for edit in &h.edits {
        say("edit", format!("tick {}: {}", edit["tick"], edit_text(edit)));
    }
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
    let frames = trace.ticks.iter().map(|t| t.frames.len()).sum::<usize>();
    say("frames", format!("{frames}, then {} after the last tick", trace.end.frames.len()));
    Ok(ExitCode::SUCCESS)
}

/// The ticks whose pass read an input after it changed since their sync point, if any (D60): how
/// many, and the reads that made them late, most first.
fn late_text(trace: &Trace) -> Option<String> {
    let ticks = trace.ticks.iter().filter(|t| !t.late.is_empty()).count();
    // (input, instruction) -> (ticks, the first)
    let mut reads = LateReads::new();
    for tick in &trace.ticks {
        for LateRead { input: i, at, .. } in &tick.late {
            reads.entry((*i, at.clone())).or_insert((0, tick.tick)).0 += 1;
        }
    }
    late_line(ticks, reads, &trace.header.inputs)
}

/// Each input's late reads by the instruction: in how many ticks, the first.
type LateReads = BTreeMap<(usize, String), (usize, u64)>;

/// [`late_text`] from what it counts: the ticks with late reads, and each input's reads by the instruction (in how many
/// ticks, the first).
fn late_line(ticks: usize, reads: LateReads, inputs: &[Item]) -> Option<String> {
    if ticks == 0 {
        return None;
    }
    let mut reads: Vec<_> = reads.into_iter().collect();
    reads.sort_by_key(|(_, (n, first))| (std::cmp::Reverse(*n), *first));
    let mut most: Vec<String> = reads
        .iter()
        .take(3)
        .map(|((i, at), (n, first))| format!("{} at {at} in {n} (first tick {first})", inputs[*i].name))
        .collect();
    if reads.len() > 3 {
        most.push(format!("{} other reads", reads.len() - 3));
    }
    let ticks = if ticks == 1 { "1 tick reads".to_owned() } else { format!("{ticks} ticks read") };
    Some(format!(
        "{ticks} an input after it changed since the sync point: {}; the trace keeps what each read saw, which \
         the port reads there (docs/porting.md, \"Input that arrives during the pass\")",
        most.join(", ")
    ))
}

/// An item's value over the ticks: each tick it changed (an input: of those that read it), as its
/// type reads.
/// An edit as the trace lists it: `lives = 1 (TOY.EXE+0148B: 01)`.
pub(crate) fn edit_text(edit: &Value) -> String {
    let field = |key: &str| edit[key].as_str().unwrap_or("?").to_owned();
    format!("{} ({}: {})", field("edit"), field("at"), field("hex"))
}

fn show_item(trace: &Trace, name: &str, ty: &dyn Fn(&Item) -> String, lines: usize) -> Result<ExitCode, String> {
    let h = &trace.header;
    let (values, item): (Vec<Option<String>>, &Item) = if let Some(i) = h.inputs.iter().position(|i| i.name == name) {
        (trace.input_values(i), &h.inputs[i])
    } else if let Some(i) = h.state.iter().position(|i| i.name == name) {
        (trace.values(i), &h.state[i])
    } else {
        return Err(format!("the trace has no {name}"));
    };
    let ty = ty(item);
    let mut shown = 0;
    let mut last: Option<&Option<String>> = None;
    for (tick, value) in trace.ticks.iter().zip(&values) {
        if last == Some(value) || !item.read_at(&tick.sync) {
            continue;
        }
        last = Some(value);
        shown += 1;
        if shown <= lines {
            say(&tick.tick.to_string(), value_text(value.as_deref(), &ty));
        }
    }
    if shown > lines {
        say("...", format!("{} more changes (--lines N shows more)", shown - lines));
    }
    Ok(ExitCode::SUCCESS)
}

/// How much memory the tick's call `n` kept (D235), as `rrc trace --tick` says it.
fn memory_text(tick: &Tick, n: usize) -> String {
    tick.hook_memory
        .iter()
        .find(|(call, _)| *call == n)
        .map_or(String::new(), |(_, hex)| format!("; {} bytes kept", hex.len() / 2))
}

/// Most calls through hooks `rrc trace --tick` shows for a tick.
const HOOK_CALLS: usize = 8;

/// Most elements of an array `value_text` shows.
const ELEMENTS: usize = 16;

/// A value as its type reads (`u16`, `i16`, `u8[256]`, ...): a number in decimal, an array as its
/// elements that aren't zero (`[index]=value`, the first `ELEMENTS` of them); hex when the type isn't
/// one of these.
pub fn value_text(hex: Option<&str>, ty: &str) -> String {
    let Some(hex) = hex else { return "unmapped".into() };
    let bytes: Vec<u8> =
        (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect();
    match (ty, bytes.len()) {
        ("mouse", 46) => return mouse_text(&bytes),
        ("joystick", 12) => return joystick_text(&bytes),
        _ => {}
    }
    let (element, array) = match ty.split_once('[') {
        Some((element, _)) => (element, true),
        None => (ty, false),
    };
    let width = match element {
        "u8" | "i8" => 1,
        "u16" | "i16" => 2,
        "u32" | "i32" => 4,
        _ => return short(hex),
    };
    let signed = element.starts_with('i');
    let number = |chunk: &[u8]| -> i64 {
        let raw = chunk.iter().rev().fold(0u64, |v, &b| (v << 8) | u64::from(b));
        let shift = 64 - 8 * width as u32;
        if signed { ((raw << shift) as i64) >> shift } else { raw as i64 }
    };
    let values: Vec<i64> = bytes.chunks_exact(width).map(number).collect();
    if !array && values.len() == 1 {
        return values[0].to_string();
    }
    let set: Vec<String> =
        values.iter().enumerate().filter(|(_, v)| **v != 0).map(|(i, v)| format!("[{i:#X}]={v}")).collect();
    match set.len() {
        0 => format!("all {} zero", values.len()),
        n if n > ELEMENTS => format!("{} … ({} more not zero)", set[..ELEMENTS].join(" "), n - ELEMENTS),
        _ => set.join(" "),
    }
}

/// The mouse's state (docs/formats.md): the buttons, the position and the motion, and the press
/// and release counts that aren't zero.
fn mouse_text(bytes: &[u8]) -> String {
    let w: Vec<i16> = bytes.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    let mut text = format!("buttons {}, at ({}, {}), motion ({}, {})", w[0], w[1] as u16, w[2] as u16, w[3], w[4]);
    for (i, kind) in [(5, "pressed"), (14, "released")] {
        for (button, name) in ["left", "right", "middle"].iter().enumerate() {
            let at = i + 3 * button;
            if w[at] != 0 {
                text +=
                    &format!(", {name} {kind} {} (last at {}, {})", w[at] as u16, w[at + 1] as u16, w[at + 2] as u16);
            }
        }
    }
    text
}

/// The game port's sticks (docs/formats.md).
fn joystick_text(bytes: &[u8]) -> String {
    let sticks: Vec<String> = bytes
        .chunks_exact(6)
        .zip(["A", "B"])
        .filter(|(s, _)| s[0] != 0)
        .map(|(s, name)| {
            let (x, y) = (i16::from_le_bytes([s[2], s[3]]), i16::from_le_bytes([s[4], s[5]]));
            format!("{name}: buttons {}, at ({x}, {y})", s[1])
        })
        .collect();
    if sticks.is_empty() { "no stick".into() } else { sticks.join("; ") }
}

/// A value, shortened for a line of output.
fn short(hex: &str) -> String {
    if hex.len() <= 64 { hex.to_owned() } else { format!("{}… ({} bytes)", &hex[..64], hex.len() / 2) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_item_in_expanded_memory_is_never_in_the_memory_kept() {
        let item = |at: &str| super::Item { name: "x".into(), at: at.into(), size: 4, ..Default::default() };
        assert_eq!(super::not_kept(&item("EMS#1+11F000")), Some("is in expanded memory"));
        assert_eq!(super::not_kept(&item("PROG.EXE+0100")), None);
        let device = super::Item { device: "mouse".into(), ..item("") };
        assert_eq!(super::not_kept(&device), Some("is a device's"));
    }

    use super::*;

    #[test]
    fn a_trace_reads_back_as_written() {
        let item = |name: &str, at: &str| Item { name: name.into(), at: at.into(), size: 2, ..Item::default() };
        let header = Header {
            format: FORMAT.into(),
            version: VERSION,
            game: "game".into(),
            session: "walk".into(),
            program: "GAME.EXE".into(),
            clockwork: "0.1.0".into(),
            syncs: vec![SyncPoint { name: "play".into(), at: "GAME.EXE+00100".into(), when_changed: vec![] }],
            inputs: vec![item("elapsed", "GAME.EXE+20000")],
            state: vec![item("x", "GAME.EXE+20002"), item("y", "GAME.EXE+20004")],
            edits: vec![json!({"tick": 2, "at": "GAME.EXE+20002", "hex": "0100", "edit": "x = 1"})],
            hooks: vec![],
            power_on: None,
        };
        let tick = |n: u64, inputs: Vec<(usize, Option<String>)>, state: Vec<(usize, Option<String>)>| Tick {
            tick: n,
            sync: "play".into(),
            cycle: n * 1000,
            skipped: None,
            inputs,
            state,
            late: vec![],
            hooks: vec![],
            hook_memory: vec![],
            frames: vec![json!([n, n * 1000 - 10, 19, "61f5"])],
            sound: serde_json::Map::new(),
            screen: None,
        };
        let one = |hex: &str| vec![(0, Some(hex.to_owned()))];
        let trace = Trace {
            header,
            ticks: vec![
                tick(1, one("0100"), vec![(0, Some("0000".into())), (1, Some("0500".into()))]),
                tick(2, vec![], one("0100")),
                tick(3, one("0200"), vec![]),
            ],
            end: End {
                end: "the session's end".into(),
                cycles: 4000,
                frames: vec![json!([4, 3990, 19, "61f6"])],
                sound: serde_json::Map::new(),
                state_hash: "0123456789abcdef".into(),
            },
        };
        let text = trace.to_text();
        assert_eq!(text.lines().count(), 5);
        assert!(!text.lines().nth(2).unwrap().contains("inputs"), "empty lists are left out");
        assert_eq!(Trace::parse(&text).unwrap(), trace);
        let x: Vec<Option<String>> = trace.values(0);
        assert_eq!(x, [Some("0000".into()), Some("0100".into()), Some("0100".into())]);
        let elapsed = trace.input_values(0);
        assert_eq!(elapsed, [Some("0100".into()), Some("0100".into()), Some("0200".into())], "carried over");
        // What the knowledge reads isn't the run; what the machine did is.
        let mut renamed = trace.clone();
        renamed.header.state.pop();
        renamed.ticks[0].state.truncate(1);
        renamed.ticks[2].inputs.clear();
        assert_eq!(renamed.run_hash(), trace.run_hash());
        assert_eq!(late_text(&trace), None);
        renamed.ticks[1].late = vec![LateRead {
            input: 0,
            at: "GAME.EXE+00123".into(),
            value: Some("0200".into()),
            read: 2,
            bytes: Some(vec![(0, 2)]),
            round: 0,
        }];
        assert_eq!(Trace::parse(&renamed.to_text()).unwrap(), renamed);
        // A read in a later round of the pass (D130) keeps it.
        let mut rounds = renamed.clone();
        rounds.ticks[1].late[0].round = 3;
        assert_eq!(Trace::parse(&rounds.to_text()).unwrap(), rounds);
        let late = late_text(&renamed).unwrap();
        let first = "1 tick reads an input after it changed since the sync point: elapsed at GAME.EXE+00123 in 1 (first \
                     tick 2); the trace keeps";
        assert!(late.starts_with(first), "{late}");
        assert_eq!(renamed.run_hash(), trace.run_hash());
        renamed.end.frames.clear();
        assert_ne!(renamed.run_hash(), trace.run_hash());
        assert_eq!((value_text(Some("FEFF"), "i16"), value_text(Some("FEFF"), "u16")), ("-2".into(), "65534".into()));
        assert_eq!(value_text(Some("00000100"), "u16[2]"), "[0x1]=1");
        assert_eq!(value_text(Some("0000"), "u8[2]"), "all 2 zero");
        let long = value_text(Some(&"01".repeat(20)), "u8[20]");
        assert!(long.starts_with("[0x0]=1 ") && long.ends_with("[0xF]=1 … (4 more not zero)"), "{long}");
        assert_eq!((value_text(Some("0A0B"), ""), value_text(None, "u8")), ("0A0B".into(), "unmapped".into()));
        let mut mouse = "01007800380024FF9CFF".to_owned() + &"0".repeat(72);
        mouse.replace_range(20..32, "010078003800");
        assert_eq!(
            value_text(Some(&mouse), "mouse"),
            "buttons 1, at (120, 56), motion (-220, -100), left pressed 1 (last at 120, 56)"
        );
        assert_eq!(value_text(Some("0101FEFF2C01000000000000"), "joystick"), "A: buttons 1, at (-2, 300)");
        // Written as it grows (D192), the same bytes and the same run's hash as whole, with the start-up's pass in the
        // header and sound from two devices.
        let dir = std::env::temp_dir().join(format!("rrc-tracefile-{}", std::process::id()));
        let cache = Cache::at(&dir.join("cache")).unwrap();
        let mut whole = rounds.clone();
        whole.header.power_on = Some(Box::new(Tick {
            tick: 0,
            sync: "power-on".into(),
            cycle: 0,
            frames: vec![],
            ..tick(1, one("0000"), vec![])
        }));
        whole.ticks[1].sound.insert("opl".into(), json!([[5, 1, 2], [6, 1, 3]]));
        whole.ticks[1].sound.insert("speaker".into(), json!([[7, 1]]));
        whole.ticks[2].sound.insert("opl".into(), json!([[8, 1, 4]]));
        whole.end.sound.insert("cd".into(), json!([[9, 16, 2]]));
        let mut file = TraceFile::create_in(&dir.join("trace"), whole.header.clone()).unwrap();
        file.tick(whole.header.power_on.as_deref().unwrap()).unwrap();
        for tick in &whole.ticks {
            file.tick(tick).unwrap();
        }
        let written = file.end(&whole.end, &cache).unwrap();
        let text = whole.to_text();
        assert_eq!(
            (written.hash.as_str(), written.run.as_str()),
            (crate::cache::sha256(text.as_bytes()).as_str(), whole.run_hash().as_str())
        );
        assert_eq!(cache.get(&written.hash).unwrap(), text.as_bytes(), "kept in the cache");
        assert_eq!((written.ticks, written.bytes), (3, text.len() as u64));
        assert_eq!(written.late, late_text(&whole));
        assert!(!dir.join("trace").exists(), "its folder is gone");
        std::fs::remove_dir_all(&dir).ok();
    }
}
