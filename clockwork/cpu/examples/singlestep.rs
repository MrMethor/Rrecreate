//! Runs the SingleStepTests hardware-recorded CPU test suites against the interpreter:
//! github.com/SingleStepTests/80386 (`v1_ex_real_mode`, an 80386EX) and
//! github.com/SingleStepTests/80286 (`v1_real_mode`), in their MOO format (gzipped).
//!
//! ```text
//! cargo run --release -p cpu --example singlestep -- <suite folder> [name filter] [--show N]
//! ```
//!
//! Each test loads registers and memory, executes one instruction (taking any exception it
//! raises), stands in for the HLT that ends every test, and compares registers and memory with
//! what the hardware did. Flags the suite marks undefined (in the file, the test, or the 80386
//! suite's `80386.csv`) are ignored, as are tests on the suite's revocation list. With
//! `--skip-limit-faults`, tests where the hardware raised #SS or #GP are counted apart: in real
//! mode those come from offsets past a segment's 64 KB limit, which the interpreter (like DOSBox)
//! doesn't check. Instructions that read I/O ports are skipped: their results
//! depend on the bus data the recording captured, which this runner doesn't replay.
//!
//! The suites are large and not part of the repository; see README.md for where to put them.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use cpu::{Bus, Cpu, Exit, Model, SegmentCache, flags, reg, seg};

// ---- MOO files ----

/// Register indices in MOO's 32-bit order (`RG32`); 16-bit files (`REGS`) map onto them.
mod r {
    pub const CR0: usize = 0;
    pub const CR3: usize = 1;
    pub const EAX: usize = 2;
    pub const EBX: usize = 3;
    pub const ECX: usize = 4;
    pub const EDX: usize = 5;
    pub const ESI: usize = 6;
    pub const EDI: usize = 7;
    pub const EBP: usize = 8;
    pub const ESP: usize = 9;
    pub const CS: usize = 10;
    pub const DS: usize = 11;
    pub const ES: usize = 12;
    pub const FS: usize = 13;
    pub const GS: usize = 14;
    pub const SS: usize = 15;
    pub const EIP: usize = 16;
    pub const EFLAGS: usize = 17;
    pub const DR6: usize = 18;
    pub const DR7: usize = 19;
    pub const COUNT: usize = 20;
    pub const NAMES: [&str; COUNT] = [
        "cr0", "cr3", "eax", "ebx", "ecx", "edx", "esi", "edi", "ebp", "esp", "cs", "ds", "es", "fs", "gs", "ss",
        "eip", "eflags", "dr6", "dr7",
    ];
}

/// `REGS` bit order: ax bx cx dx cs ss ds es sp bp si di ip flags.
const REGS16: [usize; 14] =
    [r::EAX, r::EBX, r::ECX, r::EDX, r::CS, r::SS, r::DS, r::ES, r::ESP, r::EBP, r::ESI, r::EDI, r::EIP, r::EFLAGS];

type Registers = [Option<u32>; r::COUNT];

#[derive(Default, Clone)]
struct State {
    regs: Registers,
    /// Undefined parts of registers: bits set in a mask are compared, clear bits are ignored.
    masks: Registers,
    ram: Vec<(u32, u8)>,
}

struct Test {
    name: String,
    bytes: Vec<u8>,
    initial: State,
    last: State,
    /// The exception the instruction raised, if any, and where it pushed FLAGS.
    exception: Option<u8>,
    flags_at: Option<u32>,
    hash: String,
}

struct MooFile {
    cpu: String,
    masks: Registers,
    tests: Vec<Test>,
}

fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().expect("4 bytes"))
}

/// Splits `data` into (id, payload) chunks.
fn chunks(mut data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    while data.len() >= 8 {
        let id: [u8; 4] = data[..4].try_into().expect("4 bytes");
        let len = u32_at(data, 4) as usize;
        let end = (8 + len).min(data.len());
        out.push((id, &data[8..end]));
        data = &data[end..];
    }
    out
}

