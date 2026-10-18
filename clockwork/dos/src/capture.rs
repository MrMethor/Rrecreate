//! Captured-call tests (PROJECT.md §5.2, "Captured-call tests"; layer L1 of §8): while a session
//! runs, a function in [`crate::hybrid::Mode::Capture`] runs its original under a tracing bus,
//! and the call becomes a test vector: the registers at entry and the bytes it read before
//! writing them (its inputs: data and the code it ran), the registers after it returned and the
//! bytes it wrote (its outputs). [`replay`] runs an override on a fresh machine given only the
//! inputs and compares the outputs, thousands of calls a second, without the session. An
//! override that calls the program's functions runs their code from the inputs.
//!
//! What interrupt handlers do meanwhile isn't the function's doing and isn't recorded. A call is
//! replayable when it only touched ordinary memory: port I/O, host services (DOS, BIOS) and video
//! memory need device state a vector doesn't hold, so such calls are recorded but marked.

use std::collections::{BTreeMap, BTreeSet};

use cpu::{Bus, Regs};
use machine::hash::Fnv64;

use crate::hybrid::{Convention, Hybrid, Mode, Override, regs};
use crate::machine::{Machine, PcBus};
use crate::{Config, Vfs};

/// Most distinct captures kept per function.
pub const MAX_CAPTURES: u64 = 64;

/// Cycles a captured call may take (a quarter second at the default speed). A longer one (a wait
/// for input, a main loop) isn't captured: the program carries on from where it is, and the
/// calls it makes are hooked again, instead of all running nested in one capture.
pub const MAX_CAPTURE_CYCLES: u64 = 2_000_000;

/// Captures nested in one another, at most; deeper calls run as the original. Each level is a
/// nested run of the machine on the host's stack.
pub const MAX_CAPTURE_DEPTH: usize = 16;

/// One access, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    Read(u32, u8),
    Write(u32, u8),
    Port,
    /// An interrupt handler wrote this byte while the call ran.
    InterruptWrite(u32),
}

/// The tracing in force while captured functions run: one log for all of them, nested; each
/// capture takes its part from where it started.
#[derive(Debug, Clone, Default)]
pub(crate) struct Tracer {
    pub(crate) log: Vec<Access>,
    /// Instruction bytes fetched since the latest capture began: only a byte's first fetch is
    /// logged (later ones add nothing to what the capture read).
    pub(crate) fetched: ByteSet,
    /// Room for [`sort_accesses`].
    seen: ByteSet,
    /// Interrupted contexts (CS, IP, SS, SP): inside a handler while any.
    interrupted: Vec<(u16, u16, u16, u16)>,
    /// Host services the function itself called.
    pub(crate) services: u32,
}

impl Tracer {
    /// As new, keeping what it allocated.
    fn reset(&mut self) {
        self.log.clear();
        self.interrupted.clear();
        self.services = 0;
        self.fetched.clear();
        self.seen.clear();
    }

    pub(crate) fn interrupt(&mut self, regs: &Regs) {
        self.interrupted.push((regs.cs(), regs.ip as u16, regs.ss(), regs.sp()));
    }

    /// Before each instruction: back from a handler? Returns whether the function itself runs.
    /// Back at a context means back from every handler that interrupted it: a second interrupt
    /// can arrive there before any instruction runs (one pending while the first handler ran),
    /// and then two entries name the same place.
    pub(crate) fn active(&mut self, regs: &Regs) -> bool {
        while let Some(&(cs, ip, ss, sp)) = self.interrupted.last()
            && regs.cs() == cs
            && regs.ip as u16 == ip
            && regs.ss() == ss
            && regs.sp() == sp
        {
            self.interrupted.pop();
        }
        self.interrupted.is_empty()
    }
}

/// A set of physical addresses: a bit each in the real-mode address space (the HMA included),
/// where nearly all of a captured call's accesses are. Clearing it clears only the words it set.
#[derive(Debug, Clone, Default)]
pub(crate) struct ByteSet {
    bits: Vec<u64>,
    touched: Vec<u32>,
    beyond: BTreeSet<u32>,
}

impl ByteSet {
    const SIZE: u32 = 0x11_0000;

