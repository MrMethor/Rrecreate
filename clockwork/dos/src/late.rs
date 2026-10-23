//! Input that changes within a tick (D60). A tick's inputs are what their sources hold at its sync
//! point, and the pass that starts there should read just that; input that arrives after the sync
//! point and before the pass reads it (a key pressed in between, a stick the game port reports a few
//! instructions later) is read by the pass but recorded with the next tick, so a port fed the tick's
//! values would do something else. This finds those ticks: at each counted sync point hit it keeps
//! its sources' bytes, and compares each read the program makes of them until the next one (a
//! device's: each call of its driver, each read of its port).
//!
//! The wait a sync point ends is left out (D47 puts the sync point where the wait is over): a timer
//! count it reads until it changes is read with the next tick's value, which the next tick records.
//! Late reads are the wait's when they were all made in the call the next hit of their source's sync
//! point comes in (the waiting function, or the loop's own code), the last at most [`WAIT_TAIL`] of the
//! program's instructions before it (interrupt handlers' don't count).
//!
//! The program's own writes change what it should read next; what interrupt handlers do is input
//! arriving, and their reads aren't the pass's ([`crate::calls`] tells them apart, and the calls,
//! so this turns call tracking on). Memory a host service reads for the program (the BIOS keyboard
//! buffer, through INT 16h) isn't seen.
//!
//! An analysis tool: not part of the machine's state, and it changes nothing the machine does.

use std::ops::Range;

use crate::calls::Frame;

/// Each source is reported once a tick, and this many findings in all.
pub const MAX_FINDINGS: usize = 10_000;

/// Late reads in the call of the next hit, the last this many of the program's instructions or fewer
/// before it, are the wait's. Measured on Wolf3D's waits: 3 to 22; the closest read of a pass, 59.
pub const WAIT_TAIL: u64 = 40;

/// What a source is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LateSource {
    /// `len` bytes of memory at linear address `at`.
    Memory { at: u32, len: u32 },
    /// An input device's state (D49), read through its driver or port.
    Device(crate::Device),
}

/// A tick whose pass read a source that had changed since its sync point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Late {
    /// The tick: the cycle of its sync point's counted hit.
    pub tick_cycle: u64,
    /// The source (an index into what [`LateInputs::new`] took).
    pub source: usize,
    /// When the program read it, and the instruction that did (linear; for a service, where its call
    /// returns to).
    pub cycle: u64,
    pub at: u32,
}

/// A read: when, by which instruction, and in which call (none: outside any).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Read {
    pub cycle: u64,
    pub at: u32,
    pub call: Option<Frame>,
}

#[derive(Debug, Clone)]
struct Source {
    what: LateSource,
    /// The sync points (linear addresses) whose hits start its ticks; empty: every point's.
    points: Vec<u32>,
    /// Since its last tick: where its memory is (physical, contiguous runs), and what each byte
    /// should read, in order (a device: its whole state). The tick's values, until the program writes.
    spans: Vec<Range<u32>>,
    expected: Vec<u8>,
    /// The cycle of its last tick; none before the first.
    tick: Option<u64>,
    /// Late reads waiting for the next hit (are they the wait's?), and whether one was found this tick
    /// (once is enough).
    pending: Option<Pending>,
    found: bool,
}

/// The late reads of a source since the first.
#[derive(Debug, Clone)]
struct Pending {
    first: Late,
    /// The call of the first, whether all were made in it, and when the last was ([`LateInputs::steps`]).
    call: Option<Frame>,
    one_call: bool,
    last: u64,
}

#[derive(Debug, Clone, Default)]
pub struct LateInputs {
    sources: Vec<Source>,
    /// Every watched physical byte's bounds, to pass over other accesses quickly.
    bounds: Range<u32>,
    pub findings: Vec<Late>,
    /// Findings past [`MAX_FINDINGS`].
    pub dropped: u64,
    /// The program's instructions run so far, interrupt handlers' left out.
    steps: u64,
}

