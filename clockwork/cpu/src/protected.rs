//! Protected mode: descriptors, segment loads, far transfers through call gates, interrupts and
//! exceptions through the IDT, privilege-level stack switches, task switches, virtual-8086 mode
//! entry and exit, and the system instructions.
//!
//! Ported from DOSBox Staging `src/cpu/cpu.cpp` (GPL-2.0-or-later): `CPU_Interrupt`,
//! `CPU_IRET`, `CPU_JMP`, `CPU_CALL`, `CPU_RET`, `cpu_switch_task`, `CPU_SetSegGeneral`,
//! `CPU_LAR`/`LSL`/`VERR`/`VERW`/`ARPL`, `CPU_LTR`/`LLDT`, `CPU_SET_CRX` and the I/O permission
//! check. Where DOSBox stops the emulator on a state it doesn't handle (`E_Exit`), this stops
//! the run with [`Exit::Unsupported`]. Differences: JMP through a call gate is supported, and
//! 16-bit (80286) TSSs aren't (as in DOSBox).

use crate::exec::Width;
use crate::flags::{self, IF, NT, TF};
use crate::regs::reg::SP;
use crate::regs::seg::{CS, DS, ES, FS, GS, SS};
use crate::{Abort, Bus, Cpu, Exit, Fault, Res, SegmentCache, SystemSegment, cr0, vector};

/// How an interrupt was raised: software interrupts check the gate's privilege, and others set
/// the EXT bit in error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum IntKind {
    /// INT n: checks the gate's DPL, and IOPL in V86 mode.
    Software,
    /// INT3 and INTO: like INT n, but no IOPL check in V86 mode.
    Breakpoint,
    External,
    Exception,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum TaskSwitch {
    Jmp,
    CallOrInt,
    Iret,
}

/// Descriptor types (the 5-bit type field including the S bit).
mod kind {
    pub const TSS_286_AVAILABLE: u8 = 0x01;
    pub const LDT: u8 = 0x02;
    pub const TSS_286_BUSY: u8 = 0x03;
    pub const CALL_GATE_286: u8 = 0x04;
    pub const TASK_GATE: u8 = 0x05;
    pub const INT_GATE_286: u8 = 0x06;
    pub const TRAP_GATE_286: u8 = 0x07;
    pub const TSS_386_AVAILABLE: u8 = 0x09;
    pub const TSS_386_BUSY: u8 = 0x0B;
    pub const CALL_GATE_386: u8 = 0x0C;
    pub const INT_GATE_386: u8 = 0x0E;
    pub const TRAP_GATE_386: u8 = 0x0F;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Descriptor {
    low: u32,
    high: u32,
}

impl Descriptor {
    fn base(&self) -> u32 {
        (self.low >> 16) | ((self.high & 0xFF) << 16) | (self.high & 0xFF00_0000)
    }

    fn limit(&self) -> u32 {
        let limit = (self.low & 0xFFFF) | (self.high & 0x000F_0000);
        if self.high & 0x0080_0000 != 0 { (limit << 12) | 0xFFF } else { limit }
    }

    fn kind(&self) -> u8 {
        ((self.high >> 8) & 0x1F) as u8
    }

    fn dpl(&self) -> u8 {
        ((self.high >> 13) & 3) as u8
    }

    fn present(&self) -> bool {
        self.high & 0x8000 != 0
    }

    fn big(&self) -> bool {
        self.high & 0x0040_0000 != 0
    }

    fn is_code(&self) -> bool {
        self.kind() & 0x18 == 0x18
    }

    fn is_data(&self) -> bool {
        self.kind() & 0x18 == 0x10
    }

    fn conforming(&self) -> bool {
        self.is_code() && self.kind() & 4 != 0
    }

    fn readable(&self) -> bool {
        self.is_data() || (self.is_code() && self.kind() & 2 != 0)
    }

    fn writable_data(&self) -> bool {
        self.is_data() && self.kind() & 2 != 0
    }

    fn gate_selector(&self) -> u16 {
        (self.low >> 16) as u16
    }

    fn gate_offset(&self) -> u32 {
        (self.low & 0xFFFF) | (self.high & 0xFFFF_0000)
    }

    fn param_count(&self) -> u32 {
        self.high & 0x1F
    }

    fn cache(&self, selector: u16) -> SegmentCache {
        SegmentCache { selector, base: self.base(), limit: self.limit(), big: self.big() }
    }
}

impl Cpu {
    /// For debuggers: the base address and default size (true: 32-bit) of `selector`'s segment
    /// as code would use it now, read without side effects. Real and V86 mode: selector × 16;
    /// protected mode: a loaded segment register's cache, or the GDT or LDT entry, read through
    /// `read_linear`. `None` for the null selector or one beyond its table.
    pub fn peek_segment(&self, selector: u16, read_linear: impl Fn(u32) -> Option<u8>) -> Option<(u32, bool)> {
        if self.real_addressing() {
            return Some((u32::from(selector) << 4, false));
        }
        if selector & !3 == 0 {
            return None;
        }
        if let Some(cache) = self.segs.iter().zip(self.regs.seg).find(|(c, s)| c.selector == selector && *s == selector)
        {
            return Some((cache.0.base, cache.0.big));
        }
        let (base, limit) = if selector & 4 != 0 {
            (self.ldtr.base, self.ldtr.limit)
        } else {
            (self.gdtr.base, u32::from(self.gdtr.limit))
        };
        let index = u32::from(selector & !7);
        if index + 7 > limit {
            return None;
        }
        let byte = |i: u32| read_linear(base.wrapping_add(index + i)).map(u32::from);
        let word = |at: u32| -> Option<u32> {
            Some(byte(at)? | byte(at + 1)? << 8 | byte(at + 2)? << 16 | byte(at + 3)? << 24)
        };
        let d = Descriptor { low: word(0)?, high: word(4)? };
        Some((d.base(), d.big()))
    }
}

fn fault(vector: u8, code: u32) -> Abort {
    Abort::Fault(Fault::with_code(vector, code))
}

fn gp(code: u32) -> Abort {
    fault(vector::GENERAL_PROTECTION, code)
}

impl Cpu {
    fn unsupported_state(&self, feature: &'static str) -> Abort {
        Exit::Unsupported { cs: self.regs.seg[CS], ip: self.regs.ip, feature }.into()
    }

