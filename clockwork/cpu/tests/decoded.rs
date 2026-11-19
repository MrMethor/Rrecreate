//! The cache of decoded instructions (D211): an instruction run decoded does what the interpreter does, reads and
//! writes in the same order included, and code that is written is decoded again.

use cpu::{Bus, Cpu, Exit, Model, SegmentCache, seg};

/// Memory for these tests (addresses wrap at its size), with code stamps per 256-byte block as Clockwork's machine
/// keeps them, with its count of code writes, or none (`on` false: every instruction runs fetched by the interpreter).
/// Every data access is logged in order; fetches aren't.
#[derive(Clone)]
struct StampBus {
    mem: Vec<u8>,
    stamps: Vec<u32>,
    writes: u64,
    on: bool,
    log: Vec<(bool, u32, u8)>,
}

const SIZE: usize = 1 << 16;

impl StampBus {
    fn new(on: bool) -> Self {
        Self { mem: vec![0; SIZE], stamps: vec![0; SIZE >> 8], writes: 0, on, log: Vec::new() }
    }

    fn at(addr: u32) -> usize {
        addr as usize % SIZE
    }
}

impl Bus for StampBus {
    fn read_u8(&mut self, addr: u32) -> u8 {
        let value = self.mem[Self::at(addr)];
        self.log.push((false, addr, value));
        value
    }

    fn write_u8(&mut self, addr: u32, value: u8) {
        self.log.push((true, addr, value));
        let at = Self::at(addr);
        if self.stamps[at >> 8] & 1 != 0 {
            self.stamps[at >> 8] += 1;
            self.writes += 1;
        }
        self.mem[at] = value;
    }

    fn fetch_u8(&mut self, addr: u32) -> u8 {
        self.mem[Self::at(addr)]
    }

    fn code_window(&mut self, addr: u32) -> Option<[u8; 16]> {
        self.on.then(|| std::array::from_fn(|i| self.mem[Self::at(addr.wrapping_add(i as u32))]))
    }

    fn code_stamp(&mut self, addr: u32, len: u32) -> Option<u64> {
        let (first, last) = (Self::at(addr), Self::at(addr.wrapping_add(len - 1)));
        if !self.on || first >> 8 != last >> 8 || last < first {
            return None;
        }
        let stamp = &mut self.stamps[first >> 8];
        *stamp |= 1;
        Some(((first >> 8) as u64) << 32 | u64::from(*stamp))
    }

    fn code_writes(&mut self) -> Option<u64> {
        self.on.then_some(self.writes)
    }

    fn port_in_u8(&mut self, port: u16) -> u8 {
        port as u8 ^ 0x5A
    }

    fn port_out_u8(&mut self, _port: u16, _value: u8) {}
}

/// xorshift64*: the same cases every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Opcodes the cache has handlers for (and some it gives to the interpreter, with the same first byte).
fn hot_opcodes() -> Vec<Vec<u8>> {
    let mut ops: Vec<Vec<u8>> = (0x00..0x40u8).filter(|op| op & 7 < 6).map(|op| vec![op]).collect();
    ops.extend((0x40..=0x5F).chain([0x68, 0x6A]).chain(0x70..=0x99).map(|op| vec![op]));
    ops.extend((0xA0..=0xA3).chain([0xA8, 0xA9]).chain(0xB0..=0xC3).chain([0xC6, 0xC7]).map(|op| vec![op]));
    ops.extend((0xD0..=0xD3).chain([0xD7]).chain(0xE0..=0xE3).chain([0xE8, 0xE9, 0xEB]).map(|op| vec![op]));
    ops.extend([0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFC, 0xFD, 0xFE, 0xFF].map(|op| vec![op]));
    ops.extend((0x80..=0x8F).map(|op| vec![0x0F, op]));
    ops
}

const CODE: u16 = 0x0010;

