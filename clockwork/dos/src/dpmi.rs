//! A DPMI 0.9 host (`--dpmi`), for programs that need one (DJGPP programs without CWSDPMI,
//! Borland's protected-mode programs, anything that asks INT 2Fh AX=1687h first). Written from
//! the DPMI 0.9 specification; DOSBox has no DPMI host of its own to port.
//!
//! The client runs at ring 0 with paging off, so linear addresses are physical, I/O goes
//! straight to the devices and interrupts need no stack switch. The host's tables (GDT, IDT,
//! LDT) and its protected-mode stubs live in an XMS block. Every IDT gate leads to a stub
//! (`callback; IRET`): INT 31h is served in the host, a vector the client hooked (0205h) jumps
//! to its handler with the interrupt frame in place, CPU exceptions go to the client's
//! exception handler (0203h) or end it, and anything else is reflected to real mode. While a
//! client runs, the PIC's interrupts come in on vectors of their own ([`IRQ_BASE`]), so IRQ 0-7
//! can't be taken for exceptions 08h-0Fh.
//!
//! Nothing nests a CPU loop: to reflect an interrupt or simulate a real-mode call (0300h-0302h)
//! the host saves the protected-mode state, switches the CPU to real mode and lets the handler
//! return into a real-mode stub, whose callback switches back. A DOS exit from protected mode
//! is then an ordinary real-mode termination, after which the client's state is dropped.
//!
//! A hardware interrupt that comes while the client is in real mode (in DOS, in a real-mode call)
//! goes to the client's protected-mode handler if it has one: the host switches to protected
//! mode, runs the handler on its own stack and switches back when it returns. The previous
//! handler 0204h gives a client to chain to is the host's default for that vector (reflection
//! to real mode), never the client's own.
//!
//! Not done: no VCPI, no DPMI 1.0 functions.

use std::collections::BTreeMap;

use cpu::{Regs, SegmentCache, SystemSegment, TableRegister, seg};

use crate::Machine;
use crate::bios::{ROM_SEGMENT, callback_instruction};

/// Real-mode stubs, in the ROM.
pub const RM_STUBS: u16 = 0x8800;
const RM_ENTRY: u16 = RM_STUBS;
const RM_RETURN: u16 = RM_STUBS + 0x10;
const RM_RAW: u16 = RM_STUBS + 0x20;
/// A RETF in the ROM (also the DOS case map routine).
pub(crate) const RM_RETF: u16 = RM_STUBS + 0x30;
const RM_CALLBACKS: u16 = RM_STUBS + 0x40;
const CALLBACKS: usize = 16;

/// Callback ids.
pub mod id {
    pub const ENTER: u16 = 0x0150;
    pub const RM_RETURN: u16 = 0x0151;
    pub const RAW_TO_PM: u16 = 0x0152;
    pub const RAW_TO_RM: u16 = 0x0153;
    pub const EXCEPTION_RETURN: u16 = 0x0154;
    pub const CALLBACK_RETURN: u16 = 0x0155;
    /// A protected-mode handler of an interrupt that came in real mode returned.
    pub const RM_IRQ_RETURN: u16 = 0x0156;
    pub const RM_CALLBACK: u16 = 0x0160;
    /// 0200h + vector: a protected-mode interrupt.
    pub const PM_INT: u16 = 0x0200;
    /// 0300h + vector: the host's default handler, which 0204h gives clients to chain to.
    pub const PM_DEFAULT: u16 = 0x0300;
}

/// The vectors the PIC's IRQ 0-15 come in on while a client runs.
pub const IRQ_BASE: u8 = 0x50;

// The host area (an XMS block): tables, stubs, a stack.
const GDT: u32 = 0x0000;
const IDT: u32 = 0x0100;
const LDT: u32 = 0x1000;
const LDT_ENTRIES: usize = 8192;
const STUBS: u32 = 0x11000;
const PM_RAW: u32 = STUBS + 0x800;
const PM_EXCEPTION_RETURN: u32 = STUBS + 0x810;
const PM_CALLBACK_RETURN: u32 = STUBS + 0x820;
const PM_RETF: u32 = STUBS + 0x830;
const PM_IRQ_RETURN: u32 = STUBS + 0x840;
/// The default handlers' stubs (6 bytes each: callback, IRET or IRETD).
const DEFAULT_STUBS: u32 = STUBS + 0x900;
/// Where on the host's stack the handlers of interrupts that came in real mode start (the
/// exception frames are above), 200h a level.
const RM_IRQ_STACK: u32 = 0x800;
const HOST_STACK: u32 = 0x13000;
const AREA_SIZE: u32 = 0x14000;
/// GDT selectors: the host's code (the stubs) and the LDT.
const HOST_CODE: u16 = 0x08;
const LDT_SELECTOR: u16 = 0x10;
const HOST_STACK_SELECTOR: u16 = 0x18;

/// The real-mode stack the host uses in the client's host data area (paragraphs it allocates).
const HOST_DATA_PARAGRAPHS: u16 = 0x0100;

const CF: u32 = 0x0001;
const IF: u32 = 0x0200;
/// The flags a reflected interrupt returns: OF, DF, SF, ZF, AF, PF, CF.
const RESULT_FLAGS: u32 = 0x0CD5;

mod error {
    pub const DESCRIPTOR_UNAVAILABLE: u16 = 0x8011;
    pub const LINEAR_MEMORY_UNAVAILABLE: u16 = 0x8012;
    pub const CALLBACK_UNAVAILABLE: u16 = 0x8015;
    pub const INVALID_VALUE: u16 = 0x8021;
    pub const INVALID_SELECTOR: u16 = 0x8022;
    pub const INVALID_HANDLE: u16 = 0x8023;
    pub const INVALID_CALLBACK: u16 = 0x8024;
    pub const UNSUPPORTED: u16 = 0x8001;
}

/// A protected-mode address: selector and offset.
type FarPointer = (u16, u32);

/// The host's state (in the DOS state, so it is hashed and cloned with the machine).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Dpmi {
    /// The host area's linear address and XMS handle, once a client has entered.
    area: Option<(u32, u16)>,
    client: Option<Client>,
    /// Protected-mode states waiting for a real-mode handler or call to come back.
    saved: Vec<Saved>,
    /// Real-mode states a hardware interrupt interrupted, while their protected-mode handlers run.
    interrupted: Vec<Regs>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Client {
    bits32: bool,
    psp: u16,
    /// The client's host data area: the real-mode stack for reflections.
    host_data: u16,
    /// How deep reflections are nested (each takes a part of the real-mode stack).
    depth: u16,
    ldt: Vec<bool>,
    /// Selectors made by 0002h, by segment.
    segments: BTreeMap<u16, u16>,
    /// 0205h: protected-mode interrupt handlers, by vector.
    vectors: BTreeMap<u8, (u16, u32)>,
    /// 0203h: exception handlers, by exception.
    exceptions: BTreeMap<u8, (u16, u32)>,
    /// 0501h: memory blocks by handle: (XMS handle, linear address, size).
    blocks: BTreeMap<u32, (u16, u32, u32)>,
    next_block: u32,
    /// 0100h: DOS memory by its first selector: (segment, selectors).
    dos_blocks: BTreeMap<u16, (u16, u16)>,
    /// 0303h: real-mode callbacks: the protected-mode procedure and the register structure.
    callbacks: Vec<Option<(FarPointer, FarPointer)>>,
    /// The environment segment the PSP had (it holds a selector while the client runs).
    environment: u16,
}

