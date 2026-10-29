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

/// A tick of the inputs file: the inputs whose values changed (a value holds until it changes), the
/// inputs its pass read late, and the fields a scenario's edit set at this tick (D64), by path, with the
/// original's values.
#[derive(Debug, Default, PartialEq)]
pub struct InputsLine {
    pub tick: u64,
    pub sync: String,
    pub inputs: Vec<(String, Vec<u8>)>,
    pub late: Vec<LateRead>,
    pub set: Vec<(String, Value)>,
}

/// A read of the pass that saw an input otherwise than the read before it there (D68, D93): the input,
/// the reading instruction's ID, which of its reads, the value it read (the tick's with its bytes), and
/// its bytes that differ from what the program should have read, the tick's value with the program's own
/// writes since (D97; none in files from traces before version 8).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LateRead {
    pub name: String,
    pub site: String,
    pub read: u32,
    pub value: Vec<u8>,
    pub bytes: Option<Vec<(usize, u8)>>,
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
            let mut late = Vec::new();
            for read in v["late"].as_array().into_iter().flatten() {
                let i = read[0].as_u64().ok_or("a late read's input")? as usize;
                let name = names.get(i).ok_or_else(|| format!("tick {tick}: no input {i}"))?;
                let site = read[1].as_str().unwrap_or_default().to_owned();
                // An instruction's first read where the file doesn't say (before D93).
                let n = read.get(3).map_or(Some(1), Value::as_u64).ok_or("a late read's number")? as u32;
                let bytes = match read.get(4) {
                    Some(list) => Some(
                        list.as_array()
                            .into_iter()
                            .flatten()
                            .map(|pair| match (pair[0].as_u64(), pair[1].as_u64()) {
                                (Some(offset), Some(value)) => Ok((offset as usize, value as u8)),
                                _ => Err("a late read's bytes are [offset, value]"),
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    ),
                    None => None,
                };
                let value = unhex(read[2].as_str().unwrap_or_default())?;
                late.push(LateRead { name: name.clone(), site, read: n, value, bytes });
            }
            let set =
                v["set"].as_object().into_iter().flatten().map(|(path, value)| (path.clone(), value.clone())).collect();
            let sync = v["sync"].as_str().unwrap_or_default().to_owned();
            file.ticks.push(InputsLine { tick, sync, inputs, late, set });
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
            set_path(&mut base, path, value.clone())?;
        }
        Ok(base)
    }
}

/// A step of a path: a field, or an element.
enum Step<'a> {
    Field(&'a str),
    Element(usize),
}

/// Sets `value` at `path` (`player.x`, `enemies[2].alive`) in a serialized state, making the objects
/// on the way; an element must be there already.
pub fn set_path(state: &mut Value, path: &str, value: Value) -> Result<(), String> {
    let wrong = || format!("{path}: not a path of field names and [index]es");
    let mut steps = Vec::new();
    for part in path.split('.') {
        let (field, mut rest) = part.split_at(part.find('[').unwrap_or(part.len()));
        if field.is_empty() {
            return Err(wrong());
        }
        steps.push(Step::Field(field));
        while let Some(after) = rest.strip_prefix('[') {
            let (index, tail) = after.split_once(']').ok_or_else(wrong)?;
            steps.push(Step::Element(index.parse().map_err(|_| wrong())?));
            rest = tail;
        }
        if !rest.is_empty() {
            return Err(wrong());
        }
    }
    set(state, &steps, value).map_err(|()| format!("{path}: the port's state has no place there"))
}

fn set(at: &mut Value, steps: &[Step], value: Value) -> Result<(), ()> {
    match steps {
        [] => {
            *at = value;
            Ok(())
        }
        [Step::Field(field), rest @ ..] => {
            let object = at.as_object_mut().ok_or(())?;
            set(object.entry((*field).to_owned()).or_insert_with(|| json!({})), rest, value)
        }
        [Step::Element(i), rest @ ..] => set(at.as_array_mut().ok_or(())?.get_mut(*i).ok_or(())?, rest, value),
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
