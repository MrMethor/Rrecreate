//! Ported from DOSBox Staging `src/dos/dos_tables.cpp` and `src/dos/dos_classes.cpp`
//! (`DOS_InfoBlock`, `DOS_SDA`) (GPL-2.0-or-later).
//!
//! DOS's internal tables, which some programs read directly instead of asking DOS: DOS
//! extenders, TSRs, memory and file-handle tools. They are:
//! - the "List of Lists" (SYSVARS, INT 21h AH=52h): the first memory block, a system file
//!   table chain, the NUL and CON device headers, the current directory structure and drive
//!   parameter blocks;
//! - the swappable data area (AX=5D06h) with the InDOS flag (AH=34h).
//!
//! As in DOSBox, the tables give a consistent picture to programs that look; the DOS emulation
//! itself keeps files, memory and processes host-side. The file table chain adds up to the real
//! size of the system file table, and the swappable data area and the current directory
//! structure follow the current PSP, DTA, drive and directories after every INT 21h call.
//!
//! The tables sit in the free paragraphs below the first memory block (600h-AFFh).

use crate::Machine;
use crate::dos::{C_DRIVE, FIRST_MCB};

/// SYSVARS starts here; programs get a pointer 26h bytes in (the List of Lists proper).
const SYSVARS: u16 = 0x0060;
const LIST_OF_LISTS: u16 = 0x0026;
/// Two system file table headers: DOS's built-in five files, then the rest.
const SFT: u16 = 0x0070;
const CON_DEVICE: u16 = 0x0072;
/// Current directory structure: one 58h-byte entry per drive, A: to the last drive.
const CDS: u16 = 0x0074;
const CDS_ENTRY: u32 = 0x58;
const FCB_SFT: u16 = 0x0090;
/// DOSBox's drive parameter block stubs: nine bytes per drive.
const DPB: u16 = 0x0091;
const DISK_BUFFER: u16 = 0x00A0;
/// The swappable data area. Programs that swap it (around DOS calls from interrupt handlers or
/// protected mode) copy 80h bytes while in DOS and 1Ah bytes always, as DOSBox reports.
pub const SDA: u16 = 0x00A2;
pub const SDA_IN_DOS_SIZE: u16 = 0x80;
pub const SDA_ALWAYS_SIZE: u16 = 0x1A;
/// The double-byte character set lead-byte table (INT 21h AH=63h): empty, as on a US DOS.
pub const DBCS_TABLE: (u16, u16) = (0x00B8, 0);

/// The List of Lists, as INT 21h AH=52h returns it in ES:BX.
pub fn list_of_lists() -> (u16, u16) {
    (SYSVARS, LIST_OF_LISTS)
}

/// The InDOS flag, as INT 21h AH=34h returns it in ES:BX. HLE services run in one step, so it
/// always reads 0.
pub fn in_dos_flag() -> (u16, u16) {
    (SDA, 1)
}

fn far(segment: u16, offset: u16) -> u32 {
    (u32::from(segment) << 16) | u32::from(offset)
}

