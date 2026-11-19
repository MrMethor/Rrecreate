//! The cache of decoded instructions (D211): an instruction decoded once is kept by its linear address, with the
//! bus's code stamp of its bytes ([`Bus::code_stamp`]), and runs again from what was decoded, without fetching,
//! parsing its prefixes or its ModR/M byte. The design is Bochs' (decoded instructions validated by stamps of the
//! pages written since) and DOSBox's dynamic core's (code pages that know when they are written); no code was taken
//! from either.
//!
//! Kept to what a fetch would do: only without paging, with the bus's stamps (which change with every write to the
//! bytes, by the CPU or the host), for instructions whose bytes lie in one block the bus stamps and don't wrap at the
//! segment's end. The instructions programs run most have handlers of their own here; each does what the interpreter
//! does, in the same order of reads and writes, through the same helpers. The rest are kept as `Generic` and run by
//! the interpreter, fetched as before.

use super::{ModRm, Rep, Rm, Width};
use crate::flags::{CF, DF, TF, ZF};
use crate::regs::reg::{AX, BP, BX, CX, DI, DX, SI};
use crate::regs::seg::{CS, DS, SS};
use crate::{Bus, Cpu, Exit, Model, Res};

/// Entries in the cache: direct-mapped by linear address.
const ENTRIES: usize = 4096;

/// No register (an addressing form without a base or an index).
const NONE: u8 = 8;

/// The instructions with handlers of their own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// Run by the interpreter.
    Generic,
    /// ALU op `sub` (ADD OR ADC SBB AND SUB XOR CMP): r/m, reg (00-39, forms 0-1).
    AluRmReg,
    /// reg, r/m (forms 2-3).
    AluRegRm,
    /// accumulator, immediate (forms 4-5).
    AluAccImm,
    /// r/m, immediate (80-83, `sub` from ModR/M).
    AluRmImm,
    TestRmReg,
    TestAccImm,
    MovRmReg,
    MovRegRm,
    MovRegImm,
    MovRmImm,
    /// MOV accumulator from memory (A0, A1), and to it (A2, A3).
    MovAccMem,
    MovMemAcc,
    Lea,
    /// MOV Sreg, r/m (8E) and MOV r/m, Sreg (8C): `reg` the segment register.
    MovSegRm,
    MovRmSeg,
    /// Group 2 by `sub`; `count`: the count's source.
    Shift,
    IncReg,
    DecReg,
    /// INC (`sub` 0) or DEC (1) of r/m (FE, FF).
    IncDecRm,
    PushReg,
    PopReg,
    PushImm,
    PushRm,
    /// Jcc (`sub` the condition), JMP, CALL near relative: `imm` the displacement.
    Jcc,
    Jmp,
    Call,
    /// RET near, `imm` bytes released.
    Ret,
    CallRm,
    JmpRm,
    /// LOOPNZ, LOOPZ, LOOP (`sub` the opcode) and JCXZ.
    Loop,
    Jcxz,
    Xchg,
    XchgAcc,
    Xlat,
    /// Group 3 (F6, F7) by `sub`; `imm` TEST's immediate.
    Group3,
    Cbw,
    Cwd,
    Cmc,
    Clc,
    Stc,
    Cld,
    Std,
}

/// Where a shift's count comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Count {
    One,
    Cl,
    Imm,
}

/// An operand as decoded: a register, or a memory address computed from the registers when it runs, without a
/// branch: `((a & a_mask) << a_shift) + ((b & b_mask) << b_shift) + disp`, within 64 KB unless `wide` (32-bit
/// addressing). A register left out has a mask of 0.
#[derive(Clone, Copy, Debug)]
enum Ea {
    Reg(u8),
    Mem { a: u8, b: u8, a_shift: u8, b_shift: u8, seg: u8, wide: bool, a_mask: u32, b_mask: u32, disp: u32 },
}

impl Ea {
    /// 16-bit addressing by the R/M field (0-7: BX+SI ... BX; [`NONE`]: the displacement alone).
    fn mem16(form: u8, seg: u8, disp: u16) -> Ea {
        let (a, b) = match form {
            0 => (Some(BX), Some(SI)),
            1 => (Some(BX), Some(DI)),
            2 => (Some(BP), Some(SI)),
            3 => (Some(BP), Some(DI)),
            4 => (Some(SI), None),
            5 => (Some(DI), None),
            6 => (Some(BP), None),
            7 => (Some(BX), None),
            _ => (None, None),
        };
        let part = |r: Option<usize>| (r.unwrap_or(0) as u8, if r.is_some() { 0xFFFF } else { 0 });
        let ((a, a_mask), (b, b_mask)) = (part(a), part(b));
        Ea::Mem { a, b, a_shift: 0, b_shift: 0, seg, wide: false, a_mask, b_mask, disp: u32::from(disp) }
    }

