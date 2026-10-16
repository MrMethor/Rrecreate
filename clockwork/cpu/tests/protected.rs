//! Protected mode: a small "kernel" is built in memory (GDT, IDT, TSS, page tables), entered from
//! real mode, and exercised. Code is assembled with iced-x86.

use cpu::{Bus, Cpu, Exit};
use iced_x86::code_asm::*;

struct Memory {
    bytes: Vec<u8>,
}

impl Bus for Memory {
    fn read_u8(&mut self, addr: u32) -> u8 {
        self.bytes.get(addr as usize).copied().unwrap_or(0xFF)
    }
    fn write_u8(&mut self, addr: u32, value: u8) {
        if let Some(byte) = self.bytes.get_mut(addr as usize) {
            *byte = value;
        }
    }
    fn port_in_u8(&mut self, _port: u16) -> u8 {
        0xFF
    }
    fn port_out_u8(&mut self, _port: u16, _value: u8) {}
}

impl Memory {
    fn put(&mut self, at: u32, data: &[u8]) {
        self.bytes[at as usize..at as usize + data.len()].copy_from_slice(data);
    }
    fn put64(&mut self, at: u32, value: u64) {
        self.put(at, &value.to_le_bytes());
    }
    fn dword(&self, at: u32) -> u32 {
        u32::from_le_bytes(self.bytes[at as usize..at as usize + 4].try_into().unwrap())
    }
}

const GDT: u32 = 0x1000;
const IDT: u32 = 0x2000;
const TSS: u32 = 0x3000;
const BOOT: u32 = 0x1_0000;
const KERNEL: u32 = 0x2_0000;
const USER: u32 = 0x2_0800;
const HANDLERS: u32 = 0x3_0000;
const DATA: u32 = 0x4_0000;

const CODE0: u16 = 0x08;
const DATA0: u16 = 0x10;
const CODE16: u16 = 0x18;
const DATA16: u16 = 0x20;
const TSS_SELECTOR: u16 = 0x28;
const CODE3: u16 = 0x33;
const DATA3: u16 = 0x3B;

fn segment(base: u32, limit: u32, access: u64, flags: u64) -> u64 {
    let (base, limit) = (u64::from(base), u64::from(limit));
    (limit & 0xFFFF)
        | ((base & 0xFFFF) << 16)
        | (((base >> 16) & 0xFF) << 32)
        | (access << 40)
        | (((limit >> 16) & 0xF) << 48)
        | (flags << 52)
        | ((base >> 24) << 56)
}

fn gate(selector: u16, offset: u32, attributes: u64) -> u64 {
    let offset = u64::from(offset);
    (offset & 0xFFFF) | (u64::from(selector) << 16) | (attributes << 40) | ((offset >> 16) << 48)
}

fn assemble(bitness: u32, base: u32, build: impl FnOnce(&mut CodeAssembler) -> Result<(), IcedError>) -> Vec<u8> {
    let mut a = CodeAssembler::new(bitness).unwrap();
    build(&mut a).unwrap();
    a.assemble(u64::from(base)).unwrap()
}

