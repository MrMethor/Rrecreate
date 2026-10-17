//! Hybrid execution (PROJECT.md §5.2, "Override table" and "Dual-run"): Rust functions that
//! replace functions of the original program, one at a time and in any order, and the check of
//! each against the original.
//!
//! An [`Override`] names a function by its offset in the program's load image and says how it
//! is called ([`Convention`]). When the CPU reaches a hooked entry, by its mode:
//! - [`Mode::Rust`]: the Rust function runs on the machine ([`Call`] decodes arguments, sets
//!   results, and calls other functions of the program with [`Call::call`]); then the host does
//!   the function's return (`ret` or `retf`, popping what the callee pops) and emulation goes on;
//! - [`Mode::Dual`]: the machine is snapshotted, the original runs until it returns, the Rust
//!   function runs on the snapshot, and the two results are compared under the convention:
//!   registers that carry results or must be preserved, memory except the function's dead stack
//!   frame and what interrupt handlers wrote meanwhile (the timer's tick count, the game's own
//!   timer variables: the Rust function takes no time, so they don't run for it), video memory
//!   and palette. A difference becomes a [`Finding`]; either way the run goes on with the
//!   original's result, so a dual session behaves exactly like an original one;
//! - [`Mode::Capture`]: the original runs, traced, and the call becomes a test vector
//!   ([`crate::capture`]);
//! - [`Mode::Poison`]: the original runs, then the registers and result flags its convention
//!   leaves out of the comparison get junk values. If the session's output (frames, sound,
//!   console, files) stays the same, nothing read them: the convention's exclusions hold on that
//!   session's paths (dos_re's poison test);
//! - [`Mode::Original`]: not hooked.
//!
//! Real mode only (the protected-mode code of DOS extenders isn't hooked). Rust code takes no
//! virtual time.

use std::collections::{BTreeMap, BTreeSet};

use cpu::seg;

use crate::machine::{Machine, Stop};

/// Registers, as bits: AX, CX, DX, BX, SP, BP, SI, DI (bits 0-7, as `cpu::regs::reg`), then ES,
/// CS, SS, DS (bits 8-11).
pub mod regs {
    pub const AX: u16 = 1 << 0;
    pub const CX: u16 = 1 << 1;
    pub const DX: u16 = 1 << 2;
    pub const BX: u16 = 1 << 3;
    pub const SP: u16 = 1 << 4;
    pub const BP: u16 = 1 << 5;
    pub const SI: u16 = 1 << 6;
    pub const DI: u16 = 1 << 7;
    pub const ES: u16 = 1 << 8;
    pub const CS: u16 = 1 << 9;
    pub const SS: u16 = 1 << 10;
    pub const DS: u16 = 1 << 11;
    pub const NAMES: [&str; 12] = ["AX", "CX", "DX", "BX", "SP", "BP", "SI", "DI", "ES", "CS", "SS", "DS"];
}

/// How a function is called and returns: what the host needs to return for a Rust function,
/// and what dual-run compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Convention {
    /// Returns with `retf` (called far) rather than `ret`.
    pub far: bool,
    /// Bytes of arguments the callee pops on return (pascal); 0 when the caller does (cdecl).
    pub pops: u16,
    /// Bytes of stack arguments. With the C convention the caller pops them right after the call,
    /// so whatever the callee left in them is dead (some functions use them as variables).
    pub args: u16,
    /// Registers carrying results.
    pub outputs: u16,
    /// Registers the function leaves as it found them. The rest are scratch: not compared.
    pub preserved: u16,
    /// Flags carrying results (CF for an error return, ...), as a FLAGS mask.
    pub flags: u16,
}

impl Convention {
    const CALLEE_SAVED: u16 = regs::BP | regs::SI | regs::DI | regs::DS | regs::SS;

    /// C calling convention (Borland, Microsoft, Watcom `__cdecl`): `arg_bytes` of arguments on
    /// the stack, which the caller pops; result in AX (or DX:AX); BX, CX, DX and ES are scratch.
    pub fn cdecl(far: bool, arg_bytes: u16, outputs: u16) -> Self {
        Convention { far, pops: 0, args: arg_bytes, outputs, preserved: Self::CALLEE_SAVED, flags: 0 }
    }

    /// Pascal convention: arguments pushed left to right, the callee pops `arg_bytes`.
    pub fn pascal(far: bool, arg_bytes: u16, outputs: u16) -> Self {
        Convention { far, pops: arg_bytes, args: arg_bytes, outputs, preserved: Self::CALLEE_SAVED, flags: 0 }
    }