    // ---- system memory (descriptor tables, TSS): supervisor accesses ----

    pub(crate) fn read_system<B: Bus>(&mut self, bus: &mut B, linear: u32, w: Width) -> Res<u32> {
        let mut value = 0;
        for i in 0..w.bytes() {
            let addr = linear.wrapping_add(i);
            let physical = if self.paging() { self.translate(bus, addr, false, false)? } else { addr };
            value |= u32::from(bus.read_u8(physical)) << (8 * i);
        }
        Ok(value)
    }

    pub(crate) fn write_system<B: Bus>(&mut self, bus: &mut B, linear: u32, w: Width, value: u32) -> Res<()> {
        for i in 0..w.bytes() {
            let addr = linear.wrapping_add(i);
            let physical = if self.paging() { self.translate(bus, addr, true, false)? } else { addr };
            self.wrote(physical);
            bus.write_u8(physical, (value >> (8 * i)) as u8);
        }
        Ok(())
    }

    /// The descriptor `selector` names in the GDT or LDT, or `None` beyond the table's limit.
    fn descriptor<B: Bus>(&mut self, bus: &mut B, selector: u16) -> Res<Option<Descriptor>> {
        let index = u32::from(selector & !7);
        let (base, limit) = if selector & 4 != 0 {
            (self.ldtr.base, self.ldtr.limit)
        } else {
            (self.gdtr.base, u32::from(self.gdtr.limit))
        };
        if index + 7 > limit {
            return Ok(None);
        }
        let low = self.read_system(bus, base + index, Width::Dword)?;
        let high = self.read_system(bus, base + index + 4, Width::Dword)?;
        Ok(Some(Descriptor { low, high }))
    }

    /// Rewrites a descriptor's type field (TSS busy bit).
    fn set_descriptor_kind<B: Bus>(&mut self, bus: &mut B, selector: u16, kind: u8) -> Res<()> {
        let base = if selector & 4 != 0 { self.ldtr.base } else { self.gdtr.base };
        let at = base + u32::from(selector & !7) + 4;
        let high = self.read_system(bus, at, Width::Dword)?;
        self.write_system(bus, at, Width::Dword, (high & !0x1F00) | (u32::from(kind) << 8))
    }

    // ---- segment registers ----

    /// Real-mode (and V86) segment load: base = selector × 16.
    pub(crate) fn load_segment_real(&mut self, seg: usize, selector: u16) {
        self.regs.seg[seg] = selector;
        let cache = &mut self.segs[seg];
        cache.selector = selector;
        cache.base = u32::from(selector) << 4;
        if self.regs.flags & flags::VM != 0 {
            cache.limit = 0xFFFF;
            cache.big = false;
        }
    }

    /// MOV, POP, LDS etc. into a data or stack segment register (DOSBox `CPU_SetSegGeneral`).
    pub(crate) fn load_segment<B: Bus>(&mut self, bus: &mut B, seg: usize, selector: u16) -> Res<()> {
        if self.real_addressing() {
            self.load_segment_real(seg, selector);
            return Ok(());
        }
        let code = u32::from(selector & 0xFFFC);
        let rpl = (selector & 3) as u8;
        if seg == SS {
            if selector & 0xFFFC == 0 {
                return Err(gp(0));
            }
            let d = self.descriptor(bus, selector)?.ok_or(gp(code))?;
            if rpl != self.cpl || d.dpl() != self.cpl || !d.writable_data() {
                return Err(gp(code));
            }
            if !d.present() {
                return Err(fault(vector::STACK, code));
            }
            self.regs.seg[SS] = selector;
            self.segs[SS] = d.cache(selector);
            return Ok(());
        }
        if selector & 0xFFFC == 0 {
            self.regs.seg[seg] = selector;
            self.segs[seg] = SegmentCache { selector, base: 0, limit: 0, big: false };
            return Ok(());
        }
        let d = self.descriptor(bus, selector)?.ok_or(gp(code))?;
        if !d.readable() {
            return Err(gp(code));
        }
        if !d.conforming() && (rpl > d.dpl() || self.cpl > d.dpl()) {
            return Err(gp(code));
        }
        if !d.present() {
            return Err(fault(vector::NOT_PRESENT, code));
        }
        self.regs.seg[seg] = selector;
        self.segs[seg] = d.cache(selector);
        Ok(())
    }

    fn set_cs(&mut self, selector: u16, d: &Descriptor) {
        self.regs.seg[CS] = selector;
        self.segs[CS] = d.cache(selector);
    }

    fn set_stack(&mut self, selector: u16, d: &Descriptor, esp: u32) {
        self.regs.seg[SS] = selector;
        self.segs[SS] = d.cache(selector);
        if d.big() {
            self.regs.gpr[SP] = esp;
        } else {
            self.regs.set_sp(esp as u16);
        }
    }

    /// After a return to an outer level: data segments the new privilege may not use become null.
    fn check_segments<B: Bus>(&mut self, bus: &mut B) -> Res<()> {
        for seg in [ES, DS, FS, GS] {
            let selector = self.regs.seg[seg];
            let invalid = match self.descriptor(bus, selector)? {
                None => true,
                Some(d) => (d.is_data() || (d.is_code() && !d.conforming())) && self.cpl > d.dpl(),
            };
            if invalid {
                self.regs.seg[seg] = 0;
                self.segs[seg] = SegmentCache { selector: 0, base: 0, limit: 0, big: false };
            }
        }
        Ok(())
    }

    /// A word at offset `index` × `w` above the stack pointer, without popping.
    fn stack_peek<B: Bus>(&mut self, bus: &mut B, index: u32, w: Width) -> Res<u32> {
        let mask = if self.stack32() { u32::MAX } else { 0xFFFF };
        let off = self.regs.gpr[SP].wrapping_add(index * w.bytes()) & mask;
        self.read_mem(bus, SS, off, mask, w)
    }

    pub(crate) fn release_stack(&mut self, bytes: u32) {
        if self.stack32() {
            self.regs.gpr[SP] = self.regs.gpr[SP].wrapping_add(bytes);
        } else {
            self.regs.set_sp(self.regs.sp().wrapping_add(bytes as u16));
        }
    }