    /// 32-bit addressing: `index << scale` plus `base << base_shift` (the 386's scale of a base without an index)
    /// plus `disp`; [`NONE`] for a missing base or index.
    fn mem32(base: u8, base_shift: u8, index: u8, scale: u8, seg: u8, disp: u32) -> Ea {
        let part = |r: u8| if r == NONE { (0, 0) } else { (r, u32::MAX) };
        let ((a, a_mask), (b, b_mask)) = (part(index), part(base));
        Ea::Mem { a, b, a_shift: scale, b_shift: base_shift, seg, wide: true, a_mask, b_mask, disp }
    }
}

/// An instruction as decoded.
#[derive(Clone, Copy, Debug)]
struct Decoded {
    /// Where it was: its linear address, the cache's epoch, the code segment's size, and the bus's stamp of its bytes,
    /// and the bus's count of code writes when the stamp was last seen to hold ([`Bus::code_writes`]).
    linear: u32,
    epoch: u32,
    default32: bool,
    stamp: u64,
    seen: Option<u64>,
    len: u8,
    kind: Kind,
    w: Width,
    op32: bool,
    addr32: bool,
    seg: Option<u8>,
    reg: u8,
    sub: u8,
    count: Count,
    ea: Ea,
    imm: u32,
}

impl Decoded {
    const EMPTY: Decoded = Decoded {
        linear: 0,
        epoch: 0,
        default32: false,
        stamp: 0,
        seen: None,
        len: 0,
        kind: Kind::Generic,
        w: Width::Byte,
        op32: false,
        addr32: false,
        seg: None,
        reg: 0,
        sub: 0,
        count: Count::One,
        ea: Ea::Reg(0),
        imm: 0,
    };
}

/// The cache. Scratch data, like the code read ahead: equal and hashed alike whatever it holds, and a clone starts
/// empty.
pub(crate) struct DecodeCache {
    entries: Vec<Decoded>,
    /// Entries of another epoch are forgotten ([`Cpu::forget_decoded`]).
    epoch: u32,
}

impl Default for DecodeCache {
    fn default() -> Self {
        Self { entries: Vec::new(), epoch: 1 }
    }
}

impl std::fmt::Debug for DecodeCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodeCache").field("epoch", &self.epoch).finish_non_exhaustive()
    }
}

impl Clone for DecodeCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for DecodeCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for DecodeCache {}

impl std::hash::Hash for DecodeCache {
    fn hash<H: std::hash::Hasher>(&self, _: &mut H) {}
}

impl DecodeCache {
    pub(crate) fn forget(&mut self) {
        self.epoch = self.epoch.wrapping_add(1).max(1);
    }

    fn slot(linear: u32) -> usize {
        ((linear ^ (linear >> 12)) as usize) & (ENTRIES - 1)
    }
}

/// Reads an instruction's bytes, as the interpreter's fetches would.
struct Reader<'a> {
    bytes: &'a [u8; 16],
    at: usize,
}

