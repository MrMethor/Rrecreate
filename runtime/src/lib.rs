//! What a Rust port builds on (DESIGN.md §6, §7, D61, D62): the port's side of its contract with
//! `rrc`. A port is a program; `rrc compare` runs it with the session's inputs, and it writes its
//! state at every tick for `rrc` to compare with the original's. The files are documented in
//! `docs/formats.md`, so a port in another language does the same without this crate.
//!
//! A port implements [`Simulation`] on its state (a plain struct that serde serializes: its fields
//! are what `mapping.toml` names) and calls [`main`]:
//!
//! ```no_run
//! # use rrc_runtime::{Simulation, Tick};
//! #[derive(serde::Serialize, serde::Deserialize)]
//! struct Game { score: u16 }
//!
//! impl Simulation for Game {
//!     fn start() -> Self { Game { score: 0 } }
//!     fn step(&mut self, tick: &Tick) { self.score = self.score.wrapping_add(tick.inputs.u16("elapsed")) }
//! }
//!
//! fn main() -> std::process::ExitCode { rrc_runtime::main::<Game>() }
//! ```
//!
//! What the port loads from the person's copy of the game (the program's static data, the game's
//! files) is in [`original`] (D82). A port that draws (M6) shows its simulation's scene through a
//! presentation and calls [`main_drawn`]; its frames are compared with the original's screen ([`present`],
//! D107).

pub mod audio;
pub mod codecs;
mod files;
pub mod mouse;
pub mod original;
pub mod present;

use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;
use std::sync::Mutex;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

pub use files::{InputsFile, PortState, StartState};
/// The knowledge's types and the original's memory read and written as their values: what a port reads and
/// writes the original's memory images with (its saved games, D126).
pub use rrc_types as types;
/// The JSON values its traits take (`Simulation::transplant`, `Presentation::given`), for a port to read them.
pub use serde_json;

/// A port's game logic: its state, stepped one tick at a time (D21: deterministic, no clock, no
/// hardware). The state serializes to the values `rrc` compares: the fields `mapping.toml` names,
/// by their serde names (`player.x` is the `x` of the field `player`).
pub trait Simulation: Serialize + DeserializeOwned {
    /// The state at the session's first tick: what the original holds when it first reaches a sync
    /// point (its start-up already run).
    fn start() -> Self;

    /// Whether, from power-on, the state starts from the program's own initial data (D135): each mapped field at a
    /// fixed place in the program's load image, as the image holds it, set over [`Simulation::start`]'s state (as a
    /// transplant's are), so `start` and `power_on` make only what the start-up's code does. A port whose `start`
    /// loads all of it itself leaves this false.
    const INITIAL_DATA: bool = false;

    /// From power-on, after [`Simulation::start`]: what the start-up made of what it read before the first
    /// sync point (D134), a clock it seeded a generator from. `tick` is tick 0, its sync point `power-on`; its
    /// inputs are as they were at power-on, and `at_site` gives what each read saw. Nothing by default.
    fn power_on(&mut self, tick: &Tick) {
        let _ = tick;
    }

    /// One pass of the loop whose sync point began the tick, with the tick's inputs: the state at
    /// this tick becomes the state at the next.
    fn step(&mut self, tick: &Tick);

    /// The state the original's values at a tick give (a transplant, DESIGN.md §8): `mapped` is the
    /// start state's serialization with every mapped field set from the original. The default
    /// deserializes it; a port overrides this where its state isn't the mapped fields alone (an
    /// index the original keeps as a pointer, a value it derives).
    fn transplant(mapped: Value) -> Result<Self, String> {
        serde_json::from_value(mapped).map_err(|e| format!("the transplant doesn't fit the port's state: {e}"))
    }
}

/// A tick: its number, the sync point that began it (which loop's pass runs), and its inputs.
pub struct Tick<'a> {
    pub number: u64,
    pub sync: &'a str,
    pub inputs: &'a Inputs,
    /// How many times the original went round the wait the pass ends at before the next tick (D130): the hits
    /// that sync point's gate passed over, nothing having changed (D48). A pass that ends at a gated wait polls it
    /// so many times more, for what a poll does each time (a cursor's blink). None where it isn't known (a live
    /// play; a session recorded before it).
    pub spun: Option<u64>,
    /// The sync point the next tick is at: where the pass ends (D143). None where it isn't known (a live play, the
    /// last tick).
    pub next: Option<&'a str>,
}

/// The quirks fixed in this run (D74): none in a comparison, which keeps every one (DESIGN.md §6.5).
static FIXED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Whether the port keeps the original's quirk `name` (the port's `quirks.toml`): its code branches on
/// this, the original's behaviour while it is true. A comparison keeps them all; a player's settings may
/// fix some ([`fix`]).
pub fn keeps(name: &str) -> bool {
    !FIXED.lock().is_ok_and(|fixed| fixed.contains(name))
}

/// Fixes quirk `name` from now on: a behaviour toggle (DESIGN.md §6.4), never on in a comparison.
pub fn fix(name: &str) {
    if let Ok(mut fixed) = FIXED.lock() {
        fixed.insert(name.to_owned());
    }
}

/// What the port noted since the trace's last line (D115): by what, how many times.
static NOTES: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

/// Notes `what` for the comparison (D115): a path the port doesn't port yet, or anything it wants said where it
/// diverges. `rrc compare` lists the notes of the pass to the tick that differs, and `--notes` all of them. A
/// note changes nothing in the state.
pub fn note(what: &str) {
    if let Ok(mut notes) = NOTES.lock() {
        *notes.entry(what.to_owned()).or_default() += 1;
    }
}

/// The notes made since the last time, taken.
fn notes_made() -> BTreeMap<String, u64> {
    NOTES.lock().map(|mut notes| std::mem::take(&mut *notes)).unwrap_or_default()
}

/// The boundary values at a tick (D15, D49): each input's bytes, as the original's memory or a
/// device held them at the sync point; and what the pass read of them later, where they had changed
/// meanwhile (D68).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inputs {
    values: BTreeMap<String, Vec<u8>>,
    /// By the ID of the instruction that read it: the reads that saw an input otherwise than the read
    /// before, in order of their numbers.
    late: BTreeMap<String, Vec<files::LateRead>>,
    /// The reads made at each site so far this round of the pass, by input ([`Inputs::at_site`]).
    reads: std::cell::RefCell<BTreeMap<(String, String), u32>>,
    /// Each input's round of the pass: the hits of gated waits it went round, none counted (D130).
    rounds: std::cell::RefCell<BTreeMap<String, u32>>,
    /// The inputs whose ticks only some sync points' hits start, with those points (else every point's).
    syncs: BTreeMap<String, Vec<String>>,
    /// The program's own writes to inputs this tick, as the port made them ([`Inputs::write`]).
    written: std::cell::RefCell<BTreeMap<String, BTreeMap<usize, u8>>>,
    /// Each input as the pass last read it where that read saw it changed (a late read), with the writes since:
    /// what it holds now as far as the pass knows ([`Inputs::current`], D132).
    seen: std::cell::RefCell<BTreeMap<String, Vec<u8>>>,
    /// The calls a part not ported made into ported code in the pass, by hook, in order (D98), and how
    /// many of each the port took ([`Inputs::next_call`]).
    calls: BTreeMap<String, Vec<Call>>,
    taken: std::cell::RefCell<BTreeMap<String, usize>>,
}

