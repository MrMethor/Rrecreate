//! The knowledge's types (`types.toml`: structures, enumerations, pointers, packed bits) and the original's
//! memory read and written as their values ([`values`]): what `rrc` decodes the original's state with, and what
//! a port reads and writes the original's memory images with (its saved games, D126).

use std::collections::BTreeMap;

pub mod values;

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
    /// bytes), the address segment × 16 + offset (real mode); `segment`: a segment alone (2 bytes),
    /// pointing at its start.
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