impl Reader<'_> {
    fn u8(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes([self.u8()?, self.u8()?]))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from(self.u16()?) | (u32::from(self.u16()?) << 16))
    }

    /// An immediate of width `w`.
    fn imm(&mut self, w: Width) -> Option<u32> {
        match w {
            Width::Byte => self.u8().map(u32::from),
            Width::Word => self.u16().map(u32::from),
            Width::Dword => self.u32(),
        }
    }

    /// A ModR/M operand, as `Cpu::decode_modrm` takes it: the reg field and the operand.
    fn modrm(&mut self, addr32: bool, seg: Option<u8>) -> Option<(u8, Ea)> {
        let byte = self.u8()?;
        let (md, reg, rm) = (byte >> 6, (byte >> 3) & 7, byte & 7);
        if md == 3 {
            return Some((reg, Ea::Reg(rm)));
        }
        if !addr32 {
            let default_seg = match rm {
                2 | 3 => SS,
                6 if md != 0 => SS,
                _ => DS,
            } as u8;
            let (form, disp) = match md {
                0 if rm == 6 => (NONE, self.u16()?),
                0 => (rm, 0),
                1 => (rm, self.u8()? as i8 as u16),
                _ => (rm, self.u16()?),
            };
            return Some((reg, Ea::mem16(form, seg.unwrap_or(default_seg), disp)));
        }
        let mut default_seg = DS as u8;
        let (mut base, mut base_shift, mut index, mut scale) = (NONE, 0, NONE, 0);
        let mut disp32_only = false;
        if rm == 4 {
            let sib = self.u8()?;
            let (s, i, b) = (sib >> 6, (sib >> 3) & 7, sib & 7);
            if i != 4 {
                (index, scale) = (i, s);
            }
            if b == 5 && md == 0 {
                disp32_only = true;
            } else {
                // The 386 scales a base that has no index (`Cpu::decode_modrm`).
                (base, base_shift) = (b, if i == 4 { s } else { 0 });
                if usize::from(b) == crate::regs::reg::SP || usize::from(b) == BP {
                    default_seg = SS as u8;
                }
            }
        } else if rm == 5 && md == 0 {
            disp32_only = true;
        } else {
            base = rm;
            if usize::from(rm) == BP {
                default_seg = SS as u8;
            }
        }
        let disp = if disp32_only || md == 2 {
            self.u32()?
        } else if md == 1 {
            self.u8()? as i8 as i32 as u32
        } else {
            0
        };
        Some((reg, Ea::mem32(base, base_shift, index, scale, seg.unwrap_or(default_seg), disp)))
    }
}