/// The gates of a port's gated sync points (D48, D129): each one's sources' values at its last tick. The original's
/// loop that polls passes its sync point again and again, and Clockwork counts a hit there as a tick only when a
/// source changed since that point's last tick (the first always): a port whose pass reaches such a hit asks
/// whether it counted, to end its step there (the next tick is that hit) or go on as the original did.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Gates {
    last: BTreeMap<String, Vec<Vec<u8>>>,
}

impl Gates {
    /// The tick at `sync` (the pass begins there): its sources' values now are its gate's.
    pub fn ticked(&mut self, sync: &str, sources: &[&str], inputs: &Inputs) {
        self.last.insert(sync.to_owned(), sources.iter().map(|s| inputs.current(s)).collect());
    }

    /// Whether a hit at `sync` now counts: its sources' values (with the pass's own writes) differ from its last
    /// tick's, or it had none.
    pub fn counts(&self, sync: &str, sources: &[&str], inputs: &Inputs) -> bool {
        let now: Vec<Vec<u8>> = sources.iter().map(|s| inputs.current(s)).collect();
        self.last.get(sync) != Some(&now)
    }

    /// The pass reached `sync`: whether the hit counts (the next tick is there); if not, the pass goes round
    /// it, and the reads after it are the next round's ([`Inputs::go_round`]).
    pub fn hit(&self, sync: &str, sources: &[&str], inputs: &Inputs) -> bool {
        let counts = self.counts(sync, sources, inputs);
        if !counts {
            inputs.go_round(sync);
        }
        counts
    }

    /// The pass arrived at the gated wait `sync`, having polled there `polled` times and gone round it `went_round`
    /// times (hits that didn't count): whether the tick is there (D143). Where the next tick is at another sync
    /// point ([`Tick::next`]), not: a hit the original's gate passed over (the pass goes round); unless the pass
    /// can't go on there (polled twice, nothing more to read): its port no longer follows the original, and ends
    /// the pass for the comparison to show where, rather than going round for ever. At the next tick's,
    /// when the gate counts the hit (`hit`), or when the original went round no more ([`Tick::spun`]) and read
    /// nothing of its sources in a later round ([`Inputs::more_rounds_of`]): Clockwork's gate sees a device's change
    /// after the pass's
    /// last read (a button let go while the pass waited), which the port's, comparing the inputs as the pass last
    /// read them (D132), can't. Not knowing the next tick (a live play, D143): by the gate, or once the pass has
    /// polled the wait: a tick is a poll, the host's inputs as it began.
    pub fn arrives(&self, sync: &str, sources: &[&str], tick: &Tick, polled: u64, went_round: u64) -> bool {
        let rounds = tick.inputs.more_rounds_of(sources);
        let more = rounds || tick.spun.is_some_and(|spun| went_round < spun);
        match tick.next {
            // `spun` counts the rounds at the next tick's wait, not this one's.
            Some(next) if next != sync => {
                let more = rounds;
                tick.inputs.go_round(sync);
                !more && polled >= 2
            }
            Some(_) if tick.spun.is_some() => self.hit(sync, sources, tick.inputs) || !more,
            Some(_) => self.hit(sync, sources, tick.inputs) || (!more && polled >= 2),
            None => self.hit(sync, sources, tick.inputs) || polled >= 1,
        }
    }
}

/// A call a part of the program not ported made into ported code in the pass (D98): the registers it
/// was made with, by name (`ax`, `cl`, `di`, as the hook's `registers` list them).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Call {
    pub registers: BTreeMap<String, u32>,
}

impl Call {
    /// A register's value (0 if the hook doesn't take it).
    pub fn get(&self, register: &str) -> u32 {
        self.registers.get(register).copied().unwrap_or(0)
    }
}

impl Inputs {
    pub fn new(values: BTreeMap<String, Vec<u8>>) -> Inputs {
        Inputs { values, ..Inputs::default() }
    }

    /// The inputs as the original's instruction `site` (its ID, `GAME.EXE+04BB9`) read them, its next
    /// read: where the pass read an input there after it had changed since the sync point (a clock read
    /// again after the frame was drawn, a key pressed meanwhile), the value that read saw; else the
    /// tick's. Each call is one read there: the port calls it where it does what that instruction did, as
    /// often as the original read there (twice for a function called twice, each time round a loop that
    /// polls), and the n-th call gets the original's n-th read (D93), counted within the pass's round (a gated
    /// wait the pass went round starts another, D130). (A live platform reads its clock or device again here.)
    pub fn at_site(&self, site: &str) -> Inputs {
        let mut names: std::collections::BTreeSet<String> = self.values.keys().cloned().collect();
        names.extend(self.late.get(site).into_iter().flatten().map(|l| l.name.clone()));
        let mut made = BTreeMap::new();
        {
            let mut reads = self.reads.borrow_mut();
            for name in names {
                let n = reads.entry((site.to_owned(), name.clone())).or_default();
                *n += 1;
                made.insert(name, *n);
            }
        }
        self.at_reads(site, |name| made.get(name).copied().unwrap_or(1))
    }

    /// The inputs as the original's instruction `site` read them at its `n`-th read in the pass's round (from
    /// 1), without counting a read ([`Inputs::at_site`]).
    pub fn at_read(&self, site: &str, n: u32) -> Inputs {
        self.at_reads(site, |_| n)
    }

    /// The pass went round a gated wait at `sync`, a hit that didn't count (D130): the reads after it are the
    /// next round's, numbered afresh, as the original's (each input whose ticks that point's hits start).
    /// [`Gates::hit`] says so where the port asks whether a hit counts.
    pub fn go_round(&self, sync: &str) {
        let mut names: std::collections::BTreeSet<&String> = self.values.keys().collect();
        names.extend(self.syncs.keys());
        let mut rounds = self.rounds.borrow_mut();
        let mut reads = self.reads.borrow_mut();
        for name in names {
            if self.syncs.get(name).is_some_and(|points| !points.iter().any(|p| p == sync)) {
                continue;
            }
            *rounds.entry(name.clone()).or_default() += 1;
            reads.retain(|(_, input), _| input != name);
        }
    }

    /// Whether the original's pass went round a gated wait more times than the port's has yet and read
    /// something there that arrived meanwhile (a late read in a later round, D130): the port goes round on, for
    /// what arrives.
    pub fn more_rounds(&self) -> bool {
        self.late.values().flatten().any(|l| l.round > self.round(&l.name))
    }

    /// [`Inputs::more_rounds`] for these inputs alone (a gated wait's sources, D143): another input's later rounds
    /// (a clock read in a loop the pass gets to after the wait) aren't this wait's.
    pub fn more_rounds_of(&self, names: &[&str]) -> bool {
        self.late.values().flatten().any(|l| names.contains(&l.name.as_str()) && l.round > self.round(&l.name))
    }

    /// An input's round of the pass (D130).
    fn round(&self, name: &str) -> u32 {
        self.rounds.borrow().get(name).copied().unwrap_or(0)
    }

