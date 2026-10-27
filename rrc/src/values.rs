//! The original's memory as the knowledge's types read it (D62, D72): numbers (`u8`, `i8`, `u16`,
//! `i16`, `u32`, `i32`), `bool`, arrays (`T[N]`), pointers (`far`, and the `[[pointer]]` types of
//! `types.toml`), packed bits, and the structures and enumerations of `types.toml`, as JSON numbers,
//! booleans, strings, arrays and objects: what a port's state serializes to, so the two compare as
//! values. And back: a scenario's edits (D64) are values written as bytes.

use ids::Id;
use serde_json::{Map, Value, json};

use crate::knowledge::{Bits, Enum, Knowledge, Pointer, Struct, Types, parse_id};

/// The number types: name, size, signed.
const PRIMITIVES: [(&str, u32, bool); 6] =
    [("u8", 1, false), ("i8", 1, true), ("u16", 2, false), ("i16", 2, true), ("u32", 4, false), ("i32", 4, true)];

/// The types `types.toml` needn't declare: the numbers, `bool` (a byte: 0 false, anything else true)
/// and `far` (a far pointer, read as the ID it points to).
pub const BUILT_IN: [&str; 8] = ["u8", "i8", "u16", "i16", "u32", "i32", "bool", "far"];

/// What a type is.
enum Kind<'a> {
    Array(&'a str, u32),
    Number(u32, bool),
    Bool,
    /// How it points, and its declaration (none for `far`).
    Pointer(PointerKind, Option<&'a Pointer>),
    Struct(&'a Struct),
    Bits(&'a Bits),
    Enum(&'a Enum),
}

fn kind<'a>(ty: &'a str, types: &'a Types) -> Result<Kind<'a>, String> {
    if let Some((element, count)) = array(ty)? {
        return Ok(Kind::Array(element, count));
    }
    if let Some(&(_, n, signed)) = PRIMITIVES.iter().find(|p| p.0 == ty) {
        return Ok(Kind::Number(n, signed));
    }
    match ty {
        "bool" => return Ok(Kind::Bool),
        "far" => return Ok(Kind::Pointer(PointerKind::Far, None)),
        _ => {}
    }
    if let Some(s) = types.structs.iter().find(|s| s.name == ty) {
        return Ok(Kind::Struct(s));
    }
    if let Some(e) = types.enums.iter().find(|e| e.name == ty) {
        return Ok(Kind::Enum(e));
    }
    if let Some(p) = types.pointers.iter().find(|p| p.name == ty) {
        return Ok(Kind::Pointer(PointerKind::of(p)?, Some(p)));
    }
    if let Some(b) = types.bits.iter().find(|b| b.name == ty) {
        return Ok(Kind::Bits(b));
    }
    Err(format!(
        "unknown type {ty:?} (u8, i8, u16, i16, u32, i32, bool, far, T[N], or a type of types.toml: struct, enum, \
         pointer, bits)"
    ))
}

/// How a pointer's bytes give an address (D72).
#[derive(Debug, Clone, PartialEq)]
pub enum PointerKind {
    /// An offset of `size` bytes from `base`.
    Near { base: Id, size: u32 },
    /// An offset, then a segment: segment × 16 + offset (real mode).
    Far,
}

/// Where a pointer points: an ID (a near pointer's), or a linear address (a far pointer's).
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Id(Id),
    Linear(u32),
}

impl PointerKind {
    pub fn of(p: &Pointer) -> Result<PointerKind, String> {
        match p.kind.as_str() {
            "near" => {
                let base = parse_id(&p.base).map_err(|e| format!("pointer {}: base: {e}", p.name))?;
                Ok(PointerKind::Near { base, size: p.size })
            }
            "far" => Ok(PointerKind::Far),
            kind => Err(format!("pointer {}: kind {kind:?}: near or far", p.name)),
        }
    }

    /// The pointer type `ty` is (`far`, or a `[[pointer]]` of `types.toml`), if it is one.
    pub fn of_type(ty: &str, types: &Types) -> Option<Result<PointerKind, String>> {
        match kind(ty, types) {
            Ok(Kind::Pointer(p, _)) => Some(Ok(p)),
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        }
    }

    pub fn size(&self) -> u32 {
        match self {
            PointerKind::Near { size, .. } => *size,
            PointerKind::Far => 4,
        }
    }

    /// Where `bytes` point, moved by `past` bytes; none for a null pointer (all zero).
    pub fn target(&self, bytes: &[u8], past: u32) -> Option<Target> {
        if bytes.len() != self.size() as usize || bytes.iter().all(|&b| b == 0) {
            return None;
        }
        match self {
            PointerKind::Near { base, size } => moved(base, number(bytes, *size, false) as u32 + past).map(Target::Id),
            PointerKind::Far => {
                let word = |at: usize| u32::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]));
                Some(Target::Linear(word(2) * 16 + word(0) + past))
            }
        }
    }
}

