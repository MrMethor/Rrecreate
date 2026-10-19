//! Call tracking: a shadow stack of the calls and interrupts in progress, kept from what
//! instructions do to the stack. A transfer of control that pushes a return address just past the
//! instruction is a call (or a software interrupt, when flags were pushed too); a frame ends when
//! the stack rises above its return address. Hardware interrupts open frames of their own. No
//! decoding is needed, so it follows hand-written assembly as well as compiled code.
//!
//! Memory watches report the chain at each access. Tracking from the start of a run gives the whole
//! chain to a watch started later ([`crate::machine::Machine::track_calls`]); a watch started
//! without it only sees the calls made since.

use cpu::{Cpu, seg};

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

/// The instruction about to run, as [`Calls::before`] saw it.
#[derive(Debug, Clone, Copy, Default)]
struct Context {
    cs: u16,
    ip: u32,
    code_base: u32,
    stack_base: u32,
    top: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Calls {
    stack: Vec<Frame>,
    context: Context,
}

impl Calls {
    /// The call chain now, innermost first.
    pub fn chain(&self) -> Vec<Frame> {
        self.stack.iter().rev().copied().collect()
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
}

/// Linear address of the top of the stack.
fn stack_top(cpu: &Cpu) -> u32 {
    let sp = if cpu.stack32() { cpu.regs.gpr[4] } else { cpu.regs.gpr[4] & 0xFFFF };
    cpu.segment_base(seg::SS).wrapping_add(sp)
}