/// Decodes the instruction in `bytes` (at the start of the 16), for a code segment of `default32`; `None` when it
/// has no handler here.
fn decode(bytes: &[u8; 16], default32: bool) -> Option<Decoded> {
    let mut r = Reader { bytes, at: 0 };
    let (mut seg, mut op32, mut addr32, mut rep, mut lock) = (None, default32, default32, Rep::None, false);
    let mut prefixes = 0;
    let op = loop {
        let byte = r.u8()?;
        match byte {
            0x26 => seg = Some(0),
            0x2E => seg = Some(CS as u8),
            0x36 => seg = Some(SS as u8),
            0x3E => seg = Some(DS as u8),
            0x64 => seg = Some(4),
            0x65 => seg = Some(5),
            0x66 => op32 = !default32,
            0x67 => addr32 = !default32,
            0xF0 => lock = true,
            0xF2 => rep = Rep::Repne,
            0xF3 => rep = Rep::Repe,
            _ => break byte,
        }
        prefixes += 1;
        if prefixes > super::MAX_PREFIXES {
            return None;
        }
    };
    // LOCK is checked by the interpreter; a REP prefix changes none of the instructions here.
    let _ = rep;
    if lock {
        return None;
    }
    let v = if op32 { Width::Dword } else { Width::Word };
    let width = |op: u8| if op & 1 == 0 { Width::Byte } else { v };
    let mut d = Decoded { op32, addr32, seg, w: v, ..Decoded::EMPTY };
    let modrm = |r: &mut Reader, d: &mut Decoded| -> Option<()> {
        let (reg, ea) = r.modrm(addr32, seg)?;
        (d.reg, d.ea) = (reg, ea);
        Some(())
    };
    match op {
        0x00..=0x3F if op & 7 < 6 => {
            (d.sub, d.w) = (op >> 3, width(op));
            match op & 7 {
                0 | 1 => {
                    d.kind = Kind::AluRmReg;
                    modrm(&mut r, &mut d)?;
                }
                2 | 3 => {
                    d.kind = Kind::AluRegRm;
                    modrm(&mut r, &mut d)?;
                }
                _ => {
                    d.kind = Kind::AluAccImm;
                    d.imm = r.imm(d.w)?;
                }
            }
        }
        0x40..=0x47 => (d.kind, d.reg) = (Kind::IncReg, op & 7),
        0x48..=0x4F => (d.kind, d.reg) = (Kind::DecReg, op & 7),
        0x50..=0x57 => (d.kind, d.reg) = (Kind::PushReg, op & 7),
        0x58..=0x5F => (d.kind, d.reg) = (Kind::PopReg, op & 7),
        0x68 => (d.kind, d.imm) = (Kind::PushImm, r.imm(v)?),
        0x6A => (d.kind, d.imm) = (Kind::PushImm, r.u8()? as i8 as i32 as u32 & v.mask()),
        0x70..=0x7F => (d.kind, d.sub, d.imm) = (Kind::Jcc, op & 0x0F, r.u8()? as i8 as u32),
        0x80..=0x83 => {
            (d.kind, d.w) = (Kind::AluRmImm, width(op));
            modrm(&mut r, &mut d)?;
            d.sub = d.reg;
            d.imm = if op == 0x83 { r.u8()? as i8 as i32 as u32 & d.w.mask() } else { r.imm(d.w)? };
        }
        0x84 | 0x85 => {
            (d.kind, d.w) = (Kind::TestRmReg, width(op));
            modrm(&mut r, &mut d)?;
        }
        0x86 | 0x87 => {
            (d.kind, d.w) = (Kind::Xchg, width(op));
            modrm(&mut r, &mut d)?;
        }
        0x88 | 0x89 => {
            (d.kind, d.w) = (Kind::MovRmReg, width(op));
            modrm(&mut r, &mut d)?;
        }
        0x8A | 0x8B => {
            (d.kind, d.w) = (Kind::MovRegRm, width(op));
            modrm(&mut r, &mut d)?;
        }
        0x8C => {
            d.kind = Kind::MovRmSeg;
            modrm(&mut r, &mut d)?;
            if d.reg >= 6 {
                return None;
            }
            // A register destination takes the operand size; memory is always 16 bits.
            d.w = if matches!(d.ea, Ea::Reg(_)) { v } else { Width::Word };
        }
        0x8D => {
            d.kind = Kind::Lea;
            modrm(&mut r, &mut d)?;
            if matches!(d.ea, Ea::Reg(_)) {
                return None;
            }
        }
        0x8E => {
            d.kind = Kind::MovSegRm;
            modrm(&mut r, &mut d)?;
            if usize::from(d.reg) == CS || d.reg >= 6 {
                return None;
            }
        }
        0x90..=0x97 => (d.kind, d.reg) = (Kind::XchgAcc, op & 7),
        0x98 => d.kind = Kind::Cbw,
        0x99 => d.kind = Kind::Cwd,
        0xA0..=0xA3 => {
            d.kind = if op < 0xA2 { Kind::MovAccMem } else { Kind::MovMemAcc };
            d.w = width(op);
            let seg = seg.unwrap_or(DS as u8);
            d.ea = if addr32 { Ea::mem32(NONE, 0, NONE, 0, seg, r.u32()?) } else { Ea::mem16(NONE, seg, r.u16()?) };
        }
        0xA8 | 0xA9 => {
            (d.kind, d.w) = (Kind::TestAccImm, width(op));
            d.imm = r.imm(d.w)?;
        }
        0xB0..=0xB7 => (d.kind, d.w, d.reg, d.imm) = (Kind::MovRegImm, Width::Byte, op & 7, u32::from(r.u8()?)),
        0xB8..=0xBF => (d.kind, d.reg, d.imm) = (Kind::MovRegImm, op & 7, r.imm(v)?),
        0xC0 | 0xC1 | 0xD0..=0xD3 => {
            (d.kind, d.w) = (Kind::Shift, width(op));
            modrm(&mut r, &mut d)?;
            d.sub = d.reg;
            d.count = match op {
                0xD0 | 0xD1 => Count::One,
                0xD2 | 0xD3 => Count::Cl,
                _ => {
                    d.imm = u32::from(r.u8()?);
                    Count::Imm
                }
            };
        }
        0xC2 => (d.kind, d.imm) = (Kind::Ret, u32::from(r.u16()?)),
        0xC3 => d.kind = Kind::Ret,
        0xC6 | 0xC7 => {
            (d.kind, d.w) = (Kind::MovRmImm, width(op));
            modrm(&mut r, &mut d)?;
            if d.reg != 0 {
                return None;
            }
            d.imm = r.imm(d.w)?;
        }
        0xD7 => d.kind = Kind::Xlat,
        0xE0..=0xE2 => (d.kind, d.sub, d.imm) = (Kind::Loop, op, r.u8()? as i8 as u32),
        0xE3 => (d.kind, d.imm) = (Kind::Jcxz, r.u8()? as i8 as u32),
        0xE8 | 0xE9 => {
            d.kind = if op == 0xE8 { Kind::Call } else { Kind::Jmp };
            d.imm = if op32 { r.u32()? } else { r.u16()? as i16 as u32 };
        }
        0xEB => (d.kind, d.imm) = (Kind::Jmp, r.u8()? as i8 as u32),
        0xF5 => d.kind = Kind::Cmc,
        0xF6 | 0xF7 => {
            (d.kind, d.w) = (Kind::Group3, width(op));
            modrm(&mut r, &mut d)?;
            d.sub = d.reg;
            if d.reg <= 1 {
                d.imm = r.imm(d.w)?;
            }
        }
        0xF8 => d.kind = Kind::Clc,
        0xF9 => d.kind = Kind::Stc,
        0xFC => d.kind = Kind::Cld,
        0xFD => d.kind = Kind::Std,
        0xFE | 0xFF => {
            d.w = width(op);
            modrm(&mut r, &mut d)?;
            d.sub = d.reg;
            d.kind = match (op, d.reg) {
                (_, 0 | 1) => Kind::IncDecRm,
                (0xFF, 2) => Kind::CallRm,
                (0xFF, 4) => Kind::JmpRm,
                (0xFF, 6) => Kind::PushRm,
                // The host's callback (FE 38), the far forms, and invalid encodings.
                _ => return None,
            };
        }
        0x0F => match r.u8()? {
            op2 @ 0x80..=0x8F => {
                (d.kind, d.sub) = (Kind::Jcc, op2 & 0x0F);
                d.imm = if op32 { r.u32()? } else { r.u16()? as i16 as u32 };
            }
            _ => return None,
        },
        _ => return None,
    }
    d.len = r.at as u8;
    Some(d)
}