/// The protected-mode CPU state a host round trip to real mode comes back to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PmState {
    regs: Regs,
    segs: [SegmentCache; 6],
    gdtr: TableRegister,
    idtr: TableRegister,
    ldtr: SystemSegment,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Saved {
    /// A reflected interrupt: back to the stub's IRET, with the handler's registers and flags.
    Reflect(PmState),
    /// 0300h-0302h: the result goes to the register structure at this linear address.
    Call(PmState, u32),
    /// A real-mode callback running in protected mode (its real-mode state is in the structure).
    Callback,
}

impl Dpmi {
    pub fn active(&self) -> bool {
        self.client.is_some()
    }
}

/// Puts the real-mode stubs in the ROM.
pub(crate) fn install(m: &mut Machine) {
    let rom = |m: &mut Machine, at: u16, code: &[u8]| m.mem.load(Machine::lin(ROM_SEGMENT, at), code);
    // The mode switch: on failure the callback returns with CF set and the RETF goes back.
    rom(m, RM_ENTRY, &[&callback_instruction(id::ENTER)[..], &[0xCB]].concat());
    rom(m, RM_RETURN, &[&callback_instruction(id::RM_RETURN)[..], &[0xF4]].concat());
    rom(m, RM_RAW, &[&callback_instruction(id::RAW_TO_PM)[..], &[0xF4]].concat());
    rom(m, RM_RETF, &[0xCB]);
    for i in 0..CALLBACKS as u16 {
        rom(m, RM_CALLBACKS + 8 * i, &[&callback_instruction(id::RM_CALLBACK + i)[..], &[0xF4]].concat());
    }
}

/// INT 2Fh AX=1687h: the host's entry point (or a note that there is none).
pub(crate) fn detect(m: &mut Machine) {
    if !m.config.dpmi {
        m.diagnostic("the program looked for a DPMI host (INT 2Fh AX=1687h): rec-host run --dpmi provides one".into());
        return;
    }
    let r = &mut m.cpu.regs;
    r.set_ax(0);
    r.set_bx(1); // 32-bit programs supported
    r.set_cl(if m.cpu.model == cpu::Model::I386 { 3 } else { 4 });
    r.set_dx(0x005A); // version 0.90
    r.set_si(HOST_DATA_PARAGRAPHS);
    r.seg[seg::ES] = ROM_SEGMENT;
    r.set_di(RM_ENTRY);
}

/// Host callbacks of the DPMI host; false if `id` isn't one.
pub(crate) fn dispatch(m: &mut Machine, callback: u16) -> bool {
    match callback {
        id::ENTER => enter(m),
        id::RM_RETURN => rm_return(m),
        id::RAW_TO_PM => raw_to_pm(m),
        id::RAW_TO_RM => raw_to_rm(m),
        id::EXCEPTION_RETURN => exception_return(m),
        id::CALLBACK_RETURN => callback_return(m),
        id::RM_IRQ_RETURN => rm_irq_return(m),
        n if (id::RM_CALLBACK..id::RM_CALLBACK + CALLBACKS as u16).contains(&n) => {
            rm_callback(m, usize::from(n - id::RM_CALLBACK))
        }
        n if (id::PM_INT..id::PM_INT + 0x100).contains(&n) => pm_interrupt(m, (n - id::PM_INT) as u8),
        n if (id::PM_DEFAULT..id::PM_DEFAULT + 0x100).contains(&n) => default_interrupt(m, (n - id::PM_DEFAULT) as u8),
        _ => return false,
    }
    true
}

/// While a client runs in protected mode the PIC's vectors move to [`IRQ_BASE`].
pub(crate) fn irq_vector(m: &Machine, vector: u8) -> u8 {
    if !m.cpu.protected_mode() || !m.dos.dpmi.active() {
        return vector;
    }
    match vector {
        0x08..=0x0F => IRQ_BASE + vector - 0x08,
        0x70..=0x77 => IRQ_BASE + 8 + vector - 0x70,
        v => v,
    }
}

/// The client's process ended: its state goes (its DOS memory goes with the process).
pub(crate) fn terminated(m: &mut Machine, psp: u16) {
    let Some(client) = m.dos.dpmi.client.as_ref().filter(|c| c.psp == psp) else { return };
    let blocks: Vec<u16> = client.blocks.values().map(|b| b.0).collect();
    let environment = client.environment;
    for handle in blocks {
        crate::xms::free_locked(m, handle);
    }
    m.mem.write_u16(Machine::lin(psp, 0x2C), environment);
    m.dos.dpmi.client = None;
    m.dos.dpmi.saved.clear();
    m.dos.dpmi.interrupted.clear();
}

// ---- descriptors ----

/// A descriptor: base, limit (in bytes; above 1 MB page-granular), access byte, 32-bit.
fn descriptor(base: u32, limit: u32, access: u8, big: bool) -> u64 {
    let (limit, granular) = if limit > 0xF_FFFF { (limit >> 12, true) } else { (limit, false) };
    let flags = (u64::from(granular) << 3) | (u64::from(big) << 2);
    u64::from(limit & 0xFFFF)
        | u64::from(base & 0xFF_FFFF) << 16
        | u64::from(access) << 40
        | u64::from((limit >> 16) & 0xF) << 48
        | flags << 52
        | u64::from(base >> 24) << 56
}

fn base_of(d: u64) -> u32 {
    ((d >> 16) & 0xFF_FFFF) as u32 | ((d >> 56) as u32) << 24
}

fn limit_of(d: u64) -> u32 {
    let raw = (d & 0xFFFF) as u32 | (((d >> 48) & 0xF) as u32) << 16;
    if d & (1 << 55) != 0 { raw << 12 | 0xFFF } else { raw }
}

/// Writes physical memory: the host area is above 1 MB, where `Memory::load` doesn't reach.
fn put(m: &mut Machine, at: u32, data: &[u8]) {
    for (i, &byte) in data.iter().enumerate() {
        m.mem.write_phys(at + i as u32, byte);
    }
}

fn area(m: &Machine) -> u32 {
    m.dos.dpmi.area.map_or(0, |(base, _)| base)
}

/// The descriptor for `selector` (GDT or LDT), if it is in a table.
fn read_descriptor(m: &Machine, selector: u16) -> Option<u64> {
    let index = u32::from(selector >> 3);
    let at = if selector & 4 != 0 {
        if index as usize >= LDT_ENTRIES {
            return None;
        }
        area(m) + LDT + index * 8
    } else {
        if index >= 8 {
            return None;
        }
        area(m) + GDT + index * 8
    };
    Some(u64::from(m.mem.read_u32(at)) | u64::from(m.mem.read_u32(at + 4)) << 32)
}

fn write_ldt(m: &mut Machine, selector: u16, d: u64) {
    let at = area(m) + LDT + u32::from(selector >> 3) * 8;
    m.mem.write_u32(at, d as u32);
    m.mem.write_u32(at + 4, (d >> 32) as u32);
}

/// Loads segment register `index` with `selector` (its hidden part from the descriptor).
fn load(m: &mut Machine, index: usize, selector: u16) -> bool {
    if selector & !3 == 0 {
        m.cpu.regs.seg[index] = 0;
        m.cpu.segs[index] = SegmentCache { selector: 0, base: 0, limit: 0, big: false };
        return index != seg::CS && index != seg::SS;
    }
    let Some(d) = read_descriptor(m, selector).filter(|d| d & (1 << 47) != 0) else { return false };
    m.cpu.regs.seg[index] = selector;
    m.cpu.segs[index] = SegmentCache { selector, base: base_of(d), limit: limit_of(d), big: d & (1 << 54) != 0 };
    true
}

/// Sets a real-mode segment register (and its hidden part, by real-mode rules).
fn set_real(m: &mut Machine, index: usize, value: u16) {
    m.cpu.regs.seg[index] = value;
    m.cpu.segs[index] = SegmentCache { selector: value, base: u32::from(value) << 4, limit: 0xFFFF, big: false };
}

