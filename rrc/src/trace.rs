//! Traces (DESIGN.md §9, D54): a session run through Clockwork, read at every tick. A tick ends when
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
pub const VERSION: u32 = 9;
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
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl ItemVia {
    fn of(m: &crate::mapping::Mapped, via: &crate::mapping::Via) -> ItemVia {
        let (pointer, base) = match &via.pointer {
            PointerKind::Near { base, .. } => ("near", base.to_string()),
            PointerKind::Far => ("far", String::new()),
            PointerKind::Segment => ("segment", String::new()),
        };
        ItemVia { at: m.id.to_string(), pointer: pointer.into(), base, size: via.pointer.size(), offset: via.offset }
    }

    pub fn kind(&self) -> Result<PointerKind, String> {
        match self.pointer.as_str() {
            "near" => Ok(PointerKind::Near { base: crate::knowledge::parse_id(&self.base)?, size: self.size }),
            "far" => Ok(PointerKind::Far),
            "segment" => Ok(PointerKind::Segment),
            other => Err(format!("a trace's pointer is near or far, not {other:?}")),
        }
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
}

/// A hook a trace follows (D98): its name, its entry's ID and the registers it takes.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HookItem {
    pub name: String,
    pub at: String,
    pub registers: Vec<String>,
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
/// read before it in the pass (D93): `[input, ID, value, read, bytes]`. Version 5 and 6 traces have no `read`:
/// one value an instruction, its first read's; versions before 8 no `bytes`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "Vec<Value>", into = "Vec<Value>")]
pub struct LateRead {
    /// The input (its index in the header's).
    pub input: usize,
    /// The instruction that read it (for a driver, the one its call returns to).
    pub at: String,
    /// What it read: the tick's value with the bytes that read saw (none: memory not mapped).
    pub value: Option<String>,
    /// Which of that instruction's reads of the input in the pass it was, from 1.
    pub read: u32,
    /// The bytes it saw that differ from what the program should have read there, `[offset, value]`: the
    /// tick's value with the program's own writes since (D97); none in traces before version 8.
    pub bytes: Option<Vec<(u32, u8)>>,
}

impl TryFrom<Vec<Value>> for LateRead {
    type Error = String;

    fn try_from(v: Vec<Value>) -> Result<LateRead, String> {
        let wrong = || format!("a late read is [input, ID, value, read]: {v:?}");
        let input = v.first().and_then(Value::as_u64).ok_or_else(wrong)? as usize;
        let at = v.get(1).and_then(Value::as_str).ok_or_else(wrong)?.to_owned();
        let value = v.get(2).and_then(Value::as_str).map(str::to_owned);
        let read = v.get(3).map_or(Some(1), Value::as_u64).ok_or_else(wrong)? as u32;
        let bytes = match v.get(4) {
            Some(list) => Some(byte_pairs(list).ok_or_else(wrong)?),
            None => None,
        };
        Ok(LateRead { input, at, value, read, bytes })
    }
}

impl From<LateRead> for Vec<Value> {
    fn from(l: LateRead) -> Vec<Value> {
        let mut v = vec![json!(l.input), json!(l.at), json!(l.value), json!(l.read)];
        if let Some(bytes) = l.bytes {
            v.push(json!(bytes));
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
    /// The frames shown during the tick: [number, cycle, video mode, fingerprint, picture (its hash in
    /// the cache)].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<Value>,
    /// Sound events during the tick, by device (only those with events).
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub sound: serde_json::Map<String, Value>,
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
    let a = Args::parse(args, &[], &["--run"])?;
    let [name] = a.positionals(1, 1, "record SESSION [--run]")? else { unreachable!() };
    let mut session = Session::load(repo, name)?;
    let k = Knowledge::load(&repo.knowledge())?;
    if k.ticks.syncs.is_empty() {
        return Err("no sync points in knowledge/ticks.toml: find where a tick ends first (docs/formats.md)".into());
    }
    let module = repo.module();
    let (syncs, inputs, state) = items(repo, &k, &module)?;
    let edits = crate::edits::listed(&crate::edits::resolve(&k, &session.edits)?);
    let exe = clockwork::find(&repo.local)?;
    let header = Header {
        format: FORMAT.into(),
        version: VERSION,
        game: repo.settings.game.name.clone(),
        session: name.clone(),
        program: module.clone(),
        clockwork: clockwork::version(&exe).unwrap_or_default(),
        syncs,
        inputs,
        state,
        edits,
        hooks: k
            .ticks
            .hooks
            .iter()
            .map(|h| HookItem { name: h.name.clone(), at: h.at.clone(), registers: h.registers.clone() })
            .collect(),
    };
    // Only items added since the last recording, all in its kept memory: no run (D92).
    let kept = if a.has("--run") { Err(String::new()) } else { from_kept(repo, &session, &header)? };
    let (trace, snapshots, memory) = match kept {
        Ok(trace) => (trace, None, session.trace.as_ref().and_then(|t| t.memory.clone())),
        Err(why) => {
            if !why.is_empty() {
                say("runs", format!("the session: {why}"));
            }
            let (trace, snapshots, memory) = run(repo, &session, header)?;
            (trace, Some(snapshots), memory)
        }
    };
    let cache = Cache::open(&repo.local)?;
    let text = trace.to_text();
    let late = late_text(&trace);
    let hash = cache.put(text.as_bytes())?;
    let run = trace.run_hash();
    let ticks = trace.ticks.len() as u64;
    let recorded =
        Recorded { hash: hash.clone(), run: run.clone(), ticks, clockwork: trace.header.clockwork.clone(), memory };
    let before = session.trace.replace(recorded);
    // Paired checkpoints are of the run they were taken in (D63).
    let pairs = session.pairs.len();
    if pairs > 0 && before.as_ref().is_none_or(|b| b.run != run) {
        session.pairs.clear();
    }
    session.save(repo, name)?;
    say("trace", format!("{ticks} ticks, {} KB: {}", text.len() / 1024, trace.end.end));
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
        .filter(|(id, d)| (id.module() == module || id.is_memory()) && d.size.is_some())
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
    let text = cache.get(hash).ok()?;
    let first = text.split(|&b| b == b'\n').next()?;
    serde_json::from_slice(first).ok()
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
    // The scenario's edits, written when their ticks are counted (D64).
    let mut edits = Vec::new();
    if !session.edits.is_empty() {
        let k = Knowledge::load(&repo.knowledge())?;
        for w in crate::edits::resolve(&k, &session.edits)? {
            edits.push(
                json!({"tick": w.tick, "at": places.address(&w.id.to_string())?, "hex": crate::edits::hex(&w.bytes)}),
            );
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
/// edits), an item isn't in the kept memory (a device, an item read through a pointer, memory outside
/// the data segment), or nothing was kept.
fn from_kept(repo: &Repo, session: &Session, header: &Header) -> Result<Result<Trace, String>, String> {
    let Some(before) = &session.trace else { return Ok(Err(String::new())) };
    let Some(memory) = &before.memory else { return Ok(Err("its last recording kept no memory".into())) };
    let cache = Cache::open(&repo.local)?;
    let old = cache.get(&before.hash).ok().and_then(|text| Trace::parse(&String::from_utf8_lossy(&text)).ok());
    let Some(old) = old else { return Ok(Err("its last trace isn't in the cache".into())) };
    // The same run: all but the state alike.
    let run = |h: &Header| Header { state: Vec::new(), ..h.clone() };
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
    // Nothing added: recording again checks the run is the same, so it runs.
    if old.header == *header {
        return Ok(Err("nothing added since its last recording: it checks the run is the same".into()));
    }
    let key = crate::memory::key(repo, session, &header.syncs, "data")?;
    let ticks = old.ticks.len() as u64;
    let span = crate::memory::kept(repo, &key, 1, ticks)?.filter(|s| &s.hash == memory);
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
        if item.via.is_some() || !item.device.is_empty() {
            return Ok(Err(format!("{} is read through a pointer or a device, not kept", item.name)));
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
    Ok(Ok(Trace { header: header.clone(), ticks: new_ticks, end: old.end }))
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

/// The session run to its end, a tick at each sync point hit; how many snapshots it kept; and the hash
/// of the data segment kept at every tick (D92), unless that was more than the cache keeps.
fn run(repo: &Repo, session: &Session, header: Header) -> Result<(Trace, usize, Option<String>), String> {
    let (mut clockwork, loaded, syncs) = begin(repo, session, &header.syncs)?;
    let places = Places::new(repo, session);
    let mut saver = crate::snapshots::Saver::new(repo, session, &header.syncs)?;
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
    // Calls into ported code from a part not ported (D98).
    if !header.hooks.is_empty() {
        let at: Vec<String> = header.hooks.iter().map(|h| places.address(&h.at)).collect::<Result<_, _>>()?;
        clockwork.ask("entries", json!({ "at": at }))?;
    }
    // Each frame's picture, kept in the cache (D25).
    clockwork.ask("frame_images", json!({"on": true}))?;
    let writer = Cache::open(&repo.local)?.writer();
    let mut picture = Value::Null;
    let start = clockwork::number(&loaded, "cycles")?;
    let mut ranges = Vec::new();
    for item in header.inputs.iter().chain(header.state.iter().filter(|item| item.via.is_none())) {
        ranges.push(item.request(&places)?);
    }
    let pointers = Pointers::new(&header.state)?;
    let items = ranges.len();
    // The data segment at each tick, from the first on (D92).
    let mut data: Vec<(String, u32)> = Vec::new();
    let mut memory: Vec<Vec<Option<Vec<u8>>>> = Vec::new();
    let mut keeping = true;
    let ranges = json!({ "ranges": ranges });
    let length_ms = (session.seconds * 1000.0) as u64;
    let mut last_cycle = start;
    // The values last written: an input's when it was last read, the state's at the tick before.
    let mut last_input: Vec<Option<Option<String>>> = vec![None; header.inputs.len()];
    let mut previous: Vec<Option<String>> = vec![None; header.state.len()];
    let mut ticks = Vec::new();
    let stop = loop {
        let mut asked = ranges["ranges"].as_array().cloned().unwrap_or_default();
        if keeping {
            asked.extend(data.iter().map(|(at, len)| json!({"at": at, "len": len})));
        }
        let request = json!({"from": last_cycle, "ranges": asked, "until_ms": length_ms});
        let ran = clockwork.ask("tick", request)?;
        let cycle = clockwork::number(&ran, "cycles")?;
        let Some(at) = ran["sync"]["at"].as_str() else {
            break (ran["stop"].as_str().unwrap_or("?").to_owned(), cycle);
        };
        let sync = syncs.get(at).cloned().unwrap_or_else(|| at.to_owned());
        let mut values: Vec<Option<String>> =
            ran["data"].as_array().into_iter().flatten().map(|v| v.as_str().map(str::to_owned)).collect();
        if keeping {
            let kept: Vec<Option<Vec<u8>>> = if data.is_empty() {
                // The first tick says where the data segment is.
                data = data_ranges(repo, &mut clockwork, &places, clockwork::linear(&loaded, "image")?)?;
                let request: Vec<Value> = data.iter().map(|(at, len)| json!({"at": at, "len": len})).collect();
                let read = clockwork.ask("read", json!({ "ranges": request }))?;
                read["data"].as_array().into_iter().flatten().map(|v| v.as_str().map(unhex)).collect()
            } else {
                values.split_off(items.min(values.len())).iter().map(|v| v.as_deref().map(unhex)).collect()
            };
            memory.push(kept);
            let bytes = data.iter().map(|(_, len)| u64::from(*len)).sum::<u64>() * memory.len() as u64;
            if bytes > crate::memory::MAX_KEPT {
                (keeping, memory) = (false, Vec::new());
            }
        }
        let (inputs, direct) = values.split_at(header.inputs.len().min(values.len()));
        let state = pointers.read(&mut clockwork, &places, &header.state, direct)?;
        let mut read = Vec::new();
        for (i, value) in inputs.iter().enumerate() {
            if header.inputs[i].read_at(&sync) && last_input[i].as_ref() != Some(value) {
                last_input[i] = Some(value.clone());
                read.push((i, value.clone()));
            }
        }
        let changed = state
            .iter()
            .enumerate()
            .filter(|(i, v)| ticks.is_empty() || previous[*i] != **v)
            .map(|(i, v)| (i, v.clone()))
            .collect();
        previous = state;
        let frames = pictures(&writer, &ran["frames"], &ran["images"], &mut picture);
        let sound = sound_events(&ran["sound"]);
        let skipped = ran["sync"]["skipped"].as_u64();
        ticks.push(Tick {
            tick: ticks.len() as u64 + 1,
            sync,
            cycle,
            skipped,
            inputs: read,
            state: changed,
            late: Vec::new(),
            hooks: Vec::new(),
            frames,
            sound,
        });
        saver.tick(&mut clockwork, ticks.len() as u64, cycle, clockwork::number(&ran, "ms")?)?;
        last_cycle = cycle;
    };
    if !header.inputs.is_empty() {
        let found = clockwork.ask("late_inputs_end", Value::Null)?;
        places.objects()?;
        // Each late read as the input's value at its tick with the bytes the read saw (D68), numbered among
        // its instruction's reads in the pass (D93).
        let values: Vec<Vec<Option<String>>> =
            (0..header.inputs.len()).map(|i| carried(&ticks, i, |t| &t.inputs)).collect();
        for late in found["late"].as_array().into_iter().flatten() {
            let (Some(cycle), Some(input), Some(at)) =
                (late["tick"].as_u64(), late["source"].as_u64(), late["at"].as_str())
            else {
                continue;
            };
            let at = places.id(at).map_or_else(|| at.to_owned(), |id| id.to_string());
            if let Ok(i) = ticks.binary_search_by_key(&cycle, |t: &Tick| t.cycle) {
                let input = input as usize;
                let read = values.get(input).and_then(|v| v[i].as_deref()).map(|hex| {
                    let mut bytes: Vec<u8> = (0..hex.len() / 2)
                        .filter_map(|k| u8::from_str_radix(&hex[2 * k..2 * k + 2], 16).ok())
                        .collect();
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
                ticks[i].late.push(LateRead { input, at, value: read, read: n, bytes });
            }
        }
    }
    // Each hook's call in the pass of the tick before it: its registers.
    if !header.hooks.is_empty() {
        let found = clockwork.ask("entries_end", Value::Null)?;
        for entry in found["entries"].as_array().into_iter().flatten() {
            let (Some(cycle), Some(at)) = (entry["cycle"].as_u64(), entry["at"].as_str()) else { continue };
            let at = places.id(at).map_or_else(|| at.to_owned(), |id| id.to_string());
            let Some(hook) = header.hooks.iter().position(|h| h.at == at) else { continue };
            let i = ticks.partition_point(|t| t.cycle <= cycle);
            let Some(tick) = i.checked_sub(1).and_then(|i| ticks.get_mut(i)) else { continue };
            let values =
                header.hooks[hook].registers.iter().map(|r| register(&entry["regs"], r).unwrap_or(0)).collect();
            tick.hooks.push((hook, values));
        }
        if found["dropped"].as_u64().unwrap_or(0) > 0 {
            say("hooks", format!("{} calls past the limit not kept", found["dropped"]));
        }
    }
    // What the run did after the last tick, and the machine's state at the end.
    let (stop, cycles) = stop;
    let span = json!({ "from": last_cycle });
    let shown = clockwork.ask("frames", span.clone())?;
    let taken = clockwork.ask("take_frame_images", Value::Null)?;
    let frames = pictures(&writer, &shown["frames"], &taken["images"], &mut picture);
    writer.finish()?;
    let sound = sound_events(&clockwork.ask("sound", span)?);
    let state_hash = clockwork.ask("hash", Value::Null)?["state_hash"].as_str().unwrap_or_default().to_owned();
    let end = if stop == "time limit reached" { "the session's end".to_owned() } else { stop };
    let kept = if keeping && !memory.is_empty() {
        let key = crate::memory::key(repo, session, &header.syncs, "data")?;
        Some(crate::memory::keep(repo, &key, 1, &data, &memory)?.hash)
    } else {
        None
    };
    Ok((Trace { header, ticks, end: End { end, cycles, frames, sound, state_hash } }, saver.finish()?, kept))
}

/// The items read through pointers (D72): each one's index in the state, where it is past where its
/// pointer points, how the pointer points, and the pointer's index.
struct Pointers(Vec<(usize, u32, PointerKind, usize)>);

impl Pointers {
    fn new(state: &[Item]) -> Result<Pointers, String> {
        let mut found = Vec::new();
        for (i, item) in state.iter().enumerate() {
            let Some(via) = &item.via else { continue };
            let pointer = state
                .iter()
                .position(|p| p.via.is_none() && p.at == via.at && p.size == via.size)
                .ok_or_else(|| format!("{}: its pointer {} isn't traced", item.name, via.at))?;
            found.push((i, via.offset, via.kind()?, pointer));
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
        for (i, offset, kind, pointer) in &self.0 {
            let bytes = all[*pointer].as_deref().map(unhex).unwrap_or_default();
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

/// Hex as bytes (what isn't hex is left out).
pub(crate) fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect()
}

/// Clockwork's frames (`[number, cycle, mode, fingerprint]`) with their pictures' hashes: `images`,
/// the pictures rendered for some of them (`[[number, hex]]`), go into the cache; a frame without one
/// shows the picture before it (`last`).
fn pictures(cache: &crate::cache::Writer, frames: &Value, images: &Value, last: &mut Value) -> Vec<Value> {
    let mut rendered = BTreeMap::new();
    for image in images.as_array().into_iter().flatten() {
        let (Some(number), Some(hex)) = (image[0].as_u64(), image[1].as_str()) else { continue };
        let bytes: Vec<u8> =
            (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect();
        rendered.insert(number, cache.put(bytes));
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
        for (i, values) in tick.hooks.iter().take(HOOK_CALLS) {
            let hook = &h.hooks[*i];
            let args: Vec<String> = hook.registers.iter().zip(values).map(|(r, v)| format!("{r}={v:X}h")).collect();
            say("hook", format!("{} ({}): {}", hook.name, hook.at, args.join(", ")));
        }
        if tick.hooks.len() > HOOK_CALLS {
            say("...", format!("{} more calls through hooks in this pass", tick.hooks.len() - HOOK_CALLS));
        }
        for edit in h.edits.iter().filter(|e| e["tick"].as_u64() == Some(n)) {
            say("edit", edit_text(edit));
        }
        say("frames", tick.frames.len());
        for (device, events) in &tick.sound {
            say("sound", format!("{device}: {} events", events.as_array().map_or(0, Vec::len)));
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
    if ticks == 0 {
        return None;
    }
    // (input, instruction) -> (ticks, the first)
    let mut reads: BTreeMap<(usize, &str), (usize, u64)> = BTreeMap::new();
    for tick in &trace.ticks {
        for LateRead { input: i, at, .. } in &tick.late {
            reads.entry((*i, at.as_str())).or_insert((0, tick.tick)).0 += 1;
        }
    }
    let mut reads: Vec<_> = reads.into_iter().collect();
    reads.sort_by_key(|(_, (n, first))| (std::cmp::Reverse(*n), *first));
    let mut most: Vec<String> = reads
        .iter()
        .take(3)
        .map(|((i, at), (n, first))| format!("{} at {at} in {n} (first tick {first})", trace.header.inputs[*i].name))
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
            frames: vec![json!([n, n * 1000 - 10, 19, "61f5"])],
            sound: serde_json::Map::new(),
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
        }];
        assert_eq!(Trace::parse(&renamed.to_text()).unwrap(), renamed);
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
    }
}