/// Builds the tables (DOSBox `DOS_SetupTables` and `DOS_InfoBlock::SetLocation`).
pub(crate) fn install(m: &mut Machine, files: u16, last_drive: u8) {
    let base = Machine::lin(SYSVARS, 0);
    let lol = base + u32::from(LIST_OF_LISTS);
    // As DOSBox: FFh everywhere, then the fields below.
    m.mem.load(base, &[0xFF; 0x90]);
    m.mem.load(base, &[0; 14]);
    m.mem.load(Machine::lin(DBCS_TABLE.0, DBCS_TABLE.1), &[0; 4]);
    let mem = &mut m.mem;
    mem.write_u16(lol - 0x22, 0x0001); // magic word: DOS 5+
    for at in [0x18, 0x16, 0x14, 0x0C, 0x0A, 0x04] {
        mem.write_u16(lol - at, 0); // CX of the last 5Eh call, LRU counters, sharing retries, CON input
    }
    mem.write_u16(lol - 0x02, FIRST_MCB);
    mem.write_u32(lol, far(DPB, 0));
    mem.write_u32(lol + 0x04, far(SFT, 0));
    mem.write_u32(lol + 0x0C, far(CON_DEVICE, 0)); // active console
    mem.write_u16(lol + 0x10, 0x200); // largest sector
    mem.write_u32(lol + 0x12, far(SYSVARS, LIST_OF_LISTS + 0x47)); // disk info buffer
    mem.write_u32(lol + 0x16, far(CDS, 0));
    mem.write_u32(lol + 0x1A, far(FCB_SFT, 0));
    mem.write_u16(lol + 0x1E, 0); // protected FCBs
    mem.write_u8(lol + 0x20, 0); // block devices
    mem.write_u8(lol + 0x21, last_drive);
    // The NUL device header, first in the device chain, followed by CON.
    mem.write_u32(lol + 0x22, far(CON_DEVICE, 0));
    mem.write_u16(lol + 0x26, 0x8004);
    mem.write_u32(lol + 0x28, 0);
    mem.load(lol + 0x2C, b"NUL     ");
    mem.write_u8(lol + 0x34, 0); // joined drives
    mem.write_u16(lol + 0x35, 0); // special code segment
    mem.write_u32(lol + 0x37, 0); // SETVER
    mem.write_u16(lol + 0x3B, 0); // A20 fix
    mem.write_u16(lol + 0x3D, 0); // last PSP if DOS is in the HMA
    mem.write_u16(lol + 0x3F, 50); // BUFFERS=50,50
    mem.write_u16(lol + 0x41, 50);
    mem.write_u8(lol + 0x43, C_DRIVE + 1); // boot drive
    mem.write_u8(lol + 0x44, 1); // 386: dword moves
    let extended_kb = m.config.extended_kb.min(0xFFFF) as u16;
    let mem = &mut m.mem;
    mem.write_u16(lol + 0x45, extended_kb);
    mem.write_u32(lol + 0x47, far(DISK_BUFFER, 0));
    mem.write_u16(lol + 0x4B, 0); // dirty buffers
    mem.write_u32(lol + 0x4D, 0); // lookahead buffers
    mem.write_u16(lol + 0x51, 0);
    mem.write_u8(lol + 0x53, 0); // buffers in conventional memory
    mem.write_u32(lol + 0x54, 0);
    mem.write_u8(lol + 0x63, 0); // UMBs not linked
    mem.write_u16(lol + 0x64, 0);
    mem.write_u16(lol + 0x66, 0xFFFF); // no UMB chain
    mem.write_u16(lol + 0x68, FIRST_MCB + 1); // allocation scans from here

    // System file tables: five built-in entries, then the rest of FILES=. DOSBox only writes
    // the headers, and so do we: the entries themselves stay host-side.
    let sft = Machine::lin(SFT, 0);
    mem.write_u32(sft, far(SFT + 1, 0));
    mem.write_u16(sft + 4, 5);
    mem.write_u32(sft + 0x10, 0xFFFF_FFFF);
    mem.write_u16(sft + 0x14, files.saturating_sub(5));

    // CON's device header: end of the chain, character device, standard input/output.
    let con = Machine::lin(CON_DEVICE, 0);
    mem.write_u32(con, 0xFFFF_FFFF);
    mem.write_u16(con + 4, 0x8013);
    mem.write_u32(con + 6, 0xFFFF_FFFF);
    mem.load(con + 10, b"CON     ");

    // FCB file table (DOSBox's 100 FCBs, none open).
    let fcbs = Machine::lin(FCB_SFT, 0);
    mem.write_u32(fcbs, 0xFFFF_FFFF);
    mem.write_u16(fcbs + 4, 100);

    // DOSBox's drive parameter block stubs: drive, unit, bytes per sector, media ID.
    for drive in 0..26u8 {
        let dpb = Machine::lin(DPB, 0) + u32::from(drive) * 9;
        mem.write_u8(dpb, drive);
        mem.write_u8(dpb + 1, drive);
        mem.write_u16(dpb + 2, 0x200);
    }

    // An unused disk buffer header.
    let buffer = Machine::lin(DISK_BUFFER, 0);
    mem.load(buffer, &[0; 0x20]);
    mem.write_u16(buffer, 0xFFFF);
    mem.write_u16(buffer + 2, 0xFFFF);
    mem.write_u8(buffer + 4, 0xFF);
    mem.write_u8(buffer + 0x0A, 1);
    mem.write_u32(buffer + 0x0D, 0xFFFF_FFFF);

    // The swappable data area: no critical error on any drive.
    let sda = Machine::lin(SDA, 0);
    mem.load(sda, &[0; SDA_IN_DOS_SIZE as usize]);
    mem.write_u8(sda + 2, 0xFF);

    for drive in 0..last_drive {
        let entry = Machine::lin(CDS, 0) + u32::from(drive) * CDS_ENTRY;
        mem.load(entry, &[0; CDS_ENTRY as usize]);
    }
    sync(m);
}

/// Brings the swappable data area and the current directory structure up to date with the
/// DOS state. Runs after every INT 21h call.
pub(crate) fn sync(m: &mut Machine) {
    let sda = Machine::lin(SDA, 0);
    let (dta_segment, dta_offset) = m.dos.dta;
    let (error, psp, return_code, drive) = (m.dos.last_error, m.dos.psp, m.dos.return_code, m.dos.drive);
    m.mem.write_u16(sda + 0x04, error);
    m.mem.write_u32(sda + 0x0C, far(dta_segment, dta_offset));
    m.mem.write_u16(sda + 0x10, psp);
    m.mem.write_u16(sda + 0x14, return_code);
    m.mem.write_u8(sda + 0x16, drive);

    let last_drive = m.mem.read_u8(Machine::lin(SYSVARS, LIST_OF_LISTS) + 0x21);
    for drive in 0..last_drive {
        let entry = Machine::lin(CDS, 0) + u32::from(drive) * CDS_ENTRY;
        let flags: u16 = if Some(drive) == m.dos.cd_drive {
            0xC000 // network (MSCDEX) and physical
        } else if m.dos.drive_exists(drive) {
            0x4000 // physical
        } else {
            0
        };
        let mut path = format!("{}:\\{}", char::from(b'A' + drive), m.dos.cwd[usize::from(drive)]).into_bytes();
        path.resize(0x43, 0);
        m.mem.load(entry, &path);
        m.mem.write_u16(entry + 0x43, flags);
        m.mem.write_u32(entry + 0x45, far(DPB, u16::from(drive) * 9));
        m.mem.write_u16(entry + 0x4F, 2); // the root's backslash
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tables_fit_below_the_xms_entry_and_first_mcb() {
        // The CDS covers A: to E: (5 × 58h bytes) and the SDA ends before the XMS entry stub.
        assert!(u32::from(super::CDS) * 16 + 5 * super::CDS_ENTRY <= u32::from(super::FCB_SFT) * 16);
        assert!(
            u32::from(super::SDA) * 16 + u32::from(super::SDA_IN_DOS_SIZE) <= u32::from(crate::xms::ENTRY_SEGMENT) * 16
        );
    }
}