impl LateInputs {
    /// Each source with the sync points whose hits start its ticks (none: every point's).
    pub fn new(sources: Vec<(LateSource, Vec<u32>)>) -> LateInputs {
        let sources = sources
            .into_iter()
            .map(|(what, points)| Source {
                what,
                points,
                spans: Vec::new(),
                expected: Vec::new(),
                tick: None,
                pending: None,
                found: false,
            })
            .collect();
        LateInputs { sources, ..LateInputs::default() }
    }

    /// The sources a hit of the sync point at `at` starts a tick for: their indexes and what they read.
    pub(crate) fn ticking(&self, at: u32) -> impl Iterator<Item = (usize, &LateSource)> {
        self.sources.iter().enumerate().filter(move |(_, s)| s.ticks_at(at)).map(|(i, s)| (i, &s.what))
    }

    /// The program (not an interrupt handler) ran an instruction.
    #[inline]
    pub(crate) fn step(&mut self) {
        self.steps += 1;
    }

    /// The sync point at `at` was reached (counted or not), in `call`: late reads of its sources are
    /// the wait's if they came just before in the same call, else the pass's.
    pub(crate) fn hit(&mut self, at: u32, call: Option<Frame>) {
        for i in 0..self.sources.len() {
            let s = &mut self.sources[i];
            if !s.ticks_at(at) {
                continue;
            }
            if let Some(p) = s.pending.take()
                && !(p.one_call && p.call == call && self.steps - p.last <= WAIT_TAIL)
            {
                s.found = true;
                self.push(p.first);
            }
        }
    }

    /// A tick of source `source` starts at `cycle`: its memory is at `spans` (physical) and holds
    /// `values` (in order), or the device holds `values`.
    pub(crate) fn tick(&mut self, source: usize, cycle: u64, spans: Vec<Range<u32>>, values: Vec<u8>) {
        let s = &mut self.sources[source];
        (s.spans, s.expected, s.tick, s.pending, s.found) = (spans, values, Some(cycle), None, false);
        let spans = || self.sources.iter().flat_map(|s| &s.spans);
        self.bounds = spans().map(|s| s.start).min().unwrap_or(0)..spans().map(|s| s.end).max().unwrap_or(0);
    }

    /// Whether physical address `addr` may be a source's (cheap: a bounds check).
    #[inline]
    pub(crate) fn watches(&self, addr: u32) -> bool {
        self.bounds.contains(&addr)
    }

    /// The program read `value` at physical address `addr`.
    pub(crate) fn memory_read(&mut self, addr: u32, value: u8, read: Read) {
        for (i, s) in self.sources.iter_mut().enumerate() {
            if let Some(byte) = s.byte(addr)
                && s.expected[byte] != value
            {
                s.late(i, read, self.steps);
            }
        }
    }

    /// The program wrote `value` at physical address `addr`: what it reads there next.
    pub(crate) fn memory_written(&mut self, addr: u32, value: u8) {
        for s in &mut self.sources {
            if let Some(byte) = s.byte(addr) {
                s.expected[byte] = value;
            }
        }
    }

    /// The program read `device` (called its driver, read its port); `state` is what it holds now.
    pub(crate) fn device_read(&mut self, device: crate::Device, state: &[u8], read: Read) {
        for (i, s) in self.sources.iter_mut().enumerate() {
            if s.what == LateSource::Device(device) && s.tick.is_some() && s.expected != state {
                s.late(i, read, self.steps);
            }
        }
    }

    /// The findings, with the late reads no hit came after (the run ended in the pass).
    pub fn finish(mut self) -> LateInputs {
        let pending: Vec<Late> = self.sources.iter_mut().filter_map(|s| s.pending.take()).map(|p| p.first).collect();
        pending.into_iter().for_each(|late| self.push(late));
        self.findings.sort_by_key(|l| (l.tick_cycle, l.source));
        self
    }

    fn push(&mut self, late: Late) {
        if self.findings.len() < MAX_FINDINGS {
            self.findings.push(late);
        } else {
            self.dropped += 1;
        }
    }
}

impl Source {
    fn ticks_at(&self, at: u32) -> bool {
        self.points.is_empty() || self.points.contains(&at)
    }

    /// Which of its bytes physical address `addr` is, if one.
    fn byte(&self, addr: u32) -> Option<usize> {
        let mut index = 0;
        for span in &self.spans {
            if span.contains(&addr) {
                return Some(index + (addr - span.start) as usize);
            }
            index += span.len();
        }
        None
    }

