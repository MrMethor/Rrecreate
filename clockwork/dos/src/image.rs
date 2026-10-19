//! The loaded image, for analysis: the program's memory as it is now (after loading, unpacking, an
//! extender's loader, code generated at run time), with what an analyzer needs to place it.
//!
//! In real mode that is the program's block from its load segment (its PSP's paragraph 10h) to the
//! block's end: image offsets are offsets in it, and segment values in its code are those of this
//! load segment. In protected mode it is the descriptor tables' code and data segments (the selector
//! map) and the memory they cover; segments as big as memory (a flat model) are listed but not
//! dumped, since their extent says nothing about where the program is.

use crate::machine::Machine;

/// Segments larger than this aren't dumped.
pub const MAX_REGION: u32 = 0x100_0000;

/// A code or data segment from the GDT or the LDT.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Segment {
    pub selector: u16,
    pub base: u32,
    pub limit: u32,
    pub code: bool,
    /// 32-bit (the D/B bit).
    pub big: bool,
    pub dpl: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Image {
    Real {
        psp: u16,
        load_segment: u16,
        /// From the load segment to the end of the program's block.
        bytes: Vec<u8>,
        /// Where the CPU is now.
        cs: u16,
        ip: u32,
        ss: u16,
        sp: u32,
    },
    Protected {
        segments: Vec<Segment>,
        /// (linear base, bytes) of the memory the segments cover, overlaps merged; bytes on pages
        /// that aren't present read as zero.
        regions: Vec<(u32, Vec<u8>)>,
    },
}

impl Machine {
    /// The program's image as it is now (see [`crate::image`]).
    pub fn image(&self) -> Image {
        if !self.cpu.protected_mode() {
            let psp = self.dos.psp;
            let start = (u32::from(psp) + 0x10) << 4;
            let paragraphs = u32::from(self.mem.read_u16((u32::from(psp.wrapping_sub(1)) << 4) + 3));
            let end = (u32::from(psp) + paragraphs) << 4;
            let regs = &self.cpu.regs;
            return Image::Real {
                psp,
                load_segment: psp + 0x10,
                bytes: self.peek_bytes(start, end.saturating_sub(start) as usize),
                cs: regs.cs(),
                ip: regs.ip,
                ss: regs.ss(),
                sp: regs.gpr[4],
            };
        }
        let mut segments = Vec::new();
        let tables =
            [(self.cpu.gdtr.base, u32::from(self.cpu.gdtr.limit), 0u16), (self.cpu.ldtr.base, self.cpu.ldtr.limit, 4)];
        for (base, limit, table) in tables {
            if limit == 0 {
                continue;
            }
            for index in 1..(limit + 1) / 8 {
                let at = base.wrapping_add(index * 8);
                let bytes: Option<Vec<u8>> = (0..8).map(|i| self.peek_linear_u8(at + i)).collect();
                if let Some(segment) = bytes.and_then(|d| descriptor(&d, (index as u16) << 3 | table)) {
                    segments.push(segment);
                }
            }
        }
        let mut ranges: Vec<(u32, u32)> = segments
            .iter()
            .filter(|s| s.limit < MAX_REGION)
            .map(|s| (s.base, s.base.saturating_add(s.limit).saturating_add(1)))
            .collect();
        ranges.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::new();
        for (start, end) in ranges {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        let regions = merged
            .into_iter()
            .map(|(start, end)| (start, (start..end).map(|at| self.peek_linear_u8(at).unwrap_or(0)).collect()))
            .collect();
        Image::Protected { segments, regions }
    }
}

/// A present code or data descriptor.
fn descriptor(d: &[u8], selector: u16) -> Option<Segment> {
    let access = d[5];
    let (present, system, code) = (access & 0x80 != 0, access & 0x10 == 0, access & 0x08 != 0);
    if !present || system {
        return None;
    }
    let base = u32::from(d[2]) | u32::from(d[3]) << 8 | u32::from(d[4]) << 16 | u32::from(d[7]) << 24;
    let raw = u32::from(d[0]) | u32::from(d[1]) << 8 | u32::from(d[6] & 0x0F) << 16;
    let limit = if d[6] & 0x80 != 0 { raw << 12 | 0xFFF } else { raw };
    Some(Segment {
        selector: selector | u16::from(access >> 5 & 3),
        base,
        limit,
        code,
        big: d[6] & 0x40 != 0,
        dpl: access >> 5 & 3,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_decode() {
        // A 32-bit ring-3 code segment at 12345678h, 4 KB granular, limit FFFFFh pages.
        let code = [0xFF, 0xFF, 0x78, 0x56, 0x34, 0xFA, 0xCF, 0x12];
        assert_eq!(
            descriptor(&code, 0x0C),
            Some(Segment { selector: 0x0F, base: 0x1234_5678, limit: 0xFFFF_FFFF, code: true, big: true, dpl: 3 })
        );
        // A system descriptor (a TSS) and a non-present one aren't segments.
        assert_eq!(descriptor(&[0x67, 0, 0, 0, 0, 0x89, 0, 0], 8), None);
        assert_eq!(descriptor(&[0xFF, 0xFF, 0, 0, 0, 0x12, 0, 0], 8), None);
    }
}