    // ---- flags ----

    /// Writes the flags in `mask` from `value` (DOSBox `CPU_SetFlags`).
    pub(crate) fn set_flags_masked(&mut self, value: u32, mask: u32) {
        self.regs.flags = (self.regs.flags & !mask) | (value & mask) | u32::from(flags::FIXED);
    }

    /// The mask POPF and IRET apply: IOPL only at CPL 0, IF only when IOPL allows.
    fn popf_mask(&self) -> u32 {
        // DOSBox `cpu_extflags_toggle`: the 486's AC flag is always writable.
        let all = if self.model >= crate::Model::I486 { flags::ALL | flags::AC } else { flags::ALL };
        if !self.protected_mode() {
            return match self.model {
                crate::Model::I286 => u32::from(flags::WRITABLE),
                _ => all,
            };
        }
        let mut mask = all;
        if self.cpl > 0 {
            mask &= !u32::from(flags::IOPL);
        }
        if !self.v86() && self.regs.iopl() < self.cpl {
            mask &= !u32::from(IF);
        }
        mask
    }

    pub(crate) fn popf<B: Bus>(&mut self, bus: &mut B, w: Width) -> Res<()> {
        if self.protected_mode() && self.v86() && self.regs.iopl() != 3 {
            return Err(gp(0));
        }
        let mask = self.popf_mask();
        let value = self.pop(bus, w)?;
        let mask = if w == Width::Word { mask & 0xFFFF } else { mask };
        self.set_flags_masked(value, mask);
        Ok(())
    }

    pub(crate) fn pushf<B: Bus>(&mut self, bus: &mut B, w: Width) -> Res<()> {
        if self.protected_mode() && self.v86() && self.regs.iopl() != 3 {
            return Err(gp(0));
        }
        // PUSHFD clears VM and RF in the pushed image.
        let value = self.regs.flags & 0x00FC_FFFF;
        self.push(bus, w, value)
    }

    /// CLI/STI privilege check.
    pub(crate) fn check_interrupt_flag_access(&self) -> Res<()> {
        let denied = self.protected_mode()
            && ((!self.v86() && self.regs.iopl() < self.cpl) || (self.v86() && self.regs.iopl() < 3));
        if denied { Err(gp(0)) } else { Ok(()) }
    }

    /// Instructions only CPL 0 may run in protected mode (LGDT, MOV CRn, HLT, ...).
    pub(crate) fn check_privileged(&self) -> Res<()> {
        if self.protected_mode() && (self.cpl > 0 || self.v86()) { Err(gp(0)) } else { Ok(()) }
    }

    /// I/O permission (DOSBox `CPU_IO_Exception`): CPL > IOPL or V86 mode consults the TSS's
    /// I/O bitmap.
    pub(crate) fn check_io<B: Bus>(&mut self, bus: &mut B, port: u16, w: Width) -> Res<()> {
        if !self.protected_mode() || (self.regs.iopl() >= self.cpl && !self.v86()) {
            return Ok(());
        }
        if !self.tr.is386 {
            return Err(gp(0));
        }
        let map = self.read_system(bus, self.tr.base + 0x66, Width::Word)?;
        if map > self.tr.limit {
            return Err(gp(0));
        }
        let at = self.tr.base + map + u32::from(port / 8);
        let bits = self.read_system(bus, at, Width::Word)?;
        let mask = (0xFFFF >> (16 - w.bytes())) << (port & 7);
        if bits & mask != 0 { Err(gp(0)) } else { Ok(()) }
    }

    // ---- control registers ----

    /// MOV CRn, reg (DOSBox `CPU_WRITE_CRX`/`CPU_SET_CRX`).
    pub(crate) fn write_control<B: Bus>(&mut self, bus: &mut B, index: u8, value: u32) -> Res<()> {
        self.check_privileged()?;
        match index {
            0 => self.set_cr0(bus, value),
            2 => {
                self.cr2 = value;
                Ok(())
            }
            3 => {
                self.cr3 = value;
                self.flush_tlb();
                Ok(())
            }
            _ => Err(fault(vector::INVALID_OPCODE, 0)),
        }
    }

    pub(crate) fn read_control(&self, index: u8) -> Res<u32> {
        self.check_privileged()?;
        match index {
            0 => Ok(self.cr0),
            2 => Ok(self.cr2),
            3 => Ok(self.cr3 & !0xFFF),
            _ => Err(fault(vector::INVALID_OPCODE, 0)),
        }
    }

    pub(crate) fn set_cr0<B: Bus>(&mut self, _bus: &mut B, value: u32) -> Res<()> {
        let value = value | cr0::ET;
        if self.model == crate::Model::I286 && value & cr0::PE != 0 {
            return Err(self.unsupported_state("80286 protected mode"));
        }
        let changed = self.cr0 ^ value;
        if changed & cr0::PE != 0 && value & cr0::PE != 0 {
            // Entering protected mode: the caches keep their real-mode contents.
            for seg in 0..6 {
                let selector = self.regs.seg[seg];
                if self.segs[seg].selector != selector {
                    self.segs[seg].selector = selector;
                    self.segs[seg].base = u32::from(selector) << 4;
                }
            }
            self.cpl = 0;
        }
        if changed & cr0::PE != 0 && value & cr0::PE == 0 {
            self.cpl = 0;
        }
        // WP too: the TLB entries' write permissions depend on it.
        if changed & (cr0::PG | cr0::PE | cr0::WP) != 0 {
            self.flush_tlb();
        }
        self.cr0 = value;
        Ok(())
    }

    /// LMSW: sets the low four CR0 bits but can't clear PE.
    pub(crate) fn lmsw<B: Bus>(&mut self, bus: &mut B, word: u16) -> Res<()> {
        self.check_privileged()?;
        let value = (self.cr0 & !0xF) | u32::from(word & 0xF) | (self.cr0 & cr0::PE);
        self.set_cr0(bus, value)
    }

    // ---- LDT, TR and descriptor queries ----