/// Memory with descriptor tables, a TSS, and boot code at 1000:0000 that loads them, enters
/// protected mode and jumps to `kernel` (32-bit, at 2_0000h) with flat ring-0 segments.
fn world(kernel: impl FnOnce(&mut CodeAssembler) -> Result<(), IcedError>) -> (Cpu, Memory) {
    let mut m = Memory { bytes: vec![0; 0x20_0000] };
    let descriptors = [
        0,
        segment(0, 0xF_FFFF, 0x9A, 0xC),
        segment(0, 0xF_FFFF, 0x92, 0xC),
        segment(BOOT, 0xFFFF, 0x9A, 0),
        segment(0, 0xFFFF, 0x92, 0),
        segment(TSS, 0x67, 0x89, 0),
        segment(0, 0xF_FFFF, 0xFA, 0xC),
        segment(0, 0xF_FFFF, 0xF2, 0xC),
    ];
    for (i, &d) in descriptors.iter().enumerate() {
        m.put64(GDT + 8 * i as u32, d);
    }
    // TSS: ring-0 stack at 7_0000h.
    m.put(TSS + 4, &0x7_0000u32.to_le_bytes());
    m.put(TSS + 8, &u32::from(DATA0).to_le_bytes());
    m.put(TSS + 0x66, &0x68u16.to_le_bytes());
    // Pseudo-descriptors for LGDT and LIDT at 1000:00F0 and 1000:00F8.
    // Room for 16 descriptors; tests add their own after the standard eight.
    m.put(BOOT + 0xF0, &(16u16 * 8 - 1).to_le_bytes());
    m.put(BOOT + 0xF2, &GDT.to_le_bytes());
    m.put(BOOT + 0xF8, &(256u16 * 8 - 1).to_le_bytes());
    m.put(BOOT + 0xFA, &IDT.to_le_bytes());
    let mut boot = vec![0xFA]; // cli
    boot.extend([0x2E, 0x0F, 0x01, 0x16, 0xF0, 0x00]); // lgdt cs:[00F0]
    boot.extend([0x2E, 0x0F, 0x01, 0x1E, 0xF8, 0x00]); // lidt cs:[00F8]
    boot.extend([0x0F, 0x20, 0xC0, 0x0C, 0x01, 0x0F, 0x22, 0xC0]); // mov eax,cr0; or al,1; mov cr0,eax
    boot.extend([0x66, 0xEA]); // jmp far CODE0:KERNEL
    boot.extend(KERNEL.to_le_bytes());
    boot.extend(CODE0.to_le_bytes());
    m.put(BOOT, &boot);
    let code = assemble(32, KERNEL, |a| {
        a.mov(ax, DATA0 as u32)?;
        a.mov(ds, ax)?;
        a.mov(es, ax)?;
        a.mov(ss, ax)?;
        a.mov(esp, 0x8_0000)?;
        kernel(a)
    });
    m.put(KERNEL, &code);
    let mut cpu = Cpu::new();
    cpu.regs.set_cs(0x1000);
    cpu.regs.ip = 0;
    (cpu, m)
}

fn set_gate(m: &mut Memory, vector: u8, offset: u32, attributes: u64) {
    m.put64(IDT + 8 * u32::from(vector), gate(CODE0, offset, attributes));
}

fn handler(m: &mut Memory, at: u32, build: impl FnOnce(&mut CodeAssembler) -> Result<(), IcedError>) {
    let code = assemble(32, at, build);
    m.put(at, &code);
}

fn run(cpu: &mut Cpu, m: &mut Memory) {
    for _ in 0..100_000 {
        match cpu.step(m) {
            Ok(()) => {}
            Err(Exit::Halt) => return,
            Err(other) => panic!("unexpected exit: {other}"),
        }
    }
    panic!("no HLT");
}

#[test]
fn enters_protected_mode_and_runs_32_bit_code() {
    let (mut cpu, mut m) = world(|a| {
        a.mov(dword_ptr(DATA), 0x1234_5678)?;
        a.mov(eax, cs)?;
        a.mov(dword_ptr(DATA + 4), eax)?;
        a.hlt()
    });
    run(&mut cpu, &mut m);
    assert!(cpu.protected_mode() && cpu.code32() && cpu.stack32());
    assert_eq!(m.dword(DATA), 0x1234_5678);
    assert_eq!(m.dword(DATA + 4), u32::from(CODE0));
}

#[test]
fn interrupt_gate_and_iretd() {
    let (mut cpu, mut m) = world(|a| {
        a.int(0x30)?;
        a.mov(dword_ptr(DATA + 4), 1)?;
        a.hlt()
    });
    set_gate(&mut m, 0x30, HANDLERS, 0x8E);
    handler(&mut m, HANDLERS, |a| {
        a.mov(dword_ptr(DATA), 0xC0DE)?;
        a.iretd()
    });
    run(&mut cpu, &mut m);
    assert_eq!((m.dword(DATA), m.dword(DATA + 4)), (0xC0DE, 1));
}

#[test]
fn general_protection_fault_pushes_the_selector() {
    let (mut cpu, mut m) = world(|a| {
        a.mov(ebx, 0x1234)?;
        a.mov(ds, ebx)?; // beyond the GDT: #GP(1234h)
        a.mov(dword_ptr(DATA + 4), 1)?;
        a.hlt()
    });
    set_gate(&mut m, 13, HANDLERS, 0x8E);
    handler(&mut m, HANDLERS, |a| {
        a.pop(eax)?;
        a.mov(dword_ptr(DATA), eax)?;
        a.add(dword_ptr(esp), 2)?; // skip the 2-byte MOV DS,BX
        a.iretd()
    });
    run(&mut cpu, &mut m);
    assert_eq!(m.dword(DATA), 0x1234);
    assert_eq!(m.dword(DATA + 4), 1, "DS kept its old value and the program went on");
}

