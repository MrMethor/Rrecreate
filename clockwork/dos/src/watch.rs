//! Memory watches: every write (and, if asked, read) of the watched bytes, with who did it: the
//! instruction, the call chain it ran in, and the cycle. Writes by host services (a DOS read
//! filling a buffer) and by devices (DMA) are found by comparing the watched bytes around them.
//!
//! Watches work like MAME's (`wpset`): on the bus, so every access counts, a write of the same
//! value included, rather than DOSBox's polling for changed values. Addresses are physical.
//!
//! The call chain is a shadow stack kept from what instructions do to the stack: a transfer of
//! control that pushes a return address just past the instruction is a call (or a software
//! interrupt, when flags were pushed too); a frame ends when the stack rises above its return
//! address. Hardware interrupts open frames of their own. No decoding is needed, so it follows
//! hand-written assembly as well as compiled code.

use std::ops::Range;

use cpu::{Bus, Cpu, seg};

use crate::machine::PcBus;

/// Hits kept, at most; later ones are counted in [`Watch::dropped`].
pub const DEFAULT_LIMIT: usize = 10_000;

/// Watched bytes compared around each service call and device event, at most (beyond, those
/// writes aren't seen).
pub const MAX_COMPARED: u32 = 0x1_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Read,
    Write,
}

/// What made an access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    /// The instruction at `linear` (`cs:ip`).
    Instruction { cs: u16, ip: u32, linear: u32 },
    /// A host service (BIOS, DOS, EMS, ...), entered through callback `id` with AX = `ax`.
    Service { id: u16, ax: u16 },
    /// A device event (DMA, the mouse cursor).
    Device,
}

/// One frame of the call chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// Linear address the call or interrupt went to.
    pub entry: u32,
    /// Linear address it returns to.
    pub from: u32,
    /// An interrupt (software or hardware), not a call.
    pub interrupt: bool,
    /// Where its return address is on the stack (linear), and the stack segment's base.
    slot: u32,
    stack_base: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub cycle: u64,
    /// Physical address of the byte.
    pub address: u32,
    pub kind: Kind,
    /// The byte read or written, and for a write the byte it replaced.
    pub value: u8,
    pub old: u8,
    pub by: By,
    /// The call chain at the access, innermost first.
    pub chain: Vec<Frame>,
}

/// The instruction about to run, as [`Watch::before`] saw it.
#[derive(Debug, Clone, Copy, Default)]
struct Context {
    cs: u16,
    ip: u32,
    code_base: u32,
    stack_base: u32,
    top: u32,
}

#[derive(Debug, Clone)]
pub struct Watch {
    ranges: Vec<Range<u32>>,
    bounds: Range<u32>,
    reads: bool,
    pub limit: usize,
    pub hits: Vec<Hit>,
    /// Hits past `limit`.
    pub dropped: u64,
    stack: Vec<Frame>,
    context: Context,
}

impl Watch {
    /// Watches writes to `ranges` (physical addresses), and reads too if `reads`.
    pub fn new(ranges: Vec<Range<u32>>, reads: bool) -> Self {
        let bounds = ranges.iter().map(|r| r.start).min().unwrap_or(0)..ranges.iter().map(|r| r.end).max().unwrap_or(0);
        Self {
            ranges,
            bounds,
            reads,
            limit: DEFAULT_LIMIT,
            hits: Vec::new(),
            dropped: 0,
            stack: Vec::new(),
            context: Context::default(),
        }
    }

    #[inline]
    fn watched(&self, address: u32) -> bool {
        self.bounds.contains(&address) && self.ranges.iter().any(|r| r.contains(&address))
    }

    /// The call chain now, innermost first.
    pub fn chain(&self) -> Vec<Frame> {
        self.stack.iter().rev().copied().collect()
    }

    fn record(&mut self, cycle: u64, address: u32, kind: Kind, value: u8, old: u8, by: By) {
        if self.hits.len() >= self.limit {
            self.dropped += 1;
            return;
        }
        let chain = self.chain();
        self.hits.push(Hit { cycle, address, kind, value, old, by, chain });
    }

    fn by_instruction(&self) -> By {
        let c = &self.context;
        By::Instruction { cs: c.cs, ip: c.ip, linear: c.code_base.wrapping_add(c.ip) }
    }

    /// Before an instruction runs.
    pub(crate) fn before(&mut self, cpu: &Cpu) {
        self.context = Context {
            cs: cpu.regs.cs(),
            ip: cpu.regs.ip,
            code_base: cpu.segment_base(seg::CS),
            stack_base: cpu.segment_base(seg::SS),
            top: stack_top(cpu),
        };
    }

