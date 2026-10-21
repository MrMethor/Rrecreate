//! Sync points: instruction addresses whose executions are counted, so a run can stop just before
//! one is reached for the n-th time, and a session's timeline (the cycle of each hit) can be
//! recorded. A game's main loop is the usual one: each pass is a tick.
//!
//! A hit is counted when the instruction is about to run. A run that stopped there and goes on
//! runs it without counting it again.
//!
//! A point may have a gate (D48): what a busy-polling loop reads (its inputs, the counter its
//! timing reads). A hit then counts only when the gate's bytes differ from what they held at the
//! point's last counted hit, so a loop that polls as fast as the CPU runs ticks when something it
//! reads changed, not once a poll. The first hit always counts; the hits a gate passed over are
//! counted apart (`skipped`).

use std::collections::BTreeMap;

/// Linear addresses below this are checked against a bit set, above it against the map.
const BITS_END: u32 = 0x11_0000;

/// What a gate compares.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Source {
    /// `len` bytes of memory at linear address `at`.
    Memory { at: u32, len: u32 },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Gate {
    pub sources: Vec<Source>,
    /// The sources' bytes at the last counted hit (`None` where memory isn't mapped).
    last: Option<Vec<Option<u8>>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Point {
    /// Times the instruction was reached (with a gate: the hits it counted).
    pub hits: u64,
    /// The cycle of each hit, when logged.
    pub cycles: Option<Vec<u64>>,
    /// The hits a gate passed over just before the last counted one.
    pub skipped: u64,
    pub gate: Option<Gate>,
    /// The hits a gate passed over since the last counted one.
    passed: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Syncs {
    points: BTreeMap<u32, Point>,
    bits: Vec<u64>,
    any_high: bool,
    /// Stop just before this point's hit number.
    target: Option<(u32, u64)>,
    /// Stop just before the next hit of any point.
    next: bool,
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

    /// Gives the point at `at` (added if it isn't one) a gate on `sources`, or none if empty: from
    /// its next hit on, a hit counts only when they changed since the last counted one.
    pub fn gate(&mut self, at: u32, sources: Vec<Source>) {
        self.add(at, false);
        let point = self.points.get_mut(&at).expect("just added");
        point.gate = (!sources.is_empty()).then_some(Gate { sources, last: None });
    }

    pub fn get(&self, at: u32) -> Option<&Point> {
        self.points.get(&at)
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Whether `at` is a sync point (cheap: a bit test below 1 MB + 64 KB).
    #[inline]
    pub(crate) fn is_point(&self, at: u32) -> bool {
        if at < BITS_END {
            !self.bits.is_empty() && self.bits[(at / 64) as usize] >> (at % 64) & 1 != 0
        } else {
            self.any_high && self.points.contains_key(&at)
        }
    }

    /// What the gate of the point at `at` compares, if it has one.
    pub(crate) fn gate_sources(&self, at: u32) -> Option<&[Source]> {
        self.points.get(&at)?.gate.as_ref().map(|g| g.sources.as_slice())
    }

    pub(crate) fn set_target(&mut self, at: u32, hit: u64) {
        self.target = Some((at, hit));
    }

    pub(crate) fn set_next(&mut self) {
        self.next = true;
    }

    pub(crate) fn clear_target(&mut self) {
        self.target = None;
        self.next = false;
    }

    /// Before the instruction at `at`, a sync point, runs: counts a hit there (with a gate, only if
    /// `gate_state`, its sources' bytes now, differs from the last counted hit's), and says whether
    /// to stop (the target hit is reached; the instruction hasn't run).
    #[inline]
    pub(crate) fn reached(&mut self, at: u32, cycle: u64, gate_state: Option<Vec<Option<u8>>>) -> bool {
        // An interrupt may run before the instruction a run stopped at: the mark waits for it.
        if self.resume_at == Some(at) {
            self.resume_at = None;
            return false;
        }
        let point = self.points.get_mut(&at).expect("a point for each bit");
        if let (Some(gate), Some(state)) = (&mut point.gate, gate_state) {
            if gate.last.as_ref() == Some(&state) {
                point.passed += 1;
                return false;
            }
            gate.last = Some(state);
        }
        point.hits += 1;
        point.skipped = std::mem::take(&mut point.passed);
        if let Some(cycles) = &mut point.cycles {
            cycles.push(cycle);
        }
        if self.next || self.target == Some((at, point.hits)) {
            (self.target, self.next) = (None, false);
            self.resume_at = Some(at);
            return true;
        }
        false
    }
}