/// LDT selector for entry `index` (RPL 0).
fn selector(index: usize) -> u16 {
    (index as u16) << 3 | 4
}

/// `count` free LDT entries in a row (from entry 16: the first 16 are for 000Dh).
fn allocate_ldt(m: &mut Machine, count: usize) -> Option<u16> {
    let client = m.dos.dpmi.client.as_mut()?;
    let first = (16..LDT_ENTRIES.saturating_sub(count)).find(|&i| client.ldt[i..i + count].iter().all(|used| !used))?;
    client.ldt[first..first + count].fill(true);
    let data = if client.bits32 { descriptor(0, 0, 0x92, true) } else { descriptor(0, 0, 0x92, false) };
    for i in first..first + count {
        write_ldt(m, selector(i), data);
    }
    Some(selector(first))
}

fn free_ldt(m: &mut Machine, sel: u16) -> bool {
    let index = usize::from(sel >> 3);
    let Some(client) = m.dos.dpmi.client.as_mut() else { return false };
    if sel & 4 == 0 || index >= LDT_ENTRIES || !client.ldt[index] {
        return false;
    }
    client.ldt[index] = false;
    client.segments.retain(|_, s| *s != sel);
    write_ldt(m, sel, 0);
    true
}

fn in_use(m: &Machine, sel: u16) -> bool {
    let index = usize::from(sel >> 3);
    sel & 4 != 0 && index < LDT_ENTRIES && m.dos.dpmi.client.as_ref().is_some_and(|c| c.ldt[index])
}

/// A data selector for real-mode segment `segment`, limit 64 KB.
fn data_selector(m: &mut Machine, segment: u16, limit: u32) -> Option<u16> {
    let sel = allocate_ldt(m, 1)?;
    write_ldt(m, sel, descriptor(u32::from(segment) << 4, limit, 0x92, false));
    Some(sel)
}

// ---- mode switches ----

fn save_pm(m: &Machine) -> PmState {
    let c = &m.cpu;
    PmState { regs: c.regs, segs: c.segs, gdtr: c.gdtr, idtr: c.idtr, ldtr: c.ldtr }
}

fn restore_pm(m: &mut Machine, state: &PmState) {
    let c = &mut m.cpu;
    c.cr0 |= cpu::cr0::PE;
    c.regs = state.regs;
    c.segs = state.segs;
    (c.gdtr, c.idtr, c.ldtr, c.cpl) = (state.gdtr, state.idtr, state.ldtr, 0);
    c.flush_code_cache();
    m.set_a20(true);
}

/// Protected mode with the host's tables (segment registers to be loaded by the caller).
fn tables_pm(m: &mut Machine) {
    let base = area(m);
    let c = &mut m.cpu;
    c.cr0 |= cpu::cr0::PE;
    c.gdtr = TableRegister { base: base + GDT, limit: 8 * 8 - 1 };
    c.idtr = TableRegister { base: base + IDT, limit: 256 * 8 - 1 };
    c.ldtr =
        SystemSegment { selector: LDT_SELECTOR, base: base + LDT, limit: (LDT_ENTRIES * 8 - 1) as u32, is386: false };
    c.cpl = 0;
    c.flush_code_cache();
    m.set_a20(true);
}

/// Real mode: the interrupt vector table again; segment registers set by the caller.
fn to_real(m: &mut Machine) {
    let c = &mut m.cpu;
    c.cr0 &= !cpu::cr0::PE;
    c.idtr = TableRegister { base: 0, limit: 0x3FF };
    c.cpl = 0;
    for i in [seg::FS, seg::GS] {
        set_real(m, i, 0);
    }
    m.cpu.flush_code_cache();
}

/// The real-mode stack for a host round trip: below the ones in progress.
fn rm_stack(m: &mut Machine) -> (u16, u16) {
    let client = m.dos.dpmi.client.as_mut().expect("client");
    let top = HOST_DATA_PARAGRAPHS * 16 - 2 - client.depth * 0x200;
    client.depth = (client.depth + 1).min(7);
    (client.host_data, top)
}

fn rm_stack_done(m: &mut Machine) {
    if let Some(client) = m.dos.dpmi.client.as_mut() {
        client.depth = client.depth.saturating_sub(1);
    }
}

fn push_rm(m: &mut Machine, value: u16) {
    let sp = m.cpu.regs.sp().wrapping_sub(2);
    m.cpu.regs.set_sp(sp);
    m.mem.write_u16(Machine::lin(m.cpu.regs.ss(), sp), value);
}

/// The client's stack top (SS:SP or SS:ESP) as a linear address.
fn stack(m: &Machine) -> u32 {
    let sp = if m.cpu.stack32() { m.cpu.regs.gpr[4] } else { u32::from(m.cpu.regs.sp()) };
    m.cpu.segment_base(seg::SS).wrapping_add(sp)
}

fn bits32(m: &Machine) -> bool {
    m.dos.dpmi.client.as_ref().is_some_and(|c| c.bits32)
}

/// The interrupt frame the gate pushed (at the stub): where its flags are.
fn frame_flags_at(m: &Machine, error_code: bool) -> u32 {
    let word = if bits32(m) { 4 } else { 2 };
    stack(m) + word * (2 + u32::from(error_code))
}

fn set_frame_carry(m: &mut Machine, carry: bool) {
    let at = frame_flags_at(m, false);
    let flags = m.mem.read_u16(at);
    m.mem.write_u16(at, if carry { flags | 1 } else { flags & !1 });
}

/// ES:(E)DI, DS:(E)SI and friends: a protected-mode pointer as a linear address.
fn pointer(m: &Machine, segment: usize, reg: usize) -> u32 {
    let offset = if bits32(m) { m.cpu.regs.gpr[reg] } else { m.cpu.regs.gpr[reg] & 0xFFFF };
    m.cpu.segment_base(segment).wrapping_add(offset)
}

// ---- entry ----

/// The mode switch entry (a far call from real mode, AX bit 0: a 32-bit client, ES: its host
/// data area): protected mode, with selectors for CS, DS, SS, the PSP (ES) and the environment.
fn enter(m: &mut Machine) {
    let r = m.cpu.regs;
    let fail = |m: &mut Machine, why: &str| {
        m.diagnostic(format!("DPMI: the mode switch failed: {why}"));
        m.cpu.regs.flags |= CF;
    };
    if m.dos.dpmi.client.is_some() {
        return fail(m, "a client is already running (one at a time)");
    }
    if m.dos.dpmi.area.is_none() {
        match crate::xms::allocate_locked(m, AREA_SIZE) {
            Ok((handle, base)) => m.dos.dpmi.area = Some((base, handle)),
            Err(_) => return fail(m, "no extended memory for the host's tables"),
        }
    }
    // The host area is above 1 MB: without A20 its addresses would wrap into low memory.
    m.set_a20(true);
    let bits32 = r.ax() & 1 != 0;
    let psp = m.dos.psp;
    let environment = m.mem.read_u16(Machine::lin(psp, 0x2C));
    m.dos.dpmi.client = Some(Client {
        bits32,
        psp,
        host_data: r.es(),
        depth: 0,
        ldt: vec![false; LDT_ENTRIES],
        segments: BTreeMap::new(),
        vectors: BTreeMap::new(),
        exceptions: BTreeMap::new(),
        blocks: BTreeMap::new(),
        next_block: 1,
        dos_blocks: BTreeMap::new(),
        callbacks: vec![None; CALLBACKS],
        environment,
    });
    build_tables(m, bits32);
    // The caller's return address (a far call), then its segments as selectors.
    let (ss, sp) = (r.ss(), r.sp());
    let (ip, cs) = (m.mem.read_u16(Machine::lin(ss, sp)), m.mem.read_u16(Machine::lin(ss, sp.wrapping_add(2))));
    let code = allocate_ldt(m, 1).expect("an empty LDT");
    write_ldt(m, code, descriptor(u32::from(cs) << 4, 0xFFFF, 0x9A, false));
    let data = data_selector(m, r.ds(), 0xFFFF).expect("LDT");
    let stack_selector = if r.ss() == r.ds() { data } else { data_selector(m, r.ss(), 0xFFFF).expect("LDT") };
    let psp_selector = data_selector(m, psp, 0xFF).expect("LDT");
    if environment != 0 {
        let env = data_selector(m, environment, 0xFFFF).expect("LDT");
        m.mem.write_u16(Machine::lin(psp, 0x2C), env);
    }
    tables_pm(m);
    load(m, seg::CS, code);
    load(m, seg::DS, data);
    load(m, seg::SS, stack_selector);
    load(m, seg::ES, psp_selector);
    load(m, seg::FS, 0);
    load(m, seg::GS, 0);
    m.cpu.regs.ip = u32::from(ip);
    m.cpu.regs.gpr[4] = u32::from(sp.wrapping_add(4));
    m.cpu.regs.flags &= !CF;
}