    pub(crate) fn lldt<B: Bus>(&mut self, bus: &mut B, selector: u16) -> Res<()> {
        self.check_privileged()?;
        if selector & 0xFFFC == 0 {
            self.ldtr = SystemSegment::default();
            return Ok(());
        }
        let code = u32::from(selector & 0xFFFC);
        let d = self.descriptor(bus, selector & !4)?.ok_or(gp(code))?;
        if selector & 4 != 0 || d.kind() != kind::LDT {
            return Err(gp(code));
        }
        if !d.present() {
            return Err(fault(vector::NOT_PRESENT, code));
        }
        self.ldtr = SystemSegment { selector, base: d.base(), limit: d.limit(), is386: false };
        Ok(())
    }

    pub(crate) fn ltr<B: Bus>(&mut self, bus: &mut B, selector: u16) -> Res<()> {
        self.check_privileged()?;
        let code = u32::from(selector & 0xFFFC);
        if selector & 0xFFFC == 0 {
            return Err(gp(0));
        }
        let d = self.descriptor(bus, selector)?.ok_or(gp(code))?;
        if selector & 4 != 0 || !matches!(d.kind(), kind::TSS_286_AVAILABLE | kind::TSS_386_AVAILABLE) {
            return Err(gp(code));
        }
        if !d.present() {
            return Err(fault(vector::NOT_PRESENT, code));
        }
        self.tr = SystemSegment { selector, base: d.base(), limit: d.limit(), is386: d.kind() & 8 != 0 };
        self.set_descriptor_kind(bus, selector, d.kind() | 2)
    }

    /// LAR and LSL: the descriptor's access rights or limit if it's visible at CPL/RPL.
    pub(crate) fn lar_lsl<B: Bus>(&mut self, bus: &mut B, selector: u16, lsl: bool) -> Res<Option<u32>> {
        if selector & 0xFFFC == 0 {
            return Ok(None);
        }
        let Some(d) = self.descriptor(bus, selector)? else { return Ok(None) };
        let rpl = (selector & 3) as u8;
        let valid_kind = match d.kind() {
            kind::TSS_286_AVAILABLE | kind::LDT | kind::TSS_286_BUSY | kind::TSS_386_AVAILABLE | kind::TSS_386_BUSY => {
                true
            }
            kind::CALL_GATE_286 | kind::TASK_GATE | kind::CALL_GATE_386 => !lsl,
            k => k & 0x10 != 0,
        };
        if !valid_kind || (!d.conforming() && (d.dpl() < self.cpl || d.dpl() < rpl)) {
            return Ok(None);
        }
        Ok(Some(if lsl { d.limit() } else { d.high & 0x00FF_FF00 }))
    }

    /// VERR (`write` false) and VERW.
    pub(crate) fn verify<B: Bus>(&mut self, bus: &mut B, selector: u16, write: bool) -> Res<bool> {
        if selector & 0xFFFC == 0 {
            return Ok(false);
        }
        let Some(d) = self.descriptor(bus, selector)? else { return Ok(false) };
        let rpl = (selector & 3) as u8;
        let usable = if write { d.writable_data() } else { d.readable() };
        if !usable {
            return Ok(false);
        }
        Ok(d.conforming() && !write || (d.dpl() >= self.cpl && d.dpl() >= rpl))
    }

    // ---- far transfers ----

    /// Far JMP (DOSBox `CPU_JMP`).
    pub(crate) fn far_jump<B: Bus>(&mut self, bus: &mut B, w: Width, selector: u16, offset: u32) -> Res<()> {
        if self.real_addressing() {
            self.load_segment_real(CS, selector);
            self.regs.ip = if w == Width::Dword { offset } else { offset & 0xFFFF };
            return Ok(());
        }
        let code = u32::from(selector & 0xFFFC);
        if selector & 0xFFFC == 0 {
            return Err(gp(0));
        }
        let rpl = (selector & 3) as u8;
        let d = self.descriptor(bus, selector)?.ok_or(gp(code))?;
        match d.kind() {
            k if k & 0x18 == 0x18 => {
                if d.conforming() {
                    if self.cpl < d.dpl() {
                        return Err(gp(code));
                    }
                } else if rpl > self.cpl || self.cpl != d.dpl() {
                    return Err(gp(code));
                }
                if !d.present() {
                    return Err(fault(vector::NOT_PRESENT, code));
                }
                self.set_cs((selector & 0xFFFC) | u16::from(self.cpl), &d);
                self.regs.ip = if w == Width::Dword { offset } else { offset & 0xFFFF };
                Ok(())
            }
            kind::CALL_GATE_286 | kind::CALL_GATE_386 => {
                if d.dpl() < self.cpl || d.dpl() < rpl {
                    return Err(gp(code));
                }
                if !d.present() {
                    return Err(fault(vector::NOT_PRESENT, code));
                }
                let target = d.gate_selector();
                let target_code = u32::from(target & 0xFFFC);
                let cs = self.descriptor(bus, target)?.ok_or(gp(target_code))?;
                let allowed = cs.is_code() && if cs.conforming() { cs.dpl() <= self.cpl } else { cs.dpl() == self.cpl };
                if !allowed {
                    return Err(gp(target_code));
                }
                if !cs.present() {
                    return Err(fault(vector::NOT_PRESENT, target_code));
                }
                self.set_cs((target & 0xFFFC) | u16::from(self.cpl), &cs);
                let eip = d.gate_offset();
                self.regs.ip = if d.kind() == kind::CALL_GATE_386 { eip } else { eip & 0xFFFF };
                Ok(())
            }
            kind::TSS_386_AVAILABLE | kind::TSS_286_AVAILABLE => {
                if d.dpl() < self.cpl || d.dpl() < rpl {
                    return Err(gp(code));
                }
                let next_ip = self.regs.ip;
                self.switch_task(bus, selector, TaskSwitch::Jmp, next_ip)
            }
            kind::TASK_GATE => {
                if d.dpl() < self.cpl || d.dpl() < rpl {
                    return Err(gp(code));
                }
                if !d.present() {
                    return Err(fault(vector::NOT_PRESENT, code));
                }
                let next_ip = self.regs.ip;
                self.switch_task(bus, d.gate_selector(), TaskSwitch::Jmp, next_ip)
            }
            _ => Err(gp(code)),
        }
    }

