//! Contract inference: what each function of the program reads, returns and keeps, from what
//! its calls did during a run, so a calling convention needn't be guessed (hand-written assembly
//! is common in DOS games).
//!
//! Every instruction is decoded (iced-x86's instruction info: the registers and flags it reads
//! and writes) and every data access is traced. A `CALL` opens a frame, which closes when the
//! stack pointer rises above the return address. Per function, over all its calls:
//! - inputs: registers and flags read before anything in the call wrote them, and the stack
//!   argument bytes read. A register only pushed (saved) counts if the call didn't restore it;
//! - outputs: registers a call changed, and flags it set, that the code after it read before
//!   writing (a flag set to the value it had is a result too: `CLC` returning a clear carry);
//! - preserved: registers equal at every return;
//! - the bytes popped besides the return address, and the return (near, far, IRET);
//! - memory it read before writing and memory it wrote, itself (not its callees): load-image
//!   offsets, video memory, BIOS data, other memory; port I/O; `INT` instructions (vector, AH).
//!
//! Interrupt handlers (hardware interrupts, `INT` instructions) and calls out of the program (to
//! drivers and ROM) are opaque: what happens inside isn't the caller's doing, and the registers
//! they change count as written when they return. A host service's register reads aren't seen,
//! except AX. Real mode only. A debugging aid like [`crate::coverage`]: not part of the
//! machine's state or hash.

use std::collections::{BTreeMap, BTreeSet};

use crate::memory::Memory;
use cpu::Regs;
use iced_x86::{
    Decoder, DecoderOptions, FlowControl, InstructionInfoFactory, InstructionInfoOptions, Mnemonic, OpAccess, OpKind,
    Register, RflagsBits,
};

use crate::probe::Access;

/// Registers and flags as bits ("units"): for AX, CX, DX, BX, SP, BP, SI, DI (`cpu::Regs`
/// order) the low byte, high byte and upper half (bits 3r, 3r+1, 3r+2); ES, CS, SS, DS, FS, GS
/// (bits 24-29); CF, PF, AF, ZF, SF, IF, DF, OF (bits 32-39).
pub mod units {
    pub const fn low(r: usize) -> u64 {
        1 << (3 * r)
    }
    pub const fn high(r: usize) -> u64 {
        1 << (3 * r + 1)
    }
    pub const fn upper(r: usize) -> u64 {
        1 << (3 * r + 2)
    }
    pub const fn word(r: usize) -> u64 {
        low(r) | high(r)
    }
    pub const fn seg(s: usize) -> u64 {
        1 << (24 + s)
    }
    pub const fn flag(f: usize) -> u64 {
        1 << (32 + f)
    }
    pub const COUNT: usize = 40;
    pub const FLAGS: u64 = 0xFF << 32;
    pub const FLAG_NAMES: [&str; 8] = ["CF", "PF", "AF", "ZF", "SF", "IF", "DF", "OF"];
    /// The EFLAGS bit of each flag unit.
    pub const FLAG_BITS: [u32; 8] = [0, 2, 4, 6, 7, 9, 10, 11];
    /// SP, CS and SS belong to the call mechanism, not to a contract.
    pub const STRUCTURAL: u64 = word(4) | upper(4) | seg(1) | seg(2);
}

use units::{COUNT, FLAG_BITS, FLAG_NAMES, FLAGS, STRUCTURAL};

/// Ways a function returned, as bits.
pub const NEAR: u8 = 1;
pub const FAR: u8 = 2;
pub const IRET: u8 = 4;

/// Addresses traced: the real-mode space.
const SPACE: usize = crate::addr::REAL_MODE_SPACE;
/// Bytes above the return address looked at for stack arguments.
const ARGS_WINDOW: u32 = 64;
/// Most load-image bytes listed per function, read and written each.
pub const MAX_BYTES: usize = 4096;

