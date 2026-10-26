//! The files of the port contract (D61, `docs/formats.md`): the session's inputs (`rrc-inputs`, which
//! `rrc` writes), a start state (`rrc-state`, a transplant), the port's own saved state
//! (`rrc-port-state`) and the port's trace (`rrc-port-trace`, which the port writes).

use std::collections::BTreeMap;
use std::io::Write;

use serde_json::{Map, Value, json};

/// The session's inputs, tick by tick.
#[derive(Debug, Default, PartialEq)]
pub struct InputsFile {
    pub session: String,
    pub ticks: Vec<InputsLine>,
}

/// A tick of the inputs file: the inputs whose values changed (a value holds until it changes).
#[derive(Debug, Default, PartialEq)]
pub struct InputsLine {
    pub tick: u64,
    pub sync: String,
    pub inputs: Vec<(String, Vec<u8>)>,
}

impl InputsFile {
    pub fn read(path: &str) -> Result<InputsFile, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        InputsFile::parse(&text).map_err(|e| format!("{path}: {e}"))
    }

    pub fn parse(text: &str) -> Result<InputsFile, String> {
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let header: Value = serde_json::from_str(lines.next().ok_or("empty")?).map_err(|e| e.to_string())?;
        if header["format"] != "rrc-inputs" || header["version"] != 1 {
            return Err(format!("not an inputs file this runtime reads ({} {})", header["format"], header["version"]));
        }
        let names: Vec<String> = header["inputs"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| i["name"].as_str().unwrap_or_default().to_owned())
            .collect();
        let mut file = InputsFile { session: header["session"].as_str().unwrap_or_default().to_owned(), ticks: vec![] };
        for line in lines {
            let v: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
            let Some(tick) = v["tick"].as_u64() else { continue };
            let mut inputs = Vec::new();
            for pair in v["inputs"].as_array().into_iter().flatten() {
                let i = pair[0].as_u64().ok_or("an input's index")? as usize;
                let name = names.get(i).ok_or_else(|| format!("tick {tick}: no input {i}"))?;
                inputs.push((name.clone(), unhex(pair[1].as_str().unwrap_or_default())?));
            }
            file.ticks.push(InputsLine { tick, sync: v["sync"].as_str().unwrap_or_default().to_owned(), inputs });
        }
        Ok(file)
    }
}

/// The original's mapped values at a tick, to start from (a transplant).
#[derive(Debug, PartialEq)]
pub struct StartState {
    pub tick: u64,
    /// By field path (`player.x`).
    pub state: BTreeMap<String, Value>,
}

impl StartState {
    pub fn read(path: &str) -> Result<StartState, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
        if v["format"] != "rrc-state" || v["version"] != 1 {
            return Err(format!("{path}: not a start state this runtime reads"));
        }
        let tick = v["tick"].as_u64().ok_or_else(|| format!("{path}: no tick"))?;
        let state = v["state"].as_object().cloned().unwrap_or_default().into_iter().collect();
        Ok(StartState { tick, state })
    }

    /// `base` (the port's start state, serialized) with every field of this state set at its path.
    pub fn overlay(&self, mut base: Value) -> Result<Value, String> {
        for (path, value) in &self.state {
            let parts: Vec<&str> = path.split('.').collect();
            set(&mut base, &parts, value.clone())
                .map_err(|()| format!("{path}: the port's state has no object there"))?;
        }
        Ok(base)
    }
}

/// Sets `value` at `parts` in `at`, making the objects on the way.
fn set(at: &mut Value, parts: &[&str], value: Value) -> Result<(), ()> {
    let object = at.as_object_mut().ok_or(())?;
    match parts {
        [] => Ok(()),
        [last] => {
            object.insert((*last).to_owned(), value);
            Ok(())
        }
        [first, rest @ ..] => set(object.entry((*first).to_owned()).or_insert_with(|| json!({})), rest, value),
    }
}

/// The port's own state at a tick, as it serializes it: to resume from (a paired checkpoint).
#[derive(Debug, PartialEq)]
pub struct PortState {
    pub tick: u64,
    pub state: Value,
}

impl PortState {
    pub fn read(path: &str) -> Result<PortState, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
        if v["format"] != "rrc-port-state" || v["version"] != 1 {
            return Err(format!("{path}: not a port state this runtime reads"));
        }
        Ok(PortState { tick: v["tick"].as_u64().ok_or("no tick")?, state: v["state"].clone() })
    }

    pub fn write(&self, path: &str) -> Result<(), String> {
        let v = json!({"format": "rrc-port-state", "version": 1, "tick": self.tick, "state": self.state});
        std::fs::write(path, format!("{v}\n")).map_err(|e| format!("{path}: {e}"))
    }
}

/// Writes the port's trace: its state at each tick, the top-level fields that changed (all at the
/// first tick written).
pub(crate) struct TraceWriter {
    file: std::io::BufWriter<std::fs::File>,
    path: String,
    last: Option<Map<String, Value>>,
}

impl TraceWriter {
    pub fn create(path: &str, session: &str, from: u64) -> Result<TraceWriter, String> {
        let file = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
        let mut writer = TraceWriter { file: std::io::BufWriter::new(file), path: path.to_owned(), last: None };
        writer.line(&json!({"format": "rrc-port-trace", "version": 1, "session": session, "from": from}))?;
        Ok(writer)
    }

    pub fn tick(&mut self, tick: u64, state: Value) -> Result<(), String> {
        let state = match state {
            Value::Object(fields) => fields,
            other => Map::from_iter([("state".to_owned(), other)]),
        };
        let changed: Map<String, Value> = match &self.last {
            None => state.clone(),
            Some(last) => {
                state.iter().filter(|(k, v)| last.get(*k) != Some(*v)).map(|(k, v)| (k.clone(), v.clone())).collect()
            }
        };
        self.last = Some(state);
        self.line(&json!({"tick": tick, "state": changed}))
    }

    pub fn end(mut self, last: u64) -> Result<(), String> {
        self.line(&json!({ "end": last }))?;
        self.file.flush().map_err(|e| format!("{}: {e}", self.path))
    }

    fn line(&mut self, value: &Value) -> Result<(), String> {
        writeln!(self.file, "{value}").map_err(|e| format!("{}: {e}", self.path))
    }
}

fn unhex(text: &str) -> Result<Vec<u8>, String> {
    if text.len() % 2 != 0 {
        return Err(format!("odd hex {text:?}"));
    }
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).map_err(|_| format!("bad hex {text:?}")))
        .collect()
}