    /// Adds `addr`, and returns whether it was new.
    pub(crate) fn insert(&mut self, addr: u32) -> bool {
        if addr >= Self::SIZE {
            return self.beyond.insert(addr);
        }
        if self.bits.is_empty() {
            self.bits = vec![0; (Self::SIZE / 64) as usize];
        }
        let (word, bit) = ((addr / 64) as usize, 1u64 << (addr % 64));
        if self.bits[word] == 0 {
            self.touched.push(word as u32);
        }
        let new = self.bits[word] & bit == 0;
        self.bits[word] |= bit;
        new
    }

    pub(crate) fn contains(&self, addr: u32) -> bool {
        if addr >= Self::SIZE {
            return self.beyond.contains(&addr);
        }
        !self.bits.is_empty() && self.bits[(addr / 64) as usize] >> (addr % 64) & 1 != 0
    }

    pub(crate) fn clear(&mut self) {
        for word in self.touched.drain(..) {
            self.bits[word as usize] = 0;
        }
        self.beyond.clear();
    }
}

/// A bus that logs data accesses, and instruction fetches as reads when it has `fetched`: a
/// captured call's code is one of its inputs (contract inference wants data only).
pub(crate) struct TracingBus<'a, 'b> {
    pub(crate) inner: PcBus<'a>,
    pub(crate) log: &'b mut Vec<Access>,
    pub(crate) fetched: Option<&'b mut ByteSet>,
}

impl Bus for TracingBus<'_, '_> {
    fn read_u8(&mut self, addr: u32) -> u8 {
        let value = self.inner.read_u8(addr);
        self.log.push(Access::Read(addr, value));
        value
    }

    fn write_u8(&mut self, addr: u32, value: u8) {
        self.log.push(Access::Write(addr, value));
        self.inner.write_u8(addr, value);
    }

    fn fetch_u8(&mut self, addr: u32) -> u8 {
        let value = self.inner.read_u8(addr);
        if let Some(fetched) = &mut self.fetched
            && fetched.insert(addr)
        {
            self.log.push(Access::Read(addr, value));
        }
        value
    }

    fn code_window(&mut self, addr: u32) -> Option<[u8; 16]> {
        // Logged code is fetched byte by byte: a window reads ahead of what runs.
        if self.fetched.is_some() { None } else { self.inner.code_window(addr) }
    }

    fn port_in_u8(&mut self, port: u16) -> u8 {
        self.log.push(Access::Port);
        self.inner.port_in_u8(port)
    }

    fn port_out_u8(&mut self, port: u16, value: u8) {
        self.log.push(Access::Port);
        self.inner.port_out_u8(port, value);
    }
}

/// Registers as a captured call sees them: AX..DI, ES/CS/SS/DS, IP and FLAGS (16-bit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Registers {
    pub gpr: [u16; 8],
    pub seg: [u16; 4],
    pub ip: u16,
    pub flags: u16,
}

impl Registers {
    pub fn of(regs: &Regs) -> Self {
        Registers {
            gpr: std::array::from_fn(|i| regs.gpr[i] as u16),
            seg: std::array::from_fn(|i| regs.seg[i]),
            ip: regs.ip as u16,
            flags: regs.flags as u16,
        }
    }

    fn apply(&self, regs: &mut Regs) {
        for i in 0..8 {
            regs.gpr[i] = u32::from(self.gpr[i]);
        }
        regs.seg[..4].copy_from_slice(&self.seg);
        regs.ip = u32::from(self.ip);
        regs.flags = u32::from(self.flags) | 0x2;
    }

    /// Register `bit` of [`crate::hybrid::regs`].
    fn get(&self, bit: usize) -> u16 {
        if bit < 8 { self.gpr[bit] } else { self.seg[bit - 8] }
    }
}

/// A captured call: a test vector for the function at `offset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub offset: u32,
    pub name: String,
    /// Linear address of the load image when it was captured.
    pub base: u32,
    pub cycle: u64,
    pub entry: Registers,
    /// Bytes read before being written, by physical address: data, and the code that ran.
    pub inputs: BTreeMap<u32, u8>,
    pub exit: Registers,
    /// Final values of the bytes written (the dead stack left out).
    pub writes: BTreeMap<u32, u8>,
    /// Why it can't be replayed, if it can't.
    pub unreplayable: Option<String>,
    /// Of the entry registers and inputs (code included): equal hashes, equal calls.
    pub input_hash: u64,
}

