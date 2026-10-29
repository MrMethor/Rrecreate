//! The state mapping (DESIGN.md §7, D16, D62, D72): `mapping.toml` in the port repository. Each
//! `[[map]]` is a field of the port's state (a path in its serialization, `player.x`) and the
//! original's item it is: a data name or an ID, read as the knowledge's type for it or the one given;
//! or, `via` a pointer, what the pointer points to at each tick (data on the heap); with a `mask`, only
//! those bits of an unsigned number (D95: the others are a part's the port doesn't have, a renderer's
//! scratch in a flags word, neither compared nor transplanted). `[[ignore]]` is the
//! original's memory that may differ (padding, caches, driver internals), named so that nothing is left
//! out silently. `rrc record` traces every mapped item; `rrc compare` compares them, and a transplant
//! builds the port's state from them.

use ids::Id;

use crate::knowledge::{Knowledge, parse_id};
use crate::repo::{Repo, read_toml};
use crate::values::{self, PointerKind};

pub const MAPPING: &str = "mapping.toml";

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Mapping {
    #[serde(default, rename = "map", skip_serializing_if = "Vec::is_empty")]
    pub maps: Vec<Map>,
    #[serde(default, rename = "ignore", skip_serializing_if = "Vec::is_empty")]
    pub ignores: Vec<Ignore>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Map {
    /// The port's field: a path in its state's serialization.
    pub field: String,
    /// The original's item: a data name or an ID; or
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at: String,
    /// a pointer (a data name or an ID, with a path into it as its type reads: `world.level`): the
    /// item is where it points at each tick,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub via: String,
    /// moved by this many bytes.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: u32,
    /// How to read it; empty: the knowledge's type for the data (an item `via` a pointer needs one).
    #[serde(default, rename = "type", skip_serializing_if = "String::is_empty")]
    pub ty: String,
    /// The bits of the item (an unsigned number) that are the field; none: all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
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

/// A mapped field resolved against the knowledge: its item's ID, type and size; for an item `via` a
/// pointer, `id` is the pointer's.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapped {
    pub field: String,
    pub id: Id,
    pub ty: String,
    pub size: u32,
    pub via: Option<Via>,
    /// The bits that are the field ([`Map::mask`]).
    pub mask: Option<u64>,
}

/// How a mapped item is found through a pointer: the pointer as the mapping names it, how it points,
/// and how far past where it points the item is.
#[derive(Debug, Clone, PartialEq)]
pub struct Via {
    pub name: String,
    pub pointer: PointerKind,
    pub offset: u32,
}

impl Via {
    /// What the trace calls the item: `*trail`, `*world.level+4`.
    pub fn item_name(&self) -> String {
        match self.offset {
            0 => format!("*{}", self.name),
            n => format!("*{}+{n}", self.name),
        }
    }
}

impl Mapped {
    /// A value of the field (the original's or the port's): the bits of its mask.
    pub fn masked(&self, value: &serde_json::Value) -> serde_json::Value {
        match (self.mask, value.as_u64()) {
            (Some(mask), Some(n)) => serde_json::Value::from(n & mask),
            _ => value.clone(),
        }
    }

    /// The original's memory the field covers: the item, or the pointer it is found through.
    pub fn covers(&self) -> (&Id, u32) {
        match &self.via {
            Some(via) => (&self.id, via.pointer.size()),
            None => (&self.id, self.size),
        }
    }
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
            let resolved = match (m.at.is_empty(), m.via.is_empty()) {
                (false, true) => direct(k, m),
                (true, false) => via(k, m),
                (false, false) => Err("at or via, not both".into()),
                (true, true) => Err("no at: the original's item (or via: a pointer to it)".into()),
            };
            let mut resolved = resolved.map_err(at)?;
            if let Some(mask) = m.mask {
                if !matches!(resolved.ty.as_str(), "u8" | "u16" | "u32") {
                    return Err(at(format!("a mask takes some bits of an unsigned number, not of {}", resolved.ty)));
                }
                resolved.mask = Some(mask);
            }
            mapped.push(resolved);
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

/// A field mapped to an item `at` an ID.
fn direct(k: &Knowledge, m: &Map) -> Result<Mapped, String> {
    if m.offset != 0 {
        return Err("offset goes with via; give the item's own ID".into());
    }
    let id = item(k, &m.at)?;
    let datum = k.data.get(&id);
    let ty = match (&m.ty, datum) {
        (ty, _) if !ty.is_empty() => ty.clone(),
        (_, Some(d)) if !d.ty.is_empty() => d.ty.clone(),
        _ => return Err(format!("{}: no type: give one, or one in data.tsv", m.at)),
    };
    let size = values::size(&ty, &k.types)?;
    if let Some(known) = datum.and_then(|d| d.size)
        && known != size
    {
        return Err(format!("{ty} is {size} bytes, and data.tsv says {} is {known}", m.at));
    }
    if k.ticks.inputs.iter().any(|i| i.at == id.to_string()) {
        return Err(format!("{} is an input: the port gets it with each tick's inputs", m.at));
    }
    Ok(Mapped { field: m.field.clone(), id, ty, size, via: None, mask: None })
}

/// A field mapped to what a pointer points to (D72).
fn via(k: &Knowledge, m: &Map) -> Result<Mapped, String> {
    let (name, path) = crate::edits::split(&m.via);
    let id = item(k, name)?;
    let holder = k.data.get(&id).map(|d| d.ty.clone()).unwrap_or_default();
    if holder.is_empty() {
        return Err(format!("via {name}: no type in data.tsv: give it one (a pointer type, or one holding it)"));
    }
    let (offset, pointer_ty) =
        values::locate(&holder, path, &k.types).ok_or_else(|| format!("via: {holder} has no {path}"))?;
    let pointer = PointerKind::of_type(&pointer_ty, &k.types).ok_or_else(|| {
        format!("via {}: {pointer_ty} isn't a pointer type (far, or a [[pointer]] of types.toml)", m.via)
    })??;
    if m.ty.is_empty() {
        return Err("an item via a pointer needs a type: what it points to".into());
    }
    let size = values::size(&m.ty, &k.types)?;
    let id = values::moved(&id, offset).ok_or("past the address space")?;
    let via = Via { name: m.via.clone(), pointer, offset: m.offset };
    Ok(Mapped { field: m.field.clone(), id, ty: m.ty.clone(), size, via: Some(via), mask: None })
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
    use crate::knowledge::{Datum, Decision, Field, Pointer, Struct};

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
        k.data.insert("GAME.EXE+00110".parse().unwrap(), datum("trail", 4, "far"));
        k.data.insert("GAME.EXE+00120".parse().unwrap(), datum("world", 6, "world"));
        k.types.structs.push(Struct {
            name: "world".into(),
            size: 6,
            fields: vec![
                Field { offset: 0, size: 2, name: "turn".into(), ty: "u16".into() },
                Field { offset: 2, size: 2, name: "level".into(), ty: "level_ptr".into() },
            ],
            provenance: String::new(),
        });
        k.types.pointers.push(Pointer {
            name: "level_ptr".into(),
            kind: "near".into(),
            size: 2,
            base: "GAME.EXE+00000".into(),
            into: String::new(),
            provenance: String::new(),
        });
        k
    }

    fn map(field: &str, at: &str, ty: &str) -> Map {
        Map { field: field.into(), at: at.into(), ty: ty.into(), ..Map::default() }
    }

    fn via(field: &str, via: &str, offset: u32, ty: &str) -> Map {
        Map { field: field.into(), via: via.into(), offset, ty: ty.into(), ..Map::default() }
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
        let flags = |ty: &str| Map { mask: Some(0xFFFD), ..map("flags", "score", ty) };
        assert!(wrong(vec![flags("i16")]).contains("some bits of an unsigned number"));
        let mapped = Mapping { maps: vec![flags("")], ignores: vec![] }.resolve(&k).unwrap();
        assert_eq!(mapped[0].masked(&serde_json::json!(3)), serde_json::json!(1), "bit 1 isn't the field's");
        assert_eq!(mapped[0].masked(&serde_json::json!([3])), serde_json::json!([3]), "a number's bits only");
    }

    #[test]
    fn an_item_is_found_through_a_pointer() {
        let k = knowledge();
        let m = Mapping {
            maps: vec![via("trail", "trail", 0, "i16[32]"), via("level.cells", "world.level", 4, "u8[64]")],
            ignores: vec![],
        };
        let mapped = m.resolve(&k).unwrap();
        assert_eq!((mapped[0].id.to_string(), mapped[0].size), ("GAME.EXE+00110".into(), 64));
        assert_eq!(mapped[0].via, Some(Via { name: "trail".into(), pointer: PointerKind::Far, offset: 0 }));
        assert_eq!(mapped[1].via.as_ref().unwrap().item_name(), "*world.level+4");
        assert_eq!(mapped[0].covers(), (&mapped[0].id, 4), "the pointer");
        assert_eq!(mapped[1].id.to_string(), "GAME.EXE+00122", "the field holding the pointer");
        let near = PointerKind::Near { base: "GAME.EXE+00000".parse().unwrap(), size: 2 };
        assert_eq!(mapped[1].via, Some(Via { name: "world.level".into(), pointer: near, offset: 4 }));
        let wrong = |maps| Mapping { maps, ignores: vec![] }.resolve(&k).unwrap_err();
        assert!(wrong(vec![via("t", "trail", 0, "")]).contains("needs a type"));
        assert!(wrong(vec![via("t", "score", 0, "u8")]).contains("u16 isn't a pointer type"));
        assert!(wrong(vec![via("t", "world.turn", 0, "u8")]).contains("isn't a pointer type"));
        assert!(wrong(vec![Map { at: "score".into(), ..via("t", "trail", 0, "u8") }]).contains("not both"));
        assert!(wrong(vec![Map { offset: 2, ..map("s", "score", "") }]).contains("offset goes with via"));
    }
}
