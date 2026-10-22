//! Coverage of the code a run executes, for analysis: where each instruction that ran started, and
//! which calls were made, from where to where and how often. Code static analysis missed
//! (functions reached only through pointers, overlays) shows up in the call targets.
//!
//! Recorded in every mode and wherever code runs: the program's load image, a DOS extender and the
//! objects it loads, the BIOS's stubs. Addresses are linear (protected-mode code by its segment's
//! base, whether paging is on or not); the JSON gives the started program's image too, where its
//! offsets count from. A debugging aid: not part of the machine's state or hash.

use std::collections::BTreeMap;
use std::ops::Range;

/// Bytes a page of the record covers.
const PAGE: u32 = 4096;

/// A bit per byte of a page: where an instruction started.
type Page = [u64; PAGE as usize / 64];

/// What one run executed.
#[derive(Debug, Clone, Default)]
pub struct Coverage {
    /// The started program's load image (linear), as the JSON gives it.
    pub image: Range<u32>,
    /// The pages instructions ran in, by number (linear address / [`PAGE`]).
    pages: BTreeMap<u32, Box<Page>>,
    /// (call site, target), linear → times the call was made.
    pub calls: BTreeMap<(u32, u32), u64>,
    /// A call is executing: its site, to be completed with the target.
    pending_call: Option<u32>,
}

impl Coverage {
    pub fn new(image: Range<u32>) -> Self {
        Coverage { image, ..Coverage::default() }
    }

    /// Before an instruction at `linear` runs; `opcode` are its first bytes. A call's target comes
    /// with `after`.
    pub fn before(&mut self, linear: u32, opcode: [u8; 4]) {
        let page = self.pages.entry(linear / PAGE).or_insert_with(|| Box::new([0; PAGE as usize / 64]));
        let offset = linear % PAGE;
        page[offset as usize / 64] |= 1 << (offset % 64);
        self.pending_call = is_call(opcode).then_some(linear);
    }

    /// After the instruction ran: if it was a call, `target` (CS's base and IP, linear) is where it
    /// went.
    pub fn after(&mut self, target: u32) {
        if let Some(site) = self.pending_call.take() {
            *self.calls.entry((site, target)).or_insert(0) += 1;
        }
    }

    /// The instruction didn't complete (a fault or a host callback).
    pub fn cancel(&mut self) {
        self.pending_call = None;
    }

    /// Linear addresses where instructions started, ascending.
    pub fn executed(&self) -> impl Iterator<Item = u32> + '_ {
        self.pages.iter().flat_map(|(&page, bits)| {
            bits.iter().enumerate().flat_map(move |(i, &word)| {
                (0..64).filter(move |bit| word >> bit & 1 != 0).map(move |bit| page * PAGE + (i * 64 + bit) as u32)
            })
        })
    }

    /// JSON: the module and its image (`[base, size]`, linear), the executed instructions' linear
    /// addresses, and the calls (`[site, target, count]`, linear).
    pub fn to_json(&self, module: &str) -> String {
        let executed: Vec<String> = self.executed().map(|at| at.to_string()).collect();
        let calls: Vec<String> =
            self.calls.iter().map(|(&(site, target), count)| format!("[{site},{target},{count}]")).collect();
        format!(
            "{{\"module\":\"{}\",\"image\":[{},{}],\"executed\":[{}],\"calls\":[{}]}}\n",
            module.replace('\\', "\\\\").replace('"', "\\\""),
            self.image.start,
            self.image.len(),
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
    fn records_executed_instructions_and_calls_anywhere() {
        let mut c = Coverage::new(0x1000..0x1100);
        c.before(0x1010, [0xE8, 0, 0, 0]);
        c.after(0x1080);
        c.before(0x1010, [0xE8, 0, 0, 0]);
        c.after(0x1080);
        c.before(0x1013, [0x90, 0, 0, 0]);
        c.after(0x1014);
        c.before(0x43_5024, [0xE8, 0, 0, 0]); // flat code, out of the image
        c.after(0x43_5010);
        c.before(0x1020, [0x9A, 0, 0, 0]);
        c.after(0xF0000); // into the BIOS
        assert_eq!(c.executed().collect::<Vec<_>>(), [0x1010, 0x1013, 0x1020, 0x43_5024]);
        assert_eq!(c.calls.len(), 3);
        let json = c.to_json("GAME.EXE");
        assert!(json.contains("\"image\":[4096,256]"), "{json}");
        assert!(json.contains("\"executed\":[4112,4115,4128,4411428]"), "{json}");
        assert!(json.contains("[4112,4224,2]") && json.contains("[4411428,4411408,1]"), "{json}");
    }
}
