//! Bounds checks on declared data: an instruction that reads or writes a declared object (a named
//! table, with its size) and also touches the bytes just outside it is reported, with the offsets
//! it reached. Such an index past a table's end is usually an original bug the port has to keep
//! (a quirk).
//!
//! Every data access is looked up among the objects and the guard zones around them: up to the
//! object's size (at most 64 bytes) before and after it, stopping halfway to the next object. Only
//! instructions that also touched the object itself are reported, so code that happens to use the
//! bytes next to a table for something else isn't.

use std::collections::{BTreeMap, BTreeSet};

const MAX_SLACK: u32 = 64;
const MAX_OFFSETS: usize = 8;

/// A declared object: physical start and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Object {
    pub start: u32,
    pub size: u32,
}

/// An instruction that reached past an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Linear address of the instruction.
    pub instruction: u32,
    /// Index of the object in the list given to [`Bounds::new`].
    pub object: usize,
    /// Accesses outside it, and the first few offsets from its start they reached (negative
    /// before it, at least its size after it).
    pub outside: u64,
    pub offsets: Vec<i64>,
    pub first_cycle: u64,
}

#[derive(Debug, Clone)]
struct Near {
    count: u64,
    offsets: BTreeSet<i64>,
    first_cycle: u64,
}

#[derive(Debug, Clone)]
pub struct Bounds {
    /// (start, end, object), sorted.
    objects: Vec<(u32, u32, usize)>,
    /// Guard zones: (start, end, object), sorted.
    zones: Vec<(u32, u32, usize)>,
    /// Each object's start, by index.
    starts: Vec<u32>,
    inside: BTreeSet<(u32, usize)>,
    near: BTreeMap<(u32, usize), Near>,
}

impl Bounds {
    pub fn new(objects: &[Object]) -> Self {
        let mut sorted: Vec<(u32, u32, usize)> =
            objects.iter().enumerate().map(|(i, o)| (o.start, o.start + o.size.max(1), i)).collect();
        sorted.sort_unstable();
        let mut zones = Vec::new();
        for (k, &(start, end, i)) in sorted.iter().enumerate() {
            let slack = (end - start).clamp(2, MAX_SLACK);
            let low = k.checked_sub(1).map_or(0, |p| {
                let prev = sorted[p].1;
                prev + start.saturating_sub(prev).div_ceil(2)
            });
            let high = sorted.get(k + 1).map_or(u32::MAX, |next| end + next.0.saturating_sub(end) / 2);
            let before = start.saturating_sub(slack).max(low)..start;
            let after = end..end.saturating_add(slack).min(high);
            for zone in [before, after] {
                if !zone.is_empty() {
                    zones.push((zone.start, zone.end, i));
                }
            }
        }
        zones.sort_unstable();
        let starts = objects.iter().map(|o| o.start).collect();
        Self { objects: sorted, zones, starts, inside: BTreeSet::new(), near: BTreeMap::new() }
    }

    /// A data access by the instruction at linear address `instruction` to physical `addr`.
    pub(crate) fn access(&mut self, instruction: u32, addr: u32, cycle: u64) {
        if let Some(object) = find(&self.objects, addr) {
            self.inside.insert((instruction, object));
        } else if let Some(object) = find(&self.zones, addr) {
            let start = self.starts[object];
            let near = self.near.entry((instruction, object)).or_insert(Near {
                count: 0,
                offsets: BTreeSet::new(),
                first_cycle: cycle,
            });
            near.count += 1;
            if near.offsets.len() < MAX_OFFSETS {
                near.offsets.insert(i64::from(addr) - i64::from(start));
            }
        }
    }

    /// Instructions that touched an object and the bytes just outside it.
    pub fn findings(&self) -> Vec<Finding> {
        self.near
            .iter()
            .filter(|(key, _)| self.inside.contains(key))
            .map(|(&(instruction, object), near)| Finding {
                instruction,
                object,
                outside: near.count,
                offsets: near.offsets.iter().copied().collect(),
                first_cycle: near.first_cycle,
            })
            .collect()
    }
}

/// The entry of a sorted list of (start, end, value) whose range holds `addr`.
fn find(sorted: &[(u32, u32, usize)], addr: u32) -> Option<usize> {
    let k = sorted.partition_point(|&(start, ..)| start <= addr).checked_sub(1)?;
    let (_, end, value) = sorted[k];
    (addr < end).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_zones_stop_halfway_to_the_next_object() {
        let b = Bounds::new(&[Object { start: 100, size: 10 }, Object { start: 116, size: 4 }]);
        assert_eq!(b.zones, [(90, 100, 0), (110, 113, 0), (113, 116, 1), (120, 124, 1)]);
    }

    #[test]
    fn only_instructions_that_touched_the_object_are_reported() {
        let mut b = Bounds::new(&[Object { start: 100, size: 10 }]);
        b.access(1, 105, 7);
        b.access(1, 110, 8); // one past the end
        b.access(2, 111, 9); // something else's
        assert_eq!(
            b.findings(),
            [Finding { instruction: 1, object: 0, outside: 1, offsets: vec![10], first_cycle: 8 }]
        );
    }
}
