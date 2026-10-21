//! `clockwork unpack`: undoes an executable packer (PKLITE, LZEXE, EXEPACK, DIET, ...) by running
//! the program until the packer hands over to it, whatever the packer.
//!
//! Packers decompress the program into its load area, then jump to its entry point from their
//! own code, which sits higher in the program's memory block (where they moved themselves). So
//! the hand-over is the first transfer that leaves one code segment of the block for a lower
//! address in the block, landing on bytes that weren't there at load time. Interrupts don't
//! count: their handlers and returns run outside the block.
//!
//! At that point the load area holds the program as its own loader would have left it, with
//! relocations applied for the segment it was loaded at. Running it a second time loaded higher
//! (the environment block, allocated before the program, is padded) tells relocations apart: they
//! are the words that differ by exactly the difference of the load segments. The result is
//! written as a plain MZ executable. The image runs up to the program's stack top (or the end
//! of its memory block), so past the original file image it may hold the packer's leftovers.
//!
//! A flat program under a DOS extender (an LE executable) is placed by the extender's loader
//! instead: Clockwork runs it until its objects are in memory ([`dos::le`]), then again to its entry
//! point, and writes the objects as they are then, at their linear addresses, as an ELF executable:
//! the form analysers load at given addresses (each object a segment, in the LE's order).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dos::{Config, Machine, Stop, Vfs};

use crate::options::USAGE;

/// Instructions to run at most before giving up on finding the hand-over.
const MAX_INSTRUCTIONS: u64 = 400_000_000;
/// Environment padding for the second run, in bytes: moves the program up by about 23h
/// paragraphs, so a relocated word changes in its low byte and not by a multiple of 256.
const PADDING: usize = 0x230;

/// An unpacked program: its load image and what an MZ header needs.
pub struct Unpacked {
    pub image: Vec<u8>,
    /// Image offsets of the words holding a segment (relative to the load segment).
    pub relocations: Vec<u32>,
    pub cs: u16,
    pub ip: u16,
    pub ss: u16,
    pub sp: u16,
    /// Instructions the packer ran.
    pub instructions: u64,
    /// Bytes that differed between the two runs without being part of a relocation.
    pub stray_bytes: usize,
}

/// The program's state at the hand-over.
struct Handover {
    load_segment: u16,
    image: Vec<u8>,
    cs: u16,
    ip: u16,
    ss: u16,
    sp: u16,
    instructions: u64,
}

fn boot(path: &Path, padding: usize) -> Result<Machine, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).ok_or("bad program file name")?.to_ascii_uppercase();
    let mut drive = Vfs::new();
    drive.insert_file(&name, data);
    let mut config = Config::default();
    if padding > 0 {
        config.environment.push(("REC_UNPACK_PADDING".into(), "x".repeat(padding)));
    }
    Machine::with_program(config, drive, &name, "").map_err(|e| e.to_string())
}

fn run_to_handover(m: &mut Machine) -> Result<Handover, String> {
    let psp = u32::from(m.dos.psp);
    let std::ops::Range { start: base, end: block_end } = m.image_range();
    let in_block = |linear: u32| (base..block_end).contains(&linear);
    let loaded = m.mem.read_bytes(base, (block_end - base) as usize);
    let position = |m: &Machine| (m.cpu.regs.cs(), (u32::from(m.cpu.regs.cs()) << 4) + m.cpu.regs.ip);
    let start = m.cpu.cycles;
    let mut before = position(m);
    while m.cpu.cycles - start < MAX_INSTRUCTIONS {
        match m.run(1) {
            Stop::CycleLimit => {}
            other => return Err(format!("the program stopped before the packer handed over: {other:?}")),
        }
        let after = position(m);
        if after.0 != before.0 && in_block(before.1) && in_block(after.1) && after.1 < before.1 {
            let at = (after.1 - base) as usize;
            let end = (at + 16).min(loaded.len());
            if m.mem.read_bytes(after.1, end - at) != loaded[at..end] {
                let regs = m.cpu.regs;
                let load_segment = (psp + 0x10) as u16;
                let stack_top = (u32::from(regs.ss()) << 4) + u32::from(regs.sp());
                let top = if stack_top > after.1 && stack_top <= block_end { stack_top } else { block_end };
                return Ok(Handover {
                    load_segment,
                    image: m.mem.read_bytes(base, (top - base) as usize),
                    cs: regs.cs().wrapping_sub(load_segment),
                    ip: regs.ip as u16,
                    ss: regs.ss().wrapping_sub(load_segment),
                    sp: regs.sp(),
                    instructions: m.cpu.cycles - start,
                });
            }
        }
        before = after;
    }
    Err(format!("no hand-over within {MAX_INSTRUCTIONS} instructions: is the program packed?"))
}

