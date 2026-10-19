//! Shared helpers for the CPU tests. Code is hand-encoded; each byte group is one instruction.

#![allow(dead_code)]

use cpu::{Bus, Cpu, Exit, Model};

pub const CODE_SEG: u16 = 0x1000;
pub const STACK_SEG: u16 = 0x2000;
pub const DATA_SEG: u16 = 0x3000;
pub const EXTRA_SEG: u16 = 0x4000;

pub struct TestBus {
    pub mem: Vec<u8>,
    pub port_writes: Vec<(u16, u8)>,
    pub port_reads: Vec<u16>,
}

impl Bus for TestBus {
    fn read_u8(&mut self, addr: u32) -> u8 {
        self.mem[addr as usize]
    }
    fn write_u8(&mut self, addr: u32, value: u8) {
        self.mem[addr as usize] = value;
    }
    fn port_in_u8(&mut self, port: u16) -> u8 {
        self.port_reads.push(port);
        (port as u8) ^ 0x5A
    }
    fn port_out_u8(&mut self, port: u16, value: u8) {
        self.port_writes.push((port, value));
    }
}

impl TestBus {
    pub fn new() -> Self {
        Self { mem: vec![0; 0x11_0000], port_writes: vec![], port_reads: vec![] }
    }
    pub fn lin(seg: u16, off: u16) -> usize {
        (usize::from(seg) << 4) + usize::from(off)
    }
    pub fn put(&mut self, seg: u16, off: u16, bytes: &[u8]) {
        let at = Self::lin(seg, off);
        self.mem[at..at + bytes.len()].copy_from_slice(bytes);
    }
    pub fn word(&self, seg: u16, off: u16) -> u16 {
        let at = Self::lin(seg, off);
        u16::from_le_bytes([self.mem[at], self.mem[at + 1]])
    }
    pub fn set_vector(&mut self, vector: u8, seg: u16, off: u16) {
        let at = usize::from(vector) * 4;
        self.mem[at..at + 2].copy_from_slice(&off.to_le_bytes());
        self.mem[at + 2..at + 4].copy_from_slice(&seg.to_le_bytes());
    }
}

/// An 80386 with code at 1000:0000, the stack at 2000:1000, DS=3000 and ES=4000.
pub fn setup(code: &[u8]) -> (Cpu, TestBus) {
    setup_model(code, Model::I386)
}

pub fn setup_model(code: &[u8], model: Model) -> (Cpu, TestBus) {
    let mut bus = TestBus::new();
    bus.put(CODE_SEG, 0, code);
    let mut cpu = Cpu::with_model(model);
    cpu.regs.seg = [EXTRA_SEG, CODE_SEG, STACK_SEG, DATA_SEG, 0, 0];
    cpu.regs.set_sp(0x1000);
    (cpu, bus)
}

/// Steps until HLT; panics on any other exit or after too many steps.
pub fn run_to_hlt(cpu: &mut Cpu, bus: &mut TestBus) {
    for _ in 0..1_000_000 {
        match cpu.step(bus) {
            Ok(()) => {}
            Err(Exit::Halt) => return,
            Err(other) => panic!("unexpected exit: {other}"),
        }
    }
    panic!("no HLT after 1000000 steps");
}

pub fn run(code: &[u8]) -> (Cpu, TestBus) {
    run_model(code, Model::I386)
}

pub fn run_model(code: &[u8], model: Model) -> (Cpu, TestBus) {
    let (mut cpu, mut bus) = setup_model(code, model);
    run_to_hlt(&mut cpu, &mut bus);
    (cpu, bus)
}

pub fn flags_set(cpu: &Cpu, mask: u16) -> bool {
    cpu.regs.flags & u32::from(mask) == u32::from(mask)
}

pub fn flags_clear(cpu: &Cpu, mask: u16) -> bool {
    cpu.regs.flags & u32::from(mask) == 0
}
