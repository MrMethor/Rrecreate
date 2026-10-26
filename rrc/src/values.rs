//! The original's memory as the knowledge's types read it (D62): primitives (`u8`, `i8`, `u16`, `i16`,
//! `u32`, `i32`), arrays (`T[N]`), and the structures and enumerations of `types.toml`, as JSON numbers,
//! arrays and objects: what a port's state serializes to, so the two compare as values. And back: a
//! scenario's edits (D64) are values written as bytes.

use serde_json::{Map, Value, json};

use crate::knowledge::Types;

/// The primitive types: name, size, signed.
const PRIMITIVES: [(&str, u32, bool); 6] =
    [("u8", 1, false), ("i8", 1, true), ("u16", 2, false), ("i16", 2, true), ("u32", 4, false), ("i32", 4, true)];

/// A type's size in bytes.
pub fn size(ty: &str, types: &Types) -> Result<u32, String> {
    if let Some((element, count)) = array(ty)? {
        return Ok(size(element, types)? * count);
    }
    if let Some(&(_, n, _)) = PRIMITIVES.iter().find(|p| p.0 == ty) {
        return Ok(n);
    }
    if let Some(s) = types.structs.iter().find(|s| s.name == ty) {
        return Ok(s.size);
    }
    if let Some(e) = types.enums.iter().find(|e| e.name == ty) {
        return Ok(e.size);
    }
    Err(format!("unknown type {ty:?} (u8, i8, u16, i16, u32, i32, T[N], or a struct or enum of types.toml)"))
}

/// `T[N]` as (T, N); `T[N][M]` is M arrays of `T[N]`.
fn array(ty: &str) -> Result<Option<(&str, u32)>, String> {
    let Some(open) = ty.rfind('[') else { return Ok(None) };
    let count = ty[open + 1..]
        .strip_suffix(']')
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("bad array type {ty:?}"))?;
    Ok(Some((&ty[..open], count)))
}

/// `bytes` (as many as the type's size) as the type reads them.
pub fn decode(bytes: &[u8], ty: &str, types: &Types) -> Result<Value, String> {
    let expected = size(ty, types)? as usize;
    if bytes.len() != expected {
        return Err(format!("{ty} is {expected} bytes, not {}", bytes.len()));
    }
    if let Some((element, count)) = array(ty)? {
        let width = size(element, types)? as usize;
        let elements = (0..count as usize).map(|i| decode(&bytes[i * width..(i + 1) * width], element, types));
        return elements.collect::<Result<Vec<_>, _>>().map(Value::Array);
    }
    if let Some(&(_, n, signed)) = PRIMITIVES.iter().find(|p| p.0 == ty) {
        return Ok(json!(number(bytes, n, signed)));
    }
    if let Some(s) = types.structs.iter().find(|s| s.name == ty) {
        let mut object = Map::new();
        for f in &s.fields {
            let range = f.offset as usize..(f.offset + f.size) as usize;
            let field = bytes.get(range).ok_or_else(|| format!("{ty}.{}: past the structure's end", f.name))?;
            let field_ty = if f.ty.is_empty() { plain(f.size) } else { f.ty.clone() };
            object.insert(f.name.clone(), decode(field, &field_ty, types)?);
        }
        return Ok(Value::Object(object));
    }
    let e = types.enums.iter().find(|e| e.name == ty).ok_or_else(|| format!("unknown type {ty:?}"))?;
    Ok(json!(number(bytes, e.size, false)))
}

/// The type of a field `types.toml` gives no type: an unsigned number of its size, or bytes.
fn plain(size: u32) -> String {
    match size {
        1 => "u8".into(),
        2 => "u16".into(),
        4 => "u32".into(),
        n => format!("u8[{n}]"),
    }
}

/// A little-endian number of `width` bytes.
fn number(bytes: &[u8], width: u32, signed: bool) -> i64 {
    let raw = bytes.iter().rev().fold(0u64, |v, &b| (v << 8) | u64::from(b));
    let shift = 64 - 8 * width;
    if signed { ((raw << shift) as i64) >> shift } else { raw as i64 }
}