/// What a function's calls did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    /// Calls that returned.
    pub calls: u64,
    /// Calls left otherwise (the stack unwound past them without a return).
    pub abnormal: u64,
    pub inputs: u64,
    pub outputs: u64,
    /// Units equal at every return.
    pub preserved: u64,
    /// Bytes popped besides the return address.
    pub pops: BTreeSet<u16>,
    pub returns: u8,
    /// Bytes of stack arguments read (above the return address), at most.
    pub arg_bytes: u16,
    /// Load-image offsets read before being written, and written.
    pub reads: BTreeSet<u32>,
    pub writes: BTreeSet<u32>,
    /// More than [`MAX_BYTES`] were read or written.
    pub capped: bool,
    pub vram: bool,
    pub ports: bool,
    pub bios_data: bool,
    pub far_memory: bool,
    /// `INT` instructions it executed: (vector, AH).
    pub services: BTreeSet<(u8, u8)>,
}

impl Default for Contract {
    fn default() -> Self {
        Contract {
            calls: 0,
            abnormal: 0,
            inputs: 0,
            outputs: 0,
            preserved: !0,
            pops: BTreeSet::new(),
            returns: 0,
            arg_bytes: 0,
            reads: BTreeSet::new(),
            writes: BTreeSet::new(),
            capped: false,
            vram: false,
            ports: false,
            bios_data: false,
            far_memory: false,
            services: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Other,
    Call,
    Return(u8),
    Interrupt(u8),
}

/// How an instruction reads the stack back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pop {
    No,
    /// POP of one register (or LEAVE): these units.
    Register(u64),
    /// POPA, POPF.
    Other,
}

/// An instruction's register use, decoded once per address.
#[derive(Debug, Clone, Copy)]
struct Decoded {
    bytes: [u8; 15],
    len: u8,
    reads: u64,
    /// Read only to be saved on the stack (PUSH, PUSHA, PUSHF, ENTER).
    saves: u64,
    writes: u64,
    kind: Kind,
    /// PUSH of one register: its stack slot is followed (see `Contracts::slots`).
    push: bool,
    pop: Pop,
}

fn register_units(r: Register) -> u64 {
    let n = r.number();
    if r.is_gpr8() {
        if n < 4 { units::low(n) } else { units::high(n - 4) }
    } else if r.is_gpr16() {
        units::word(n)
    } else if r.is_gpr32() {
        units::word(n) | units::upper(n)
    } else if r.is_segment_register() {
        units::seg(n)
    } else {
        0
    }
}

fn flag_units(bits: u32) -> u64 {
    const ICED: [u32; 8] = [
        RflagsBits::CF,
        RflagsBits::PF,
        RflagsBits::AF,
        RflagsBits::ZF,
        RflagsBits::SF,
        RflagsBits::IF,
        RflagsBits::DF,
        RflagsBits::OF,
    ];
    ICED.iter().enumerate().filter(|&(_, &b)| bits & b != 0).fold(0, |m, (i, _)| m | units::flag(i))
}

fn decode(bytes: [u8; 15], ip: u16) -> Decoded {
    let instr = Decoder::with_ip(16, &bytes, u64::from(ip), DecoderOptions::NONE).decode();
    let mut d = Decoded {
        bytes,
        len: instr.len() as u8,
        reads: 0,
        saves: 0,
        writes: 0,
        kind: Kind::Other,
        push: false,
        pop: Pop::No,
    };
    if instr.is_invalid() {
        // Host callbacks and the like.
        d.len = 1;
        return d;
    }
    let saves_all = matches!(
        instr.mnemonic(),
        Mnemonic::Pusha | Mnemonic::Pushad | Mnemonic::Pushf | Mnemonic::Pushfd | Mnemonic::Enter
    );
    let saved =
        (instr.mnemonic() == Mnemonic::Push && instr.op0_kind() == OpKind::Register).then(|| instr.op0_register());
    d.push = saved.is_some();
    let mut factory = InstructionInfoFactory::new();
    let info = factory.info_options(&instr, InstructionInfoOptions::NO_MEMORY_USAGE);
    for used in info.used_registers() {
        let u = register_units(used.register());
        let read = if saves_all || saved == Some(used.register()) { &mut d.saves } else { &mut d.reads };
        match used.access() {
            OpAccess::Read | OpAccess::CondRead | OpAccess::ReadCondWrite => *read |= u,
            OpAccess::ReadWrite => {
                *read |= u;
                d.writes |= u;
            }
            OpAccess::Write => d.writes |= u,
            _ => {}
        }
    }
    let flags_read = flag_units(instr.rflags_read());
    if saves_all {
        d.saves |= flags_read;
    } else {
        d.reads |= flags_read;
    }
    d.writes |= flag_units(instr.rflags_modified());
    d.pop = match instr.mnemonic() {
        Mnemonic::Pop if instr.op0_kind() == OpKind::Register => Pop::Register(d.writes & !STRUCTURAL & !FLAGS),
        Mnemonic::Leave => Pop::Register(d.writes & !STRUCTURAL & !FLAGS),
        Mnemonic::Popa | Mnemonic::Popad | Mnemonic::Popf | Mnemonic::Popfd => Pop::Other,
        _ => Pop::No,
    };
    d.kind = match instr.flow_control() {
        FlowControl::Call | FlowControl::IndirectCall => Kind::Call,
        FlowControl::Return => Kind::Return(match instr.mnemonic() {
            Mnemonic::Retf => FAR,
            Mnemonic::Iret | Mnemonic::Iretd => IRET,
            _ => NEAR,
        }),
        FlowControl::Interrupt => {
            // The service's selector (AH) and usual argument (AL).
            d.reads |= units::word(0);
            Kind::Interrupt(match instr.mnemonic() {
                Mnemonic::Int => instr.immediate8(),
                Mnemonic::Int1 => 1,
                Mnemonic::Int3 => 3,
                Mnemonic::Into => 4,
                _ => 0,
            })
        }
        _ => Kind::Other,
    };
    d
}

/// The value of unit `u`.
fn unit_value(regs: &Regs, u: usize) -> u32 {
    match u {
        0..24 => {
            let v = regs.gpr[u / 3];
            match u % 3 {
                0 => v & 0xFF,
                1 => v >> 8 & 0xFF,
                _ => v >> 16,
            }
        }
        24..30 => u32::from(regs.seg[u - 24]),
        32..40 => regs.flags >> FLAG_BITS[u - 32] & 1,
        _ => 0,
    }
}

fn changed_units(a: &Regs, b: &Regs) -> u64 {
    (0..COUNT).filter(|&u| unit_value(a, u) != unit_value(b, u)).fold(0, |m, u| m | 1 << u)
}

fn bits(mut mask: u64) -> impl Iterator<Item = usize> {
    std::iter::from_fn(move || {
        (mask != 0).then(|| {
            let u = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            u
        })
    })
}

const GPR: [&str; 8] = ["AX", "CX", "DX", "BX", "SP", "BP", "SI", "DI"];
const SEG: [&str; 6] = ["ES", "CS", "SS", "DS", "FS", "GS"];

/// Register names for the units in `mask`: `AX`, `AL`, `AH`, `EAX`, `DS`, ...
pub fn register_names(mask: u64) -> Vec<String> {
    let mut out = Vec::new();
    for (r, name) in GPR.iter().enumerate() {
        let (l, h, u) = (mask & units::low(r) != 0, mask & units::high(r) != 0, mask & units::upper(r) != 0);
        let letter = &name[..1];
        let word = match (l, h) {
            (true, true) => Some(name.to_string()),
            (true, false) if r < 4 => Some(format!("{letter}L")),
            (false, true) if r < 4 => Some(format!("{letter}H")),
            (false, false) => None,
            _ => Some(name.to_string()),
        };
        match (u, word) {
            (true, Some(_)) if l && h => out.push(format!("E{name}")),
            (true, word) => out.extend(word.into_iter().chain([format!("E{name}:high")])),
            (false, word) => out.extend(word),
        }
    }
    out.extend(SEG.iter().enumerate().filter(|&(s, _)| mask & units::seg(s) != 0).map(|(_, n)| n.to_string()));
    out
}

pub fn flag_names(mask: u64) -> Vec<&'static str> {
    FLAG_NAMES.iter().enumerate().filter(|&(f, _)| mask & units::flag(f) != 0).map(|(_, &n)| n).collect()
}