    /// The inputs as `site` read them, each input at its read `n(name)` of its round.
    fn at_reads(&self, site: &str, n: impl Fn(&str) -> u32) -> Inputs {
        let mut values = self.values.clone();
        for (name, bytes) in self.written.borrow().iter() {
            let value = values.entry(name.clone()).or_default();
            for (&offset, &byte) in bytes {
                if let Some(b) = value.get_mut(offset) {
                    *b = byte;
                }
            }
        }
        // The last read there in the round that saw a change, of each input: its reads since saw the same.
        let mut last: BTreeMap<&str, &files::LateRead> = BTreeMap::new();
        for l in self.late.get(site).into_iter().flatten() {
            if l.round == self.round(&l.name) && l.read <= n(&l.name) {
                last.insert(&l.name, l);
            }
        }
        let changed: Vec<String> = last.keys().map(|n| (*n).to_owned()).collect();
        for (name, l) in last {
            match &l.bytes {
                // What the program should have read there (the tick's value and its writes), with what
                // that read saw otherwise (D97).
                Some(bytes) => {
                    let value = values.entry(name.to_owned()).or_default();
                    for &(offset, byte) in bytes {
                        if let Some(b) = value.get_mut(offset) {
                            *b = byte;
                        }
                    }
                }
                None => {
                    values.insert(name.to_owned(), l.value.clone());
                }
            }
        }
        let mut seen = self.seen.borrow_mut();
        for name in changed {
            if let Some(value) = values.get(&name) {
                seen.insert(name, value.clone());
            }
        }
        Inputs { values, ..Inputs::default() }
    }

    /// The calls the part not ported made into the ported code through hook `name` in the tick's pass, in
    /// order, with their registers (D98): the port makes each where the part would have (the renderer's
    /// wakes, at the port's view step), as the original's code was called.
    pub fn calls(&self, name: &str) -> &[Call] {
        self.calls.get(name).map_or(&[], Vec::as_slice)
    }

    /// The next of hook `name`'s runs in the tick's pass, none past the last: for a hook where a call the
    /// ported code makes into a part not ported returns, that call's results (D99: the level's text a
    /// cell has, read from data the port doesn't have yet). Each call is one return there, in order.
    pub fn next_call(&self, name: &str) -> Option<&Call> {
        let mut taken = self.taken.borrow_mut();
        let n = taken.entry(name.to_owned()).or_default();
        let call = self.calls.get(name)?.get(*n);
        *n += 1;
        call
    }

    /// The program's own write to an input (a keyboard buffer it empties), which the port makes where the
    /// original did (D97): `bytes` at `offset` of input `name`. The reads after it, through
    /// [`Inputs::at_site`], see it, with what arrived since (the bytes a read saw otherwise).
    pub fn write(&self, name: &str, offset: usize, bytes: &[u8]) {
        let mut written = self.written.borrow_mut();
        let input = written.entry(name.to_owned()).or_default();
        for (i, &byte) in bytes.iter().enumerate() {
            input.insert(offset + i, byte);
        }
        if let Some(seen) = self.seen.borrow_mut().get_mut(name) {
            for (i, &byte) in bytes.iter().enumerate() {
                if let Some(b) = seen.get_mut(offset + i) {
                    *b = byte;
                }
            }
        }
    }

    /// The program's own writes to input `name` this tick ([`Inputs::write`]), by offset: what a live platform's
    /// input handling takes up for the next tick (D119), as the original's handlers would find them.
    pub fn written(&self, name: &str) -> BTreeMap<usize, u8> {
        self.written.borrow().get(name).cloned().unwrap_or_default()
    }

    /// Whether the port's reads at `site` this tick have seen all the original's did: as many as its
    /// last there that saw a change (none: from the start). A loop polling there that hasn't seen what
    /// ends it never would (each read after sees the same), while the original's ended: the port went
    /// another way. It stops there and says so, as for a path not ported, instead of polling on.
    pub fn read_out(&self, site: &str) -> bool {
        let reads = self.reads.borrow();
        self.late
            .get(site)
            .into_iter()
            .flatten()
            .filter(|l| l.round == self.round(&l.name))
            .all(|l| reads.get(&(site.to_owned(), l.name.clone())).copied().unwrap_or(0) >= l.read)
    }

    /// An input's bytes now, as far as the pass knows: as it last read it changed (a late read, D132), else the
    /// tick's; with the pass's own writes to it since ([`Inputs::write`]). What a gate compares ([`Gates::hit`]).
    pub fn current(&self, name: &str) -> Vec<u8> {
        if let Some(seen) = self.seen.borrow().get(name) {
            return seen.clone();
        }
        let mut value = self.bytes(name).to_vec();
        for (&offset, &byte) in self.written.borrow().get(name).into_iter().flatten() {
            if let Some(b) = value.get_mut(offset) {
                *b = byte;
            }
        }
        value
    }

    /// An input's bytes (empty if the session has no such input, or it wasn't read yet).
    pub fn bytes(&self, name: &str) -> &[u8] {
        self.values.get(name).map_or(&[], Vec::as_slice)
    }

    pub fn u8(&self, name: &str) -> u8 {
        self.u8_at(name, 0)
    }

    pub fn u16(&self, name: &str) -> u16 {
        self.u16_at(name, 0)
    }

    pub fn u32(&self, name: &str) -> u32 {
        let b = self.bytes(name);
        u32::from_le_bytes([0, 1, 2, 3].map(|i| b.get(i).copied().unwrap_or(0)))
    }

    /// Element `i` of an input that is an array of bytes (a key-state array).
    pub fn u8_at(&self, name: &str, i: usize) -> u8 {
        self.bytes(name).get(i).copied().unwrap_or(0)
    }

    /// Element `i` of an input that is an array of 16-bit little-endian words.
    pub fn u16_at(&self, name: &str, i: usize) -> u16 {
        let b = self.bytes(name);
        u16::from_le_bytes([b.get(2 * i).copied().unwrap_or(0), b.get(2 * i + 1).copied().unwrap_or(0)])
    }

    pub(crate) fn set(&mut self, name: &str, bytes: Vec<u8>) {
        self.values.insert(name.to_owned(), bytes);
    }

    /// This tick's late reads, replacing the last tick's; no read made yet, nothing written.
    pub(crate) fn set_late(&mut self, late: &[files::LateRead]) {
        self.late.clear();
        for l in late {
            self.late.entry(l.site.clone()).or_default().push(l.clone());
        }
        self.late.values_mut().for_each(|reads| reads.sort_by_key(|l| (l.round, l.read)));
        self.reads.borrow_mut().clear();
        self.rounds.borrow_mut().clear();
        self.written.borrow_mut().clear();
        self.seen.borrow_mut().clear();
    }

    /// The inputs whose ticks only some sync points start (the inputs file's header), for their rounds.
    pub(crate) fn set_syncs(&mut self, syncs: &BTreeMap<String, Vec<String>>) {
        self.syncs = syncs.clone();
    }

    /// This tick's calls through hooks, replacing the last tick's.
    /// With `calls` through hook `name` in the pass, in order (D98): what a live platform's input handling
    /// answers where a comparison gives the original's (a driver's status, D121).
    pub fn with_calls(mut self, name: &str, calls: Vec<Call>) -> Inputs {
        self.calls.insert(name.to_owned(), calls);
        self
    }