    /// Far CALL (DOSBox `CPU_CALL`).
    pub(crate) fn far_call<B: Bus>(&mut self, bus: &mut B, w: Width, selector: u16, offset: u32) -> Res<()> {
        let return_ip = self.regs.ip;
        if self.real_addressing() {
            let cs = u32::from(self.regs.seg[CS]);
            self.push(bus, w, cs)?;
            self.push(bus, w, return_ip)?;
            self.load_segment_real(CS, selector);
            self.regs.ip = if w == Width::Dword { offset } else { offset & 0xFFFF };
            return Ok(());
        }
        let code = u32::from(selector & 0xFFFC);
        if selector & 0xFFFC == 0 {
            return Err(gp(0));
        }
        let rpl = (selector & 3) as u8;
        let d = self.descriptor(bus, selector)?.ok_or(gp(code))?;
        match d.kind() {
            k if k & 0x18 == 0x18 => {
                if d.conforming() {
                    if d.dpl() > self.cpl {
                        return Err(gp(code));
                    }
                } else if rpl > self.cpl || d.dpl() != self.cpl {
                    return Err(gp(code));
                }
                if !d.present() {
                    return Err(fault(vector::NOT_PRESENT, code));
                }
                let cs = u32::from(self.regs.seg[CS]);
                self.push(bus, w, cs)?;
                self.push(bus, w, return_ip)?;
                self.set_cs((selector & 0xFFFC) | u16::from(self.cpl), &d);
                self.regs.ip = if w == Width::Dword { offset } else { offset & 0xFFFF };
                Ok(())
            }
            kind::CALL_GATE_286 | kind::CALL_GATE_386 => self.call_gate(bus, w, &d, selector, return_ip),
            kind::TSS_386_AVAILABLE | kind::TSS_286_AVAILABLE => {
                if d.dpl() < self.cpl || d.dpl() < rpl {
                    return Err(gp(code));
                }
                if !d.present() {
                    return Err(fault(vector::NOT_PRESENT, code));
                }
                self.switch_task(bus, selector, TaskSwitch::CallOrInt, return_ip)
            }
            kind::TASK_GATE => {
                if d.dpl() < self.cpl || d.dpl() < rpl {
                    return Err(gp(code));
                }
                if !d.present() {
                    return Err(fault(vector::NOT_PRESENT, code));
                }
                self.switch_task(bus, d.gate_selector(), TaskSwitch::CallOrInt, return_ip)
            }
            _ => Err(gp(code)),
        }
    }

    fn call_gate<B: Bus>(
        &mut self,
        bus: &mut B,
        w: Width,
        gate: &Descriptor,
        selector: u16,
        return_ip: u32,
    ) -> Res<()> {
        let code = u32::from(selector & 0xFFFC);
        let rpl = (selector & 3) as u8;
        if gate.dpl() < self.cpl || gate.dpl() < rpl {
            return Err(gp(code));
        }
        if !gate.present() {
            return Err(fault(vector::NOT_PRESENT, code));
        }
        let target = gate.gate_selector();
        let target_code = u32::from(target & 0xFFFC);
        if target & 0xFFFC == 0 {
            return Err(gp(0));
        }
        let cs = self.descriptor(bus, target)?.ok_or(gp(target_code))?;
        if !cs.is_code() || cs.dpl() > self.cpl {
            return Err(gp(target_code));
        }
        if !cs.present() {
            return Err(fault(vector::NOT_PRESENT, target_code));
        }
        let gate32 = gate.kind() == kind::CALL_GATE_386;
        let frame = if gate32 { Width::Dword } else { Width::Word };
        let new_eip = if gate32 { gate.gate_offset() } else { gate.gate_offset() & 0xFFFF };
        let old_cs = u32::from(self.regs.seg[CS]);
        let _ = w;
        if !cs.conforming() && cs.dpl() < self.cpl {
            // Inner privilege: new stack from the TSS, parameters copied across.
            let dpl = cs.dpl();
            let (ss_sel, esp) = self.tss_stack(bus, dpl)?;
            let ss_code = u32::from(ss_sel & 0xFFFC);
            if ss_sel & 0xFFFC == 0 {
                return Err(fault(vector::INVALID_TSS, 0));
            }
            let ss = self.descriptor(bus, ss_sel)?.ok_or(fault(vector::INVALID_TSS, ss_code))?;
            if (ss_sel & 3) as u8 != dpl || ss.dpl() != dpl || !ss.writable_data() {
                return Err(fault(vector::INVALID_TSS, ss_code));
            }
            if !ss.present() {
                return Err(fault(vector::STACK, ss_code));
            }
            let (old_ss, old_esp) = (u32::from(self.regs.seg[SS]), self.regs.gpr[SP]);
            let count = gate.param_count();
            let params: Vec<u32> = (0..count).map(|i| self.stack_peek(bus, i, frame)).collect::<Res<Vec<u32>>>()?;
            self.set_stack(ss_sel, &ss, esp);
            self.cpl = dpl;
            self.set_cs((target & 0xFFFC) | u16::from(dpl), &cs);
            self.regs.ip = new_eip;
            self.push(bus, frame, old_ss)?;
            self.push(bus, frame, old_esp)?;
            for &param in params.iter().rev() {
                self.push(bus, frame, param)?;
            }
            self.push(bus, frame, old_cs)?;
            self.push(bus, frame, return_ip)?;
            return Ok(());
        }
        if !cs.conforming() && cs.dpl() > self.cpl {
            return Err(gp(target_code));
        }
        self.push(bus, frame, old_cs)?;
        self.push(bus, frame, return_ip)?;
        self.set_cs((target & 0xFFFC) | u16::from(self.cpl), &cs);
        self.regs.ip = new_eip;
        Ok(())
    }