    /// Watcom's default register convention: arguments in AX, DX, BX, CX (`args`; then the stack,
    /// which the callee pops). The argument registers are scratch (Watcom's code changes them
    /// freely, as dual-run showed); every other register but the results is preserved.
    pub fn watcom(far: bool, args: u16, stack_bytes: u16, outputs: u16) -> Self {
        let all = regs::AX | regs::BX | regs::CX | regs::DX | regs::ES | Self::CALLEE_SAVED;
        Convention { far, pops: stack_bytes, args: stack_bytes, outputs, preserved: all & !outputs & !args, flags: 0 }
    }

    /// Hand-written code with its own contract.
    pub fn custom(far: bool, pops: u16, outputs: u16, preserved: u16, flags: u16) -> Self {
        Convention { far, pops, args: pops, outputs, preserved, flags }
    }
}

pub type OverrideFn = fn(&mut Call) -> Result<(), String>;

/// The first 16 bytes of a function as the knowledge store has them (a game crate's generated
/// `ids.rs`): an override is written against these. Byte `i` is bits `8i..8i+8` of `bytes`;
/// bytes whose `mask` bit is clear are not compared (words the loader relocates, the end of the
/// image).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signature {
    pub offset: u32,
    pub bytes: u128,
    pub mask: u16,
}

impl Signature {
    /// Whether `code` (the 16 bytes at the entry) matches.
    pub fn matches(&self, code: &[u8; 16]) -> bool {
        (0..16).all(|i| self.mask & 1 << i == 0 || code[i] == (self.bytes >> (8 * i)) as u8)
    }

    fn describe(&self) -> String {
        (0..16)
            .map(
                |i| {
                    if self.mask & 1 << i == 0 { "..".into() } else { format!("{:02X}", (self.bytes >> (8 * i)) as u8) }
                },
            )
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn hex(code: &[u8]) -> String {
    code.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

/// A variable of the program: `size` bytes at a load-image offset (a game crate's generated
/// `layout.rs`, PROJECT.md §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Global {
    pub offset: u32,
    pub size: u32,
}

impl Global {
    /// The load-image offset of element `i` of `element` bytes.
    pub const fn at(self, i: u32, element: u32) -> u32 {
        self.offset + i * element
    }
}

/// A Rust function standing in for a function of the program.
#[derive(Clone, Copy)]
pub struct Override {
    /// Offset of the function's entry in the program's load image.
    pub offset: u32,
    pub name: &'static str,
    pub conv: Convention,
    pub run: OverrideFn,
}

impl Override {
    /// A function of the program with no Rust version yet: its calls can be captured
    /// ([`Mode::Capture`]), as test vectors for the version to come; named `-`.
    pub fn placeholder(offset: u32) -> Self {
        fn missing(_: &mut Call) -> Result<(), String> {
            Err("no Rust version of this function yet".into())
        }
        Override { offset, name: "-", conv: Convention::custom(false, 0, 0, 0, 0), run: missing }
    }
}

impl std::fmt::Debug for Override {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Override({:05X} {})", self.offset, self.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Original,
    Rust,
    Dual,
    Capture,
    Poison,
}

/// Counts per override.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub rust: u64,
    pub dual: u64,
    pub mismatches: u64,
    /// Distinct calls captured (kept in `captures`).
    pub captured: u64,
    /// Calls whose excluded registers were poisoned.
    pub poisoned: u64,
}

/// A difference between the original and an override, found by dual-run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Image offset and name of the function.
    pub offset: u32,
    pub name: &'static str,
    /// Which dual-run of this function it was (1 for the first).
    pub call: u64,
    pub cycle: u64,
    /// (register, original, Rust).
    pub registers: Vec<(&'static str, u16, u16)>,
    /// FLAGS, masked with the convention's result flags: (original, Rust).
    pub flags: Option<(u16, u16)>,
    /// Physical address and bytes in each run, runs of adjacent bytes merged.
    pub memory: Vec<(u32, Vec<u8>, Vec<u8>)>,
    /// Video memory offset and bytes; then palette entries that differ.
    pub vram: Vec<(u32, Vec<u8>, Vec<u8>)>,
    pub palette: Vec<u8>,
    /// What went wrong otherwise (the override failed, the original didn't return).
    pub note: Option<String>,
}

/// Most differing bytes a finding keeps.
const MAX_DIFFERENCES: usize = 256;
/// Longest an original may run in a dual-run before it is given up on.
const MAX_DUAL_CYCLES: u64 = 200_000_000;

/// The overrides of the program the machine runs, and what they found.
#[derive(Debug, Clone)]
pub struct Hybrid {
    pub module: String,
    /// Linear address of the load image.
    pub base: u32,
    overrides: Vec<(Override, Mode)>,
    by_linear: BTreeMap<u32, usize>,
    /// A bit per linear address below 110000h: an entry is hooked there.
    hooks: Vec<u64>,
    pub stats: Vec<Stats>,
    pub findings: Vec<Finding>,
    /// Distinct captured calls ([`Mode::Capture`]).
    pub captures: Vec<crate::capture::Capture>,
    seen: BTreeSet<(u32, u64)>,
    /// Dual-runs of one function, at most (then it runs as the original).
    pub dual_limit: u64,
    /// Inside a dual-run or a capture: nested hooked functions run as the original.
    pub(crate) in_dual: bool,
    /// The hooked entry the original was started at, not to be hooked again right away.
    pub(crate) skip: Option<u32>,
    /// What each override was written against ([`Hybrid::expect`]), by offset.
    signatures: BTreeMap<u32, Signature>,
    /// The code at each override's entry when it was first called, by offset.
    code: BTreeMap<u32, [u8; 16]>,
}

impl Hybrid {
    pub fn new(module: &str, base: u32, overrides: Vec<Override>) -> Self {
        let n = overrides.len();
        let mut hybrid = Hybrid {
            module: module.to_string(),
            base,
            overrides: overrides.into_iter().map(|o| (o, Mode::Original)).collect(),
            by_linear: BTreeMap::new(),
            hooks: vec![0; 0x11_0000 / 64],
            stats: vec![Stats::default(); n],
            findings: Vec::new(),
            captures: Vec::new(),
            seen: BTreeSet::new(),
            signatures: BTreeMap::new(),
            code: BTreeMap::new(),
            dual_limit: 100,
            in_dual: false,
            skip: None,
        };
        for (i, (o, _)) in hybrid.overrides.iter().enumerate() {
            hybrid.by_linear.insert(base + o.offset, i);
        }
        hybrid
    }