/// The GDT (host code, LDT), the IDT (a stub per vector) and the stubs.
fn build_tables(m: &mut Machine, bits32: bool) {
    let base = area(m);
    put(m, base, &vec![0; (STUBS + 0x1000) as usize]);
    let gdt = [
        0,
        descriptor(base + STUBS, 0xFFFF, 0x9A, false),
        descriptor(base + LDT, (LDT_ENTRIES * 8 - 1) as u32, 0x82, false),
        descriptor(base + HOST_STACK - 0x1000, 0xFFF, 0x92, false),
    ];
    for (i, d) in gdt.iter().enumerate() {
        m.mem.write_u32(base + GDT + 8 * i as u32, *d as u32);
        m.mem.write_u32(base + GDT + 8 * i as u32 + 4, (*d >> 32) as u32);
    }
    // Interrupt gates (286 or 386, as the client) to stub n: callback, IRET (IRETD).
    let gate_kind: u64 = if bits32 { 0x8E } else { 0x86 };
    for n in 0..256u32 {
        let offset = n * 8;
        let gate =
            u64::from(offset & 0xFFFF) | u64::from(HOST_CODE) << 16 | gate_kind << 40 | u64::from(offset >> 16) << 48;
        m.mem.write_u32(base + IDT + n * 8, gate as u32);
        m.mem.write_u32(base + IDT + n * 8 + 4, (gate >> 32) as u32);
        let mut stub = callback_instruction(id::PM_INT + n as u16).to_vec();
        stub.extend(if bits32 { &[0x66, 0xCF][..] } else { &[0xCF][..] });
        put(m, base + STUBS + offset, &stub);
    }
    let retf: &[u8] = if bits32 { &[0x66, 0xCB] } else { &[0xCB] };
    put(m, base + PM_RAW, &[&callback_instruction(id::RAW_TO_RM)[..], &[0xF4]].concat());
    put(m, base + PM_EXCEPTION_RETURN, &[&callback_instruction(id::EXCEPTION_RETURN)[..], &[0xF4]].concat());
    put(m, base + PM_CALLBACK_RETURN, &[&callback_instruction(id::CALLBACK_RETURN)[..], &[0xF4]].concat());
    put(m, base + PM_RETF, retf);
    put(m, base + PM_IRQ_RETURN, &[&callback_instruction(id::RM_IRQ_RETURN)[..], &[0xF4]].concat());
    for n in 0..256u32 {
        let mut stub = callback_instruction(id::PM_DEFAULT + n as u16).to_vec();
        stub.extend(if bits32 { &[0x66, 0xCF][..] } else { &[0xCF][..] });
        put(m, base + DEFAULT_STUBS + 6 * n, &stub);
    }
}

// ---- interrupts in protected mode ----

/// Exceptions that push an error code.
fn has_error_code(vector: u8) -> bool {
    matches!(vector, 0x08 | 0x0A..=0x0E | 0x11)
}

fn pm_interrupt(m: &mut Machine, vector: u8) {
    if m.dos.dpmi.client.is_none() {
        return;
    }
    match vector {
        0x31 => services(m),
        v if (IRQ_BASE..IRQ_BASE + 16).contains(&v) => {
            let irq = v - IRQ_BASE;
            let real = if irq < 8 { 0x08 + irq } else { 0x70 + irq - 8 };
            interrupt(m, real);
        }
        0x00..=0x0F => exception(m, vector),
        0x2F if m.cpu.regs.ax() == 0x1686 => m.cpu.regs.set_ax(0), // in protected mode
        v => interrupt(m, v),
    }
}

/// The host's default handler of `vector`, which a client's handler chains to (an INT frame in
/// place): what an interrupt the client didn't hook does, without asking the client's table.
fn default_interrupt(m: &mut Machine, vector: u8) {
    if m.dos.dpmi.client.is_none() {
        return;
    }
    match vector {
        0x31 => services(m),
        0x2F if m.cpu.regs.ax() == 0x1686 => m.cpu.regs.set_ax(0),
        v => reflect(m, v),
    }
}

/// A hardware interrupt in real mode while a client runs: to its protected-mode handler, if it
/// has one for the vector, on the host's stack; [`rm_irq_return`] comes back. False: the
/// real-mode handler takes it as usual.
pub(crate) fn real_mode_irq(m: &mut Machine, vector: u8) -> bool {
    if m.cpu.protected_mode() || !matches!(vector, 0x08..=0x0F | 0x70..=0x77) {
        return false;
    }
    let Some((selector, offset, wide)) =
        m.dos.dpmi.client.as_ref().and_then(|c| c.vectors.get(&vector).map(|&(s, o)| (s, o, c.bits32)))
    else {
        return false;
    };
    let level = m.dos.dpmi.interrupted.len() as u32;
    if level >= 4 {
        return false;
    }
    let interrupted = m.cpu.regs;
    m.dos.dpmi.interrupted.push(interrupted);
    tables_pm(m);
    load(m, seg::SS, HOST_STACK_SELECTOR);
    for i in [seg::DS, seg::ES, seg::FS, seg::GS] {
        load(m, i, 0);
    }
    // An interrupt frame returning into the host's stub.
    let stack_base = area(m) + HOST_STACK - 0x1000;
    let mut sp = RM_IRQ_STACK - 0x200 * level;
    let flags = interrupted.flags & !(IF | 0x0100);
    for value in [flags, u32::from(HOST_CODE), PM_IRQ_RETURN - STUBS] {
        if wide {
            sp -= 4;
            m.mem.write_u32(stack_base + sp, value);
        } else {
            sp -= 2;
            m.mem.write_u16(stack_base + sp, value as u16);
        }
    }
    m.cpu.regs.gpr[4] = sp;
    m.cpu.regs.flags &= !(IF | 0x0100);
    if !load(m, seg::CS, selector) {
        m.dos.dpmi.interrupted.pop();
        to_real(m);
        m.cpu.regs = interrupted;
        for i in [seg::ES, seg::CS, seg::SS, seg::DS, seg::FS, seg::GS] {
            set_real(m, i, interrupted.seg[i]);
        }
        return false;
    }
    m.cpu.regs.ip = offset;
    true
}