    /// Far RET (DOSBox `CPU_RET`), releasing `bytes` of parameters.
    pub(crate) fn far_return<B: Bus>(&mut self, bus: &mut B, w: Width, bytes: u32) -> Res<()> {
        if self.real_addressing() {
            let ip = self.pop(bus, w)?;
            let cs = self.pop(bus, w)? as u16;
            self.release_stack(bytes);
            self.load_segment_real(CS, cs);
            self.regs.ip = ip;
            return Ok(());
        }
        let selector = self.stack_peek(bus, 1, w)? as u16;
        let code = u32::from(selector & 0xFFFC);
        let rpl = (selector & 3) as u8;
        if rpl < self.cpl {
            return Err(gp(code));
        }
        if selector & 0xFFFC == 0 {
            return Err(gp(0));
        }
        let d = self.descriptor(bus, selector)?.ok_or(gp(code))?;
        if !d.is_code() {
            return Err(gp(code));
        }
        if if d.conforming() { d.dpl() > rpl } else { d.dpl() != rpl } {
            return Err(gp(code));
        }
        if !d.present() {
            return Err(fault(vector::NOT_PRESENT, code));
        }
        if rpl == self.cpl {
            let ip = self.pop(bus, w)?;
            let cs = self.pop(bus, w)? as u16;
            self.set_cs(cs, &d);
            self.regs.ip = ip;
            self.release_stack(bytes);
            return Ok(());
        }
        // Return to an outer level: the caller's SS:ESP follow the parameters.
        let ip = self.pop(bus, w)?;
        let cs = self.pop(bus, w)? as u16;
        self.release_stack(bytes);
        let esp = self.pop(bus, w)?;
        let ss_sel = self.pop(bus, w)? as u16;
        let ss_code = u32::from(ss_sel & 0xFFFC);
        if ss_sel & 0xFFFC == 0 {
            return Err(gp(0));
        }
        let ss = self.descriptor(bus, ss_sel)?.ok_or(gp(ss_code))?;
        if (ss_sel & 3) as u8 != rpl || ss.dpl() != rpl || !ss.writable_data() {
            return Err(gp(ss_code));
        }
        if !ss.present() {
            return Err(fault(vector::STACK, ss_code));
        }
        self.cpl = rpl;
        self.set_cs((cs & 0xFFFC) | u16::from(rpl), &d);
        self.regs.ip = ip;
        self.set_stack(ss_sel, &ss, esp.wrapping_add(bytes));
        self.check_segments(bus)
    }

    // ---- interrupts ----

    /// Enters interrupt `num` (DOSBox `CPU_Interrupt`): through the IVT in real mode, through an
    /// interrupt, trap or task gate in protected mode, switching stacks when the handler runs at
    /// a more privileged level (and leaving virtual-8086 mode).
    pub(crate) fn deliver<B: Bus>(&mut self, bus: &mut B, num: u8, how: IntKind, error: Option<u32>) -> Res<()> {
        let return_ip = self.regs.ip;
        if !self.protected_mode() {
            // The vector is read before the frame is pushed: it matters when the stack overlaps
            // the vector table (SingleStepTests).
            let entry = self.idtr.base.wrapping_add(u32::from(num) * 4);
            let ip = self.read_system(bus, entry, Width::Word)?;
            let new_cs = self.read_system(bus, entry + 2, Width::Word)? as u16;
            let flags = self.regs.flags & 0xFFFF;
            self.push(bus, Width::Word, flags)?;
            let cs = u32::from(self.regs.seg[CS]);
            self.push(bus, Width::Word, cs)?;
            self.push(bus, Width::Word, return_ip)?;
            self.regs.flags &= !u32::from(IF | TF);
            let cs = new_cs;
            self.load_segment_real(CS, cs);
            self.regs.ip = ip;
            return Ok(());
        }
        let software = matches!(how, IntKind::Software | IntKind::Breakpoint);
        let ext = u32::from(!software);
        let idt_code = u32::from(num) * 8 + 2 + ext;
        if self.v86() && how == IntKind::Software && self.regs.iopl() != 3 {
            return Err(gp(0));
        }
        if u32::from(num) * 8 + 7 > u32::from(self.idtr.limit) {
            return Err(gp(idt_code));
        }
        let at = self.idtr.base + u32::from(num) * 8;
        let gate = Descriptor {
            low: self.read_system(bus, at, Width::Dword)?,
            high: self.read_system(bus, at + 4, Width::Dword)?,
        };
        if software && gate.dpl() < self.cpl {
            return Err(gp(u32::from(num) * 8 + 2));
        }
        match gate.kind() {
            kind::INT_GATE_286 | kind::TRAP_GATE_286 | kind::INT_GATE_386 | kind::TRAP_GATE_386 => {
                if !gate.present() {
                    return Err(fault(vector::NOT_PRESENT, idt_code));
                }
                let selector = gate.gate_selector();
                let sel_code = u32::from(selector & 0xFFFC) + ext;
                if selector & 0xFFFC == 0 {
                    return Err(gp(ext));
                }
                let cs = self.descriptor(bus, selector)?.ok_or(gp(sel_code))?;
                if !cs.is_code() || cs.dpl() > self.cpl {
                    return Err(gp(sel_code));
                }
                if !cs.present() {
                    return Err(fault(vector::NOT_PRESENT, sel_code));
                }
                let gate32 = gate.kind() & 8 != 0;
                let frame = if gate32 { Width::Dword } else { Width::Word };
                let old_flags = self.regs.flags;
                let old_cs = u32::from(self.regs.seg[CS]);
                let was_v86 = self.v86();
                if !cs.conforming() && cs.dpl() < self.cpl {
                    // Inner level: the handler's stack comes from the TSS.
                    let dpl = cs.dpl();
                    if was_v86 && dpl != 0 {
                        return Err(gp(u32::from(selector & 0xFFFC)));
                    }
                    let (ss_sel, esp) = self.tss_stack(bus, dpl)?;
                    let ss_code = u32::from(ss_sel & 0xFFFC) + ext;
                    if ss_sel & 0xFFFC == 0 {
                        return Err(fault(vector::INVALID_TSS, ext));
                    }
                    let ss = self.descriptor(bus, ss_sel)?.ok_or(fault(vector::INVALID_TSS, ss_code))?;
                    if (ss_sel & 3) as u8 != dpl || ss.dpl() != dpl || !ss.writable_data() {
                        return Err(fault(vector::INVALID_TSS, ss_code));
                    }
                    if !ss.present() {
                        return Err(fault(vector::STACK, ss_code));
                    }
                    let (old_ss, old_esp) = (u32::from(self.regs.seg[SS]), self.regs.gpr[SP]);
                    self.regs.flags &= !flags::VM;
                    self.set_stack(ss_sel, &ss, esp);
                    self.cpl = dpl;
                    if was_v86 {
                        if !gate32 {
                            return Err(self.unsupported_state("V86 interrupt through a 16-bit gate"));
                        }
                        for seg in [GS, FS, DS, ES] {
                            let value = u32::from(self.regs.seg[seg]);
                            self.push(bus, Width::Dword, value)?;
                            self.regs.seg[seg] = 0;
                            self.segs[seg] = SegmentCache { selector: 0, base: 0, limit: 0, big: false };
                        }
                    }
                    self.push(bus, frame, old_ss)?;
                    self.push(bus, frame, old_esp)?;
                } else {
                    if was_v86 {
                        return Err(gp(u32::from(selector & 0xFFFC)));
                    }
                    if !cs.conforming() && cs.dpl() != self.cpl {
                        return Err(gp(sel_code));
                    }
                }
                self.push(bus, frame, old_flags)?;
                self.push(bus, frame, old_cs)?;
                self.push(bus, frame, return_ip)?;
                if let Some(code) = error {
                    self.push(bus, frame, code)?;
                }
                self.set_cs((selector & 0xFFFC) | u16::from(self.cpl), &cs);
                self.regs.ip = if gate32 { gate.gate_offset() } else { gate.gate_offset() & 0xFFFF };
                if gate.kind() & 1 == 0 {
                    self.regs.flags &= !u32::from(IF);
                }
                self.regs.flags &= !(u32::from(TF | NT) | flags::VM | flags::RF);
                Ok(())
            }
            kind::TASK_GATE => {
                if !gate.present() {
                    return Err(fault(vector::NOT_PRESENT, idt_code));
                }
                self.switch_task(bus, gate.gate_selector(), TaskSwitch::CallOrInt, return_ip)?;
                if let Some(code) = error {
                    let w = if self.tr.is386 { Width::Dword } else { Width::Word };
                    self.push(bus, w, code)?;
                }
                Ok(())
            }
            _ => Err(gp(idt_code)),
        }
    }

