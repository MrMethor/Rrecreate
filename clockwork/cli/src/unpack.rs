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

use std::path::Path;

use dos::{Config, Machine, Stop, Vfs};

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
    let base = (psp + 0x10) << 4;
    let block_paragraphs = u32::from(m.mem.read_u16(((psp - 1) << 4) + 3));
    let block_end = (psp + block_paragraphs) << 4;
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