/// Unpacks the program at `path`.
pub fn unpack(path: &Path) -> Result<Unpacked, String> {
    let first = run_to_handover(&mut boot(path, 0)?)?;
    let second = run_to_handover(&mut boot(path, PADDING)?)?;
    let shift = second.load_segment.wrapping_sub(first.load_segment);
    if shift == 0 || (first.cs, first.ip, first.ss, first.sp) != (second.cs, second.ip, second.ss, second.sp) {
        return Err("the two runs handed over differently".into());
    }
    let length = first.image.len().min(second.image.len());
    let (a, b) = (&first.image[..length], &second.image[..length]);
    let mut relocations = Vec::new();
    let mut stray_bytes = 0;
    let mut i = 0;
    while i < length {
        if i + 1 < length {
            let (wa, wb) = (u16::from_le_bytes([a[i], a[i + 1]]), u16::from_le_bytes([b[i], b[i + 1]]));
            if wb.wrapping_sub(wa) == shift {
                relocations.push(i as u32);
                i += 2;
                continue;
            }
        }
        stray_bytes += usize::from(a[i] != b[i]);
        i += 1;
    }
    // The image as if loaded at segment 0: relocated words hold relative segments.
    let mut image = a.to_vec();
    for &at in &relocations {
        let at = at as usize;
        let value = u16::from_le_bytes([image[at], image[at + 1]]).wrapping_sub(first.load_segment);
        image[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }
    Ok(Unpacked {
        image,
        relocations,
        cs: first.cs,
        ip: first.ip,
        ss: first.ss,
        sp: first.sp,
        instructions: first.instructions,
        stray_bytes,
    })
}

/// The unpacked program as an MZ executable: the image needs no extra memory (it already runs to
/// the stack top), and may have all there is.
pub fn mz_file(u: &Unpacked) -> Vec<u8> {
    let header_len = (0x1C + 4 * u.relocations.len()).next_multiple_of(16);
    let file_len = header_len + u.image.len();
    let mut out = Vec::with_capacity(file_len);
    for word in [
        0x5A4D,
        (file_len % 512) as u16,
        file_len.div_ceil(512) as u16,
        u.relocations.len() as u16,
        (header_len / 16) as u16,
        0,      // minimum extra paragraphs
        0xFFFF, // maximum
        u.ss,
        u.sp,
        0, // checksum
        u.ip,
        u.cs,
        0x1C, // relocation table
        0,    // overlay number
    ] {
        out.extend(word.to_le_bytes());
    }
    for &at in &u.relocations {
        out.extend(((at & 0xF) as u16).to_le_bytes());
        out.extend(((at >> 4) as u16).to_le_bytes());
    }
    out.resize(header_len, 0);
    out.extend(&u.image);
    out
}

/// `clockwork unpack`.
/// A flat program as its extender's loader placed it: each object's base, bytes and LE flags, at the
/// entry point.
pub struct Placed {
    pub objects: Vec<(u32, Vec<u8>, u32)>,
    pub entry: u32,
    /// Instructions run to the entry point.
    pub instructions: u64,
}

/// Runs the LE program at `path` until its objects are placed, then again to its entry point.
pub fn place(path: &Path, le: &dos::le::Le) -> Result<Placed, String> {
    let mut m = boot(path, 0)?;
    let slice = m.config.instructions_per_second / 20;
    let bases = loop {
        let stop = m.run(slice);
        if let Some(bases) = le.bases(&m) {
            break bases;
        }
        if stop != Stop::CycleLimit || m.cpu.cycles > MAX_INSTRUCTIONS {
            return Err(format!(
                "the extender didn't place the program's objects ({})",
                crate::summary::stop_text(&stop)
            ));
        }
    };
    let entry = bases[le.entry.0].wrapping_add(le.entry.1);
    let mut m = boot(path, 0)?;
    match m.run_to_sync(entry, 1, MAX_INSTRUCTIONS) {
        Stop::Sync { .. } => {}
        other => {
            return Err(format!("the program didn't reach its entry point ({})", crate::summary::stop_text(&other)));
        }
    }
    let objects = le
        .objects
        .iter()
        .zip(&bases)
        .map(|(o, &base)| {
            (base, (0..o.size).map(|i| m.peek_linear_u8(base.wrapping_add(i)).unwrap_or(0)).collect(), o.flags)
        })
        .collect();
    Ok(Placed { objects, entry, instructions: m.cpu.cycles })
}

/// The placed objects as a 32-bit x86 ELF executable: a loadable segment for each, at its base, and
/// the entry point.
pub fn elf_file(p: &Placed) -> Vec<u8> {
    let count = p.objects.len() as u32;
    let mut offset = 52 + 32 * count;
    let mut out = Vec::new();
    out.extend_from_slice(b"\x7FELF\x01\x01\x01");
    out.resize(16, 0);
    let half = |out: &mut Vec<u8>, v: u16| out.extend_from_slice(&v.to_le_bytes());
    let word = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    half(&mut out, 2); // an executable
    half(&mut out, 3); // i386
    word(&mut out, 1);
    word(&mut out, p.entry);
    word(&mut out, 52); // program headers
    word(&mut out, 0); // no section headers
    word(&mut out, 0);
    for v in [52, 32, count as u16, 40, 0, 0] {
        half(&mut out, v);
    }
    for (base, bytes, flags) in &p.objects {
        // LE: 1 readable, 2 writable, 4 executable; ELF: 4, 2, 1.
        let elf_flags = (flags & 1) << 2 | (flags & 2) | (flags & 4) >> 2;
        for v in [1, offset, *base, *base, bytes.len() as u32, bytes.len() as u32, elf_flags, 1] {
            word(&mut out, v);
        }
        offset += bytes.len() as u32;
    }
    for (_, bytes, _) in &p.objects {
        out.extend_from_slice(bytes);
    }
    out
}

pub(crate) fn cmd_unpack(args: &[String]) -> Result<ExitCode, String> {
    let (mut program, mut out, mut json) = (None, None, false);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(args.next().ok_or("--out needs a file")?)),
            "--json" => json = true,
            flag if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n\n{USAGE}"));
            }
            path if program.is_none() => program = Some(PathBuf::from(path)),
            extra => return Err(format!("unexpected argument {extra:?}")),
        }
    }
    let program = program.ok_or("unpack needs a program")?;
    let out = out.ok_or("unpack needs --out FILE")?;
    let data = std::fs::read(&program).map_err(|e| format!("{}: {e}", program.display()))?;
    if let Some(le) = dos::le::Le::parse(&data)? {
        let placed = place(&program, &le)?;
        std::fs::write(&out, elf_file(&placed)).map_err(|e| format!("{}: {e}", out.display()))?;
        let objects: Vec<String> = placed
            .objects
            .iter()
            .enumerate()
            .map(|(i, (base, bytes, flags))| {
                format!("{} at {base:08X} ({} bytes, flags {flags:X})", i + 1, bytes.len())
            })
            .collect();
        if json {
            let objects: Vec<String> = placed
                .objects
                .iter()
                .map(|(base, bytes, flags)| {
                    format!(r#"{{"base":"{base:08X}","size":{},"flags":{flags}}}"#, bytes.len())
                })
                .collect();
            println!(
                r#"{{"format":"elf","entry":"{:08X}","objects":[{}],"instructions":{}}}"#,
                placed.entry,
                objects.join(","),
                placed.instructions
            );
        } else {
            println!(
                "placed    LE objects {}; entry {:08X} ({} instructions)",
                objects.join(", "),
                placed.entry,
                placed.instructions
            );
        }
        return Ok(ExitCode::SUCCESS);
    }
    let unpacked = unpack(&program)?;
    std::fs::write(&out, mz_file(&unpacked)).map_err(|e| format!("{}: {e}", out.display()))?;
    let u = &unpacked;
    if json {
        println!(
            r#"{{"entry":"{:04X}:{:04X}","stack":"{:04X}:{:04X}","image_size":{},"relocations":{},"instructions":{},"stray_bytes":{}}}"#,
            u.cs,
            u.ip,
            u.ss,
            u.sp,
            u.image.len(),
            u.relocations.len(),
            u.instructions,
            u.stray_bytes
        );
    } else {
        println!(
            "unpacked  entry {:04X}:{:04X}, stack {:04X}:{:04X}, image {} bytes, {} relocations ({} instructions)",
            u.cs,
            u.ip,
            u.ss,
            u.sp,
            u.image.len(),
            u.relocations.len(),
            u.instructions
        );
        if u.stray_bytes > 0 {
            println!("warning   {} bytes differ between the two runs outside relocations", u.stray_bytes);
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mz_header_fields() {
        let u = Unpacked {
            image: vec![0x90; 1000],
            relocations: vec![0x12, 0x10005],
            cs: 1,
            ip: 2,
            ss: 3,
            sp: 4,
            instructions: 0,
            stray_bytes: 0,
        };
        let file = mz_file(&u);
        let word = |i: usize| u16::from_le_bytes([file[i], file[i + 1]]);
        assert_eq!(file.len(), 0x30 + 1000);
        assert_eq!((word(0), word(2), word(4), word(6), word(8)), (0x5A4D, 1048 % 512, 3, 2, 3));
        assert_eq!((word(0x0E), word(0x10), word(0x14), word(0x16)), (3, 4, 2, 1));
        assert_eq!((word(0x1C), word(0x1E), word(0x20), word(0x22)), (2, 1, 5, 0x1000));
    }
}
