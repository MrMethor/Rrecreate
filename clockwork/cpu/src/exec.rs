//! Instruction decoding and execution.
//!
//! Every memory access can fault (a page fault or protection error in protected mode), so the
//! helpers return [`Res`] and instructions propagate faults with `?`; [`Cpu::step`] then undoes
//! the instruction and enters the exception handler.

use crate::flags::{AF, CF, DF, IF, OF, PF, SF, ZF};
use crate::protected::IntKind;
use crate::regs::reg::{AX, BP, BX, CX, DI, DX, SI, SP};
use crate::regs::seg::{CS, DS, ES, FS, GS, SS};
use crate::{Abort, Bus, Cpu, Exit, Fault, Model, Res, TableRegister, vector};

/// Longest run of prefixes accepted before the instruction is treated as invalid.
const MAX_PREFIXES: usize = 14;

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum Width {
    Byte,
    Word,
    Dword,
}

impl Width {
    fn mask(self) -> u32 {
        match self {
            Width::Byte => 0xFF,
            Width::Word => 0xFFFF,
            Width::Dword => 0xFFFF_FFFF,
        }
    }

    fn sign(self) -> u32 {
        match self {
            Width::Byte => 0x80,
            Width::Word => 0x8000,
            Width::Dword => 0x8000_0000,
        }
    }

    fn bits(self) -> u32 {
        match self {
            Width::Byte => 8,
            Width::Word => 16,
            Width::Dword => 32,
        }
    }

    pub(crate) fn bytes(self) -> u32 {
        self.bits() / 8
    }