fn return_names(returns: u8) -> String {
    let names = [(NEAR, "near"), (FAR, "far"), (IRET, "iret")];
    names.iter().filter(|&&(bit, _)| returns & bit != 0).map(|&(_, n)| n).collect::<Vec<_>>().join("/")
}

/// What a frame closes over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    /// A function of the program.
    Function,
    /// An `INT` instruction, or a call out of the program (a driver, the ROM): its register
    /// changes are results.
    Service,
    /// A hardware interrupt: nothing it does is the interrupted code's.
    Hardware,
}

#[derive(Debug, Clone)]
struct Frame {
    kind: FrameKind,
    /// Linear address called.
    entry: u32,
    ss: u16,
    /// SP with the return address on top.
    sp: u16,
    /// Bytes the call pushed.
    pushed: u16,
    /// Sequence number of the first instruction inside.
    start: u64,
    /// Registers when it was entered.
    regs: Regs,
    inputs: u64,
    saves: u64,
    /// Bytes above `sp` it read (return address and arguments), at most.
    stack_top: u16,
    /// When it was entered: the register write sequence numbers and value producers.
    reg_seq: Box<[u64; COUNT]>,
    producer: Box<[Option<u32>; COUNT]>,
}

/// The instruction about to run.
#[derive(Debug, Clone, Copy)]
struct Pre {
    at: usize,
    regs: Regs,
}