    pub fn overrides(&self) -> impl Iterator<Item = (&Override, Mode)> {
        self.overrides.iter().map(|(o, mode)| (o, *mode))
    }

    /// Sets the mode of the override at `offset` (or by name); false if there is none.
    pub fn set_mode(&mut self, which: &str, mode: Mode) -> bool {
        let offset = u32::from_str_radix(which.trim_start_matches('+'), 16).ok();
        let Some(i) = self.overrides.iter().position(|(o, _)| o.name == which || Some(o.offset) == offset) else {
            return false;
        };
        self.overrides[i].1 = mode;
        let linear = self.base + self.overrides[i].0.offset;
        let (word, bit) = (linear as usize / 64, linear % 64);
        if word < self.hooks.len() {
            if mode == Mode::Original {
                self.hooks[word] &= !(1 << bit);
            } else {
                self.hooks[word] |= 1 << bit;
            }
        }
        true
    }

    /// The bytes the overrides were written against: at an override's first call its entry must
    /// hold them (else the program is another version, or the code isn't loaded yet).
    pub fn expect(&mut self, signatures: &[Signature]) {
        for s in signatures {
            if self.overrides.iter().any(|(o, _)| o.offset == s.offset) {
                self.signatures.insert(s.offset, *s);
            }
        }
    }

    /// Checks the code at an override's entry: at the first call against its signature, later
    /// against what the first call found (an overlay or self-modifying code changed it). The
    /// override is only valid for the code it was written for, so a difference stops the run.
    pub(crate) fn check_code(&mut self, o: &Override, code: [u8; 16]) -> Result<(), String> {
        if let Some(first) = self.code.get(&o.offset) {
            if *first != code {
                return Err(format!(
                    "{}: the code at +{:05X} changed since its first call ({} became {}): an overlay or                      self-modifying code; the override is for the first",
                    o.name,
                    o.offset,
                    hex(first),
                    hex(&code)
                ));
            }
            return Ok(());
        }
        if let Some(s) = self.signatures.get(&o.offset)
            && !s.matches(&code)
        {
            return Err(format!(
                "{}: the code at +{:05X} isn't what the override was written for (expected {}, found {}):                  another version of the program?",
                o.name,
                o.offset,
                s.describe(),
                hex(&code)
            ));
        }
        self.code.insert(o.offset, code);
        Ok(())
    }

    pub fn set_all(&mut self, mode: Mode) {
        let names: Vec<_> = self.overrides.iter().map(|(o, _)| o.name).collect();
        for name in names {
            self.set_mode(name, mode);
        }
    }