/// The protected-mode handler of an interrupt that came in real mode returned: real mode again,
/// where the interrupt came.
fn rm_irq_return(m: &mut Machine) {
    let Some(interrupted) = m.dos.dpmi.interrupted.pop() else {
        m.diagnostic("DPMI: a return to real mode with nothing to return to".into());
        return;
    };
    to_real(m);
    m.cpu.regs = interrupted;
    for i in [seg::ES, seg::CS, seg::SS, seg::DS, seg::FS, seg::GS] {
        set_real(m, i, interrupted.seg[i]);
    }
}

/// An interrupt: to the client's handler if it has one (the frame is in place: its IRET goes
/// back to where the interrupt came), else reflected to real mode.
fn interrupt(m: &mut Machine, vector: u8) {
    let handler = m.dos.dpmi.client.as_ref().and_then(|c| c.vectors.get(&vector).copied());
    match handler {
        Some((selector, offset)) => {
            if load(m, seg::CS, selector) {
                m.cpu.regs.ip = offset;
            }
        }
        None => reflect(m, vector),
    }
}

/// Runs real-mode interrupt `vector`'s handler; [`rm_return`] comes back to the stub's IRET.
fn reflect(m: &mut Machine, vector: u8) {
    let state = save_pm(m);
    let flags_at = frame_flags_at(m, false);
    let flags = m.mem.read_u16(flags_at);
    m.dos.dpmi.saved.push(Saved::Reflect(state));
    let (ss, sp) = rm_stack(m);
    to_real(m);
    set_real(m, seg::SS, ss);
    m.cpu.regs.gpr[4] = u32::from(sp);
    set_real(m, seg::DS, ss);
    set_real(m, seg::ES, ss);
    // An INT: the client's flags, then a return into the host's stub.
    push_rm(m, flags);
    push_rm(m, ROM_SEGMENT);
    push_rm(m, RM_RETURN);
    m.cpu.regs.flags &= !(IF | 0x0100);
    let at = u32::from(vector) * 4;
    set_real(m, seg::CS, m.mem.read_u16(at + 2));
    m.cpu.regs.ip = u32::from(m.mem.read_u16(at));
}

/// A real-mode handler or call came back into the host's stub: protected mode again.
fn rm_return(m: &mut Machine) {
    let Some(saved) = m.dos.dpmi.saved.pop() else {
        m.diagnostic("DPMI: a return to protected mode with nothing to return to".into());
        return;
    };
    rm_stack_done(m);
    let result = m.cpu.regs;
    match saved {
        Saved::Reflect(state) => {
            restore_pm(m, &state);
            m.cpu.regs.gpr = result.gpr;
            m.cpu.regs.gpr[4] = state.regs.gpr[4];
            let at = frame_flags_at(m, false);
            let flags = u32::from(m.mem.read_u16(at));
            m.mem.write_u16(at, ((flags & !RESULT_FLAGS) | (result.flags & RESULT_FLAGS)) as u16);
        }
        Saved::Call(state, structure) => {
            write_structure(m, structure, &result);
            restore_pm(m, &state);
            set_frame_carry(m, false);
        }
        Saved::Callback => m.diagnostic("DPMI: a real-mode return inside a callback".into()),
    }
}

/// The real-mode register structure of 0300h-0302h and callbacks (50 bytes).
fn read_structure(m: &Machine, at: u32, r: &mut Regs) {
    for (i, reg) in [7usize, 6, 5].into_iter().enumerate() {
        r.gpr[reg] = m.mem.read_u32(at + 4 * i as u32);
    }
    for (i, reg) in [3usize, 2, 1, 0].into_iter().enumerate() {
        r.gpr[reg] = m.mem.read_u32(at + 0x10 + 4 * i as u32);
    }
    r.flags = (r.flags & 0xFFFF_0000) | u32::from(m.mem.read_u16(at + 0x20)) | 2;
}

fn write_structure(m: &mut Machine, at: u32, r: &Regs) {
    for (i, reg) in [7usize, 6, 5].into_iter().enumerate() {
        m.mem.write_u32(at + 4 * i as u32, r.gpr[reg]);
    }
    for (i, reg) in [3usize, 2, 1, 0].into_iter().enumerate() {
        m.mem.write_u32(at + 0x10 + 4 * i as u32, r.gpr[reg]);
    }
    m.mem.write_u16(at + 0x20, r.flags as u16);
    for (i, s) in [seg::ES, seg::DS, seg::FS, seg::GS].into_iter().enumerate() {
        m.mem.write_u16(at + 0x22 + 2 * i as u32, r.seg[s]);
    }
}

/// A CPU exception: the client's handler (with DPMI's exception frame on the host's stack),
/// the real-mode handler for 0-7 (as hosts do by default), or the end of the client.
fn exception(m: &mut Machine, vector: u8) {
    let handler = m.dos.dpmi.client.as_ref().and_then(|c| c.exceptions.get(&vector).copied());
    let code = has_error_code(vector);
    let wide = bits32(m);
    let word = if wide { 4 } else { 2 };
    let frame = stack(m);
    let read = |m: &Machine, i: u32| {
        if wide { m.mem.read_u32(frame + i * word) } else { u32::from(m.mem.read_u16(frame + i * word)) }
    };
    let error = if code { read(m, 0) } else { 0 };
    let first = u32::from(code);
    let (eip, cs, eflags) = (read(m, first), read(m, first + 1) as u16, read(m, first + 2));
    let client_sp = if m.cpu.stack32() { m.cpu.regs.gpr[4] } else { u32::from(m.cpu.regs.sp()) } + (first + 3) * word;
    let client_ss = m.cpu.regs.ss();
    match handler {
        Some((selector, offset)) => {
            // Exception frame: return to the host, error code, EIP, CS, EFLAGS, ESP, SS.
            load(m, seg::SS, HOST_STACK_SELECTOR);
            m.cpu.regs.gpr[4] = 0x1000;
            let base = m.cpu.segment_base(seg::SS);
            let mut sp = m.cpu.regs.gpr[4];
            for value in [
                u32::from(client_ss),
                client_sp,
                eflags,
                u32::from(cs),
                eip,
                error,
                u32::from(HOST_CODE),
                PM_EXCEPTION_RETURN - STUBS,
            ] {
                sp -= word;
                if wide { m.mem.write_u32(base + sp, value) } else { m.mem.write_u16(base + sp, value as u16) }
            }
            m.cpu.regs.gpr[4] = sp;
            if load(m, seg::CS, selector) {
                m.cpu.regs.ip = offset;
            }
        }
        None if vector < 8 && !code => reflect(m, vector),
        None => {
            m.diagnostic(format!(
                "DPMI: exception {vector:02X}h at {cs:04X}:{eip:08X} (error {error:X}): the client ends"
            ));
            // End it the way DOS would: INT 21h AX=4CFFh in real mode.
            m.dos.dpmi.saved.clear();
            let (ss, sp) = rm_stack(m);
            to_real(m);
            set_real(m, seg::SS, ss);
            m.cpu.regs.gpr[4] = u32::from(sp);
            m.cpu.regs.set_ax(0x4CFF);
            push_rm(m, 0);
            push_rm(m, ROM_SEGMENT);
            push_rm(m, RM_RETURN);
            let at = 0x21 * 4;
            set_real(m, seg::CS, m.mem.read_u16(at + 2));
            m.cpu.regs.ip = u32::from(m.mem.read_u16(at));
        }
    }
}

/// The client's exception handler returned (RETF to the host): go on at the frame's CS:EIP.
fn exception_return(m: &mut Machine) {
    let wide = bits32(m);
    let word = if wide { 4 } else { 2 };
    let frame = stack(m);
    let read = |m: &Machine, i: u32| {
        if wide { m.mem.read_u32(frame + i * word) } else { u32::from(m.mem.read_u16(frame + i * word)) }
    };
    let (eip, cs, eflags, esp, ss) = (read(m, 1), read(m, 2) as u16, read(m, 3), read(m, 4), read(m, 5) as u16);
    load(m, seg::SS, ss);
    m.cpu.regs.gpr[4] = esp;
    load(m, seg::CS, cs);
    m.cpu.regs.ip = eip;
    m.cpu.regs.flags = eflags | 2;
}

