//! The knowledge files (DESIGN.md §9, `docs/formats.md`): named functions and data as TSV tables,
//! one item per line sorted by ID; types and ticks as TOML.

use std::collections::BTreeMap;
use std::path::Path;

use ids::Id;

/// Named functions.
pub const FUNCTIONS: &str = "functions.tsv";
pub const FUNCTIONS_HEADER: &str = "# id\tname\tprovenance\tconfidence\tevidence";
/// Named data.
pub const DATA: &str = "data.tsv";
pub const DATA_HEADER: &str = "# id\tname\tsize\ttype\tprovenance\tconfidence\tevidence";
/// Structures and enumerations.
pub const TYPES: &str = "types.toml";
/// Sync points and boundary values.
pub const TICKS: &str = "ticks.toml";
/// The input devices a boundary value can be (D49), with the bytes of their state (docs/formats.md).
pub const DEVICES: [(&str, u32); 2] = [("mouse", 46), ("joystick", 12)];

/// What every named item carries: who decided it and from what.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub name: String,
    /// `human`, `agent:<model>`, `ghidra`, `library:<pack>`, `prior-art:<source>`, `import:<source>`.
    pub provenance: String,
    pub confidence: Option<f64>,
    /// Free text: the IDs, sessions or sources that support it.
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Datum {
    pub decision: Decision,
    /// Bytes, when known.
    pub size: Option<u32>,
    /// A type from `types.toml` or a primitive (`u8`, `i16`, `u32`, `u8[256]`, ...), when known.
    pub ty: String,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Types {
    #[serde(default, rename = "struct", skip_serializing_if = "Vec::is_empty")]
    pub structs: Vec<Struct>,
    #[serde(default, rename = "enum", skip_serializing_if = "Vec::is_empty")]
    pub enums: Vec<Enum>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Struct {
    pub name: String,
    pub size: u32,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Field {
    pub offset: u32,
    pub size: u32,
    pub name: String,
    #[serde(default, rename = "type", skip_serializing_if = "String::is_empty")]
    pub ty: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Enum {
    pub name: String,
    pub size: u32,
    pub values: BTreeMap<String, i64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provenance: String,
}

/// Where ticks end and what they read (D15).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Ticks {
    /// Sync points: instructions where a tick ends (one per loop: menus, play, demo playback).
    #[serde(default, rename = "sync", skip_serializing_if = "Vec::is_empty")]
    pub syncs: Vec<Sync>,
    /// Boundary values: what the original's logic reads at each tick (key states, elapsed time).
    #[serde(default, rename = "input", skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<Input>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sync {
    pub name: String,
    /// The instruction's ID.
    pub at: String,
    /// A gate (D48): inputs or named data, by name. A hit is a tick only when one of them differs
    /// from what it was at this point's last tick: a loop that polls as fast as the CPU runs ticks
    /// when what it reads changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when_changed: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    pub name: String,
    /// The value's ID; or
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at: String,
    /// an input device's state (D49): `mouse` or `joystick` (`DEVICES`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device: String,
    /// Bytes (a device's are known).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub size: u32,
    /// Read only at these sync points (by name), else at all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sync: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl Input {
    /// Its size: a device's state, or the declared one.
    pub fn bytes(&self) -> u32 {
        DEVICES.iter().find(|(d, _)| *d == self.device).map_or(self.size, |(_, size)| *size)
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Knowledge {
    pub functions: BTreeMap<Id, Decision>,
    pub data: BTreeMap<Id, Datum>,
    pub types: Types,
    pub ticks: Ticks,
}

impl Knowledge {
    /// Reads the knowledge files in `dir` (missing ones are empty).
    pub fn load(dir: &Path) -> Result<Knowledge, String> {
        let table = |file: &str| -> Result<Vec<(usize, Vec<String>)>, String> {
            let path = dir.join(file);
            if !path.is_file() {
                return Ok(Vec::new());
            }
            let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(rows(&text))
        };
        let mut knowledge = Knowledge::default();
        for (line, row) in table(FUNCTIONS)? {
            let at = |e: String| format!("{FUNCTIONS}:{line}: {e}");
            let id = parse_id(&row[0]).map_err(at)?;
            let decision = decision(&row, 1).map_err(at)?;
            if knowledge.functions.insert(id.clone(), decision).is_some() {
                return Err(at(format!("{id} twice")));
            }
        }
        for (line, row) in table(DATA)? {
            let at = |e: String| format!("{DATA}:{line}: {e}");
            let id = parse_id(&row[0]).map_err(at)?;
            let size = match row.get(2).map(String::as_str).unwrap_or("") {
                "" => None,
                n => Some(n.parse().map_err(|_| at(format!("bad size {n:?}")))?),
            };
            let ty = row.get(3).cloned().unwrap_or_default();
            let mut fields = row.clone();
            fields.drain(2..4.min(fields.len()));
            let datum = Datum { decision: decision(&fields, 1).map_err(at)?, size, ty };
            if knowledge.data.insert(id.clone(), datum).is_some() {
                return Err(at(format!("{id} twice")));
            }
        }
        for (file, slot) in [(TYPES, 0), (TICKS, 1)] {
            let path = dir.join(file);
            if path.is_file() {
                match slot {
                    0 => knowledge.types = crate::repo::read_toml(&path)?,
                    _ => knowledge.ticks = crate::repo::read_toml(&path)?,
                }
            }
        }
        Ok(knowledge)
    }

    /// Writes the knowledge files in `dir`, tables sorted by ID.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut functions = format!("{FUNCTIONS_HEADER}\n");
        for (id, d) in &self.functions {
            functions.push_str(&line(&[
                &id.to_string(),
                &d.name,
                &d.provenance,
                &confidence(d.confidence),
                &d.evidence,
            ]));
        }
        let mut data = format!("{DATA_HEADER}\n");
        for (id, datum) in &self.data {
            let d = &datum.decision;
            let size = datum.size.map(|s| s.to_string()).unwrap_or_default();
            data.push_str(&line(&[
                &id.to_string(),
                &d.name,
                &size,
                &datum.ty,
                &d.provenance,
                &confidence(d.confidence),
                &d.evidence,
            ]));
        }
        let write = |file: &str, text: String| {
            let path = dir.join(file);
            std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
        };
        write(FUNCTIONS, functions)?;
        write(DATA, data)?;
        write(TYPES, format!("# Structures and enumerations (docs/formats.md).\n{}", to_toml(&self.types)?))?;
        write(TICKS, format!("# Sync points and boundary values (docs/formats.md).\n{}", to_toml(&self.ticks)?))
    }

    /// The function or data item named `name`.
    pub fn named(&self, name: &str) -> Option<&Id> {
        let function = self.functions.iter().find(|(_, d)| d.name == name);
        function.map(|(id, _)| id).or_else(|| self.data.iter().find(|(_, d)| d.decision.name == name).map(|(id, _)| id))
    }

    /// What a sync point's `when_changed` names: an input, else named data with a size (as an
    /// input).
    pub fn gate_item(&self, name: &str) -> Result<Input, String> {
        if let Some(input) = self.ticks.inputs.iter().find(|i| i.name == name) {
            return Ok(input.clone());
        }
        match self.data.iter().find(|(_, d)| d.decision.name == name) {
            Some((id, Datum { size: Some(size), .. })) => {
                Ok(Input { name: name.into(), at: id.to_string(), size: *size, ..Input::default() })
            }
            Some(_) => Err(format!("when_changed: {name} has no size")),
            None => Err(format!("when_changed: {name} is neither an input nor named data")),
        }
    }

    /// What is wrong with the knowledge: names used twice, overlapping data, data outside the
    /// image (`image_size` bytes, when known), bad tick declarations, malformed types.
    pub fn problems(&self, module: &str, image_size: Option<u32>) -> Vec<String> {
        let mut problems = Vec::new();
        let mut names: BTreeMap<&str, &Id> = BTreeMap::new();
        let decisions = self.functions.iter().chain(self.data.iter().map(|(id, d)| (id, &d.decision)));
        for (id, d) in decisions {
            if let Some(other) = names.insert(&d.name, id) {
                problems.push(format!("{} names both {other} and {id}", d.name));
            }
            if !is_name(&d.name) {
                problems.push(format!("{id}: {:?} isn't a name", d.name));
            }
            if id.module() != module {
                problems.push(format!("{id}: not in {module}"));
            }
            if let Some(size) = image_size
                && id.part().is_none()
                && id.offset() >= size
            {
                problems.push(format!("{id}: past the image's end (+{size:05X})"));
            }
        }
        let mut previous: Option<(&Id, u32)> = None;
        for (id, datum) in &self.data {
            if let Some((other, end)) = previous
                && other.module() == id.module()
                && other.part() == id.part()
                && id.offset() < end
            {
                problems.push(format!("{id} ({}) overlaps {other}", datum.decision.name));
            }
            if datum.size == Some(0) {
                problems.push(format!("{id}: size 0"));
            }
            previous = Some((id, id.offset() + datum.size.unwrap_or(1)));
        }
        for s in &self.ticks.syncs {
            if let Err(e) = parse_id(&s.at) {
                problems.push(format!("sync {}: {e}", s.name));
            }
            for name in &s.when_changed {
                if let Err(e) = self.gate_item(name) {
                    problems.push(format!("sync {}: {e}", s.name));
                }
            }
        }
        for i in &self.ticks.inputs {
            if i.device.is_empty() {
                if let Err(e) = parse_id(&i.at) {
                    problems.push(format!("input {}: {e}", i.name));
                }
                if i.size == 0 {
                    problems.push(format!("input {}: size 0", i.name));
                }
            } else if !i.at.is_empty() {
                problems.push(format!("input {}: an ID or a device, not both", i.name));
            } else if !DEVICES.iter().any(|(d, _)| *d == i.device) {
                problems.push(format!("input {}: no device {:?} (mouse or joystick)", i.name, i.device));
            } else if i.size != 0 && i.size != i.bytes() {
                problems.push(format!("input {}: a {} is {} bytes", i.name, i.device, i.bytes()));
            }
            for sync in i.sync.iter().filter(|s| !self.ticks.syncs.iter().any(|t| t.name == **s)) {
                problems.push(format!("input {}: no sync point {sync}", i.name));
            }
        }
        for s in &self.types.structs {
            for f in &s.fields {
                if f.offset + f.size > s.size {
                    problems.push(format!("struct {}: field {} runs past its size", s.name, f.name));
                }
            }
        }
        problems
    }
}

/// A C-style name, with the `@` and `$` compilers' runtime names have (`F_LXMUL@`).
pub fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    let other = |c: char| c.is_ascii_alphanumeric() || "_@$".contains(c);
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(other)
}

pub fn parse_id(text: &str) -> Result<Id, String> {
    text.parse().map_err(|e: ids::ParseIdError| e.to_string())
}

/// The lines of a table that hold items, with their line numbers, split at tabs.
fn rows(text: &str) -> Vec<(usize, Vec<String>)> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|(i, line)| (i + 1, line.split('\t').map(str::to_owned).collect()))
        .collect()
}

/// The decision in `row` from column `from` on: name, provenance, confidence, evidence.
fn decision(row: &[String], from: usize) -> Result<Decision, String> {
    let column = |i: usize| row.get(from + i).cloned().unwrap_or_default();
    let name = column(0);
    if name.is_empty() {
        return Err("no name".into());
    }
    let provenance = column(1);
    if provenance.is_empty() {
        return Err(format!("{name}: no provenance"));
    }
    let confidence = match column(2).as_str() {
        "" => None,
        c => Some(c.parse().map_err(|_| format!("{name}: bad confidence {c:?}"))?),
    };
    Ok(Decision { name, provenance, confidence, evidence: column(3) })
}

fn confidence(c: Option<f64>) -> String {
    c.map(|c| c.to_string()).unwrap_or_default()
}

/// A table line: the fields, with tabs and line breaks in them made spaces.
fn line(fields: &[&str]) -> String {
    let clean: Vec<String> = fields.iter().map(|f| f.replace(['\t', '\n', '\r'], " ")).collect();
    format!("{}\n", clean.join("\t").trim_end())
}

fn to_toml<T: serde::Serialize>(value: &T) -> Result<String, String> {
    toml::to_string_pretty(value).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> Id {
        text.parse().unwrap()
    }

    fn decision(name: &str) -> Decision {
        Decision { name: name.into(), provenance: "human".into(), confidence: Some(0.8), evidence: String::new() }
    }

    #[test]
    fn knowledge_reads_back_as_written() {
        let dir = std::env::temp_dir().join(format!("rrc-knowledge-{}", std::process::id()));
        let mut k = Knowledge::default();
        k.functions
            .insert(id("GAME.EXE+00120"), Decision { evidence: "GAME.EXE+00200 calls it".into(), ..decision("Play") });
        k.functions.insert(id("GAME.EXE+0001A"), decision("main"));
        k.data.insert(id("GAME.EXE+20000"), Datum { decision: decision("elapsed"), size: Some(2), ty: "u16".into() });
        k.data.insert(id("GAME.EXE+20002"), Datum { decision: decision("map"), size: None, ty: String::new() });
        k.ticks.syncs.push(Sync {
            name: "play".into(),
            at: "GAME.EXE+00120".into(),
            when_changed: vec!["elapsed".into()],
            note: String::new(),
        });
        k.ticks.inputs.push(Input {
            name: "elapsed".into(),
            at: "GAME.EXE+20000".into(),
            size: 2,
            note: "x".into(),
            ..Input::default()
        });
        k.ticks.inputs.push(Input { name: "mouse".into(), device: "mouse".into(), ..Input::default() });
        k.types.structs.push(Struct {
            name: "actor".into(),
            size: 4,
            fields: vec![Field { offset: 0, size: 2, name: "x".into(), ty: "i16".into() }],
            provenance: "human".into(),
        });
        k.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join(FUNCTIONS)).unwrap();
        assert!(text.find("main").unwrap() < text.find("Play").unwrap(), "sorted by ID:\n{text}");
        assert_eq!(Knowledge::load(&dir).unwrap(), k);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(k.named("elapsed"), Some(&id("GAME.EXE+20000")));
        assert!(k.problems("GAME.EXE", Some(0x30000)).is_empty(), "{:?}", k.problems("GAME.EXE", Some(0x30000)));
    }

    #[test]
    fn problems_are_named() {
        let mut k = Knowledge::default();
        k.functions.insert(id("GAME.EXE+00100"), decision("twice"));
        k.data.insert(id("GAME.EXE+00200"), Datum { decision: decision("twice"), size: Some(4), ty: String::new() });
        k.data.insert(id("GAME.EXE+00202"), Datum { decision: decision("inside"), size: Some(2), ty: String::new() });
        k.data.insert(id("OTHER.EXE+00000"), Datum { decision: decision("bad name"), size: None, ty: String::new() });
        k.ticks.syncs.push(Sync {
            name: "menu".into(),
            at: "GAME.EXE+00100".into(),
            when_changed: vec!["nothing".into()],
            note: String::new(),
        });
        k.ticks.inputs.push(Input { name: "pad".into(), device: "gamepad".into(), ..Input::default() });
        let problems = k.problems("GAME.EXE", Some(0x201));
        assert!(problems.iter().any(|p| p.contains("nothing is neither")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("no device \"gamepad\"")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("twice names both")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("overlaps GAME.EXE+00200")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("isn't a name")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("not in GAME.EXE")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("GAME.EXE+00202: past the image's end")), "{problems:?}");
    }

    #[test]
    fn a_table_may_leave_out_trailing_columns() {
        let k_text = format!("{FUNCTIONS_HEADER}\nGAME.EXE+00010\tmain\thuman\n");
        let dir = std::env::temp_dir().join(format!("rrc-knowledge-short-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FUNCTIONS), k_text).unwrap();
        let k = Knowledge::load(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        let d = &k.functions[&id("GAME.EXE+00010")];
        assert_eq!((d.name.as_str(), d.confidence, d.evidence.as_str()), ("main", None, ""));
    }
}