    #[inline]
    fn hooked(&self, linear: u32) -> bool {
        self.hooks.get(linear as usize / 64).is_some_and(|word| word >> (linear % 64) & 1 != 0)
    }
}

/// Where a nested run stops: back at a return address with the stack unwound.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReturnTrap {
    cs: u16,
    ip: u16,
    ss: u16,
    /// SP once the callee has returned: at least this.
    sp: u16,
    /// Lowest SP seen while the callee ran.
    pub(crate) lowest_sp: u16,
}

impl ReturnTrap {
    pub(crate) fn new(cs: u16, ip: u16, ss: u16, sp: u16, lowest_sp: u16) -> Self {
        ReturnTrap { cs, ip, ss, sp, lowest_sp }
    }

    #[inline]
    pub(crate) fn reached(&mut self, regs: &cpu::Regs) -> bool {
        let sp = regs.sp();
        if regs.ss() == self.ss && sp < self.lowest_sp {
            self.lowest_sp = sp;
        }
        regs.ip as u16 == self.ip && regs.cs() == self.cs && regs.ss() == self.ss && sp >= self.sp
    }
}

/// During a dual-run's original pass: the memory hardware interrupt handlers wrote, which isn't
/// the function's doing.
#[derive(Debug, Clone, Default)]
pub(crate) struct IsrWatch {
    /// Low memory when the outermost handler was entered.
    before: Option<Box<[u8]>>,
    /// Interrupted contexts (CS, IP, SS, SP), the innermost last.
    interrupted: Vec<(u16, u16, u16, u16)>,
    pub(crate) written: BTreeSet<u32>,
}

impl IsrWatch {
    /// A hardware interrupt is about to be delivered.
    pub(crate) fn enter(&mut self, regs: &cpu::Regs, mem: &machine::memory::Memory) {
        if self.interrupted.is_empty() {
            self.before = Some(mem.slice(0, machine::addr::REAL_MODE_SPACE).into());
        }
        self.interrupted.push((regs.cs(), regs.ip as u16, regs.ss(), regs.sp()));
    }

    /// Before each instruction: back from the innermost handler?
    pub(crate) fn step(&mut self, regs: &cpu::Regs, mem: &machine::memory::Memory) {
        let Some(&(cs, ip, ss, sp)) = self.interrupted.last() else { return };
        if regs.cs() != cs || regs.ip as u16 != ip || regs.ss() != ss || regs.sp() != sp {
            return;
        }
        self.interrupted.pop();
        if self.interrupted.is_empty()
            && let Some(before) = self.before.take()
        {
            let now = mem.slice(0, before.len());
            self.written.extend((0..before.len()).filter(|&i| before[i] != now[i]).map(|i| i as u32));
        }
    }
}

/// A Rust function's view of the call it stands in for.
pub struct Call<'a> {
    pub m: &'a mut Machine,
    /// SS:SP at entry: the return address is there, the stack arguments above it.
    sp: u16,
    far: bool,
    /// Image offset of the function this call stands in for.
    offset: u32,
}

