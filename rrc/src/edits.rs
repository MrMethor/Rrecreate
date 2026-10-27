//! A scenario session's edits (DESIGN.md §8, D59, D64): values written into the original's memory at
//! ticks, so a session sets up the scenario it needs instead of playing to it. Each names named data
//! (or an ID), a path into it as its type reads (`enemies[2].alive`) and a value; it becomes bytes at
//! IDs, which Clockwork writes when the session's tick is counted, before that pass reads them. Every
//! run of the session (recording, a watch, a snapshot's) gets them, and the trace lists them.

use ids::Id;
use serde_json::{Value, json};

use crate::knowledge::Knowledge;
use crate::session::Edit;

/// An edit's bytes at an ID.
#[derive(Debug, Clone, PartialEq)]
pub struct Write {
    pub tick: u64,
    pub id: Id,
    pub bytes: Vec<u8>,
    /// The edit as the session has it (`lives = 1`).
    pub what: String,
}

/// The session's edits as writes, or what is wrong with one.
pub fn resolve(k: &Knowledge, edits: &[Edit]) -> Result<Vec<Write>, String> {
    let mut writes = Vec::new();
    for edit in edits {
        let what = format!("{} = {}", edit.at, edit.value);
        let wrong = |e: String| format!("edit at tick {}, {what}: {e}", edit.tick);
        if edit.tick == 0 {
            return Err(wrong("ticks count from 1".into()));
        }
        let (name, path) = split(&edit.at);
        let id = crate::mapping::item(k, name).map_err(wrong)?;
        let ty = match (&edit.ty, k.data.get(&id)) {
            (ty, _) if !ty.is_empty() => ty.clone(),
            (_, Some(d)) if !d.ty.is_empty() => d.ty.clone(),
            _ => return Err(wrong(format!("{name}: no type: give one (type), or one in data.tsv"))),
        };
        let (offset, leaf) =
            crate::values::locate(&ty, path, &k.types).ok_or_else(|| wrong(format!("{ty} has no {path}")))?;
        let reader = crate::values::Reader::of(k);
        for (at, bytes) in crate::values::encode(&edit.value, &leaf, &reader).map_err(wrong)? {
            let id = Id::new(id.module(), id.part(), id.offset() + offset + at).map_err(|e| wrong(e.to_string()))?;
            writes.push(Write { tick: edit.tick, id, bytes, what: what.clone() });
        }
    }
    Ok(writes)
}

/// `name.path` as the item and the path into it; an ID's path starts after its offset
/// (`GAME.EXE+00100[2]`).
pub(crate) fn split(at: &str) -> (&str, &str) {
    let from = at.find('+').unwrap_or(0);
    let end = at[from..].find(['[', '.']).map_or(at.len(), |e| from + e);
    (&at[..end], &at[end..])
}

/// The writes as the trace's header lists them, and as what makes the run the same.
pub fn listed(writes: &[Write]) -> Vec<Value> {
    writes
        .iter()
        .map(|w| json!({"tick": w.tick, "at": w.id.to_string(), "hex": hex(&w.bytes), "edit": w.what}))
        .collect()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::{Datum, Decision, Field, Struct};

    #[test]
    fn edits_become_bytes_at_ids() {
        let mut k = Knowledge::default();
        let datum = |name: &str, size, ty: &str| Datum {
            decision: Decision {
                name: name.into(),
                provenance: "human".into(),
                confidence: None,
                evidence: String::new(),
            },
            size: Some(size),
            ty: ty.into(),
        };
        k.data.insert("GAME.EXE+00100".parse().unwrap(), datum("lives", 1, "u8"));
        k.data.insert("GAME.EXE+00200".parse().unwrap(), datum("enemies", 8, "enemy[2]"));
        let field = |offset, size, name: &str, ty: &str| Field { offset, size, name: name.into(), ty: ty.into() };
        k.types.structs.push(Struct {
            name: "enemy".into(),
            size: 4,
            fields: vec![field(0, 2, "x", "i16"), field(2, 1, "alive", "u8")],
            provenance: String::new(),
        });
        let edit =
            |at: &str, ty: &str, value| Edit { tick: 5, at: at.into(), ty: ty.into(), value, note: String::new() };
        let writes = resolve(&k, &[edit("lives", "", json!(1)), edit("enemies[1].alive", "", json!(0))]).unwrap();
        let at: Vec<(String, Vec<u8>)> = writes.iter().map(|w| (w.id.to_string(), w.bytes.clone())).collect();
        assert_eq!(at, [("GAME.EXE+00100".into(), vec![1]), ("GAME.EXE+00206".into(), vec![0])]);
        let by_id = resolve(&k, &[edit("GAME.EXE+00300[1]", "u16[2]", json!(-1))]);
        assert!(by_id.unwrap_err().contains("isn't in 0 to 65535"));
        let by_id = resolve(&k, &[edit("GAME.EXE+00300[1]", "u16[2]", json!(7))]).unwrap();
        assert_eq!((by_id[0].id.to_string(), by_id[0].bytes.clone()), ("GAME.EXE+00302".into(), vec![7, 0]));
        assert!(resolve(&k, &[edit("enemies[2]", "", json!({}))]).unwrap_err().contains("has no [2]"));
        assert!(resolve(&k, &[edit("nothing", "", json!(1))]).unwrap_err().contains("no data named nothing"));
    }
}
