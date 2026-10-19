//! `rec-host listing IMAGE --entry OFFSET --segment SEG`: a function's code from a load image,
//! found by following its control flow from the entry, so a jump table's cases are in it however
//! the analysis split the function. Lines are image offsets (`+135F5`), as function IDs and
//! overrides have them; near and far branch and call targets are image offsets too; a jump through
//! a table in the code segment lists the table. `rec context` shows it in task packets.

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
    let path = path.ok_or("usage: rec-host listing IMAGE --entry OFFSET --segment SEG")?;
    let image = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    let entry = entry.ok_or("--entry OFFSET (image offset, hex) is needed")?;
    let segment = segment.ok_or("--segment SEG (the function's link segment, hex) is needed")?;
    print!("{}", listing(&image, entry, segment)?);
    Ok(ExitCode::SUCCESS)
}

/// The listing as text.
pub fn listing(image: &[u8], entry: u32, segment: u32) -> Result<String, String> {
    let base = segment * 16;
    if entry < base || entry - base > 0xFFFF || entry as usize >= image.len() {
        return Err(format!("+{entry:05X} isn't in segment {segment:04X} of a {} byte image", image.len()));
    }
    let decode = |ip: u32| -> Option<Instruction> {
        let at = (base + ip) as usize;
        let bytes = image.get(at..(at + 15).min(image.len()))?;
        let mut decoder = Decoder::with_ip(16, bytes, u64::from(ip), DecoderOptions::NONE);
        let instruction = decoder.decode();
        (!instruction.is_invalid()).then_some(instruction)
    };
    let mut code: BTreeMap<u32, Instruction> = BTreeMap::new();
    let mut tables: BTreeMap<u32, (u32, Vec<u32>)> = BTreeMap::new();
    let mut invalid = BTreeSet::new();
    let mut queue = vec![entry - base];
    while let Some(mut ip) = queue.pop() {
        while !code.contains_key(&ip) && code.len() < MAX_INSTRUCTIONS {
            let Some(instruction) = decode(ip) else {
                invalid.insert(ip);
                break;
            };
            code.insert(ip, instruction);
            let next = instruction.next_ip() as u32 & 0xFFFF;
            match instruction.flow_control() {
                FlowControl::Next | FlowControl::Call | FlowControl::IndirectCall | FlowControl::Interrupt => ip = next,
                FlowControl::ConditionalBranch => {
                    if near(&instruction) {
                        queue.push(instruction.near_branch_target() as u32 & 0xFFFF);
                    }
                    ip = next;
                }
                FlowControl::UnconditionalBranch => {
                    if near(&instruction) {
                        queue.push(instruction.near_branch_target() as u32 & 0xFFFF);
                    }
                    break;
                }
                FlowControl::IndirectBranch => {
                    if let Some((table, targets)) = jump_table(image, base, &instruction, &code, ip) {
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
    let mut out = format!("code segment {segment:04X} (runtime CS = load segment + {segment:04X}); image offsets\n");
    let mut expected = None;
    for (&ip, instruction) in &code {
        if expected.is_some_and(|e| e != ip) {
            out.push_str("  ...\n");
        }
        expected = Some(instruction.next_ip() as u32 & 0xFFFF);
        let mut text = String::new();
        if near(instruction) {
            let mnemonic = format!("{:?}", instruction.mnemonic()).to_ascii_lowercase();
            text = format!("{mnemonic} +{:05X}", base + (instruction.near_branch_target() as u32 & 0xFFFF));
        } else if instruction.op0_kind() == OpKind::FarBranch16 {
            let mnemonic = format!("{:?}", instruction.mnemonic()).to_ascii_lowercase();
            let target = u32::from(instruction.far_branch_selector()) * 16 + u32::from(instruction.far_branch16());
            text = format!("{mnemonic} far +{target:05X}");
        } else {
            formatter.format(instruction, &mut text);
        }
        out.push_str(&format!("+{:05X}  {text}\n", base + ip));
        if let Some((table, targets)) = tables.get(&ip) {
            let cases: Vec<String> =
                targets.iter().enumerate().map(|(i, t)| format!("{i}: +{:05X}", base + t)).collect();
            out.push_str(&format!("        ; table at +{:05X}: {}\n", base + table, cases.join(", ")));
        }
    }
    if code.len() >= MAX_INSTRUCTIONS {
        out.push_str(&format!("  ... stopped after {MAX_INSTRUCTIONS} instructions\n"));
    }
    for ip in invalid {
        out.push_str(&format!("+{:05X}  (not an instruction: control reaches data?)\n", base + ip));
    }
    Ok(out)
}

/// A near (relative or absolute 16-bit) branch or call.
fn near(instruction: &Instruction) -> bool {
    matches!(instruction.op0_kind(), OpKind::NearBranch16 | OpKind::NearBranch32)
}

/// `jmp word ptr cs:[reg+table]`: the table's offset and its targets. Its size is the bound
/// checked just before (`cmp reg,N` then a jump past the table: N + 1 entries); without one, the
/// entries that look like code in the segment, up to [`MAX_TABLE`].
fn jump_table(
    image: &[u8],
    base: u32,
    jump: &Instruction,
    code: &BTreeMap<u32, Instruction>,
    ip: u32,
) -> Option<(u32, Vec<u32>)> {
    if jump.op0_kind() != OpKind::Memory
        || jump.memory_segment() != Register::CS
        || jump.memory_base() == Register::None
    {
        return None;
    }
    let table = jump.memory_displacement32() & 0xFFFF;
    let bound = code
        .range(..ip)
        .rev()
        .take(8)
        .find(|(_, i)| {
            format!("{:?}", i.mnemonic()) == "Cmp" && i.op0_kind() == OpKind::Register && is_immediate(i.op1_kind())
        })
        .map(|(_, i)| i.immediate(1) as usize + 1);
    let read = |i: usize| -> Option<u32> {
        let at = (base + table) as usize + 2 * i;
        Some(u32::from(u16::from_le_bytes([*image.get(at)?, *image.get(at + 1)?])))
    };
    let mut targets = Vec::new();
    for i in 0..bound.unwrap_or(MAX_TABLE).min(256) {
        let Some(target) = read(i) else { break };
        // Without a bound: stop at what can't be a case (outside the image, or into the table).
        if bound.is_none()
            && ((base + target) as usize >= image.len() || (table..table + 2 * MAX_TABLE as u32).contains(&target))
        {
            break;
        }
        targets.push(target);
    }
    (!targets.is_empty()).then_some((table, targets))
}

fn is_immediate(kind: OpKind) -> bool {
    matches!(kind, OpKind::Immediate8 | OpKind::Immediate16 | OpKind::Immediate8to16)
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
}