    pub(crate) fn set_calls(&mut self, calls: &[(String, Call)]) {
        self.calls.clear();
        self.taken.borrow_mut().clear();
        for (name, call) in calls {
            self.calls.entry(name.clone()).or_default().push(call.clone());
        }
    }
}

/// What a run of the port is asked for (the command line `rrc compare` gives it).
#[derive(Debug, Default, PartialEq)]
pub struct Request {
    pub inputs: String,
    pub out: String,
    /// Start from the original's values at a tick (`rrc-state`).
    pub transplant: Option<String>,
    /// Start from the port's own state saved at a tick (`rrc-port-state`).
    pub resume: Option<String>,
    /// Save the port's own state at this tick, to this file.
    pub save: Option<(u64, String)>,
    /// The last tick to run.
    pub to: Option<u64>,
    /// Write the frames it draws into this folder (D107).
    pub frames: Option<String>,
    /// The original's screen at the first tick, for a port that starts from a transplant or a saved
    /// state (a frame's picture).
    pub screen: Option<String>,
    /// The presentation's own state at the first tick, the original's (`rrc-presentation`, D110).
    pub presentation: Option<String>,
    /// From power-on, the program's own initial data (`rrc-state`, tick 0, D135), for a port whose
    /// [`Simulation::INITIAL_DATA`] takes it.
    pub initial: Option<String>,
}

impl Request {
    /// `run --inputs FILE --out FILE [--transplant FILE | --resume FILE] [--save-at TICK --save FILE]
    /// [--to TICK] [--frames DIR [--screen FILE] [--presentation FILE]]`
    pub fn parse(args: &[String]) -> Result<Request, String> {
        let usage = "usage: PORT run --inputs FILE --out FILE [--transplant FILE | --resume FILE] \
                     [--save-at TICK --save FILE] [--to TICK] [--frames DIR [--screen FILE] \
                     [--presentation FILE]]";
        let mut args = args.iter();
        if args.next().map(String::as_str) != Some("run") {
            return Err(usage.into());
        }
        let mut request = Request::default();
        let (mut save_at, mut save) = (None, None);
        while let Some(flag) = args.next() {
            let mut value = || args.next().cloned().ok_or_else(|| format!("{flag} needs a value"));
            let tick = |text: String| text.parse::<u64>().map_err(|_| format!("{flag} is a tick number"));
            match flag.as_str() {
                "--inputs" => request.inputs = value()?,
                "--out" => request.out = value()?,
                "--transplant" => request.transplant = Some(value()?),
                "--resume" => request.resume = Some(value()?),
                "--save-at" => save_at = Some(tick(value()?)?),
                "--save" => save = Some(value()?),
                "--to" => request.to = Some(tick(value()?)?),
                "--frames" => request.frames = Some(value()?),
                "--screen" => request.screen = Some(value()?),
                "--presentation" => request.presentation = Some(value()?),
                "--initial" => request.initial = Some(value()?),
                _ => return Err(format!("unknown option {flag}\n{usage}")),
            }
        }
        if request.inputs.is_empty() || request.out.is_empty() {
            return Err(usage.into());
        }
        if request.transplant.is_some() && request.resume.is_some() {
            return Err("--transplant or --resume, not both".into());
        }
        request.save = match (save_at, save) {
            (Some(at), Some(file)) => Some((at, file)),
            (None, None) => None,
            _ => return Err("--save-at and --save go together".into()),
        };
        Ok(request)
    }
}

/// A port's `main`: reads the command line, runs the request, and reports errors on stderr.
pub fn main<S: Simulation>() -> ExitCode {
    main_with::<S>(&mut present::Undrawn)
}

/// The `main` of a port that draws: its simulation's scene drawn by its presentation `P` (D107).
pub fn main_drawn<S: Simulation + present::Shows, P: present::Presentation<S>>() -> ExitCode {
    main_with::<S>(&mut present::Drawn(P::default()))
}

fn main_with<S: Simulation>(drawer: &mut dyn present::Drawer<S>) -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match Request::parse(&args).and_then(|request| run_drawn::<S>(&request, drawer)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("port: error: {message}");
            ExitCode::from(1)
        }
    }
}

/// Where a port starts playing from (D120): its state, and what its presentation is given there (its own state,
/// the screen), from a folder `rrc start` wrote; or its start, from power-on.
pub struct Started<S> {
    pub state: S,
    pub presentation: Option<Value>,
    pub screen: Option<present::Frame>,
    /// From power-on (a folder of tick 0, D141): the inputs the session's start-up had (a command line), which a
    /// live platform gives the port's start-up with what its host's devices have instead (a clock).
    pub power_on: Option<Inputs>,
}

/// The start folder `dir` (`rrc start`): the state the original's mapped values give (`state.json`, a
/// transplant), the presentation's own (`presentation.json`) and the screen (`screen.frm`) if it has them. Of
/// tick 0, power-on (D141): `start`'s state, with the program's own initial data for a port that takes it
/// ([`Simulation::INITIAL_DATA`]), and the start-up's inputs (`power-on.json`).
pub fn started<S: Simulation>(dir: &std::path::Path) -> Result<Started<S>, String> {
    let path = |name: &str| dir.join(name).to_string_lossy().into_owned();
    let start = files::StartState::read(&path("state.json"))?;
    if start.tick == 0 {
        let state = if S::INITIAL_DATA { S::transplant(start.overlay(serialize(&S::start())?)?)? } else { S::start() };
        let power_on = if dir.join("power-on.json").is_file() {
            Some(files::PowerOnInputs::read(&path("power-on.json"))?)
        } else {
            None
        };
        // Nothing drawn yet: the start-up draws the first screen.
        return Ok(Started { state, presentation: None, screen: None, power_on });
    }
    let state = S::transplant(start.overlay(serialize(&S::start())?)?)?;
    let presentation = if dir.join("presentation.json").is_file() {
        Some(files::PresentationState::read(&path("presentation.json"))?)
    } else {
        None
    };
    let screen = match std::fs::read(dir.join("screen.frm")) {
        Ok(bytes) => Some(present::Frame::decode(&bytes)?),
        Err(_) => None,
    };
    Ok(Started { state, presentation, screen, power_on: None })
}

/// Runs the port as `request` says: from its start state (or a transplant, or a saved state) through
/// the session's ticks, writing its state at every tick before it steps.
pub fn run<S: Simulation>(request: &Request) -> Result<(), String> {
    run_drawn::<S>(request, &mut present::Undrawn)
}

