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

pub const FORMAT: &str = "rrc-trace";
pub const VERSION: u32 = 4;
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
}

impl Item {
    fn from_input(i: &crate::knowledge::Input) -> Item {
        let (name, at, device) = (i.name.clone(), i.at.clone(), i.device.clone());
        Item { name, at, device, size: i.bytes(), sync: i.sync.clone() }
    }

    /// Whether an input is read at sync point `sync`.
    pub fn read_at(&self, sync: &str) -> bool {
        self.sync.is_empty() || self.sync.iter().any(|s| s == sync)
    }

    /// What Clockwork's `read` and a gate take for it.
    fn request(&self, places: &Places) -> Result<Value, String> {
        if self.device.is_empty() {
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
    /// The inputs the tick's pass read with another value than the tick has, as (index in the header's,
    /// the ID of the instruction that read it first; for a driver, the one its call returns to): they
    /// changed after its sync point, before the read (D60).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub late: Vec<(usize, String)>,
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
        if header.format != FORMAT || header.version != VERSION {
            return Err(format!("not a trace this rrc reads ({} {})", header.format, header.version));
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
    let a = Args::parse(args, &[], &[])?;
    let [name] = a.positionals(1, 1, "record SESSION")? else { unreachable!() };
    let mut session = Session::load(repo, name)?;
    let k = Knowledge::load(&repo.knowledge())?;
    if k.ticks.syncs.is_empty() {
        return Err("no sync points in knowledge/ticks.toml: find where a tick ends first (docs/formats.md)".into());
    }
    let module = repo.module();
    let (syncs, inputs, state) = items(repo, &k, &module)?;
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
    };
    let (trace, snapshots) = run(repo, &session, header)?;
    let cache = Cache::open(&repo.local)?;
    let text = trace.to_text();
    let late = late_text(&trace);
    let hash = cache.put(text.as_bytes())?;
    let run = trace.run_hash();
    let ticks = trace.ticks.len() as u64;
    let recorded = Recorded { hash: hash.clone(), run: run.clone(), ticks, clockwork: trace.header.clockwork.clone() };
    let before = session.trace.replace(recorded);
    session.save(repo, name)?;
    say("trace", format!("{ticks} ticks, {} KB: {}", text.len() / 1024, trace.end.end));
    if let Some(late) = late {
        say("late", late);
    }
    say(
        "snapshots",
        format!(
            "{snapshots}, one every {} s, for rrc watch and rrc inputs",
            crate::snapshots::spacing_ms() as f64 / 1000.0
        ),
    );
    match before {
        Some(b) if b.hash == hash => say("same", "as the trace recorded before"),
        Some(b) if b.run == run => {
            let older = recorded_header(&cache, &b.hash).is_some_and(|h| h.version != VERSION);
            let why = if older { "it was in an older format" } else { "the knowledge changed" };
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
    say("hash", hash);
    say("run", run);
    Ok(ExitCode::SUCCESS)
}

/// The sync points, inputs and state items of a trace.
pub(crate) type Items = (Vec<SyncPoint>, Vec<Item>, Vec<Item>);

/// What a trace follows, from the knowledge: the sync points, the boundary values, the named data
/// (but an item that is an input at every tick: it is written once, as the input) and the items the
/// state mapping maps that aren't named data of their size (D62).
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
    for m in crate::mapping::Mapping::load(repo)?.resolve(k)? {
        let at = m.id.to_string();
        if !state.iter().any(|item| item.at == at && item.size == m.size) {
            state.push(Item { name: m.field, at, size: m.size, ..Item::default() });
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
    // The places first: finding where the game's program goes may take a run of its own.
    let (mut clockwork, loaded) = Clockwork::session(repo, session)?;
    for request in requests {
        clockwork.ask("sync", request)?;
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

/// The session run to its end, a tick at each sync point hit; and how many snapshots it kept.
fn run(repo: &Repo, session: &Session, header: Header) -> Result<(Trace, usize), String> {
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
    // Each frame's picture, kept in the cache (D25).
    clockwork.ask("frame_images", json!({"on": true}))?;
    let writer = Cache::open(&repo.local)?.writer();
    let mut picture = Value::Null;
    let start = clockwork::number(&loaded, "cycles")?;
    let mut ranges = Vec::new();
    for item in header.inputs.iter().chain(&header.state) {
        ranges.push(item.request(&places)?);
    }
    let ranges = json!({ "ranges": ranges });
    let length_ms = (session.seconds * 1000.0) as u64;
    let mut last_cycle = start;
    // The values last written: an input's when it was last read, the state's at the tick before.
    let mut last_input: Vec<Option<Option<String>>> = vec![None; header.inputs.len()];
    let mut previous: Vec<Option<String>> = vec![None; header.state.len()];
    let mut ticks = Vec::new();
    let stop = loop {
        let request = json!({"from": last_cycle, "ranges": ranges["ranges"], "until_ms": length_ms});
        let ran = clockwork.ask("tick", request)?;
        let cycle = clockwork::number(&ran, "cycles")?;
        let Some(at) = ran["sync"]["at"].as_str() else {
            break (ran["stop"].as_str().unwrap_or("?").to_owned(), cycle);
        };
        let sync = syncs.get(at).cloned().unwrap_or_else(|| at.to_owned());
        let values: Vec<Option<String>> =
            ran["data"].as_array().into_iter().flatten().map(|v| v.as_str().map(str::to_owned)).collect();
        let (inputs, state) = values.split_at(header.inputs.len().min(values.len()));
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
        previous = state.to_vec();
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
            frames,
            sound,
        });
        saver.tick(&mut clockwork, ticks.len() as u64, cycle, clockwork::number(&ran, "ms")?)?;
        last_cycle = cycle;
    };
    if !header.inputs.is_empty() {
        let found = clockwork.ask("late_inputs_end", Value::Null)?;
        places.objects()?;
        for late in found["late"].as_array().into_iter().flatten() {
            let (Some(cycle), Some(input), Some(at)) =
                (late["tick"].as_u64(), late["source"].as_u64(), late["at"].as_str())
            else {
                continue;
            };
            let at = places.id(at).map_or_else(|| at.to_owned(), |id| id.to_string());
            if let Ok(i) = ticks.binary_search_by_key(&cycle, |t: &Tick| t.cycle) {
                ticks[i].late.push((input as usize, at));
            }
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
    Ok((Trace { header, ticks, end: End { end, cycles, frames, sound, state_hash } }, saver.finish()?))
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
        for (i, at) in &tick.late {
            say("late", format!("{}: changed after the sync point, before {at} read it", h.inputs[*i].name));
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
        for (i, at) in &tick.late {
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
        "{ticks} an input after it changed since the sync point: {}; the pass reads what the next tick records: \
         move the sync point to the read (docs/porting.md, \"Input that arrives during the pass\")",
        most.join(", ")
    ))
}

/// An item's value over the ticks: each tick it changed (an input: of those that read it), as its
/// type reads.
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

/// A value as its type reads (`u16`, `i16`, `u8[256]`, ...): a number in decimal, an array as its
/// elements that aren't zero (`[index]=value`); hex when the type isn't one of these.
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
    if set.is_empty() { format!("all {} zero", values.len()) } else { set.join(" ") }
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
        };
        let tick = |n: u64, inputs: Vec<(usize, Option<String>)>, state: Vec<(usize, Option<String>)>| Tick {
            tick: n,
            sync: "play".into(),
            cycle: n * 1000,
            skipped: None,
            inputs,
            state,
            late: vec![],
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
        renamed.ticks[1].late = vec![(0, "GAME.EXE+00123".into())];
        assert_eq!(Trace::parse(&renamed.to_text()).unwrap(), renamed);
        let late = late_text(&renamed).unwrap();
        let first = "1 tick reads an input after it changed since the sync point: elapsed at GAME.EXE+00123 in 1 (first \
                     tick 2); the pass";
        assert!(late.starts_with(first), "{late}");
        assert_eq!(renamed.run_hash(), trace.run_hash());
        renamed.end.frames.clear();
        assert_ne!(renamed.run_hash(), trace.run_hash());
        assert_eq!((value_text(Some("FEFF"), "i16"), value_text(Some("FEFF"), "u16")), ("-2".into(), "65534".into()));
        assert_eq!(value_text(Some("00000100"), "u16[2]"), "[0x1]=1");
        assert_eq!(value_text(Some("0000"), "u8[2]"), "all 2 zero");
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