/// Why [`Cpu::run_batch`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Batch {
    /// The cycle counter reached the limit, or an instruction raised an exception, now entered: the host looks again
    /// before the next.
    Again,
    /// The next instruction is the host's to run, with [`Cpu::step`]: the stop said so, or it isn't one the batch
    /// runs.
    Step,
}

impl Cpu {
    /// Runs decoded instructions one after another, each as [`Cpu::step`] would, until the cycle counter reaches
    /// `until`, `before` (asked before each with its linear address, the host's look before an instruction) says to
    /// stop, or the next instruction isn't one the batch runs: one without a handler in the cache, or after one that
    /// inhibits interrupts. `after` is the host's look after each instruction that ran. None of these
    /// instructions does I/O, changes the code segment or the flags IF and TF, or can stop for the host: a host loop
    /// with nothing else to look at between them (no interrupt deliverable, no device due before `until`) runs them
    /// so, without its own look at each (D211). Runs nothing unless in real or virtual-8086 addressing without paging,
    /// with TF and the interrupt inhibit clear.
    pub fn run_batch<B: Bus>(
        &mut self,
        bus: &mut B,
        until: u64,
        mut before: impl FnMut(&Cpu, &mut B, u32) -> bool,
        mut after: impl FnMut(&Cpu, &mut B),
    ) -> Result<Batch, Exit> {
        if !self.batches() {
            return Ok(Batch::Step);
        }
        let cs_base = self.segment_base(CS);
        loop {
            if self.cycles >= until {
                return Ok(Batch::Again);
            }
            let start_ip = self.regs.ip;
            // Decoded before the host's look, which the host takes only for an instruction that runs.
            let Some(d) = self.decoded_at(bus, start_ip) else { return Ok(Batch::Step) };
            if before(self, bus, cs_base.wrapping_add(start_ip)) {
                return Ok(Batch::Step);
            }
            self.irq_inhibit = false;
            let result = self.run_decoded(bus, &d, start_ip);
            if result.is_err() {
                let concluded = self.conclude(bus, result, start_ip, None, false);
                after(self, bus);
                return concluded.map(|()| Batch::Again);
            }
            self.cycles += 1;
            after(self, bus);
            if self.irq_inhibit {
                return Ok(Batch::Again);
            }
        }
    }

    /// Whether [`Cpu::run_batch`] would run anything now: a host asks before setting one up.
    #[inline]
    pub fn batches(&self) -> bool {
        self.model >= Model::I386
            && self.real_addressing()
            && !self.paging()
            && !self.regs.flag(TF)
            && !self.irq_inhibit
    }

    /// Runs the instruction at CS:`start_ip` from the cache when it can, else through the interpreter.
    #[inline(always)]
    pub(crate) fn exec_decoded<B: Bus>(&mut self, bus: &mut B, start_ip: u32) -> Res<()> {
        match self.decoded_at(bus, start_ip) {
            Some(d) => self.run_decoded(bus, &d, start_ip),
            None => self.exec(bus, start_ip),
        }
    }