/// The same, with the frames `drawer` draws written when the request asks for them (D107): each one
/// that changed, into `frames` as `<tick>.frm`, named on its tick's line.
fn run_drawn<S: Simulation>(request: &Request, drawer: &mut dyn present::Drawer<S>) -> Result<(), String> {
    let inputs = InputsFile::read(&request.inputs)?;
    let (mut state, first) = match (&request.transplant, &request.resume) {
        (Some(file), _) => {
            let start = StartState::read(file)?;
            (S::transplant(start.overlay(serialize(&S::start())?)?)?, start.tick)
        }
        (_, Some(file)) => {
            let saved = PortState::read(file)?;
            (serde_json::from_value(saved.state).map_err(|e| format!("{file}: {e}"))?, saved.tick)
        }
        _ => {
            // The program's own initial data under the start's state, for a port that takes it (D135).
            let state = match request.initial.as_deref().filter(|_| S::INITIAL_DATA) {
                Some(file) => S::transplant(StartState::read(file)?.overlay(serialize(&S::start())?)?)?,
                None => S::start(),
            };
            (state, 1)
        }
    };
    let sounding = drawer.sounds() && request.frames.is_some();
    let devices = sounding.then(|| drawer.sound_devices());
    let mut out = files::TraceWriter::create(&request.out, &inputs.session, first, devices)?;
    let drawing = request.frames.as_deref().filter(|_| drawer.draws());
    if let Some(dir) = drawing {
        std::fs::create_dir_all(dir).map_err(|e| format!("{dir}: {e}"))?;
    }
    let given = match &request.screen {
        Some(file) => Some(present::Frame::decode(&std::fs::read(file).map_err(|e| format!("{file}: {e}"))?)?),
        None => None,
    };
    let mut frame = given.clone().unwrap_or_else(|| present::Frame::new(320, 200));
    if let Some(file) = &request.presentation {
        drawer.given(files::PresentationState::read(file)?)?;
    }
    let mut shown: Option<present::Frame> = None;
    // The pass the last tick began, which draws the next tick's frame.
    let mut pass: Option<String> = None;
    let mut current = Inputs::default();
    current.set_syncs(&inputs.syncs);
    // The fed fields' values, each as it last changed (D124).
    let mut feeding: BTreeMap<String, Value> = BTreeMap::new();
    let mut last = 0;
    for (i, line) in inputs.ticks.iter().enumerate() {
        // Where the pass ends: the next tick's sync point (D143).
        let next = inputs.ticks.get(i + 1).map(|l| l.sync.as_str());
        for (name, bytes) in &line.inputs {
            current.set(name, bytes.clone());
        }
        for (path, value) in &line.fed {
            feeding.insert(path.clone(), value.clone());
        }
        current.set_late(&line.late);
        current.set_calls(&line.calls);
        // The start-up's pass, from power-on to the first tick (D134): the port's `power_on` takes what it read.
        if line.tick == 0 && first == 1 && request.transplant.is_none() && request.resume.is_none() {
            state.power_on(&Tick { number: 0, sync: &line.sync, inputs: &current, spun: line.spun, next });
        }
        if line.tick < first {
            continue;
        }
        if request.to.is_some_and(|to| line.tick > to) {
            break;
        }
        // A scenario's edit at this tick (D64), or a fed field (D84): the original's values of the fields
        // set, and the port's own where they differed (D114: the comparison judges them).
        let mut overrode = serde_json::Map::new();
        if !line.set.is_empty() || !feeding.is_empty() {
            let mut value = serialize(&state)?;
            for (path, v) in feeding.iter().chain(line.set.iter().map(|(p, v)| (p, v))) {
                if let Some(own) = files::get_path(&value, path).filter(|own| *own != v) {
                    overrode.insert(path.clone(), own.clone());
                }
                files::set_path(&mut value, path, v.clone())?;
            }
            state = S::transplant(value)?;
        }
        let mut drawn = None;
        if drawing.is_some() || sounding {
            match &pass {
                None => drawer.start(&state, &mut frame, given.is_some()),
                Some(pass) => drawer.draw(pass, &state, &mut frame),
            }
        }
        if let Some(dir) = drawing {
            if shown.as_ref() != Some(&frame) {
                let name = format!("{}.frm", line.tick);
                let path = std::path::Path::new(dir).join(&name);
                std::fs::write(&path, frame.encode()).map_err(|e| format!("{}: {e}", path.display()))?;
                shown = Some(frame.clone());
                drawn = Some(name);
            }
        }
        // The sound the pass before made (or the start-up, from power-on), as for the frame (D108).
        let mut sounds = present::Sounds::default();
        if sounding && (pass.is_some() || given.is_none()) {
            drawer.sound(pass.as_deref(), &state, &mut sounds);
        }
        let value = serialize(&state)?;
        out.tick(
            line.tick,
            value.clone(),
            drawn.as_deref(),
            sounding.then(|| sounds.to_json()),
            &overrode,
            &notes_made(),
        )?;
        if let Some((at, file)) = &request.save
            && *at == line.tick
        {
            PortState { tick: line.tick, state: value }.write(file)?;
        }
        state.step(&Tick { number: line.tick, sync: &line.sync, inputs: &current, spun: line.spun, next });
        pass = Some(line.sync.clone());
        last = line.tick;
    }
    out.end(last)
}