    /// IRET/IRETD (DOSBox `CPU_IRET`).
    pub(crate) fn iret<B: Bus>(&mut self, bus: &mut B, w: Width) -> Res<()> {
        if !self.protected_mode() {
            let ip = self.pop(bus, w)?;
            let cs = self.pop(bus, w)? as u16;
            let value = self.pop(bus, w)?;
            self.load_segment_real(CS, cs);
            self.regs.ip = ip;
            let mask = if w == Width::Word { self.popf_mask() & 0xFFFF } else { self.popf_mask() };
            self.set_flags_masked(value, mask);
            return Ok(());
        }
        if self.v86() {
            if self.regs.iopl() != 3 {
                return Err(gp(0));
            }
            let ip = self.pop(bus, w)?;
            let cs = self.pop(bus, w)? as u16;
            let value = self.pop(bus, w)?;
            self.load_segment_real(CS, cs);
            self.regs.ip = ip;
            // IOPL can't be changed from V86 mode.
            let mask = flags::NORMAL | u32::from(NT);
            self.set_flags_masked(value, if w == Width::Word { mask & 0xFFFF } else { mask });
            return Ok(());
        }
        if self.regs.flag(NT) {
            // Return from a nested task.
            let back = self.read_system(bus, self.tr.base, Width::Word)? as u16;
            let ip = self.regs.ip;
            return self.switch_task(bus, back, TaskSwitch::Iret, ip);
        }
        let ip = self.stack_peek(bus, 0, w)?;
        let selector = self.stack_peek(bus, 1, w)? as u16;
        let mut new_flags = self.stack_peek(bus, 2, w)?;
        if w == Width::Word {
            new_flags = (new_flags & 0xFFFF) | (self.regs.flags & 0xFFFF_0000);
        }
        if w == Width::Dword && new_flags & flags::VM != 0 && self.cpl == 0 {
            // Back to virtual-8086 mode: the V86 segments and stack are on the frame.
            let values: Vec<u32> = (3..9).map(|i| self.stack_peek(bus, i, Width::Dword)).collect::<Res<Vec<u32>>>()?;
            let [esp, ss, es, ds, fs, gs] = [values[0], values[1], values[2], values[3], values[4], values[5]];
            self.set_flags_masked(new_flags, flags::ALL | flags::VM);
            self.cpl = 3;
            for (seg, value) in [(SS, ss), (ES, es), (DS, ds), (FS, fs), (GS, gs), (CS, selector.into())] {
                self.load_segment_real(seg, value as u16);
            }
            self.regs.gpr[SP] = esp;
            self.regs.ip = ip & 0xFFFF;
            return Ok(());
        }
        let code = u32::from(selector & 0xFFFC);
        if selector & 0xFFFC == 0 {
            return Err(gp(0));
        }
        let rpl = (selector & 3) as u8;
        let d = self.descriptor(bus, selector)?.ok_or(gp(code))?;
        if rpl < self.cpl || !d.is_code() {
            return Err(gp(code));
        }
        if if d.conforming() { d.dpl() > rpl } else { d.dpl() != rpl } {
            return Err(gp(code));
        }
        if !d.present() {
            return Err(fault(vector::NOT_PRESENT, code));
        }
        let mut mask = if self.cpl > 0 { flags::NORMAL | u32::from(NT) } else { flags::ALL };
        if self.regs.iopl() < self.cpl {
            mask &= !u32::from(IF);
        }
        if w == Width::Word {
            mask &= 0xFFFF;
        }
        if rpl == self.cpl {
            self.release_stack(3 * w.bytes());
            self.set_cs(selector, &d);
            self.regs.ip = ip;
            self.set_flags_masked(new_flags, mask);
            return Ok(());
        }
        // Return to an outer level: the outer SS:ESP follow the frame.
        let esp = self.stack_peek(bus, 3, w)?;
        let ss_sel = self.stack_peek(bus, 4, w)? as u16;
        let ss_code = u32::from(ss_sel & 0xFFFC);
        if ss_sel & 0xFFFC == 0 {
            return Err(gp(0));
        }
        let ss = self.descriptor(bus, ss_sel)?.ok_or(gp(ss_code))?;
        if (ss_sel & 3) as u8 != rpl || ss.dpl() != rpl || !ss.writable_data() {
            return Err(gp(ss_code));
        }
        if !ss.present() {
            return Err(fault(vector::STACK, ss_code));
        }
        self.set_cs(selector, &d);
        self.set_flags_masked(new_flags, mask);
        self.cpl = rpl;
        self.regs.ip = ip;
        self.set_stack(ss_sel, &ss, esp);
        self.check_segments(bus)
    }