// ---- INT 31h ----

fn services(m: &mut Machine) {
    let result = service(m);
    match result {
        Ok(()) => set_frame_carry(m, false),
        Err(code) => {
            m.cpu.regs.set_ax(code);
            set_frame_carry(m, true);
        }
    }
}

fn dx_cx(r: &Regs) -> u32 {
    u32::from(r.cx()) << 16 | u32::from(r.dx())
}

fn bx_cx(r: &Regs) -> u32 {
    u32::from(r.bx()) << 16 | u32::from(r.cx())
}

fn set_bx_cx(m: &mut Machine, value: u32) {
    m.cpu.regs.set_bx((value >> 16) as u16);
    m.cpu.regs.set_cx(value as u16);
}

fn service(m: &mut Machine) -> Result<(), u16> {
    let r = m.cpu.regs;
    let wide = bits32(m);
    match r.ax() {
        // Descriptors.
        0x0000 => {
            let count = usize::from(r.cx()).max(1);
            let sel = allocate_ldt(m, count).ok_or(error::DESCRIPTOR_UNAVAILABLE)?;
            m.cpu.regs.set_ax(sel);
        }
        0x0001 => {
            if !free_ldt(m, r.bx()) {
                return Err(error::INVALID_SELECTOR);
            }
        }
        0x0002 => {
            let segment = r.bx();
            let existing = m.dos.dpmi.client.as_ref().and_then(|c| c.segments.get(&segment).copied());
            let sel = match existing {
                Some(sel) => sel,
                None => {
                    let sel = data_selector(m, segment, 0xFFFF).ok_or(error::DESCRIPTOR_UNAVAILABLE)?;
                    m.dos.dpmi.client.as_mut().expect("client").segments.insert(segment, sel);
                    sel
                }
            };
            m.cpu.regs.set_ax(sel);
        }
        0x0003 => m.cpu.regs.set_ax(8),
        0x0006 => {
            let d = read_descriptor(m, r.bx()).filter(|_| in_use(m, r.bx())).ok_or(error::INVALID_SELECTOR)?;
            let base = base_of(d);
            m.cpu.regs.set_cx((base >> 16) as u16);
            m.cpu.regs.set_dx(base as u16);
        }
        0x0007..=0x0009 => {
            if !in_use(m, r.bx()) {
                return Err(error::INVALID_SELECTOR);
            }
            let d = read_descriptor(m, r.bx()).expect("in use");
            let (mut base, mut limit, mut access, mut big) =
                (base_of(d), limit_of(d), (d >> 40) as u8, d & (1 << 54) != 0);
            match r.ax() {
                0x0007 => base = dx_cx(&r),
                0x0008 => {
                    limit = dx_cx(&r);
                    if limit > 0xF_FFFF && limit & 0xFFF != 0xFFF {
                        return Err(error::INVALID_VALUE);
                    }
                }
                _ => {
                    // CL: the access byte (present, a code or data segment); CH bit 6: 32-bit.
                    if r.cl() & 0x10 == 0 {
                        return Err(error::INVALID_VALUE);
                    }
                    access = r.cl() & !0x60; // DPL 0, as the client runs
                    big = r.ch() & 0x40 != 0;
                    if r.ch() & 0x80 != 0 && limit <= 0xF_FFFF {
                        limit = limit << 12 | 0xFFF;
                    }
                }
            }
            write_ldt(m, r.bx(), descriptor(base, limit, access, big));
            reload(m, r.bx());
        }
        0x000A => {
            if !in_use(m, r.bx()) {
                return Err(error::INVALID_SELECTOR);
            }
            let d = read_descriptor(m, r.bx()).expect("in use");
            let alias = allocate_ldt(m, 1).ok_or(error::DESCRIPTOR_UNAVAILABLE)?;
            write_ldt(m, alias, descriptor(base_of(d), limit_of(d), 0x92, d & (1 << 54) != 0));
            m.cpu.regs.set_ax(alias);
        }
        0x000B => {
            let d = read_descriptor(m, r.bx()).filter(|_| in_use(m, r.bx())).ok_or(error::INVALID_SELECTOR)?;
            let at = pointer(m, seg::ES, 7);
            m.mem.write_u32(at, d as u32);
            m.mem.write_u32(at + 4, (d >> 32) as u32);
        }
        0x000C => {
            if !in_use(m, r.bx()) {
                return Err(error::INVALID_SELECTOR);
            }
            let at = pointer(m, seg::ES, 7);
            let d = u64::from(m.mem.read_u32(at)) | u64::from(m.mem.read_u32(at + 4)) << 32;
            // DPL 0 whatever was asked.
            write_ldt(m, r.bx(), d & !(3 << 45));
            reload(m, r.bx());
        }
        0x000D => {
            let index = usize::from(r.bx() >> 3);
            let client = m.dos.dpmi.client.as_mut().expect("client");
            if r.bx() & 4 == 0 || index >= 16 || client.ldt[index] {
                return Err(error::DESCRIPTOR_UNAVAILABLE);
            }
            client.ldt[index] = true;
            let data = descriptor(0, 0, 0x92, client.bits32);
            write_ldt(m, r.bx(), data);
        }
        // DOS memory.
        0x0100 => {
            let paragraphs = r.bx();
            let psp = m.dos.dpmi.client.as_ref().expect("client").psp;
            match crate::dos::allocate(m, paragraphs, psp) {
                Ok(segment) => {
                    let count = usize::from(paragraphs).div_ceil(0x1000).max(1);
                    let Some(first) = allocate_ldt(m, count) else {
                        let _ = crate::dos::free(m, segment);
                        return Err(error::DESCRIPTOR_UNAVAILABLE);
                    };
                    for i in 0..count as u16 {
                        let base = (u32::from(segment) << 4) + u32::from(i) * 0x10000;
                        let size = (u32::from(paragraphs) * 16).saturating_sub(u32::from(i) * 0x10000);
                        write_ldt(m, first + i * 8, descriptor(base, size.clamp(1, 0x10000) - 1, 0x92, false));
                    }
                    m.dos.dpmi.client.as_mut().expect("client").dos_blocks.insert(first, (segment, count as u16));
                    m.cpu.regs.set_ax(segment);
                    m.cpu.regs.set_dx(first);
                }
                Err((code, largest)) => {
                    m.cpu.regs.set_bx(largest);
                    return Err(code);
                }
            }
        }
        0x0101 | 0x0102 => {
            let block = m.dos.dpmi.client.as_ref().and_then(|c| c.dos_blocks.get(&r.dx()).copied());
            let (segment, count) = block.ok_or(error::INVALID_SELECTOR)?;
            if r.ax() == 0x0101 {
                crate::dos::free(m, segment)?;
                for i in 0..count {
                    free_ldt(m, r.dx() + i * 8);
                }
                m.dos.dpmi.client.as_mut().expect("client").dos_blocks.remove(&r.dx());
            } else {
                if usize::from(r.bx()).div_ceil(0x1000).max(1) > usize::from(count) {
                    return Err(error::DESCRIPTOR_UNAVAILABLE);
                }
                crate::dos::resize(m, segment, r.bx()).map_err(|(code, largest)| {
                    m.cpu.regs.set_bx(largest);
                    code
                })?;
                let size = u32::from(r.bx()) * 16;
                write_ldt(m, r.dx(), descriptor(u32::from(segment) << 4, size.clamp(1, 0x10000) - 1, 0x92, false));
            }
        }
        // Interrupt vectors.
        0x0200 => {
            let at = u32::from(r.bl()) * 4;
            m.cpu.regs.set_cx(m.mem.read_u16(at + 2));
            m.cpu.regs.set_dx(m.mem.read_u16(at));
        }
        0x0201 => {
            let at = u32::from(r.bl()) * 4;
            m.mem.write_u16(at, r.dx());
            m.mem.write_u16(at + 2, r.cx());
        }
        0x0202 | 0x0204 => {
            let vector = r.bl();
            let table = |c: &Client| {
                if r.ax() == 0x0202 { c.exceptions.get(&vector).copied() } else { c.vectors.get(&vector).copied() }
            };
            if r.ax() == 0x0202 && vector >= 0x20 {
                return Err(error::INVALID_VALUE);
            }
            // The host's own handler: for exceptions the gate's stub, for interrupts the default
            // handler, which doesn't ask the client's table again (a client chains to it).
            let default =
                if r.ax() == 0x0202 { u32::from(vector) * 8 } else { DEFAULT_STUBS - STUBS + 6 * u32::from(vector) };
            let (selector, offset) = m.dos.dpmi.client.as_ref().and_then(table).unwrap_or((HOST_CODE, default));
            m.cpu.regs.set_cx(selector);
            m.cpu.regs.gpr[2] = if wide { offset } else { (m.cpu.regs.gpr[2] & 0xFFFF_0000) | (offset & 0xFFFF) };
        }
        0x0203 | 0x0205 => {
            let vector = r.bl();
            if r.ax() == 0x0203 && vector >= 0x20 {
                return Err(error::INVALID_VALUE);
            }
            let offset = if wide { r.gpr[2] } else { u32::from(r.dx()) };
            let client = m.dos.dpmi.client.as_mut().expect("client");
            let table = if r.ax() == 0x0203 { &mut client.exceptions } else { &mut client.vectors };
            // The host's own stub back: no handler.
            if r.cx() == HOST_CODE {
                table.remove(&vector);
            } else {
                table.insert(vector, (r.cx(), offset));
            }
        }
        // Real mode.
        0x0300..=0x0302 => return real_mode_call(m),
        0x0303 => {
            let procedure = (r.ds(), if wide { r.gpr[6] } else { u32::from(r.si()) });
            let structure = (r.es(), if wide { r.gpr[7] } else { u32::from(r.di()) });
            let client = m.dos.dpmi.client.as_mut().expect("client");
            let slot = client.callbacks.iter().position(Option::is_none).ok_or(error::CALLBACK_UNAVAILABLE)?;
            client.callbacks[slot] = Some((procedure, structure));
            m.cpu.regs.set_cx(ROM_SEGMENT);
            m.cpu.regs.set_dx(RM_CALLBACKS + 8 * slot as u16);
        }
        0x0304 => {
            let slot = r.dx().wrapping_sub(RM_CALLBACKS) / 8;
            let client = m.dos.dpmi.client.as_mut().expect("client");
            if r.cx() != ROM_SEGMENT || usize::from(slot) >= CALLBACKS || client.callbacks[usize::from(slot)].is_none()
            {
                return Err(error::INVALID_CALLBACK);
            }
            client.callbacks[usize::from(slot)] = None;
        }
        0x0305 => {
            // No state to save: both addresses are a far return.
            m.cpu.regs.set_ax(0);
            m.cpu.regs.set_bx(ROM_SEGMENT);
            m.cpu.regs.set_cx(RM_RETF);
            m.cpu.regs.set_si(HOST_CODE);
            m.cpu.regs.gpr[7] = PM_RETF - STUBS;
        }
        0x0306 => {
            m.cpu.regs.set_bx(ROM_SEGMENT);
            m.cpu.regs.set_cx(RM_RAW);
            m.cpu.regs.set_si(HOST_CODE);
            m.cpu.regs.gpr[7] = PM_RAW - STUBS;
        }
        0x0400 => {
            m.cpu.regs.set_ax(0x005A);
            // 32-bit host, returns to real mode for interrupts (not V86), no virtual memory.
            m.cpu.regs.set_bx(0b011);
            m.cpu.regs.set_cl(if m.cpu.model == cpu::Model::I386 { 3 } else { 4 });
            m.cpu.regs.set_dx(0x0870); // the PIC's real-mode bases
        }
        // Memory.
        0x0500 => {
            let (largest, total) = m.xms.free_kb();
            let at = pointer(m, seg::ES, 7);
            put(m, at, &[0xFF; 0x30]);
            m.mem.write_u32(at, largest * 1024);
            m.mem.write_u32(at + 0x14, total / 4);
            m.mem.write_u32(at + 0x18, (m.config.extended_kb + 1024) / 4);
        }
        0x0501 => {
            let size = bx_cx(&r);
            if size == 0 {
                return Err(error::INVALID_VALUE);
            }
            let (xms, base) = crate::xms::allocate_locked(m, size).map_err(|_| error::LINEAR_MEMORY_UNAVAILABLE)?;
            let client = m.dos.dpmi.client.as_mut().expect("client");
            let handle = client.next_block;
            client.next_block += 1;
            client.blocks.insert(handle, (xms, base, size));
            set_bx_cx(m, base);
            m.cpu.regs.set_si((handle >> 16) as u16);
            m.cpu.regs.set_di(handle as u16);
        }
        0x0502 => {
            let handle = u32::from(r.si()) << 16 | u32::from(r.di());
            let (xms, ..) =
                m.dos.dpmi.client.as_mut().expect("client").blocks.remove(&handle).ok_or(error::INVALID_HANDLE)?;
            crate::xms::free_locked(m, xms);
        }
        0x0503 => {
            let handle = u32::from(r.si()) << 16 | u32::from(r.di());
            let (xms, _, _) =
                *m.dos.dpmi.client.as_ref().expect("client").blocks.get(&handle).ok_or(error::INVALID_HANDLE)?;
            let size = bx_cx(&r);
            let base = crate::xms::resize_locked(m, xms, size).map_err(|_| error::LINEAR_MEMORY_UNAVAILABLE)?;
            m.dos.dpmi.client.as_mut().expect("client").blocks.insert(handle, (xms, base, size));
            set_bx_cx(m, base);
        }
        // Locking, paging and physical addresses: nothing pages, so all is resident.
        0x0600..=0x0603 | 0x0702 | 0x0703 | 0x0801 => {}
        0x0604 => set_bx_cx(m, 4096),
        0x0800 => {} // linear = physical: BX:CX stays
        // The virtual interrupt flag is the real one (the frame's, which IRET restores).
        0x0900..=0x0902 => {
            let at = frame_flags_at(m, false);
            let flags = m.mem.read_u16(at);
            m.cpu.regs.set_al(u8::from(u32::from(flags) & IF != 0));
            match r.ax() {
                0x0900 => m.mem.write_u16(at, flags & !(IF as u16)),
                0x0901 => m.mem.write_u16(at, flags | IF as u16),
                _ => {}
            }
        }
        0x0E00 => m.cpu.regs.set_ax(if m.cpu.fpu.is_some() { 0x0045 } else { 0 }),
        0x0E01 => {}
        function => {
            m.diagnostic(format!("DPMI: INT 31h AX={function:04X}h is not implemented"));
            return Err(error::UNSUPPORTED);
        }
    }
    Ok(())
}