fn serialize<S: Serialize>(state: &S) -> Result<Value, String> {
    serde_json::to_value(state).map_err(|e| format!("the port's state doesn't serialize: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Counter {
        count: u16,
        keys: Vec<u8>,
        player: Player,
    }

    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Player {
        x: i16,
    }

    impl Simulation for Counter {
        fn start() -> Self {
            Counter { count: 0, keys: vec![], player: Player { x: 5 } }
        }
        fn step(&mut self, tick: &Tick) {
            self.count = self.count.wrapping_add(tick.inputs.u16("elapsed"));
            self.keys.push(tick.inputs.u8_at("keys", 1));
            if tick.sync == "play" {
                self.player.x += 1;
            }
        }
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rrc-runtime-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const INPUTS: &str = r#"{"format":"rrc-inputs","version":1,"session":"walk","inputs":[{"name":"elapsed","size":2},{"name":"keys","size":2}]}
{"tick":1,"sync":"menu","inputs":[[0,"0100"],[1,"0000"]]}
{"tick":2,"sync":"play","inputs":[[1,"0001"]]}
{"tick":3,"sync":"play","inputs":[[0,"0300"]]}
{"end":"the session's end","ticks":3}
"#;

    #[test]
    fn a_port_runs_the_session_s_ticks_and_writes_its_state() {
        let d = dir("run");
        std::fs::write(d.join("inputs.jsonl"), INPUTS).unwrap();
        let path = |f: &str| d.join(f).to_string_lossy().into_owned();
        let request = Request {
            inputs: path("inputs.jsonl"),
            out: path("port.jsonl"),
            save: Some((2, path("saved.json"))),
            ..Request::default()
        };
        run::<Counter>(&request).unwrap();
        let trace = std::fs::read_to_string(d.join("port.jsonl")).unwrap();
        let lines: Vec<Value> = trace.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines[0]["format"], "rrc-port-trace");
        assert_eq!(lines[1], serde_json::json!({"tick": 1, "state": {"count": 0, "keys": [], "player": {"x": 5}}}));
        assert_eq!(lines[2], serde_json::json!({"tick": 2, "state": {"count": 1, "keys": [0]}}), "what changed");
        assert_eq!(lines[3]["state"]["count"], 2, "elapsed holds until it changes");
        assert_eq!(lines[3]["state"]["player"], serde_json::json!({"x": 6}));
        assert_eq!(lines[4], serde_json::json!({"end": 3}));

        // Resumed from the state saved at tick 2, the port goes on the same.
        let resumed = Request {
            inputs: path("inputs.jsonl"),
            out: path("resumed.jsonl"),
            resume: Some(path("saved.json")),
            ..Request::default()
        };
        run::<Counter>(&resumed).unwrap();
        let again = std::fs::read_to_string(d.join("resumed.jsonl")).unwrap();
        let again: Vec<Value> = again.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(again[1]["tick"], 2);
        assert_eq!(again[2]["state"], lines[3]["state"], "tick 3 as before (its first line has every field)");
        std::fs::remove_dir_all(&d).ok();
    }

    /// A start-up that seeds from a clock it reads before the first sync point (D134).
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Seeded {
        seed: u16,
        steps: u16,
    }

    impl Simulation for Seeded {
        const INITIAL_DATA: bool = true;

        fn start() -> Self {
            Seeded { seed: 0, steps: 0 }
        }
        fn power_on(&mut self, tick: &Tick) {
            assert_eq!((tick.number, tick.sync), (0, "power-on"));
            self.seed = tick.inputs.at_site("GAME.EXE+00100").u16("clock") | 1;
        }
        fn step(&mut self, _: &Tick) {
            self.steps += 1;
        }
    }

    #[test]
    fn from_power_on_the_start_up_s_pass_is_the_port_s_power_on() {
        // The clock 0 at power-on; the start-up read it at 40h, the first tick's value 500h.
        let inputs = r#"{"format":"rrc-inputs","version":4,"session":"boot","inputs":[{"name":"clock","size":2}]}
{"tick":0,"sync":"power-on","inputs":[[0,"0000"]],"late":[[0,"GAME.EXE+00100","4000",1,[[0,64]]]]}
{"tick":1,"sync":"menu","inputs":[[0,"0005"]]}
{"tick":2,"sync":"menu","inputs":[]}
{"end":"the session's end","ticks":2}
"#;
        let d = dir("power-on");
        std::fs::write(d.join("inputs.jsonl"), inputs).unwrap();
        let path = |f: &str| d.join(f).to_string_lossy().into_owned();
        // The program's own initial data (D135): its steps 3, its seed 2, which the start-up's seeding replaces.
        let initial = r#"{"format":"rrc-state","version":1,"session":"boot","tick":0,"state":{"seed":2,"steps":3}}"#;
        std::fs::write(d.join("initial.json"), initial).unwrap();
        let request = Request {
            inputs: path("inputs.jsonl"),
            out: path("port.jsonl"),
            initial: Some(path("initial.json")),
            ..Request::default()
        };
        run::<Seeded>(&request).unwrap();
        let trace = std::fs::read_to_string(d.join("port.jsonl")).unwrap();
        let lines: Vec<Value> = trace.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(
            lines[1],
            serde_json::json!({"tick": 1, "state": {"seed": 0x41, "steps": 3}}),
            "the image's steps, seeded, not stepped"
        );
        assert_eq!(lines[2]["state"]["steps"], 4);
        // A port whose start makes its state itself doesn't take the initial data.
        std::fs::write(d.join("walk.jsonl"), INPUTS).unwrap();
        let initial = r#"{"format":"rrc-state","version":1,"session":"walk","tick":0,"state":{"count":9}}"#;
        std::fs::write(d.join("counted.json"), initial).unwrap();
        let request = Request {
            inputs: path("walk.jsonl"),
            out: path("walk-port.jsonl"),
            initial: Some(path("counted.json")),
            ..Request::default()
        };
        run::<Counter>(&request).unwrap();
        let trace = std::fs::read_to_string(d.join("walk-port.jsonl")).unwrap();
        let first: Value = serde_json::from_str(trace.lines().nth(1).unwrap()).unwrap();
        assert_eq!(first["state"]["count"], 0);

        // From a transplant there is no start-up to run.
        let start = r#"{"format":"rrc-state","version":1,"session":"boot","tick":2,"state":{"seed":7}}"#;
        std::fs::write(d.join("start.json"), start).unwrap();
        let request = Request {
            inputs: path("inputs.jsonl"),
            out: path("transplanted.jsonl"),
            transplant: Some(path("start.json")),
            ..Request::default()
        };
        run::<Seeded>(&request).unwrap();
        let trace = std::fs::read_to_string(d.join("transplanted.jsonl")).unwrap();
        let first: Value = serde_json::from_str(trace.lines().nth(1).unwrap()).unwrap();
        assert_eq!(first["state"]["seed"], 7);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_transplant_sets_the_mapped_fields_over_the_start_state() {
        let d = dir("transplant");
        std::fs::write(d.join("inputs.jsonl"), INPUTS).unwrap();
        let start =
            r#"{"format":"rrc-state","version":1,"session":"walk","tick":3,"state":{"count":40,"player.x":-2}}"#;
        std::fs::write(d.join("start.json"), start).unwrap();
        let path = |f: &str| d.join(f).to_string_lossy().into_owned();
        let request = Request {
            inputs: path("inputs.jsonl"),
            out: path("port.jsonl"),
            transplant: Some(path("start.json")),
            ..Request::default()
        };
        run::<Counter>(&request).unwrap();
        let trace = std::fs::read_to_string(d.join("port.jsonl")).unwrap();
        let first: Value = serde_json::from_str(trace.lines().nth(1).unwrap()).unwrap();
        assert_eq!(first, serde_json::json!({"tick": 3, "state": {"count": 40, "keys": [], "player": {"x": -2}}}));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_start_folder_gives_the_state_the_presentation_and_the_screen() {
        // D120: what `rrc start` writes, read as a port plays from it.
        let d = dir("started");
        let start = r#"{"format":"rrc-state","version":1,"session":"walk","tick":3,"state":{"count":40}}"#;
        std::fs::write(d.join("state.json"), start).unwrap();
        let bare = started::<Counter>(&d).unwrap();
        assert_eq!((bare.state.count, bare.presentation.is_none(), bare.screen.is_none()), (40, true, true));
        let own = r#"{"format":"rrc-presentation","version":1,"session":"walk","tick":3,"state":{"shade":7}}"#;
        std::fs::write(d.join("presentation.json"), own).unwrap();
        let mut screen = present::Frame::new(320, 200);
        screen.set(1, 2, 9);
        std::fs::write(d.join("screen.frm"), screen.encode()).unwrap();
        let full = started::<Counter>(&d).unwrap();
        assert_eq!(full.presentation, Some(serde_json::json!({"shade": 7})));
        assert_eq!(full.screen, Some(screen));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_start_folder_of_power_on_gives_the_initial_data_and_the_start_up_s_inputs() {
        // D141: tick 0's state is the program's initial data, for a port that takes it (D135); `start`'s alone for
        // one that doesn't. The start-up's inputs are the session's.
        let d = dir("started-power-on");
        let initial =
            r#"{"format":"rrc-state","version":1,"session":"boot","tick":0,"state":{"seed":2,"steps":3,"count":9}}"#;
        std::fs::write(d.join("state.json"), initial).unwrap();
        let inputs = r#"{"format":"rrc-power-on","version":1,"session":"boot","inputs":{"clock":"0040"}}"#;
        std::fs::write(d.join("power-on.json"), inputs).unwrap();
        let seeded = started::<Seeded>(&d).unwrap();
        assert_eq!((seeded.state.seed, seeded.state.steps), (2, 3));
        assert_eq!(seeded.power_on.unwrap().u16("clock"), 0x4000);
        let counter = started::<Counter>(&d).unwrap();
        assert_eq!(counter.state.count, 0, "Counter's own start: it doesn't take the initial data");
        std::fs::remove_dir_all(&d).ok();
    }

    impl present::Shows for Counter {
        type Scene<'a> = &'a Counter;

        fn scene(&self) -> &Counter {
            self
        }
    }

    /// A presentation that keeps a count of its own, as a drawing code's blink: each pass draws it.
    #[derive(Default)]
    struct Blinking {
        blink: u8,
    }

    impl present::Presentation<Counter> for Blinking {
        fn given(&mut self, own: Value) -> Result<(), String> {
            self.blink = own["drawn"]["blink"].as_u64().ok_or("no drawn.blink")? as u8;
            Ok(())
        }
        fn draw(&mut self, _: &str, _: &&Counter, frame: &mut present::Frame) {
            self.blink = self.blink.wrapping_add(1);
            frame.set(0, 0, self.blink);
        }
    }

    /// The same without state of its own.
    #[derive(Default)]
    struct Plain;

    impl present::Presentation<Counter> for Plain {}

    #[test]
    fn a_presentation_is_given_its_own_state_at_a_transplant() {
        let d = dir("presentation");
        std::fs::write(d.join("inputs.jsonl"), INPUTS).unwrap();
        let start = r#"{"format":"rrc-state","version":1,"session":"walk","tick":2,"state":{}}"#;
        std::fs::write(d.join("start.json"), start).unwrap();
        let own = r#"{"format":"rrc-presentation","version":1,"session":"walk","tick":2,"state":{"drawn.blink":7}}"#;
        std::fs::write(d.join("own.json"), own).unwrap();
        let path = |f: &str| d.join(f).to_string_lossy().into_owned();
        let request = Request {
            inputs: path("inputs.jsonl"),
            out: path("port.jsonl"),
            transplant: Some(path("start.json")),
            frames: Some(path("frames")),
            presentation: Some(path("own.json")),
            ..Request::default()
        };
        run_drawn::<Counter>(&request, &mut present::Drawn(Blinking::default())).unwrap();
        let frame = present::Frame::decode(&std::fs::read(d.join("frames").join("3.frm")).unwrap()).unwrap();
        assert_eq!(frame.pixels[0], 8, "tick 3's pass drew from the original's count, 7, on");
        let refused = run_drawn::<Counter>(&request, &mut present::Drawn(Plain)).unwrap_err();
        assert!(refused.contains("takes no state of its own") && refused.contains("drawn"), "{refused}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_scenario_s_edit_sets_fields_at_its_tick() {
        let d = dir("edit");
        let inputs = INPUTS.replace(
            r#"{"tick":2,"sync":"play","inputs":[[1,"0001"]]}"#,
            r#"{"tick":2,"sync":"play","inputs":[[1,"0001"]],"set":{"count":100,"keys[0]":9}}"#,
        );
        std::fs::write(d.join("inputs.jsonl"), inputs).unwrap();
        let path = |f: &str| d.join(f).to_string_lossy().into_owned();
        let request = Request { inputs: path("inputs.jsonl"), out: path("port.jsonl"), ..Request::default() };
        run::<Counter>(&request).unwrap();
        let trace = std::fs::read_to_string(d.join("port.jsonl")).unwrap();
        let lines: Vec<Value> = trace.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(
            lines[2],
            serde_json::json!({"tick": 2, "state": {"count": 100, "keys": [9]}, "overrode": {"count": 1, "keys[0]": 0}}),
            "set, then written, with the port's own values it changed (D114)"
        );
        assert_eq!(lines[3]["state"]["count"], 101, "and stepped from there");
        let mut state = serde_json::json!({"a": [{"b": 1}]});
        assert!(files::set_path(&mut state, "a[1].b", serde_json::json!(2)).is_err(), "no such element");
        assert!(files::set_path(&mut state, "a[0]x", serde_json::json!(2)).is_err());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn an_input_read_late_is_read_at_its_site() {
        let text = INPUTS.replace(
            r#"{"tick":3,"sync":"play","inputs":[[0,"0300"]]}"#,
            r#"{"tick":3,"sync":"play","inputs":[[0,"0300"]],"late":[[0,"GAME.EXE+04BB9","0600"],[0,"GAME.EXE+09017","0700",3]]}"#,
        );
        let file = InputsFile::parse(&text).unwrap();
        let site = |s: &str| s.to_owned();
        let first = files::LateRead {
            name: site("elapsed"),
            site: site("GAME.EXE+04BB9"),
            read: 1,
            value: vec![6, 0],
            bytes: None,
            round: 0,
        };
        assert_eq!(file.ticks[2].late[0], first, "a first read");
        let mut inputs = Inputs::default();
        inputs.set("elapsed", vec![3, 0]);
        inputs.set_late(&file.ticks[2].late);
        assert_eq!(inputs.u16("elapsed"), 3, "the tick's value");
        assert_eq!(inputs.at_site("GAME.EXE+04BB9").u16("elapsed"), 6, "as that instruction read it");
        assert_eq!(inputs.at_site("GAME.EXE+00100").u16("elapsed"), 3, "elsewhere, the tick's");
        // An instruction that read it three times, the third after it changed: each call is its next read.
        let polls: Vec<u16> = (0..4).map(|_| inputs.at_site("GAME.EXE+09017").u16("elapsed")).collect();
        assert_eq!(polls, [3, 3, 7, 7]);
        assert_eq!(inputs.at_read("GAME.EXE+09017", 2).u16("elapsed"), 3);
        assert!(
            inputs.read_out("GAME.EXE+09017") && inputs.read_out("GAME.EXE+00200"),
            "a read not made late: from the start"
        );
        inputs.set_late(&file.ticks[2].late);
        let site = "GAME.EXE+09017";
        let read_out: Vec<bool> = (0..3)
            .map(|_| {
                inputs.at_site(site);
                inputs.read_out(site)
            })
            .collect();
        assert_eq!(read_out, [false, false, true], "out once the third read, the last that saw a change, is made");
        inputs.set_late(&[]);
        assert_eq!(inputs.at_site("GAME.EXE+04BB9").u16("elapsed"), 3, "a late read is the tick's alone");
    }

    #[test]
    fn a_read_sees_the_program_s_own_writes_with_what_arrived_since() {
        // A key count of 1 at the sync point; the program empties the buffer, then a key arrives: the
        // read that saw it differs from what the program should read (0) in the count, though it is the
        // tick's value again.
        let text = INPUTS.replace(
            r#"{"tick":3,"sync":"play","inputs":[[0,"0300"]]}"#,
            r#"{"tick":3,"sync":"play","inputs":[[0,"0100"]],"late":[[0,"GAME.EXE+16E85","0100",4,[[0,1]]]]}"#,
        );
        let file = InputsFile::parse(&text).unwrap();
        assert_eq!(file.ticks[2].late[0].bytes, Some(vec![(0, 1)]));
        let mut inputs = Inputs::default();
        inputs.set("elapsed", vec![1, 0]);
        inputs.set_late(&file.ticks[2].late);
        inputs.write("elapsed", 0, &[0, 0]);
        let polls: Vec<u16> = (0..5).map(|_| inputs.at_site("GAME.EXE+16E85").u16("elapsed")).collect();
        assert_eq!(polls, [0, 0, 0, 1, 1], "emptied until the key's read");
        assert_eq!(inputs.u16("elapsed"), 1, "the tick's value, read at no site");
        inputs.set_late(&file.ticks[2].late);
        assert_eq!(inputs.at_site("GAME.EXE+16E85").u16("elapsed"), 1, "a new tick: nothing written");
    }

    #[test]
    fn a_part_not_ported_calls_the_ported_code_through_hooks() {
        let text = INPUTS
            .replace(
                r#"{"name":"keys","size":2}]}"#,
                r#"{"name":"keys","size":2}],"hooks":[{"name":"wake","registers":["ax","cl"]}]}"#,
            )
            .replace(
                r#"{"tick":3,"sync":"play","inputs":[[0,"0300"]]}"#,
                r#"{"tick":3,"sync":"play","inputs":[[0,"0300"]],"hooks":[[0,[1472,5]],[0,[96,7]]]}"#,
            );
        let file = InputsFile::parse(&text).unwrap();
        let mut inputs = Inputs::default();
        inputs.set_calls(&file.ticks[2].calls);
        let calls: Vec<(u32, u32)> = inputs.calls("wake").iter().map(|c| (c.get("ax"), c.get("cl"))).collect();
        assert_eq!(calls, [(1472, 5), (96, 7)], "in order");
        assert!(inputs.calls("other").is_empty() && inputs.calls("wake")[0].get("dx") == 0);
        let next: Vec<Option<u32>> = (0..3).map(|_| inputs.next_call("wake").map(|c| c.get("ax"))).collect();
        assert_eq!(next, [Some(1472), Some(96), None], "results taken in order");
        inputs.set_calls(&file.ticks[1].calls);
        assert!(inputs.calls("wake").is_empty(), "a tick without calls");
        inputs.set_calls(&file.ticks[2].calls);
        assert_eq!(inputs.next_call("wake").map(|c| c.get("cl")), Some(5), "a new tick: none taken");
    }

    #[test]
    fn quirks_are_kept_until_fixed() {
        assert!(keeps("a_quirk"));
        fix("a_quirk");
        assert!(!keeps("a_quirk") && keeps("another"));
    }

    #[test]
    fn the_command_line() {
        let args = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let r = Request::parse(&args("run --inputs i --out o --save-at 5 --save s --to 9")).unwrap();
        assert_eq!((r.save, r.to), (Some((5, "s".into())), Some(9)));
        assert!(Request::parse(&args("run --inputs i")).is_err());
        assert!(Request::parse(&args("run --inputs i --out o --save-at 5")).is_err());
        assert!(Request::parse(&args("run --inputs i --out o --transplant a --resume b")).is_err());
        let r = Request::parse(&args("run --inputs i --out o --frames f --presentation p")).unwrap();
        assert_eq!(r.presentation.as_deref(), Some("p"));
    }

    #[test]
    fn a_read_after_a_wait_went_round_is_numbered_in_its_round() {
        // A key wait goes round twice without a tick; the key arrives in the third round, at its first read.
        let text = INPUTS.replace(
            r#"{"tick":3,"sync":"play","inputs":[[0,"0300"]]}"#,
            r#"{"tick":3,"sync":"play","inputs":[[0,"0300"]],"late":[[0,"GAME.EXE+07143","0900",1,[[0,9]],2]]}"#,
        );
        let file = InputsFile::parse(&text).unwrap();
        assert_eq!(file.ticks[2].late[0].round, 2);
        let mut inputs = Inputs::default();
        inputs.set("elapsed", vec![3, 0]);
        inputs.set_late(&file.ticks[2].late);
        assert!(inputs.more_rounds(), "the key comes in a later round");
        assert!(inputs.more_rounds_of(&["elapsed"]) && !inputs.more_rounds_of(&["mouse"]), "its own input's rounds");
        let mut seen = Vec::new();
        for round in 0..3 {
            seen.push(inputs.at_site("GAME.EXE+07143").u16("elapsed"));
            if round < 2 {
                inputs.go_round("key");
            }
        }
        assert_eq!(seen, [3, 3, 9], "the tick's value in the first two rounds, the key in the third");
        assert!(!inputs.more_rounds(), "nothing read in a later round");
        assert_eq!(inputs.current("elapsed"), [9, 0], "now: as the pass last read it (D132)");
        inputs.write("elapsed", 0, &[4]);
        assert_eq!(inputs.current("elapsed"), [4, 0], "with the pass's writes since");
        assert!(Gates::default().hit("key", &["elapsed"], &inputs), "no gate yet: the first hit counts");
        // A gate that holds: the hit doesn't count, and the pass goes round.
        let mut gates = Gates::default();
        inputs.set_late(&file.ticks[2].late);
        gates.ticked("key", &["elapsed"], &inputs);
        assert!(!gates.hit("key", &["elapsed"], &inputs));
        assert!(!gates.hit("key", &["elapsed"], &inputs));
        assert_eq!(inputs.at_site("GAME.EXE+07143").u16("elapsed"), 9, "round 2's first read");
    }

    #[test]
    fn a_gate_counts_a_hit_when_a_source_changed_since_its_last_tick() {
        let inputs = |mouse: &[u8]| Inputs::new([("mouse".to_owned(), mouse.to_vec())].into());
        let mut gates = Gates::default();
        let first = inputs(&[0, 1]);
        assert!(gates.counts("click", &["mouse"], &first), "the first always");
        gates.ticked("click", &["mouse"], &first);
        assert!(!gates.counts("click", &["mouse"], &first), "nothing changed");
        assert!(gates.counts("click", &["mouse"], &inputs(&[1, 1])));
        // The pass's own write is what the next hit sees.
        first.write("mouse", 0, &[9]);
        assert!(gates.counts("click", &["mouse"], &first));
        assert!(gates.counts("other", &["mouse"], &first), "each sync point its own");
    }

    #[test]
    fn a_wait_s_tick_is_where_the_original_went_round_no_more() {
        let inputs = Inputs::new([("mouse".to_owned(), vec![1, 0])].into());
        let mut gates = Gates::default();
        gates.ticked("click", &["mouse"], &inputs);
        let tick = |spun, next| Tick { number: 2, sync: "click", inputs: &inputs, spun, next };
        // The button let go after the pass's last read: the port's gate sees nothing, the original counted it.
        assert!(gates.arrives("click", &["mouse"], &tick(Some(0), Some("click")), 1, 0), "it went round no more");
        assert!(!gates.arrives("click", &["mouse"], &tick(Some(2), Some("click")), 1, 0), "it went round twice more");
        assert!(gates.arrives("click", &["mouse"], &tick(Some(2), Some("click")), 3, 2));
        // The pass ends elsewhere: a release wait on the way isn't the tick, whatever it went round.
        gates.ticked("release", &["mouse"], &inputs);
        assert!(!gates.arrives("release", &["mouse"], &tick(Some(0), Some("click")), 1, 0), "the tick is at click");
        assert!(gates.arrives("release", &["mouse"], &tick(Some(0), Some("click")), 2, 1), "stuck: the pass ends");
        assert!(
            gates.arrives("release", &["mouse"], &tick(Some(9999), Some("click")), 2, 1),
            "click's rounds, not its"
        );
        // A live play (D143): not before a poll, then the tick is there.
        assert!(!gates.arrives("click", &["mouse"], &tick(None, None), 0, 0), "a live play: not before a poll");
        assert!(gates.arrives("click", &["mouse"], &tick(None, None), 1, 0), "after one");
    }
}
