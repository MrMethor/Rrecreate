//! The original's memory as the knowledge's types read it, and back: `rrc-types`' (D126), with the knowledge's
//! named arrays for `into` pointers.

pub use rrc_types::values::*;

use ids::Id;

use crate::knowledge::Knowledge;

impl Arrays for Knowledge {
    fn array(&self, name: &str) -> Option<(Id, String, u32)> {
        let id = self.named(name)?;
        let (element, count) = array_of(&self.data.get(id)?.ty, &self.types)?;
        Some((id.clone(), element.to_owned(), count))
    }
}

/// A reader of the knowledge's types and named data.
pub fn of(k: &Knowledge) -> Reader<'_> {
    Reader::new(&k.types).with_arrays(k)
}