/// The bytes of `value` as type `ty` (what [`decode`] reads back), as pieces (offset, bytes): a
/// number in the type's range, an array of the type's length, an object with some or all of a
/// structure's fields (only those are written), an enumeration's number or the name of one of its
/// values.
pub fn encode(value: &Value, ty: &str, types: &Types) -> Result<Vec<(u32, Vec<u8>)>, String> {
    let mut pieces = Vec::new();
    encode_at(value, ty, types, 0, &mut pieces)?;
    // In order, adjacent pieces as one.
    pieces.sort_by_key(|(offset, _)| *offset);
    let mut merged: Vec<(u32, Vec<u8>)> = Vec::new();
    for (offset, bytes) in pieces {
        match merged.last_mut() {
            Some((at, last)) if *at + last.len() as u32 == offset => last.extend(bytes),
            _ => merged.push((offset, bytes)),
        }
    }
    Ok(merged)
}

fn encode_at(value: &Value, ty: &str, types: &Types, at: u32, out: &mut Vec<(u32, Vec<u8>)>) -> Result<(), String> {
    if let Some((element, count)) = array(ty)? {
        let items = value.as_array().ok_or_else(|| format!("{ty}: an array, not {value}"))?;
        if items.len() != count as usize {
            return Err(format!("{ty}: {count} elements, not {}", items.len()));
        }
        let width = size(element, types)?;
        for (i, item) in items.iter().enumerate() {
            encode_at(item, element, types, at + i as u32 * width, out)?;
        }
        return Ok(());
    }
    if let Some(&(_, n, signed)) = PRIMITIVES.iter().find(|p| p.0 == ty) {
        out.push((at, integer(value, ty, n, signed)?));
        return Ok(());
    }
    if let Some(s) = types.structs.iter().find(|s| s.name == ty) {
        let object = value.as_object().ok_or_else(|| format!("{ty}: an object of its fields, not {value}"))?;
        for (name, v) in object {
            let f = s.fields.iter().find(|f| f.name == *name).ok_or_else(|| format!("{ty} has no field {name}"))?;
            let field_ty = if f.ty.is_empty() { plain(f.size) } else { f.ty.clone() };
            encode_at(v, &field_ty, types, at + f.offset, out).map_err(|e| format!("{ty}.{name}: {e}"))?;
        }
        return Ok(());
    }
    let e = types.enums.iter().find(|e| e.name == ty).ok_or_else(|| format!("unknown type {ty:?}"))?;
    let number = match value.as_str() {
        Some(name) => json!(e.values.get(name).ok_or_else(|| format!("{ty} has no value {name}"))?),
        None => value.clone(),
    };
    out.push((at, integer(&number, ty, e.size, false)?));
    Ok(())
}

/// A whole number as `width` little-endian bytes, if the type holds it.
fn integer(value: &Value, ty: &str, width: u32, signed: bool) -> Result<Vec<u8>, String> {
    let n = value.as_i64().ok_or_else(|| format!("{ty}: a whole number, not {value}"))?;
    let bits = 8 * width;
    let (min, max) = if signed { (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1) } else { (0, (1i64 << bits) - 1) };
    if !(min..=max).contains(&n) {
        return Err(format!("{ty}: {n} isn't in {min} to {max}"));
    }
    Ok(n.to_le_bytes()[..width as usize].to_vec())
}