impl Machine {
    /// Captures a call of override `o` (its original runs, traced, until it returns).
    pub(crate) fn capture_call(&mut self, o: Override) -> Result<Option<Capture>, crate::Stop> {
        let regs = self.cpu.regs;
        let (ss, sp) = (regs.ss(), regs.sp());
        // A placeholder's convention is unknown: its call may have been near, far or an interrupt.
        let placeholder = o.name == Override::PLACEHOLDER;
        let trap = if placeholder {
            crate::hybrid::ReturnTrap::any_return(regs.cs(), ss, sp)
        } else {
            let ret_ip = self.mem.read_u16(Machine::lin(ss, sp));
            let ret_cs = if o.conv.far { self.mem.read_u16(Machine::lin(ss, sp.wrapping_add(2))) } else { regs.cs() };
            let after_return = sp.wrapping_add(if o.conv.far { 4 } else { 2 }).wrapping_add(o.conv.pops);
            crate::hybrid::ReturnTrap::new(ret_cs, ret_ip, ss, after_return, sp)
        };
        let entry = Registers::of(&regs);
        let cycle = self.cpu.cycles;
        let hybrid = self.hybrid.as_mut().expect("hooked");
        hybrid.capturing += 1;
        let tracer = self.tracer.get_or_insert_with(|| hybrid.spare_tracer.take().unwrap_or_default());
        let (start, services) = (tracer.log.len(), tracer.services);
        // The code this call runs is logged again, though an outer capture logged it before.
        tracer.fetched.clear();
        let (stop, lowest_sp) = self.run_to_return_within(trap, MAX_CAPTURE_CYCLES);
        let tracer = self.tracer.as_deref_mut().expect("tracing");
        let accesses = (stop == crate::Stop::Returned).then(|| sort_accesses(&tracer.log[start..], &mut tracer.seen));
        let services = tracer.services - services;
        let hybrid = self.hybrid.as_mut().expect("hooked");
        hybrid.capturing -= 1;
        hybrid.skip = None;
        if hybrid.capturing == 0
            && let Some(mut tracer) = self.tracer.take()
        {
            tracer.reset();
            hybrid.spare_tracer = Some(tracer);
        }
        let (inputs, mut written, ports, interrupted) = match (stop, accesses) {
            (_, Some(accesses)) => accesses,
            (crate::Stop::CycleLimit, None) => return Ok(None),
            (other, None) => return Err(other),
        };
        let base = self.hybrid.as_ref().expect("hooked").base;
        let dead_end = if placeholder {
            // What the return popped: the return address and any arguments the callee removed.
            self.cpu.regs.sp()
        } else if o.conv.pops == 0 {
            sp.wrapping_add(if o.conv.far { 4 } else { 2 }).wrapping_add(o.conv.args)
        } else {
            sp
        };
        let dead = Machine::lin(ss, lowest_sp)..Machine::lin(ss, dead_end);
        let mut unreplayable = ports.then(|| "port I/O".to_string());
        if services > 0 {
            unreplayable = Some("host services".to_string());
        }
        let video = |addr: &u32| addr.wrapping_sub(0xA_0000) < 0x2_0000;
        if inputs.keys().chain(written.keys()).any(video) {
            unreplayable = Some("video memory".to_string());
        }
        if interrupted {
            // A replay gives it the value from before the interrupt (a timer tick between two reads
            // of the time): dual-run, which runs the original again without interrupts, checks it.
            unreplayable = Some("reads memory an interrupt changed during the call".to_string());
        }
        written.retain(|addr, _| !dead.contains(addr));
        let mut h = Fnv64::default();
        std::hash::Hash::hash(&entry, &mut h);
        for (addr, value) in &inputs {
            std::hash::Hasher::write(&mut h, &addr.to_le_bytes());
            std::hash::Hasher::write(&mut h, &[*value]);
        }
        let input_hash = std::hash::Hasher::finish(&h);
        Ok(Some(Capture {
            offset: o.offset,
            name: o.name.to_string(),
            base,
            cycle,
            entry,
            inputs,
            exit: Registers::of(&self.cpu.regs),
            writes: written,
            unreplayable,
            input_hash,
        }))
    }
}