/// A register unit pushed on the stack.
#[derive(Debug, Clone, Copy)]
struct Slot {
    /// Linear address of the pushed word.
    addr: u32,
    unit: usize,
    /// The unit's write sequence number and producer when pushed.
    seq: u64,
    producer: Option<u32>,
    /// Frames then.
    depth: usize,
}

/// Most pushed units followed at once.
const MAX_SLOTS: usize = 64;

/// Units a host service or driver may have written even when the value is the same: AX and CF
/// carry their results.
const SERVICE_RESULTS: u64 = units::word(0) | units::flag(0);

/// Contract inference over a run, for the program loaded at `base`.
#[derive(Debug, Clone)]
pub struct Contracts {
    /// Linear address and size of the program's memory block.
    pub base: u32,
    pub size: u32,
    /// By load-image offset of the function.
    pub functions: BTreeMap<u32, Contract>,
    frames: Vec<Frame>,
    /// Interrupt handlers and services running (their frames are the innermost).
    opaque: usize,
    /// Instructions executed (the current one's number).
    seq: u64,
    /// Sequence number of the last write of each unit, and of each byte.
    reg_seq: [u64; COUNT],
    mem_seq: Vec<u64>,
    /// Per unit: the function (load-image offset) whose call left its value; reading the value
    /// makes the unit an output of that function.
    producer: [Option<u32>; COUNT],
    /// Registers pushed on the stack. Popped back, a register is as it was (a save and
    /// restore); read there otherwise (an argument), it is used.
    slots: Vec<Slot>,
    /// Linear address → 1 + index in `decoded`.
    index: Vec<u32>,
    decoded: Vec<Decoded>,
    pub(crate) log: Vec<Access>,
    pre: Option<Pre>,
}

impl Contracts {
    pub fn new(base: u32, size: u32) -> Self {
        Contracts {
            base,
            size,
            functions: BTreeMap::new(),
            frames: Vec::new(),
            opaque: 0,
            seq: 0,
            reg_seq: [0; COUNT],
            mem_seq: vec![0; SPACE],
            producer: [None; COUNT],
            slots: Vec::new(),
            index: vec![0; SPACE],
            decoded: Vec::new(),
            log: Vec::new(),
            pre: None,
        }
    }

    fn offset(&self, linear: u32) -> Option<u32> {
        linear.checked_sub(self.base).filter(|&offset| offset < self.size)
    }