/// After a descriptor changed: segment registers holding its selector see the change.
fn reload(m: &mut Machine, sel: u16) {
    for i in [seg::DS, seg::ES, seg::FS, seg::GS] {
        if m.cpu.regs.seg[i] == sel {
            load(m, i, sel);
        }
    }
}

/// 0300h (an interrupt), 0301h (a far procedure) and 0302h (one ending in IRET) in real mode,
/// with the registers of the structure at ES:(E)DI and CX words from the protected-mode stack.
fn real_mode_call(m: &mut Machine) -> Result<(), u16> {
    let r = m.cpu.regs;
    let structure = pointer(m, seg::ES, 7);
    let state = save_pm(m);
    let words = r.cx();
    // The parameters: above the INT 31h frame on the client's stack.
    let wide = bits32(m);
    let frame_bytes = if wide { 12 } else { 6 };
    let parameters: Vec<u16> = (0..u32::from(words)).map(|i| m.mem.read_u16(stack(m) + frame_bytes + 2 * i)).collect();
    m.dos.dpmi.saved.push(Saved::Call(state, structure));
    let mut regs = m.cpu.regs;
    read_structure(m, structure, &mut regs);
    let (own_ss, own_sp) = rm_stack(m);
    let (given_ss, given_sp) = (m.mem.read_u16(structure + 0x30), m.mem.read_u16(structure + 0x2E));
    let (ss, sp) = if given_ss == 0 && given_sp == 0 { (own_ss, own_sp) } else { (given_ss, given_sp) };
    let (cs, ip) = (m.mem.read_u16(structure + 0x2C), m.mem.read_u16(structure + 0x2A));
    to_real(m);
    m.cpu.regs.gpr = regs.gpr;
    m.cpu.regs.flags = regs.flags;
    for (i, s) in [seg::ES, seg::DS, seg::FS, seg::GS].into_iter().enumerate() {
        let value = m.mem.read_u16(structure + 0x22 + 2 * i as u32);
        set_real(m, s, value);
    }
    set_real(m, seg::SS, ss);
    m.cpu.regs.gpr[4] = u32::from(sp);
    for &word in parameters.iter().rev() {
        push_rm(m, word);
    }
    let flags = regs.flags as u16;
    match r.ax() {
        0x0300 => {
            push_rm(m, flags);
            push_rm(m, ROM_SEGMENT);
            push_rm(m, RM_RETURN);
            m.cpu.regs.flags &= !(IF | 0x0100);
            let at = u32::from(r.bl()) * 4;
            set_real(m, seg::CS, m.mem.read_u16(at + 2));
            m.cpu.regs.ip = u32::from(m.mem.read_u16(at));
        }
        function => {
            if function == 0x0302 {
                push_rm(m, flags);
            }
            push_rm(m, ROM_SEGMENT);
            push_rm(m, RM_RETURN);
            set_real(m, seg::CS, cs);
            m.cpu.regs.ip = u32::from(ip);
        }
    }
    Ok(())
}