impl Call<'_> {
    /// Image offset of the function this call stands in for (one runner can serve several, as
    /// the Python bridge does).
    pub fn offset(&self) -> u32 {
        self.offset
    }

    /// The `i`th word of stack arguments, counting up from the return address (for cdecl the
    /// first argument; for pascal the last).
    pub fn arg16(&mut self, i: u16) -> u16 {
        let first = self.sp.wrapping_add(if self.far { 4 } else { 2 });
        let ss = self.m.cpu.regs.ss();
        self.read16(ss, first.wrapping_add(2 * i))
    }

    /// Words `i` and `i + 1` of stack arguments as a 32-bit value (a `long`, or a far pointer as
    /// offset and segment).
    pub fn arg32(&mut self, i: u16) -> u32 {
        u32::from(self.arg16(i)) | (u32::from(self.arg16(i + 1)) << 16)
    }

    /// The run-time segment of a segment the program refers to as `relative` (a relocated
    /// segment of its load image, as the disassembly shows it from load segment 0).
    pub fn segment(&self, relative: u16) -> u16 {
        let base = self.m.hybrid.as_ref().map_or(0, |h| h.base);
        ((base >> 4) as u16).wrapping_add(relative)
    }

    /// `OUT port, value`.
    pub fn out8(&mut self, port: u16, value: u8) {
        self.m.port_out(port, value);
    }

    /// `OUT port, AX`: the low byte to `port`, the high byte to `port + 1`.
    pub fn out16(&mut self, port: u16, value: u16) {
        let [lo, hi] = value.to_le_bytes();
        self.m.port_out(port, lo);
        self.m.port_out(port.wrapping_add(1), hi);
    }

    /// `IN AL, port`.
    pub fn in8(&mut self, port: u16) -> u8 {
        self.m.port_in(port)
    }

    pub fn ax(&self) -> u16 {
        self.m.cpu.regs.ax()
    }
    pub fn bx(&self) -> u16 {
        self.m.cpu.regs.bx()
    }
    pub fn cx(&self) -> u16 {
        self.m.cpu.regs.cx()
    }
    pub fn dx(&self) -> u16 {
        self.m.cpu.regs.dx()
    }
    pub fn ds(&self) -> u16 {
        self.m.cpu.regs.ds()
    }

    /// The result in AX.
    pub fn ret16(&mut self, value: u16) {
        self.m.cpu.regs.set_ax(value);
    }

    /// The result in DX:AX.
    pub fn ret32(&mut self, value: u32) {
        self.m.cpu.regs.set_ax(value as u16);
        self.m.cpu.regs.set_dx((value >> 16) as u16);
    }

    /// Memory as the CPU sees it (video memory included); `segment:offset`, the offset wrapping
    /// within the segment.
    pub fn read8(&mut self, segment: u16, offset: u16) -> u8 {
        self.m.read_u8(Machine::lin(segment, offset))
    }
    pub fn read16(&mut self, segment: u16, offset: u16) -> u16 {
        let lo = self.read8(segment, offset);
        let hi = self.read8(segment, offset.wrapping_add(1));
        u16::from_le_bytes([lo, hi])
    }
    pub fn write8(&mut self, segment: u16, offset: u16, value: u8) {
        self.m.write_u8(Machine::lin(segment, offset), value);
    }
    pub fn write16(&mut self, segment: u16, offset: u16, value: u16) {
        let [lo, hi] = value.to_le_bytes();
        self.write8(segment, offset, lo);
        self.write8(segment, offset.wrapping_add(1), hi);
    }

    /// The offset in DS of a load-image offset (of the program's data segment, DS at the call):
    /// for indexing as the original does, wrapping within the segment.
    pub fn in_ds(&self, offset: u32) -> u16 {
        let base = self.m.hybrid.as_ref().map_or(0, |h| h.base);
        base.wrapping_add(offset).wrapping_sub(u32::from(self.ds()) << 4) as u16
    }

    /// Memory at a load-image offset (where the program's data is, whatever DS holds).
    pub fn image8(&mut self, offset: u32) -> u8 {
        let base = self.m.hybrid.as_ref().map_or(0, |h| h.base);
        self.m.read_u8(base.wrapping_add(offset))
    }
    pub fn image16(&mut self, offset: u32) -> u16 {
        u16::from_le_bytes([self.image8(offset), self.image8(offset.wrapping_add(1))])
    }
    pub fn set_image8(&mut self, offset: u32, value: u8) {
        let base = self.m.hybrid.as_ref().map_or(0, |h| h.base);
        self.m.write_u8(base.wrapping_add(offset), value);
    }
    pub fn set_image16(&mut self, offset: u32, value: u16) {
        let [lo, hi] = value.to_le_bytes();
        self.set_image8(offset, lo);
        self.set_image8(offset.wrapping_add(1), hi);
    }

    /// Calls the program's function at image offset `offset` (an original or another override)
    /// with stack arguments `stack` (pushed last to first, so `stack[0]` ends up nearest the
    /// return address) and registers already set; returns when it does.
    pub fn call(&mut self, offset: u32, conv: Convention, stack: &[u16]) -> Result<(), String> {
        self.m.call_function(offset, conv, stack)
    }
}

impl Machine {
    /// Calls the program's function at image offset `offset`: pushes `stack` (last first) and a
    /// return address (the current CS:IP), runs it, nested, until it returns there, and leaves
    /// the stack as the caller of a `conv` function finds it (cdecl arguments are popped too).
    pub fn call_function(&mut self, offset: u32, conv: Convention, stack: &[u16]) -> Result<(), String> {
        let base = self.hybrid.as_ref().ok_or("no program is hooked")?.base;
        let linear = base + offset;
        let regs = self.cpu.regs;
        let (ss, cs, ip) = (regs.ss(), regs.cs(), regs.ip as u16);
        let mut sp = regs.sp();
        let mut words: Vec<u16> = stack.iter().rev().copied().collect();
        let after_args = sp.wrapping_sub(2 * words.len() as u16);
        if conv.far {
            words.push(cs);
        }
        words.push(ip);
        for word in words {
            sp = sp.wrapping_sub(2);
            self.mem.write_u16(Machine::lin(ss, sp), word);
        }
        // The callee runs in the segment that holds its entry (a near callee must be in ours).
        let target_cs = if conv.far { (linear >> 4) as u16 } else { cs };
        let target_ip = linear.wrapping_sub(u32::from(target_cs) << 4);
        if target_ip > 0xFFFF {
            return Err(format!("{offset:05X} isn't reachable with a near call from {cs:04X}"));
        }
        self.cpu.regs.set_sp(sp);
        self.cpu.regs.set_cs(target_cs);
        self.cpu.regs.ip = target_ip;
        let trap = ReturnTrap { cs, ip, ss, sp: after_args.wrapping_add(conv.pops), lowest_sp: sp };
        let (stop, _) = self.run_to_return(trap, MAX_DUAL_CYCLES);
        match stop {
            Stop::Returned => {
                // The caller pops cdecl arguments.
                if conv.pops == 0 {
                    let sp = self.cpu.regs.sp().wrapping_add(2 * stack.len() as u16);
                    self.cpu.regs.set_sp(sp);
                }
                Ok(())
            }
            other => Err(format!("the call to {offset:05X} didn't return: {other:?}")),
        }
    }