#[test]
fn ring_3_enters_ring_0_through_an_interrupt_gate() {
    let (mut cpu, mut m) = world(|a| {
        a.mov(ax, TSS_SELECTOR as u32)?;
        a.ltr(ax)?;
        a.push(DATA3 as i32)?;
        a.push(0x6_0000)?;
        a.pushfd()?;
        a.push(CODE3 as i32)?;
        a.push(USER as i32)?;
        a.iretd()
    });
    let user = assemble(32, USER, |a| {
        a.int(0x40)?;
        a.cli()?; // IOPL 0 < CPL 3: #GP
        a.hlt()
    });
    m.put(USER, &user);
    m.put64(IDT + 8 * 0x40, gate(CODE0, HANDLERS, 0xEE));
    handler(&mut m, HANDLERS, |a| {
        a.mov(eax, ss)?;
        a.mov(dword_ptr(DATA), eax)?;
        a.mov(dword_ptr(DATA + 4), esp)?;
        a.mov(eax, dword_ptr(esp + 4))?;
        a.mov(dword_ptr(DATA + 8), eax)?;
        a.iretd()
    });
    set_gate(&mut m, 13, HANDLERS + 0x100, 0x8E);
    handler(&mut m, HANDLERS + 0x100, |a| {
        a.mov(dword_ptr(DATA + 12), 0xDEAD)?;
        a.hlt()
    });
    run(&mut cpu, &mut m);
    assert_eq!(m.dword(DATA), u32::from(DATA0), "the handler runs on the TSS's ring-0 stack");
    assert_eq!(m.dword(DATA + 4), 0x7_0000 - 20, "SS, ESP, EFLAGS, CS, EIP pushed");
    assert_eq!(m.dword(DATA + 8), u32::from(CODE3), "interrupted at CPL 3");
    assert_eq!(m.dword(DATA + 12), 0xDEAD, "CLI at CPL 3 faulted");
    assert_eq!(cpu.cpl, 0);
}

#[test]
fn page_fault_is_handled_and_the_access_retried() {
    let (mut cpu, mut m) = world(|a| {
        a.mov(eax, 0x5_0000)?;
        a.mov(cr3, eax)?;
        a.mov(eax, cr0)?;
        a.or(eax, 0x8000_0000u32 as i32)?;
        a.mov(cr0, eax)?;
        a.mov(dword_ptr(0x9_0010), 0xFEED_FACEu32 as i32)?;
        a.hlt()
    });
    // Identity-map the first 4 MB, except linear 9_0000h, which isn't present.
    m.put(0x5_0000, &(0x5_1000u32 | 7).to_le_bytes());
    for page in 0..1024u32 {
        let pte = if page == 0x90 { 0 } else { (page << 12) | 7 };
        m.put(0x5_1000 + 4 * page, &pte.to_le_bytes());
    }
    set_gate(&mut m, 14, HANDLERS, 0x8E);
    handler(&mut m, HANDLERS, |a| {
        a.pop(eax)?;
        a.mov(dword_ptr(DATA), eax)?;
        a.mov(eax, cr2)?;
        a.mov(dword_ptr(DATA + 4), eax)?;
        a.mov(dword_ptr(0x5_1000 + 4 * 0x90), 0x4_1007)?; // map it to physical 4_1000h
        a.iretd()
    });
    run(&mut cpu, &mut m);
    assert_eq!(m.dword(DATA), 2, "error code: write to a page that isn't present, supervisor");
    assert_eq!(m.dword(DATA + 4), 0x9_0010, "CR2");
    assert_eq!(m.dword(0x4_1010), 0xFEED_FACE, "the retried write went through the new mapping");
    assert_eq!(m.dword(0x5_1000 + 4 * 0x90) & 0x60, 0x60, "accessed and dirty bits set");
}