// ---- raw switches and callbacks ----

/// The raw switch from real mode: AX DS, CX ES, DX SS, (E)BX (E)SP, SI CS, (E)DI (E)IP.
fn raw_to_pm(m: &mut Machine) {
    let r = m.cpu.regs;
    tables_pm(m);
    let ok = load(m, seg::DS, r.ax()) & load(m, seg::ES, r.cx()) & load(m, seg::SS, r.dx()) & load(m, seg::CS, r.si());
    if !ok {
        m.diagnostic("DPMI: a raw switch to protected mode with invalid selectors".into());
    }
    load(m, seg::FS, 0);
    load(m, seg::GS, 0);
    m.cpu.regs.gpr[4] = if bits32(m) { r.gpr[3] } else { u32::from(r.bx()) };
    m.cpu.regs.ip = if bits32(m) { r.gpr[7] } else { u32::from(r.di()) };
}

/// The raw switch from protected mode: the same registers, real-mode segments.
fn raw_to_rm(m: &mut Machine) {
    let r = m.cpu.regs;
    to_real(m);
    set_real(m, seg::DS, r.ax());
    set_real(m, seg::ES, r.cx());
    set_real(m, seg::SS, r.dx());
    set_real(m, seg::CS, r.si());
    m.cpu.regs.gpr[4] = u32::from(r.bx());
    m.cpu.regs.ip = u32::from(r.di());
}

/// Real-mode callback `slot` was called: its procedure runs in protected mode with DS:(E)SI at
/// the real-mode stack, ES:(E)DI at the register structure (filled in), interrupts off.
fn rm_callback(m: &mut Machine, slot: usize) {
    let Some(((proc_sel, proc_off), (struct_sel, struct_off))) =
        m.dos.dpmi.client.as_ref().and_then(|c| c.callbacks.get(slot).copied().flatten())
    else {
        m.diagnostic(format!("DPMI: real-mode callback {slot} isn't allocated"));
        return;
    };
    let rm = m.cpu.regs;
    tables_pm(m);
    let structure = read_descriptor(m, struct_sel).map_or(0, base_of).wrapping_add(struct_off);
    write_structure(m, structure, &rm);
    m.mem.write_u16(structure + 0x2A, rm.ip as u16);
    m.mem.write_u16(structure + 0x2C, rm.cs());
    m.mem.write_u16(structure + 0x2E, rm.sp());
    m.mem.write_u16(structure + 0x30, rm.ss());
    let stack_selector = {
        let existing = m.dos.dpmi.client.as_ref().and_then(|c| c.segments.get(&rm.ss()).copied());
        existing.or_else(|| {
            let sel = data_selector(m, rm.ss(), 0xFFFF)?;
            m.dos.dpmi.client.as_mut().expect("client").segments.insert(rm.ss(), sel);
            Some(sel)
        })
    };
    m.dos.dpmi.saved.push(Saved::Callback);
    load(m, seg::DS, stack_selector.unwrap_or(0));
    m.cpu.regs.gpr[6] = u32::from(rm.sp());
    load(m, seg::ES, struct_sel);
    m.cpu.regs.gpr[7] = struct_off;
    load(m, seg::FS, 0);
    load(m, seg::GS, 0);
    // The host's stack, and an IRET frame back into the host.
    load(m, seg::SS, HOST_STACK_SELECTOR);
    let wide = bits32(m);
    let word = if wide { 4 } else { 2 };
    let base = m.cpu.segment_base(seg::SS);
    let mut sp = 0x1000u32;
    for value in [0x0002u32, u32::from(HOST_CODE), PM_CALLBACK_RETURN - STUBS] {
        sp -= word;
        if wide { m.mem.write_u32(base + sp, value) } else { m.mem.write_u16(base + sp, value as u16) }
    }
    m.cpu.regs.gpr[4] = sp;
    m.cpu.regs.flags &= !IF;
    load(m, seg::CS, proc_sel);
    m.cpu.regs.ip = proc_off;
}

/// The callback procedure returned (IRET into the host): real mode with the registers of the
/// structure at ES:(E)DI.
fn callback_return(m: &mut Machine) {
    let structure = pointer(m, seg::ES, 7);
    if let Some(Saved::Callback) = m.dos.dpmi.saved.last() {
        m.dos.dpmi.saved.pop();
    }
    let mut regs = m.cpu.regs;
    read_structure(m, structure, &mut regs);
    to_real(m);
    m.cpu.regs.gpr = regs.gpr;
    m.cpu.regs.flags = regs.flags;
    for (i, s) in [seg::ES, seg::DS, seg::FS, seg::GS].into_iter().enumerate() {
        let value = m.mem.read_u16(structure + 0x22 + 2 * i as u32);
        set_real(m, s, value);
    }
    set_real(m, seg::CS, m.mem.read_u16(structure + 0x2C));
    m.cpu.regs.ip = u32::from(m.mem.read_u16(structure + 0x2A));
    set_real(m, seg::SS, m.mem.read_u16(structure + 0x30));
    m.cpu.regs.gpr[4] = u32::from(m.mem.read_u16(structure + 0x2E));
}
