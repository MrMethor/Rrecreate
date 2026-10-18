//! Session diffs (PROJECT.md §8, L3): a session with Rust overrides runs in lockstep with the same
//! session running the originals; the two are compared at checkpoints, and the first divergence
//! is localized to the call that caused it.
//!
//! The two machines start as one. Between hooked calls both run the same code on the same state,
//! so they stay equal. At a hooked entry the reference runs the original and the test machine the
//! override. For the comparison to be exact, a hooked call is atomic on both sides: no device event
//! is serviced and no interrupt is taken until it returns. The test machine is charged the cycles
//! the original took, and it takes the reference's dead stack and what its convention leaves out
//! (scratch registers and flags, which no caller relies on: `--poison` checks that claim). A
//! correct override therefore leaves the test machine identical to the reference.
//!
//! Every `interval` cycles the machine states (their hashes) and the frames drawn since the last
//! checkpoint are compared. At the first difference both machines go back to the last equal
//! checkpoint and are compared after every call: the call after which they first differ is the
//! culprit, and becomes a finding with what a dual-run would show. If no call differs, the
//! divergence isn't an override's doing: suspect the host.

use std::collections::BTreeSet;

use cpu::seg;

use crate::hybrid::{Finding, Mode, ReturnTrap, compare, regs};
use crate::{Machine, Stop};

/// Longest a hooked call may run atomically.
pub const MAX_CALL_CYCLES: u64 = 20_000_000;

/// What a session diff found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    /// How the session ended (the cycle limit, or where it diverged).
    pub stop: Stop,
    pub checkpoints: u64,
    /// Hooked calls made, on each side.
    pub calls: u64,
    pub divergence: Option<Divergence>,
}

/// Where the two runs first differed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// Cycles of the last checkpoint where the machines were equal, and of the one where they
    /// were found to differ.
    pub equal_at: u64,
    pub found_at: u64,
    /// What differed: `state`, `frames`, or how the runs stopped.
    pub what: String,
    /// The hooked call after which the states first differed (counting all hooked calls from the
    /// session's start); its finding is the test machine's last. None: no call did.
    pub call: Option<u64>,
}

/// Runs `test` (with overrides hooked; those in Rust mode run Rust) until cycle `until`, in
/// lockstep with a copy that runs every original, checkpointing every `interval` cycles. Returns
/// the test machine as it ended (at the divergence, if there was one) and what was found.
pub fn diff(test: Machine, until: u64, interval: u64) -> (Machine, Diff) {
    let mut pair = Pair::new(test);
    let mut equal = pair.clone();
    let mut checkpoints = 0;
    loop {
        let (at, frames) = (pair.test.cpu.cycles, pair.test.frames.len());
        let target = at.saturating_add(interval.max(1)).min(until);
        let (ended, what) = match pair.advance(target, false) {
            Ok(ended) => (ended, None),
            Err(parted) => (None, Some(parted.what)),
        };
        checkpoints += 1;
        if let Some(what) = what.or_else(|| pair.differs(frames)) {
            let found_at = pair.test.cpu.cycles;
            let (pair, divergence) = localize(equal, target, what, (at, found_at));
            let diff = Diff { stop: Stop::CycleLimit, checkpoints, calls: pair.calls, divergence: Some(divergence) };
            return (pair.test, diff);
        }
        if let Some(stop) = ended.or((target >= until).then_some(Stop::CycleLimit)) {
            let diff = Diff { stop, checkpoints, calls: pair.calls, divergence: None };
            return (pair.test, diff);
        }
        equal = pair.clone();
    }
}

/// From the last equal checkpoint, compares after every call up to `until`.
fn localize(mut pair: Pair, until: u64, what: String, (equal_at, found_at): (u64, u64)) -> (Pair, Divergence) {
    let (what, call) = match pair.advance(until, true) {
        Err(parted) => (parted.what, parted.call),
        Ok(_) => (format!("{what}, but no call differs: not an override's doing (suspect the host)"), None),
    };
    (pair, Divergence { equal_at, found_at, what, call })
}

/// Why the runs parted.
struct Parted {
    what: String,
    call: Option<u64>,
}

#[derive(Clone)]
struct Pair {
    reference: Machine,
    test: Machine,
    calls: u64,
}

impl Pair {
    fn new(mut test: Machine) -> Pair {
        let mut reference = test.clone();
        let hybrid = reference.hybrid.as_mut().expect("a session diff needs overrides hooked");
        hybrid.originals_only = true;
        hybrid.pause = true;
        test.hybrid.as_mut().expect("a session diff needs overrides hooked").pause = true;
        Pair { reference, test, calls: 0 }
    }

    /// Runs both machines to cycle `target`, making the hooked calls on the way; with `check`,
    /// compares them after every call. Ok: the session's end, if it ended.
    fn advance(&mut self, target: u64, check: bool) -> Result<Option<Stop>, Parted> {
        loop {
            let a = self.reference.run_until(target);
            let b = self.test.run_until(target);
            match (a, b) {
                (Stop::Hooked(i), Stop::Hooked(j)) if i == j && self.reference.cpu.cycles == self.test.cpu.cycles => {
                    self.calls += 1;
                    self.call(i, check)?;
                }
                (Stop::CycleLimit, Stop::CycleLimit) => return Ok(None),
                (a, b) if a == b => return Ok(Some(b)),
                (a, b) => {
                    let what = format!("the runs stopped differently (original {a:?}, with overrides {b:?})");
                    return Err(Parted { what, call: None });
                }
            }
        }
    }