#[test]
fn virtual_8086_mode_runs_real_mode_code_and_traps_interrupts() {
    let (mut cpu, mut m) = world(|a| {
        a.mov(ax, TSS_SELECTOR as u32)?;
        a.ltr(ax)?;
        for value in [0, 0, 0x5000, 0, 0x6000, 0xFFF0] {
            a.push(value)?; // GS, FS, DS, ES, SS, SP
        }
        a.push(0x0002_3002)?; // EFLAGS: VM, IOPL 3
        a.push(0x7000)?; // CS
        a.push(0)?; // IP
        a.iretd()
    });
    let v86 = assemble(16, 0x7_0000, |a| {
        a.mov(word_ptr(0x10), 0xBEEF)?; // DS = 5000h: linear 5_0010h
        a.int(0x41)
    });
    m.put(0x7_0000, &v86);
    m.put64(IDT + 8 * 0x41, gate(CODE0, HANDLERS, 0xEE));
    handler(&mut m, HANDLERS, |a| {
        a.mov(eax, dword_ptr(esp + 8))?;
        a.mov(dword_ptr(DATA), eax)?;
        a.mov(eax, ds)?;
        a.mov(dword_ptr(DATA + 4), eax)?;
        a.mov(eax, dword_ptr(esp + 24))?; // the V86 DS pushed by the CPU
        a.mov(dword_ptr(DATA + 8), eax)?;
        a.hlt()
    });
    run(&mut cpu, &mut m);
    assert_eq!(m.dword(0x5_0010) & 0xFFFF, 0xBEEF);
    assert_ne!(m.dword(DATA) & 0x2_0000, 0, "VM set in the pushed EFLAGS");
    assert_eq!(m.dword(DATA + 4), 0, "data segments are null in the handler");
    assert_eq!(m.dword(DATA + 8), 0x5000);
    assert!(!cpu.v86());
}

#[test]
fn returns_to_real_mode() {
    let (mut cpu, mut m) = world(|a| {
        a.db(&[0xEA])?; // jmp far CODE16:0200 (into the 16-bit segment at 1_0000h)
        a.dd(&[0x200])?;
        a.dw(&[CODE16])
    });
    let back = assemble(16, 0x200, |a| {
        a.mov(ax, DATA16 as u32)?;
        a.mov(ds, ax)?;
        a.mov(eax, cr0)?;
        a.and(al, 0xFE)?;
        a.mov(cr0, eax)?;
        a.db(&[0xEA, 0x00, 0x03, 0x00, 0x10]) // jmp far 1000:0300
    });
    m.put(BOOT + 0x200, &back);
    let real = assemble(16, 0x300, |a| {
        a.mov(ax, 0x4000)?;
        a.mov(ds, ax)?;
        a.mov(word_ptr(0), 0x5555)?;
        a.hlt()
    });
    m.put(BOOT + 0x300, &real);
    run(&mut cpu, &mut m);
    assert!(!cpu.protected_mode());
    assert_eq!(m.dword(DATA) & 0xFFFF, 0x5555);
}

#[test]
fn call_gate_to_ring_0_copies_parameters_and_returns_outward() {
    let (mut cpu, mut m) = world(|a| {
        a.mov(ax, TSS_SELECTOR as u32)?;
        a.ltr(ax)?;
        a.push(DATA3 as i32)?;
        a.push(0x6_0000)?;
        a.pushfd()?;
        a.push(CODE3 as i32)?;
        a.push(USER as i32)?;
        a.iretd()
    });
    // Call gate (selector 48h, DPL 3) to ring 0, copying one doubleword.
    m.put64(GDT + 0x48, gate(CODE0, HANDLERS, 0xEC) | 1 << 32);
    let user = assemble(32, USER, |a| {
        a.mov(ax, DATA3 as u32)?;
        a.mov(ds, ax)?;
        a.push(0x77)?;
        a.db(&[0x9A, 0, 0, 0, 0, 0x4B, 0x00])?; // call far 004Bh:0
        a.mov(dword_ptr(DATA + 4), eax)?;
        a.mov(eax, cs)?;
        a.mov(dword_ptr(DATA + 8), eax)?;
        a.int(0x40)
    });
    m.put(USER, &user);
    handler(&mut m, HANDLERS, |a| {
        a.mov(eax, dword_ptr(esp + 4))?; // caller's CS
        a.mov(dword_ptr(DATA), eax)?;
        a.mov(eax, dword_ptr(esp + 8))?; // the copied parameter
        a.add(eax, 1)?;
        a.retf_1(4)
    });
    m.put64(IDT + 8 * 0x40, gate(CODE0, HANDLERS + 0x100, 0xEE));
    handler(&mut m, HANDLERS + 0x100, |a| a.hlt());
    run(&mut cpu, &mut m);
    assert_eq!(m.dword(DATA), u32::from(CODE3), "the gate handler ran with the ring-3 caller's frame");
    assert_eq!(m.dword(DATA + 4), 0x78, "parameter copied to the ring-0 stack");
    assert_eq!(m.dword(DATA + 8), u32::from(CODE3), "returned to ring 3");
}