    /// Whether the next step's data accesses are wanted.
    pub(crate) fn tracing(&self) -> bool {
        self.pre.is_some()
    }

    /// Before an instruction at `linear` runs (real mode).
    pub(crate) fn before(&mut self, regs: &Regs, mem: &Memory, linear: u32) {
        self.log.clear();
        self.pre = None;
        let at = linear as usize;
        if at + 15 > SPACE {
            return;
        }
        let bytes: [u8; 15] = mem.slice(linear, 15).try_into().expect("15 bytes");
        let slot = self.index[at] as usize;
        let i = match slot.checked_sub(1) {
            Some(i)
                if self.decoded[i].bytes[..usize::from(self.decoded[i].len)]
                    == bytes[..usize::from(self.decoded[i].len)] =>
            {
                i
            }
            Some(i) => {
                self.decoded[i] = decode(bytes, regs.ip as u16);
                i
            }
            None => {
                self.decoded.push(decode(bytes, regs.ip as u16));
                self.index[at] = self.decoded.len() as u32;
                self.decoded.len() - 1
            }
        };
        self.pre = Some(Pre { at: i, regs: *regs });
    }

    /// After the instruction ran (or stopped for a host callback).
    pub(crate) fn after(&mut self, regs: &Regs) {
        let Some(pre) = self.pre.take() else { return };
        self.seq += 1;
        let d = self.decoded[pre.at];
        // Register reads first: LEAVE reads BP before popping it back.
        for u in bits(d.reads & !STRUCTURAL) {
            self.read(u, false);
        }
        let restored = self.memory(&pre.regs, regs, d.pop);
        if !d.push {
            for u in bits(d.saves & !STRUCTURAL) {
                self.read(u, true);
            }
        } else if self.opaque == 0 {
            let addr = (u32::from(regs.ss()) << 4) + u32::from(regs.sp());
            for unit in bits(d.saves & !STRUCTURAL) {
                if self.slots.len() == MAX_SLOTS {
                    self.slots.remove(0);
                }
                let (seq, producer) = (self.reg_seq[unit], self.producer[unit]);
                self.slots.push(Slot { addr, unit, seq, producer, depth: self.frames.len() });
            }
        }
        for u in bits(d.writes & !STRUCTURAL & !restored) {
            self.write(u);
        }
        let same_stack = regs.ss() == pre.regs.ss();
        let pushed = pre.regs.sp().wrapping_sub(regs.sp());
        match d.kind {
            Kind::Call if same_stack && matches!(pushed, 2 | 4 | 8) => {
                let entry = (u32::from(regs.cs()) << 4) + (regs.ip & 0xFFFF);
                let kind = if self.offset(entry).is_some() { FrameKind::Function } else { FrameKind::Service };
                self.enter(kind, entry, regs, *regs, pushed);
            }
            Kind::Interrupt(vector) => {
                if let Some(c) = self.innermost() {
                    c.services.insert((vector, (pre.regs.gpr[0] >> 8) as u8));
                }
                if same_stack && pushed == 6 {
                    self.enter(FrameKind::Service, 0, regs, pre.regs, 6);
                }
            }
            _ => {}
        }
        self.unwind(regs, d.kind);
    }

    /// A hardware interrupt was delivered.
    pub(crate) fn hardware_interrupt(&mut self, regs: &Regs) {
        self.enter(FrameKind::Hardware, 0, regs, *regs, 6);
    }

    /// The innermost frame's contract, unless an interrupt handler or code outside the program runs.
    fn innermost(&mut self) -> Option<&mut Contract> {
        if self.opaque > 0 {
            return None;
        }
        let entry = self.frames.last()?.entry;
        Some(self.functions.entry(entry - self.base).or_default())
    }

    fn enter(&mut self, kind: FrameKind, entry: u32, regs: &Regs, entry_regs: Regs, pushed: u16) {
        if kind != FrameKind::Function {
            self.opaque += 1;
        }
        self.frames.push(Frame {
            kind,
            entry,
            ss: regs.ss(),
            sp: regs.sp(),
            pushed,
            start: self.seq + 1,
            regs: entry_regs,
            inputs: 0,
            saves: 0,
            stack_top: 0,
            reg_seq: Box::new(self.reg_seq),
            producer: Box::new(self.producer),
        });
    }

