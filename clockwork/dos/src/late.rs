//! Input that changes within a tick (D60). A tick's inputs are what their sources hold at its sync
//! point, and the pass that starts there should read just that; input that arrives after the sync
//! point and before the pass reads it (a key pressed in between, a stick the game port reports a few
//! instructions later, a clock read again after the frame was drawn) is read by the pass but recorded
//! with the next tick, so a port fed the tick's values would do something else. This finds those reads:
//! at each counted sync point hit it keeps its sources' bytes, and compares each read the program makes
//! of them until the next one (a device's: each call of its driver, each read of its port). It numbers
//! each instruction's reads of a source in the pass, and keeps a read's bytes that differ from the
//! tick's where they differ from what that instruction's read before saw (D68, D93): so a port can be
//! given what each read saw, a polling loop's included (a modal wait reading the mouse until a click).
//!
//! The wait a sync point ends is left out (D47 puts the sync point where the wait is over): a timer
//! count it reads until it changes is read with the next tick's value, which the next tick records.
//! An instruction's late reads are the wait's when they were all made in the call the next hit of
//! their source's sync point comes in (the waiting function, or the loop's own code), the last at most
//! [`WAIT_TAIL`] of the program's instructions before it (interrupt handlers' don't count).
//!
//! The program's own writes change what it should read next; what interrupt handlers do is input
//! arriving, and their reads aren't the pass's ([`crate::calls`] tells them apart, and the calls,
//! so this turns call tracking on). Memory a host service reads for the program (the BIOS keyboard
//! buffer, through INT 16h) isn't seen.
//!
//! An analysis tool: not part of the machine's state, and it changes nothing the machine does.

use std::ops::Range;

use crate::calls::Frame;

/// Findings kept in all: past this many they are counted.
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

/// A read in a tick's pass that saw a source otherwise than the instruction's read before it did (the
/// first, otherwise than the tick's value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Late {
    /// The tick: the cycle of its sync point's counted hit.
    pub tick_cycle: u64,
    /// The source (an index into what [`LateInputs::new`] took).
    pub source: usize,
    /// When the program read it, the instruction that did (linear; for a service, where its call
    /// returns to), and which of that instruction's reads of the source in the pass it was (from 1).
    pub cycle: u64,
    pub at: u32,
    pub read: u32,
    /// The bytes it read that differ from what the program should have read (the tick's value, as the
    /// program's own writes changed it): (offset in the source, value). None: it read the tick's value
    /// again, after a read that didn't.
    pub bytes: Vec<(u32, u8)>,
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
    /// Each instruction's reads of it this tick, waiting for the next hit (are its late reads the
    /// wait's?), and whether this tick's were reported (a hit of another source's sync point ended it).
    readers: Vec<Reader>,
    found: bool,
}

/// An instruction's reads of a source this tick.
#[derive(Debug, Clone)]
struct Reader {
    at: u32,
    /// Its reads so far, the step of the last (the bytes of one read come one by one), and what that
    /// read saw that differs from the tick's value.
    reads: u32,
    step: u64,
    cycle: u64,
    current: Vec<(u32, u8)>,
    /// What its last kept read saw (none: the tick's value), and the reads kept.
    seen: Vec<(u32, u8)>,
    kept: Vec<Late>,
    /// The call of its first late read, whether all its late reads were made in it, and the step of the
    /// last ([`LateInputs::steps`]).
    call: Option<Option<Frame>>,
    one_call: bool,
    last: u64,
}

