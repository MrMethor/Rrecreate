//! Sync points: instruction addresses whose executions are counted, so a run can stop just before
//! one is reached for the n-th time, and a session's timeline (the cycle of each hit) can be
//! recorded. A game's main loop is the usual one: each pass is a tick.
//!
//! A hit is counted when the instruction is about to run. A run that stopped there and goes on
//! runs it without counting it again.

use std::collections::BTreeMap;

/// Linear addresses below this are checked against a bit set, above it against the map.
const BITS_END: u32 = 0x11_0000;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Point {
    /// Times the instruction was reached.
    pub hits: u64,
    /// The cycle of each hit, when logged.
    pub cycles: Option<Vec<u64>>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Syncs {
    points: BTreeMap<u32, Point>,
    bits: Vec<u64>,
    any_high: bool,
    /// Stop just before this point's hit number.
    target: Option<(u32, u64)>,
    /// The instruction a run stopped at: its hit was counted already.
    resume_at: Option<u32>,
}

impl Syncs {
    /// Counts the executions of the instruction at linear address `at` (from now on), logging the
    /// cycle of each if `log`. Adding a point again keeps its count.
    pub fn add(&mut self, at: u32, log: bool) {
        let point = self.points.entry(at).or_default();
        if log && point.cycles.is_none() {
            point.cycles = Some(Vec::new());
        }
        if at < BITS_END {
            if self.bits.is_empty() {
                self.bits = vec![0; (BITS_END / 64) as usize];
            }
            self.bits[(at / 64) as usize] |= 1 << (at % 64);
        } else {
            self.any_high = true;
        }
    }

    pub fn get(&self, at: u32) -> Option<&Point> {
        self.points.get(&at)
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub(crate) fn set_target(&mut self, at: u32, hit: u64) {
        self.target = Some((at, hit));
    }

    pub(crate) fn clear_target(&mut self) {
        self.target = None;
    }

    /// Before the instruction at `at` runs: counts a hit there, and says whether to stop (the
    /// target hit is reached; the instruction hasn't run).
    #[inline]
    pub(crate) fn reached(&mut self, at: u32, cycle: u64) -> bool {
        let candidate = if at < BITS_END {
            !self.bits.is_empty() && self.bits[(at / 64) as usize] >> (at % 64) & 1 != 0
        } else {
            self.any_high && self.points.contains_key(&at)
        };
        if !candidate {
            return false;
        }
        // An interrupt may run before the instruction a run stopped at: the mark waits for it.
        if self.resume_at == Some(at) {
            self.resume_at = None;
            return false;
        }
        let point = self.points.get_mut(&at).expect("a point for each bit");
        point.hits += 1;
        if let Some(cycles) = &mut point.cycles {
            cycles.push(cycle);
        }
        if self.target == Some((at, point.hits)) {
            self.target = None;
            self.resume_at = Some(at);
            return true;
        }
        false
    }
}
