//! A port played on its own (D226, the person's: a port is a program that plays, reading the game's files, and
//! nothing beside it): no `rrc`, no Clockwork, no start folder. What `rrc` gives a port's start from power-on (the
//! program's initial data, D135; the start-up's inputs, D141) the port makes itself from the game's program, by its
//! book: `standalone.json` in the port repository, which `rrc standalone` writes from the mapping and the knowledge
//! (`docs/formats.md`) and the port builds in. It holds where each field's initial value is in the program's load
//! image and its type, the inputs the start-up finds in the image, the program's file and its versions, and the
//! arguments the port's runs give it; nothing of the game's own.

use std::collections::BTreeMap;

use rrc_types::Types;
use rrc_types::values::{Arrays, Reader, decode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Inputs;

/// The book's file in a port repository.
pub const FILE: &str = "standalone.json";

/// What a port played on its own needs to start from power-on (`rrc-standalone`, version 1).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Book {
    pub format: String,
    pub version: u32,
    /// The game's program, by its file's name in the game's folder (`ACD.EXE`).
    pub program: String,
    /// The SHA-256 of the program's file in each version the port is of (lowercase hex).
    #[serde(default)]
    pub versions: Vec<String>,
    /// The arguments the port's runs give the program (its command line after its name).
    #[serde(default)]
    pub args: String,
    /// The knowledge's types, which the items' are.
    #[serde(default)]
    pub types: Types,
    /// The named arrays a pointer type is read into, by its element (D72).
    #[serde(default)]
    pub arrays: Vec<Array>,
    /// The fields whose initial values are in the load image (D135).
    #[serde(default)]
    pub items: Vec<Item>,
    /// The inputs at places in the load image: at power-on, what the image holds there.
    #[serde(default)]
    pub inputs: Vec<Input>,
}

/// A named array: its ID, its element's type, its length.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Array {
    pub name: String,
    pub id: String,
    pub element: String,
    pub count: u32,
}

/// A field's initial value: `size` bytes at `offset` in the load image, read as `type` (masked; or, for a pointer
/// mapped as whether it is set, D178, whether any byte is set).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub field: String,
    pub offset: u32,
    pub size: u32,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub set: bool,
}

/// An input at `offset` in the load image, `size` bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    pub name: String,
    pub offset: u32,
    pub size: u32,
}

/// The book's version.
pub const VERSION: u32 = 1;

impl Book {
    /// The book `text` (`standalone.json`).
    pub fn read(text: &str) -> Result<Book, String> {
        let book: Book = serde_json::from_str(text).map_err(|e| format!("{FILE}: {e}"))?;
        if book.format != "rrc-standalone" || book.version != VERSION {
            return Err(format!("{FILE}: not a book this runtime reads (rrc standalone writes it again)"));
        }
        Ok(book)
    }

    /// The book as its file holds it.
    pub fn text(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).unwrap_or_default();
        text.push('\n');
        text
    }

    /// The program's initial data (D135): each item as `image` (the load image) holds it, by field. Items past the
    /// image are left out (the program's uninitialised memory).
    pub fn initial(&self, image: &[u8]) -> Result<BTreeMap<String, Value>, String> {
        let r = Reader::new(&self.types).with_arrays(self);
        let mut state = BTreeMap::new();
        for item in &self.items {
            let at = item.offset as usize;
            let Some(bytes) = image.get(at..at + item.size as usize) else { continue };
            let value = if item.set {
                Value::Bool(bytes.iter().any(|&b| b != 0))
            } else {
                let value = decode(bytes, &item.ty, &r).map_err(|e| format!("{}: {e}", item.field))?;
                match (item.mask, value.as_u64()) {
                    (Some(mask), Some(n)) => Value::from(n & mask),
                    _ => value,
                }
            };
            state.insert(item.field.clone(), value);
        }
        Ok(state)
    }

    /// The inputs in the load image as the start-up finds them there (D134): what `image` holds at their places.
    pub fn inputs(&self, image: &[u8]) -> BTreeMap<String, Vec<u8>> {
        self.inputs
            .iter()
            .filter_map(|i| {
                Some((i.name.clone(), image.get(i.offset as usize..(i.offset + i.size) as usize)?.to_vec()))
            })
            .collect()
    }

    /// The command line's tail as DOS gives it the program in its PSP at 80h: its length, the arguments after a
    /// space, a carriage return, then zeros, 128 bytes.
    pub fn command_tail(&self) -> Vec<u8> {
        let mut tail = vec![0u8; 128];
        let text: Vec<u8> = match self.args.is_empty() {
            true => Vec::new(),
            false => format!(" {}", self.args).into_bytes(),
        };
        let n = text.len().min(126);
        tail[0] = n as u8;
        tail[1..1 + n].copy_from_slice(&text[..n]);
        tail[1 + n] = 0x0D;
        tail
    }
}

impl Arrays for Book {
    fn array(&self, name: &str) -> Option<(ids::Id, String, u32)> {
        let a = self.arrays.iter().find(|a| a.name == name)?;
        Some((a.id.parse().ok()?, a.element.clone(), a.count))
    }
}

/// The port's start from power-on on its own (D226): its state from `S::start()` with the program's initial data
/// set (for a port with `INITIAL_DATA`), and the start-up's inputs the image holds.
pub fn started<S: crate::Simulation>(book: &Book) -> Result<crate::Started<S>, String> {
    let program = crate::original::try_program()?;
    let state = match S::INITIAL_DATA {
        true => {
            let initial = crate::files::StartState { tick: 0, state: book.initial(program.image())? };
            S::transplant(initial.overlay(crate::serialize(&S::start())?)?)?
        }
        false => S::start(),
    };
    let power_on = Inputs::new(book.inputs(program.image()));
    Ok(crate::Started { state, presentation: None, screen: None, power_on: Some(power_on) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> Book {
        Book {
            format: "rrc-standalone".into(),
            version: VERSION,
            program: "GAME.EXE".into(),
            args: "-x 1".into(),
            items: vec![
                Item { field: "a".into(), offset: 1, size: 2, ty: "u16".into(), mask: None, set: false },
                Item { field: "b".into(), offset: 3, size: 1, ty: "u8".into(), mask: Some(0x0F), set: false },
                Item { field: "c".into(), offset: 4, size: 2, ty: "u16".into(), mask: None, set: true },
                Item { field: "past".into(), offset: 9, size: 2, ty: "u16".into(), mask: None, set: false },
            ],
            inputs: vec![Input { name: "keys".into(), offset: 0, size: 2 }],
            ..Book::default()
        }
    }

    #[test]
    fn the_initial_data_and_the_inputs_are_read_from_the_image_by_the_book() {
        let image = [7, 0x34, 0x12, 0xAB, 0, 1];
        let initial = book().initial(&image).unwrap();
        assert_eq!(initial["a"], 0x1234);
        assert_eq!(initial["b"], 0x0B);
        assert_eq!(initial["c"], true);
        assert!(!initial.contains_key("past"));
        assert_eq!(book().inputs(&image)["keys"], [7, 0x34]);
        assert_eq!(Book::read(&book().text()).unwrap(), book());
    }

    #[test]
    fn the_command_tail_is_dos_s() {
        let tail = book().command_tail();
        assert_eq!(&tail[..7], b"\x05 -x 1\r");
        assert!(tail[7..].iter().all(|&b| b == 0) && tail.len() == 128);
        let none = Book { args: String::new(), ..book() }.command_tail();
        assert_eq!(&none[..2], b"\x00\r");
    }
}