#[test]
fn jump_to_a_tss_switches_tasks() {
    let (mut cpu, mut m) = world(|a| {
        a.mov(ax, TSS_SELECTOR as u32)?;
        a.ltr(ax)?;
        a.db(&[0xEA, 0, 0, 0, 0, 0x50, 0x00])?; // jmp far 0050h:0 (the second TSS)
        a.hlt()
    });
    // Task B: its own TSS at 3100h, starting at HANDLERS with flat ring-0 segments.
    let tss_b = 0x3100;
    m.put64(GDT + 0x50, segment(tss_b, 0x67, 0x89, 0));
    let field = |offset: u32, value: u32, m: &mut Memory| m.put(tss_b + offset, &value.to_le_bytes());
    field(0x20, HANDLERS, &mut m);
    field(0x24, 2, &mut m);
    field(0x38, 0x6_0000, &mut m); // ESP
    for (offset, selector) in [(0x48, DATA0), (0x4C, CODE0), (0x50, DATA0), (0x54, DATA0), (0x58, 0), (0x5C, 0)] {
        field(offset, u32::from(selector), &mut m);
    }
    handler(&mut m, HANDLERS, |a| {
        a.mov(dword_ptr(DATA), 0xB)?;
        a.str(ax)?;
        a.mov(dword_ptr(DATA + 4), eax)?;
        a.hlt()
    });
    run(&mut cpu, &mut m);
    assert_eq!(m.dword(DATA), 0xB);
    assert_eq!(m.dword(DATA + 4) & 0xFFFF, 0x50);
    assert_eq!(cpu.tr.selector, 0x50);
    assert!(m.dword(TSS + 0x20) > KERNEL, "the old task's EIP was saved in its TSS");
    assert_eq!((m.dword(GDT + 0x28 + 4) >> 8) & 0xF, 0x9, "the old TSS is no longer busy");
    assert_eq!((m.dword(GDT + 0x50 + 4) >> 8) & 0xF, 0xB, "the new TSS is busy");
}

#[test]
fn rep_movsd_resumes_after_a_page_fault() {
    let (mut cpu, mut m) = world(|a| {
        a.mov(eax, 0x5_0000)?;
        a.mov(cr3, eax)?;
        a.mov(eax, cr0)?;
        a.or(eax, 0x8000_0000u32 as i32)?;
        a.mov(cr0, eax)?;
        a.mov(esi, 0x4_2000)?;
        a.mov(edi, 0x8_FFF8)?;
        a.mov(ecx, 8)?;
        a.cld()?;
        a.rep().movsd()?;
        a.mov(dword_ptr(DATA + 4), ecx)?;
        a.hlt()
    });
    m.put(0x5_0000, &(0x5_1000u32 | 7).to_le_bytes());
    for page in 0..1024u32 {
        let pte = if page == 0x90 { 0 } else { (page << 12) | 7 };
        m.put(0x5_1000 + 4 * page, &pte.to_le_bytes());
    }
    for i in 0..8u32 {
        m.put(0x4_2000 + 4 * i, &(0x1111_1111 * (i + 1)).to_le_bytes());
    }
    set_gate(&mut m, 14, HANDLERS, 0x8E);
    handler(&mut m, HANDLERS, |a| {
        a.add(esp, 4)?;
        a.inc(dword_ptr(DATA))?;
        a.mov(dword_ptr(0x5_1000 + 4 * 0x90), 0x4_3007)?;
        a.iretd()
    });
    run(&mut cpu, &mut m);
    assert_eq!(m.dword(DATA), 1, "one fault");
    assert_eq!(m.dword(DATA + 4), 0, "ECX counted down to 0");
    assert_eq!((m.dword(0x8_FFF8), m.dword(0x8_FFFC)), (0x1111_1111, 0x2222_2222));
    let rest: Vec<u32> = (0..6).map(|i| m.dword(0x4_3000 + 4 * i)).collect();
    assert_eq!(rest, (3..=8).map(|i| 0x1111_1111 * i).collect::<Vec<u32>>());
}
