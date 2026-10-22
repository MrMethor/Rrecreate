//! Call tracking: a shadow stack of the calls and interrupts in progress, kept from what
//! instructions do to the stack. A CALL or INT that pushes a return address just past itself and goes
//! elsewhere is a call (or a software interrupt, when flags were pushed too); a frame ends when the
//! stack rises above its return address. Hardware interrupts open frames of their own. Only the
//! opcode is decoded, so it follows hand-written assembly as well as compiled code.
//!
//! Memory watches report the chain at each access. Tracking from the start of a run gives the whole
//! chain to a watch started later ([`crate::machine::Machine::track_calls`]); a watch started
//! without it only sees the calls made since.

use cpu::{Cpu, seg};

/// Most frames on other stacks an unwind passes over to reach this stack's newest.
const PASSED: usize = 64;

/// One frame of the call chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// The instruction about to run, as [`Calls::before`] saw it.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct Context {
    cs: u16,
    ip: u32,
    code_base: u32,
    stack_base: u32,
    top: u32,
}

/// A snapshot keeps it (beside the machine's state, not in it), so tracking goes on from a loaded
/// machine with the whole chain.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Calls {
    stack: Vec<Frame>,
    context: Context,
    /// When counting: the calls made since, by the linear address they went to (an analysis's, not
    /// kept by snapshots). Interrupts aren't calls.
    #[serde(skip)]
    counts: Option<std::collections::BTreeMap<u32, u64>>,
}

impl Calls {
    /// Counts the calls made from now on, by target (`on`), or stops counting.
    pub fn count(&mut self, on: bool) {
        self.counts = on.then(|| self.counts.take().unwrap_or_default());
    }

    /// The calls counted, by target, if counting.
    pub fn counts(&self) -> Option<&std::collections::BTreeMap<u32, u64>> {
        self.counts.as_ref()
    }

    /// The call chain now, innermost first.
    pub fn chain(&self) -> Vec<Frame> {
        self.stack.iter().rev().copied().collect()
    }

    /// The entry of the innermost interrupt handler running, if one is.
    pub fn interrupt_entry(&self) -> Option<u32> {
        self.stack.iter().rev().find(|f| f.interrupt).map(|f| f.entry)
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
        let stack32 = cpu.stack32();
        let linear_ip = cpu.segment_base(seg::CS).wrapping_add(cpu.regs.ip);
        let fell = fell(c.top, top, stack32);
        if stack_base == c.stack_base && fell > 0 {
            let u16_at = |at: u32| Some(u16::from_le_bytes([peek(at)?, peek(at.wrapping_add(1))?]));
            let u32_at = |at: u32| Some(u32::from(u16_at(at)?) | u32::from(u16_at(at.wrapping_add(2))?) << 16);
            // The return address a call or interrupt pushed: just past the instruction.
            let past = |ret: u32| ret > c.ip && ret <= c.ip.wrapping_add(15);
            let pushed = match fell {
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
                // Control went elsewhere, by a CALL or INT: not a PUSH of a value that happens to look
                // like a return address (a small count, in code at the start of its segment).
                if linear_ip != from && calls_or_interrupts(&peek, c.code_base.wrapping_add(c.ip)) {
                    self.stack.push(Frame { entry: linear_ip, from, interrupt, slot: top, stack_base });
                    if let Some(counts) = self.counts.as_mut().filter(|_| !interrupt) {
                        *counts.entry(linear_ip).or_default() += 1;
                    }
                }
            }
        }
        // Back where a frame on another stack was entered from (an IRET from a handler that ran on its
        // own stack): it ended.
        while let Some(frame) = self.stack.last()
            && frame.stack_base != stack_base
            && frame.from == linear_ip
        {
            self.stack.pop();
        }
        // Just after SS changed, SP may still be the old stack's (`mov ss` then `mov sp`): the next
        // instruction unwinds.
        if stack_base == c.stack_base {
            self.unwind(stack_base, top, stack32);
        }
    }

    /// A hardware interrupt was taken: it opens a frame of its own.
    pub(crate) fn interrupt(&mut self, cpu: &Cpu, from: u32) {
        let entry = cpu.segment_base(seg::CS).wrapping_add(cpu.regs.ip);
        let stack_base = cpu.segment_base(seg::SS);
        self.stack.push(Frame { entry, from, interrupt: true, slot: stack_top(cpu), stack_base });
    }

    /// Frames whose return address the stack has risen above are gone, and every frame opened after
    /// them with them: an extender's handler may switch stacks and come back to its caller's without
    /// returning on its own, so frames on another stack above a closed one close too. The newest
    /// frame on this stack decides (older ones on it are deeper); frames on other stacks above it are
    /// passed over, at most [`PASSED`] of them.
    fn unwind(&mut self, stack_base: u32, top: u32, stack32: bool) {
        loop {
            let newest = self.stack.iter().enumerate().rev().take(PASSED).find(|(_, f)| f.stack_base == stack_base);
            match newest {
                Some((i, frame)) if fell(frame.slot, top, stack32) < 0 => self.stack.truncate(i),
                _ => break,
            }
        }
    }
}

/// How far the top of the stack went down from `from` to `to` (negative: up). A 16-bit stack counts
/// modulo 64 KB: SP 0 is its top, and a push wraps it to FFFEh.
fn fell(from: u32, to: u32, stack32: bool) -> i32 {
    let down = from.wrapping_sub(to);
    if stack32 { down as i32 } else { i32::from(down as u16 as i16) }
}

/// Whether the instruction at linear address `at` is a CALL or a software interrupt (past its
/// prefixes): the instructions that push a return address and go elsewhere.
fn calls_or_interrupts(peek: &impl Fn(u32) -> Option<u8>, mut at: u32) -> bool {
    for _ in 0..14 {
        match peek(at) {
            Some(0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 | 0x66 | 0x67 | 0xF0 | 0xF2 | 0xF3) => at = at.wrapping_add(1),
            Some(0xE8 | 0x9A | 0xCC | 0xCD | 0xCE | 0xF1) => return true,
            // CALL near or far through a register or memory: FF /2, FF /3.
            Some(0xFF) => return peek(at.wrapping_add(1)).is_some_and(|modrm| matches!(modrm >> 3 & 7, 2 | 3)),
            _ => return false,
        }
    }
    false
}

/// Linear address of the top of the stack.
fn stack_top(cpu: &Cpu) -> u32 {
    let sp = if cpu.stack32() { cpu.regs.gpr[4] } else { cpu.regs.gpr[4] & 0xFFFF };
    cpu.segment_base(seg::SS).wrapping_add(sp)
}