    // ---- tasks ----

    /// SS:ESP for privilege level `level` from the current TSS.
    fn tss_stack<B: Bus>(&mut self, bus: &mut B, level: u8) -> Res<(u16, u32)> {
        if self.tr.is386 {
            let at = self.tr.base + 4 + u32::from(level) * 8;
            let esp = self.read_system(bus, at, Width::Dword)?;
            let ss = self.read_system(bus, at + 4, Width::Word)? as u16;
            Ok((ss, esp))
        } else {
            let at = self.tr.base + 2 + u32::from(level) * 4;
            let sp = self.read_system(bus, at, Width::Word)?;
            let ss = self.read_system(bus, at + 2, Width::Word)? as u16;
            Ok((ss, sp))
        }
    }

    /// Switches to the task in TSS `selector` (DOSBox `cpu_switch_task`, 32-bit TSSs).
    fn switch_task<B: Bus>(&mut self, bus: &mut B, selector: u16, how: TaskSwitch, return_ip: u32) -> Res<()> {
        let code = u32::from(selector & 0xFFFC);
        if selector & 4 != 0 || selector & 0xFFFC == 0 {
            return Err(fault(vector::INVALID_TSS, code));
        }
        let d = self.descriptor(bus, selector)?.ok_or(fault(vector::INVALID_TSS, code))?;
        let busy = match d.kind() {
            kind::TSS_386_AVAILABLE => false,
            kind::TSS_386_BUSY => true,
            kind::TSS_286_AVAILABLE | kind::TSS_286_BUSY => return Err(self.unsupported_state("80286 task switch")),
            _ => return Err(gp(code)),
        };
        if busy != (how == TaskSwitch::Iret) {
            return Err(gp(code));
        }
        if !d.present() {
            return Err(fault(vector::NOT_PRESENT, code));
        }
        let new_base = d.base();
        let read = |cpu: &mut Cpu, bus: &mut B, offset: u32| cpu.read_system(bus, new_base + offset, Width::Dword);
        let new_cr3 = read(self, bus, 0x1C)?;
        let new_eip = read(self, bus, 0x20)?;
        let mut new_flags = read(self, bus, 0x24)?;
        let gpr: Vec<u32> = (0..8).map(|i| read(self, bus, 0x28 + 4 * i)).collect::<Res<Vec<u32>>>()?;
        let segs: Vec<u16> =
            (0..6).map(|i| read(self, bus, 0x48 + 4 * i).map(|v| v as u16)).collect::<Res<Vec<u16>>>()?;
        let new_ldt = read(self, bus, 0x60)? as u16;

        // The old task's busy bit clears on JMP and IRET; its state goes to its TSS.
        if matches!(how, TaskSwitch::Jmp | TaskSwitch::Iret) && self.tr.selector & 0xFFFC != 0 {
            self.set_descriptor_kind(bus, self.tr.selector, kind::TSS_386_AVAILABLE)?;
        }
        let mut old_flags = self.regs.flags;
        if how == TaskSwitch::Iret {
            old_flags &= !u32::from(NT);
        }
        if self.tr.selector & 0xFFFC != 0 {
            let base = self.tr.base;
            let mut values = vec![(0x20, return_ip), (0x24, old_flags)];
            values.extend((0..8).map(|i| (0x28 + 4 * i, self.regs.gpr[i as usize])));
            values.extend((0..6).map(|i| (0x48 + 4 * i, u32::from(self.regs.seg[i as usize]))));
            for (offset, value) in values {
                self.write_system(bus, base + offset, Width::Dword, value)?;
            }
        }
        if how == TaskSwitch::CallOrInt {
            let back = u32::from(self.tr.selector);
            self.write_system(bus, new_base, Width::Word, back)?;
            new_flags |= u32::from(NT);
        }
        if matches!(how, TaskSwitch::Jmp | TaskSwitch::CallOrInt) {
            self.set_descriptor_kind(bus, selector, kind::TSS_386_BUSY)?;
        }
        self.tr = SystemSegment { selector, base: new_base, limit: d.limit(), is386: true };
        self.cr3 = new_cr3;
        self.flush_tlb();
        self.regs.ip = new_eip;
        self.set_flags_masked(new_flags, flags::ALL | flags::VM);
        self.regs.gpr.copy_from_slice(&gpr);
        self.cr0 |= 0x08; // TS
        if self.v86() {
            self.cpl = 3;
            for (seg, &value) in segs.iter().enumerate() {
                self.load_segment_real(seg, value);
            }
            return Ok(());
        }
        self.lldt_unchecked(bus, new_ldt)?;
        let cs_sel = segs[CS];
        let cs_code = u32::from(cs_sel & 0xFFFC);
        let cs = self.descriptor(bus, cs_sel)?.ok_or(fault(vector::INVALID_TSS, cs_code))?;
        if !cs.is_code() {
            return Err(fault(vector::INVALID_TSS, cs_code));
        }
        if !cs.present() {
            return Err(fault(vector::NOT_PRESENT, cs_code));
        }
        self.cpl = (cs_sel & 3) as u8;
        self.set_cs(cs_sel, &cs);
        for seg in [SS, ES, DS, FS, GS] {
            self.load_segment(bus, seg, segs[seg])?;
        }
        Ok(())
    }

    fn lldt_unchecked<B: Bus>(&mut self, bus: &mut B, selector: u16) -> Res<()> {
        let saved_cpl = self.cpl;
        self.cpl = 0;
        let result = self.lldt(bus, selector);
        self.cpl = saved_cpl;
        result
    }
}