impl Reader {
    /// The read under way is over: kept if it saw otherwise than the read before.
    fn end_read(&mut self, tick_cycle: u64, source: usize) {
        if self.reads == 0 {
            return;
        }
        let mut now = std::mem::take(&mut self.current);
        now.sort_unstable();
        if now != self.seen {
            self.kept.push(Late {
                tick_cycle,
                source,
                cycle: self.cycle,
                at: self.at,
                read: self.reads,
                bytes: now.clone(),
            });
            self.seen = now;
        }
    }
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
                readers: Vec::new(),
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
            let Some(tick_cycle) = s.tick else { continue };
            let readers = std::mem::take(&mut s.readers);
            for mut r in readers {
                r.end_read(tick_cycle, i);
                let wait = r.call.is_some_and(|c| c == call) && r.one_call && self.steps - r.last <= WAIT_TAIL;
                if !wait && !r.kept.is_empty() {
                    self.sources[i].found = true;
                    r.kept.into_iter().for_each(|late| self.push(late));
                }
            }
        }
    }

    /// A tick of source `source` starts at `cycle`: its memory is at `spans` (physical) and holds
    /// `values` (in order), or the device holds `values`.
    pub(crate) fn tick(&mut self, source: usize, cycle: u64, spans: Vec<Range<u32>>, values: Vec<u8>) {
        let s = &mut self.sources[source];
        (s.spans, s.expected, s.tick, s.readers, s.found) = (spans, values, Some(cycle), Vec::new(), false);
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
            if let Some(byte) = s.byte(addr) {
                let differ: &[(u32, u8)] = if s.expected[byte] != value { &[(byte as u32, value)] } else { &[] };
                s.read(i, read, self.steps, differ);
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
            if s.what == LateSource::Device(device) && s.tick.is_some() {
                let differ: Vec<(u32, u8)> = (0..state.len())
                    .filter(|&k| s.expected.get(k) != Some(&state[k]))
                    .map(|k| (k as u32, state[k]))
                    .collect();
                s.read(i, read, self.steps, &differ);
            }
        }
    }

    /// The program called `device`'s driver, which held `before` and holds `after`: what the call
    /// changed (a position set, counts read and cleared) is what it reads next. The rest isn't the
    /// program's: a button let go before the call still differs from what it should read.
    pub(crate) fn device_written(&mut self, device: crate::Device, before: &[u8], after: &[u8]) {
        for s in &mut self.sources {
            if s.what == LateSource::Device(device) && s.tick.is_some() {
                for (k, (&was, &now)) in before.iter().zip(after).enumerate() {
                    if was != now
                        && let Some(byte) = s.expected.get_mut(k)
                    {
                        *byte = now;
                    }
                }
            }
        }
    }

    /// The findings, with the late reads no hit came after (the run ended in the pass).
    pub fn finish(mut self) -> LateInputs {
        let mut kept = Vec::new();
        for (i, s) in self.sources.iter_mut().enumerate() {
            let Some(tick_cycle) = s.tick else { continue };
            for mut r in std::mem::take(&mut s.readers) {
                r.end_read(tick_cycle, i);
                kept.extend(r.kept);
            }
        }
        kept.into_iter().for_each(|late| self.push(late));
        self.findings.sort_by_key(|l| (l.tick_cycle, l.source, l.cycle, l.at, l.read));
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

    /// It (source number `source`) was read at `step`; `bytes` (offset, value) differ from what the
    /// program should have read. A read's bytes come one by one, in the same step.
    fn read(&mut self, source: usize, read: Read, step: u64, bytes: &[(u32, u8)]) {
        let Some(tick_cycle) = self.tick.filter(|_| !self.found) else { return };
        let r = match self.readers.iter().position(|r| r.at == read.at) {
            Some(r) => &mut self.readers[r],
            None => {
                self.readers.push(Reader {
                    at: read.at,
                    reads: 0,
                    step: 0,
                    cycle: 0,
                    current: Vec::new(),
                    seen: Vec::new(),
                    kept: Vec::new(),
                    call: None,
                    one_call: true,
                    last: 0,
                });
                self.readers.last_mut().expect("just pushed")
            }
        };
        if r.reads == 0 || r.step != step {
            r.end_read(tick_cycle, source);
            (r.reads, r.step, r.cycle) = (r.reads + 1, step, read.cycle);
        }
        for &(offset, value) in bytes {
            if !r.current.iter().any(|(o, _)| *o == offset) {
                r.current.push((offset, value));
            }
        }
        if !bytes.is_empty() {
            match r.call {
                Some(call) => r.one_call &= call == read.call,
                None => r.call = Some(read.call),
            }
            r.last = step;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_read_that_saw_otherwise_than_the_one_before_is_kept() {
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
        at(&mut late, 110).memory_read(0x101, 2, read(110, 2)); // as at the tick
        late.memory_written(0x100, 5); // the program's own write
        at(&mut late, 115).memory_read(0x100, 5, read(115, 3));
        at(&mut late, 120).memory_read(0x200, 4, read(120, 4)); // changed: late, the first read there
        at(&mut late, 130).memory_read(0x101, 7, read(130, 5)); // late too, by another instruction
        at(&mut late, 131).memory_read(0x200, 6, read(131, 4)); // the first instruction again, changed again
        at(&mut late, 132).memory_read(0x200, 6, read(132, 4)); // and as it read before: nothing new
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
        at(&mut late, 1700).memory_read(0x102, 12, read(1700, 13)); // then the wait: left out
        at(&mut late, 1701).hit(5, None);
        late.tick(1, 2000, vec![], vec![0; 4]);
        // A modal wait reading the mouse until a click: its third read is the one that saw it, its fourth
        // the release (the tick's value again).
        at(&mut late, 2010).device_read(crate::Device::Mouse, &[0; 4], read(2010, 9));
        at(&mut late, 2015).device_read(crate::Device::Mouse, &[0; 4], read(2015, 9));
        at(&mut late, 2020).device_read(crate::Device::Mouse, &[1, 0, 0, 0], read(2020, 9));
        at(&mut late, 2030).device_read(crate::Device::Mouse, &[0; 4], read(2030, 9));
        // Pressed again; a read's call changes nothing, so the next read is as the one before (not the
        // tick's value again); a position set is the program's.
        let pressed = [1, 0, 0, 0];
        at(&mut late, 2040).device_read(crate::Device::Mouse, &pressed, read(2040, 9));
        late.device_written(crate::Device::Mouse, &pressed, &pressed);
        at(&mut late, 2050).device_read(crate::Device::Mouse, &pressed, read(2050, 9));
        late.device_written(crate::Device::Mouse, &pressed, &[1, 0, 5, 0]);
        at(&mut late, 2060).device_read(crate::Device::Mouse, &[1, 0, 5, 0], read(2060, 9));
        let late = late.finish();
        let l = |tick_cycle, source, cycle, at, read, bytes: &[(u32, u8)]| Late {
            tick_cycle,
            source,
            cycle,
            at,
            read,
            bytes: bytes.to_vec(),
        };
        assert_eq!(
            late.findings,
            [
                l(100, 0, 120, 4, 1, &[(2, 4)]),
                l(100, 0, 130, 5, 1, &[(1, 7)]),
                l(100, 0, 131, 4, 2, &[(2, 6)]),
                l(1100 + WAIT_TAIL, 0, 1200, 8, 1, &[(2, 8)]),
                l(1600, 0, 1610, 12, 1, &[(2, 11)]),
                l(2000, 1, 2020, 9, 3, &[(0, 1)]),
                l(2000, 1, 2030, 9, 4, &[]),
                l(2000, 1, 2040, 9, 5, &[(0, 1)]),
            ]
        );
    }
}