    /// Runs nested until `trap` is reached (or for the longest a dual-run allows).
    /// Runs the original of `o` to its return, then poisons what its convention doesn't compare:
    /// general and segment registers that are neither results nor preserved, and the arithmetic
    /// flags that aren't results. Values depend on `n`, the call number, deterministically.
    fn poison_call(&mut self, o: Override, n: u64) -> Result<(), Stop> {
        let regs = self.cpu.regs;
        let (ss, sp) = (regs.ss(), regs.sp());
        let ret_ip = self.mem.read_u16(Machine::lin(ss, sp));
        let ret_cs = if o.conv.far { self.mem.read_u16(Machine::lin(ss, sp.wrapping_add(2))) } else { regs.cs() };
        let after_return = sp.wrapping_add(if o.conv.far { 4 } else { 2 }).wrapping_add(o.conv.pops);
        self.hybrid.as_mut().expect("hooked").in_dual = true;
        let (stop, _) = self.run_to_return_public(ReturnTrap::new(ret_cs, ret_ip, ss, after_return, sp));
        let hybrid = self.hybrid.as_mut().expect("hooked");
        hybrid.in_dual = false;
        hybrid.skip = None;
        match stop {
            Stop::Returned => {}
            Stop::CycleLimit => return Ok(()),
            other => return Err(other),
        }
        let compared = o.conv.outputs | o.conv.preserved | regs::SP | regs::SS | regs::CS;
        let junk = |bit: u64| (0xA5C3u64 ^ n.wrapping_mul(0x9E37) ^ bit.wrapping_mul(0x3B1D)) as u16;
        let r = &mut self.cpu.regs;
        for bit in 0..12 {
            if compared & 1 << bit != 0 {
                continue;
            }
            match bit {
                0..8 => r.set_reg16(bit, junk(bit as u64)),
                8 => r.seg[seg::ES] = junk(8),
                11 => r.seg[seg::DS] = junk(11),
                _ => {}
            }
        }
        // CF, PF, AF, ZF, SF, OF: DF, IF and TF keep their meaning between calls.
        for (k, mask) in [0x001u16, 0x004, 0x010, 0x040, 0x080, 0x800].into_iter().enumerate() {
            if o.conv.flags & mask == 0 {
                r.set_flag(mask, junk(12 + k as u64) & 1 != 0);
            }
        }
        Ok(())
    }

    pub(crate) fn run_to_return_public(&mut self, trap: ReturnTrap) -> (Stop, u16) {
        self.run_to_return(trap, MAX_DUAL_CYCLES)
    }

    /// Runs nested until `trap` is reached; returns why it stopped and the lowest SP seen.
    fn run_to_return(&mut self, trap: ReturnTrap, max_cycles: u64) -> (Stop, u16) {
        let outer = self.return_trap.replace(trap);
        let stop = self.run(max_cycles);
        let lowest = self.return_trap.map_or(trap.lowest_sp, |t| t.lowest_sp);
        self.return_trap = outer;
        (stop, lowest)
    }

