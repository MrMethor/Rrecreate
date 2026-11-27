//! The program's runs of chosen entries ([`crate::Machine::start_entries`]), with the registers it ran them with
//! (D98): the calls a part of the program a port doesn't have makes into code the port has (a renderer waking a
//! creature it drew). Interrupt handlers' runs aren't the program's and are left out, but those of the entries in
//! [`Entries::interrupts`] (D174: a driver's service a timer interrupt calls), which are kept in order with the rest.
//! An entry may keep memory too (D235): bytes its registers point at as it runs, a buffer the program hands a DOS
//! service or memory the port doesn't model.
//!
//! An analysis tool: not part of the machine's state, and it changes nothing the machine does (it reads memory
//! without side effects). Clockwork's source hash leaves this file out (D173).

/// The most bytes an entry's memory keeps at a run.
pub const MAX_ENTRY_MEMORY: u32 = 0x1_0000;

/// The most bytes of memory the runs kept since the last take hold in all: past it, runs with memory are dropped
/// (counted), so a hook that keeps a buffer at every pass can't run the host out of memory (D171).
pub const MAX_KEPT_MEMORY: usize = 64 << 20;

/// The program's runs of chosen entries.
#[derive(Debug, Clone, Default)]
pub struct Entries {
    /// The entries (linear), sorted.
    pub at: Vec<u32>,
    /// Each entry's [`entry_bit`]: an instruction whose bit isn't set is no entry, which is all a run asks of most.
    pub mask: u64,
    /// The entries whose runs in interrupt handlers count too (linear), sorted.
    pub interrupts: Vec<u32>,
    /// The memory an entry keeps at each run (linear entry, what to keep), sorted by entry (D235).
    pub memory: Vec<(u32, EntryMemory)>,
    pub hits: Vec<EntryHit>,
    /// Runs past [`crate::machine::MAX_EXEC_HITS`], or with memory past [`MAX_KEPT_MEMORY`].
    pub dropped: u64,
    /// The bytes of memory `hits` hold.
    pub kept: usize,
}

/// A run of an entry: its cycle, the entry (linear), the registers, and the memory its entry keeps (none if it
/// keeps none).
#[derive(Debug, Clone)]
pub struct EntryHit {
    pub cycle: u64,
    pub at: u32,
    pub regs: cpu::Regs,
    pub memory: Vec<u8>,
}

/// A register's value or a number, as a run's memory is found by (D235).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operand {
    /// A segment register (ES, CS, SS, DS, FS, GS: 0 to 5); as an offset or a length, its value.
    Segment(usize),
    /// A general register (EAX, ECX, EDX, EBX, ESP, EBP, ESI, EDI: 0 to 7), its low word or all of it.
    General {
        index: usize,
        wide: bool,
    },
    Number(u32),
}

impl Operand {
    /// `ds`, `si`, `ecx` or a number in hex (`FFFF`).
    pub fn parse(text: &str) -> Result<Operand, String> {
        const SEGMENTS: [&str; 6] = ["es", "cs", "ss", "ds", "fs", "gs"];
        const GENERAL: [&str; 8] = ["ax", "cx", "dx", "bx", "sp", "bp", "si", "di"];
        let lower = text.to_ascii_lowercase();
        if let Some(i) = SEGMENTS.iter().position(|s| *s == lower) {
            return Ok(Operand::Segment(i));
        }
        if let Some(i) = GENERAL.iter().position(|g| *g == lower) {
            return Ok(Operand::General { index: i, wide: false });
        }
        if let Some(i) = lower.strip_prefix('e').and_then(|rest| GENERAL.iter().position(|g| *g == rest)) {
            return Ok(Operand::General { index: i, wide: true });
        }
        let digits = lower.strip_suffix('h').unwrap_or(&lower);
        u32::from_str_radix(digits, 16)
            .map(Operand::Number)
            .map_err(|_| format!("{text}: neither a register (ds, si, ecx...) nor a number in hex"))
    }

    fn value(self, regs: &cpu::Regs) -> u32 {
        match self {
            Operand::Segment(i) => u32::from(regs.seg[i]),
            Operand::General { index, wide: true } => regs.gpr[index],
            Operand::General { index, wide: false } => regs.gpr[index] & 0xFFFF,
            Operand::Number(n) => n,
        }
    }
}

/// The memory an entry keeps at each run: `len` bytes at `segment`:`offset` (D235). A segment register is taken at
/// its base (real mode: its value × 16; protected: its descriptor's); a number as a real-mode segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryMemory {
    pub segment: Operand,
    pub offset: Operand,
    pub len: Operand,
}