    /// After it ran: a call or software interrupt opens a frame, a return closes frames.
    /// `peek` reads guest memory at a linear address without side effects.
    pub(crate) fn after(&mut self, cpu: &Cpu, peek: impl Fn(u32) -> Option<u8>) {
        let c = self.context;
        let (stack_base, top) = (cpu.segment_base(seg::SS), stack_top(cpu));
        let linear_ip = cpu.segment_base(seg::CS).wrapping_add(cpu.regs.ip);
        if stack_base == c.stack_base && top < c.top {
            let u16_at = |at: u32| Some(u16::from_le_bytes([peek(at)?, peek(at.wrapping_add(1))?]));
            let u32_at = |at: u32| Some(u32::from(u16_at(at)?) | u32::from(u16_at(at.wrapping_add(2))?) << 16);
            // The return address a call or interrupt pushed: just past the instruction.
            let past = |ret: u32| ret > c.ip && ret <= c.ip.wrapping_add(15);
            let pushed = match c.top - top {
                2 => u16_at(top).map(u32::from).filter(|&r| past(r)).map(|r| (r, false)),
                4 => match (u16_at(top), u16_at(top + 2), u32_at(top)) {
                    (Some(ip), Some(cs), _) if cs == c.cs && past(u32::from(ip)) => Some((u32::from(ip), false)),
                    (_, _, Some(eip)) if past(eip) => Some((eip, false)),
                    _ => None,
                },
                6 => match (u16_at(top), u16_at(top + 2)) {
                    (Some(ip), Some(cs)) if cs == c.cs && past(u32::from(ip)) => Some((u32::from(ip), true)),
                    _ => None,
                },
                8 => u32_at(top).filter(|&r| past(r)).map(|r| (r, false)),
                12 => u32_at(top).filter(|&r| past(r)).map(|r| (r, true)),
                _ => None,
            };
            if let Some((ret, interrupt)) = pushed {
                let from = c.code_base.wrapping_add(ret);
                // Control went elsewhere: not a PUSH of a value that happens to look like one.
                if linear_ip != from {
                    self.stack.push(Frame { entry: linear_ip, from, interrupt, slot: top, stack_base });
                }
            }
        }
        self.unwind(stack_base, top);
    }

    /// A hardware interrupt was taken: it opens a frame of its own.
    pub(crate) fn interrupt(&mut self, cpu: &Cpu, from: u32) {
        let entry = cpu.segment_base(seg::CS).wrapping_add(cpu.regs.ip);
        let stack_base = cpu.segment_base(seg::SS);
        self.stack.push(Frame { entry, from, interrupt: true, slot: stack_top(cpu), stack_base });
    }

    /// Frames whose return address the stack has risen above are gone.
    fn unwind(&mut self, stack_base: u32, top: u32) {
        while let Some(frame) = self.stack.last()
            && frame.stack_base == stack_base
            && top > frame.slot
        {
            self.stack.pop();
        }
    }

    /// The watched bytes, for comparing around a service or a device event; `None` when there are
    /// too many.
    pub(crate) fn bytes(&self, peek: impl Fn(u32) -> u8) -> Option<Vec<u8>> {
        let size: u32 = self.ranges.iter().map(|r| r.end - r.start).sum();
        (size <= MAX_COMPARED).then(|| self.ranges.iter().flat_map(|r| r.clone()).map(peek).collect())
    }

    /// Records what changed between `before` and `after` (from [`Watch::bytes`]) as writes by `by`.
    pub(crate) fn changed(&mut self, cycle: u64, before: &[u8], after: &[u8], by: By) {
        let addresses: Vec<u32> = self.ranges.iter().flat_map(|r| r.clone()).collect();
        for ((&address, &old), &value) in addresses.iter().zip(before).zip(after) {
            if old != value {
                self.record(cycle, address, Kind::Write, value, old, by);
            }
        }
    }
}

/// Linear address of the top of the stack.
fn stack_top(cpu: &Cpu) -> u32 {
    let sp = if cpu.stack32() { cpu.regs.gpr[4] } else { cpu.regs.gpr[4] & 0xFFFF };
    cpu.segment_base(seg::SS).wrapping_add(sp)
}

/// The bus while a watch is on.
pub(crate) struct WatchBus<'a, 'b> {
    pub(crate) inner: PcBus<'a>,
    pub(crate) watch: &'b mut Watch,
    pub(crate) now: u64,
}

impl Bus for WatchBus<'_, '_> {
    fn read_u8(&mut self, addr: u32) -> u8 {
        let value = self.inner.read_u8(addr);
        if self.watch.reads && self.watch.watched(addr) {
            let by = self.watch.by_instruction();
            self.watch.record(self.now, addr, Kind::Read, value, value, by);
        }
        value
    }

    fn write_u8(&mut self, addr: u32, value: u8) {
        if self.watch.watched(addr) {
            let old = self.inner.peek_u8(addr);
            let by = self.watch.by_instruction();
            self.watch.record(self.now, addr, Kind::Write, value, old, by);
        }
        self.inner.write_u8(addr, value);
    }

    fn fetch_u8(&mut self, addr: u32) -> u8 {
        self.inner.read_u8(addr)
    }

    fn code_window(&mut self, addr: u32) -> Option<[u8; 16]> {
        self.inner.code_window(addr)
    }

    fn port_in_u8(&mut self, port: u16) -> u8 {
        self.inner.port_in_u8(port)
    }

    fn port_out_u8(&mut self, port: u16, value: u8) {
        self.inner.port_out_u8(port, value);
    }
}