    /// The instruction at CS:`start_ip` as decoded, if it has a handler here and its bytes are cached.
    #[inline(always)]
    fn decoded_at<B: Bus>(&mut self, bus: &mut B, start_ip: u32) -> Option<Decoded> {
        if self.model < Model::I386 {
            return None;
        }
        let default32 = self.code32();
        let linear = self.segment_base(CS).wrapping_add(start_ip);
        let slot = DecodeCache::slot(linear);
        let epoch = self.decoded.epoch;
        let writes = bus.code_writes();
        if let Some(d) = self.decoded.entries.get_mut(slot)
            && d.linear == linear
            && d.epoch == epoch
            && d.default32 == default32
        {
            // No code written since its stamp was last seen: it holds. Else it is asked for again.
            if writes.is_some() && d.seen == writes {
                return (d.kind != Kind::Generic).then_some(*d);
            }
            if bus.code_stamp(linear, u32::from(d.len.max(1))) == Some(d.stamp) {
                d.seen = writes;
                return (d.kind != Kind::Generic).then_some(*d);
            }
        }
        self.decode_at(bus, start_ip, linear, default32, writes)
    }

    /// Decodes the instruction at CS:`start_ip` and keeps it.
    #[cold]
    #[inline(never)]
    fn decode_at<B: Bus>(
        &mut self,
        bus: &mut B,
        start_ip: u32,
        linear: u32,
        default32: bool,
        seen: Option<u64>,
    ) -> Option<Decoded> {
        // A bus that keeps no stamps there: nothing is decoded.
        bus.code_stamp(linear, 1)?;
        let ip_mask = if default32 { u32::MAX } else { 0xFFFF };
        let decoded = match bus.code_window(linear) {
            // Its bytes in order at `linear`: not wrapping at the segment's end.
            Some(bytes) if start_ip <= ip_mask => {
                decode(&bytes, default32).filter(|d| u64::from(start_ip) + u64::from(d.len) <= u64::from(ip_mask) + 1)
            }
            _ => None,
        };
        // An instruction without a handler is kept too, so that it isn't decoded again (with its first byte's stamp).
        let (mut d, len) = match decoded {
            Some(d) => (d, u32::from(d.len)),
            None => (Decoded::EMPTY, 1),
        };
        let stamp = bus.code_stamp(linear, len);
        let Some(stamp) = stamp else {
            // Its bytes aren't the bus's ordinary memory, or lie in two of its blocks: run as fetched, kept as such.
            let kept = bus.code_stamp(linear, 1)?;
            self.keep(Decoded { linear, epoch: self.decoded.epoch, default32, stamp: kept, seen, ..Decoded::EMPTY });
            return None;
        };
        (d.linear, d.epoch, d.default32, d.stamp, d.seen) = (linear, self.decoded.epoch, default32, stamp, seen);
        self.keep(d);
        (d.kind != Kind::Generic).then_some(d)
    }

    fn keep(&mut self, d: Decoded) {
        let cache = &mut self.decoded;
        if cache.entries.is_empty() {
            cache.entries = vec![Decoded::EMPTY; ENTRIES];
        }
        cache.entries[DecodeCache::slot(d.linear)] = d;
    }

    /// The operand as the interpreter's `decode_modrm` gives it, from the registers now.
    #[inline(always)]
    fn operand(&self, ea: Ea) -> Rm {
        match ea {
            Ea::Reg(r) => Rm::Reg(r),
            Ea::Mem { a, b, a_shift, b_shift, seg, wide, a_mask, b_mask, disp } => {
                let g = &self.regs.gpr;
                let mask = if wide { u32::MAX } else { 0xFFFF };
                let a = (g[usize::from(a & 7)] & a_mask) << (a_shift & 31);
                let b = (g[usize::from(b & 7)] & b_mask) << (b_shift & 31);
                Rm::Mem { seg: usize::from(seg & 7), off: a.wrapping_add(b).wrapping_add(disp) & mask, mask }
            }
        }
    }