/// One case: an instruction and a machine state, run twice (decoded, then from the cache) by a CPU with a cache, and
/// twice by the interpreter alone.
fn run_case(rng: &mut Rng, opcodes: &[Vec<u8>]) -> Result<(), String> {
    let mut bytes = Vec::new();
    for prefix in [0x66u8, 0x67] {
        if rng.below(4) == 0 {
            bytes.push(prefix);
        }
    }
    if rng.below(3) == 0 {
        bytes.push([0x26, 0x2E, 0x36, 0x3E, 0x64, 0x65][rng.below(6) as usize]);
    }
    if rng.below(20) == 0 {
        bytes.push([0xF2, 0xF3][rng.below(2) as usize]);
    }
    bytes.extend(&opcodes[rng.below(opcodes.len() as u64) as usize]);
    while bytes.len() < 16 {
        bytes.push(rng.next() as u8);
    }
    let mut decoded_bus = StampBus::new(true);
    for byte in decoded_bus.mem.iter_mut() {
        *byte = rng.next() as u8;
    }
    let start = usize::from(CODE) << 4;
    decoded_bus.mem[start..start + 16].copy_from_slice(&bytes);
    let mut fetched_bus = StampBus { on: false, ..decoded_bus.clone() };

    let mut cpu = Cpu::with_model(Model::I386);
    cpu.fpu = None;
    for r in cpu.regs.gpr.iter_mut() {
        // Small values half the time, so that addresses land near the segments' starts.
        *r = if rng.below(2) == 0 { rng.next() as u32 & 0xFF } else { rng.next() as u32 };
    }
    cpu.regs.flags = (rng.next() as u32 & 0x0CD5) | 2;
    cpu.regs.seg = [rng.next() as u16, CODE, rng.next() as u16, rng.next() as u16, rng.next() as u16, 0];
    for (i, cache) in cpu.segs.iter_mut().enumerate() {
        let selector = cpu.regs.seg[i];
        *cache = SegmentCache { selector, base: u32::from(selector) << 4, limit: 0xFFFF, big: false };
    }
    // A 32-bit code segment in a third of the cases.
    cpu.segs[seg::CS].big = rng.below(3) == 0;
    cpu.regs.ip = 0;
    let (initial, mut fetched_cpu) = (cpu.clone(), cpu.clone());

    for pass in 0..2 {
        let (decoded, fetched) = (cpu.step(&mut decoded_bus), fetched_cpu.step(&mut fetched_bus));
        if decoded != fetched {
            return Err(format!("{bytes:02X?}, pass {pass}: {decoded:?} against {fetched:?}"));
        }
        if decoded_bus.log != fetched_bus.log {
            return Err(format!(
                "{bytes:02X?}, pass {pass}: accesses differ\n {:?}\n {:?}",
                decoded_bus.log, fetched_bus.log
            ));
        }
        if cpu != fetched_cpu || decoded_bus.mem != fetched_bus.mem {
            return Err(format!("{bytes:02X?}, pass {pass}: state differs\n {:?}\n {:?}", cpu.regs, fetched_cpu.regs));
        }
        // Again from the start: the second time from the cache.
        for c in [&mut cpu, &mut fetched_cpu] {
            c.regs = initial.regs;
            c.segs = initial.segs;
        }
    }
    Ok(())
}

#[test]
fn decoded_instructions_do_what_the_interpreter_does() {
    let opcodes = hot_opcodes();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let failures: Vec<String> = (0..3000).filter_map(|_| run_case(&mut rng, &opcodes).err()).take(5).collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The state after each step, run by the interpreter and through the cache, compared.
fn last_state(code: &[u8], cx: u16) -> (Cpu, Vec<u8>) {
    let mut results = Vec::new();
    for on in [true, false] {
        let mut bus = StampBus::new(on);
        let at = usize::from(CODE) << 4;
        bus.mem[at..at + code.len()].copy_from_slice(code);
        let mut cpu = Cpu::with_model(Model::I386);
        cpu.regs.seg = [0, CODE, 0x0F00, 0, 0, 0];
        cpu.regs.set_sp(0x100);
        cpu.regs.set_cx(cx);
        let mut steps = 0;
        loop {
            match cpu.step(&mut bus) {
                Ok(()) => {}
                Err(Exit::Halt) => break,
                Err(other) => panic!("{other}"),
            }
            steps += 1;
            assert!(steps < 100_000, "no HLT");
        }
        results.push((cpu, bus.mem));
    }
    let fetched = results.pop().unwrap();
    let decoded = results.pop().unwrap();
    assert_eq!(decoded.0, fetched.0, "the decoded run ends as the fetched one");
    decoded
}

#[test]
fn code_the_program_writes_is_decoded_again() {
    // 0000 MOV AL, 11h ; 0002 MOV BYTE CS:[0001], 22h ; 0008 LOOP 0000 ; 000A HLT. The second pass reads 22h.
    let code = [0xB0, 0x11, 0x2E, 0xC6, 0x06, 0x01, 0x00, 0x22, 0xE2, 0xF6, 0xF4];
    let (cpu, _) = last_state(&code, 2);
    assert_eq!(cpu.regs.al(), 0x22);
}

#[test]
fn code_the_host_writes_is_decoded_again() {
    let mut bus = StampBus::new(true);
    let at = usize::from(CODE) << 4;
    bus.mem[at..at + 3].copy_from_slice(&[0xB8, 0x34, 0x12]);
    let mut cpu = Cpu::with_model(Model::I386);
    cpu.regs.seg[seg::CS] = CODE;
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.regs.ax(), 0x1234);
    bus.write_u8((at + 2) as u32, 0x56);
    cpu.regs.ip = 0;
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.regs.ax(), 0x5634, "the host's write is seen");
}

#[test]
fn a_loop_runs_the_same_decoded() {
    // MOV SI, 0 ; 0003 ADD AX, [SI] ; INC SI ; INC SI ; SHL DX, 3 ; XOR DX, AX ; LOOP 0003 ; HLT
    let code = [0xBE, 0x00, 0x00, 0x03, 0x04, 0x46, 0x46, 0xC1, 0xE2, 0x03, 0x31, 0xC2, 0xE2, 0xF5, 0xF4];
    let (cpu, _) = last_state(&code, 300);
    assert_eq!(cpu.regs.cx(), 0);
}