/// A `REGS`/`RG32` (or `RMSK`/`RM32`) chunk into `into`.
fn read_registers(id: &[u8; 4], data: &[u8], into: &mut Registers) {
    match id {
        b"RG32" | b"RM32" => {
            let mask = u32_at(data, 0);
            let mut at = 4;
            for (i, slot) in into.iter_mut().enumerate() {
                if mask & (1 << i) != 0 {
                    *slot = Some(u32_at(data, at));
                    at += 4;
                }
            }
        }
        _ => {
            let mask = u16_at(data, 0);
            let mut at = 2;
            for (i, &index) in REGS16.iter().enumerate() {
                if mask & (1 << i) != 0 {
                    into[index] = Some(u32::from(u16_at(data, at)));
                    at += 2;
                }
            }
        }
    }
}

fn read_state(data: &[u8]) -> State {
    let mut state = State::default();
    for (id, payload) in chunks(data) {
        match &id {
            b"REGS" | b"RG32" => read_registers(&id, payload, &mut state.regs),
            b"RMSK" | b"RM32" => read_registers(&id, payload, &mut state.masks),
            b"RAM " => {
                let count = u32_at(payload, 0) as usize;
                state.ram = (0..count).map(|i| (u32_at(payload, 4 + 5 * i), payload[8 + 5 * i])).collect();
            }
            _ => {}
        }
    }
    state
}

fn read_moo(path: &Path) -> Result<MooFile, String> {
    let raw = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let data = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gz")) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&raw[..]).read_to_end(&mut out).map_err(|e| format!("{}: {e}", path.display()))?;
        out
    } else {
        raw
    };
    let mut file = MooFile { cpu: String::new(), masks: [None; r::COUNT], tests: Vec::new() };
    for (id, payload) in chunks(&data) {
        match &id {
            b"MOO " => file.cpu = String::from_utf8_lossy(&payload[8..12]).trim().to_string(),
            b"RMSK" | b"RM32" => read_registers(&id, payload, &mut file.masks),
            b"TEST" => {
                let mut test = Test {
                    name: String::new(),
                    bytes: Vec::new(),
                    initial: State::default(),
                    last: State::default(),
                    exception: None,
                    flags_at: None,
                    hash: String::new(),
                };
                for (sub, body) in chunks(&payload[4..]) {
                    match &sub {
                        b"NAME" => test.name = String::from_utf8_lossy(&body[4..]).into_owned(),
                        b"BYTS" => test.bytes = body[4..].to_vec(),
                        b"INIT" => test.initial = read_state(body),
                        b"FINA" => test.last = read_state(body),
                        b"HASH" => test.hash = body.iter().map(|b| format!("{b:02x}")).collect(),
                        b"EXCP" => {
                            test.exception = body.first().copied();
                            test.flags_at = (body.len() >= 5).then(|| u32_at(body, 1));
                        }
                        _ => {}
                    }
                }
                file.tests.push(test);
            }
            _ => {}
        }
    }
    Ok(file)
}

// ---- running a test ----

/// 16 MB of memory, sparse; bytes a test doesn't set read as 0.
#[derive(Default)]
struct Memory {
    bytes: HashMap<u32, u8>,
}

impl Bus for Memory {
    fn read_u8(&mut self, addr: u32) -> u8 {
        self.bytes.get(&(addr & 0xFF_FFFF)).copied().unwrap_or(0)
    }
    fn write_u8(&mut self, addr: u32, value: u8) {
        self.bytes.insert(addr & 0xFF_FFFF, value);
    }
    fn port_in_u8(&mut self, _port: u16) -> u8 {
        0xFF
    }
    fn port_out_u8(&mut self, _port: u16, _value: u8) {}
}

enum Outcome {
    Pass,
    Fail(String),
    Unimplemented(String),
    Skipped,
}

/// The opcode after any prefixes.
fn opcode(bytes: &[u8]) -> (u8, Option<u8>) {
    let mut i = 0;
    while i < bytes.len()
        && matches!(bytes[i], 0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 | 0x66 | 0x67 | 0xF0 | 0xF2 | 0xF3)
    {
        i += 1;
    }
    (bytes.get(i).copied().unwrap_or(0), bytes.get(i + 1).copied())
}

/// Flags compared: the defined status and control flags (IOPL and NT exist on the 386 only).
fn flag_mask(model: Model) -> u32 {
    let low = u32::from(
        flags::CF | flags::PF | flags::AF | flags::ZF | flags::SF | flags::TF | flags::IF | flags::DF | flags::OF,
    );
    match model {
        Model::I286 => low,
        Model::I386 => low | u32::from(flags::IOPL | flags::NT),
    }
}