impl EntryMemory {
    /// `SEGMENT:OFFSET:LENGTH`, each a register or a number in hex: `ds:si:cx`, `ds:0:FFFF`.
    pub fn parse(text: &str) -> Result<EntryMemory, String> {
        let parts: Vec<&str> = text.split(':').map(str::trim).collect();
        let [segment, offset, len] = parts.as_slice() else {
            return Err(format!("{text}: SEGMENT:OFFSET:LENGTH (ds:si:cx, ds:0:FFFF)"));
        };
        Ok(EntryMemory {
            segment: Operand::parse(segment)?,
            offset: Operand::parse(offset)?,
            len: Operand::parse(len)?,
        })
    }

    /// Where it is and how long, for a run with these registers (`base`, a segment register's base now).
    fn place(&self, regs: &cpu::Regs, base: impl Fn(usize) -> u32) -> (u32, u32) {
        let segment = match self.segment {
            Operand::Segment(i) => base(i),
            other => other.value(regs) << 4,
        };
        let len = self.len.value(regs).min(MAX_ENTRY_MEMORY);
        (segment.wrapping_add(self.offset.value(regs)), len)
    }
}

/// An address's bit in [`Entries::mask`], one of 64, its bits mixed so that aligned entries spread.
#[inline]
pub(crate) fn entry_bit(at: u32) -> u64 {
    1 << (at.wrapping_mul(0x9E37_79B1) >> 26)
}

/// The instruction at `linear` is about to run: kept if it is an entry's (M6.5's step 1, D187: the address first, the
/// question whether a handler runs only at an entry), but in an interrupt handler (call tracking, when it is on, tells
/// them apart) unless the entry is one of `interrupts` (D174); with the memory the entry keeps (D235).
pub(crate) fn watch_entries(m: &mut crate::Machine, linear: u32) {
    if m.probes.entries.as_deref().is_none_or(|e| e.at.binary_search(&linear).is_err()) {
        return;
    }
    let in_handler = m.probes.calls.as_deref().is_some_and(|c| c.in_hardware_interrupt());
    let Some(entries) = m.probes.entries.as_deref() else { return };
    if in_handler && entries.interrupts.binary_search(&linear).is_err() {
        return;
    }
    let regs = m.cpu.regs;
    let keeps = entries.memory.binary_search_by_key(&linear, |(at, _)| *at).ok().map(|i| entries.memory[i].1);
    let full = entries.hits.len() >= crate::machine::MAX_EXEC_HITS;
    let memory = match keeps {
        Some(keeps) if !full => {
            let (at, len) = keeps.place(&regs, |seg| m.cpu.segment_base(seg));
            if entries.kept + len as usize > MAX_KEPT_MEMORY {
                None
            } else {
                // Bytes on a page that isn't present read 0.
                Some(m.peek_linear(at, len as usize).into_iter().map(|b| b.unwrap_or(0)).collect())
            }
        }
        Some(_) => None,
        None => Some(Vec::new()),
    };
    let Some(entries) = m.probes.entries.as_deref_mut() else { return };
    let Some(memory) = memory.filter(|_| !full) else {
        entries.dropped += 1;
        return;
    };
    entries.kept += memory.len();
    entries.hits.push(EntryHit { cycle: m.cpu.cycles, at: linear, regs, memory });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operands_are_registers_or_hex() {
        assert_eq!(Operand::parse("ds"), Ok(Operand::Segment(3)));
        assert_eq!(Operand::parse("SI"), Ok(Operand::General { index: 6, wide: false }));
        assert_eq!(Operand::parse("ecx"), Ok(Operand::General { index: 1, wide: true }));
        assert_eq!(Operand::parse("FFFF"), Ok(Operand::Number(0xFFFF)));
        assert_eq!(Operand::parse("42h"), Ok(Operand::Number(0x42)));
        assert!(Operand::parse("xyz").is_err());
        assert!(EntryMemory::parse("ds:si").is_err());
    }

    #[test]
    fn memory_is_found_by_the_run_s_registers() {
        let mut regs = cpu::Regs::default();
        regs.seg[3] = 0x1234;
        regs.gpr[6] = 0xFFFF_0010; // ESI: SI is 10h
        regs.gpr[1] = 0x20; // CX
        let base = |seg: usize| u32::from(regs.seg[seg]) << 4;
        let keeps = EntryMemory::parse("ds:si:cx").unwrap();
        assert_eq!(keeps.place(&regs, base), (0x12350, 0x20));
        let whole = EntryMemory::parse("0:0:FFFFFF").unwrap();
        assert_eq!(whole.place(&regs, base), (0, MAX_ENTRY_MEMORY), "a length past the most kept is cut");
    }
}
