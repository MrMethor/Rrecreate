//! Records: what a run produced (the devices' sound logs, the frames, the console, diagnostics), as
//! opposed to the machine's state. A record is history: it grows with every second of a run, and
//! nothing the machine does depends on it. So it isn't part of a snapshot (one taken an hour into a
//! game would otherwise carry an hour of sound) or of the state hash; frames and sound are compared
//! through their own hashes. Equality compares a record's contents, as for any value.
//!
//! A machine loaded from a snapshot starts its records empty: they hold what it produced since.
//!
//! The logs that grow with a run are [`Log`]s: copies of a machine (snapshots in memory) share
//! what the copied machine had logged, so taking a copy late in a game costs no more than early.

use std::hash::{Hash, Hasher};
use std::ops::{Deref, DerefMut, Index};
use std::sync::Arc;

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

/// Items in a [`Log`]'s full chunks.
const CHUNK: usize = 64;

/// An append-only list whose full chunks are shared between clones: a clone copies a pointer per
/// chunk and the last, unfilled one. Appending never copies what a clone shares.
#[derive(Clone)]
pub struct Log<T> {
    /// Full chunks of exactly [`CHUNK`] items.
    chunks: Vec<Arc<Vec<T>>>,
    /// The items after them: never more than [`CHUNK`], and holding the last item unless the log
    /// is empty (so [`Log::last_mut`] never has to copy a shared chunk).
    tail: Vec<T>,
}

impl<T> Log<T> {
    pub const fn new() -> Self {
        Self { chunks: Vec::new(), tail: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.chunks.len() * CHUNK + self.tail.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tail.is_empty()
    }

    pub fn push(&mut self, item: T) {
        if self.tail.len() == CHUNK {
            let full = std::mem::replace(&mut self.tail, Vec::with_capacity(CHUNK));
            self.chunks.push(Arc::new(full));
        }
        self.tail.push(item);
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        match self.chunks.get(index / CHUNK) {
            Some(chunk) => chunk.get(index % CHUNK),
            None => self.tail.get(index - self.chunks.len() * CHUNK),
        }
    }

    pub fn last(&self) -> Option<&T> {
        self.tail.last()
    }

    pub fn last_mut(&mut self) -> Option<&mut T> {
        self.tail.last_mut()
    }

    pub fn iter(&self) -> Iter<'_, T> {
        self.iter_from(0)
    }

    /// The items from `index` on.
    pub fn iter_from(&self, index: usize) -> Iter<'_, T> {
        let sealed = self.chunks.len() * CHUNK;
        if index >= sealed {
            let tail = &self.tail[(index - sealed).min(self.tail.len())..];
            return Iter { chunks: [].iter(), current: tail.iter(), tail: &[] };
        }
        let mut chunks = self.chunks[index / CHUNK..].iter();
        let first = chunks.next().expect("a chunk");
        Iter { chunks, current: first[index % CHUNK..].iter(), tail: &self.tail }
    }

    /// The index of the first item for which `pred` is false, for a log in which it is true of
    /// a prefix (a log in time order: the first item at or after a time).
    pub fn partition_point(&self, pred: impl Fn(&T) -> bool) -> usize {
        let (mut low, mut high) = (0, self.len());
        while low < high {
            let mid = low + (high - low) / 2;
            if pred(&self[mid]) {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low
    }

    pub fn to_vec(&self) -> Vec<T>
    where
        T: Clone,
    {
        self.iter().cloned().collect()
    }
}

impl<T> Default for Log<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> From<Vec<T>> for Log<T> {
    fn from(items: Vec<T>) -> Self {
        let mut log = Self::new();
        for item in items {
            log.push(item);
        }
        log
    }
}

impl<T> Index<usize> for Log<T> {
    type Output = T;

    fn index(&self, index: usize) -> &T {
        self.get(index).expect("index within the log")
    }
}

impl<T: PartialEq> PartialEq for Log<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl<T: Eq> Eq for Log<T> {}

impl<T: std::fmt::Debug> std::fmt::Debug for Log<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<'a, T> IntoIterator for &'a Log<T> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Iter<'a, T> {
        self.iter()
    }
}

/// A [`Log`]'s items in order.
pub struct Iter<'a, T> {
    chunks: std::slice::Iter<'a, Arc<Vec<T>>>,
    current: std::slice::Iter<'a, T>,
    tail: &'a [T],
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        loop {
            if let Some(item) = self.current.next() {
                return Some(item);
            }
            match self.chunks.next() {
                Some(chunk) => self.current = chunk.iter(),
                None if !self.tail.is_empty() => self.current = std::mem::take(&mut self.tail).iter(),
                None => return None,
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.current.len() + self.chunks.len() * CHUNK + self.tail.len();
        (n, Some(n))
    }
}

impl<T> ExactSizeIterator for Iter<'_, T> {}

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

    #[test]
    fn a_log_s_clones_share_its_full_chunks() {
        let mut a = Log::new();
        for i in 0..1000u32 {
            a.push(i);
        }
        let b = a.clone();
        a.push(1000);
        *a.last_mut().unwrap() += 1;
        assert!(Arc::ptr_eq(&a.chunks[3], &b.chunks[3]), "shared, not copied");
        assert_eq!((a.len(), b.len(), a[1000], b.last()), (1001, 1000, 1001, Some(&999)));
        assert_eq!(a.iter_from(130).take(3).copied().collect::<Vec<_>>(), [130, 131, 132]);
        assert_eq!(b.iter_from(990).count(), 10);
        assert_eq!(b.iter_from(5000).count(), 0);
        assert_eq!(a.iter().len(), 1001);
        assert!(b.iter().copied().eq(0..1000));
        assert_eq!(Log::from((0..1000).collect::<Vec<u32>>()), b);
        assert_eq!((b.partition_point(|&x| x < 777), b.partition_point(|_| true)), (777, 1000));
    }
}
