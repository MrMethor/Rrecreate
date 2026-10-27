//! The knowledge files (DESIGN.md §9, `docs/formats.md`): named functions and data as TSV tables,
//! one item per line sorted by ID; types and ticks as TOML.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

/// The knowledge changes one `rrc` at a time: a command that changes it holds this from loading it to
/// saving it, so two running at once don't lose each other's changes. A lock left by an `rrc` that
/// stopped is taken over after a minute (changes take moments).
pub struct EditLock {
    path: PathBuf,
}

impl EditLock {
    /// Takes the lock in `state`, the port's state folder (`.rrc`).
    pub fn take(state: &Path) -> Result<EditLock, String> {
        let path = state.join("knowledge.lock");
        let started = std::time::Instant::now();
        loop {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(EditLock { path }),
                // On Windows, a lock being removed can't be created again for a moment: access is denied.
                Err(e)
                    if e.kind() == std::io::ErrorKind::AlreadyExists
                        || e.kind() == std::io::ErrorKind::PermissionDenied =>
                {
                    let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
                    if age.is_some_and(|age| age.as_secs() >= 60) {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if started.elapsed().as_secs() >= 30 {
                        return Err(match path.exists() {
                            true => format!(
                                "another rrc is changing the knowledge ({}; remove it if none runs)",
                                path.display()
                            ),
                            false => format!("{}: {e}", path.display()),
                        });
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
    }
}

impl Drop for EditLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// What every named item carries: who decided it and from what.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub name: String,
    /// `human`, `agent:<model>`, `ghidra`, `library:<pack>`, `prior-art:<source>`, `import:<source>`, `map`.
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
    #[serde(default, rename = "pointer", skip_serializing_if = "Vec::is_empty")]
    pub pointers: Vec<Pointer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bits: Vec<Bits>,
}

/// A pointer type (D72): its bytes read as what they point to, the ID there or, with `into`, the
/// index of an element of an array.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pointer {
    pub name: String,
    /// `near`: an offset of `size` bytes (2 or 4) from `base`; `far`: an offset, then a segment (4
    /// bytes), the address segment × 16 + offset (real mode).
    pub kind: String,
    #[serde(default)]
    pub size: u32,
    /// A near pointer's: the ID its offset 0 is (its segment's start; `MEM+00000` for a flat one).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base: String,
    /// Named data, an array: the pointer read as the index of the element it points to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub into: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provenance: String,
}

/// Packed bits (D72): a number of `size` bytes whose bits are fields, read as an object of them.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Bits {
    pub name: String,
    pub size: u32,
    #[serde(default)]
    pub fields: Vec<BitField>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BitField {
    /// The lowest bit, from 0.
    pub bit: u32,
    #[serde(default = "one")]
    pub width: u32,
    pub name: String,
    /// Empty: an unsigned number; `bool`: one bit, true or false.
    #[serde(default, rename = "type", skip_serializing_if = "String::is_empty")]
    pub ty: String,
}

fn one() -> u32 {
    1
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

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sync {
    pub name: String,
    /// The instruction's ID.
    pub at: String,
    /// A gate (D48): inputs or named data, by name. A hit is a tick only when one of them differs
    /// from what it was at this point's last tick: a loop that polls as fast as the CPU runs ticks
    /// when what it reads changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when_changed: Vec<String>,
    /// Who decided, and from what (as `Decision`'s).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provenance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub evidence: String,
    /// What it is.
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
    /// Who decided, and from what (as `Decision`'s).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provenance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub evidence: String,
    /// What it is.
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

    /// Writes the knowledge files in `dir` whose items changed, tables sorted by ID. A TOML file
    /// keeps the comments at its top; other comments are lost when a file is written.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let before = Knowledge::load(dir).unwrap_or_default();
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
        // A file is written when its items changed, or when it isn't there.
        let write = |file: &str, same: bool, text: String| {
            let path = dir.join(file);
            if same && path.is_file() {
                return Ok(());
            }
            // Written aside and renamed: an rrc stopped midway leaves the file as it was.
            let partial = path.with_extension(format!("partial-{}", std::process::id()));
            std::fs::write(&partial, text).map_err(|e| format!("{}: {e}", partial.display()))?;
            std::fs::rename(&partial, &path).map_err(|e| format!("{}: {e}", path.display()))
        };
        // A TOML file's comments at its top, kept; else a line saying what the file is.
        let top = |file: &str, what: &str| {
            let old = std::fs::read_to_string(dir.join(file)).unwrap_or_default();
            let kept: String = old
                .lines()
                .take_while(|l| l.starts_with('#') || l.trim().is_empty())
                .map(|l| format!("{l}\n"))
                .collect();
            if kept.trim().is_empty() { format!("# {what} (docs/formats.md).\n") } else { kept }
        };
        write(FUNCTIONS, before.functions == self.functions, functions)?;
        write(DATA, before.data == self.data, data)?;
        let types = format!("{}{}", top(TYPES, "Structures and enumerations"), to_toml(&self.types)?);
        write(TYPES, before.types == self.types, types)?;
        let ticks = format!("{}{}", top(TICKS, "Sync points and boundary values"), to_toml(&self.ticks)?);
        write(TICKS, before.ticks == self.ticks, ticks)
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
    /// What's wrong with the knowledge: names taken twice or not names, IDs out of `module`, past
    /// its image's end (`image_size`, when known) or its LE objects' (`objects`, their sizes, when it
    /// is LE), overlapping data, bad tick declarations, malformed types.
    pub fn problems(&self, module: &str, image_size: Option<u32>, objects: Option<&[u32]>) -> Vec<String> {
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
            if id.module() != module && !id.is_memory() {
                problems.push(format!("{id}: not in {module} (or MEM, outside it)"));
            }
            if let Some(size) = image_size
                && id.module() == module
                && id.part().is_none()
                && id.offset() >= size
            {
                problems.push(format!("{id}: past the image's end (+{size:05X})"));
            }
            if let (Some(objects), Some(n)) = (objects, id.part())
                && id.module() == module
            {
                match usize::from(n).checked_sub(1).and_then(|i| objects.get(i)) {
                    Some(&size) if id.offset() >= size => {
                        problems.push(format!("{id}: past object {n}'s end ({size:X}h bytes)"));
                    }
                    Some(_) => {}
                    None => problems.push(format!("{id}: {module} has {} objects", objects.len())),
                }
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
        problems.extend(self.type_problems());
        problems
    }

    /// What's wrong with the pointer and bits types, and type names taken twice.
    fn type_problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let t = &self.types;
        let mut names: Vec<&str> = crate::values::BUILT_IN.to_vec();
        let all = t.structs.iter().map(|s| &s.name).chain(t.enums.iter().map(|e| &e.name));
        for name in all.chain(t.pointers.iter().map(|p| &p.name)).chain(t.bits.iter().map(|b| &b.name)) {
            if names.contains(&name.as_str()) {
                problems.push(format!("type {name}: named twice, or a built-in type's name"));
            }
            names.push(name);
        }
        for p in &t.pointers {
            let wrong = |e: String| format!("pointer {}: {e}", p.name);
            match (p.kind.as_str(), p.size) {
                ("near", 2 | 4) => match parse_id(&p.base) {
                    Ok(_) => {}
                    Err(_) if p.base.is_empty() => problems.push(wrong("a near pointer needs a base (an ID)".into())),
                    Err(e) => problems.push(wrong(format!("base: {e}"))),
                },
                ("near", n) => problems.push(wrong(format!("a near pointer is 2 or 4 bytes, not {n}"))),
                ("far", 0 | 4) => {}
                ("far", n) => problems.push(wrong(format!("a far pointer is 4 bytes, not {n}"))),
                (kind, _) => problems.push(wrong(format!("kind {kind:?}: near or far"))),
            }
            if !p.into.is_empty() {
                let array = self.named(&p.into).and_then(|id| self.data.get(id)).map(|d| d.ty.as_str());
                match array {
                    None => problems.push(wrong(format!("into: no data named {}", p.into))),
                    Some(ty) if !ty.ends_with(']') => {
                        problems.push(wrong(format!("into: {} isn't an array ({ty:?})", p.into)));
                    }
                    Some(_) => {}
                }
            }
        }
        for b in &t.bits {
            if ![1, 2, 4].contains(&b.size) {
                problems.push(format!("bits {}: 1, 2 or 4 bytes, not {}", b.name, b.size));
            }
            let mut taken = 0u64;
            for f in &b.fields {
                let wrong = |e: &str| format!("bits {}: field {}: {e}", b.name, f.name);
                if f.width == 0 || f.bit + f.width > 8 * b.size {
                    problems.push(wrong("runs past its size"));
                    continue;
                }
                let mask = ((1u64 << f.width) - 1) << f.bit;
                if taken & mask != 0 {
                    problems.push(wrong("overlaps another"));
                }
                taken |= mask;
                match f.ty.as_str() {
                    "" => {}
                    "bool" if f.width == 1 => {}
                    "bool" => problems.push(wrong("a bool is one bit")),
                    ty => problems.push(wrong(&format!("type {ty:?}: none (a number) or bool"))),
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
            provenance: "human".into(),
            confidence: Some(0.9),
            ..Sync::default()
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
        let problems = k.problems("GAME.EXE", Some(0x30000), None);
        assert!(problems.is_empty(), "{problems:?}");
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
            ..Sync::default()
        });
        k.ticks.inputs.push(Input { name: "pad".into(), device: "gamepad".into(), ..Input::default() });
        let problems = k.problems("GAME.EXE", Some(0x201), None);
        assert!(problems.iter().any(|p| p.contains("nothing is neither")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("no device \"gamepad\"")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("twice names both")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("overlaps GAME.EXE+00200")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("isn't a name")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("not in GAME.EXE")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("GAME.EXE+00202: past the image's end")), "{problems:?}");

        // An LE program's IDs, against its objects' sizes.
        let mut k = Knowledge::default();
        k.functions.insert(id("FLAT.EXE#1+00010"), decision("square"));
        k.functions.insert(id("FLAT.EXE#1+03181"), decision("past_it"));
        k.data.insert(id("FLAT.EXE#3+00000"), Datum { decision: decision("nowhere"), size: None, ty: String::new() });
        let problems = k.problems("FLAT.EXE", None, Some(&[0x3181, 0x10590]));
        assert_eq!(
            problems,
            ["FLAT.EXE#1+03181: past object 1's end (3181h bytes)", "FLAT.EXE#3+00000: FLAT.EXE has 2 objects"]
        );
    }

    #[test]
    fn pointer_and_bits_types_are_checked() {
        let mut k = Knowledge::default();
        k.data.insert(id("GAME.EXE+00200"), Datum { decision: decision("score"), size: Some(2), ty: "u16".into() });
        let pointer = |name: &str, kind: &str, size, base: &str, into: &str| Pointer {
            name: name.into(),
            kind: kind.into(),
            size,
            base: base.into(),
            into: into.into(),
            provenance: String::new(),
        };
        k.types.pointers = vec![
            pointer("good", "near", 2, "GAME.EXE+00000", ""),
            pointer("no_base", "near", 2, "", ""),
            pointer("wide", "far", 2, "", ""),
            pointer("odd", "huge", 4, "", ""),
            pointer("into_number", "near", 2, "GAME.EXE+00000", "score"),
            pointer("u16", "far", 4, "", ""),
        ];
        let bit = |bit, width, name: &str, ty: &str| BitField { bit, width, name: name.into(), ty: ty.into() };
        k.types.bits.push(Bits {
            name: "flags".into(),
            size: 1,
            fields: vec![bit(0, 2, "a", "bool"), bit(1, 1, "b", ""), bit(7, 2, "c", ""), bit(3, 1, "d", "i8")],
            provenance: String::new(),
        });
        let problems = k.problems("GAME.EXE", None, None);
        let expected = [
            "type u16: named twice, or a built-in type's name",
            "pointer no_base: a near pointer needs a base (an ID)",
            "pointer wide: a far pointer is 4 bytes, not 2",
            "pointer odd: kind \"huge\": near or far",
            "pointer into_number: into: score isn't an array (\"u16\")",
            "bits flags: field a: a bool is one bit",
            "bits flags: field b: overlaps another",
            "bits flags: field c: runs past its size",
            "bits flags: field d: type \"i8\": none (a number) or bool",
        ];
        assert_eq!(problems, expected);
    }

    #[test]
    fn changes_made_at_once_are_all_kept() {
        // Two rrc changing the knowledge at the same time, each taking the lock around its change.
        let dir = std::env::temp_dir().join(format!("rrc-knowledge-lock-{}", std::process::id()));
        let (knowledge, state) = (dir.join("knowledge"), dir.join(".rrc"));
        std::fs::create_dir_all(&state).unwrap();
        let change = |first: u32| {
            for n in first..first + 40 {
                let _lock = EditLock::take(&state).unwrap();
                let mut k = Knowledge::load(&knowledge).unwrap_or_default();
                k.functions.insert(Id::new("GAME.EXE", None, n).unwrap(), decision(&format!("f{n}")));
                k.save(&knowledge).unwrap();
            }
        };
        std::thread::scope(|s| {
            s.spawn(|| change(0));
            s.spawn(|| change(1000));
        });
        assert_eq!(Knowledge::load(&knowledge).unwrap().functions.len(), 80);
        assert!(!state.join("knowledge.lock").exists(), "released");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saving_writes_what_changed_and_keeps_a_toml_file_s_top_comments() {
        let dir = std::env::temp_dir().join(format!("rrc-knowledge-comments-{}", std::process::id()));
        let mut k = Knowledge::default();
        k.ticks.syncs.push(Sync { name: "play".into(), at: "GAME.EXE+00120".into(), ..Sync::default() });
        k.save(&dir).unwrap();
        let notes = "# Sync points.\n#\n# The play loop waits for the timer here.\n\n";
        let ticks = std::fs::read_to_string(dir.join(TICKS)).unwrap();
        let ticks = format!("{notes}{}", ticks.lines().skip(1).map(|l| format!("{l}\n")).collect::<String>());
        std::fs::write(dir.join(TICKS), &ticks).unwrap();
        std::fs::write(dir.join(DATA), format!("{DATA_HEADER}\n# a comment of its own\n")).unwrap();
        k.functions.insert(id("GAME.EXE+00120"), decision("play_loop"));
        k.save(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(TICKS)).unwrap(), ticks, "unchanged: not written");
        assert!(std::fs::read_to_string(dir.join(DATA)).unwrap().contains("a comment of its own"));
        k.ticks.syncs[0].note = "where the wait returns".into();
        k.save(&dir).unwrap();
        let written = std::fs::read_to_string(dir.join(TICKS)).unwrap();
        assert!(written.starts_with(notes) && written.contains("where the wait returns"), "{written}");
        std::fs::remove_dir_all(&dir).ok();
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