    /// It (source number `source`) was read with another value than expected, at `step`.
    fn late(&mut self, source: usize, read: Read, step: u64) {
        let Some(tick_cycle) = self.tick.filter(|_| !self.found) else { return };
        match &mut self.pending {
            Some(p) => {
                p.one_call &= p.call == read.call;
                p.last = step;
            }
            None => {
                let first = Late { tick_cycle, source, cycle: read.cycle, at: read.at };
                self.pending = Some(Pending { first, call: read.call, one_call: true, last: step });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_read_other_than_its_tick_left_it_is_late_once_a_tick() {
        let memory = LateSource::Memory { at: 0x100, len: 3 };
        let mut late = LateInputs::new(vec![(memory, vec![]), (LateSource::Device(crate::Device::Mouse), vec![7])]);
        assert_eq!(late.ticking(5).count(), 1);
        let read = |cycle, at| Read { cycle, at, call: None };
        // The program's instructions, here one a cycle.
        fn at(late: &mut LateInputs, cycle: u64) -> &mut LateInputs {
            late.steps = cycle;
            late
        }
        late.memory_read(0x100, 9, read(10, 1)); // before the first tick: nothing to compare
        late.tick(0, 100, vec![0x100..0x102, 0x200..0x201], vec![1, 2, 3]);
        assert!(late.watches(0x200) && !late.watches(0x201));
        late.memory_read(0x101, 2, read(110, 2)); // as at the tick
        late.memory_written(0x100, 5); // the program's own write
        late.memory_read(0x100, 5, read(115, 3));
        late.memory_read(0x200, 4, read(120, 4)); // changed: late
        late.memory_read(0x101, 7, read(130, 5)); // late too, but once a tick is enough
        late.device_read(crate::Device::Mouse, &[1, 0, 0, 0], read(135, 6)); // no tick of its own yet
        at(&mut late, 1000).hit(5, None);
        late.tick(0, 1000, std::iter::once(0x100..0x103).collect(), vec![1, 2, 4]);
        at(&mut late, 1100).memory_read(0x102, 9, read(1100, 7)); // the wait: the hit comes right after, in its call
        at(&mut late, 1100 + WAIT_TAIL).hit(5, None);
        late.tick(0, 1100 + WAIT_TAIL, std::iter::once(0x100..0x103).collect(), vec![1, 2, 9]);
        at(&mut late, 1200).memory_read(0x102, 8, read(1200, 8)); // just before the hit too, but not in its call
        at(&mut late, 1210).hit(5, Some(crate::calls::Frame::for_tests(0x500)));
        late.tick(0, 1300, std::iter::once(0x100..0x103).collect(), vec![1, 2, 8]);
        at(&mut late, 1400).memory_read(0x102, 9, read(1400, 11)); // a wait that polls until the value is far enough
        at(&mut late, 1500).memory_read(0x102, 10, read(1500, 11));
        at(&mut late, 1500 + WAIT_TAIL).hit(5, None);
        late.tick(0, 1600, std::iter::once(0x100..0x103).collect(), vec![1, 2, 10]);
        let callee = Some(crate::calls::Frame::for_tests(0x600));
        at(&mut late, 1610).memory_read(0x102, 11, Read { cycle: 1610, at: 12, call: callee }); // the pass, in a call
        at(&mut late, 1700).memory_read(0x102, 12, read(1700, 13)); // then the wait
        at(&mut late, 1701).hit(5, None);
        late.tick(1, 2000, vec![], vec![0; 4]);
        late.device_read(crate::Device::Mouse, &[0; 4], read(2010, 9));
        late.device_read(crate::Device::Mouse, &[1, 0, 0, 0], read(2020, 10)); // changed since: late, at the end
        let late = late.finish();
        let found: Vec<(u64, usize, u64, u32)> =
            late.findings.iter().map(|l| (l.tick_cycle, l.source, l.cycle, l.at)).collect();
        assert_eq!(found, [(100, 0, 120, 4), (1100 + WAIT_TAIL, 0, 1200, 8), (1600, 0, 1610, 12), (2000, 1, 2020, 10)]);
    }
}
