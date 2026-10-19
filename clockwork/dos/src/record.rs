//! Records: what a run produced (the devices' sound logs, the frames, the console, diagnostics), as
//! opposed to the machine's state. A record is history: it grows with every second of a run, and
//! nothing the machine does depends on it. So it isn't part of a snapshot (one taken an hour into a
//! game would otherwise carry an hour of sound) or of the state hash; frames and sound are compared
//! through their own hashes. Equality compares a record's contents, as for any value.
//!
//! A machine loaded from a snapshot starts its records empty: they hold what it produced since.

use std::hash::{Hash, Hasher};
use std::ops::{Deref, DerefMut};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record<T>(pub T);

impl<T> Deref for Record<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for Record<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<'a, T> IntoIterator for &'a Record<T>
where
    &'a T: IntoIterator,
{
    type Item = <&'a T as IntoIterator>::Item;
    type IntoIter = <&'a T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        (&self.0).into_iter()
    }
}

/// Not state: left out of the state hash.
impl<T> Hash for Record<T> {
    fn hash<H: Hasher>(&self, _: &mut H) {}
}

/// Not state: a snapshot holds nothing for it.
impl<T> serde::Serialize for Record<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_unit()
    }
}

impl<'de, T: Default> serde::Deserialize<'de> for Record<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        <()>::deserialize(deserializer)?;
        Ok(Self::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_isn_t_state() {
        let (a, b) = (Record(vec![1, 2, 3]), Record(Vec::<u8>::new()));
        let hash = |r: &Record<Vec<u8>>| {
            let mut h = crate::hash::Fnv64::default();
            r.hash(&mut h);
            h.finish()
        };
        assert_eq!(hash(&a), hash(&b));
        let bytes = postcard::to_stdvec(&a).unwrap();
        assert!(bytes.is_empty());
        let back: Record<Vec<u8>> = postcard::from_bytes(&bytes).unwrap();
        assert!(back.is_empty());
    }
}