    /// Closes the frames the stack pointer rose above.
    fn unwind(&mut self, regs: &Regs, kind: Kind) {
        let mut first = true;
        while let Some(i) = self.frames.iter().rposition(|f| f.ss == regs.ss()) {
            // Signed: a stack starting at SP 0 wraps to FFFEh on the first push.
            if regs.sp().wrapping_sub(self.frames[i].sp) as i16 <= 0 {
                break;
            }
            // Frames on another stack above it were left without returning.
            while self.frames.len() > i + 1 {
                self.close(regs, None);
            }
            let returned = match kind {
                Kind::Return(how) if first => Some(how),
                _ => None,
            };
            self.close(regs, returned);
            first = false;
        }
    }

    fn close(&mut self, regs: &Regs, returned: Option<u8>) {
        let frame = self.frames.pop().expect("a frame");
        let changed = changed_units(&frame.regs, regs) & !STRUCTURAL;
        if frame.kind != FrameKind::Function {
            // Nothing inside was the caller's doing: the registers' history is as it was.
            self.opaque -= 1;
            self.reg_seq = *frame.reg_seq;
            self.producer = *frame.producer;
            if frame.kind == FrameKind::Service {
                for u in bits(changed | SERVICE_RESULTS) {
                    self.write(u);
                }
            }
            return;
        }
        let offset = frame.entry - self.base;
        // What it changed now holds its result, and the flags it set (flags aren't saved and
        // restored as registers are); what it kept has its earlier producer.
        let set_flags = (0..COUNT).filter(|&u| FLAGS & 1 << u != 0 && self.reg_seq[u] >= frame.start);
        let results = set_flags.fold(changed, |m, u| m | 1 << u);
        for u in 0..COUNT {
            self.producer[u] = if results & 1 << u == 0 { frame.producer[u] } else { returned.map(|_| offset) };
        }
        let c = self.functions.entry(offset).or_default();
        c.inputs |= frame.inputs | frame.saves & changed;
        let Some(how) = returned else {
            c.abnormal += 1;
            c.arg_bytes = c.arg_bytes.max(frame.stack_top.saturating_sub(frame.pushed));
            return;
        };
        // The return address's size, by how it returned (a far function called with
        // `push cs; call near` returns far).
        let address = match how {
            NEAR => 2,
            FAR => 4,
            _ => 6,
        };
        c.calls += 1;
        c.preserved &= !changed;
        c.returns |= how;
        c.pops.insert(regs.sp().wrapping_sub(frame.sp).wrapping_sub(address));
        c.arg_bytes = c.arg_bytes.max(frame.stack_top.saturating_sub(address));
    }

    fn read(&mut self, u: usize, save: bool) {
        if self.opaque > 0 {
            return;
        }
        if !save && let Some(f) = self.producer[u] {
            self.functions.entry(f).or_default().outputs |= 1 << u;
        }
        let last = self.reg_seq[u];
        for frame in self.frames.iter_mut().rev() {
            if frame.start <= last {
                break;
            }
            if save {
                frame.saves |= 1 << u;
            } else {
                frame.inputs |= 1 << u;
            }
        }
    }

    fn write(&mut self, u: usize) {
        self.reg_seq[u] = self.seq;
        self.producer[u] = None;
    }