    /// Hooked call `i`, at its entry on both machines.
    fn call(&mut self, i: usize, check: bool) -> Result<(), Parted> {
        let (o, mode) = self.test.hybrid.as_ref().expect("hooked").overrides().nth(i).map(|(o, m)| (*o, m)).unwrap();
        let (r, t) = (&mut self.reference, &mut self.test);
        let regs = r.cpu.regs;
        let (ss, sp) = (regs.ss(), regs.sp());
        let linear = r.cpu.segment_base(seg::CS).wrapping_add(regs.ip);
        let ret_ip = r.mem.read_u16(Machine::lin(ss, sp));
        let ret_cs = if o.conv.far { r.mem.read_u16(Machine::lin(ss, sp.wrapping_add(2))) } else { regs.cs() };
        let ret_size = if o.conv.far { 4 } else { 2 };
        let trap = ReturnTrap::new(ret_cs, ret_ip, ss, sp.wrapping_add(ret_size).wrapping_add(o.conv.pops), sp);
        let start = r.cpu.cycles;
        let (stop, low_r) = r.original_call(linear, trap);
        if stop != Stop::Returned {
            let what = format!("{} (+{:05X}) didn't return within {MAX_CALL_CYCLES} cycles", o.name, o.offset);
            return Err(Parted { what, call: Some(self.calls) });
        }
        let cost = r.cpu.cycles - start;
        let low_t = if mode == Mode::Rust {
            t.begin_atomic(None);
            // Watches the stack that the override's calls into the program use.
            t.return_trap = Some(ReturnTrap::new(0, 0, ss, 0, sp));
            let result = t.run_override_public(o);
            let low = t.return_trap.take().map_or(sp, |watch| watch.lowest_sp);
            t.hybrid.as_mut().expect("hooked").stats[i].rust += 1;
            if let Err(e) = result {
                t.end_atomic();
                let what = format!("{} (+{:05X}) failed: {e}", o.name, o.offset);
                return Err(Parted { what, call: Some(self.calls) });
            }
            low
        } else {
            let (stop, low) = t.original_call(linear, trap);
            if stop != Stop::Returned {
                return Err(Parted { what: format!("{} didn't return", o.name), call: Some(self.calls) });
            }
            low
        };
        t.cpu.cycles = t.cpu.cycles.max(start + cost);
        t.end_atomic();
        // What a caller can't rely on comes from the reference: the dead stack (the callee's
        // frame, and the arguments the caller pops), scratch registers and flags.
        let dead_end = if o.conv.pops == 0 { sp.wrapping_add(ret_size).wrapping_add(o.conv.args) } else { sp };
        let low = low_r.min(low_t);
        for addr in Machine::lin(ss, low)..Machine::lin(ss, dead_end) {
            t.mem.write_phys(addr, r.mem.read_phys(addr));
        }
        let compared = o.conv.outputs | o.conv.preserved | regs::SP | regs::SS | regs::CS;
        for bit in (0..12).filter(|bit| compared & 1 << bit == 0) {
            match bit {
                0..8 => t.cpu.regs.gpr[bit] = r.cpu.regs.gpr[bit],
                8 => t.cpu.regs.seg[seg::ES] = r.cpu.regs.seg[seg::ES],
                11 => t.cpu.regs.seg[seg::DS] = r.cpu.regs.seg[seg::DS],
                _ => {}
            }
        }
        let scratch = u32::from(!o.conv.flags & 0x08D5); // CF, PF, AF, ZF, SF, OF
        t.cpu.regs.flags = (t.cpu.regs.flags & !scratch) | (r.cpu.regs.flags & scratch);
        if check && r.state_hash() != t.state_hash() {
            let hybrid = t.hybrid.as_ref().expect("hooked");
            let mut finding = Finding {
                offset: o.offset,
                name: o.name,
                call: hybrid.stats[i].rust,
                cycle: start,
                registers: Vec::new(),
                flags: None,
                memory: Vec::new(),
                vram: Vec::new(),
                palette: Vec::new(),
                note: None,
            };
            compare(r, t, o.conv, (ss, low, dead_end), &BTreeSet::new(), &mut finding);
            let found = !finding.registers.is_empty()
                || finding.flags.is_some()
                || !finding.memory.is_empty()
                || !finding.vram.is_empty()
                || !finding.palette.is_empty();
            if !found {
                finding.note =
                    Some("the machines differ after this call, outside what it returns (device state?)".into());
            }
            let hybrid = t.hybrid.as_mut().expect("hooked");
            hybrid.stats[i].mismatches += 1;
            hybrid.findings.push(finding);
            return Err(Parted { what: "state".into(), call: Some(self.calls) });
        }
        Ok(())
    }

    /// What differs at a checkpoint: the frames drawn since frame `frames`, or the state.
    fn differs(&self, frames: usize) -> Option<String> {
        if self.reference.frames.get(frames..) != self.test.frames.get(frames..) {
            return Some("frames".into());
        }
        (self.reference.state_hash() != self.test.state_hash()).then(|| "state".into())
    }
}

impl Machine {
    /// Runs the original at the hooked entry `linear` to its return, atomically.
    fn original_call(&mut self, linear: u32, trap: ReturnTrap) -> (Stop, u16) {
        self.begin_atomic(Some(linear));
        let result = self.run_to_return_within(trap, MAX_CALL_CYCLES);
        self.end_atomic();
        result
    }

    fn begin_atomic(&mut self, skip: Option<u32>) {
        self.set_atomic(true);
        let hybrid = self.hybrid.as_mut().expect("hooked");
        hybrid.pause = false;
        hybrid.skip = skip;
    }

    fn end_atomic(&mut self) {
        self.set_atomic(false);
        let hybrid = self.hybrid.as_mut().expect("hooked");
        hybrid.pause = true;
        hybrid.skip = None;
        self.settle();
    }
}
