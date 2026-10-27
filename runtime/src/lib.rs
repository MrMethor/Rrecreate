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

mod files;

use std::collections::BTreeMap;
use std::process::ExitCode;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

pub use files::{InputsFile, PortState, StartState};

/// A port's game logic: its state, stepped one tick at a time (D21: deterministic, no clock, no
/// hardware). The state serializes to the values `rrc` compares: the fields `mapping.toml` names,
/// by their serde names (`player.x` is the `x` of the field `player`).
pub trait Simulation: Serialize + DeserializeOwned {
    /// The state at the session's first tick: what the original holds when it first reaches a sync
    /// point (its start-up already run).
    fn start() -> Self;

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
}

/// The boundary values at a tick (D15, D49): each input's bytes, as the original's memory or a
/// device held them at the sync point; and what the pass read of them later, where they had changed
/// meanwhile (D68).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inputs {
    values: BTreeMap<String, Vec<u8>>,
    /// By (input, the ID of the instruction that read it): the value it read.
    late: BTreeMap<(String, String), Vec<u8>>,
}

impl Inputs {
    pub fn new(values: BTreeMap<String, Vec<u8>>) -> Inputs {
        Inputs { values, late: BTreeMap::new() }
    }

    /// The inputs as the original's instruction `site` (its ID, `GAME.EXE+04BB9`) read them: where the
    /// pass read an input there after it had changed since the sync point (a clock read again after
    /// the frame was drawn, a key pressed meanwhile), the value it read; else the tick's. The port reads
    /// such an input through this, at the place that does what that instruction did. (A live platform
    /// reads its clock or device again here.)
    pub fn at_site(&self, site: &str) -> Inputs {
        let mut values = self.values.clone();
        for ((name, at), value) in &self.late {
            if at == site {
                values.insert(name.clone(), value.clone());
            }
        }
        Inputs { values, late: BTreeMap::new() }
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

    /// This tick's late reads, replacing the last tick's.
    pub(crate) fn set_late(&mut self, late: &[(String, String, Vec<u8>)]) {
        self.late = late.iter().map(|(name, site, value)| ((name.clone(), site.clone()), value.clone())).collect();
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
}

impl Request {
    /// `run --inputs FILE --out FILE [--transplant FILE | --resume FILE] [--save-at TICK --save FILE]
    /// [--to TICK]`
    pub fn parse(args: &[String]) -> Result<Request, String> {
        let usage = "usage: PORT run --inputs FILE --out FILE [--transplant FILE | --resume FILE] \
                     [--save-at TICK --save FILE] [--to TICK]";
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    match Request::parse(&args).and_then(|request| run::<S>(&request)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("port: error: {message}");
            ExitCode::from(1)
        }
    }
}

/// Runs the port as `request` says: from its start state (or a transplant, or a saved state) through
/// the session's ticks, writing its state at every tick before it steps.
pub fn run<S: Simulation>(request: &Request) -> Result<(), String> {
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
        _ => (S::start(), 1),
    };
    let mut out = files::TraceWriter::create(&request.out, &inputs.session, first)?;
    let mut current = Inputs::default();
    let mut last = 0;
    for line in &inputs.ticks {
        for (name, bytes) in &line.inputs {
            current.set(name, bytes.clone());
        }
        current.set_late(&line.late);
        if line.tick < first {
            continue;
        }
        if request.to.is_some_and(|to| line.tick > to) {
            break;
        }
        // A scenario's edit at this tick (D64): the original's values of the fields it set.
        if !line.set.is_empty() {
            let mut value = serialize(&state)?;
            for (path, v) in &line.set {
                files::set_path(&mut value, path, v.clone())?;
            }
            state = S::transplant(value)?;
        }
        let value = serialize(&state)?;
        out.tick(line.tick, value.clone())?;
        if let Some((at, file)) = &request.save
            && *at == line.tick
        {
            PortState { tick: line.tick, state: value }.write(file)?;
        }
        state.step(&Tick { number: line.tick, sync: &line.sync, inputs: &current });
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
        assert_eq!(lines[2], serde_json::json!({"tick": 2, "state": {"count": 100, "keys": [9]}}), "set, then written");
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
            r#"{"tick":3,"sync":"play","inputs":[[0,"0300"]],"late":[[0,"GAME.EXE+04BB9","0600"]]}"#,
        );
        let file = InputsFile::parse(&text).unwrap();
        assert_eq!(file.ticks[2].late, [("elapsed".to_owned(), "GAME.EXE+04BB9".to_owned(), vec![6, 0])]);
        let mut inputs = Inputs::default();
        inputs.set("elapsed", vec![3, 0]);
        inputs.set_late(&file.ticks[2].late);
        assert_eq!(inputs.u16("elapsed"), 3, "the tick's value");
        assert_eq!(inputs.at_site("GAME.EXE+04BB9").u16("elapsed"), 6, "as that instruction read it");
        assert_eq!(inputs.at_site("GAME.EXE+00100").u16("elapsed"), 3, "elsewhere, the tick's");
        inputs.set_late(&[]);
        assert_eq!(inputs.at_site("GAME.EXE+04BB9").u16("elapsed"), 3, "a late read is the tick's alone");
    }

    #[test]
    fn the_command_line() {
        let args = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let r = Request::parse(&args("run --inputs i --out o --save-at 5 --save s --to 9")).unwrap();
        assert_eq!((r.save, r.to), (Some((5, "s".into())), Some(9)));
        assert!(Request::parse(&args("run --inputs i")).is_err());
        assert!(Request::parse(&args("run --inputs i --out o --save-at 5")).is_err());
        assert!(Request::parse(&args("run --inputs i --out o --transplant a --resume b")).is_err());
    }
}