    /// Called before each instruction while overrides are loaded: runs the override hooked at
    /// CS:IP, if any. Returns whether it did (and the CPU is back at the caller).
    pub(crate) fn hybrid_entry(&mut self) -> Result<bool, Stop> {
        if self.cpu.protected_mode() {
            return Ok(false);
        }
        let linear = self.cpu.segment_base(seg::CS).wrapping_add(self.cpu.regs.ip);
        let Some(hybrid) = self.hybrid.as_mut() else { return Ok(false) };
        if !hybrid.hooked(linear) {
            return Ok(false);
        }
        if hybrid.skip.take() == Some(linear) {
            return Ok(false);
        }
        let i = hybrid.by_linear[&linear];
        let (o, mode) = hybrid.overrides[i];
        if mode != Mode::Original {
            let code = std::array::from_fn(|k| self.mem.read_u8(linear.wrapping_add(k as u32)));
            let hybrid = self.hybrid.as_mut().expect("hooked");
            hybrid.check_code(&o, code).map_err(Stop::Override)?;
        }
        let hybrid = self.hybrid.as_mut().expect("hooked");
        match mode {
            Mode::Original => Ok(false),
            Mode::Dual if hybrid.in_dual || hybrid.stats[i].dual >= hybrid.dual_limit => Ok(false),
            Mode::Capture if hybrid.in_dual || hybrid.stats[i].captured >= crate::capture::MAX_CAPTURES => Ok(false),
            Mode::Capture => {
                hybrid.skip = Some(linear);
                if let Some(capture) = self.capture_call(o)? {
                    let hybrid = self.hybrid.as_mut().expect("hooked");
                    if hybrid.seen.insert((capture.offset, capture.input_hash)) {
                        hybrid.stats[i].captured += 1;
                        hybrid.captures.push(capture);
                    }
                }
                Ok(true)
            }
            Mode::Poison if hybrid.in_dual => Ok(false),
            Mode::Poison => {
                hybrid.stats[i].poisoned += 1;
                let n = hybrid.stats[i].poisoned;
                hybrid.skip = Some(linear);
                self.poison_call(o, n)?;
                Ok(true)
            }
            Mode::Rust => {
                hybrid.stats[i].rust += 1;
                self.run_override(o).map_err(Stop::Override)?;
                Ok(true)
            }
            Mode::Dual => {
                hybrid.stats[i].dual += 1;
                hybrid.skip = Some(linear);
                self.dual_run(i, o)?;
                Ok(true)
            }
        }
    }

    /// Runs a Rust function at its entry, then returns from it as the original would.
    pub(crate) fn run_override_public(&mut self, o: Override) -> Result<(), String> {
        self.run_override(o)
    }

    fn run_override(&mut self, o: Override) -> Result<(), String> {
        let (ss, sp) = (self.cpu.regs.ss(), self.cpu.regs.sp());
        let mut call = Call { m: self, sp, far: o.conv.far, offset: o.offset };
        (o.run)(&mut call).map_err(|e| format!("{} ({:05X}): {e}", o.name, o.offset))?;
        // Code it wrote must not be fetched stale.
        self.cpu.flush_code_cache();
        // The return: IP (and CS) from the entry's stack, then what the callee pops.
        let ip = self.mem.read_u16(Machine::lin(ss, sp));
        let mut after = sp.wrapping_add(2);
        if o.conv.far {
            let cs = self.mem.read_u16(Machine::lin(ss, after));
            self.cpu.regs.set_cs(cs);
            after = after.wrapping_add(2);
        }
        self.cpu.regs.ip = u32::from(ip);
        self.cpu.regs.set_ss(ss);
        self.cpu.regs.set_sp(after.wrapping_add(o.conv.pops));
        Ok(())
    }

    /// Dual-run of override `i` (see the module documentation).
    fn dual_run(&mut self, i: usize, o: Override) -> Result<(), Stop> {
        let regs = self.cpu.regs;
        let (ss, sp) = (regs.ss(), regs.sp());
        let ret_ip = self.mem.read_u16(Machine::lin(ss, sp));
        let ret_cs = if o.conv.far { self.mem.read_u16(Machine::lin(ss, sp.wrapping_add(2))) } else { regs.cs() };
        let after_return = sp.wrapping_add(if o.conv.far { 4 } else { 2 }).wrapping_add(o.conv.pops);
        let mut rust = self.clone();
        // The original, from its entry to its return.
        let cycle = self.cpu.cycles;
        self.hybrid.as_mut().expect("hooked").in_dual = true;
        let trap = ReturnTrap { cs: ret_cs, ip: ret_ip, ss, sp: after_return, lowest_sp: sp };
        self.isr_watch = Some(IsrWatch::default());
        let (stop, lowest_sp) = self.run_to_return(trap, MAX_DUAL_CYCLES);
        let interrupts_wrote = self.isr_watch.take().map(|w| w.written).unwrap_or_default();
        let hybrid = self.hybrid.as_mut().expect("hooked");
        hybrid.in_dual = false;
        hybrid.skip = None;
        let call = hybrid.stats[i].dual;
        let mut finding = Finding {
            offset: o.offset,
            name: o.name,
            call,
            cycle,
            registers: Vec::new(),
            flags: None,
            memory: Vec::new(),
            vram: Vec::new(),
            palette: Vec::new(),
            note: None,
        };
        match stop {
            Stop::Returned => {}
            Stop::CycleLimit => {
                // Gone for too long (a loop that never returns): no comparison; carry on.
                finding.note = Some(format!("the original didn't return within {MAX_DUAL_CYCLES} cycles"));
                self.record(i, finding);
                return Ok(());
            }
            other => return Err(other),
        }
        // The override, on the state at entry.
        if let Some(h) = rust.hybrid.as_mut() {
            h.in_dual = true;
        }
        if let Err(e) = rust.run_override(o) {
            finding.note = Some(e);
            self.record(i, finding);
            return Ok(());
        }
        // Dead after the return: the original's frame below the entry SP, and the arguments the
        // caller pops.
        let dead_end = if o.conv.pops == 0 {
            sp.wrapping_add(if o.conv.far { 4 } else { 2 }).wrapping_add(o.conv.args)
        } else {
            sp
        };
        compare(self, &rust, o.conv, (ss, lowest_sp, dead_end), &interrupts_wrote, &mut finding);
        if !finding.registers.is_empty()
            || finding.flags.is_some()
            || !finding.memory.is_empty()
            || !finding.vram.is_empty()
            || !finding.palette.is_empty()
        {
            self.record(i, finding);
        }
        Ok(())
    }