/// What a call's accesses come to: the bytes it read before writing them, the final values of
/// the bytes it wrote, whether it used ports, and whether it read a byte an interrupt handler
/// changed while it ran. `seen` is room for the bytes already read or written.
fn sort_accesses(log: &[Access], seen: &mut ByteSet) -> (BTreeMap<u32, u8>, BTreeMap<u32, u8>, bool, bool) {
    seen.clear();
    let (mut inputs, mut written, mut ports) = (Vec::new(), BTreeMap::new(), false);
    let (mut changed, mut interrupted) = (BTreeSet::new(), false);
    for access in log {
        match *access {
            Access::Read(addr, value) => {
                interrupted |= changed.contains(&addr);
                if seen.insert(addr) {
                    inputs.push((addr, value));
                }
            }
            Access::InterruptWrite(addr) => {
                changed.insert(addr);
            }
            Access::Write(addr, value) => {
                seen.insert(addr);
                written.insert(addr, value);
            }
            Access::Port => ports = true,
        }
    }
    inputs.sort_unstable_by_key(|&(addr, _)| addr);
    (inputs.into_iter().collect(), written, ports, interrupted)
}

/// The bus while a captured call is interrupted: the handler's writes are logged (for
/// [`sort_accesses`]), nothing else.
pub(crate) struct InterruptBus<'a, 'b> {
    pub(crate) inner: PcBus<'a>,
    pub(crate) log: &'b mut Vec<Access>,
}

impl Bus for InterruptBus<'_, '_> {
    fn read_u8(&mut self, addr: u32) -> u8 {
        self.inner.read_u8(addr)
    }

