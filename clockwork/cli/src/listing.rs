//! `clockwork listing IMAGE --entry OFFSET --segment SEG`: a function's code from a load image,
//! found by following its control flow from the entry, so a jump table's cases are in it however
//! the analysis split the function. Lines are image offsets (`+135F5`), as function IDs have
//! them; near and far branch and call targets are image offsets too; a jump through a table in
//! the code segment lists the table.
//!
//! `clockwork listing FILE.ELF --entry LINEAR`: the same for a flat program's 32-bit code, from the
//! ELF file `clockwork unpack` writes for an LE program (its objects at their linear addresses).
//! Lines and targets are linear addresses (`@435024`); a jump through a table of 32-bit addresses
//! lists the table.

use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;

use iced_x86::{Decoder, DecoderOptions, FlowControl, Formatter, Instruction, IntelFormatter, OpKind, Register};

/// Most instructions a listing follows.
const MAX_INSTRUCTIONS: usize = 6000;
/// Most entries read from a jump table whose size isn't evident.
const MAX_TABLE: usize = 64;

pub fn cmd_listing(args: &[String]) -> Result<ExitCode, String> {
    let mut path = None;
    let (mut entry, mut segment) = (None, None);
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut hex = |name: &str| -> Result<u32, String> {
            let value = iter.next().ok_or_else(|| format!("{name} needs a value"))?;
            u32::from_str_radix(value.trim_start_matches('+'), 16).map_err(|_| format!("{name}: bad hex {value:?}"))
        };
        match arg.as_str() {
            "--entry" => entry = Some(hex(arg)?),
            "--segment" => segment = Some(hex(arg)?),
            other if path.is_none() => path = Some(other.to_string()),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let path = path.ok_or("usage: clockwork listing IMAGE --entry OFFSET --segment SEG | FILE.ELF --entry LINEAR")?;
    let image = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    let entry = entry.ok_or("--entry OFFSET (image offset, or linear address in an ELF file, hex) is needed")?;
    if image.starts_with(b"\x7FELF") {
        print!("{}", flat_listing(&image, entry)?);
        return Ok(ExitCode::SUCCESS);
    }
    let segment = segment.ok_or("--segment SEG (the function's link segment, hex) is needed")?;
    print!("{}", listing(&image, entry, segment)?);
    Ok(ExitCode::SUCCESS)
}

/// The listing of 16-bit code in segment `segment` of a load image, as text.
pub fn listing(image: &[u8], entry: u32, segment: u32) -> Result<String, String> {
    let base = segment * 16;
    if entry < base || entry - base > 0xFFFF || entry as usize >= image.len() {
        return Err(format!("+{entry:05X} isn't in segment {segment:04X} of a {} byte image", image.len()));
    }
    let code = Code {
        bitness: 16,
        mask: 0xFFFF,
        bytes: &|ip: u32| {
            let at = (base + ip) as usize;
            image.get(at..(at + 15).min(image.len()))
        },
        name: &|ip: u32| format!("+{:05X}", base + ip),
        table: &|jump: &Instruction| {
            // `jmp word ptr cs:[reg+table]`: 16-bit entries in the code segment.
            let ok = jump.memory_segment() == Register::CS && jump.memory_base() != Register::None;
            ok.then(|| jump.memory_displacement32() & 0xFFFF)
        },
        entry_at: &|table: u32, i: usize| {
            let at = (base + table) as usize + 2 * i;
            Some(u32::from(u16::from_le_bytes([*image.get(at)?, *image.get(at + 1)?])))
        },
        is_code: &|target: u32| ((base + target) as usize) < image.len(),
    };
    let header = format!("code segment {segment:04X} (runtime CS = load segment + {segment:04X}); image offsets");
    Ok(code.list(entry - base, &header))
}

/// The listing of a flat program's 32-bit code from the ELF file `clockwork unpack` writes (its
/// program headers place the objects at their linear addresses), as text.
pub fn flat_listing(elf: &[u8], entry: u32) -> Result<String, String> {
    let half = |at: usize| Some(usize::from(u16::from_le_bytes(elf.get(at..at + 2)?.try_into().ok()?)));
    let word = |at: usize| Some(u32::from_le_bytes(elf.get(at..at + 4)?.try_into().ok()?));
    // Each segment: (linear address, its bytes in the file).
    let segments: Option<Vec<(u32, &[u8])>> = (|| {
        let (table, size, count) = (word(28)? as usize, half(42)?, half(44)?);
        (0..count)
            .map(|i| {
                let header = table + i * size;
                let (offset, address, length) = (word(header + 4)? as usize, word(header + 8)?, word(header + 16)?);
                Some((address, elf.get(offset..offset + length as usize)?))
            })
            .collect()
    })();
    let segments = segments.ok_or("not an ELF file clockwork unpack wrote")?;
    let find = |at: u32| segments.iter().find(|(start, bytes)| at.wrapping_sub(*start) < bytes.len() as u32);
    if find(entry).is_none() {
        return Err(format!("@{entry:X} isn't in the program's objects"));
    }
    let bytes = |at: u32| {
        let (start, bytes) = find(at)?;
        let at = (at - start) as usize;
        bytes.get(at..(at + 15).min(bytes.len()))
    };
    let code = Code {
        bitness: 32,
        mask: 0xFFFF_FFFF,
        bytes: &bytes,
        name: &|at: u32| format!("@{at:X}"),
        table: &|jump: &Instruction| {
            // `jmp dword ptr [reg*4+table]`: 32-bit entries at a linear address.
            let ok = jump.memory_index() != Register::None && jump.memory_index_scale() == 4;
            ok.then(|| jump.memory_displacement32())
        },
        entry_at: &|table: u32, i: usize| {
            let at = table.wrapping_add(4 * i as u32);
            let b = bytes(at)?;
            Some(u32::from_le_bytes(b.get(..4)?.try_into().ok()?))
        },
        is_code: &|target: u32| find(target).is_some(),
    };
    Ok(code.list(entry, "flat code (32-bit); linear addresses"))
}

/// Where code is read from and how it is named: a 16-bit segment of a load image (instruction
/// pointers are offsets in it) or a flat program's memory (linear addresses).
struct Code<'a> {
    bitness: u32,
    /// Instruction pointers wrap at this.
    mask: u32,
    /// Up to 15 bytes at an instruction pointer.
    bytes: &'a dyn Fn(u32) -> Option<&'a [u8]>,
    /// An instruction pointer as the listing names it.
    name: &'a dyn Fn(u32) -> String,
    /// A jump through a table: where the table is.
    table: &'a dyn Fn(&Instruction) -> Option<u32>,
    /// A table's entry `i`.
    entry_at: &'a dyn Fn(u32, usize) -> Option<u32>,
    /// Whether a target is in the code (a table without a bound ends where its entries stop being).
    is_code: &'a dyn Fn(u32) -> bool,
}