fn value(cpu: &Cpu, index: usize) -> u32 {
    let regs = &cpu.regs;
    match index {
        r::CR0 => cpu.cr0,
        r::CR3 => cpu.cr3,
        r::EAX => regs.gpr[reg::AX],
        r::EBX => regs.gpr[reg::BX],
        r::ECX => regs.gpr[reg::CX],
        r::EDX => regs.gpr[reg::DX],
        r::ESI => regs.gpr[reg::SI],
        r::EDI => regs.gpr[reg::DI],
        r::EBP => regs.gpr[reg::BP],
        r::ESP => regs.gpr[reg::SP],
        r::CS => u32::from(regs.seg[seg::CS]),
        r::DS => u32::from(regs.seg[seg::DS]),
        r::ES => u32::from(regs.seg[seg::ES]),
        r::FS => u32::from(regs.seg[seg::FS]),
        r::GS => u32::from(regs.seg[seg::GS]),
        r::SS => u32::from(regs.seg[seg::SS]),
        r::EIP => regs.ip,
        r::EFLAGS => regs.flags,
        r::DR6 => cpu.dr[6],
        _ => cpu.dr[7],
    }
}

/// The `f_umask` column of the 80386 suite's `80386.csv`: the flags each opcode (and group
/// extension) defines, keyed like the file names without size prefixes (`D0.4`, `0FAF`).
fn flag_masks(csv: &str) -> HashMap<String, u32> {
    let mut lines = csv.lines();
    let header: Vec<&str> = lines.next().unwrap_or_default().split(',').collect();
    let column = |name: &str| header.iter().position(|&h| h == name);
    let (Some(op), Some(group), Some(ext), Some(umask)) = (column("op"), column("g"), column("ex"), column("f_umask"))
    else {
        return HashMap::new();
    };
    let mut masks = HashMap::new();
    for line in lines {
        let fields: Vec<&str> = line.split(',').collect();
        let Some(mask) = fields.get(umask).and_then(|m| u32::from_str_radix(m.trim_start_matches("0x"), 16).ok())
        else {
            continue;
        };
        let key = if fields.get(group).is_some_and(|g| !g.is_empty()) {
            format!("{}.{}", fields[op], fields.get(ext).copied().unwrap_or("0"))
        } else {
            fields[op].to_string()
        };
        masks.insert(key, mask);
    }
    masks
}

/// A test file's name without its operand- and address-size prefixes.
fn base_opcode(name: &str) -> &str {
    let mut rest = name;
    while rest.len() > 2 && (rest.starts_with("66") || rest.starts_with("67")) {
        rest = &rest[2..];
    }
    rest
}