    /// Runs a decoded instruction: what the interpreter does for it, its fetches done.
    #[inline(always)]
    fn run_decoded<B: Bus>(&mut self, bus: &mut B, d: &Decoded, start_ip: u32) -> Res<()> {
        let ip_mask = if d.default32 { u32::MAX } else { 0xFFFF };
        self.regs.ip = start_ip.wrapping_add(u32::from(d.len)) & ip_mask;
        let w = d.w;
        match d.kind {
            Kind::Generic => unreachable!("run by the interpreter"),
            Kind::AluRmReg => {
                let rm = self.operand(d.ea);
                let a = self.read_rm(bus, rm, w)?;
                let b = self.read_reg(d.reg, w);
                let r = self.alu(d.sub, w, a, b);
                if d.sub != 7 {
                    self.write_rm(bus, rm, w, r)?;
                }
            }
            Kind::AluRegRm => {
                let rm = self.operand(d.ea);
                let a = self.read_reg(d.reg, w);
                let b = self.read_rm(bus, rm, w)?;
                let r = self.alu(d.sub, w, a, b);
                if d.sub != 7 {
                    self.write_reg(d.reg, w, r);
                }
            }
            Kind::AluAccImm => {
                let a = self.read_reg(0, w);
                let r = self.alu(d.sub, w, a, d.imm);
                if d.sub != 7 {
                    self.write_reg(0, w, r);
                }
            }
            Kind::AluRmImm => {
                let rm = self.operand(d.ea);
                let a = self.read_rm(bus, rm, w)?;
                let r = self.alu(d.sub, w, a, d.imm);
                if d.sub != 7 {
                    self.write_rm(bus, rm, w, r)?;
                }
            }
            Kind::TestRmReg => {
                let rm = self.operand(d.ea);
                let a = self.read_rm(bus, rm, w)?;
                let b = self.read_reg(d.reg, w);
                self.logic(w, a & b);
            }
            Kind::TestAccImm => {
                let a = self.read_reg(0, w);
                self.logic(w, a & d.imm);
            }
            Kind::MovRmReg => {
                let rm = self.operand(d.ea);
                let value = self.read_reg(d.reg, w);
                self.write_rm(bus, rm, w, value)?;
            }
            Kind::MovRegRm => {
                let rm = self.operand(d.ea);
                let value = self.read_rm(bus, rm, w)?;
                self.write_reg(d.reg, w, value);
            }
            Kind::MovRegImm => self.write_reg(d.reg, w, d.imm),
            Kind::MovRmImm => {
                let rm = self.operand(d.ea);
                self.write_rm(bus, rm, w, d.imm)?;
            }
            Kind::MovAccMem => {
                let rm = self.operand(d.ea);
                let value = self.read_rm(bus, rm, w)?;
                self.write_reg(0, w, value);
            }
            Kind::MovMemAcc => {
                let rm = self.operand(d.ea);
                let value = self.read_reg(0, w);
                self.write_rm(bus, rm, w, value)?;
            }
            Kind::Lea => {
                if let Rm::Mem { off, .. } = self.operand(d.ea) {
                    self.write_reg(d.reg, w, off & w.mask());
                }
            }
            Kind::MovSegRm => {
                let rm = self.operand(d.ea);
                let target = usize::from(d.reg);
                let value = self.read_rm(bus, rm, Width::Word)? as u16;
                self.load_segment(bus, target, value)?;
                if target == SS {
                    self.irq_inhibit = true;
                }
            }
            Kind::MovRmSeg => {
                let rm = self.operand(d.ea);
                let value = u32::from(self.regs.seg[usize::from(d.reg)]);
                self.write_rm(bus, rm, w, value)?;
            }
            Kind::Shift => {
                let rm = self.operand(d.ea);
                let count = match d.count {
                    Count::One => 1,
                    Count::Cl => self.regs.cl(),
                    Count::Imm => d.imm as u8,
                };
                if count & 0x1F != 0 {
                    let value = self.read_rm(bus, rm, w)?;
                    let result = self.shift(d.sub, w, value, count);
                    self.write_rm(bus, rm, w, result)?;
                }
            }
            Kind::IncReg | Kind::DecReg => {
                let value = self.read_reg(d.reg, w);
                let cf = self.regs.flag(CF);
                let result = if d.kind == Kind::IncReg { self.add(w, value, 1, 0) } else { self.sub(w, value, 1, 0) };
                self.regs.set_flag(CF, cf);
                self.write_reg(d.reg, w, result);
            }
            Kind::IncDecRm => {
                let rm = self.operand(d.ea);
                let value = self.read_rm(bus, rm, w)?;
                let cf = self.regs.flag(CF);
                let result = if d.sub == 0 { self.add(w, value, 1, 0) } else { self.sub(w, value, 1, 0) };
                self.regs.set_flag(CF, cf);
                self.write_rm(bus, rm, w, result)?;
            }
            Kind::PushReg => {
                // PUSH SP pushes the value before the push (80286 and later).
                let value = self.read_reg(d.reg, w);
                self.push(bus, w, value)?;
            }
            Kind::PopReg => {
                let value = self.pop(bus, w)?;
                self.write_reg(d.reg, w, value);
            }
            Kind::PushImm => self.push(bus, w, d.imm)?,
            Kind::PushRm => {
                let rm = self.operand(d.ea);
                let value = self.read_rm(bus, rm, w)?;
                self.push(bus, w, value)?;
            }
            Kind::Jcc => {
                if self.condition(d.sub) {
                    self.jump_relative(d.imm, d.op32);
                }
            }
            Kind::Jmp => self.jump_relative(d.imm, d.op32),
            Kind::Call => {
                let ip = self.regs.ip;
                self.push(bus, w, ip)?;
                self.jump_relative(d.imm, d.op32);
            }
            Kind::Ret => {
                let ip = self.pop(bus, w)?;
                self.regs.ip = if d.op32 { ip } else { ip & 0xFFFF };
                self.release_stack(d.imm);
            }
            Kind::CallRm => {
                let rm = self.operand(d.ea);
                let target = self.read_rm(bus, rm, w)?;
                let ip = self.regs.ip;
                self.push(bus, w, ip)?;
                self.regs.ip = target;
            }
            Kind::JmpRm => {
                let rm = self.operand(d.ea);
                self.regs.ip = self.read_rm(bus, rm, w)?;
            }
            Kind::Loop => {
                let mask = if d.addr32 { u32::MAX } else { 0xFFFF };
                let count = (self.regs.gpr[CX] & mask).wrapping_sub(1) & mask;
                if d.addr32 {
                    self.regs.gpr[CX] = count;
                } else {
                    self.regs.set_cx(count as u16);
                }
                let taken = count != 0
                    && match d.sub {
                        0xE0 => !self.regs.flag(ZF),
                        0xE1 => self.regs.flag(ZF),
                        _ => true,
                    };
                if taken {
                    self.jump_relative(d.imm, d.op32);
                }
            }
            Kind::Jcxz => {
                let mask = if d.addr32 { u32::MAX } else { 0xFFFF };
                if self.regs.gpr[CX] & mask == 0 {
                    self.jump_relative(d.imm, d.op32);
                }
            }
            Kind::Xchg => {
                let rm = self.operand(d.ea);
                let a = self.read_rm(bus, rm, w)?;
                let b = self.read_reg(d.reg, w);
                self.write_rm(bus, rm, w, b)?;
                self.write_reg(d.reg, w, a);
            }
            Kind::XchgAcc => {
                let (a, b) = (self.read_reg(0, w), self.read_reg(d.reg, w));
                self.write_reg(0, w, b);
                self.write_reg(d.reg, w, a);
            }
            Kind::Xlat => {
                let mask = if d.addr32 { u32::MAX } else { 0xFFFF };
                let off = (self.regs.gpr[BX] & mask).wrapping_add(u32::from(self.regs.al()));
                let rm = Rm::Mem { seg: usize::from(d.seg.unwrap_or(DS as u8)), off, mask };
                let value = self.read_rm(bus, rm, Width::Byte)?;
                self.regs.set_al(value as u8);
            }
            Kind::Group3 => {
                let rm = self.operand(d.ea);
                let imm = d.imm;
                self.group3_on(bus, w, ModRm { reg: d.sub, rm }, |_, _| Ok(imm))?;
            }
            Kind::Cbw => match w {
                Width::Dword => self.regs.gpr[AX] = i32::from(self.regs.ax() as i16) as u32,
                _ => self.regs.set_ax(i16::from(self.regs.al() as i8) as u16),
            },
            Kind::Cwd => match w {
                Width::Dword => self.regs.gpr[DX] = if self.regs.gpr[AX] & 0x8000_0000 != 0 { u32::MAX } else { 0 },
                _ => self.regs.set_dx(if self.regs.ax() & 0x8000 != 0 { 0xFFFF } else { 0 }),
            },
            Kind::Cmc => self.regs.flags ^= u32::from(CF),
            Kind::Clc => self.regs.set_flag(CF, false),
            Kind::Stc => self.regs.set_flag(CF, true),
            Kind::Cld => self.regs.set_flag(DF, false),
            Kind::Std => self.regs.set_flag(DF, true),
        }
        Ok(())
    }
}
