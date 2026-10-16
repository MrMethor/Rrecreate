//! `run --hotspots N`: where the program spends its time. CS:IP is sampled at a fixed cycle
//! interval while running; the most frequent addresses are disassembled. Running in chunks
//! doesn't change execution, so the run itself stays identical to an unsampled one.

use std::collections::HashMap;

use dos::{Machine, Stop};
use iced_x86::{Decoder, DecoderOptions, Formatter, Instruction, IntelFormatter};

/// Cycles between samples; prime, so samples don't line up with loop lengths.
const INTERVAL: u64 = 997;

pub struct Profile {
    samples: HashMap<(u16, u16), u64>,
    total: u64,
}

/// Runs until `limit` cycles or a stop, sampling CS:IP.
pub fn run_sampled(m: &mut Machine, limit: u64) -> (Stop, Profile) {
    let mut profile = Profile { samples: HashMap::new(), total: 0 };
    let end = m.cpu.cycles.saturating_add(limit);
    loop {
        let next = m.cpu.cycles.saturating_add(INTERVAL).min(end);
        let stop = m.run_until(next);
        *profile.samples.entry((m.cpu.regs.cs(), m.cpu.regs.ip)).or_default() += 1;
        profile.total += 1;
        if stop != Stop::CycleLimit || m.cpu.cycles >= end {
            return (stop, profile);
        }
    }
}

impl Profile {
    pub fn print(&self, m: &Machine, count: usize) {
        let mut hot: Vec<_> = self.samples.iter().collect();
        hot.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        println!("hotspots  {} samples, every {INTERVAL} cycles", self.total);
        for (&(cs, ip), &samples) in hot.into_iter().take(count) {
            let percent = samples as f64 * 100.0 / self.total.max(1) as f64;
            println!("  {percent:5.1}%  {cs:04X}:{ip:04X}  {}", disassemble(m, cs, ip));
        }
    }
}

/// Disassembles `count` instructions starting at `cs:ip` in the machine's current memory.
pub fn print_listing(m: &Machine, cs: u16, ip: u16, count: usize) {
    let linear = (u32::from(cs) << 4) + u32::from(ip);
    let bytes = m.mem.read_bytes(linear, count * 15);
    let mut decoder = Decoder::with_ip(16, &bytes, u64::from(ip), DecoderOptions::NONE);
    let mut formatter = IntelFormatter::new();
    let mut instruction = Instruction::default();
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
        "          CS={:04X} IP={:04X} DS={:04X} ES={:04X} SS={:04X} FS={:04X} GS={:04X} FLAGS={:04X}",
        regs.cs(),
        regs.ip,
        regs.ds(),
        regs.es(),
        regs.ss(),
        regs.fs(),
        regs.gs(),
        regs.flags
    );
    println!("listing   {cs:04X}:{ip:04X}");
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
        println!("  {cs:04X}:{:04X}  {:<20} {text}", instruction.ip() as u16, encoded.join(" "));
    }
}

fn disassemble(m: &Machine, cs: u16, ip: u16) -> String {
    let linear = (u32::from(cs) << 4) + u32::from(ip);
    let bytes = m.mem.read_bytes(linear, 16);
    let mut decoder = Decoder::with_ip(16, &bytes, u64::from(ip), DecoderOptions::NONE);
    let mut instruction = Instruction::default();
    decoder.decode_out(&mut instruction);
    let mut text = String::new();
    IntelFormatter::new().format(&instruction, &mut text);
    let encoded: Vec<String> = bytes[..instruction.len().max(1)].iter().map(|b| format!("{b:02X}")).collect();
    format!("{:<20} {text}", encoded.join(" "))
}