    /// The step's data accesses. Returns the units a POP restored.
    fn memory(&mut self, pre: &Regs, regs: &Regs, pop: Pop) -> u64 {
        let mut restored = 0;
        if self.log.is_empty() || self.opaque > 0 {
            return restored;
        }
        let ss_base = u32::from(pre.ss()) << 4;
        let stack = ss_base + u32::from(pre.sp().min(regs.sp()))..ss_base + 0x1_0000;
        let frame = self.frames.last().map(|f| (f.entry - self.base, f.start, f.ss, ss_base + u32::from(f.sp)));
        let (base, size) = (self.base, self.size);
        let mut stack_top = 0;
        let log = std::mem::take(&mut self.log);
        for &access in &log {
            let (addr, write) = match access {
                Access::Read(addr, _) => (addr, false),
                Access::Write(addr, _) => (addr, true),
                Access::Port => {
                    if let Some((offset, ..)) = frame {
                        self.functions.entry(offset).or_default().ports = true;
                    }
                    continue;
                }
            };
            if addr as usize >= SPACE {
                continue;
            }
            let mut i = 0;
            while i < self.slots.len() {
                let slot = self.slots[i];
                if addr != slot.addr && addr != slot.addr + 1 {
                    i += 1;
                    continue;
                }
                self.slots.remove(i);
                match pop {
                    _ if write => {}
                    Pop::Register(units) if units & 1 << slot.unit != 0 => {
                        self.reg_seq[slot.unit] = slot.seq;
                        self.producer[slot.unit] = slot.producer;
                        restored |= 1 << slot.unit;
                    }
                    Pop::Other => {}
                    _ => self.use_slot(slot),
                }
            }
            let seq = &mut self.mem_seq[addr as usize];
            let last = *seq;
            if write {
                *seq = self.seq;
            }
            let Some((offset, start, ss, top)) = frame else { continue };
            if !write && last >= start {
                continue;
            }
            let c = self.functions.entry(offset).or_default();
            if stack.contains(&addr) {
                if !write && ss == pre.ss() && (top..top + ARGS_WINDOW).contains(&addr) {
                    stack_top = stack_top.max((addr - top + 1) as u16);
                }
            } else if let Some(image) = addr.checked_sub(base).filter(|&o| o < size) {
                let set = if write { &mut c.writes } else { &mut c.reads };
                if set.len() < MAX_BYTES {
                    set.insert(image);
                } else if !set.contains(&image) {
                    c.capped = true;
                }
            } else if (0xA_0000..0xC_0000).contains(&addr) {
                c.vram = true;
            } else if addr < 0x500 {
                c.bios_data = true;
            } else {
                c.far_memory = true;
            }
        }
        self.log = log;
        if stack_top > 0
            && let Some(frame) = self.frames.last_mut()
        {
            frame.stack_top = frame.stack_top.max(stack_top);
        }
        restored
    }

    /// A pushed register read from the stack other than by popping it back: its value is used,
    /// as if read then.
    fn use_slot(&mut self, slot: Slot) {
        let bit = 1 << slot.unit;
        if let Some(f) = slot.producer {
            self.functions.entry(f).or_default().outputs |= bit;
        }
        let depth = slot.depth.min(self.frames.len());
        for frame in self.frames[..depth].iter_mut().rev() {
            if frame.kind != FrameKind::Function || frame.start <= slot.seq {
                break;
            }
            frame.inputs |= bit;
        }
    }