/// `id` moved by `by` bytes.
pub fn moved(id: &Id, by: u32) -> Option<Id> {
    if id.is_memory() {
        return Some(Id::memory(id.offset().wrapping_add(by)));
    }
    Id::new(id.module(), id.part(), id.offset().checked_add(by)?).ok()
}

/// What reading values needs besides the types: for a pointer read as an index (`into`), the named
/// data; for a far pointer, the ID of a linear address (where the session put the program).
#[derive(Clone, Copy)]
pub struct Reader<'a> {
    pub types: &'a Types,
    pub k: Option<&'a Knowledge>,
    pub linear: Option<&'a dyn Fn(u32) -> Option<Id>>,
}

impl<'a> Reader<'a> {
    pub fn new(types: &'a Types) -> Reader<'a> {
        Reader { types, k: None, linear: None }
    }

    pub fn of(k: &'a Knowledge) -> Reader<'a> {
        Reader { types: &k.types, k: Some(k), linear: None }
    }

    pub fn with_linear(self, linear: &'a dyn Fn(u32) -> Option<Id>) -> Reader<'a> {
        Reader { linear: Some(linear), ..self }
    }

    /// The ID a pointer's target is, if it can be said.
    fn id(&self, target: &Target) -> Option<Id> {
        match target {
            Target::Id(id) => Some(id.clone()),
            Target::Linear(linear) => self.linear.and_then(|f| f(*linear)),
        }
    }

    /// The array named data `name` is: its ID, its element type and size, and its length.
    fn array(&self, name: &str) -> Option<(Id, String, u32, u32)> {
        let k = self.k?;
        let id = k.named(name)?;
        let (element, count) = array(&k.data.get(id)?.ty).ok()??;
        Some((id.clone(), element.to_owned(), size(element, self.types).ok()?, count))
    }
}

/// A type's size in bytes.
pub fn size(ty: &str, types: &Types) -> Result<u32, String> {
    Ok(match kind(ty, types)? {
        Kind::Array(element, count) => size(element, types)? * count,
        Kind::Number(n, _) => n,
        Kind::Bool => 1,
        Kind::Pointer(p, _) => p.size(),
        Kind::Struct(s) => s.size,
        Kind::Bits(b) => b.size,
        Kind::Enum(e) => e.size,
    })
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
pub fn decode(bytes: &[u8], ty: &str, r: &Reader) -> Result<Value, String> {
    let types = r.types;
    let expected = size(ty, types)? as usize;
    if bytes.len() != expected {
        return Err(format!("{ty} is {expected} bytes, not {}", bytes.len()));
    }
    Ok(match kind(ty, types)? {
        Kind::Array(element, count) => {
            let width = size(element, types)? as usize;
            let elements = (0..count as usize).map(|i| decode(&bytes[i * width..(i + 1) * width], element, r));
            Value::Array(elements.collect::<Result<Vec<_>, _>>()?)
        }
        Kind::Number(n, signed) => json!(number(bytes, n, signed)),
        Kind::Bool => json!(bytes[0] != 0),
        Kind::Pointer(p, declared) => pointer_value(bytes, &p, declared, r),
        Kind::Struct(s) => {
            let mut object = Map::new();
            for f in &s.fields {
                let range = f.offset as usize..(f.offset + f.size) as usize;
                let field = bytes.get(range).ok_or_else(|| format!("{ty}.{}: past the structure's end", f.name))?;
                let field_ty = if f.ty.is_empty() { plain(f.size) } else { f.ty.clone() };
                object.insert(f.name.clone(), decode(field, &field_ty, r)?);
            }
            Value::Object(object)
        }
        Kind::Bits(b) => {
            let raw = number(bytes, b.size, false) as u64;
            let mut object = Map::new();
            for f in &b.fields {
                let v = (raw >> f.bit) & ((1u64 << f.width) - 1);
                object.insert(f.name.clone(), if f.ty == "bool" { json!(v != 0) } else { json!(v) });
            }
            Value::Object(object)
        }
        Kind::Enum(e) => json!(number(bytes, e.size, false)),
    })
}

/// A pointer's value: null; the index of the element it points to, for one read `into` an array; or
/// the ID it points to (a far one whose ID isn't known: its segment:offset).
fn pointer_value(bytes: &[u8], p: &PointerKind, declared: Option<&Pointer>, r: &Reader) -> Value {
    let Some(target) = p.target(bytes, 0) else { return Value::Null };
    let Some(id) = r.id(&target) else {
        let word = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        return json!(format!("{:04X}:{:04X}", word(2), word(0)));
    };
    let into = declared.map(|d| d.into.as_str()).filter(|into| !into.is_empty());
    if let Some((array, _, width, count)) = into.and_then(|into| r.array(into))
        && (id.module(), id.part()) == (array.module(), array.part())
        && let Some(by) = id.offset().checked_sub(array.offset())
        && width > 0
        && by % width == 0
        && by / width < count
    {
        return json!(by / width);
    }
    json!(id.to_string())
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
/// number in the type's range, a boolean, an array of the type's length, an object with some or all
/// of a structure's fields (only those are written) or all of packed bits' fields, an enumeration's
/// number or the name of one of its values, a pointer as null, an index (`into`) or an ID (a far one:
/// SEGMENT:OFFSET in hex).
pub fn encode(value: &Value, ty: &str, r: &Reader) -> Result<Vec<(u32, Vec<u8>)>, String> {
    let mut pieces = Vec::new();
    encode_at(value, ty, r, 0, &mut pieces)?;
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

fn encode_at(value: &Value, ty: &str, r: &Reader, at: u32, out: &mut Vec<(u32, Vec<u8>)>) -> Result<(), String> {
    let types = r.types;
    match kind(ty, types)? {
        Kind::Array(element, count) => {
            let items = value.as_array().ok_or_else(|| format!("{ty}: an array, not {value}"))?;
            if items.len() != count as usize {
                return Err(format!("{ty}: {count} elements, not {}", items.len()));
            }
            let width = size(element, types)?;
            for (i, item) in items.iter().enumerate() {
                encode_at(item, element, r, at + i as u32 * width, out)?;
            }
        }
        Kind::Number(n, signed) => out.push((at, integer(value, ty, n, signed)?)),
        Kind::Bool => {
            let b = value.as_bool().ok_or_else(|| format!("{ty}: true or false, not {value}"))?;
            out.push((at, vec![u8::from(b)]));
        }
        Kind::Pointer(p, declared) => {
            out.push((at, pointer_bytes(value, &p, declared, r).map_err(|e| format!("{ty}: {e}"))?))
        }
        Kind::Struct(s) => {
            let object = value.as_object().ok_or_else(|| format!("{ty}: an object of its fields, not {value}"))?;
            for (name, v) in object {
                let f = s.fields.iter().find(|f| f.name == *name).ok_or_else(|| format!("{ty} has no field {name}"))?;
                let field_ty = if f.ty.is_empty() { plain(f.size) } else { f.ty.clone() };
                encode_at(v, &field_ty, r, at + f.offset, out).map_err(|e| format!("{ty}.{name}: {e}"))?;
            }
        }
        Kind::Bits(b) => out.push((at, bits_bytes(value, b)?)),
        Kind::Enum(e) => {
            let number = match value.as_str() {
                Some(name) => json!(e.values.get(name).ok_or_else(|| format!("{ty} has no value {name}"))?),
                None => value.clone(),
            };
            out.push((at, integer(&number, ty, e.size, false)?));
        }
    }
    Ok(())
}

/// A pointer's bytes: null, an index into its array (`into`), or the ID it points to; a far one's,
/// null or SEGMENT:OFFSET.
fn pointer_bytes(value: &Value, p: &PointerKind, declared: Option<&Pointer>, r: &Reader) -> Result<Vec<u8>, String> {
    if value.is_null() {
        return Ok(vec![0; p.size() as usize]);
    }
    let (base, size) = match p {
        PointerKind::Far => {
            let text = value.as_str().ok_or_else(|| format!("a far pointer is null or SEGMENT:OFFSET, not {value}"))?;
            let word = |t: &str| u16::from_str_radix(t, 16).ok();
            let (segment, offset) = text
                .split_once(':')
                .and_then(|(s, o)| Some((word(s)?, word(o)?)))
                .ok_or_else(|| format!("a far pointer is SEGMENT:OFFSET in hex, not {text:?}"))?;
            return Ok([offset.to_le_bytes(), segment.to_le_bytes()].concat());
        }
        PointerKind::Near { base, size } => (base, *size),
    };
    let id = match value {
        Value::String(text) => parse_id(text)?,
        Value::Number(_) => {
            let into = declared.map(|d| d.into.as_str()).filter(|into| !into.is_empty());
            let into = into.ok_or("an index points into the array the pointer type's into names, and it names none")?;
            let (array, _, width, count) = r.array(into).ok_or_else(|| format!("into: no array named {into}"))?;
            let i = value
                .as_u64()
                .filter(|&i| i < u64::from(count))
                .ok_or_else(|| format!("{value} isn't an index of {into} (0 to {})", count.saturating_sub(1)))?;
            moved(&array, i as u32 * width).ok_or("past the address space")?
        }
        _ => return Err(format!("null, an index or an ID, not {value}")),
    };
    let offset = id
        .offset()
        .checked_sub(base.offset())
        .filter(|_| (id.module(), id.part()) == (base.module(), base.part()))
        .ok_or_else(|| format!("{id} isn't after the pointer's base, {base}"))?;
    if size < 4 && offset >> (8 * size) != 0 {
        return Err(format!("{id} is past what a {size}-byte offset from {base} reaches"));
    }
    Ok(offset.to_le_bytes()[..size as usize].to_vec())
}

/// Packed bits' bytes from an object of all its fields, if its fields cover every bit (a bit no field
/// declares would be written as 0).
fn bits_bytes(value: &Value, b: &Bits) -> Result<Vec<u8>, String> {
    let object = value.as_object().ok_or_else(|| format!("{}: an object of its fields, not {value}", b.name))?;
    if let Some(name) = object.keys().find(|name| !b.fields.iter().any(|f| f.name == **name)) {
        return Err(format!("{} has no field {name}", b.name));
    }
    let (mut raw, mut covered) = (0u64, 0u64);
    for f in &b.fields {
        let wrong = |e: String| format!("{}.{}: {e}", b.name, f.name);
        let v = object.get(&f.name).ok_or_else(|| {
            wrong("packed bits are written whole: give all their fields, or the bytes by the ID".into())
        })?;
        let n = match (f.ty.as_str(), v) {
            ("bool", Value::Bool(b)) => u64::from(*b),
            ("bool", _) => return Err(wrong(format!("true or false, not {v}"))),
            (_, v) => v
                .as_u64()
                .filter(|&n| n >> f.width == 0)
                .ok_or_else(|| wrong(format!("{v} isn't in 0 to {}", (1u64 << f.width) - 1)))?,
        };
        let mask = ((1u64 << f.width) - 1) << f.bit;
        raw |= n << f.bit;
        covered |= mask;
    }
    let all = if b.size >= 8 { u64::MAX } else { (1u64 << (8 * b.size)) - 1 };
    if covered != all {
        return Err(format!(
            "{}: bits {:#X} have no field, and writing it would clear them: declare them",
            b.name,
            all & !covered
        ));
    }
    Ok(raw.to_le_bytes()[..b.size as usize].to_vec())
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

/// Where a path into a value of type `ty` (`[3].x`, `.y`, empty) is: its offset and type. Packed
/// bits are the smallest place: a path into them is where they are.
pub fn locate(ty: &str, path: &str, types: &Types) -> Option<(u32, String)> {
    let (mut offset, mut ty, mut rest) = (0, ty.to_owned(), path);
    while !rest.is_empty() {
        if types.bits.iter().any(|b| b.name == ty) {
            break;
        }
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

/// Whether `ty` is a number type; its size.
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
    use crate::knowledge::{BitField, Datum, Decision, Field};

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
            ..Types::default()
        }
    }

    #[test]
    fn bytes_read_as_their_type() {
        let t = types();
        let r = Reader::new(&t);
        assert_eq!(decode(&[0xFE, 0xFF], "i16", &r).unwrap(), json!(-2));
        assert_eq!(decode(&[0xFE, 0xFF], "u16", &r).unwrap(), json!(65534));
        assert_eq!(decode(&[1, 0, 0, 0x80], "i32", &r).unwrap(), json!(-2147483647));
        assert_eq!(decode(&[1, 2, 3], "u8[3]", &r).unwrap(), json!([1, 2, 3]));
        let enemies = decode(&[0x10, 0, 0xFF, 0xFF, 1, 7, 0, 0, 0, 0, 0, 0], "enemy[2]", &r).unwrap();
        assert_eq!(enemies[0], json!({"x": 16, "dx": -1, "alive": 1, "hits": 7}));
        assert_eq!((size("enemy[2]", &t).unwrap(), size("u16[3][2]", &t).unwrap()), (12, 12));
        assert_eq!(decode(&[0, 2], "bool[2]", &r).unwrap(), json!([false, true]));
        assert!(decode(&[1], "u16", &r).is_err());
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
        let r = Reader::new(&t);
        let pieces = |v: Value, ty| encode(&v, ty, &r);
        assert_eq!(pieces(json!(-2), "i16").unwrap(), [(0, vec![0xFE, 0xFF])]);
        assert!(pieces(json!(70000), "u16").unwrap_err().contains("isn't in 0 to 65535"));
        assert!(pieces(json!(-1), "u8").is_err() && pieces(json!(1.5), "u8").is_err());
        // A structure whole is its bytes; some of its fields, those alone.
        let bytes = [0x10, 0, 0xFF, 0xFF, 1, 7];
        let whole = decode(&bytes, "enemy", &r).unwrap();
        assert_eq!(pieces(whole, "enemy").unwrap(), [(0, bytes.to_vec())]);
        assert_eq!(pieces(json!({"alive": 0, "x": 5}), "enemy").unwrap(), [(0, vec![5, 0]), (4, vec![0])]);
        assert!(pieces(json!({"z": 0}), "enemy").unwrap_err().contains("no field z"));
        assert_eq!(pieces(json!([1, 2]), "u8[2]").unwrap(), [(0, vec![1, 2])]);
        assert!(pieces(json!([1]), "u8[2]").is_err());
        assert_eq!(pieces(json!(true), "bool").unwrap(), [(0, vec![1])]);
        assert_eq!(locate("enemy[2]", "[1].alive", &t), Some((10, "u8".into())));
        assert_eq!(locate("enemy[2]", "[2]", &t), None);
        assert_eq!(locate("u16", "", &t), Some((0, "u16".into())));
    }

    #[test]
    fn pointers_read_as_ids_or_as_indices() {
        let mut k = Knowledge { types: types(), ..Knowledge::default() };
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
        k.data.insert("GAME.EXE+20100".parse().unwrap(), datum("enemies", 12, "enemy[2]"));
        let pointer = |name: &str, kind: &str, size, into: &str| Pointer {
            name: name.into(),
            kind: kind.into(),
            size,
            base: "GAME.EXE+20000".into(),
            into: into.into(),
            provenance: String::new(),
        };
        k.types.pointers = vec![pointer("enemy_ptr", "near", 2, "enemies"), pointer("near_ptr", "near", 2, "")];
        let r = Reader::of(&k);
        // A near pointer is an offset from its base: into the array, an index; elsewhere, an ID.
        assert_eq!(decode(&[0x06, 0x01], "enemy_ptr", &r).unwrap(), json!(1));
        assert_eq!(decode(&[0x00, 0x00], "enemy_ptr", &r).unwrap(), Value::Null);
        assert_eq!(decode(&[0x07, 0x01], "enemy_ptr", &r).unwrap(), json!("GAME.EXE+20107"), "inside an element");
        assert_eq!(decode(&[0x0C, 0x01], "enemy_ptr", &r).unwrap(), json!("GAME.EXE+2010C"), "past the array");
        assert_eq!(decode(&[0x06, 0x01], "near_ptr", &r).unwrap(), json!("GAME.EXE+20106"));
        // A far pointer: segment × 16 + offset, the ID there if the session's places are known.
        assert_eq!(decode(&[0x34, 0x12, 0x00, 0xB8], "far", &r).unwrap(), json!("B800:1234"));
        let linear = |l: u32| Some(Id::memory(l));
        let placed = r.with_linear(&linear);
        assert_eq!(decode(&[0x34, 0x12, 0x00, 0xB8], "far", &placed).unwrap(), json!("MEM+B9234"));
        // And back.
        let bytes = |v: Value, ty: &str| encode(&v, ty, &r).map(|p| p[0].1.clone());
        assert_eq!(bytes(json!(1), "enemy_ptr").unwrap(), [0x06, 0x01]);
        assert_eq!(bytes(Value::Null, "enemy_ptr").unwrap(), [0, 0]);
        assert_eq!(bytes(json!("GAME.EXE+20104"), "near_ptr").unwrap(), [0x04, 0x01]);
        assert_eq!(bytes(json!("B800:1234"), "far").unwrap(), [0x34, 0x12, 0x00, 0xB8]);
        assert!(bytes(json!(2), "enemy_ptr").unwrap_err().contains("isn't an index of enemies"));
        assert!(bytes(json!(1), "near_ptr").unwrap_err().contains("names none"));
        assert!(bytes(json!("GAME.EXE+10000"), "near_ptr").unwrap_err().contains("isn't after"));
        let near = PointerKind::of_type("enemy_ptr", &k.types).unwrap().unwrap();
        assert_eq!(near.target(&[0x06, 0x01], 2), Some(Target::Id("GAME.EXE+20108".parse().unwrap())));
        assert_eq!(PointerKind::Far.target(&[0x10, 0, 0x00, 0x20], 0), Some(Target::Linear(0x20010)));
        assert!(PointerKind::of_type("u16", &k.types).is_none());
    }

    #[test]
    fn packed_bits_read_as_their_fields() {
        let mut t = types();
        let bit = |bit, width, name: &str, ty: &str| BitField { bit, width, name: name.into(), ty: ty.into() };
        t.bits.push(Bits {
            name: "status".into(),
            size: 1,
            fields: vec![bit(0, 1, "moving", "bool"), bit(1, 2, "facing", ""), bit(3, 5, "unused", "")],
            provenance: String::new(),
        });
        let r = Reader::new(&t);
        assert_eq!(decode(&[0b101], "status", &r).unwrap(), json!({"moving": true, "facing": 2, "unused": 0}));
        let all = json!({"moving": false, "facing": 3, "unused": 1});
        assert_eq!(encode(&all, "status", &r).unwrap(), [(0, vec![0b1110])]);
        assert!(encode(&json!({"moving": true}), "status", &r).unwrap_err().contains("give all their fields"));
        assert!(encode(&json!({"moving": 1, "facing": 0, "unused": 0}), "status", &r).is_err());
        assert!(encode(&json!({"moving": true, "facing": 4, "unused": 0}), "status", &r).is_err());
        // Bits are the smallest place.
        assert_eq!(locate("status", ".facing", &t), Some((0, "status".into())));
        t.bits[0].fields.pop();
        let r = Reader::new(&t);
        let some = json!({"moving": true, "facing": 0});
        assert!(encode(&some, "status", &r).unwrap_err().contains("bits 0xF8 have no field"));
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
