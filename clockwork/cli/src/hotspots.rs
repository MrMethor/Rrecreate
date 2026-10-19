//! `run --hotspots N`: where the program spends its time. CS:EIP is sampled at a fixed cycle
//! interval while running; the most frequent addresses are disassembled. Running in chunks
//! doesn't change execution, so the run itself stays identical to an unsampled one.
//!
//! Each sample also records where the code was (the code segment's base, as a linear address)
//! and its size, so protected-mode code is found through its descriptor and the page tables.

use std::collections::HashMap;

use dos::{Machine, Stop};
use iced_x86::{Decoder, DecoderOptions, Formatter, Instruction, IntelFormatter};

/// Cycles between samples; prime, so samples don't line up with loop lengths.
const INTERVAL: u64 = 997;

/// Where sampled code is: its linear address and whether it's 32-bit code.
#[derive(Clone, Copy)]
pub struct Code {
    pub linear: u32,
    pub bits: u32,
}

pub struct Profile {
    samples: HashMap<(u16, u32), (u64, Code)>,
    total: u64,
}

/// Runs until `limit` cycles or a stop, sampling CS:EIP.
pub fn run_sampled(m: &mut Machine, limit: u64) -> (Stop, Profile) {
    let mut profile = Profile { samples: HashMap::new(), total: 0 };
    let end = m.cpu.cycles.saturating_add(limit);
    loop {
        let next = m.cpu.cycles.saturating_add(INTERVAL).min(end);
        let stop = m.run_until(next);
        let code = current_code(m);
        profile.samples.entry((m.cpu.regs.cs(), m.cpu.regs.ip)).or_insert((0, code)).0 += 1;
        profile.total += 1;
        if stop != Stop::CycleLimit || m.cpu.cycles >= end {
            return (stop, profile);
        }
    }
}

/// The code at the current CS:EIP.
pub fn current_code(m: &Machine) -> Code {
    Code {
        linear: m.cpu.segment_base(1 /* CS */).wrapping_add(m.cpu.regs.ip),
        bits: if m.cpu.code32() { 32 } else { 16 },
    }
}

impl Profile {
    pub fn print(&self, m: &Machine, count: usize) {
        let mut hot: Vec<_> = self.samples.iter().collect();
        hot.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
        println!("hotspots  {} samples, every {INTERVAL} cycles", self.total);
        for (&(cs, ip), &(samples, code)) in hot.into_iter().take(count) {
            let percent = samples as f64 * 100.0 / self.total.max(1) as f64;
            println!("  {percent:5.1}%  {cs:04X}:{ip:04X}  {}", disassemble(m, code, ip));
        }
    }
}

/// The bytes at `code`, with pages that aren't present marked.
fn code_bytes(m: &Machine, code: Code, len: usize) -> Option<Vec<u8>> {
    let bytes = m.peek_linear(code.linear, len);
    let mapped = bytes.iter().take_while(|b| b.is_some()).count();
    (mapped > 0).then(|| bytes[..mapped].iter().flatten().copied().collect())
}

/// Shows the registers and disassembles `count` instructions at `cs:ip`, which is at `code`.
pub fn print_listing(m: &Machine, cs: u16, ip: u32, code: Code, count: usize) {
    let regs = &m.cpu.regs;
    println!(
        "registers EAX={:08X} EBX={:08X} ECX={:08X} EDX={:08X} ESI={:08X} EDI={:08X} EBP={:08X} ESP={:08X}",
        regs.eax(),
        regs.ebx(),
        regs.ecx(),
        regs.edx(),
        regs.esi(),
        regs.edi(),
        regs.ebp(),
        regs.esp()
    );
    println!(
        "          CS={:04X} EIP={:08X} DS={:04X} ES={:04X} SS={:04X} FS={:04X} GS={:04X} EFLAGS={:08X} CPL={}{}",
        regs.cs(),
        regs.ip,
        regs.ds(),
        regs.es(),
        regs.ss(),
        regs.fs(),
        regs.gs(),
        regs.flags,
        m.cpu.cpl,
        if m.cpu.cr0 & cpu::cr0::PG != 0 { " paging" } else { "" }
    );
    println!("listing   {cs:04X}:{ip:04X} (linear {:08X})", code.linear);
    let Some(bytes) = code_bytes(m, code, count * 15) else {
        println!("  (page not present)");
        return;
    };
    let mut decoder = Decoder::with_ip(code.bits, &bytes, u64::from(ip), DecoderOptions::NONE);
    let mut formatter = IntelFormatter::new();
    let mut instruction = Instruction::default();
    for _ in 0..count {
        if !decoder.can_decode() {
            break;
        }
        let offset = decoder.position();
        decoder.decode_out(&mut instruction);
        let mut text = String::new();
        formatter.format(&instruction, &mut text);
        let encoded: Vec<String> =
            bytes[offset..offset + instruction.len()].iter().map(|b| format!("{b:02X}")).collect();
        println!("  {cs:04X}:{:04X}  {:<20} {text}", instruction.ip(), encoded.join(" "));
    }
}

fn disassemble(m: &Machine, code: Code, ip: u32) -> String {
    let Some(bytes) = code_bytes(m, code, 16) else {
        return "(page not present)".into();
    };
    let mut decoder = Decoder::with_ip(code.bits, &bytes, u64::from(ip), DecoderOptions::NONE);
    let mut instruction = Instruction::default();
    decoder.decode_out(&mut instruction);
    let mut text = String::new();
    IntelFormatter::new().format(&instruction, &mut text);
    let shown = instruction.len().clamp(1, bytes.len());
    let encoded: Vec<String> = bytes[..shown].iter().map(|b| format!("{b:02X}")).collect();
    format!("{:<20} {text}", encoded.join(" "))
}