/// Where a path into a value of type `ty` (`[3].x`, `.y`, empty) is: its offset and type.
pub fn locate(ty: &str, path: &str, types: &Types) -> Option<(u32, String)> {
    let (mut offset, mut ty, mut rest) = (0, ty.to_owned(), path);
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('[') {
            let (index, tail) = after.split_once(']')?;
            let (element, count) = array(&ty).ok()??;
            let index: u32 = index.parse().ok().filter(|&i| i < count)?;
            offset += index * size(element, types).ok()?;
            ty = element.to_owned();
            rest = tail;
        } else {
            let after = rest.strip_prefix('.')?;
            let end = after.find(['[', '.']).unwrap_or(after.len());
            let s = types.structs.iter().find(|s| s.name == ty)?;
            let f = s.fields.iter().find(|f| f.name == after[..end])?;
            offset += f.offset;
            ty = if f.ty.is_empty() { plain(f.size) } else { f.ty.clone() };
            rest = &after[end..];
        }
    }
    Some((offset, ty))
}

/// Where byte `offset` of a value of type `ty` is: the path to the innermost element or field that
/// holds it (`[1].hits`; empty for a primitive), that one's type, and the offset in it.
pub fn place(ty: &str, offset: u32, types: &Types) -> Option<(String, String, u32)> {
    let (mut path, mut ty, mut offset) = (String::new(), ty.to_owned(), offset);
    loop {
        if let Ok(Some((element, count))) = array(&ty) {
            let width = size(element, types).ok().filter(|&w| w > 0)?;
            let index = offset / width;
            if index >= count {
                return None;
            }
            path.push_str(&format!("[{index}]"));
            offset -= index * width;
            ty = element.to_owned();
        } else if let Some(s) = types.structs.iter().find(|s| s.name == ty) {
            // Padding: the structure's.
            let Some(f) = s.fields.iter().find(|f| f.offset <= offset && offset < f.offset + f.size) else {
                return Some((path, ty, offset));
            };
            path.push('.');
            path.push_str(&f.name);
            offset -= f.offset;
            ty = if f.ty.is_empty() { plain(f.size) } else { f.ty.clone() };
        } else {
            return Some((path, ty, offset));
        }
    }
}

/// Whether `ty` is a primitive type; its size.
pub fn primitive(ty: &str) -> Option<u32> {
    PRIMITIVES.iter().find(|p| p.0 == ty).map(|p| p.1)
}

/// The places where `original` and `port` differ, at most `max`: (path, the original's, the port's).
/// Arrays and objects are compared element by element; a field the port has and the original's type
/// doesn't is left alone.
pub fn differences(path: &str, original: &Value, port: &Value, max: usize, out: &mut Vec<(String, Value, Value)>) {
    if out.len() >= max {
        return;
    }
    match (original, port) {
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                differences(&format!("{path}[{i}]"), x, y, max, out);
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            for (key, x) in a {
                let y = b.get(key).unwrap_or(&Value::Null);
                differences(&format!("{path}.{key}"), x, y, max, out);
            }
        }
        _ if original != port => out.push((path.to_owned(), original.clone(), port.clone())),
        _ => {}
    }
}

/// The value at a path into a decoded value (`[3].x`, `.y`, empty: itself).
pub fn at_leaf<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let (mut value, mut rest) = (value, path);
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('[') {
            let (index, tail) = after.split_once(']')?;
            value = value.get(index.parse::<usize>().ok()?)?;
            rest = tail;
        } else {
            let after = rest.strip_prefix('.')?;
            let end = after.find(['[', '.']).unwrap_or(after.len());
            value = value.get(&after[..end])?;
            rest = &after[end..];
        }
    }
    Some(value)
}

