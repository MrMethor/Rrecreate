//! The state mapping (DESIGN.md §7, D16, D62): `mapping.toml` in the port repository. Each `[[map]]` is
//! a field of the port's state (a path in its serialization, `player.x`) and the original's item it
//! is: a data name or an ID, read as the knowledge's type for it or the one given. `[[ignore]]` is the
//! original's memory that may differ (padding, caches, driver internals), named so that nothing is left
//! out silently. `rrc record` traces every mapped item; `rrc compare` compares them, and a transplant
//! builds the port's state from them.

use ids::Id;

use crate::knowledge::{Knowledge, parse_id};
use crate::repo::{Repo, read_toml};
use crate::values;

pub const MAPPING: &str = "mapping.toml";

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Mapping {
    #[serde(default, rename = "map", skip_serializing_if = "Vec::is_empty")]
    pub maps: Vec<Map>,
    #[serde(default, rename = "ignore", skip_serializing_if = "Vec::is_empty")]
    pub ignores: Vec<Ignore>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Map {
    /// The port's field: a path in its state's serialization.
    pub field: String,
    /// The original's item: a data name or an ID.
    pub at: String,
    /// How to read it; empty: the knowledge's type for the data.
    #[serde(default, rename = "type", skip_serializing_if = "String::is_empty")]
    pub ty: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Ignore {
    /// A data name or an ID.
    pub at: String,
    /// Bytes; 0: the data's size.
    #[serde(default)]
    pub size: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// A mapped field resolved against the knowledge.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapped {
    pub field: String,
    pub id: Id,
    pub ty: String,
    pub size: u32,
}

impl Mapping {
    pub fn load(repo: &Repo) -> Result<Mapping, String> {
        let path = repo.root.join(MAPPING);
        if path.is_file() { read_toml(&path) } else { Ok(Mapping::default()) }
    }

    /// Every mapped field with its ID, type and size, or what is wrong with one.
    pub fn resolve(&self, k: &Knowledge) -> Result<Vec<Mapped>, String> {
        let mut mapped: Vec<Mapped> = Vec::new();
        for m in &self.maps {
            let at = |e: String| format!("{MAPPING}: {}: {e}", m.field);
            if m.field.is_empty() || m.field.split('.').any(str::is_empty) {
                return Err(at("a field is a path of names (player.x)".into()));
            }
            if mapped.iter().any(|other| other.field == m.field) {
                return Err(at("mapped twice".into()));
            }
            let id = item(k, &m.at).map_err(at)?;
            let datum = k.data.get(&id);
            let ty = match (&m.ty, datum) {
                (ty, _) if !ty.is_empty() => ty.clone(),
                (_, Some(d)) if !d.ty.is_empty() => d.ty.clone(),
                _ => return Err(at(format!("{}: no type: give one, or one in data.tsv", m.at))),
            };
            let size = values::size(&ty, &k.types).map_err(at)?;
            if let Some(known) = datum.and_then(|d| d.size)
                && known != size
            {
                return Err(at(format!("{ty} is {size} bytes, and data.tsv says {} is {known}", m.at)));
            }
            if k.ticks.inputs.iter().any(|i| i.at == id.to_string()) {
                return Err(at(format!("{} is an input: the port gets it with each tick's inputs", m.at)));
            }
            mapped.push(Mapped { field: m.field.clone(), id, ty, size });
        }
        Ok(mapped)
    }

    /// The memory declared to differ, as (ID, size).
    pub fn ignored(&self, k: &Knowledge) -> Result<Vec<(Id, u32)>, String> {
        self.ignores
            .iter()
            .map(|i| {
                let id = item(k, &i.at).map_err(|e| format!("{MAPPING}: ignore {}: {e}", i.at))?;
                let size = match i.size {
                    0 => k
                        .data
                        .get(&id)
                        .and_then(|d| d.size)
                        .ok_or_else(|| format!("{MAPPING}: ignore {}: give a size", i.at))?,
                    n => n,
                };
                Ok((id, size))
            })
            .collect()
    }
}

/// The ID a mapping (or an edit) names: an ID, or a data name.
pub(crate) fn item(k: &Knowledge, at: &str) -> Result<Id, String> {
    if let Ok(id) = parse_id(at) {
        return Ok(id);
    }
    k.named(at).filter(|id| k.data.contains_key(*id)).cloned().ok_or_else(|| format!("no data named {at}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::{Datum, Decision};

    fn knowledge() -> Knowledge {
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
        k.data.insert("GAME.EXE+00100".parse().unwrap(), datum("score", 2, "u16"));
        k.data.insert("GAME.EXE+00102".parse().unwrap(), datum("player_x", 2, ""));
        k
    }

    fn map(field: &str, at: &str, ty: &str) -> Map {
        Map { field: field.into(), at: at.into(), ty: ty.into(), note: String::new() }
    }

    #[test]
    fn a_mapping_resolves_against_the_knowledge() {
        let k = knowledge();
        let m = Mapping {
            maps: vec![
                map("score", "score", ""),
                map("player.x", "player_x", "i16"),
                map("seed", "GAME.EXE+00104", "u32"),
            ],
            ignores: vec![],
        };
        let mapped = m.resolve(&k).unwrap();
        assert_eq!((mapped[0].ty.as_str(), mapped[1].size, mapped[2].size), ("u16", 2, 4));
        let wrong = |maps| Mapping { maps, ignores: vec![] }.resolve(&k).unwrap_err();
        assert!(wrong(vec![map("x", "player_x", "")]).contains("no type"));
        assert!(wrong(vec![map("s", "score", "u32")]).contains("data.tsv says score is 2"));
        assert!(wrong(vec![map("s", "nothing", "u8")]).contains("no data named nothing"));
        assert!(wrong(vec![map("a", "score", ""), map("a", "player_x", "i16")]).contains("mapped twice"));
    }
}