impl Code<'_> {
    fn decode(&self, ip: u32) -> Option<Instruction> {
        let bytes = (self.bytes)(ip)?;
        let mut decoder = Decoder::with_ip(self.bitness, bytes, u64::from(ip), DecoderOptions::NONE);
        let instruction = decoder.decode();
        (!instruction.is_invalid()).then_some(instruction)
    }

    /// The code reached from `entry`, as text under `header`.
    fn list(&self, entry: u32, header: &str) -> String {
        let mut code: BTreeMap<u32, Instruction> = BTreeMap::new();
        let mut tables: BTreeMap<u32, (u32, Vec<u32>)> = BTreeMap::new();
        let mut invalid = BTreeSet::new();
        let mut queue = vec![entry];
        while let Some(mut ip) = queue.pop() {
            while !code.contains_key(&ip) && code.len() < MAX_INSTRUCTIONS {
                let Some(instruction) = self.decode(ip) else {
                    invalid.insert(ip);
                    break;
                };
                code.insert(ip, instruction);
                let next = instruction.next_ip() as u32 & self.mask;
                match instruction.flow_control() {
                    FlowControl::Next | FlowControl::Call | FlowControl::IndirectCall | FlowControl::Interrupt => {
                        ip = next
                    }
                    FlowControl::ConditionalBranch => {
                        if near(&instruction) {
                            queue.push(instruction.near_branch_target() as u32 & self.mask);
                        }
                        ip = next;
                    }
                    FlowControl::UnconditionalBranch => {
                        if near(&instruction) {
                            queue.push(instruction.near_branch_target() as u32 & self.mask);
                        }
                        break;
                    }
                    FlowControl::IndirectBranch => {
                        if let Some((table, targets)) = self.jump_table(&instruction, &code, ip) {
                            queue.extend(targets.iter().copied());
                            tables.insert(ip, (table, targets));
                        }
                        break;
                    }
                    _ => break,
                }
            }
        }
        let mut formatter = IntelFormatter::new();
        formatter.options_mut().set_hex_prefix("");
        formatter.options_mut().set_hex_suffix("h");
        formatter.options_mut().set_uppercase_hex(true);
        formatter.options_mut().set_space_after_operand_separator(false);
        let name = self.name;
        let mut out = format!("{header}\n");
        let mut expected = None;
        for (&ip, instruction) in &code {
            if expected.is_some_and(|e| e != ip) {
                out.push_str("  ...\n");
            }
            expected = Some(instruction.next_ip() as u32 & self.mask);
            let mut text = String::new();
            let mnemonic = format!("{:?}", instruction.mnemonic()).to_ascii_lowercase();
            if near(instruction) {
                text = format!("{mnemonic} {}", name(instruction.near_branch_target() as u32 & self.mask));
            } else if instruction.op0_kind() == OpKind::FarBranch16 && self.bitness == 16 {
                let target = u32::from(instruction.far_branch_selector()) * 16 + u32::from(instruction.far_branch16());
                text = format!("{mnemonic} far +{target:05X}");
            } else {
                formatter.format(instruction, &mut text);
            }
            out.push_str(&format!("{}  {text}\n", name(ip)));
            if let Some((table, targets)) = tables.get(&ip) {
                let cases: Vec<String> =
                    targets.iter().enumerate().map(|(i, &t)| format!("{i}: {}", name(t))).collect();
                out.push_str(&format!("        ; table at {}: {}\n", name(*table), cases.join(", ")));
            }
        }
        if code.len() >= MAX_INSTRUCTIONS {
            out.push_str(&format!("  ... stopped after {MAX_INSTRUCTIONS} instructions\n"));
        }
        for ip in invalid {
            out.push_str(&format!("{}  (not an instruction: control reaches data?)\n", name(ip)));
        }
        out
    }

    /// A jump through a table: the table and its targets. Its size is the bound checked just before
    /// (`cmp reg,N` then a jump past the table: N + 1 entries); without one, the entries that look like
    /// code, up to [`MAX_TABLE`].
    fn jump_table(&self, jump: &Instruction, code: &BTreeMap<u32, Instruction>, ip: u32) -> Option<(u32, Vec<u32>)> {
        if jump.op0_kind() != OpKind::Memory {
            return None;
        }
        let table = (self.table)(jump)?;
        let bound = code
            .range(..ip)
            .rev()
            .take(8)
            .find(|(_, i)| {
                format!("{:?}", i.mnemonic()) == "Cmp" && i.op0_kind() == OpKind::Register && is_immediate(i.op1_kind())
            })
            .map(|(_, i)| i.immediate(1) as usize + 1);
        let width = self.bitness / 8;
        let mut targets = Vec::new();
        for i in 0..bound.unwrap_or(MAX_TABLE).min(256) {
            let Some(target) = (self.entry_at)(table, i) else { break };
            // Without a bound: stop at what can't be a case (outside the code, or into the table).
            if bound.is_none() && (!(self.is_code)(target) || target.wrapping_sub(table) < width * MAX_TABLE as u32) {
                break;
            }
            targets.push(target);
        }
        (!targets.is_empty()).then_some((table, targets))
    }
}