    fn write_u8(&mut self, addr: u32, value: u8) {
        self.log.push(Access::InterruptWrite(addr));
        self.inner.write_u8(addr, value);
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

impl Capture {
    /// One line of text: `C1 offset name base cycle hash entry exit inputs writes [reason]`, numbers
    /// in hex, registers as 14 words (AX CX DX BX SP BP SI DI ES CS SS DS IP FLAGS), bytes as runs
    /// `address:hex` joined by commas (`-` for none).
    pub fn to_line(&self) -> String {
        let words = |r: &Registers| {
            r.gpr.iter().chain(&r.seg).chain([&r.ip, &r.flags]).map(|w| format!("{w:X}")).collect::<Vec<_>>().join(",")
        };
        let runs = |bytes: &BTreeMap<u32, u8>| {
            let mut out: Vec<(u32, String)> = Vec::new();
            for (&addr, &value) in bytes {
                match out.last_mut() {
                    Some((start, hex)) if *start + hex.len() as u32 / 2 == addr => {
                        hex.push_str(&format!("{value:02X}"))
                    }
                    _ => out.push((addr, format!("{value:02X}"))),
                }
            }
            if out.is_empty() {
                "-".to_string()
            } else {
                out.iter().map(|(a, h)| format!("{a:X}:{h}")).collect::<Vec<_>>().join(",")
            }
        };
        let mut line = format!(
            "C1 {:X} {} {:X} {} {:016X} {} {} {} {}",
            self.offset,
            self.name,
            self.base,
            self.cycle,
            self.input_hash,
            words(&self.entry),
            words(&self.exit),
            runs(&self.inputs),
            runs(&self.writes)
        );
        if let Some(reason) = &self.unreplayable {
            line.push(' ');
            line.push_str(reason);
        }
        line
    }

    /// Reads [`Capture::to_line`]'s format.
    pub fn from_line(line: &str) -> Result<Capture, String> {
        let bad = |what: &str| format!("bad capture ({what}): {line:.80}");
        let mut fields = line.split(' ');
        let mut next = |what: &str| fields.next().ok_or_else(|| bad(what));
        if next("version")? != "C1" {
            return Err(bad("version"));
        }
        let hex = |text: &str, what: &str| u32::from_str_radix(text, 16).map_err(|_| bad(what));
        let offset = hex(next("offset")?, "offset")?;
        let name = next("name")?.to_string();
        let base = hex(next("base")?, "base")?;
        let cycle = next("cycle")?.parse().map_err(|_| bad("cycle"))?;
        let input_hash = u64::from_str_radix(next("hash")?, 16).map_err(|_| bad("hash"))?;
        let registers = |text: &str| -> Result<Registers, String> {
            let words: Vec<u16> = text
                .split(',')
                .map(|w| u16::from_str_radix(w, 16).map_err(|_| bad("registers")))
                .collect::<Result<_, _>>()?;
            if words.len() != 14 {
                return Err(bad("registers"));
            }
            Ok(Registers {
                gpr: words[..8].try_into().expect("8"),
                seg: words[8..12].try_into().expect("4"),
                ip: words[12],
                flags: words[13],
            })
        };
        let entry = registers(next("entry")?)?;
        let exit = registers(next("exit")?)?;
        let bytes = |text: &str| -> Result<BTreeMap<u32, u8>, String> {
            let mut out = BTreeMap::new();
            if text == "-" {
                return Ok(out);
            }
            for run in text.split(',') {
                let (start, data) = run.split_once(':').ok_or_else(|| bad("bytes"))?;
                let start = u32::from_str_radix(start, 16).map_err(|_| bad("bytes"))?;
                for i in 0..data.len() / 2 {
                    let value = u8::from_str_radix(&data[2 * i..2 * i + 2], 16).map_err(|_| bad("bytes"))?;
                    out.insert(start + i as u32, value);
                }
            }
            Ok(out)
        };
        let inputs = bytes(next("inputs")?)?;
        let writes = bytes(next("writes")?)?;
        let rest: Vec<&str> = fields.collect();
        let unreplayable = (!rest.is_empty()).then(|| rest.join(" "));
        Ok(Capture { offset, name, base, cycle, entry, inputs, exit, writes, unreplayable, input_hash })
    }
}

/// What a replay found wrong, empty when the override matched.
pub type Differences = Vec<String>;

/// A replay's difference when the override runs code of the program (`Call::call`) that the
/// captured call doesn't hold: code the original didn't run in that call, or any code in a
/// capture made before captures recorded code (2026-10-18). The call can't be replayed
/// (dual-run checks it).
pub const CALLS_PROGRAM: &str = "runs the program's code that the captured call doesn't hold";

/// A replay's difference when the override waits for an interrupt
/// ([`crate::hybrid::Call::wait_for_interrupt`]): the program's handlers that would end the wait
/// aren't in a captured call either.
pub const WAITS_FOR_INTERRUPT: &str = "waits for an interrupt, which a captured call doesn't deliver";

/// Whether a replay stopped at what a captured call doesn't hold (code of the program,
/// [`CALLS_PROGRAM`]; an interrupt to wait for, [`WAITS_FOR_INTERRUPT`]) rather than finding the
/// override wrong.
pub fn calls_program(differences: &[String]) -> bool {
    differences.iter().any(|d| d.contains(CALLS_PROGRAM) || d.contains(WAITS_FOR_INTERRUPT))
}

/// What a replaying machine holds: the capture's inputs and the bytes the replay wrote. Code
/// runs only from there; the first instruction byte fetched from anywhere else is `missing`,
/// and the run stops at it ([`CALLS_PROGRAM`]) instead of running on zeroed memory.
#[derive(Debug, Clone)]
pub(crate) struct Held {
    bytes: ByteSet,
    pub(crate) missing: Option<u32>,
}

impl Held {
    fn new(addresses: impl IntoIterator<Item = u32>) -> Self {
        let mut bytes = ByteSet::default();
        for addr in addresses {
            bytes.insert(addr);
        }
        Held { bytes, missing: None }
    }
}

/// The bus the program's code runs on during a replay ([`Held`]).
pub(crate) struct ReplayBus<'a, 'b> {
    pub(crate) inner: PcBus<'a>,
    pub(crate) held: &'b mut Held,
}

impl Bus for ReplayBus<'_, '_> {
    fn read_u8(&mut self, addr: u32) -> u8 {
        self.inner.read_u8(addr)
    }

    fn write_u8(&mut self, addr: u32, value: u8) {
        self.held.bytes.insert(addr);
        self.inner.write_u8(addr, value);
    }

    fn fetch_u8(&mut self, addr: u32) -> u8 {
        if !self.held.bytes.contains(addr) {
            self.held.missing.get_or_insert(addr);
        }
        self.inner.read_u8(addr)
    }

    fn code_window(&mut self, _addr: u32) -> Option<[u8; 16]> {
        None
    }

    fn port_in_u8(&mut self, port: u16) -> u8 {
        self.inner.port_in_u8(port)
    }

    fn port_out_u8(&mut self, port: u16, value: u8) {
        self.inner.port_out_u8(port, value);
    }
}

/// Runs override `o` on a fresh machine holding only the capture's inputs, and compares its
/// results with the capture's outputs under the convention.
pub fn replay(capture: &Capture, o: Override) -> Differences {
    replay_with(capture, o, &[])
}

/// [`replay`], with `callees` (the other overrides of the program) in place: a function `o`
/// calls ([`crate::hybrid::Call::call`]) runs as its Rust version if it has one, else as the
/// program's code the capture holds.
pub fn replay_with(capture: &Capture, o: Override, callees: &[Override]) -> Differences {
    let mut m = Machine::new(Config::default(), Vfs::new());
    for (&addr, &value) in &capture.inputs {
        m.write_u8(addr, value);
    }
    capture.entry.apply(&mut m.cpu.regs);
    let others = callees.iter().filter(|c| c.offset != o.offset).copied();
    let mut hybrid = Hybrid::new("", capture.base, std::iter::once(o).chain(others).collect());
    hybrid.set_all(Mode::Rust);
    m.hybrid = Some(Box::new(hybrid));
    // The program's code the override calls runs from the inputs, and takes no interrupts: what
    // handlers did during the call isn't in the capture.
    m.replay = Some(Box::new(Held::new(capture.inputs.keys().copied())));
    m.set_atomic(true);
    let before = m.mem.clone();
    let mut differences = Vec::new();
    if let Err(e) = m.run_override_public(o) {
        differences.push(e);
        return differences;
    }
    let after = Registers::of(&m.cpu.regs);
    let conv: Convention = o.conv;
    let compared = conv.outputs | conv.preserved | regs::SP | regs::CS | regs::SS;
    for (bit, name) in regs::NAMES.iter().enumerate() {
        if compared & (1 << bit) != 0 && after.get(bit) != capture.exit.get(bit) {
            differences.push(format!("{name} {:04X} != {:04X}", capture.exit.get(bit), after.get(bit)));
        }
    }
    if after.ip != capture.exit.ip {
        differences.push(format!("IP {:04X} != {:04X}", capture.exit.ip, after.ip));
    }
    if conv.flags != 0 && (after.flags ^ capture.exit.flags) & conv.flags != 0 {
        differences.push(format!("flags {:04X} != {:04X}", capture.exit.flags & conv.flags, after.flags & conv.flags));
    }
    for (&addr, &value) in &capture.writes {
        let got = m.mem.read_u8(addr);
        if got != value {
            differences.push(format!("[{addr:05X}] {value:02X} != {got:02X}"));
        }
    }
    // The stack around the entry's SP, by offset in SS (it may wrap around the segment).
    let (ss, sp) = (capture.entry.seg[2], capture.entry.gpr[4]);
    let frame = |addr: u32| {
        let offset = addr.wrapping_sub(u32::from(ss) << 4);
        offset < 0x1_0000 && (offset as u16).wrapping_sub(sp.wrapping_sub(0x400)) < 0x440
    };
    for addr in m.mem.differences(&before, 64) {
        if !capture.writes.contains_key(&addr) && !frame(addr) {
            differences.push(format!("[{addr:05X}] written, but the original didn't"));
        }
    }
    differences
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_interrupts_at_one_place_both_end_there() {
        let mut function = Regs { seg: [0, 0x1821, 0x389B, 0x389B, 0, 0], ip: 0x762, ..Regs::default() };
        function.set_sp(0xFF6A);
        let mut handler = function;
        handler.ip = 0x9;
        handler.set_sp(0xFF64);
        let mut t = Tracer::default();
        assert!(t.active(&function));
        t.interrupt(&function);
        assert!(!t.active(&handler));
        // The handler returned, and a second interrupt, pending meanwhile, came before any
        // instruction ran here: the same place interrupted twice.
        t.interrupt(&function);
        assert!(!t.active(&handler));
        assert!(t.active(&function), "back from both handlers: the function runs");
        assert!(t.interrupted.is_empty());
    }
}