    fn record(&mut self, i: usize, finding: Finding) {
        if let Some(h) = self.hybrid.as_mut() {
            h.stats[i].mismatches += 1;
            h.findings.push(finding);
        }
    }
}

/// Fills `finding` with what differs between the original's result `a` and the override's `b`,
/// leaving out the dead stack (SS, from the lowest SP to where the live stack starts) and what
/// interrupt handlers wrote.
fn compare(
    a: &Machine,
    b: &Machine,
    conv: Convention,
    (ss, lowest_sp, live_sp): (u16, u16, u16),
    interrupts_wrote: &BTreeSet<u32>,
    finding: &mut Finding,
) {
    let compared = conv.outputs | conv.preserved | regs::SP | regs::CS | regs::SS;
    for (bit, name) in regs::NAMES.iter().enumerate() {
        if compared & (1 << bit) == 0 {
            continue;
        }
        let value = |m: &Machine| if bit < 8 { m.cpu.regs.gpr[bit] as u16 } else { m.cpu.regs.seg[bit - 8] };
        if value(a) != value(b) {
            finding.registers.push((name, value(a), value(b)));
        }
    }
    if a.cpu.regs.ip != b.cpu.regs.ip {
        finding.registers.push(("IP", a.cpu.regs.ip as u16, b.cpu.regs.ip as u16));
    }
    if conv.flags != 0 {
        let (fa, fb) = (a.cpu.regs.flags as u16 & conv.flags, b.cpu.regs.flags as u16 & conv.flags);
        if fa != fb {
            finding.flags = Some((fa, fb));
        }
    }
    // The original's stack frame, below where SP is after the return, is dead.
    let dead = Machine::lin(ss, lowest_sp)..Machine::lin(ss, live_sp);
    let differing: Vec<u32> = a
        .mem
        .differences(&b.mem, MAX_DIFFERENCES * 4)
        .into_iter()
        .filter(|addr| !dead.contains(addr) && !interrupts_wrote.contains(addr))
        .collect();
    finding.memory = runs(&differing, |addr| a.mem.read_phys(addr), |addr| b.mem.read_phys(addr));
    let (va, vb) = (&a.devices.vga.vram, &b.devices.vga.vram);
    if va[..] != vb[..] {
        let differing: Vec<u32> =
            (0..va.len()).filter(|&i| va[i] != vb[i]).take(MAX_DIFFERENCES).map(|i| i as u32).collect();
        finding.vram = runs(&differing, |i| va[i as usize], |i| vb[i as usize]);
    }
    finding.palette =
        (0..=255u8).filter(|&i| a.devices.vga.dac[usize::from(i)] != b.devices.vga.dac[usize::from(i)]).collect();
}

/// Adjacent addresses merged into runs: (start, original bytes, Rust bytes).
fn runs(addresses: &[u32], a: impl Fn(u32) -> u8, b: impl Fn(u32) -> u8) -> Vec<(u32, Vec<u8>, Vec<u8>)> {
    let mut out: Vec<(u32, Vec<u8>, Vec<u8>)> = Vec::new();
    for &addr in addresses.iter().take(MAX_DIFFERENCES) {
        match out.last_mut() {
            Some((start, x, y)) if *start + x.len() as u32 == addr => {
                x.push(a(addr));
                y.push(b(addr));
            }
            _ => out.push((addr, vec![a(addr)], vec![b(addr)])),
        }
    }
    out
}