/// A near (relative or absolute 16-bit) branch or call.
fn near(instruction: &Instruction) -> bool {
    matches!(instruction.op0_kind(), OpKind::NearBranch16 | OpKind::NearBranch32)
}

fn is_immediate(kind: OpKind) -> bool {
    matches!(
        kind,
        OpKind::Immediate8
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_jump_table_s_cases_are_followed() {
        // Segment 10h (image offset 100h): cmp bx,2; ja 10h; shl bx,1; jmp cs:[bx+20h];
        // cases 0Ch ret, 0Dh ret, 0Eh retf (iced: "ret far"); 10h: call 0Ch; ret. Table at 20h.
        let mut image = vec![0u8; 0x100];
        let mut code = vec![0x83, 0xFB, 0x02, 0x77, 0x0B, 0xD1, 0xE3, 0x2E, 0xFF, 0xA7, 0x20, 0x00];
        code.extend([0xC3, 0xC3, 0xCB, 0x90, 0xE8, 0xF9, 0xFF, 0xC3]);
        code.resize(0x20, 0);
        code.extend([0x0C, 0x00, 0x0D, 0x00, 0x0E, 0x00]);
        image.extend(code);
        let text = listing(&image, 0x100, 0x10).unwrap();
        assert!(text.starts_with("code segment 0010"), "{text}");
        for line in [
            "+00103  ja +00110",
            "+00107  jmp word ptr cs:[bx+20h]",
            "        ; table at +00120: 0: +0010C, 1: +0010D, 2: +0010E",
            "+0010C  ret",
            "+0010E  ret far",
            "+00110  call +0010C",
        ] {
            assert!(text.lines().any(|l| l == line), "{line:?} in:\n{text}");
        }
        assert!(!text.contains("+0010F"), "the NOP is never reached:\n{text}");
        assert!(listing(&image, 0x50, 0x10).is_err(), "an entry outside the segment");
    }

    #[test]
    fn a_flat_program_s_code_is_listed_by_linear_address() {
        // At 435000h: cmp eax,2; ja 11h; jmp dword ptr [eax*4+435020h]; cases 0Ch-0Eh ret; 11h: call
        // 0Ch; ret. The table at 20h. In an ELF file as clockwork unpack writes one.
        let mut code = vec![0x83, 0xF8, 0x02, 0x77, 0x0C, 0xFF, 0x24, 0x85, 0x20, 0x50, 0x43, 0x00];
        code.extend([0xC3, 0xC3, 0xC3, 0x90, 0x90, 0xE8, 0xF6, 0xFF, 0xFF, 0xFF, 0xC3]);
        code.resize(0x20, 0);
        for case in [0x43500C_u32, 0x43500D, 0x43500E] {
            code.extend(case.to_le_bytes());
        }
        let mut elf = vec![0u8; 0x54];
        elf[..4].copy_from_slice(b"\x7FELF");
        elf[28..32].copy_from_slice(&0x34u32.to_le_bytes()); // program headers
        elf[42..44].copy_from_slice(&32u16.to_le_bytes());
        elf[44..46].copy_from_slice(&1u16.to_le_bytes());
        let header = [1, 0x54, 0x435000, 0x435000, code.len() as u32, code.len() as u32, 5, 0x1000];
        for (i, field) in header.iter().enumerate() {
            elf[0x34 + 4 * i..0x38 + 4 * i].copy_from_slice(&field.to_le_bytes());
        }
        elf.extend(&code);
        let text = flat_listing(&elf, 0x435000).unwrap();
        assert!(text.starts_with("flat code (32-bit)"), "{text}");
        for line in [
            "@435003  ja @435011",
            "@435005  jmp dword ptr [eax*4+435020h]",
            "        ; table at @435020: 0: @43500C, 1: @43500D, 2: @43500E",
            "@43500E  ret",
            "@435011  call @43500C",
        ] {
            assert!(text.lines().any(|l| l == line), "{line:?} in:\n{text}");
        }
        assert!(!text.contains("@43500F"), "the NOP is never reached:\n{text}");
        assert!(flat_listing(&elf, 0x500000).is_err(), "an entry outside the objects");
    }
}