/// The value at a dotted path (`player.x`) of a serialized state.
pub fn at_path<'a>(state: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(state, |v, part| v.get(part))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::{Field, Struct};

    fn types() -> Types {
        let field = |offset, size, name: &str, ty: &str| Field { offset, size, name: name.into(), ty: ty.into() };
        Types {
            structs: vec![Struct {
                name: "enemy".into(),
                size: 6,
                fields: vec![
                    field(0, 2, "x", "i16"),
                    field(2, 2, "dx", "i16"),
                    field(4, 1, "alive", "u8"),
                    field(5, 1, "hits", ""),
                ],
                provenance: String::new(),
            }],
            enums: vec![],
        }
    }

    #[test]
    fn bytes_read_as_their_type() {
        let t = types();
        assert_eq!(decode(&[0xFE, 0xFF], "i16", &t).unwrap(), json!(-2));
        assert_eq!(decode(&[0xFE, 0xFF], "u16", &t).unwrap(), json!(65534));
        assert_eq!(decode(&[1, 0, 0, 0x80], "i32", &t).unwrap(), json!(-2147483647));
        assert_eq!(decode(&[1, 2, 3], "u8[3]", &t).unwrap(), json!([1, 2, 3]));
        let enemies = decode(&[0x10, 0, 0xFF, 0xFF, 1, 7, 0, 0, 0, 0, 0, 0], "enemy[2]", &t).unwrap();
        assert_eq!(enemies[0], json!({"x": 16, "dx": -1, "alive": 1, "hits": 7}));
        assert_eq!((size("enemy[2]", &t).unwrap(), size("u16[3][2]", &t).unwrap()), (12, 12));
        assert!(decode(&[1], "u16", &t).is_err());
        assert!(size("float", &t).is_err());
        let place = |ty, offset| place(ty, offset, &t);
        assert_eq!(place("enemy[2]", 7), Some(("[1].x".into(), "i16".into(), 1)));
        assert_eq!(place("enemy[2]", 11), Some(("[1].hits".into(), "u8".into(), 0)));
        assert_eq!(place("u16", 1), Some((String::new(), "u16".into(), 1)));
        assert_eq!(place("u8[4]", 4), None);
    }

    #[test]
    fn values_write_as_bytes() {
        let t = types();
        let pieces = |v: Value, ty| encode(&v, ty, &t);
        assert_eq!(pieces(json!(-2), "i16").unwrap(), [(0, vec![0xFE, 0xFF])]);
        assert!(pieces(json!(70000), "u16").unwrap_err().contains("isn't in 0 to 65535"));
        assert!(pieces(json!(-1), "u8").is_err() && pieces(json!(1.5), "u8").is_err());
        // A structure whole is its bytes; some of its fields, those alone.
        let bytes = [0x10, 0, 0xFF, 0xFF, 1, 7];
        let whole = decode(&bytes, "enemy", &t).unwrap();
        assert_eq!(pieces(whole, "enemy").unwrap(), [(0, bytes.to_vec())]);
        assert_eq!(pieces(json!({"alive": 0, "x": 5}), "enemy").unwrap(), [(0, vec![5, 0]), (4, vec![0])]);
        assert!(pieces(json!({"z": 0}), "enemy").unwrap_err().contains("no field z"));
        assert_eq!(pieces(json!([1, 2]), "u8[2]").unwrap(), [(0, vec![1, 2])]);
        assert!(pieces(json!([1]), "u8[2]").is_err());
        assert_eq!(locate("enemy[2]", "[1].alive", &t), Some((10, "u8".into())));
        assert_eq!(locate("enemy[2]", "[2]", &t), None);
        assert_eq!(locate("u16", "", &t), Some((0, "u16".into())));
    }

    #[test]
    fn differences_are_the_leaves() {
        let original = json!([{"x": 1, "y": 2}, {"x": 3, "y": 4}]);
        let port = json!([{"x": 1, "y": 2, "extra": 9}, {"x": 3, "y": 5}]);
        let mut out = Vec::new();
        differences("enemies", &original, &port, 10, &mut out);
        assert_eq!(out, [("enemies[1].y".to_owned(), json!(4), json!(5))]);
        assert_eq!(at_leaf(&original, "[1].y"), Some(&json!(4)));
        assert_eq!((at_leaf(&original, ""), at_leaf(&original, "[2]")), (Some(&original), None));
        let state = json!({"player": {"x": 7}});
        assert_eq!(at_path(&state, "player.x"), Some(&json!(7)));
        assert_eq!(at_path(&state, "player.z"), None);
    }
}