    /// JSON: the image, the frames still open (kind, entry, SS, SP), and per function (load-image
    /// offset) what its calls did.
    pub fn to_json(&self, module: &str) -> String {
        let quote =
            |items: &mut dyn Iterator<Item = String>| items.map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(",");
        let functions: Vec<String> = self
            .functions
            .iter()
            .map(|(&offset, c)| {
                let preserved = if c.calls > 0 { c.preserved } else { 0 };
                // Only whole registers are preserved.
                let preserved = (0..8)
                    .filter(|&r| r != 4 && preserved & units::word(r) == units::word(r))
                    .fold(0, |m, r| m | units::word(r))
                    | preserved & (units::seg(0) | units::seg(3));
                let pops: Vec<String> = c.pops.iter().map(u16::to_string).collect();
                let services = c.services.iter().map(|&(v, ah)| format!("{v:02X}/{ah:02X}"));
                format!(
                    concat!(
                        "{{\"offset\":{},\"calls\":{},\"abnormal\":{},\"inputs\":[{}],\"flags_in\":[{}],",
                        "\"outputs\":[{}],\"flags_out\":[{}],\"preserved\":[{}],\"pops\":[{}],\"returns\":\"{}\",",
                        "\"arg_bytes\":{},\"reads\":{},\"writes\":{},\"capped\":{},\"vram\":{},\"ports\":{},",
                        "\"bios_data\":{},\"far_memory\":{},\"services\":[{}]}}"
                    ),
                    offset,
                    c.calls,
                    c.abnormal,
                    quote(&mut register_names(c.inputs).into_iter()),
                    quote(&mut flag_names(c.inputs).into_iter().map(String::from)),
                    quote(&mut register_names(c.outputs).into_iter()),
                    quote(&mut flag_names(c.outputs).into_iter().map(String::from)),
                    quote(&mut register_names(preserved).into_iter()),
                    pops.join(","),
                    return_names(c.returns),
                    c.arg_bytes,
                    ranges(&c.reads),
                    ranges(&c.writes),
                    c.capped,
                    c.vram,
                    c.ports,
                    c.bios_data,
                    c.far_memory,
                    quote(&mut services.into_iter()),
                )
            })
            .collect();
        format!(
            "{{\"module\":\"{}\",\"base\":{},\"size\":{},\"open_frames\":[{}],\"functions\":[{}]}}\n",
            module.replace('\\', "\\\\").replace('"', "\\\""),
            self.base,
            self.size,
            self.frames
                .iter()
                .map(|f| format!("[\"{:?}\",{},{},{}]", f.kind, f.entry, f.ss, f.sp))
                .collect::<Vec<_>>()
                .join(","),
            functions.join(",")
        )
    }
}

/// `[[offset,length],...]` runs of a set of offsets.
fn ranges(set: &BTreeSet<u32>) -> String {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &o in set {
        match runs.last_mut() {
            Some((start, len)) if *start + *len == o => *len += 1,
            _ => runs.push((o, 1)),
        }
    }
    format!("[{}]", runs.iter().map(|(o, n)| format!("[{o},{n}]")).collect::<Vec<_>>().join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(code: &[u8]) -> Decoded {
        let mut bytes = [0x90; 15];
        bytes[..code.len()].copy_from_slice(code);
        decode(bytes, 0x100)
    }

    #[test]
    fn instructions_read_and_write_units() {
        // add ax, bx
        let d = decoded(&[0x01, 0xD8]);
        assert_eq!(register_names(d.reads), ["AX", "BX"]);
        assert_eq!(register_names(d.writes & !FLAGS), ["AX"]);
        assert_eq!(flag_names(d.writes), ["CF", "PF", "AF", "ZF", "SF", "OF"]);
        // mov al, [si+2]: DS and SI for the address
        let d = decoded(&[0x8A, 0x44, 0x02]);
        assert_eq!(register_names(d.reads), ["SI", "DS"]);
        assert_eq!(register_names(d.writes), ["AL"]);
        // push si saves; push word [bp+4] reads BP
        assert_eq!(register_names(decoded(&[0x56]).saves & !STRUCTURAL), ["SI"]);
        let d = decoded(&[0xFF, 0x76, 0x04]);
        assert_eq!(register_names(d.reads & !STRUCTURAL), ["BP"]);
        // jc reads CF; int 21h reads AX; retf 4; call near
        assert_eq!(flag_names(decoded(&[0x72, 0x02]).reads), ["CF"]);
        assert_eq!(decoded(&[0xCD, 0x21]).kind, Kind::Interrupt(0x21));
        assert_eq!(decoded(&[0xCA, 0x04, 0x00]).kind, Kind::Return(FAR));
        assert_eq!(decoded(&[0xE8, 0x00, 0x00]).kind, Kind::Call);
        // 66h-prefixed: the upper half too
        assert_eq!(register_names(decoded(&[0x66, 0x01, 0xD8]).reads), ["EAX", "EBX"]);
    }

    #[test]
    fn names_of_partial_registers() {
        assert_eq!(register_names(units::high(0) | units::low(2) | units::word(6)), ["AH", "DL", "SI"]);
        assert_eq!(register_names(units::upper(1)), ["ECX:high"]);
    }
}
