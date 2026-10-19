//! Coverage of a program's code during a run, for the analysis pipeline (PROJECT.md §5.3, step 6,
//! "Dynamic augmentation"): which instructions of its load image ran, and which calls were made
//! from it, with their targets and counts. Code static analysis missed (functions reached only
//! through pointers, overlays) shows up in the call targets.
//!
//! Recorded in real and virtual-8086 mode only (DOS extender code runs elsewhere), and only for
//! the image of the program the machine started with. A debugging aid: not part of the machine's
//! state or hash.

use std::collections::BTreeMap;

/// What one run executed in a load image.
#[derive(Debug, Clone, Default)]
pub struct Coverage {
    /// Linear address and size of the load image.
    pub base: u32,
    pub size: u32,
    /// A bit per image byte where an instruction started.
    executed: Vec<u64>,
    /// (call site, target linear address) → times the call was made.
    pub calls: BTreeMap<(u32, u32), u64>,
    /// A call is executing: its site, to be completed with the target.
    pending_call: Option<u32>,
}

impl Coverage {
    pub fn new(base: u32, size: u32) -> Self {
        Coverage { base, size, executed: vec![0; (size as usize).div_ceil(64)], ..Coverage::default() }
    }

    /// Image offset of `linear`, if it is in the image.
    fn offset(&self, linear: u32) -> Option<u32> {
        linear.checked_sub(self.base).filter(|&offset| offset < self.size)
    }

    /// Before an instruction at `linear` runs; `opcode` are its first bytes. A call's target comes
    /// with `after`.
    pub fn before(&mut self, linear: u32, opcode: [u8; 4]) {
        self.pending_call = None;
        let Some(offset) = self.offset(linear) else { return };
        self.executed[offset as usize / 64] |= 1 << (offset % 64);
        if is_call(opcode) {
            self.pending_call = Some(offset);
        }
    }

    /// After the instruction ran: if it was a call, CS:IP is its target.
    pub fn after(&mut self, target: u32) {
        if let Some(site) = self.pending_call.take() {
            *self.calls.entry((site, target)).or_insert(0) += 1;
        }
    }

    /// The instruction didn't complete (a fault or a host callback).
    pub fn cancel(&mut self) {
        self.pending_call = None;
    }

    /// Offsets in the image where instructions started, ascending.
    pub fn executed(&self) -> impl Iterator<Item = u32> + '_ {
        self.executed.iter().enumerate().flat_map(|(i, &word)| {
            (0..64).filter(move |bit| word >> bit & 1 != 0).map(move |bit| (i * 64 + bit) as u32)
        })
    }

    /// JSON: the image, executed instruction offsets, and calls (site offset, target offset or
    /// -1 when outside the image, count).
    pub fn to_json(&self, module: &str) -> String {
        let executed: Vec<String> = self.executed().map(|o| o.to_string()).collect();
        let calls: Vec<String> = self
            .calls
            .iter()
            .map(|(&(site, target), count)| {
                let target = self.offset(target).map_or(-1, i64::from);
                format!("[{site},{target},{count}]")
            })
            .collect();
        format!(
            "{{\"module\":\"{}\",\"base\":{},\"size\":{},\"executed\":[{}],\"calls\":[{}]}}\n",
            module.replace('\\', "\\\\").replace('"', "\\\""),
            self.base,
            self.size,
            executed.join(","),
            calls.join(",")
        )
    }
}

/// Whether the instruction starting with these bytes is a CALL (near or far, direct or not).
fn is_call(bytes: [u8; 4]) -> bool {
    let mut i = 0;
    // Prefixes: operand and address size, segment overrides, LOCK, REP.
    while i < 3 && matches!(bytes[i], 0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 | 0x66 | 0x67 | 0xF0 | 0xF2 | 0xF3) {
        i += 1;
    }
    match bytes[i] {
        0xE8 | 0x9A => true,
        0xFF => i + 1 < 4 && matches!((bytes[i + 1] >> 3) & 7, 2 | 3),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_are_recognized() {
        assert!(is_call([0xE8, 0, 0, 0]));
        assert!(is_call([0x9A, 0, 0, 0]));
        assert!(is_call([0xFF, 0x16, 0, 0])); // call [disp16]
        assert!(is_call([0xFF, 0x1E, 0, 0])); // call far [disp16]
        assert!(is_call([0x2E, 0xFF, 0x57, 4])); // call cs:[bx+4]
        assert!(!is_call([0xFF, 0x26, 0, 0])); // jmp [disp16]
        assert!(!is_call([0xE9, 0, 0, 0]));
    }

    #[test]
    fn records_executed_offsets_and_calls() {
        let mut c = Coverage::new(0x1000, 0x100);
        c.before(0x1010, [0xE8, 0, 0, 0]);
        c.after(0x1080);
        c.before(0x1010, [0xE8, 0, 0, 0]);
        c.after(0x1080);
        c.before(0x1013, [0x90, 0, 0, 0]);
        c.after(0x1014);
        c.before(0x2000, [0xE8, 0, 0, 0]); // outside the image
        c.after(0x1000);
        c.before(0x1020, [0x9A, 0, 0, 0]);
        c.after(0xF0000); // into the BIOS
        assert_eq!(c.executed().collect::<Vec<_>>(), [0x10, 0x13, 0x20]);
        assert_eq!(c.calls.len(), 2);
        let json = c.to_json("GAME.EXE");
        assert!(json.contains("\"executed\":[16,19,32]"), "{json}");
        assert!(json.contains("[16,128,2]") && json.contains("[32,-1,1]"), "{json}");
    }
}
