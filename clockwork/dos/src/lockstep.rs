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
//! A call that can't finish atomically waits for an interrupt (a function spinning until the timer
//! handler moves a count). The pair goes back to the last equal checkpoint and from then on runs
//! that function's calls with interrupts on: the reference notes the cycles at which it takes
//! them and services its devices, and the test machine does both at exactly those cycles, in the
//! override's waits (`Call::wait_for_interrupt`) and then until the cycle the original returned,
//! so handlers run, sound advances and frames are captured at the same times on both machines.
//! Port writes are logged at the call's start, as in atomic calls, except the handlers'.
//! The original takes time where the
//! override takes none, so an interrupt can come after it stopped waiting and before it returned
//! (a tick between its last check of the time and its read of it): then the reference runs the
//! call again from its entry taking only the interrupts the override waited for, and the later
//! ones come after the call on both machines, as with atomic calls.
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
            Err(Parted { waits: Some(offset), .. }) if !equal.waiting.contains(&offset) => {
                // Again from the last equal state, that function's calls taking interrupts.
                equal.waiting.insert(offset);
                pair = equal.clone();
                continue;
            }
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
    /// The function whose call didn't finish atomically: it may wait for an interrupt.
    waits: Option<u32>,
}

impl Parted {
    fn at(what: String, call: Option<u64>) -> Parted {
        Parted { what, call, waits: None }
    }
}

#[derive(Clone)]
struct Pair {
    reference: Machine,
    test: Machine,
    calls: u64,
    /// Functions whose calls wait for interrupts (by image offset): they run with interrupts on.
    waiting: BTreeSet<u32>,
}

impl Pair {
    fn new(mut test: Machine) -> Pair {
        let mut reference = test.clone();
        let hybrid = reference.hybrid.as_mut().expect("a session diff needs overrides hooked");
        hybrid.originals_only = true;
        hybrid.pause = true;
        test.hybrid.as_mut().expect("a session diff needs overrides hooked").pause = true;
        Pair { reference, test, calls: 0, waiting: BTreeSet::new() }
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
                    return Err(Parted::at(what, None));
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
        let waits = self.waiting.contains(&o.offset);
        let entry = waits.then(|| r.clone());
        let (stop, mut low_r, interrupts, mut services) = r.original_call(linear, trap, waits, None);
        if stop != Stop::Returned {
            let what = format!("{} (+{:05X}) didn't return within {MAX_CALL_CYCLES} cycles", o.name, o.offset);
            return Err(Parted { what, call: Some(self.calls), waits: (!waits).then_some(o.offset) });
        }
        let mut cost = r.cpu.cycles - start;
        let low_t = if mode == Mode::Rust {
            t.begin_call(None, waits);
            if waits {
                t.irq_schedule = Some(interrupts.iter().copied().collect());
                t.service_schedule = Some(services.iter().copied().collect());
            }
            // Watches the stack that the override's calls into the program use.
            t.return_trap = Some(ReturnTrap::new(0, 0, ss, 0, sp));
            let result = t.run_override_public(o);
            let late = t.irq_schedule.as_ref().map_or(0, |s| s.len());
            let serviced = services.len() - t.service_schedule.as_ref().map_or(0, |s| s.len());
            let low = t.return_trap.take().map_or(sp, |watch| watch.lowest_sp);
            t.hybrid.as_mut().expect("hooked").stats[i].rust += 1;
            if let Err(e) = result {
                (t.irq_schedule, t.service_schedule) = (None, None);
                t.end_call();
                return Err(Parted::at(format!("{} (+{:05X}) failed: {e}", o.name, o.offset), Some(self.calls)));
            }
            if late > 0 {
                // The original took interrupts after the ones the override waited for: again from
                // its entry, taking only those.
                *r = entry.expect("a waiting call's entry");
                let waited = interrupts[..interrupts.len() - late].to_vec();
                let (stop, low, _, again) = r.original_call(linear, trap, true, Some(waited));
                if stop != Stop::Returned {
                    let what = format!(
                        "{} (+{:05X}) didn't return taking only the interrupts its override waited for",
                        o.name, o.offset
                    );
                    return Err(Parted::at(what, Some(self.calls)));
                }
                (low_r, cost, services) = (low, r.cpu.cycles - start, again);
            }
            if waits {
                // The rest of the original's time: its device events, its interrupts held.
                t.irq_schedule = Some(Default::default());
                t.service_schedule = Some(services[serviced.min(services.len())..].iter().copied().collect());
                t.pass_time(start + cost);
                (t.irq_schedule, t.service_schedule) = (None, None);
            }
            low
        } else {
            let (stop, low, ..) = t.original_call(linear, trap, waits, None);
            if stop != Stop::Returned {
                return Err(Parted::at(format!("{} didn't return", o.name), Some(self.calls)));
            }
            low
        };
        t.cpu.cycles = t.cpu.cycles.max(start + cost);
        t.end_call();
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
            return Err(Parted::at("state".into(), Some(self.calls)));
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
    /// Runs the original at the hooked entry `linear` to its return: atomically, or for a call
    /// that `waits`, taking interrupts (only at the cycles in `schedule`, if given) and noting the
    /// cycles it took them at.
    /// Returns how it stopped, its lowest SP, and for a call that waits, the cycles it took
    /// interrupts at and those it serviced device events at.
    fn original_call(
        &mut self,
        linear: u32,
        trap: ReturnTrap,
        waits: bool,
        schedule: Option<Vec<u64>>,
    ) -> (Stop, u16, Vec<u64>, Vec<u64>) {
        self.begin_call(Some(linear), waits);
        (self.irq_log, self.service_log) = (waits.then(Vec::new), waits.then(Vec::new));
        self.irq_schedule = schedule.map(Into::into);
        let (stop, low) = self.run_to_return_within(trap, MAX_CALL_CYCLES);
        let interrupts = self.irq_log.take().unwrap_or_default();
        let services = self.service_log.take().unwrap_or_default();
        self.irq_schedule = None;
        self.end_call();
        (stop, low, interrupts, services)
    }

    fn begin_call(&mut self, skip: Option<u32>, waits: bool) {
        self.set_atomic(!waits);
        self.call_writes = waits.then(|| crate::machine::CallWrites { since: self.cpu.cycles, ..Default::default() });
        let hybrid = self.hybrid.as_mut().expect("hooked");
        hybrid.pause = false;
        hybrid.skip = skip;
    }

    fn end_call(&mut self) {
        self.set_atomic(false);
        self.call_writes = None;
        let hybrid = self.hybrid.as_mut().expect("hooked");
        hybrid.pause = true;
        hybrid.skip = None;
        self.settle();
    }
}