    fn sign_extend(self, value: u32) -> i64 {
        match self {
            Width::Byte => i64::from(value as u8 as i8),
            Width::Word => i64::from(value as u16 as i16),
            Width::Dword => i64::from(value as i32),
        }
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum Rm {
    Reg(u8),
    /// `mask` is 0xFFFF for 16-bit addressing (offsets wrap within the segment) and all ones
    /// for 32-bit addressing.
    Mem {
        seg: usize,
        off: u32,
        mask: u32,
    },
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ModRm {
    pub(crate) reg: u8,
    pub(crate) rm: Rm,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Rep {
    None,
    /// F3: REP / REPE.
    Repe,
    /// F2: REPNE.
    Repne,
}

pub(crate) struct Prefixes {
    seg: Option<usize>,
    rep: Rep,
    /// 32-bit operands: the code segment's default, toggled by 66h.
    pub(crate) op32: bool,
    /// 32-bit addressing: the code segment's default, toggled by 67h.
    addr32: bool,
    /// A LOCK prefix (F0h).
    lock: bool,
}

impl Prefixes {
    /// The width of "word-or-doubleword" operands.
    fn v(&self) -> Width {
        if self.op32 { Width::Dword } else { Width::Word }
    }

    /// Width selected by bit 0 of most ALU/MOV opcodes.
    fn width(&self, op: u8) -> Width {
        if op & 1 == 0 { Width::Byte } else { self.v() }
    }

    fn addr_mask(&self) -> u32 {
        if self.addr32 { u32::MAX } else { 0xFFFF }
    }
}

/// #UD: an encoding the processor defines as invalid (a register where only memory is allowed,
/// an unused group member, a misplaced LOCK).
fn invalid_opcode() -> Abort {
    Abort::Fault(Fault::new(vector::INVALID_OPCODE))
}

impl Cpu {
    #[inline(always)]
    pub(crate) fn exec<B: Bus>(&mut self, bus: &mut B, start_ip: u32) -> Res<()> {
        let i386 = self.model >= Model::I386;
        let default32 = self.code32();
        self.fetch.base = self.segment_base(CS);
        self.fetch.ip_mask = if default32 { u32::MAX } else { 0xFFFF };
        self.fetch.page_tag = 0;
        let mut p = Prefixes { seg: None, rep: Rep::None, op32: default32, addr32: default32, lock: false };
        let mut prefixes = 0;
        let op = loop {
            let byte = self.fetch8(bus)?;
            match byte {
                0x26 => p.seg = Some(ES),
                0x2E => p.seg = Some(CS),
                0x36 => p.seg = Some(SS),
                0x3E => p.seg = Some(DS),
                0x64 if i386 => p.seg = Some(FS),
                0x65 if i386 => p.seg = Some(GS),
                0x66 if i386 => p.op32 = !default32,
                0x67 if i386 => p.addr32 = !default32,
                0xF0 => p.lock = true, // no effect on a single CPU, but checked below
                0xF2 => p.rep = Rep::Repne,
                0xF3 => p.rep = Rep::Repe,
                _ => break byte,
            }
            prefixes += 1;
            if prefixes > MAX_PREFIXES {
                return Err(self.unimplemented(bus, start_ip));
            }
        };
        if p.lock && i386 {
            self.check_lock(bus, op)?;
        }
        let v = p.v();

        match op {
            // ALU ops in their six standard forms: ADD OR ADC SBB AND SUB XOR CMP.
            0x00..=0x3F if op & 7 < 6 => {
                let alu_op = op >> 3;
                let w = p.width(op);
                match op & 7 {
                    0 | 1 => {
                        let m = self.modrm(bus, &p)?;
                        let a = self.read_rm(bus, m.rm, w)?;
                        let b = self.read_reg(m.reg, w);
                        let r = self.alu(alu_op, w, a, b);
                        if alu_op != 7 {
                            self.write_rm(bus, m.rm, w, r)?;
                        }
                    }
                    2 | 3 => {
                        let m = self.modrm(bus, &p)?;
                        let a = self.read_reg(m.reg, w);
                        let b = self.read_rm(bus, m.rm, w)?;
                        let r = self.alu(alu_op, w, a, b);
                        if alu_op != 7 {
                            self.write_reg(m.reg, w, r);
                        }
                    }
                    _ => {
                        let a = self.read_reg(0, w);
                        let b = self.fetch(bus, w)?;
                        let r = self.alu(alu_op, w, a, b);
                        if alu_op != 7 {
                            self.write_reg(0, w, r);
                        }
                    }
                }
            }
            0x06 | 0x0E | 0x16 | 0x1E => {
                let value = u32::from(self.regs.seg[usize::from(op >> 3)]);
                self.push(bus, v, value)?;
            }
            0x07 | 0x17 | 0x1F => {
                let value = self.pop(bus, v)? as u16;
                self.load_segment(bus, usize::from(op >> 3), value)?;
                if op == 0x17 {
                    self.irq_inhibit = true;
                }
            }
            0x0F => self.exec_0f(bus, &p, start_ip)?,
            0x27 => self.daa(),
            0x2F => self.das(),
            0x37 => self.aaa(),
            0x3F => self.aas(),
            0x40..=0x4F => {
                let r = op & 7;
                let value = self.read_reg(r, v);
                let cf = self.regs.flag(CF);
                let result = if op < 0x48 { self.add(v, value, 1, 0) } else { self.sub(v, value, 1, 0) };
                self.regs.set_flag(CF, cf);
                self.write_reg(r, v, result);
            }
            0x50..=0x57 => {
                // PUSH SP pushes the value before the push (80286 and later).
                let value = self.read_reg(op & 7, v);
                self.push(bus, v, value)?;
            }
            0x58..=0x5F => {
                let value = self.pop(bus, v)?;
                self.write_reg(op & 7, v, value);
            }
            0x60 => {
                let sp = self.read_reg(SP as u8, v);
                for r in [AX, CX, DX, BX] {
                    let value = self.read_reg(r as u8, v);
                    self.push(bus, v, value)?;
                }
                self.push(bus, v, sp)?;
                for r in [BP, SI, DI] {
                    let value = self.read_reg(r as u8, v);
                    self.push(bus, v, value)?;
                }
            }
            0x61 => {
                for r in [DI, SI, BP] {
                    let value = self.pop(bus, v)?;
                    self.write_reg(r as u8, v, value);
                }
                // The saved ESP is discarded, except that a POPAD on a 16-bit stack takes ESP's
                // upper half from it (as the 386 does; seen in SingleStepTests).
                let saved_sp = self.pop(bus, v)?;
                for r in [BX, DX, CX, AX] {
                    let value = self.pop(bus, v)?;
                    self.write_reg(r as u8, v, value);
                }
                if v == Width::Dword && !self.stack32() {
                    self.regs.gpr[SP] = (saved_sp & 0xFFFF_0000) | (self.regs.gpr[SP] & 0xFFFF);
                }
            }
            0x62 => {
                let m = self.modrm(bus, &p)?;
                let Rm::Mem { seg, off, mask } = m.rm else {
                    return Err(invalid_opcode());
                };
                let index = v.sign_extend(self.read_reg(m.reg, v));
                let lower = v.sign_extend(self.read_mem(bus, seg, off, mask, v)?);
                let upper = v.sign_extend(self.read_mem(bus, seg, off.wrapping_add(v.bytes()), mask, v)?);
                if index < lower || index > upper {
                    return Err(Abort::Fault(Fault::new(vector::BOUND)));
                }
            }
            0x63 if i386 && self.protected_mode() && !self.v86() => {
                // ARPL: raise the destination selector's RPL to the source's.
                let m = self.modrm(bus, &p)?;
                let dest = self.read_rm(bus, m.rm, Width::Word)?;
                let src = self.read_reg(m.reg, Width::Word);
                if dest & 3 < src & 3 {
                    self.write_rm(bus, m.rm, Width::Word, (dest & !3) | (src & 3))?;
                    self.regs.set_flag(ZF, true);
                } else {
                    self.regs.set_flag(ZF, false);
                }
            }
            0x68 => {
                let value = self.fetch(bus, v)?;
                self.push(bus, v, value)?;
            }
            0x6A => {
                let value = self.fetch8(bus)? as i8 as i32 as u32 & v.mask();
                self.push(bus, v, value)?;
            }
            0x69 | 0x6B => {
                let m = self.modrm(bus, &p)?;
                let a = v.sign_extend(self.read_rm(bus, m.rm, v)?);
                let b =
                    if op == 0x69 { v.sign_extend(self.fetch(bus, v)?) } else { i64::from(self.fetch8(bus)? as i8) };
                self.imul_into(m.reg, v, a, b);
            }
            0x6C..=0x6F | 0xA4..=0xA7 | 0xAA..=0xAF => self.string_op(bus, op, &p)?,
            0x70..=0x7F => {
                let rel = self.fetch8(bus)? as i8 as u32;
                if self.condition(op & 0x0F) {
                    self.jump_relative(rel, p.op32);
                }
            }
            0x80..=0x83 => {
                let w = p.width(op);
                let m = self.modrm(bus, &p)?;
                let a = self.read_rm(bus, m.rm, w)?;
                let b = if op == 0x83 { self.fetch8(bus)? as i8 as i32 as u32 & w.mask() } else { self.fetch(bus, w)? };
                let r = self.alu(m.reg, w, a, b);
                if m.reg != 7 {
                    self.write_rm(bus, m.rm, w, r)?;
                }
            }
            0x84 | 0x85 => {
                let w = p.width(op);
                let m = self.modrm(bus, &p)?;
                let a = self.read_rm(bus, m.rm, w)?;
                let b = self.read_reg(m.reg, w);
                self.logic(w, a & b);
            }
            0x86 | 0x87 => {
                let w = p.width(op);
                let m = self.modrm(bus, &p)?;
                let a = self.read_rm(bus, m.rm, w)?;
                let b = self.read_reg(m.reg, w);
                self.write_rm(bus, m.rm, w, b)?;
                self.write_reg(m.reg, w, a);
            }
            0x88 | 0x89 => {
                let w = p.width(op);
                let m = self.modrm(bus, &p)?;
                let value = self.read_reg(m.reg, w);
                self.write_rm(bus, m.rm, w, value)?;
            }
            0x8A | 0x8B => {
                let w = p.width(op);
                let m = self.modrm(bus, &p)?;
                let value = self.read_rm(bus, m.rm, w)?;
                self.write_reg(m.reg, w, value);
            }
            0x8C => {
                let m = self.modrm(bus, &p)?;
                if usize::from(m.reg) >= self.segment_count() {
                    return Err(invalid_opcode());
                }
                let value = u32::from(self.regs.seg[usize::from(m.reg)]);
                // A register destination takes the operand size; memory is always 16 bits.
                let w = if matches!(m.rm, Rm::Reg(_)) { v } else { Width::Word };
                self.write_rm(bus, m.rm, w, value)?;
            }
            0x8D => {
                let m = self.modrm(bus, &p)?;
                let Rm::Mem { off, .. } = m.rm else {
                    return Err(invalid_opcode());
                };
                self.write_reg(m.reg, v, off & v.mask());
            }
            0x8E => {
                let m = self.modrm(bus, &p)?;
                let target = usize::from(m.reg);
                if target == CS || target >= self.segment_count() {
                    return Err(invalid_opcode());
                }
                let value = self.read_rm(bus, m.rm, Width::Word)? as u16;
                self.load_segment(bus, target, value)?;
                if target == SS {
                    self.irq_inhibit = true;
                }
            }
            0x8F => {
                let byte = self.fetch8(bus)?;
                if (byte >> 3) & 7 != 0 {
                    return Err(invalid_opcode());
                }
                // The destination's address is computed with ESP after the pop.
                let value = self.pop(bus, v)?;
                let m = self.decode_modrm(bus, &p, byte)?;
                self.write_rm(bus, m.rm, v, value)?;
            }
            0x90..=0x97 => {
                let r = op & 7;
                let (a, b) = (self.read_reg(0, v), self.read_reg(r, v));
                self.write_reg(0, v, b);
                self.write_reg(r, v, a);
            }
            0x98 => match v {
                Width::Dword => self.regs.gpr[AX] = i32::from(self.regs.ax() as i16) as u32,
                _ => self.regs.set_ax(i16::from(self.regs.al() as i8) as u16),
            },
            0x99 => match v {
                Width::Dword => self.regs.gpr[DX] = if self.regs.gpr[AX] & 0x8000_0000 != 0 { u32::MAX } else { 0 },
                _ => self.regs.set_dx(if self.regs.ax() & 0x8000 != 0 { 0xFFFF } else { 0 }),
            },
            0x9A => {
                let offset = self.fetch(bus, v)?;
                let segment = self.fetch16(bus)?;
                self.far_call(bus, v, segment, offset)?;
            }
            0x9B => self.fwait()?,
            0x9C => self.pushf(bus, v)?,
            0x9D => self.popf(bus, v)?,
            0x9E => {
                let low = u16::from(self.regs.ah()) & (SF | ZF | AF | PF | CF);
                self.regs.flags = (self.regs.flags & !0xFF) | u32::from(low) | 2;
            }
            0x9F => self.regs.set_ah(self.regs.flags as u8),
            0xA0..=0xA3 => {
                let w = p.width(op);
                let off = if p.addr32 { self.fetch32(bus)? } else { u32::from(self.fetch16(bus)?) };
                let rm = Rm::Mem { seg: p.seg.unwrap_or(DS), off, mask: p.addr_mask() };
                if op < 0xA2 {
                    let value = self.read_rm(bus, rm, w)?;
                    self.write_reg(0, w, value);
                } else {
                    let value = self.read_reg(0, w);
                    self.write_rm(bus, rm, w, value)?;
                }
            }
            0xA8 | 0xA9 => {
                let w = p.width(op);
                let a = self.read_reg(0, w);
                let b = self.fetch(bus, w)?;
                self.logic(w, a & b);
            }
            0xB0..=0xB7 => {
                let value = self.fetch8(bus)?;
                self.regs.set_reg8(op & 7, value);
            }
            0xB8..=0xBF => {
                let value = self.fetch(bus, v)?;
                self.write_reg(op & 7, v, value);
            }
            0xC0 | 0xC1 | 0xD0..=0xD3 => {
                let w = p.width(op);
                let m = self.modrm(bus, &p)?;
                let count = match op {
                    0xD0 | 0xD1 => 1,
                    0xD2 | 0xD3 => self.regs.cl(),
                    _ => self.fetch8(bus)?,
                };
                if count & 0x1F != 0 {
                    let value = self.read_rm(bus, m.rm, w)?;
                    let result = self.shift(m.reg, w, value, count);
                    self.write_rm(bus, m.rm, w, result)?;
                }
            }
            0xC2 | 0xC3 => {
                let release = if op == 0xC2 { self.fetch16(bus)? } else { 0 };
                let ip = self.pop(bus, v)?;
                self.regs.ip = if p.op32 { ip } else { ip & 0xFFFF };
                self.release_stack(u32::from(release));
            }
            0xC4 | 0xC5 => self.load_far_pointer(bus, &p, if op == 0xC4 { ES } else { DS })?,
            0xC6 | 0xC7 => {
                let w = p.width(op);
                let m = self.modrm(bus, &p)?;
                if m.reg != 0 {
                    return Err(invalid_opcode());
                }
                let value = self.fetch(bus, w)?;
                self.write_rm(bus, m.rm, w, value)?;
            }
            0xC8 => {
                let size = self.fetch16(bus)?;
                let level = self.fetch8(bus)?;
                self.enter(bus, v, size, level)?;
            }
            0xC9 => {
                if self.stack32() {
                    self.regs.gpr[SP] = self.regs.gpr[BP];
                } else {
                    self.regs.set_sp(self.regs.bp());
                }
                let value = self.pop(bus, v)?;
                self.write_reg(BP as u8, v, value);
            }
            0xCA | 0xCB => {
                let release = if op == 0xCA { self.fetch16(bus)? } else { 0 };
                self.far_return(bus, v, u32::from(release))?;
            }
            0xCC => self.deliver(bus, 3, IntKind::Breakpoint, None)?,
            0xCD => {
                let vector = self.fetch8(bus)?;
                self.deliver(bus, vector, IntKind::Software, None)?;
            }
            0xCE => {
                if self.regs.flag(OF) {
                    self.deliver(bus, vector::OVERFLOW, IntKind::Breakpoint, None)?;
                }
            }
            0xCF => self.iret(bus, v)?,
            0xD4 => {
                let base = self.fetch8(bus)?;
                let al = self.regs.al();
                let Some(quotient) = al.checked_div(base) else {
                    // Before the divide error the 286 and 386 have set SF, ZF and PF from AL
                    // shifted right once, a first divide step (SingleStepTests).
                    self.set_szp(Width::Byte, u32::from(al >> 1));
                    return Err(Abort::Fault(Fault::new(vector::DIVIDE)));
                };
                self.regs.set_ah(quotient);
                self.regs.set_al(al % base);
                self.set_szp(Width::Byte, u32::from(al % base));
            }
            0xD5 => {
                let base = self.fetch8(bus)?;
                let al = self.regs.al().wrapping_add(self.regs.ah().wrapping_mul(base));
                self.regs.set_ax(u16::from(al));
                self.set_szp(Width::Byte, u32::from(al));
            }
            0xD6 => self.regs.set_al(if self.regs.flag(CF) { 0xFF } else { 0 }),
            0xD7 => {
                let mask = p.addr_mask();
                let off = (self.regs.gpr[BX] & mask).wrapping_add(u32::from(self.regs.al()));
                let rm = Rm::Mem { seg: p.seg.unwrap_or(DS), off, mask };
                let value = self.read_rm(bus, rm, Width::Byte)?;
                self.regs.set_al(value as u8);
            }
            0xD8..=0xDF => self.escape(bus, &p, op, start_ip)?,
            0xE0..=0xE2 => {
                let rel = self.fetch8(bus)? as i8 as u32;
                let count = self.counter(&p).wrapping_sub(1) & p.addr_mask();
                self.set_counter(&p, count);
                let taken = count != 0
                    && match op {
                        0xE0 => !self.regs.flag(ZF),
                        0xE1 => self.regs.flag(ZF),
                        _ => true,
                    };
                if taken {
                    self.jump_relative(rel, p.op32);
                }
            }
            0xE3 => {
                let rel = self.fetch8(bus)? as i8 as u32;
                if self.counter(&p) == 0 {
                    self.jump_relative(rel, p.op32);
                }
            }
            0xE4..=0xE7 | 0xEC..=0xEF => {
                let w = p.width(op);
                let port = if op < 0xE8 { u16::from(self.fetch8(bus)?) } else { self.regs.dx() };
                self.check_io(bus, port, w)?;
                if op & 2 == 0 {
                    let value = port_in(bus, port, w);
                    self.cycles += self.io_read_cycles;
                    self.flush_code_cache();
                    self.write_reg(0, w, value);
                } else {
                    let value = self.read_reg(0, w);
                    port_out(bus, port, w, value);
                    self.cycles += self.io_write_cycles;
                    self.flush_code_cache();
                }
            }
            0xE8 => {
                let rel = if p.op32 { self.fetch32(bus)? } else { self.fetch16(bus)? as i16 as u32 };
                let ip = self.regs.ip;
                self.push(bus, v, ip)?;
                self.jump_relative(rel, p.op32);
            }
            0xE9 => {
                let rel = if p.op32 { self.fetch32(bus)? } else { self.fetch16(bus)? as i16 as u32 };
                self.jump_relative(rel, p.op32);
            }
            0xEA => {
                let offset = self.fetch(bus, v)?;
                let segment = self.fetch16(bus)?;
                self.far_jump(bus, v, segment, offset)?;
            }
            0xEB => {
                let rel = self.fetch8(bus)? as i8 as u32;
                self.jump_relative(rel, p.op32);
            }
            0xF4 => {
                self.check_privileged()?;
                return Err(Exit::Halt.into());
            }
            0xF5 => self.regs.flags ^= u32::from(CF),
            0xF6 | 0xF7 => self.group3(bus, op, &p)?,
            0xF8 => self.regs.set_flag(CF, false),
            0xF9 => self.regs.set_flag(CF, true),
            0xFA => {
                self.check_interrupt_flag_access()?;
                self.regs.set_flag(IF, false);
            }
            0xFB => {
                self.check_interrupt_flag_access()?;
                self.regs.set_flag(IF, true);
                self.irq_inhibit = true;
            }
            0xFC => self.regs.set_flag(DF, false),
            0xFD => self.regs.set_flag(DF, true),
            0xFE => {
                let byte = self.fetch8(bus)?;
                if byte == crate::CALLBACK_OPCODE[1] {
                    let id = self.fetch16(bus)?;
                    return Err(Exit::Callback(id).into());
                }
                let m = self.decode_modrm(bus, &p, byte)?;
                if m.reg > 1 {
                    return Err(invalid_opcode());
                }
                let value = self.read_rm(bus, m.rm, Width::Byte)?;
                let cf = self.regs.flag(CF);
                let result =
                    if m.reg == 0 { self.add(Width::Byte, value, 1, 0) } else { self.sub(Width::Byte, value, 1, 0) };
                self.regs.set_flag(CF, cf);
                self.write_rm(bus, m.rm, Width::Byte, result)?;
            }
            0xFF => self.group5(bus, &p)?,
            _ => return Err(self.unimplemented(bus, start_ip)),
        }
        Ok(())
    }

    /// Two-byte opcodes. The 80286 only has the system instructions in `0F 00`, `0F 01` and CLTS.
    fn exec_0f<B: Bus>(&mut self, bus: &mut B, p: &Prefixes, start_ip: u32) -> Res<()> {
        let op = self.fetch8(bus)?;
        let v = p.v();
        let i386 = self.model >= Model::I386;
        let i486 = self.model >= Model::I486;
        let protected = self.protected_mode() && !self.v86();
        match op {
            0x00 if protected => self.system_group6(bus, p, start_ip)?,
            0x01 => self.system_group7(bus, p, start_ip)?,
            0x02 | 0x03 if protected => {
                let m = self.modrm(bus, p)?;
                let selector = self.read_rm(bus, m.rm, Width::Word)? as u16;
                match self.lar_lsl(bus, selector, op == 0x03)? {
                    Some(value) => {
                        self.write_reg(m.reg, v, value & v.mask());
                        self.regs.set_flag(ZF, true);
                    }
                    None => self.regs.set_flag(ZF, false),
                }
            }
            0x06 => {
                self.check_privileged()?;
                self.cr0 &= !0x08; // CLTS
            }
            // INVD, WBINVD: there are no caches to flush.
            0x08 | 0x09 if i486 => self.check_privileged()?,
            0xB0 | 0xB1 if i486 => self.cmpxchg(bus, p, op)?,
            0xC0 | 0xC1 if i486 => {
                // XADD: the destination gets the sum, the source register the old destination.
                let w = p.width(op);
                let m = self.modrm(bus, p)?;
                let (dest, src) = (self.read_rm(bus, m.rm, w)?, self.read_reg(m.reg, w));
                let sum = self.add(w, dest, src, 0);
                self.write_reg(m.reg, w, dest);
                self.write_rm(bus, m.rm, w, sum)?;
            }
            0xC8..=0xCF if i486 => {
                let r = usize::from(op & 7);
                if v == Width::Dword {
                    self.regs.gpr[r] = self.regs.gpr[r].swap_bytes();
                } else {
                    self.regs.set_reg16(r, 0); // undefined; DOSBox clears it
                }
            }
            0x20 | 0x22 if i386 => {
                let byte = self.fetch8(bus)?;
                let (index, r) = ((byte >> 3) & 7, usize::from(byte & 7));
                if op == 0x20 {
                    self.regs.gpr[r] = self.read_control(index)?;
                } else {
                    let value = self.regs.gpr[r];
                    self.write_control(bus, index, value)?;
                }
            }
            0x21 | 0x23 if i386 => {
                let byte = self.fetch8(bus)?;
                self.check_privileged()?;
                let (index, r) = (usize::from((byte >> 3) & 7), usize::from(byte & 7));
                if op == 0x21 {
                    self.regs.gpr[r] = self.dr[index];
                } else {
                    self.dr[index] = self.regs.gpr[r];
                }
            }
            0x24 | 0x26 if i386 => {
                // Test registers TR6/TR7: accepted, reads give 0.
                let byte = self.fetch8(bus)?;
                self.check_privileged()?;
                if op == 0x24 {
                    self.regs.gpr[usize::from(byte & 7)] = 0;
                }
            }
            0x80..=0x8F if i386 => {
                let rel = if p.op32 { self.fetch32(bus)? } else { self.fetch16(bus)? as i16 as u32 };
                if self.condition(op & 0x0F) {
                    self.jump_relative(rel, p.op32);
                }
            }
            0x90..=0x9F if i386 => {
                let m = self.modrm(bus, p)?;
                let value = u32::from(self.condition(op & 0x0F));
                self.write_rm(bus, m.rm, Width::Byte, value)?;
            }
            0xA0 | 0xA8 if i386 => {
                let value = u32::from(self.regs.seg[if op == 0xA0 { FS } else { GS }]);
                self.push(bus, v, value)?;
            }
            0xA1 | 0xA9 if i386 => {
                let value = self.pop(bus, v)? as u16;
                self.load_segment(bus, if op == 0xA1 { FS } else { GS }, value)?;
            }
            0xA3 | 0xAB | 0xB3 | 0xBB if i386 => {
                let m = self.modrm(bus, p)?;
                let offset = self.read_reg(m.reg, v);
                self.bit_test(bus, (op >> 3) & 3, m.rm, v, offset, true)?;
            }
            0xBA if i386 => {
                let m = self.modrm(bus, p)?;
                let offset = u32::from(self.fetch8(bus)?);
                if m.reg < 4 {
                    return Err(invalid_opcode());
                }
                self.bit_test(bus, m.reg & 3, m.rm, v, offset, false)?;
            }
            0xA4 | 0xA5 | 0xAC | 0xAD if i386 => {
                let m = self.modrm(bus, p)?;
                let count = if op & 1 == 0 { self.fetch8(bus)? } else { self.regs.cl() };
                let count = u32::from(count & 0x1F);
                if count != 0 {
                    let dest = self.read_rm(bus, m.rm, v)?;
                    let src = self.read_reg(m.reg, v);
                    let result = self.double_shift(v, dest, src, count, op < 0xA8);
                    self.write_rm(bus, m.rm, v, result)?;
                }
            }
            0xAF if i386 => {
                let m = self.modrm(bus, p)?;
                let a = v.sign_extend(self.read_reg(m.reg, v));
                let b = v.sign_extend(self.read_rm(bus, m.rm, v)?);
                self.imul_into(m.reg, v, a, b);
            }
            0xB2 | 0xB4 | 0xB5 if i386 => {
                let target = match op {
                    0xB2 => SS,
                    0xB4 => FS,
                    _ => GS,
                };
                self.load_far_pointer(bus, p, target)?;
            }
            0xB6 | 0xB7 | 0xBE | 0xBF if i386 => {
                let source = if op & 1 == 0 { Width::Byte } else { Width::Word };
                let m = self.modrm(bus, p)?;
                let value = self.read_rm(bus, m.rm, source)?;
                let value = if op < 0xB8 { value } else { source.sign_extend(value) as u32 & v.mask() };
                self.write_reg(m.reg, v, value);
            }
            0xBC | 0xBD if i386 => {
                let m = self.modrm(bus, p)?;
                let value = self.read_rm(bus, m.rm, v)?;
                if value == 0 {
                    self.regs.set_flag(ZF, true);
                } else {
                    self.regs.set_flag(ZF, false);
                    let index = if op == 0xBC { value.trailing_zeros() } else { 31 - value.leading_zeros() };
                    self.write_reg(m.reg, v, index);
                }
            }
            _ => return Err(self.unimplemented(bus, start_ip)),
        }
        Ok(())
    }

    /// `0F 00`: SLDT, STR, LLDT, LTR, VERR, VERW (protected mode only).
    fn system_group6<B: Bus>(&mut self, bus: &mut B, p: &Prefixes, start_ip: u32) -> Res<()> {
        let m = self.modrm(bus, p)?;
        let store_width = if matches!(m.rm, Rm::Reg(_)) { p.v() } else { Width::Word };
        match m.reg {
            0 => {
                let value = u32::from(self.ldtr.selector);
                self.write_rm(bus, m.rm, store_width, value)?;
            }
            1 => {
                let value = u32::from(self.tr.selector);
                self.write_rm(bus, m.rm, store_width, value)?;
            }
            2 | 3 => {
                let selector = self.read_rm(bus, m.rm, Width::Word)? as u16;
                if m.reg == 2 {
                    self.lldt(bus, selector)?;
                } else {
                    self.ltr(bus, selector)?;
                }
            }
            4 | 5 => {
                let selector = self.read_rm(bus, m.rm, Width::Word)? as u16;
                let ok = self.verify(bus, selector, m.reg == 5)?;
                self.regs.set_flag(ZF, ok);
            }
            _ => return Err(self.unimplemented(bus, start_ip)),
        }
        Ok(())
    }

    /// `0F 01`: SGDT, SIDT, LGDT, LIDT, SMSW, LMSW.
    fn system_group7<B: Bus>(&mut self, bus: &mut B, p: &Prefixes, start_ip: u32) -> Res<()> {
        let m = self.modrm(bus, p)?;
        match (m.reg, m.rm) {
            (0..=3, Rm::Mem { seg, off, mask }) => {
                if m.reg < 2 {
                    let TableRegister { limit, base } = if m.reg == 0 { self.gdtr } else { self.idtr };
                    // A 16-bit SGDT/SIDT stores the base's top byte as 0 on a 386... except it
                    // stores FFh on a 286; real software doesn't rely on either.
                    let base = if p.op32 || self.model >= Model::I386 { base } else { base | 0xFF00_0000 };
                    self.write_mem(bus, seg, off, mask, Width::Word, u32::from(limit))?;
                    self.write_mem(bus, seg, off.wrapping_add(2), mask, Width::Dword, base)?;
                } else {
                    self.check_privileged()?;
                    let limit = self.read_mem(bus, seg, off, mask, Width::Word)? as u16;
                    let base = self.read_mem(bus, seg, off.wrapping_add(2), mask, Width::Dword)?;
                    let base = if p.op32 { base } else { base & 0x00FF_FFFF };
                    let table = if m.reg == 2 { &mut self.gdtr } else { &mut self.idtr };
                    *table = TableRegister { limit, base };
                }
            }
            (4, rm) => {
                let msw = match self.model {
                    Model::I286 => 0xFFF0 | (self.cr0 & 0x0F),
                    Model::I386 | Model::I486 => self.cr0 & 0xFFFF,
                };
                let w = if matches!(rm, Rm::Reg(_)) { p.v() } else { Width::Word };
                self.write_rm(bus, rm, w, msw)?;
            }
            (6, rm) => {
                let value = self.read_rm(bus, rm, Width::Word)? as u16;
                self.lmsw(bus, value)?;
            }
            // INVLPG (486): DOSBox flushes the whole TLB, and so does this.
            (7, Rm::Mem { .. }) if self.model >= Model::I486 => {
                self.check_privileged()?;
                self.flush_tlb();
            }
            _ => return Err(self.unimplemented(bus, start_ip)),
        }
        Ok(())
    }

    fn group3<B: Bus>(&mut self, bus: &mut B, op: u8, p: &Prefixes) -> Res<()> {
        let w = p.width(op);
        let m = self.modrm(bus, p)?;
        match m.reg {
            0 | 1 => {
                let a = self.read_rm(bus, m.rm, w)?;
                let b = self.fetch(bus, w)?;
                self.logic(w, a & b);
            }
            2 => {
                let value = self.read_rm(bus, m.rm, w)?;
                self.write_rm(bus, m.rm, w, !value & w.mask())?;
            }
            3 => {
                let value = self.read_rm(bus, m.rm, w)?;
                let result = self.sub(w, 0, value, 0);
                self.write_rm(bus, m.rm, w, result)?;
            }
            4 | 5 => {
                let src = self.read_rm(bus, m.rm, w)?;
                let acc = self.read_reg(0, w);
                // The double-width product, as (low, high) halves.
                let (low, high, overflow) = if m.reg == 4 {
                    let product = u64::from(acc) * u64::from(src);
                    let high = (product >> w.bits()) as u32 & w.mask();
                    (product as u32 & w.mask(), high, high != 0)
                } else {
                    let product = w.sign_extend(acc) * w.sign_extend(src);
                    let low = product as u32 & w.mask();
                    (low, (product >> w.bits()) as u32 & w.mask(), product != w.sign_extend(low))
                };
                match w {
                    Width::Byte => self.regs.set_ax(((high as u16) << 8) | low as u16),
                    _ => {
                        self.write_reg(AX as u8, w, low);
                        self.write_reg(DX as u8, w, high);
                    }
                }
                self.regs.set_flag(CF, overflow);
                self.regs.set_flag(OF, overflow);
            }
            _ => {
                let src = self.read_rm(bus, m.rm, w)?;
                if !self.divide(w, m.reg == 7, src) {
                    return Err(Abort::Fault(Fault::new(vector::DIVIDE)));
                }
            }
        }
        Ok(())
    }

    /// DIV (signed = false) or IDIV. Returns false on a divide error, leaving registers untouched.
    fn divide(&mut self, w: Width, signed: bool, src: u32) -> bool {
        if src == 0 {
            return false;
        }
        // Dividend: AX for bytes, DX:AX for words, EDX:EAX for doublewords.
        let (low_reg, high_reg) = if w == Width::Byte { (AX, AX) } else { (AX, DX) };
        let dividend = match w {
            Width::Byte => u64::from(self.regs.ax()),
            _ => (u64::from(self.read_reg(high_reg as u8, w)) << w.bits()) | u64::from(self.read_reg(low_reg as u8, w)),
        };
        let (quotient, remainder) = if signed {
            let dividend = match w {
                Width::Byte => i64::from(dividend as u16 as i16),
                Width::Word => i64::from(dividend as u32 as i32),
                Width::Dword => dividend as i64,
            };
            let divisor = w.sign_extend(src);
            let Some(quotient) = dividend.checked_div(divisor) else { return false };
            let limit = i64::from(w.sign());
            if quotient < -limit || quotient >= limit {
                return false;
            }
            (quotient as u32 & w.mask(), (dividend % divisor) as u32 & w.mask())
        } else {
            let quotient = dividend / u64::from(src);
            if quotient > u64::from(w.mask()) {
                return false;
            }
            (quotient as u32, (dividend % u64::from(src)) as u32)
        };
        match w {
            Width::Byte => {
                self.regs.set_al(quotient as u8);
                self.regs.set_ah(remainder as u8);
            }
            _ => {
                self.write_reg(AX as u8, w, quotient);
                self.write_reg(DX as u8, w, remainder);
            }
        }
        true
    }

    /// Two- and three-operand IMUL: `reg = a * b`, truncated to `w`.
    fn imul_into(&mut self, reg: u8, w: Width, a: i64, b: i64) {
        let product = a * b;
        let low = product as u32 & w.mask();
        self.write_reg(reg, w, low);
        let overflow = product != w.sign_extend(low);
        self.regs.set_flag(CF, overflow);
        self.regs.set_flag(OF, overflow);
    }

    fn group5<B: Bus>(&mut self, bus: &mut B, p: &Prefixes) -> Res<()> {
        let v = p.v();
        let m = self.modrm(bus, p)?;
        match m.reg {
            0 | 1 => {
                let value = self.read_rm(bus, m.rm, v)?;
                let cf = self.regs.flag(CF);
                let result = if m.reg == 0 { self.add(v, value, 1, 0) } else { self.sub(v, value, 1, 0) };
                self.regs.set_flag(CF, cf);
                self.write_rm(bus, m.rm, v, result)?;
            }
            2 => {
                let target = self.read_rm(bus, m.rm, v)?;
                let ip = self.regs.ip;
                self.push(bus, v, ip)?;
                self.regs.ip = target;
            }
            4 => self.regs.ip = self.read_rm(bus, m.rm, v)?,
            3 | 5 => {
                let Rm::Mem { seg, off, mask } = m.rm else {
                    return Err(invalid_opcode());
                };
                let offset = self.read_mem(bus, seg, off, mask, v)?;
                let segment = self.read_mem(bus, seg, off.wrapping_add(v.bytes()), mask, Width::Word)? as u16;
                if m.reg == 3 {
                    self.far_call(bus, v, segment, offset)?;
                } else {
                    self.far_jump(bus, v, segment, offset)?;
                }
            }
            6 => {
                let value = self.read_rm(bus, m.rm, v)?;
                self.push(bus, v, value)?;
            }
            _ => return Err(invalid_opcode()), // FF /7
        }
        Ok(())
    }

    /// EIP += `rel`, kept within 64 KB for 16-bit operand size.
    fn jump_relative(&mut self, rel: u32, op32: bool) {
        let ip = self.regs.ip.wrapping_add(rel);
        self.regs.ip = if op32 { ip } else { ip & 0xFFFF };
    }

    /// ENTER (DOSBox `CPU_ENTER`).
    fn enter<B: Bus>(&mut self, bus: &mut B, w: Width, bytes: u16, level: u8) -> Res<()> {
        let level = u32::from(level & 0x1F);
        let mask = if self.stack32() { u32::MAX } else { 0xFFFF };
        let size = w.bytes();
        let mut sp = self.regs.gpr[SP] & mask;
        let mut bp = self.regs.gpr[BP] & mask;
        sp = sp.wrapping_sub(size);
        let old_bp = self.read_reg(BP as u8, w);
        self.write_mem(bus, SS, sp & mask, mask, w, old_bp)?;
        let frame = self.regs.gpr[SP].wrapping_sub(size);
        if level > 0 {
            for _ in 1..level {
                sp = sp.wrapping_sub(size);
                bp = bp.wrapping_sub(size);
                let value = self.read_mem(bus, SS, bp & mask, mask, w)?;
                self.write_mem(bus, SS, sp & mask, mask, w, value)?;
            }
            sp = sp.wrapping_sub(size);
            self.write_mem(bus, SS, sp & mask, mask, w, frame & w.mask())?;
        }
        self.write_reg(BP as u8, w, frame & w.mask());
        sp = sp.wrapping_sub(u32::from(bytes));
        self.regs.gpr[SP] = (self.regs.gpr[SP] & !mask) | (sp & mask);
        Ok(())
    }

    /// LES, LDS, LSS, LFS, LGS: a far pointer from memory into a register and `target`.
    fn load_far_pointer<B: Bus>(&mut self, bus: &mut B, p: &Prefixes, target: usize) -> Res<()> {
        let v = p.v();
        let m = self.modrm(bus, p)?;
        let Rm::Mem { seg, off, mask } = m.rm else {
            return Err(invalid_opcode());
        };
        let offset = self.read_mem(bus, seg, off, mask, v)?;
        let segment = self.read_mem(bus, seg, off.wrapping_add(v.bytes()), mask, Width::Word)? as u16;
        self.load_segment(bus, target, segment)?;
        self.write_reg(m.reg, v, offset);
        if target == SS {
            self.irq_inhibit = true;
        }
        Ok(())
    }

    /// BT, BTS, BTR, BTC (`kind` 0–3). A register-supplied bit offset can reach outside the
    /// addressed memory operand; an immediate one is taken modulo the operand size.
    fn bit_test<B: Bus>(
        &mut self,
        bus: &mut B,
        kind: u8,
        rm: Rm,
        w: Width,
        offset: u32,
        from_register: bool,
    ) -> Res<()> {
        let bits = w.bits();
        let (rm, bit) = match rm {
            Rm::Mem { seg, off, mask } if from_register => {
                let signed = w.sign_extend(offset);
                let unit = signed.div_euclid(i64::from(bits));
                let off = off.wrapping_add((unit * i64::from(w.bytes())) as u32);
                (Rm::Mem { seg, off, mask }, signed.rem_euclid(i64::from(bits)) as u32)
            }
            _ => (rm, offset % bits),
        };
        let value = self.read_rm(bus, rm, w)?;
        let mask = 1u32 << bit;
        self.regs.set_flag(CF, value & mask != 0);
        let result = match kind {
            1 => value | mask,
            2 => value & !mask,
            3 => value ^ mask,
            _ => return Ok(()),
        };
        self.write_rm(bus, rm, w, result)
    }

    /// SHLD (`left`) or SHRD with a nonzero count already masked to 5 bits. A 16-bit operand
    /// shifts through dest:src:src (SHLD) or src:src:dest (SHRD), which is what the 386 does
    /// with counts of 17-31 (found with SingleStepTests; Bochs repeats dest instead).
    fn double_shift(&mut self, w: Width, dest: u32, src: u32, count: u32, left: bool) -> u32 {
        let bits = w.bits();
        let (dest64, src64) = (u64::from(dest), u64::from(src));
        let (result, cf) = if w == Width::Word {
            if left {
                let shifted = ((dest64 << 32) | (src64 << 16) | src64) << count;
                (((shifted >> 32) & 0xFFFF) as u32, (shifted >> 48) & 1 != 0)
            } else {
                let triple = (src64 << 32) | (src64 << 16) | dest64;
                (((triple >> count) & 0xFFFF) as u32, (triple >> (count - 1)) & 1 != 0)
            }
        } else if left {
            let r = ((dest64 << count) | (src64 >> (bits - count))) as u32 & w.mask();
            (r, (dest64 >> (bits - count)) & 1 != 0)
        } else {
            let r = ((dest64 >> count) | (src64 << (bits - count))) as u32 & w.mask();
            (r, (dest64 >> (count - 1)) & 1 != 0)
        };
        self.regs.set_flag(CF, cf);
        self.regs.set_flag(OF, (result ^ dest) & w.sign() != 0);
        self.set_szp(w, result);
        result
    }

    fn string_op<B: Bus>(&mut self, bus: &mut B, op: u8, p: &Prefixes) -> Res<()> {
        let w = p.width(op);
        let kind = op & 0xFE;
        if kind == 0x6C || kind == 0x6E {
            let port = self.regs.dx();
            self.check_io(bus, port, w)?;
        }
        let src_seg = p.seg.unwrap_or(DS);
        let step = if self.regs.flag(DF) { w.bytes().wrapping_neg() } else { w.bytes() };
        if p.rep == Rep::None {
            return self.string_once(bus, kind, w, src_seg, step, p.addr32);
        }
        let compares = kind == 0xA6 || kind == 0xAE;
        while self.counter(p) != 0 {
            if let Err(fault) = self.string_once(bus, kind, w, src_seg, step, p.addr32) {
                // Keep the progress made so far: the instruction restarts where it faulted.
                self.string_restart = Some((self.regs.gpr[SI], self.regs.gpr[DI], self.regs.gpr[CX]));
                return Err(fault);
            }
            let count = self.counter(p) - 1;
            self.set_counter(p, count);
            self.cycles += 1;
            if compares && (self.regs.flag(ZF) != (p.rep == Rep::Repe)) {
                break;
            }
        }
        Ok(())
    }

    fn string_once<B: Bus>(
        &mut self,
        bus: &mut B,
        kind: u8,
        w: Width,
        src_seg: usize,
        step: u32,
        addr32: bool,
    ) -> Res<()> {
        let mask = if addr32 { u32::MAX } else { 0xFFFF };
        let (si, di) = (self.regs.gpr[SI] & mask, self.regs.gpr[DI] & mask);
        let (mut advance_si, mut advance_di) = (false, false);
        match kind {
            0xA4 => {
                let value = self.read_mem(bus, src_seg, si, mask, w)?;
                self.write_mem(bus, ES, di, mask, w, value)?;
                (advance_si, advance_di) = (true, true);
            }
            0xA6 => {
                let a = self.read_mem(bus, src_seg, si, mask, w)?;
                let b = self.read_mem(bus, ES, di, mask, w)?;
                self.sub(w, a, b, 0);
                (advance_si, advance_di) = (true, true);
            }
            0xAA => {
                let value = self.read_reg(0, w);
                self.write_mem(bus, ES, di, mask, w, value)?;
                advance_di = true;
            }
            0xAC => {
                let value = self.read_mem(bus, src_seg, si, mask, w)?;
                self.write_reg(0, w, value);
                advance_si = true;
            }
            0xAE => {
                let a = self.read_reg(0, w);
                let b = self.read_mem(bus, ES, di, mask, w)?;
                self.sub(w, a, b, 0);
                advance_di = true;
            }
            0x6C => {
                let value = port_in(bus, self.regs.dx(), w);
                self.cycles += self.io_read_cycles;
                self.flush_code_cache();
                self.write_mem(bus, ES, di, mask, w, value)?;
                advance_di = true;
            }
            _ => {
                let value = self.read_mem(bus, src_seg, si, mask, w)?;
                port_out(bus, self.regs.dx(), w, value);
                self.cycles += self.io_write_cycles;
                self.flush_code_cache();
                advance_si = true;
            }
        }
        let advance = |reg: &mut u32| *reg = (*reg & !mask) | (reg.wrapping_add(step) & mask);
        if advance_si {
            advance(&mut self.regs.gpr[SI]);
        }
        if advance_di {
            advance(&mut self.regs.gpr[DI]);
        }
        Ok(())
    }

    /// CX or ECX, by address size (REP, LOOP, JCXZ).
    fn counter(&self, p: &Prefixes) -> u32 {
        self.regs.gpr[CX] & p.addr_mask()
    }

    fn set_counter(&mut self, p: &Prefixes, value: u32) {
        if p.addr32 {
            self.regs.gpr[CX] = value;
        } else {
            self.regs.set_cx(value as u16);
        }
    }

    fn condition(&self, cc: u8) -> bool {
        let f = |mask| self.regs.flag(mask);
        let result = match cc >> 1 {
            0 => f(OF),
            1 => f(CF),
            2 => f(ZF),
            3 => f(CF) || f(ZF),
            4 => f(SF),
            5 => f(PF),
            6 => f(SF) != f(OF),
            _ => f(ZF) || f(SF) != f(OF),
        };
        result != (cc & 1 == 1)
    }

    fn segment_count(&self) -> usize {
        match self.model {
            Model::I286 => 4,
            Model::I386 | Model::I486 => 6,
        }
    }

    /// CMPXCHG (486): if the accumulator equals the destination, the source goes to the
    /// destination; otherwise the destination goes to the accumulator. The flags are those of
    /// CMP accumulator, destination (DOSBox sets ZF only), and memory is always written, as on
    /// the processor.
    fn cmpxchg<B: Bus>(&mut self, bus: &mut B, p: &Prefixes, op: u8) -> Res<()> {
        let w = if op == 0xB0 { Width::Byte } else { p.v() };
        let m = self.modrm(bus, p)?;
        let dest = self.read_rm(bus, m.rm, w)?;
        let acc = self.read_reg(0, w);
        self.sub(w, acc, dest, 0);
        if acc == dest {
            let src = self.read_reg(m.reg, w);
            self.write_rm(bus, m.rm, w, src)?;
        } else {
            if matches!(m.rm, Rm::Mem { .. }) {
                self.write_rm(bus, m.rm, w, dest)?;
            }
            self.write_reg(0, w, dest);
        }
        Ok(())
    }

    // ---- ALU ----

    fn alu(&mut self, op: u8, w: Width, a: u32, b: u32) -> u32 {
        match op {
            0 => self.add(w, a, b, 0),
            1 => self.logic(w, a | b),
            2 => {
                let carry = u32::from(self.regs.flag(CF));
                self.add(w, a, b, carry)
            }
            3 => {
                let borrow = u32::from(self.regs.flag(CF));
                self.sub(w, a, b, borrow)
            }
            4 => self.logic(w, a & b),
            6 => self.logic(w, a ^ b),
            _ => self.sub(w, a, b, 0), // 5 SUB, 7 CMP
        }
    }

    #[inline]
    fn add(&mut self, w: Width, a: u32, b: u32, carry: u32) -> u32 {
        let full = u64::from(a) + u64::from(b) + u64::from(carry);
        let r = full as u32 & w.mask();
        self.regs.set_flag(CF, full > u64::from(w.mask()));
        self.regs.set_flag(AF, (a ^ b ^ r) & 0x10 != 0);
        self.regs.set_flag(OF, (a ^ r) & (b ^ r) & w.sign() != 0);
        self.set_szp(w, r);
        r
    }

    #[inline]
    fn sub(&mut self, w: Width, a: u32, b: u32, borrow: u32) -> u32 {
        let r = a.wrapping_sub(b).wrapping_sub(borrow) & w.mask();
        self.regs.set_flag(CF, u64::from(b) + u64::from(borrow) > u64::from(a));
        self.regs.set_flag(AF, (a ^ b ^ r) & 0x10 != 0);
        self.regs.set_flag(OF, (a ^ b) & (a ^ r) & w.sign() != 0);
        self.set_szp(w, r);
        r
    }

    #[inline]
    fn logic(&mut self, w: Width, r: u32) -> u32 {
        self.regs.set_flag(CF, false);
        self.regs.set_flag(OF, false);
        self.regs.set_flag(AF, false);
        self.set_szp(w, r);
        r
    }

    #[inline]
    fn set_szp(&mut self, w: Width, r: u32) {
        self.regs.set_flag(SF, r & w.sign() != 0);
        self.regs.set_flag(ZF, r & w.mask() == 0);
        self.regs.set_flag(PF, (r as u8).count_ones() % 2 == 0);
    }

    /// Group 2: ROL ROR RCL RCR SHL SHR SAL SAR. `count` is masked to 5 bits; the caller skips
    /// a zero count, which changes neither the operand nor the flags.
    fn shift(&mut self, op: u8, w: Width, value: u32, count: u8) -> u32 {
        let count = u32::from(count & 0x1F);
        let (bits, mask) = (w.bits(), w.mask());
        let msb = |v: u32| v & w.sign() != 0;
        let wide = u64::from(value);
        match op {
            0 => {
                let c = count % bits;
                let r = if c == 0 { value } else { ((wide << c) | (wide >> (bits - c))) as u32 & mask };
                let cf = r & 1 != 0;
                self.regs.set_flag(CF, cf);
                self.regs.set_flag(OF, msb(r) != cf);
                r
            }
            1 => {
                let c = count % bits;
                let r = if c == 0 { value } else { ((wide >> c) | (wide << (bits - c))) as u32 & mask };
                self.regs.set_flag(CF, msb(r));
                self.regs.set_flag(OF, msb(r) != (r & (w.sign() >> 1) != 0));
                r
            }
            2 => {
                let mut r = value;
                let mut cf = self.regs.flag(CF);
                for _ in 0..count % (bits + 1) {
                    let out = msb(r);
                    r = ((r << 1) | u32::from(cf)) & mask;
                    cf = out;
                }
                self.regs.set_flag(CF, cf);
                self.regs.set_flag(OF, msb(r) != cf);
                r
            }
            3 => {
                let mut r = value;
                let mut cf = self.regs.flag(CF);
                for _ in 0..count % (bits + 1) {
                    let out = r & 1 != 0;
                    r = (r >> 1) | (u32::from(cf) << (bits - 1));
                    cf = out;
                }
                // The 386 sets OF from the result's top two bits, even when the count is a
                // multiple of width + 1 and nothing moved (SingleStepTests).
                self.regs.set_flag(CF, cf);
                self.regs.set_flag(OF, msb(r) != (r & (w.sign() >> 1) != 0));
                r
            }
            4 | 6 => {
                let shifted = wide << count;
                let r = shifted as u32 & mask;
                // Past the width CF is clear, except that on the 386 a byte shifted by 16 or 24
                // acts like one shifted by 8 (SingleStepTests).
                let cf = if count <= bits {
                    (shifted >> bits) & 1 != 0
                } else {
                    self.model == Model::I386 && w == Width::Byte && count % 8 == 0 && value & 1 != 0
                };
                self.regs.set_flag(CF, cf);
                self.regs.set_flag(OF, msb(r) != cf);
                self.regs.set_flag(AF, false);
                self.set_szp(w, r);
                r
            }
            5 => {
                // OF is the sign before the last 1-bit step. Past the width CF is clear, except
                // that on the 386 a byte shifted by 16 or 24 acts like one shifted by 8
                // (SingleStepTests).
                let r = value >> count;
                let cf = if count <= bits {
                    (value >> (count - 1)) & 1 != 0
                } else {
                    self.model == Model::I386 && w == Width::Byte && count % 8 == 0 && value & 0x80 != 0
                };
                self.regs.set_flag(CF, cf);
                self.regs.set_flag(OF, msb(value >> (count - 1)));
                self.regs.set_flag(AF, false);
                self.set_szp(w, r);
                r
            }
            _ => {
                let signed = w.sign_extend(value);
                let r = (signed >> count) as u32 & mask;
                self.regs.set_flag(CF, (signed >> (count - 1)) & 1 != 0);
                self.regs.set_flag(OF, false);
                self.regs.set_flag(AF, false);
                self.set_szp(w, r);
                r
            }
        }
    }

    fn daa(&mut self) {
        let (al, cf) = (self.regs.al(), self.regs.flag(CF));
        let low_adjust = al & 0x0F > 9 || self.regs.flag(AF);
        // Unlike DAS, the carry out of the low-digit adjustment never survives: CF depends only
        // on the high-digit test.
        let high_adjust = al > 0x99 || cf;
        let mut result = al;
        if low_adjust {
            result = result.wrapping_add(6);
        }
        if high_adjust {
            result = result.wrapping_add(0x60);
        }
        self.regs.set_al(result);
        self.regs.set_flag(AF, low_adjust);
        self.regs.set_flag(CF, high_adjust);
        self.set_szp(Width::Byte, u32::from(result));
    }

    fn das(&mut self) {
        let (al, cf) = (self.regs.al(), self.regs.flag(CF));
        let mut result = al;
        let mut new_cf = false;
        if al & 0x0F > 9 || self.regs.flag(AF) {
            let (diff, borrow) = result.overflowing_sub(6);
            result = diff;
            new_cf = cf || borrow;
            self.regs.set_flag(AF, true);
        } else {
            self.regs.set_flag(AF, false);
        }
        if al > 0x99 || cf {
            result = result.wrapping_sub(0x60);
            new_cf = true;
        }
        self.regs.set_al(result);
        self.regs.set_flag(CF, new_cf);
        self.set_szp(Width::Byte, u32::from(result));
    }

    fn aaa(&mut self) {
        let adjust = self.regs.al() & 0x0F > 9 || self.regs.flag(AF);
        if adjust {
            self.regs.set_ax(self.regs.ax().wrapping_add(0x106));
        }
        self.regs.set_flag(AF, adjust);
        self.regs.set_flag(CF, adjust);
        self.regs.set_al(self.regs.al() & 0x0F);
    }

    fn aas(&mut self) {
        let adjust = self.regs.al() & 0x0F > 9 || self.regs.flag(AF);
        if adjust {
            self.regs.set_ax(self.regs.ax().wrapping_sub(6));
            self.regs.set_ah(self.regs.ah().wrapping_sub(1));
        }
        self.regs.set_flag(AF, adjust);
        self.regs.set_flag(CF, adjust);
        self.regs.set_al(self.regs.al() & 0x0F);
    }

    // ---- operands ----

    #[inline]
    fn modrm<B: Bus>(&mut self, bus: &mut B, p: &Prefixes) -> Res<ModRm> {
        let byte = self.fetch8(bus)?;
        self.decode_modrm(bus, p, byte)
    }

    pub(crate) fn decode_modrm<B: Bus>(&mut self, bus: &mut B, p: &Prefixes, byte: u8) -> Res<ModRm> {
        let (md, reg, rm) = (byte >> 6, (byte >> 3) & 7, byte & 7);
        if md == 3 {
            return Ok(ModRm { reg, rm: Rm::Reg(rm) });
        }
        let g = self.regs.gpr;
        if !p.addr32 {
            let w = |r: usize| g[r] as u16;
            let (base, default_seg) = match rm {
                0 => (w(BX).wrapping_add(w(SI)), DS),
                1 => (w(BX).wrapping_add(w(DI)), DS),
                2 => (w(BP).wrapping_add(w(SI)), SS),
                3 => (w(BP).wrapping_add(w(DI)), SS),
                4 => (w(SI), DS),
                5 => (w(DI), DS),
                6 if md == 0 => (0, DS),
                6 => (w(BP), SS),
                _ => (w(BX), DS),
            };
            let disp = match md {
                0 if rm == 6 => self.fetch16(bus)?,
                0 => 0,
                1 => self.fetch8(bus)? as i8 as u16,
                _ => self.fetch16(bus)?,
            };
            let off = u32::from(base.wrapping_add(disp));
            return Ok(ModRm { reg, rm: Rm::Mem { seg: p.seg.unwrap_or(default_seg), off, mask: 0xFFFF } });
        }

        // 32-bit addressing: optional SIB byte, EBP/ESP bases default to SS.
        let mut default_seg = DS;
        let mut ea = 0u32;
        let mut disp32_only = false;
        if rm == 4 {
            let sib = self.fetch8(bus)?;
            let (scale, index, base) = (sib >> 6, usize::from((sib >> 3) & 7), usize::from(sib & 7));
            if index != 4 {
                ea = g[index] << scale;
            }
            if base == 5 && md == 0 {
                disp32_only = true;
            } else {
                // With no index, the 386 applies the scale to the base instead (an undocumented
                // quirk later processors dropped; seen in SingleStepTests).
                let base_value = if index == 4 { g[base] << scale } else { g[base] };
                ea = ea.wrapping_add(base_value);
                if base == SP || base == BP {
                    default_seg = SS;
                }
            }
        } else if rm == 5 && md == 0 {
            disp32_only = true;
        } else {
            ea = g[usize::from(rm)];
            if usize::from(rm) == BP {
                default_seg = SS;
            }
        }
        let disp = if disp32_only || md == 2 {
            self.fetch32(bus)?
        } else if md == 1 {
            self.fetch8(bus)? as i8 as i32 as u32
        } else {
            0
        };
        Ok(ModRm { reg, rm: Rm::Mem { seg: p.seg.unwrap_or(default_seg), off: ea.wrapping_add(disp), mask: u32::MAX } })
    }

    #[inline]
    fn read_reg(&self, r: u8, w: Width) -> u32 {
        match w {
            Width::Byte => u32::from(self.regs.reg8(r)),
            Width::Word => self.regs.gpr[usize::from(r)] & 0xFFFF,
            Width::Dword => self.regs.gpr[usize::from(r)],
        }
    }

    #[inline]
    fn write_reg(&mut self, r: u8, w: Width, value: u32) {
        match w {
            Width::Byte => self.regs.set_reg8(r, value as u8),
            Width::Word => self.regs.set_reg16(usize::from(r), value as u16),
            Width::Dword => self.regs.gpr[usize::from(r)] = value,
        }
    }

    #[inline]
    fn read_rm<B: Bus>(&mut self, bus: &mut B, rm: Rm, w: Width) -> Res<u32> {
        match rm {
            Rm::Reg(r) => Ok(self.read_reg(r, w)),
            Rm::Mem { seg, off, mask } => self.read_mem(bus, seg, off, mask, w),
        }
    }

    #[inline]
    fn write_rm<B: Bus>(&mut self, bus: &mut B, rm: Rm, w: Width, value: u32) -> Res<()> {
        match rm {
            Rm::Reg(r) => {
                self.write_reg(r, w, value);
                Ok(())
            }
            Rm::Mem { seg, off, mask } => self.write_mem(bus, seg, off, mask, w, value),
        }
    }

    /// The linear address of an access of `n` bytes at `off` when its bytes are consecutive (no
    /// wrap within the segment or around 4 GB) and all on one page.
    #[inline]
    fn contiguous(off: u32, base: u32, mask: u32, n: u32) -> Option<u32> {
        let start = off & mask;
        if start.checked_add(n - 1)? > mask {
            return None;
        }
        let linear = base.wrapping_add(start);
        linear.checked_add(n - 1)?;
        ((linear & 0xFFF) + n <= 0x1000).then_some(linear)
    }

    /// Little-endian access; each byte's offset is wrapped with `mask` (so 16-bit word accesses
    /// wrap within the segment, as on the 8086). With paging, each byte's page is translated
    /// (once when they share a page, and then the bus reads them together; without paging the
    /// byte path is the faster one).
    #[inline]
    pub(crate) fn read_mem<B: Bus>(&mut self, bus: &mut B, seg: usize, off: u32, mask: u32, w: Width) -> Res<u32> {
        let base = self.segment_base(seg);
        // In the interpreter's paged copy (see `Bus::PAGED`), the common case inline.
        if B::PAGED
            && self.paging()
            && let Some(linear) = Self::contiguous(off, base, mask, w.bytes())
        {
            let physical = self.translate(bus, linear, false, self.cpl == 3)?;
            return Ok(match w {
                Width::Byte => u32::from(bus.read_u8(physical)),
                Width::Word => u32::from(bus.read_u16(physical)),
                Width::Dword => bus.read_u32(physical),
            });
        }
        if !self.paging() {
            return Ok((0..w.bytes()).fold(0, |value, i| {
                let byte = bus.read_u8(base.wrapping_add(off.wrapping_add(i) & mask));
                value | (u32::from(byte) << (8 * i))
            }));
        }
        self.read_paged(bus, base, off, mask, w)
    }

    // Out of line, so that real-mode code inlines only the byte path.
    #[inline(never)]
    fn read_paged<B: Bus>(&mut self, bus: &mut B, base: u32, off: u32, mask: u32, w: Width) -> Res<u32> {
        if let Some(linear) = Self::contiguous(off, base, mask, w.bytes()) {
            let physical = self.translate(bus, linear, false, self.cpl == 3)?;
            return Ok(match w {
                Width::Byte => u32::from(bus.read_u8(physical)),
                Width::Word => u32::from(bus.read_u16(physical)),
                Width::Dword => bus.read_u32(physical),
            });
        }
        let user = self.cpl == 3;
        let mut value = 0;
        for i in 0..w.bytes() {
            let physical = self.translate(bus, base.wrapping_add(off.wrapping_add(i) & mask), false, user)?;
            value |= u32::from(bus.read_u8(physical)) << (8 * i);
        }
        Ok(value)
    }

    #[inline]
    pub(crate) fn write_mem<B: Bus>(
        &mut self,
        bus: &mut B,
        seg: usize,
        off: u32,
        mask: u32,
        w: Width,
        value: u32,
    ) -> Res<()> {
        let base = self.segment_base(seg);
        if B::PAGED
            && self.paging()
            && let Some(linear) = Self::contiguous(off, base, mask, w.bytes())
        {
            let physical = self.translate(bus, linear, true, self.cpl == 3)?;
            self.wrote(physical);
            match w {
                Width::Byte => bus.write_u8(physical, value as u8),
                Width::Word => bus.write_u16(physical, value as u16),
                Width::Dword => bus.write_u32(physical, value),
            }
            return Ok(());
        }
        if !self.paging() {
            for i in 0..w.bytes() {
                bus.write_u8(base.wrapping_add(off.wrapping_add(i) & mask), (value >> (8 * i)) as u8);
            }
            return Ok(());
        }
        self.write_paged(bus, base, off, mask, w, value)
    }

    #[inline(never)]
    fn write_paged<B: Bus>(&mut self, bus: &mut B, base: u32, off: u32, mask: u32, w: Width, value: u32) -> Res<()> {
        if let Some(linear) = Self::contiguous(off, base, mask, w.bytes()) {
            let physical = self.translate(bus, linear, true, self.cpl == 3)?;
            self.wrote(physical);
            match w {
                Width::Byte => bus.write_u8(physical, value as u8),
                Width::Word => bus.write_u16(physical, value as u16),
                Width::Dword => bus.write_u32(physical, value),
            }
            return Ok(());
        }
        // Translate every byte first, so a fault on the second page writes nothing.
        let user = self.cpl == 3;
        let mut physical = [0u32; 4];
        for i in 0..w.bytes() {
            physical[i as usize] = self.translate(bus, base.wrapping_add(off.wrapping_add(i) & mask), true, user)?;
        }
        for i in 0..w.bytes() {
            self.wrote(physical[i as usize]);
            bus.write_u8(physical[i as usize], (value >> (8 * i)) as u8);
        }
        Ok(())
    }

    /// Push on the stack (SP or ESP by the stack segment's size).
    #[inline]
    pub(crate) fn push<B: Bus>(&mut self, bus: &mut B, w: Width, value: u32) -> Res<()> {
        if self.stack32() {
            let esp = self.regs.gpr[SP].wrapping_sub(w.bytes());
            self.write_mem(bus, SS, esp, u32::MAX, w, value)?;
            self.regs.gpr[SP] = esp;
        } else {
            let sp = self.regs.sp().wrapping_sub(w.bytes() as u16);
            self.write_mem(bus, SS, u32::from(sp), 0xFFFF, w, value)?;
            self.regs.set_sp(sp);
        }
        Ok(())
    }

    #[inline]
    pub(crate) fn pop<B: Bus>(&mut self, bus: &mut B, w: Width) -> Res<u32> {
        if self.stack32() {
            let esp = self.regs.gpr[SP];
            let value = self.read_mem(bus, SS, esp, u32::MAX, w)?;
            self.regs.gpr[SP] = esp.wrapping_add(w.bytes());
            Ok(value)
        } else {
            let sp = self.regs.sp();
            let value = self.read_mem(bus, SS, u32::from(sp), 0xFFFF, w)?;
            self.regs.set_sp(sp.wrapping_add(w.bytes() as u16));
            Ok(value)
        }
    }

    #[inline]
    pub(crate) fn fetch8<B: Bus>(&mut self, bus: &mut B) -> Res<u8> {
        if !B::PAGED {
            let value = bus.fetch_u8(self.fetch.base.wrapping_add(self.regs.ip));
            self.regs.ip = self.regs.ip.wrapping_add(1) & self.fetch.ip_mask;
            return Ok(value);
        }
        let ahead = self.fetch.base.wrapping_add(self.regs.ip).wrapping_sub(self.fetch.buf_linear);
        if ahead < self.fetch.buf_len && self.fetch.buf_user == (self.cpl == 3) {
            self.regs.ip = self.regs.ip.wrapping_add(1) & self.fetch.ip_mask;
            return Ok(self.fetch.buf[ahead as usize]);
        }
        self.fetch8_fill(bus)
    }

    /// With paging: fetches a byte through the bus, and reads ahead up to 16 bytes within the
    /// page when that is plain RAM, which later fetches use (see `FetchState`). (Without
    /// paging, byte fetches are faster.)
    #[inline(never)]
    fn fetch8_fill<B: Bus>(&mut self, bus: &mut B) -> Res<u8> {
        let linear = self.fetch.base.wrapping_add(self.regs.ip);
        let physical = if linear >> 12 == self.fetch.page_tag.wrapping_sub(1) {
            self.fetch.frame | (linear & 0xFFF)
        } else {
            let physical = self.translate(bus, linear, false, self.cpl == 3)?;
            self.fetch.page_tag = (linear >> 12) + 1;
            self.fetch.frame = physical & !0xFFF;
            physical
        };
        let value = match bus.code_window(physical) {
            Some(bytes) => {
                let fetch = &mut self.fetch;
                (fetch.buf, fetch.buf_linear, fetch.buf_len) = (bytes, linear, 16.min(0x1000 - (physical & 0xFFF)));
                (fetch.buf_page, fetch.buf_user) = ((physical >> 12) + 1, self.cpl == 3);
                bytes[0]
            }
            None => bus.fetch_u8(physical),
        };
        self.regs.ip = self.regs.ip.wrapping_add(1) & self.fetch.ip_mask;
        Ok(value)
    }

    #[inline]
    fn fetch16<B: Bus>(&mut self, bus: &mut B) -> Res<u16> {
        let lo = self.fetch8(bus)?;
        let hi = self.fetch8(bus)?;
        Ok(u16::from_le_bytes([lo, hi]))
    }

    #[inline]
    fn fetch32<B: Bus>(&mut self, bus: &mut B) -> Res<u32> {
        let lo = self.fetch16(bus)?;
        let hi = self.fetch16(bus)?;
        Ok(u32::from(lo) | (u32::from(hi) << 16))
    }

    #[inline]
    fn fetch<B: Bus>(&mut self, bus: &mut B, w: Width) -> Res<u32> {
        match w {
            Width::Byte => Ok(u32::from(self.fetch8(bus)?)),
            Width::Word => Ok(u32::from(self.fetch16(bus)?)),
            Width::Dword => self.fetch32(bus),
        }
    }

    /// The 386 accepts LOCK only on the read-modify-write instructions with a memory
    /// destination (ADD..XOR except CMP, the immediate group, XCHG, NOT, NEG, INC, DEC, BTS,
    /// BTR, BTC); anything else raises #UD. The operand bytes are peeked, not consumed.
    fn check_lock<B: Bus>(&mut self, bus: &mut B, op: u8) -> Res<()> {
        let ip = self.regs.ip;
        let second = if op == 0x0F { Some(self.fetch8(bus)?) } else { None };
        let modrm = self.fetch8(bus)?;
        self.regs.ip = ip;
        let reg = (modrm >> 3) & 7;
        let lockable = match (op, second) {
            (0x00..=0x3F, None) => op & 7 < 2 && op >> 3 != 7,
            (0x80..=0x83, None) => reg != 7,
            (0x86 | 0x87, None) => true,
            (0xF6 | 0xF7, None) => reg == 2 || reg == 3,
            (0xFE | 0xFF, None) => reg < 2,
            (0x0F, Some(0xAB | 0xB3 | 0xBB)) => true,
            (0x0F, Some(0xBA)) => reg >= 5,
            _ => false,
        };
        if lockable && modrm >> 6 != 3 { Ok(()) } else { Err(invalid_opcode()) }
    }

    pub(crate) fn unimplemented<B: Bus>(&mut self, bus: &mut B, start_ip: u32) -> Abort {
        self.regs.ip = start_ip;
        let base = self.segment_base(CS);
        let mut bytes = [0; 6];
        for (i, b) in bytes.iter_mut().enumerate() {
            let linear = base.wrapping_add(start_ip.wrapping_add(i as u32));
            let physical =
                if self.paging() { self.translate(bus, linear, false, false).unwrap_or(linear) } else { linear };
            *b = bus.read_u8(physical);
        }
        Exit::Unimplemented { cs: self.regs.seg[CS], ip: start_ip, bytes }.into()
    }
}

fn port_in<B: Bus>(bus: &mut B, port: u16, w: Width) -> u32 {
    match w {
        Width::Byte => u32::from(bus.port_in_u8(port)),
        Width::Word => u32::from(bus.port_in_u16(port)),
        Width::Dword => bus.port_in_u32(port),
    }
}

fn port_out<B: Bus>(bus: &mut B, port: u16, w: Width, value: u32) {
    match w {
        Width::Byte => bus.port_out_u8(port, value as u8),
        Width::Word => bus.port_out_u16(port, value as u16),
        Width::Dword => bus.port_out_u32(port, value),
    }
}