fn run_test(test: &Test, model: Model, file_masks: &Registers) -> Outcome {
    let (op, next) = opcode(&test.bytes);
    // IN, INS: the result is whatever the bus returned during the recording.
    if matches!(op, 0x6C | 0x6D | 0xE4 | 0xE5 | 0xEC | 0xED) {
        return Outcome::Skipped;
    }
    // FE 38: our host-callback encoding (an invalid INC/DEC form on real CPUs).
    if op == 0xFE && next == Some(0x38) {
        return Outcome::Skipped;
    }
    let init = |i: usize| test.initial.regs[i].unwrap_or(0);
    let mut cpu = Cpu::with_model(model);
    cpu.fpu = None;
    let gpr = [r::EAX, r::ECX, r::EDX, r::EBX, r::ESP, r::EBP, r::ESI, r::EDI];
    for (slot, &index) in gpr.iter().enumerate() {
        cpu.regs.gpr[slot] = init(index);
    }
    for (slot, index) in
        [(seg::ES, r::ES), (seg::CS, r::CS), (seg::SS, r::SS), (seg::DS, r::DS), (seg::FS, r::FS), (seg::GS, r::GS)]
    {
        let selector = init(index) as u16;
        cpu.regs.seg[slot] = selector;
        cpu.segs[slot] = SegmentCache { selector, base: u32::from(selector) << 4, limit: 0xFFFF, big: false };
    }
    cpu.regs.ip = init(r::EIP);
    cpu.regs.flags = match model {
        Model::I286 => u32::from(flags::normalize(init(r::EFLAGS) as u16)),
        Model::I386 => init(r::EFLAGS) & (flag_mask(model) | u32::from(flags::FIXED)) | u32::from(flags::FIXED),
    };
    cpu.cr0 = (init(r::CR0) & 0x8000_001F) | if model == Model::I386 { cpu::cr0::ET } else { 0 };
    cpu.cr3 = init(r::CR3);
    cpu.dr[6] = init(r::DR6);
    cpu.dr[7] = init(r::DR7);
    let mut memory = Memory::default();
    for &(addr, byte) in &test.initial.ram {
        memory.write_u8(addr, byte);
    }

    match cpu.step(&mut memory) {
        // The HLT that follows the instruction (or is injected at a jump's target).
        // EIP isn't truncated here: a HLT at offset FFFFh leaves it at 10000h.
        Ok(()) => cpu.regs.ip = cpu.regs.ip.wrapping_add(1),
        Err(Exit::Halt) => {}
        Err(Exit::Unimplemented { bytes, .. }) => {
            return Outcome::Unimplemented(bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "));
        }
        Err(other) => return Outcome::Fail(format!("stopped: {other}")),
    }

    // A jump into the instruction's own bytes: the hardware ran the target from its prefetch
    // queue before the recording could stop it, so the test can't be reproduced.
    let start = init(r::EIP);
    let target = cpu.regs.ip.wrapping_sub(1);
    if target > start && target < start + test.bytes.len() as u32 - 1 && test.last.regs[r::CS].is_none() {
        return Outcome::Skipped;
    }
    let mut problems = Vec::new();
    let wide = model == Model::I386;
    for index in [
        r::EAX,
        r::EBX,
        r::ECX,
        r::EDX,
        r::ESI,
        r::EDI,
        r::EBP,
        r::ESP,
        r::CS,
        r::DS,
        r::ES,
        r::FS,
        r::GS,
        r::SS,
        r::EIP,
        r::EFLAGS,
        r::CR0,
    ] {
        if !wide && matches!(index, r::FS | r::GS | r::CR0) {
            continue;
        }
        let Some(expected) = test.last.regs[index].or(test.initial.regs[index]) else { continue };
        let mut mask = match index {
            r::CS | r::DS | r::ES | r::FS | r::GS | r::SS => 0xFFFF,
            r::EFLAGS => flag_mask(model),
            r::CR0 => 0x8000_000F,
            _ if !wide => 0xFFFF,
            _ => u32::MAX,
        };
        mask &= file_masks[index].unwrap_or(u32::MAX) & test.last.masks[index].unwrap_or(u32::MAX);
        let ours = value(&cpu, index);
        if (ours ^ expected) & mask != 0 {
            problems.push(format!("{} {ours:08X} != {expected:08X}", r::NAMES[index]));
        }
    }
    // FLAGS pushed by an exception may hold flags the instruction left undefined.
    let flag_bytes = file_masks[r::EFLAGS].unwrap_or(u32::MAX).to_le_bytes();
    for &(addr, expected) in &test.last.ram {
        let ours = memory.read_u8(addr);
        let mask = match test.flags_at {
            Some(at) if addr == at => flag_bytes[0],
            Some(at) if addr == at.wrapping_add(1) => flag_bytes[1],
            _ => 0xFF,
        };
        if (ours ^ expected) & mask != 0 {
            problems.push(format!("[{addr:06X}] {ours:02X} != {expected:02X}"));
        }
    }
    if problems.is_empty() { Outcome::Pass } else { Outcome::Fail(problems.join(", ")) }
}

// ---- driver ----

#[derive(Default)]
struct Tally {
    pass: usize,
    fail: usize,
    unimplemented: usize,
    skipped: usize,
    limit_faults: usize,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut folder = None;
    let mut filter = None;
    let mut show = 3usize;
    let mut detail = false;
    let mut skip_limit_faults = false;
    while let Some(arg) = args.next() {
        if arg == "--skip-limit-faults" {
            skip_limit_faults = true;
        } else if arg == "--detail" {
            detail = true;
        } else if arg == "--show" {
            show = args.next().and_then(|n| n.parse().ok()).expect("--show needs a count");
        } else if folder.is_none() {
            folder = Some(PathBuf::from(arg));
        } else {
            filter = Some(arg.to_ascii_uppercase());
        }
    }
    let folder =
        folder.expect("usage: singlestep <suite folder> [name filter] [--show N] [--detail] [--skip-limit-faults]");
    // The 80386 suite's table of undefined flags; the 80286 suite has none, and the same
    // architectural rules apply, so it borrows the 386's when that suite sits beside it.
    let root = folder.parent().unwrap_or(&folder);
    let csv = [root.join("80386.csv"), root.with_file_name("80386").join("80386.csv")]
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    let csv_masks = flag_masks(&csv);
    let revoked: HashSet<String> =
        std::fs::read_to_string(folder.parent().unwrap_or(&folder).join("revocation_list.txt"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().to_ascii_lowercase())
            .filter(|l| !l.is_empty())
            .collect();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&folder)
        .expect("suite folder")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().to_ascii_uppercase().contains(".MOO"))
        .filter(|p| {
            filter.as_ref().is_none_or(|f| {
                p.file_name().is_some_and(|n| n.to_string_lossy().to_ascii_uppercase().starts_with(f.as_str()))
            })
        })
        .collect();
    files.sort();

    let mut total = Tally::default();
    let mut failing: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut unimplemented: BTreeMap<String, usize> = BTreeMap::new();
    for path in &files {
        let name = path.file_name().unwrap_or_default().to_string_lossy().replace(".MOO.gz", "").replace(".MOO", "");
        let file = match read_moo(path) {
            Ok(file) => file,
            Err(e) => {
                println!("{name}: {e}");
                continue;
            }
        };
        let model = if file.cpu == "C286" { Model::I286 } else { Model::I386 };
        let mut masks = file.masks;
        if let Some(&mask) = csv_masks.get(base_opcode(&name)) {
            masks[r::EFLAGS] = Some(masks[r::EFLAGS].unwrap_or(u32::MAX) & mask);
        }
        let mut tally = Tally::default();
        let mut shown = 0;
        for test in &file.tests {
            if revoked.contains(&test.hash) {
                tally.skipped += 1;
                continue;
            }
            if skip_limit_faults && matches!(test.exception, Some(12 | 13)) {
                tally.limit_faults += 1;
                continue;
            }
            match run_test(test, model, &masks) {
                Outcome::Pass => tally.pass += 1,
                Outcome::Skipped => tally.skipped += 1,
                Outcome::Unimplemented(bytes) => {
                    tally.unimplemented += 1;
                    *unimplemented.entry(name.clone()).or_default() += 1;
                    if tally.unimplemented == 1 && show > 0 {
                        println!("{name}: unimplemented: {} ({bytes})", test.name);
                    }
                }
                Outcome::Fail(why) => {
                    tally.fail += 1;
                    if shown < show {
                        shown += 1;
                        let bytes: Vec<String> = test.bytes.iter().map(|b| format!("{b:02X}")).collect();
                        let exception = test.exception.map_or(String::new(), |n| format!(" (exception {n})"));
                        println!("{name}: FAIL {} [{}]{exception}: {why}", test.name, bytes.join(" "));
                        if detail {
                            let regs: Vec<String> = (0..r::COUNT)
                                .filter_map(|i| test.initial.regs[i].map(|v| format!("{}={v:X}", r::NAMES[i])))
                                .collect();
                            println!("    initial {}", regs.join(" "));
                        }
                    }
                }
            }
        }
        if tally.fail > 0 {
            failing.insert(name.clone(), (tally.fail, tally.pass + tally.fail));
        }
        total.pass += tally.pass;
        total.fail += tally.fail;
        total.unimplemented += tally.unimplemented;
        total.skipped += tally.skipped;
        total.limit_faults += tally.limit_faults;
    }
    println!();
    println!(
        "{} files: {} passed, {} failed, {} unimplemented, {} skipped, {} expecting #SS/#GP set apart",
        files.len(),
        total.pass,
        total.fail,
        total.unimplemented,
        total.skipped,
        total.limit_faults
    );
    if !failing.is_empty() {
        let list: Vec<String> = failing.iter().map(|(name, (fail, all))| format!("{name} {fail}/{all}")).collect();
        println!("failing: {}", list.join(", "));
    }
    if !unimplemented.is_empty() {
        let list: Vec<String> = unimplemented.keys().cloned().collect();
        println!("unimplemented: {}", list.join(", "));
    }
}
