//! Captured-call tests (PROJECT.md §5.2, "Captured-call tests"; layer L1 of §8): while a session
//! runs, a function in [`crate::hybrid::Mode::Capture`] runs its original under a tracing bus,
//! and the call becomes a test vector: the registers at entry and the bytes it read before
//! writing them (its inputs), the registers after it returned and the bytes it wrote (its
//! outputs). [`replay`] runs an override on a fresh machine given only the inputs and compares
//! the outputs, thousands of calls a second, without the session.
//!
//! What interrupt handlers do meanwhile isn't the function's doing and isn't recorded. A call is
//! replayable when it only touched ordinary memory: port I/O, host services (DOS, BIOS) and video
//! memory need device state a vector doesn't hold, so such calls are recorded but marked.

use std::collections::BTreeMap;

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
}

/// The tracing in force while captured functions run: one log for all of them, nested; each
/// capture takes its part from where it started.
#[derive(Debug, Clone, Default)]
pub(crate) struct Tracer {
    pub(crate) log: Vec<Access>,
    /// Interrupted contexts (CS, IP, SS, SP): inside a handler while any.
    interrupted: Vec<(u16, u16, u16, u16)>,
    /// Host services the function itself called.
    pub(crate) services: u32,
}

impl Tracer {
    pub(crate) fn interrupt(&mut self, regs: &Regs) {
        self.interrupted.push((regs.cs(), regs.ip as u16, regs.ss(), regs.sp()));
    }

    /// Before each instruction: back from a handler? Returns whether the function itself runs.
    pub(crate) fn active(&mut self, regs: &Regs) -> bool {
        if let Some(&(cs, ip, ss, sp)) = self.interrupted.last()
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

/// A bus that logs data accesses (instruction fetches aren't data).
pub(crate) struct TracingBus<'a, 'b> {
    pub(crate) inner: PcBus<'a>,
    pub(crate) log: &'b mut Vec<Access>,
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
        self.inner.read_u8(addr)
    }

    fn code_window(&mut self, addr: u32) -> Option<[u8; 16]> {
        self.inner.code_window(addr)
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
    /// Bytes read before being written, by physical address.
    pub inputs: BTreeMap<u32, u8>,
    pub exit: Registers,
    /// Final values of the bytes written (the dead stack left out).
    pub writes: BTreeMap<u32, u8>,
    /// Why it can't be replayed, if it can't.
    pub unreplayable: Option<String>,
    /// Of the entry registers and inputs: equal hashes, equal calls.
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
        self.hybrid.as_mut().expect("hooked").capturing += 1;
        let tracer = self.tracer.get_or_insert_with(Box::default);
        let (start, services) = (tracer.log.len(), tracer.services);
        let (stop, lowest_sp) = self.run_to_return_within(trap, MAX_CAPTURE_CYCLES);
        let tracer = self.tracer.as_deref_mut().expect("tracing");
        let log = tracer.log[start..].to_vec();
        let services = tracer.services - services;
        let hybrid = self.hybrid.as_mut().expect("hooked");
        hybrid.capturing -= 1;
        hybrid.skip = None;
        if hybrid.capturing == 0 {
            self.tracer = None;
        }
        let hybrid = self.hybrid.as_mut().expect("hooked");
        match stop {
            crate::Stop::Returned => {}
            crate::Stop::CycleLimit => return Ok(None),
            other => return Err(other),
        }
        let base = hybrid.base;
        let dead_end = if placeholder {
            // What the return popped: the return address and any arguments the callee removed.
            self.cpu.regs.sp()
        } else if o.conv.pops == 0 {
            sp.wrapping_add(if o.conv.far { 4 } else { 2 }).wrapping_add(o.conv.args)
        } else {
            sp
        };
        let dead = Machine::lin(ss, lowest_sp)..Machine::lin(ss, dead_end);
        let mut inputs = BTreeMap::new();
        let mut written = BTreeMap::new();
        let mut unreplayable = None;
        for access in &log {
            match *access {
                Access::Read(addr, value) => {
                    if !written.contains_key(&addr) {
                        inputs.entry(addr).or_insert(value);
                    }
                }
                Access::Write(addr, value) => {
                    written.insert(addr, value);
                }
                Access::Port => unreplayable = Some("port I/O".to_string()),
            }
        }
        if services > 0 {
            unreplayable = Some("host services".to_string());
        }
        let video = |addr: &u32| addr.wrapping_sub(0xA_0000) < 0x2_0000;
        if inputs.keys().chain(written.keys()).any(video) {
            unreplayable = Some("video memory".to_string());
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

/// A replay's difference when the override calls a function of the program (`Call::call`): a
/// captured call holds the data the original read, not the code it ran, so the call can't be
/// replayed (dual-run checks it).
pub const CALLS_PROGRAM: &str = "calls the program's code, which a captured call doesn't hold";

/// Whether a replay stopped at a call into the program's code ([`CALLS_PROGRAM`]) rather than
/// finding the override wrong.
pub fn calls_program(differences: &[String]) -> bool {
    differences.iter().any(|d| d.contains(CALLS_PROGRAM))
}

/// Runs override `o` on a fresh machine holding only the capture's inputs, and compares its
/// results with the capture's outputs under the convention.
pub fn replay(capture: &Capture, o: Override) -> Differences {
    replay_with(capture, o, &[])
}

/// [`replay`], with `callees` (the other overrides of the program) in place: the machine holds
/// no code, so a function `o` calls ([`crate::hybrid::Call::call`]) runs as its Rust version, and
/// one without a Rust version can't run.
pub fn replay_with(capture: &Capture, o: Override, callees: &[Override]) -> Differences {
    let mut m = Machine::new(Config::default(), Vfs::new());
    for (&addr, &value) in &capture.inputs {
        m.write_u8(addr, value);
    }
    capture.entry.apply(&mut m.cpu.regs);
    let others = callees.iter().filter(|c| c.offset != o.offset).copied();
    let mut hybrid = Hybrid::new("", capture.base, std::iter::once(o).chain(others).collect());
    hybrid.set_all(Mode::Rust);
    hybrid.replaying = true;
    m.hybrid = Some(Box::new(hybrid));
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
    let (ss, sp) = (capture.entry.seg[2], capture.entry.gpr[4]);
    let frame = Machine::lin(ss, sp.wrapping_sub(0x400))..Machine::lin(ss, sp.wrapping_add(0x40));
    for addr in m.mem.differences(&before, 64) {
        if !capture.writes.contains_key(&addr) && !frame.contains(&addr) {
            differences.push(format!("[{addr:05X}] written, but the original didn't"));
        }
    }
    differences
}
