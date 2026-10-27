//! DOS services (INT 20h/21h/29h) as high-level emulation: memory blocks, files on the virtual
//! drive, console I/O, program loading.
//!
//! Memory control blocks live in emulated memory like real DOS keeps them, so programs that
//! walk the MCB chain see a consistent picture. File handles and search state are host-side.

use cpu::flags::{CF, ZF};

use crate::bios::{self, callback};
use crate::machine::Machine;
use crate::mz;
use crate::time::DateTime;
use crate::vfs::{self, Vfs};

/// Segment of the first memory control block; conventional memory ends at A000h.
pub const FIRST_MCB: u16 = 0x0100;
pub const MEMORY_END: u16 = 0xA000;
/// Entries in a process's job file table (the default, inside the PSP).
const JFT_SIZE: u16 = 20;
/// Entries in the system file table (FILES=).
const SYSTEM_FILES: usize = 100;
const MAX_MCBS: usize = 4096;
const DEVICES: [&str; 11] = ["CON", "NUL", "PRN", "AUX", "CLOCK$", "LPT1", "LPT2", "LPT3", "COM1", "COM2", "EMMXXXX0"];
/// Drive number of C: (0 = A:).
pub const C_DRIVE: u8 = 2;
/// Drive letters DOS reports as present (LASTDRIVE=E).
const LAST_DRIVE: u8 = 5;
/// Every file reports 1994-01-01 00:00 so runs don't depend on host timestamps.
const FILE_DATE: u16 = ((1994 - 1980) << 9) | (1 << 5) | 1;
const FILE_TIME: u16 = 0;

pub mod error {
    pub const INVALID_FUNCTION: u16 = 0x01;
    pub const FILE_NOT_FOUND: u16 = 0x02;
    pub const PATH_NOT_FOUND: u16 = 0x03;
    pub const TOO_MANY_FILES: u16 = 0x04;
    pub const ACCESS_DENIED: u16 = 0x05;
    pub const INVALID_HANDLE: u16 = 0x06;
    pub const MCB_DESTROYED: u16 = 0x07;
    pub const INSUFFICIENT_MEMORY: u16 = 0x08;
    pub const INVALID_BLOCK: u16 = 0x09;
    pub const INVALID_FORMAT: u16 = 0x0B;
    pub const INVALID_DATA: u16 = 0x0D;
    pub const INVALID_DRIVE: u16 = 0x0F;
    pub const NO_MORE_FILES: u16 = 0x12;
    pub const FILE_EXISTS: u16 = 0x50;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Handle {
    Console,
    Null,
    File { path: String, pos: u32, write: bool, drive: u8 },
}

/// An entry in the system file table.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct OpenFile {
    pub handle: Handle,
    /// Job file table entries (in any process) that refer to this entry.
    pub refs: u16,
    /// Child processes get a copy of handles to it (open mode bit 7 clear).
    pub inherit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DosState {
    pub psp: u16,
    pub dta: (u16, u16),
    /// Current drive (0 = A:).
    pub drive: u8,
    /// Drive number of the emulated CD-ROM: read-only, announced by MSCDEX.
    pub cd_drive: Option<u8>,
    /// The folder of C: the CD drive shows (canonical; empty: all of C:).
    pub cd_root: String,
    /// Current directory per drive number, canonical (no drive, no leading backslash).
    pub cwd: Vec<String>,
    /// The system file table: open files, shared by the handles that refer to them through each
    /// process's job file table (in its PSP).
    pub files: Vec<Option<OpenFile>>,
    /// Exit code and termination type of the last child process (INT 21h AH=4Dh).
    pub return_code: u16,
    /// DOS path of each running process's program, by PSP.
    pub process_paths: std::collections::BTreeMap<u16, String>,
    pub last_error: u16,
    pub break_check: bool,
    pub allocation_strategy: u16,
    /// DOS path of the running program.
    pub program: String,
    /// Directories referenced by FindFirst/FindNext state stored in DTAs.
    search_dirs: Vec<String>,
    /// Running COMMAND.COMs ([`crate::shell`]).
    pub shells: Vec<crate::shell::Shell>,
    /// Buffered keyboard input (AH=0Ah) waiting for keys: the characters taken so far.
    pub line_input: Option<u8>,
    /// The CD drive's audio playback (MSCDEX).
    pub cd_audio: crate::cdrom::CdAudio,
    /// The DPMI host's state.
    pub dpmi: crate::dpmi::Dpmi,
    /// Seconds the DOS clock was set ahead of (or behind) the virtual clock (AH=2Bh, 2Dh).
    pub clock_offset: i64,
}

impl DosState {
    pub fn drive_exists(&self, drive: u8) -> bool {
        drive == C_DRIVE || Some(drive) == self.cd_drive
    }
}

impl Default for DosState {
    fn default() -> Self {
        // AUX, CON and PRN; the first process's handles 0-4 refer to CON, CON, CON, AUX, PRN.
        let mut files = vec![None; SYSTEM_FILES];
        files[0] = Some(OpenFile { handle: Handle::Null, refs: 1, inherit: true });
        files[1] = Some(OpenFile { handle: Handle::Console, refs: 3, inherit: true });
        files[2] = Some(OpenFile { handle: Handle::Null, refs: 1, inherit: true });
        Self {
            psp: 0,
            dta: (0, 0x80),
            drive: C_DRIVE,
            cd_drive: None,
            cd_root: String::new(),
            cwd: vec![String::new(); 26],
            files,
            return_code: 0,
            process_paths: std::collections::BTreeMap::new(),
            last_error: 0,
            break_check: false,
            allocation_strategy: 0,
            program: String::new(),
            search_dirs: Vec::new(),
            shells: Vec::new(),
            line_input: None,
            cd_audio: crate::cdrom::CdAudio::default(),
            dpmi: crate::dpmi::Dpmi::default(),
            clock_offset: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LoadError {
    NotFound(String),
    BadFormat(String),
    Unsupported(String),
    InsufficientMemory { needed: u32, available: u32 },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::NotFound(p) => write!(f, "{p}: not found on the emulated drives"),
            LoadError::BadFormat(why) => write!(f, "bad executable: {why}"),
            LoadError::Unsupported(why) => write!(f, "unsupported executable: {why}"),
            LoadError::InsufficientMemory { needed, available } => {
                write!(f, "needs {needed} paragraphs of memory, {available} available")
            }
        }
    }
}

impl std::error::Error for LoadError {}

pub(crate) fn install(m: &mut Machine) {
    write_mcb(m, FIRST_MCB, b'Z', 0, MEMORY_END - FIRST_MCB - 1);
    let number = |letter: char| (letter.to_ascii_uppercase() as u8).wrapping_sub(b'A');
    m.dos.cd_drive = m.config.cd_drive.map(number).filter(|&d| d < 26 && d != C_DRIVE);
    let root = m.config.cd_root.to_ascii_uppercase().replace('/', "\\");
    m.dos.cd_root = root.split('\\').filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\\");
    let start = number(m.config.start_drive);
    m.dos.drive = if m.dos.drive_exists(start) { start } else { C_DRIVE };
    crate::tables::install(m, SYSTEM_FILES as u16, LAST_DRIVE);
}

/// The drive a guest path refers to: its `X:` prefix, or the current drive.
fn path_drive(m: &Machine, raw: &[u8]) -> u8 {
    match raw {
        [letter, b':', ..] if letter.is_ascii_alphabetic() => letter.to_ascii_uppercase() - b'A',
        _ => m.dos.drive,
    }
}

fn on_cd(m: &Machine, raw: &[u8]) -> bool {
    Some(path_drive(m, raw)) == m.dos.cd_drive
}

/// A drive's volume label as DOS shows it (DOSBox `Set_Label`): C: is `C_DRIVE` as in DOSBox;
/// the CD's label keeps its case, as MSCDEX does (Daggerfall checks for `Daggerfa.ll`).
fn volume_label(m: &Machine, drive: u8) -> String {
    let cd = Some(drive) == m.dos.cd_drive;
    let name = match (cd, m.config.cd_label.as_str()) {
        (false, _) => format!("{}_DRIVE", char::from(b'A' + drive)),
        (true, "") if m.dos.cd_root.is_empty() => "CDROM".to_string(),
        (true, "") => vfs::name_of(&m.dos.cd_root).to_string(),
        (true, label) => label.to_string(),
    };
    // Eight characters, a dot, three more; a dot in the first eight starts the extension early.
    let mut label = String::new();
    let mut chars = name.chars().peekable();
    let (mut left, mut dotted) = (8, false);
    while left > 0 {
        let Some(c) = chars.next() else { break };
        if !dotted && c == '.' {
            (left, dotted) = (4, true);
        }
        label.push(if cd { c } else { c.to_ascii_uppercase() });
        left -= 1;
        if left == 0 && !dotted {
            chars.next_if_eq(&'.');
            label.push('.');
            (left, dotted) = (3, true);
        }
    }
    // A trailing dot goes, except after exactly eight characters on a CD (an MSCDEX quirk that
    // FIFA 96's CD check relies on).
    if label.ends_with('.') && !(cd && label.len() == 9) {
        label.pop();
    }
    label
}

/// A resolved path as the program sees it on `drive`: without the CD drive's root folder.
fn guest_path<'a>(m: &Machine, drive: u8, path: &'a str) -> &'a str {
    if Some(drive) != m.dos.cd_drive || m.dos.cd_root.is_empty() {
        return path;
    }
    let rest = &path[m.dos.cd_root.len().min(path.len())..];
    rest.strip_prefix('\\').unwrap_or(rest)
}

/// A drive number from a DL-style argument (0 = current, 1 = A:, ...).
fn drive_argument(m: &Machine, dl: u8) -> u8 {
    if dl == 0 { m.dos.drive } else { dl - 1 }
}

/// The emulated wall clock: the configured start time plus elapsed virtual time.
pub(crate) fn wall_clock(m: &Machine) -> DateTime {
    let elapsed = (m.cpu.cycles / m.config.instructions_per_second) as i64;
    DateTime::from_epoch_seconds(m.config.start_time.to_epoch_seconds() + elapsed + m.dos.clock_offset)
}

/// AH=2Bh and 2Dh: the DOS clock set to `to` from now on (the virtual clock keeps running under
/// it); setting the time also sets the BIOS tick count, as DOS does through INT 1Ah.
fn set_clock(m: &mut Machine, to: DateTime) {
    let now = wall_clock(m);
    m.dos.clock_offset += to.to_epoch_seconds() - now.to_epoch_seconds();
    if to.seconds_since_midnight() != now.seconds_since_midnight() {
        let ticks = u64::from(to.seconds_since_midnight()) * crate::clock::PIT_HZ / 65536;
        m.mem.write_u32(bios::BDA_TICKS, ticks as u32);
        m.mem.write_u8(bios::BDA_TICKS + 4, 0);
    }
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The country information of AH=38h (USA, as DOSBox has it by default): date format,
/// currency, separators, time format, the case map routine and the list separator.
fn country_info() -> [u8; 0x18] {
    let mut info = [0u8; 0x18];
    info[0x02] = b'$';
    info[0x07] = b',';
    info[0x09] = b'.';
    info[0x0B] = b'-';
    info[0x0D] = b':';
    info[0x10] = 2;
    // The case map routine: a RETF in the ROM (no characters above 7Fh change).
    info[0x12..0x14].copy_from_slice(&crate::dpmi::RM_RETF.to_le_bytes());
    info[0x14..0x16].copy_from_slice(&bios::ROM_SEGMENT.to_le_bytes());
    info[0x16] = b',';
    info
}

pub(crate) fn dispatch(m: &mut Machine, id: u16) -> bool {
    match id {
        callback::TERMINATE => terminate_process(m, 0, false),
        callback::DOS => {
            int21(m);
            crate::tables::sync(m);
        }
        callback::FAST_CONSOLE => bios::teletype(m, m.cpu.regs.al()),
        callback::MULTIPLEX => multiplex(m),
        callback::EMS => crate::ems::service(m),
        callback::SHELL => crate::shell::step(m),
        callback::XMS => crate::xms::service(m),
        other => return crate::dpmi::dispatch(m, other),
    }
    true
}

/// Calls the service behind callback `id` (INT 21h: [`callback::DOS`]) from host code, the shell's
/// way to use DOS: `set` sets the registers, and an interrupt frame on the stack takes the flags
/// the service returns. Returns the registers afterwards and whether carry was set; the caller's
/// registers come back as they were.
pub(crate) fn call_service(m: &mut Machine, id: u16, set: impl FnOnce(&mut cpu::Regs)) -> (cpu::Regs, bool) {
    let saved = m.cpu.regs;
    set(&mut m.cpu.regs);
    m.cpu.regs.flags &= !u32::from(CF);
    let sp = m.cpu.regs.sp().wrapping_sub(6);
    m.cpu.regs.set_sp(sp);
    let frame = Machine::lin(m.cpu.regs.ss(), sp);
    m.mem.write_u16(frame, m.cpu.regs.ip as u16);
    m.mem.write_u16(frame + 2, m.cpu.regs.cs());
    m.mem.write_u16(frame + 4, m.cpu.regs.flags as u16);
    m.returned_flags = false;
    if !bios::dispatch(m, id) {
        dispatch(m, id);
    }
    let carry =
        if m.returned_flags { m.mem.read_u16(frame + 4) & CF != 0 } else { m.cpu.regs.flags & u32::from(CF) != 0 };
    let after = m.cpu.regs;
    m.cpu.regs = saved;
    (after, carry)
}

fn ok(m: &mut Machine) {
    m.set_return_flag(CF, false);
}

/// INT 21h AH=0Ah: a line from the keyboard into the buffer at DS:DX (its size, then the count
/// and the characters, ended by CR), echoed, with backspace. It waits for keys, keeping the
/// count so far in [`DosState::line_input`].
fn buffered_input(m: &mut Machine) {
    let r = m.cpu.regs;
    let at = Machine::lin(r.ds(), r.dx());
    let size = m.mem.read_u8(at);
    if size == 0 {
        return;
    }
    let mut count = m.dos.line_input.unwrap_or(0);
    while let Some(key) = bios::pop_key(m) {
        match key as u8 {
            0x0D => {
                m.mem.write_u8(at + 2 + u32::from(count), 0x0D);
                m.mem.write_u8(at + 1, count);
                bios::teletype(m, 0x0D);
                m.dos.line_input = None;
                return;
            }
            0x08 => {
                if count > 0 {
                    count -= 1;
                    for c in [0x08, b' ', 0x08] {
                        bios::teletype(m, c);
                    }
                }
            }
            0 | 0xE0 => {} // an extended key
            c if count + 1 < size => {
                m.mem.write_u8(at + 2 + u32::from(count), c);
                count += 1;
                bios::teletype(m, c);
            }
            _ => bios::teletype(m, 0x07),
        }
    }
    m.dos.line_input = Some(count);
    m.wait_for_input();
}

fn fail(m: &mut Machine, code: u16) {
    m.dos.last_error = code;
    m.cpu.regs.set_ax(code);
    m.set_return_flag(CF, true);
}

fn read_asciiz(m: &Machine, segment: u16, offset: u16) -> Vec<u8> {
    (0..128u16).map(|i| m.mem.read_u8(Machine::lin(segment, offset.wrapping_add(i)))).take_while(|&b| b != 0).collect()
}

fn write_bytes(m: &mut Machine, segment: u16, offset: u16, data: &[u8]) {
    let base = Machine::lin(segment, offset);
    for (i, &b) in data.iter().enumerate() {
        m.mem.write_u8(base + i as u32, b);
    }
}

fn int21(m: &mut Machine) {
    let r = m.cpu.regs;
    match r.ah() {
        0x00 => terminate_process(m, 0, false),
        0x01 | 0x07 | 0x08 => match bios::pop_key(m) {
            Some(key) => {
                m.cpu.regs.set_al(key as u8);
                if r.ah() == 0x01 {
                    bios::teletype(m, key as u8);
                }
            }
            None => m.wait_for_input(),
        },
        0x02 => bios::teletype(m, r.dl()),
        0x06 if r.dl() == 0xFF => match bios::pop_key(m) {
            Some(key) => {
                m.cpu.regs.set_al(key as u8);
                m.set_return_flag(ZF, false);
            }
            None => {
                m.cpu.regs.set_al(0);
                m.set_return_flag(ZF, true);
            }
        },
        0x06 => bios::teletype(m, r.dl()),
        0x09 => {
            for i in 0..0xFFFFu16 {
                let ch = m.mem.read_u8(Machine::lin(r.ds(), r.dx().wrapping_add(i)));
                if ch == b'$' {
                    break;
                }
                bios::teletype(m, ch);
            }
            m.cpu.regs.set_al(b'$');
        }
        0x0A => buffered_input(m),
        0x0B => {
            let ready = bios::peek_key(m).is_some();
            m.cpu.regs.set_al(if ready { 0xFF } else { 0 });
        }
        0x0C => {
            while bios::pop_key(m).is_some() {}
            if matches!(r.al(), 0x01 | 0x06 | 0x07 | 0x08) {
                m.cpu.regs.set_ah(r.al());
                int21(m);
            }
        }
        0x0E => {
            if m.dos.drive_exists(r.dl()) {
                m.dos.drive = r.dl();
            }
            m.cpu.regs.set_al(LAST_DRIVE);
        }
        0x0F..=0x17 | 0x21..=0x24 | 0x27 | 0x28 => fcb_function(m),
        0x19 => m.cpu.regs.set_al(m.dos.drive),
        0x1A => m.dos.dta = (r.ds(), r.dx()),
        0x25 => {
            let at = u32::from(r.al()) * 4;
            m.mem.write_u16(at, r.dx());
            m.mem.write_u16(at + 2, r.ds());
        }
        0x2A => {
            let now = wall_clock(m);
            m.cpu.regs.set_cx(now.year);
            m.cpu.regs.set_dh(now.month);
            m.cpu.regs.set_dl(now.day);
            m.cpu.regs.set_al(now.weekday());
        }
        0x2B => {
            let (year, month, day) = (r.cx(), r.dh(), r.dl());
            let valid = (1980..=2099).contains(&year)
                && (1..=12).contains(&month)
                && (1..=days_in_month(year, month)).contains(&day);
            if valid {
                let now = wall_clock(m);
                set_clock(m, DateTime::new(year, month, day, now.hour, now.minute, now.second));
            }
            m.cpu.regs.set_al(if valid { 0 } else { 0xFF });
        }
        0x2D => {
            let (hour, minute, second, hundredths) = (r.ch(), r.cl(), r.dh(), r.dl());
            let valid = hour < 24 && minute < 60 && second < 60 && hundredths < 100;
            if valid {
                let now = wall_clock(m);
                set_clock(m, DateTime::new(now.year, now.month, now.day, hour, minute, second));
            }
            m.cpu.regs.set_al(if valid { 0 } else { 0xFF });
        }
        0x2C => {
            let now = wall_clock(m);
            let centis = (m.cpu.cycles % m.config.instructions_per_second) * 100 / m.config.instructions_per_second;
            m.cpu.regs.set_ch(now.hour);
            m.cpu.regs.set_cl(now.minute);
            m.cpu.regs.set_dh(now.second);
            m.cpu.regs.set_dl(centis as u8);
        }
        0x2F => {
            let (segment, offset) = m.dos.dta;
            m.cpu.regs.set_es(segment);
            m.cpu.regs.set_bx(offset);
        }
        0x30 => {
            m.cpu.regs.set_ax(0x0005); // DOS 5.0
            m.cpu.regs.set_bx(0);
            m.cpu.regs.set_cx(0);
        }
        0x26 => new_psp(m, r.dx(), false),
        0x29 => parse_fcb_name(m),
        0x34 => {
            let (segment, offset) = crate::tables::in_dos_flag();
            m.cpu.regs.set_es(segment);
            m.cpu.regs.set_bx(offset);
        }
        0x52 => {
            let (segment, offset) = crate::tables::list_of_lists();
            m.cpu.regs.set_es(segment);
            m.cpu.regs.set_bx(offset);
        }
        0x5A => create_temp_file(m),
        0x5D => match r.al() {
            0x06 => {
                // The swappable data area: DS:SI, its size while in DOS (CX) and always (DX).
                m.cpu.regs.set_ds(crate::tables::SDA);
                m.cpu.regs.set_si(0);
                m.cpu.regs.set_cx(crate::tables::SDA_IN_DOS_SIZE);
                m.cpu.regs.set_dx(crate::tables::SDA_ALWAYS_SIZE);
                ok(m);
            }
            0x0A => {
                // Set extended error information: the error code is the first word at DS:DX.
                m.dos.last_error = m.mem.read_u16(Machine::lin(r.ds(), r.dx()));
                ok(m);
            }
            al => {
                m.diagnostic(format!("INT 21h AX=5D{al:02X}h is not implemented"));
                fail(m, error::INVALID_FUNCTION);
            }
        },
        0x31 => {
            // Keep DX paragraphs of the program's memory and return to the parent.
            let _ = resize(m, m.dos.psp, r.dx().max(6));
            terminate_process(m, r.al(), true);
        }
        0x33 => match r.al() {
            0x00 => {
                let on = m.dos.break_check;
                m.cpu.regs.set_dl(u8::from(on));
            }
            0x01 => m.dos.break_check = r.dl() != 0,
            0x05 => m.cpu.regs.set_dl(3),
            0x06 => {
                m.cpu.regs.set_bx(0x0005);
                m.cpu.regs.set_dx(0);
            }
            _ => m.cpu.regs.set_al(0xFF),
        },
        0x35 => {
            let at = u32::from(r.al()) * 4;
            let (offset, segment) = (m.mem.read_u16(at), m.mem.read_u16(at + 2));
            m.cpu.regs.set_bx(offset);
            m.cpu.regs.set_es(segment);
        }
        0x36 => {
            let drive = drive_argument(m, r.dl());
            if Some(drive) == m.dos.cd_drive {
                m.cpu.regs.set_ax(1); // a full CD: 2048-byte sectors, nothing free
                m.cpu.regs.set_bx(0);
                m.cpu.regs.set_cx(2048);
                m.cpu.regs.set_dx(0xFFFF);
            } else if drive == C_DRIVE {
                m.cpu.regs.set_ax(8); // sectors per cluster
                m.cpu.regs.set_bx(0x7FFF); // free clusters (~128 MB)
                m.cpu.regs.set_cx(512);
                m.cpu.regs.set_dx(0xFFF0);
            } else {
                m.cpu.regs.set_ax(0xFFFF);
            }
        }
        0x38 if r.dx() == 0xFFFF => {
            // Setting the country: only to the one it is (as on MS-DOS 6.22, DOSBox notes).
            let country = if r.al() == 0xFF { r.bx() } else { u16::from(r.al()) };
            if country == 1 {
                m.cpu.regs.set_ax(0);
                ok(m);
            } else {
                fail(m, error::FILE_NOT_FOUND);
            }
        }
        0x38 => {
            let at = Machine::lin(r.ds(), r.dx());
            m.mem.load(at, &country_info());
            m.cpu.regs.set_bx(1);
            m.cpu.regs.set_ax(1);
            ok(m);
        }
        0x60 => {
            // TRUENAME: DS:SI's path in full, upper case, at ES:DI (a device as X:/NAME).
            let raw = read_asciiz(m, r.ds(), r.si());
            let drive = path_drive(m, &raw);
            let letter = char::from(b'A' + drive);
            let full = match resolve(m, &raw, true) {
                Ok(Target::Path(path)) => Ok(format!("{letter}:\\{}", guest_path(m, drive, &path))),
                Ok(Target::Device(name)) => Ok(format!("{letter}:/{name}")),
                Err(code) => Err(code),
            };
            match full {
                Ok(full) => {
                    write_bytes(m, r.es(), r.di(), &[full.as_bytes(), &[0]].concat());
                    ok(m);
                }
                Err(code) => fail(m, code),
            }
        }
        0x39 => path_op(m, true, |m, path| if m.drive.make_dir(&path) { Ok(()) } else { Err(error::ACCESS_DENIED) }),
        0x3A => path_op(m, true, |m, path| if m.drive.remove_dir(&path) { Ok(()) } else { Err(error::ACCESS_DENIED) }),
        0x3B => {
            let raw = read_asciiz(m, r.ds(), r.dx());
            let drive = path_drive(m, &raw);
            match resolve(m, &raw, false) {
                Ok(Target::Path(path)) if m.drive.is_dir(&path) => {
                    m.dos.cwd[usize::from(drive)] = guest_path(m, drive, &path).to_string();
                    ok(m);
                }
                Ok(_) => fail(m, error::PATH_NOT_FOUND),
                Err(code) => fail(m, code),
            }
        }
        0x3C => create(m, false),
        0x5B => create(m, true),
        0x3D => open(m, r.al()),
        0x3E => match close_handle(m, r.bx()) {
            Ok(()) => ok(m),
            Err(code) => fail(m, code),
        },
        0x3F => read(m),
        0x40 => write(m),
        0x41 => {
            path_op(m, true, |m, path| if m.drive.remove_file(&path) { Ok(()) } else { Err(error::FILE_NOT_FOUND) })
        }
        0x42 => seek(m),
        0x43 => attributes(m),
        0x44 => ioctl(m),
        0x45 => match duplicate_handle(m, r.bx()) {
            Ok(new) => {
                m.cpu.regs.set_ax(new);
                ok(m);
            }
            Err(code) => fail(m, code),
        },
        0x46 => match force_duplicate(m, r.bx(), r.cx()) {
            Ok(()) => ok(m),
            Err(code) => fail(m, code),
        },
        0x47 => {
            let drive = drive_argument(m, r.dl());
            if m.dos.drive_exists(drive) {
                let mut cwd = m.dos.cwd[usize::from(drive)].clone().into_bytes();
                cwd.push(0);
                write_bytes(m, r.ds(), r.si(), &cwd);
                ok(m);
            } else {
                fail(m, error::INVALID_DRIVE);
            }
        }
        0x48 => match allocate(m, r.bx(), m.dos.psp) {
            Ok(segment) => {
                m.cpu.regs.set_ax(segment);
                ok(m);
            }
            Err((code, largest)) => {
                fail(m, code);
                m.cpu.regs.set_bx(largest);
            }
        },
        0x49 => match free(m, r.es()) {
            Ok(()) => ok(m),
            Err(code) => fail(m, code),
        },
        0x4A => match resize(m, r.es(), r.bx()) {
            Ok(()) => ok(m),
            Err((code, available)) => {
                fail(m, code);
                m.cpu.regs.set_bx(available);
            }
        },
        0x4B => exec(m),
        0x4C => terminate_process(m, r.al(), false),
        0x4D => {
            let code = m.dos.return_code;
            m.cpu.regs.set_ax(code);
            ok(m);
        }
        0x4E => find_first(m),
        0x4F => find_next(m),
        0x50 => m.dos.psp = r.bx(),
        0x55 => new_psp(m, r.dx(), true),
        0x51 | 0x62 => m.cpu.regs.set_bx(m.dos.psp),
        // Switch character and AVAILDEV (DOSBox): '/' always, setting is accepted and ignored.
        0x37 => match r.al() {
            0 | 2 => {
                m.cpu.regs.set_al(0);
                m.cpu.regs.set_dl(b'/');
            }
            1 | 3 => m.cpu.regs.set_al(0),
            _ => m.cpu.regs.set_al(0xFF),
        },
        // DBCS lead-byte table (DOSBox: AL=0 only, clearing CF; other subfunctions return AL=FFh).
        0x63 if r.al() == 0 => {
            let (segment, offset) = crate::tables::DBCS_TABLE;
            m.cpu.regs.set_ds(segment);
            m.cpu.regs.set_si(offset);
            m.cpu.regs.set_al(0);
            ok(m);
        }
        0x63 => m.cpu.regs.set_al(0xFF),
        // Global code page: 437 (US), the only one there is.
        0x66 if r.al() == 1 => {
            m.cpu.regs.set_bx(437);
            m.cpu.regs.set_dx(437);
            ok(m);
        }
        0x66 if r.al() == 2 && r.bx() == 437 => ok(m),
        0x66 => {
            m.diagnostic(format!("INT 21h AX={:04X}h: code page {} is not supported", r.ax(), r.bx()));
            fail(m, error::INVALID_FUNCTION);
        }
        0x56 => {
            let (from_raw, to_raw) = (read_asciiz(m, r.ds(), r.dx()), read_asciiz(m, r.es(), r.di()));
            let read_only = on_cd(m, &from_raw) || on_cd(m, &to_raw);
            let from = resolve(m, &from_raw, false);
            let to = resolve(m, &to_raw, false);
            match (from, to) {
                _ if read_only => fail(m, error::ACCESS_DENIED),
                (Ok(Target::Path(from)), Ok(Target::Path(to))) => {
                    if !m.drive.is_file(&from) {
                        fail(m, error::FILE_NOT_FOUND);
                    } else if m.drive.rename_file(&from, &to) {
                        ok(m);
                    } else {
                        fail(m, error::ACCESS_DENIED);
                    }
                }
                (Err(code), _) | (_, Err(code)) => fail(m, code),
                _ => fail(m, error::ACCESS_DENIED),
            }
        }
        0x57 => match (r.al(), handle_file(m, r.bx())) {
            (0, Some(_)) => {
                m.cpu.regs.set_cx(FILE_TIME);
                m.cpu.regs.set_dx(FILE_DATE);
                ok(m);
            }
            (1, Some(_)) => ok(m),
            (_, None) => fail(m, error::INVALID_HANDLE),
            _ => fail(m, error::INVALID_FUNCTION),
        },
        0x58 => match r.al() {
            0x00 => {
                let strategy = m.dos.allocation_strategy;
                m.cpu.regs.set_ax(strategy);
                ok(m);
            }
            0x01 => {
                m.dos.allocation_strategy = r.bx();
                ok(m);
            }
            0x02 => {
                m.cpu.regs.set_al(0);
                ok(m);
            }
            _ => fail(m, error::INVALID_FUNCTION),
        },
        0x59 => {
            let code = m.dos.last_error;
            m.cpu.regs.set_ax(code);
            m.cpu.regs.set_bx(0x0D01); // class: unknown, action: retry
            m.cpu.regs.set_ch(0x01);
        }
        0x67 => match set_handle_count(m, r.bx()) {
            Ok(()) => ok(m),
            Err(code) => fail(m, code),
        },
        0x68 => ok(m), // commit file
        0x6C => extended_open(m),
        ah => {
            m.diagnostic(format!("INT 21h AH={ah:02X}h is not implemented"));
            fail(m, error::INVALID_FUNCTION);
        }
    }
}

// ---- paths ----

pub(crate) enum Target {
    Device(&'static str),
    Path(String),
}

/// Resolves a guest path against its drive's current directory, to a path in the virtual drive.
/// The CD drive shows C: or one of its folders. Components are upper-cased and truncated to 8.3
/// like DOS does; wildcards are allowed in the last component only when `wildcards` is set.
/// `..` never climbs above a drive's root.
pub(crate) fn resolve(m: &Machine, raw: &[u8], wildcards: bool) -> Result<Target, u16> {
    let mut text = raw;
    while let [rest @ .., b' '] = text {
        text = rest;
    }
    if text.is_empty() {
        return Err(error::PATH_NOT_FOUND);
    }
    let drive = path_drive(m, text);
    if !m.dos.drive_exists(drive) {
        return Err(error::PATH_NOT_FOUND);
    }
    if text.len() >= 2 && text[1] == b':' {
        text = &text[2..];
    }
    let absolute = matches!(text.first(), Some(b'\\' | b'/'));
    let cwd = &m.dos.cwd[usize::from(drive)];
    let mut parts: Vec<String> =
        if absolute { Vec::new() } else { cwd.split('\\').filter(|p| !p.is_empty()).map(String::from).collect() };
    let components: Vec<&[u8]> = text.split(|&b| b == b'\\' || b == b'/').collect();
    let last = components.len() - 1;
    for (i, component) in components.into_iter().enumerate() {
        match component {
            b"" | b"." => {}
            b".." => {
                parts.pop();
            }
            name => parts.push(normalize_name(name, wildcards && i == last).ok_or(error::PATH_NOT_FOUND)?),
        }
    }
    if let Some(last) = parts.last() {
        let base = last.split('.').next().unwrap_or("");
        if let Some(device) = DEVICES.iter().find(|&&d| d == base) {
            return Ok(Target::Device(device));
        }
    }
    if Some(drive) == m.dos.cd_drive && !m.dos.cd_root.is_empty() {
        parts.insert(0, m.dos.cd_root.clone());
    }
    Ok(Target::Path(parts.join("\\")))
}

fn normalize_name(name: &[u8], wildcards: bool) -> Option<String> {
    let valid =
        |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'()-@^_`{}~".contains(&b) || (wildcards && b"?*".contains(&b));
    let upper = name.to_ascii_uppercase();
    let (base, ext) = match upper.iter().position(|&b| b == b'.') {
        Some(dot) => (&upper[..dot], &upper[dot + 1..]),
        None => (&upper[..], &[][..]),
    };
    if base.is_empty() || !base.iter().chain(ext).all(|&b| valid(b)) {
        return None;
    }
    let base = String::from_utf8_lossy(&base[..base.len().min(8)]).into_owned();
    let ext = String::from_utf8_lossy(&ext[..ext.len().min(3)]).into_owned();
    Some(if ext.is_empty() { base } else { format!("{base}.{ext}") })
}

/// Resolves DS:DX and runs `op` on it. Operations that `write` are refused on the CD drive.
fn path_op(m: &mut Machine, write: bool, op: impl FnOnce(&mut Machine, String) -> Result<(), u16>) {
    let r = m.cpu.regs;
    let raw = read_asciiz(m, r.ds(), r.dx());
    if write && on_cd(m, &raw) {
        return fail(m, error::ACCESS_DENIED);
    }
    let result = match resolve(m, &raw, false) {
        Ok(Target::Path(path)) => op(m, path),
        Ok(Target::Device(_)) => Err(error::ACCESS_DENIED),
        Err(code) => Err(code),
    };
    match result {
        Ok(()) => ok(m),
        Err(code) => fail(m, code),
    }
}

// ---- file handles ----

/// The current process's job file table: its linear address and number of entries.
fn jft(m: &Machine) -> (u32, u16) {
    let psp = Machine::lin(m.dos.psp, 0);
    let size = m.mem.read_u16(psp + 0x32);
    let (offset, segment) = (m.mem.read_u16(psp + 0x34), m.mem.read_u16(psp + 0x36));
    (Machine::lin(segment, offset), size)
}

/// The system file table entry a handle of the current process refers to.
fn sft_index(m: &Machine, handle: u16) -> Option<usize> {
    let (table, size) = jft(m);
    if handle >= size {
        return None;
    }
    let index = usize::from(m.mem.read_u8(table + u32::from(handle)));
    m.dos.files.get(index)?.as_ref()?;
    Some(index)
}

fn handle_file(m: &Machine, handle: u16) -> Option<Handle> {
    let index = sft_index(m, handle)?;
    m.dos.files[index].as_ref().map(|f| f.handle.clone())
}

fn handle_file_mut(m: &mut Machine, handle: u16) -> Option<&mut Handle> {
    let index = sft_index(m, handle)?;
    m.dos.files[index].as_mut().map(|f| &mut f.handle)
}

fn free_jft_slot(m: &Machine) -> Option<u16> {
    let (table, size) = jft(m);
    (0..size).find(|&h| m.mem.read_u8(table + u32::from(h)) == 0xFF)
}

/// Opens a new system file table entry and a handle to it in the current process.
fn install_handle(m: &mut Machine, handle: Handle, inherit: bool) -> Result<u16, u16> {
    let slot = free_jft_slot(m).ok_or(error::TOO_MANY_FILES)?;
    let index = m.dos.files.iter().position(Option::is_none).ok_or(error::TOO_MANY_FILES)?;
    m.dos.files[index] = Some(OpenFile { handle, refs: 1, inherit });
    let (table, _) = jft(m);
    m.mem.write_u8(table + u32::from(slot), index as u8);
    Ok(slot)
}

/// Drops one reference to a system file table entry, closing it with the last one.
fn release_file(m: &mut Machine, index: usize) {
    if let Some(file) = &mut m.dos.files[index] {
        file.refs = file.refs.saturating_sub(1);
        if file.refs == 0 {
            m.dos.files[index] = None;
        }
    }
}

fn close_handle(m: &mut Machine, handle: u16) -> Result<(), u16> {
    let index = sft_index(m, handle).ok_or(error::INVALID_HANDLE)?;
    let (table, _) = jft(m);
    m.mem.write_u8(table + u32::from(handle), 0xFF);
    release_file(m, index);
    Ok(())
}

/// INT 21h AH=45h: a second handle to the same open file (sharing its position).
fn duplicate_handle(m: &mut Machine, handle: u16) -> Result<u16, u16> {
    let index = sft_index(m, handle).ok_or(error::INVALID_HANDLE)?;
    let slot = free_jft_slot(m).ok_or(error::TOO_MANY_FILES)?;
    let (table, _) = jft(m);
    m.mem.write_u8(table + u32::from(slot), index as u8);
    if let Some(file) = &mut m.dos.files[index] {
        file.refs += 1;
    }
    Ok(slot)
}

/// INT 21h AH=46h: makes `target` refer to `handle`'s file, closing it first if open.
fn force_duplicate(m: &mut Machine, handle: u16, target: u16) -> Result<(), u16> {
    let index = sft_index(m, handle).ok_or(error::INVALID_HANDLE)?;
    let (table, size) = jft(m);
    if target >= size {
        return Err(error::INVALID_HANDLE);
    }
    if target == handle {
        return Ok(());
    }
    if sft_index(m, target).is_some() {
        close_handle(m, target)?;
    }
    m.mem.write_u8(table + u32::from(target), index as u8);
    if let Some(file) = &mut m.dos.files[index] {
        file.refs += 1;
    }
    Ok(())
}

/// INT 21h AH=67h: moves the job file table into a block of `count` entries.
fn set_handle_count(m: &mut Machine, count: u16) -> Result<(), u16> {
    let (table, size) = jft(m);
    let psp = m.dos.psp;
    let open = (0..size).filter(|&h| m.mem.read_u8(table + u32::from(h)) != 0xFF).max().map_or(0, |h| h + 1);
    if count < open {
        return Err(error::TOO_MANY_FILES);
    }
    let entries: Vec<u8> = (0..size).map(|h| m.mem.read_u8(table + u32::from(h))).collect();
    let (segment, offset) = if count <= JFT_SIZE {
        (psp, 0x18)
    } else {
        (allocate(m, count.div_ceil(16), psp).map_err(|(code, _)| code)?, 0)
    };
    for h in 0..count {
        let value = entries.get(usize::from(h)).copied().unwrap_or(0xFF);
        m.mem.write_u8(Machine::lin(segment, offset) + u32::from(h), value);
    }
    let at = Machine::lin(psp, 0);
    m.mem.write_u16(at + 0x32, count);
    m.mem.write_u16(at + 0x34, offset);
    m.mem.write_u16(at + 0x36, segment);
    Ok(())
}

fn device_handle(device: &str) -> Handle {
    if device == "CON" { Handle::Console } else { Handle::Null }
}

fn open_target(m: &mut Machine, raw: &[u8], mode: u8) -> Result<u16, u16> {
    let drive = path_drive(m, raw);
    let handle = match resolve(m, raw, false)? {
        Target::Device(device) => device_handle(device),
        Target::Path(path) => {
            if !m.drive.is_file(&path) {
                return Err(if m.drive.parent_exists(&path) { error::FILE_NOT_FOUND } else { error::PATH_NOT_FOUND });
            }
            if mode & 7 > 2 {
                return Err(error::INVALID_FUNCTION);
            }
            // On the CD drive, read/write opens succeed read-only and writes fail later, as with
            // MSCDEX (and DOSBox); write-only opens are refused.
            let on_cd = Some(drive) == m.dos.cd_drive;
            if on_cd && mode & 7 == 1 {
                return Err(error::ACCESS_DENIED);
            }
            Handle::File { path, pos: 0, write: mode & 7 != 0 && !on_cd, drive }
        }
    };
    install_handle(m, handle, mode & 0x80 == 0)
}

fn create_target(m: &mut Machine, raw: &[u8], must_not_exist: bool) -> Result<u16, u16> {
    let drive = path_drive(m, raw);
    let handle = match resolve(m, raw, false)? {
        Target::Device(device) => device_handle(device),
        Target::Path(path) => {
            if Some(drive) == m.dos.cd_drive {
                return Err(error::ACCESS_DENIED);
            }
            if !m.drive.parent_exists(&path) {
                return Err(error::PATH_NOT_FOUND);
            }
            if m.drive.is_dir(&path) {
                return Err(error::ACCESS_DENIED);
            }
            if must_not_exist && m.drive.is_file(&path) {
                return Err(error::FILE_EXISTS);
            }
            if free_jft_slot(m).is_none() || !m.dos.files.iter().any(Option::is_none) {
                return Err(error::TOO_MANY_FILES);
            }
            m.drive.create(&path);
            Handle::File { path, pos: 0, write: true, drive }
        }
    };
    install_handle(m, handle, true)
}

fn open(m: &mut Machine, mode: u8) {
    let raw = read_asciiz(m, m.cpu.regs.ds(), m.cpu.regs.dx());
    match open_target(m, &raw, mode) {
        Ok(handle) => {
            m.cpu.regs.set_ax(handle);
            ok(m);
        }
        Err(code) => fail(m, code),
    }
}

/// INT 21h AH=5Ah: creates a file with a new name in the directory DS:DX names (which must have
/// room for the name to be appended), returning its handle. DOS makes the name from the clock;
/// here it comes from the cycle counter (DOSBox's are random), so runs stay deterministic.
fn create_temp_file(m: &mut Machine) {
    let r = m.cpu.regs;
    let mut dir = read_asciiz(m, r.ds(), r.dx());
    if !matches!(dir.last(), Some(b'\\' | b'/')) {
        dir.push(b'\\');
    }
    let mut stamp = m.cpu.cycles as u32;
    loop {
        let mut path = dir.clone();
        path.extend(format!("{stamp:08X}").bytes());
        let exists =
            matches!(resolve(m, &path, false), Ok(Target::Path(ref p)) if m.drive.is_file(p) || m.drive.is_dir(p));
        if !exists {
            match create_target(m, &path, true) {
                Ok(handle) => {
                    path.push(0);
                    write_bytes(m, r.ds(), r.dx(), &path);
                    m.cpu.regs.set_ax(handle);
                    ok(m);
                }
                Err(code) => fail(m, code),
            }
            return;
        }
        stamp = stamp.wrapping_add(1);
    }
}

fn create(m: &mut Machine, must_not_exist: bool) {
    let raw = read_asciiz(m, m.cpu.regs.ds(), m.cpu.regs.dx());
    match create_target(m, &raw, must_not_exist) {
        Ok(handle) => {
            m.cpu.regs.set_ax(handle);
            ok(m);
        }
        Err(code) => fail(m, code),
    }
}

fn extended_open(m: &mut Machine) {
    let r = m.cpu.regs;
    let raw = read_asciiz(m, r.ds(), r.si());
    let exists = matches!(resolve(m, &raw, false), Ok(Target::Path(ref p)) if m.drive.is_file(p))
        || matches!(resolve(m, &raw, false), Ok(Target::Device(_)));
    let action = r.dx();
    let result = match (exists, action & 0x0F, action & 0xF0) {
        (true, 0x01, _) => open_target(m, &raw, r.bl()).map(|h| (h, 1)),
        (true, 0x02, _) => create_target(m, &raw, false).map(|h| (h, 3)),
        (true, _, _) => Err(error::FILE_EXISTS),
        (false, _, 0x10) => create_target(m, &raw, true).map(|h| (h, 2)),
        (false, _, _) => Err(error::FILE_NOT_FOUND),
    };
    match result {
        Ok((handle, taken)) => {
            m.cpu.regs.set_ax(handle);
            m.cpu.regs.set_cx(taken);
            ok(m);
        }
        Err(code) => fail(m, code),
    }
}

fn read(m: &mut Machine) {
    let r = m.cpu.regs;
    let Some(handle) = handle_file(m, r.bx()) else {
        return fail(m, error::INVALID_HANDLE);
    };
    match handle {
        Handle::Null => {
            m.cpu.regs.set_ax(0);
            ok(m);
        }
        // Reading no bytes returns at once, without waiting for a key (DOSBox's CON device).
        Handle::Console if r.cx() == 0 => {
            m.cpu.regs.set_ax(0);
            ok(m);
        }
        Handle::Console => match bios::pop_key(m) {
            Some(key) if r.cx() > 0 => {
                write_bytes(m, r.ds(), r.dx(), &[key as u8]);
                m.cpu.regs.set_ax(1);
                ok(m);
            }
            Some(_) => {
                m.cpu.regs.set_ax(0);
                ok(m);
            }
            None => m.wait_for_input(),
        },
        Handle::File { path, pos, .. } => {
            let data = m.drive.file(&path).unwrap_or(&[]);
            let start = (pos as usize).min(data.len());
            let end = (start + usize::from(r.cx())).min(data.len());
            let chunk = data[start..end].to_vec();
            crate::trace::FileAccess::record(m, false, &path, start as u32, chunk.len() as u32);
            write_bytes(m, r.ds(), r.dx(), &chunk);
            set_position(m, r.bx(), pos + chunk.len() as u32);
            m.cpu.regs.set_ax(chunk.len() as u16);
            ok(m);
        }
    }
}

fn write(m: &mut Machine) {
    let r = m.cpu.regs;
    let Some(handle) = handle_file(m, r.bx()) else {
        return fail(m, error::INVALID_HANDLE);
    };
    let data = m.mem.read_bytes(Machine::lin(r.ds(), r.dx()), usize::from(r.cx()));
    match handle {
        Handle::Null => {}
        Handle::Console => data.iter().for_each(|&b| bios::teletype(m, b)),
        Handle::File { write: false, .. } => return fail(m, error::ACCESS_DENIED),
        Handle::File { path, pos, .. } => {
            if data.is_empty() {
                m.drive.set_len(&path, pos as usize);
            } else {
                crate::trace::FileAccess::record(m, true, &path, pos, data.len() as u32);
                m.drive.write_at(&path, pos as usize, &data);
                set_position(m, r.bx(), pos + data.len() as u32);
            }
        }
    }
    m.cpu.regs.set_ax(r.cx());
    ok(m);
}

fn set_position(m: &mut Machine, handle: u16, new_pos: u32) {
    if let Some(Handle::File { pos, .. }) = handle_file_mut(m, handle) {
        *pos = new_pos;
    }
}

fn seek(m: &mut Machine) {
    let r = m.cpu.regs;
    let offset = ((u32::from(r.cx()) << 16) | u32::from(r.dx())) as i32;
    let Some(handle) = handle_file(m, r.bx()) else {
        return fail(m, error::INVALID_HANDLE);
    };
    let Handle::File { path, pos, .. } = handle else {
        m.cpu.regs.set_ax(0);
        m.cpu.regs.set_dx(0);
        return ok(m);
    };
    let size = m.drive.file(&path).map_or(0, <[u8]>::len) as i64;
    let base = match r.al() {
        0 => 0,
        1 => i64::from(pos),
        2 => size,
        _ => return fail(m, error::INVALID_FUNCTION),
    };
    let target = base + i64::from(offset);
    if target < 0 {
        return fail(m, error::INVALID_DATA);
    }
    let target = target as u32;
    set_position(m, r.bx(), target);
    m.cpu.regs.set_ax(target as u16);
    m.cpu.regs.set_dx((target >> 16) as u16);
    ok(m);
}

fn attributes(m: &mut Machine) {
    let r = m.cpu.regs;
    let raw = read_asciiz(m, r.ds(), r.dx());
    let attr = match resolve(m, &raw, false) {
        Ok(Target::Path(path)) if m.drive.is_file(&path) => 0x20,
        Ok(Target::Path(path)) if m.drive.is_dir(&path) => 0x10,
        Ok(Target::Device(_)) => 0x00,
        Ok(Target::Path(_)) => return fail(m, error::FILE_NOT_FOUND),
        Err(code) => return fail(m, code),
    };
    match r.al() {
        0 => {
            m.cpu.regs.set_cx(attr);
            ok(m);
        }
        1 => ok(m),
        _ => fail(m, error::INVALID_FUNCTION),
    }
}

fn ioctl(m: &mut Machine) {
    let r = m.cpu.regs;
    match r.al() {
        0x00 => match handle_file(m, r.bx()) {
            Some(Handle::Console) => {
                m.cpu.regs.set_dx(0x80D3);
                ok(m);
            }
            Some(Handle::Null) => {
                m.cpu.regs.set_dx(0x8084);
                ok(m);
            }
            Some(Handle::File { drive, .. }) => {
                m.cpu.regs.set_dx(u16::from(drive)); // a file, with its drive number
                ok(m);
            }
            None => fail(m, error::INVALID_HANDLE),
        },
        0x01 => ok(m),
        0x08 | 0x09 => {
            let drive = drive_argument(m, r.bl());
            if !m.dos.drive_exists(drive) {
                return fail(m, error::INVALID_DRIVE);
            }
            let cd = Some(drive) == m.dos.cd_drive;
            if r.al() == 0x08 {
                m.cpu.regs.set_ax(u16::from(!cd)); // 1 = fixed disk, 0 = removable
            } else {
                m.cpu.regs.set_dx(if cd { 0x1000 } else { 0 }); // CD drives are network-redirected
            }
            ok(m);
        }
        0x0E => {
            m.cpu.regs.set_al(0);
            ok(m);
        }
        0x06 | 0x07 => {
            m.cpu.regs.set_al(0xFF); // input/output status: ready
            ok(m);
        }
        al => {
            m.diagnostic(format!("INT 21h AX=44{al:02X}h is not implemented"));
            fail(m, error::INVALID_FUNCTION);
        }
    }
}

// ---- MSCDEX (INT 2Fh AH=15h) ----

/// INT 2Fh. Only MSCDEX is answered, and only when a CD drive is configured; everything else
/// returns unchanged, as if no TSR were installed.
fn multiplex(m: &mut Machine) {
    if crate::xms::multiplex(m) {
        return;
    }
    let r = m.cpu.regs;
    if r.ax() == 0x1687 {
        return crate::dpmi::detect(m);
    }
    let Some(cd) = m.dos.cd_drive else { return };
    if r.ah() != 0x15 {
        return;
    }
    match r.al() {
        0x00 => {
            m.cpu.regs.set_bx(1); // one CD drive
            m.cpu.regs.set_cx(u16::from(cd));
        }
        0x01 => {
            // Drive device list: subunit, then a far pointer to the driver header.
            let [lo, hi] = bios::ROM_CD_DEVICE.to_le_bytes();
            let [seg_lo, seg_hi] = bios::ROM_SEGMENT.to_le_bytes();
            write_bytes(m, r.es(), r.bx(), &[0, lo, hi, seg_lo, seg_hi]);
        }
        0x0B => {
            m.cpu.regs.set_ax(if r.cx() == u16::from(cd) { 0x5AD8 } else { 0 });
            m.cpu.regs.set_bx(0xADAD);
        }
        0x0C => m.cpu.regs.set_bx(0x0215), // MSCDEX 2.21
        0x0D => write_bytes(m, r.es(), r.bx(), &[cd]),
        0x10 => cd_device_request(m),
        0x02..=0x04 => {
            // Copyright, abstract and bibliographic file names, from the volume descriptor.
            let image = cd_image(m);
            let at = [702, 739, 776][usize::from(r.al() - 2)];
            let pvd = image.read_sector(16, false).unwrap_or_else(|| vec![0; 2048]);
            let mut name: Vec<u8> = pvd[at..at + 37].iter().copied().take_while(|&b| b != b' ' && b != 0).collect();
            name.push(0);
            write_bytes(m, r.es(), r.bx(), &name);
            ok(m);
        }
        0x05 | 0x08 if r.cx() != u16::from(cd) => fail(m, 15),
        0x05 => {
            // Read the volume table of contents: descriptor DX (from sector 16) to ES:BX.
            match cd_image(m).read_sector(16 + u32::from(r.dx()), false) {
                Some(sector) => {
                    write_bytes(m, r.es(), r.bx(), &sector);
                    let kind = match (&sector[1..6] == b"CD001", sector[0]) {
                        (true, 1) => 1,
                        (true, 0xFF) => 0xFF,
                        _ => 0,
                    };
                    m.cpu.regs.set_ax(kind);
                    ok(m);
                }
                None => fail(m, 21),
            }
        }
        0x08 => {
            // Absolute read: DX sectors from SI:DI to ES:BX, cooked.
            let image = cd_image(m);
            let first = u32::from(r.si()) << 16 | u32::from(r.di());
            let mut at = Machine::lin(r.es(), r.bx());
            for sector in first..first + u32::from(r.dx()) {
                let Some(data) = image.read_sector(sector, false) else { return fail(m, 21) };
                m.mem.load(at, &data);
                at += 2048;
            }
            ok(m);
        }
        0x09 => fail(m, 21), // write: the disc is read-only
        al => m.diagnostic(format!("MSCDEX INT 2Fh AX=15{al:02X}h is not implemented")),
    }
}

/// The disc in the CD drive: its image, or the drive's folder as one data track.
fn cd_image(m: &Machine) -> std::sync::Arc<crate::cdrom::CdImage> {
    m.cd_image.clone().unwrap_or_else(|| std::sync::Arc::new(crate::cdrom::CdImage::folder(300_000)))
}

/// AX=1510h: a device driver request for the CD drive, header at ES:BX. Ported from DOSBox
/// Staging `dos_mscdex.cpp` (`MSCDEX_Interrupt_Handler`, `MSCDEX_IOCTL_Input`/`Optput`): IOCTL
/// queries, PLAY, STOP, RESUME and SEEK on the disc's audio tracks (by virtual time,
/// [`crate::cdrom::CdAudio`]), and raw sector reads from an image.
fn cd_device_request(m: &mut Machine) {
    const DONE: u16 = 0x0100;
    const ERROR: u16 = 0x8000;
    const BUSY: u16 = 0x0200;
    let header = Machine::lin(m.cpu.regs.es(), m.cpu.regs.bx());
    let command = m.mem.read_u8(header + 2);
    let buffer = Machine::lin(m.mem.read_u16(header + 0x10), m.mem.read_u16(header + 0x0E));
    let image = cd_image(m);
    let now = m.elapsed_ns();
    // A sector number, given as High Sierra or as Red Book minutes, seconds, frames.
    let address = |m: &Machine, at: u32| {
        let value = m.mem.read_u32(at);
        if m.mem.read_u8(header + 0x0D) == 0 {
            value
        } else {
            let [frames, seconds, minutes, _] = value.to_le_bytes();
            crate::cdrom::from_msf(minutes, seconds, frames).saturating_sub(crate::cdrom::FRAME_PADDING)
        }
    };
    let error = match command {
        0x03 => cd_ioctl_input(m, buffer, &image, now),
        0x0C => cd_ioctl_output(m, buffer, &image, now),
        0x0D | 0x0E => None,
        0x80 | 0x82 => {
            // READ LONG (and with prefetch): count sectors from the start, cooked or raw.
            let (first, count) = (address(m, header + 0x14), m.mem.read_u16(header + 0x12));
            let raw = m.mem.read_u8(header + 0x18) == 1;
            let mut at = buffer;
            let mut failed = None;
            for sector in first..first + u32::from(count) {
                match image.read_sector(sector, raw) {
                    Some(data) => {
                        m.mem.load(at, &data);
                        at += data.len() as u32;
                    }
                    None => {
                        failed = Some(0x08); // sector not found
                        break;
                    }
                }
            }
            failed
        }
        0x83 => {
            let sector = address(m, header + 0x14);
            m.dos.cd_audio.seek(sector, now);
            None
        }
        0x84 => {
            let (sector, length) = (address(m, header + 0x0E), m.mem.read_u32(header + 0x12));
            (!m.dos.cd_audio.play(&image, sector, length, now)).then_some(0x0C)
        }
        0x85 => {
            m.dos.cd_audio.stop(&image, now);
            None
        }
        0x88 => {
            m.dos.cd_audio.resume(&image, now);
            None
        }
        command => {
            m.diagnostic(format!("MSCDEX device request {command:02X}h is not implemented"));
            Some(0x03)
        }
    };
    let busy = if m.dos.cd_audio.is_playing(now) { BUSY } else { 0 };
    m.mem.write_u16(header + 3, DONE | busy | error.map_or(0, |code| ERROR | code));
}

/// IOCTL input: the first byte of the transfer buffer says what to return. None, or the error.
fn cd_ioctl_input(m: &mut Machine, buffer: u32, image: &crate::cdrom::CdImage, now: u64) -> Option<u16> {
    use crate::cdrom::{FRAME_PADDING, msf};
    let audio = &m.dos.cd_audio;
    let red_book = |sector: u32| {
        let (minutes, seconds, frames) = msf(sector + FRAME_PADDING);
        u32::from_le_bytes([frames, seconds, minutes, 0])
    };
    match m.mem.read_u8(buffer) {
        0x00 => {
            m.mem.write_u16(buffer + 1, bios::ROM_CD_DEVICE);
            m.mem.write_u16(buffer + 3, bios::ROM_SEGMENT);
        }
        0x01 => {
            // The head's position, High Sierra (0) or Red Book (1).
            let sector = audio.current(image, now);
            match m.mem.read_u8(buffer + 1) {
                0 => m.mem.write_u32(buffer + 2, sector),
                1 => m.mem.write_u32(buffer + 2, red_book(sector)),
                _ => return Some(0x03),
            }
        }
        0x04 => {
            let channels = audio.channels;
            for (i, (input, volume)) in channels.into_iter().enumerate() {
                m.mem.write_u8(buffer + 1 + 2 * i as u32, input);
                m.mem.write_u8(buffer + 2 + 2 * i as u32, volume);
            }
        }
        0x06 => {
            // Door closed and unlocked, raw and cooked reads, audio, Red Book and HSG; audio playing.
            let playing = if audio.is_playing(now) { 1 << 10 } else { 0 };
            m.mem.write_u32(buffer + 1, 1 << 2 | 1 << 4 | 1 << 8 | 1 << 9 | playing);
        }
        0x07 => match m.mem.read_u8(buffer + 1) {
            0 => m.mem.write_u16(buffer + 2, 2048),
            1 => m.mem.write_u16(buffer + 2, 2352),
            _ => return Some(0x03),
        },
        0x08 => m.mem.write_u32(buffer + 1, image.lead_out() + FRAME_PADDING),
        0x09 => m.mem.write_u8(buffer + 1, 1), // media not changed
        0x0A => {
            let tracks = &image.tracks;
            let (first, last) = (tracks[0].number, tracks[tracks.len() - 2].number);
            m.mem.write_u8(buffer + 1, first);
            m.mem.write_u8(buffer + 2, last);
            m.mem.write_u32(buffer + 3, red_book(image.lead_out()));
        }
        0x0B => {
            let number = m.mem.read_u8(buffer + 1);
            let track = image.tracks[..image.tracks.len() - 1].iter().find(|t| t.number == number);
            m.mem.write_u32(buffer + 2, track.map_or(0, |t| red_book(t.start)));
            m.mem.write_u8(buffer + 6, track.map_or(0, |t| t.attr));
        }
        0x0C => {
            // The Q channel: the track, and the position in it and on the disc.
            let sector = audio.current(image, now);
            let track = image.track_at(sector).unwrap_or(&image.tracks[0]);
            let relative = msf(sector.saturating_sub(track.start));
            let absolute = msf(sector + FRAME_PADDING);
            let bytes = [
                track.attr,
                crate::cdrom::bcd(track.number),
                1,
                relative.0,
                relative.1,
                relative.2,
                0,
                absolute.0,
                absolute.1,
                absolute.2,
            ];
            m.mem.load(buffer + 1, &bytes);
        }
        0x0E => m.mem.load(buffer + 1, &[0; 9]), // no catalogue number
        0x0F => {
            // Paused, and the last PLAY's start and end (where RESUME goes on), Red Book.
            let paused = audio.is_paused();
            let (start, length) = audio.last_play(now).unwrap_or((0, 0));
            m.mem.write_u16(buffer + 1, u16::from(paused));
            m.mem.write_u32(buffer + 3, if length == 0 { 0 } else { red_book(start) });
            m.mem.write_u32(buffer + 7, if length == 0 { 0 } else { red_book(start + length) });
        }
        code => {
            m.diagnostic(format!("MSCDEX IOCTL input {code:02X}h is not implemented"));
            return Some(0x03);
        }
    }
    None
}

/// IOCTL output: eject, lock, reset, channel control, close.
fn cd_ioctl_output(m: &mut Machine, buffer: u32, image: &crate::cdrom::CdImage, now: u64) -> Option<u16> {
    match m.mem.read_u8(buffer) {
        0x00 | 0x01 | 0x05 => None,
        0x02 => {
            m.dos.cd_audio.stop(image, now);
            None
        }
        0x03 => {
            for i in 0..4 {
                let input = m.mem.read_u8(buffer + 1 + 2 * i);
                let volume = m.mem.read_u8(buffer + 2 + 2 * i);
                m.dos.cd_audio.channels[i as usize] = (input, volume);
            }
            None
        }
        code => {
            m.diagnostic(format!("MSCDEX IOCTL output {code:02X}h is not implemented"));
            Some(0x03)
        }
    }
}

// ---- FindFirst / FindNext ----
//
// Search state lives in the DTA's reserved bytes like real DOS keeps it: 01–0Bh the pattern in
// FCB form, 0Ch the attribute mask, 0Dh–0Eh the next entry index, 0Fh–10h a directory id.

fn to_fcb(name: &str) -> [u8; 11] {
    let mut fcb = [b' '; 11];
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    let (name_field, ext_field) = fcb.split_at_mut(8);
    for (field, text) in [(name_field, base), (ext_field, ext)] {
        for (slot, b) in field.iter_mut().zip(text.bytes()) {
            *slot = b;
        }
    }
    fcb
}

fn pattern_fcb(pattern: &str) -> [u8; 11] {
    let mut fcb = [b' '; 11];
    let (base, ext) = pattern.split_once('.').unwrap_or((pattern, ""));
    let (name_field, ext_field) = fcb.split_at_mut(8);
    for (field, text) in [(name_field, base), (ext_field, ext)] {
        let mut star = false;
        for (i, slot) in field.iter_mut().enumerate() {
            let b = text.as_bytes().get(i).copied();
            star |= b == Some(b'*');
            *slot = if star { b'?' } else { b.unwrap_or(b' ') };
        }
    }
    fcb
}

fn fcb_matches(pattern: &[u8; 11], name: &[u8; 11]) -> bool {
    pattern.iter().zip(name).all(|(&p, &n)| p == b'?' || p == n)
}

fn find_first(m: &mut Machine) {
    let r = m.cpu.regs;
    let raw = read_asciiz(m, r.ds(), r.dx());
    let result = search_first(m, &raw, r.cl());
    report(m, result);
}

fn report(m: &mut Machine, result: Result<(), u16>) {
    match result {
        Ok(()) => ok(m),
        Err(code) => fail(m, code),
    }
}

/// FindFirst for `raw` with attribute mask `attr`: the search state and the first match go
/// into the DTA.
fn search_first(m: &mut Machine, raw: &[u8], attr: u8) -> Result<(), u16> {
    let path = match resolve(m, raw, true)? {
        Target::Path(path) => path,
        Target::Device(_) => return Err(error::NO_MORE_FILES),
    };
    let dir = vfs::parent_of(&path).to_owned();
    if !m.drive.is_dir(&dir) {
        return Err(error::PATH_NOT_FOUND);
    }
    let dir_id = match m.dos.search_dirs.iter().position(|d| *d == dir) {
        Some(i) => i,
        None => {
            m.dos.search_dirs.push(dir);
            m.dos.search_dirs.len() - 1
        }
    };
    let (segment, offset) = m.dos.dta;
    let pattern = pattern_fcb(vfs::name_of(&path));
    let mut state = vec![3u8];
    state.extend(pattern);
    state.push(attr);
    state.extend(0u16.to_le_bytes());
    state.extend((dir_id as u16).to_le_bytes());
    write_bytes(m, segment, offset, &state);
    // The volume label (DOSBox `localDrive::FindFirst`): a search for the label alone gets it on
    // any drive (a CD's even when empty), and a hard disk's root search that includes labels
    // gets it first when it matches the pattern.
    let drive = path_drive(m, raw);
    let cd = Some(drive) == m.dos.cd_drive;
    let root = guest_path(m, drive, vfs::parent_of(&path)).is_empty();
    let label = volume_label(m, drive);
    if attr == 0x08 && (cd || !label.is_empty())
        || attr & 0x08 != 0 && !cd && root && fcb_matches(&pattern, &to_fcb(&label))
    {
        found(m, &label, 0x08, 0);
        return Ok(());
    }
    search_next(m)
}

/// Fills the DTA's result fields for a found entry.
fn found(m: &mut Machine, name: &str, attr: u8, size: u32) {
    let (segment, offset) = m.dos.dta;
    let at = Machine::lin(segment, offset);
    m.mem.write_u8(at + 0x15, attr);
    m.mem.write_u16(at + 0x16, FILE_TIME);
    m.mem.write_u16(at + 0x18, FILE_DATE);
    m.mem.write_u32(at + 0x1A, size);
    let mut bytes = name.as_bytes().to_vec();
    bytes.resize(13, 0);
    write_bytes(m, segment, offset.wrapping_add(0x1E), &bytes);
}

fn find_next(m: &mut Machine) {
    let result = search_next(m);
    report(m, result);
}

/// FindNext from the search state in the DTA.
fn search_next(m: &mut Machine) -> Result<(), u16> {
    let (segment, offset) = m.dos.dta;
    let at = Machine::lin(segment, offset);
    let reserved = m.mem.read_bytes(at, 0x15);
    let pattern: [u8; 11] = reserved[1..12].try_into().expect("11 bytes");
    let attr = reserved[12];
    let mut index = u16::from_le_bytes([reserved[13], reserved[14]]);
    let dir_id = usize::from(u16::from_le_bytes([reserved[15], reserved[16]]));
    let Some(dir) = m.dos.search_dirs.get(dir_id).cloned() else {
        return Err(error::NO_MORE_FILES);
    };
    if attr == 0x08 {
        return Err(error::NO_MORE_FILES); // only the label matched
    }
    let mut entries = m.drive.list(&dir).unwrap_or_default();
    if !dir.is_empty() {
        for name in ["..", "."] {
            entries.insert(0, vfs::DirEntry { name: name.into(), is_dir: true, size: 0 });
        }
    }
    while let Some(entry) = entries.get(usize::from(index)) {
        index += 1;
        let fcb = if entry.name.starts_with('.') {
            let mut fcb = [b' '; 11];
            fcb[..entry.name.len()].copy_from_slice(entry.name.as_bytes());
            fcb
        } else {
            to_fcb(&entry.name)
        };
        if (entry.is_dir && attr & 0x10 == 0) || !fcb_matches(&pattern, &fcb) {
            continue;
        }
        m.mem.write_u16(at + 13, index);
        let (name, size) = (entry.name.clone(), entry.size);
        found(m, &name, if entry.is_dir { 0x10 } else { 0x20 }, size);
        return Ok(());
    }
    m.mem.write_u16(at + 13, index);
    Err(error::NO_MORE_FILES)
}

// ---- FCB file functions ----
//
// Ported from DOSBox Staging `src/dos/dos_files.cpp` (`DOS_FCBOpen` to `DOS_FCBSetRandomRecord`)
// and `src/dos/dos_classes.cpp` (`DOS_FCB`) (GPL-2.0-or-later). An open FCB keeps the index of
// its system file table entry at 1Bh, with no handle in the process's table; reads and writes go
// to the record its current block and record (or random record) and record size point at, with
// the DTA as the buffer. Searches keep their state in a hidden DTA, as DOSBox's do. Departures:
// renames read the new name at 11h of the FCB proper also for extended FCBs (as DOS documents
// it), take `?` in the new name from the old one and rename every match; deletes collect their
// matches before deleting.

/// A hidden DTA for FCB searches (DOSBox `dos.tables.tempdta`), in the free DOS memory below
/// the XMS entry.
const FCB_SEARCH_DTA: (u16, u16) = (0x00B0, 0);

/// Offsets in an FCB (after an extended FCB's 7-byte header).
mod fcb {
    pub const CURRENT_BLOCK: u32 = 0x0C;
    pub const RECORD_SIZE: u32 = 0x0E;
    pub const FILE_SIZE: u32 = 0x10;
    pub const DATE: u32 = 0x14;
    pub const TIME: u32 = 0x16;
    pub const FILE: u32 = 0x1B;
    pub const CURRENT_RECORD: u32 = 0x20;
    pub const RANDOM_RECORD: u32 = 0x21;
    /// AL results of the read and write functions.
    pub const NO_DATA: u8 = 1;
    pub const PARTIAL: u8 = 3;
}

/// The FCB at `segment:offset`: where its normal part starts, and an extended FCB's attribute.
fn fcb_at(m: &Machine, segment: u16, offset: u16) -> (u32, Option<u8>) {
    let at = Machine::lin(segment, offset);
    if m.mem.read_u8(at) == 0xFF { (at + 7, Some(m.mem.read_u8(at + 6))) } else { (at, None) }
}

/// The FCB's drive (0 = A:); drive byte 0 means the current drive.
fn fcb_drive(m: &Machine, at: u32) -> u8 {
    match m.mem.read_u8(at) {
        0 => m.dos.drive,
        drive => drive - 1,
    }
}

/// The FCB's name as a path, `D:NAME.EXT` without the padding.
fn fcb_name(m: &Machine, at: u32) -> Vec<u8> {
    let field = m.mem.read_bytes(at + 1, 11);
    let trimmed = |part: &[u8]| part[..part.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1)].to_vec();
    let mut name = vec![b'A' + fcb_drive(m, at), b':'];
    name.extend(trimmed(&field[..8]));
    let ext = trimmed(&field[8..]);
    if !ext.is_empty() {
        name.push(b'.');
        name.extend(ext);
    }
    name
}

/// The 11-byte padded name of a directory entry name (DOSBox `DTAExtendName`).
fn padded_name(name: &str) -> [u8; 11] {
    let mut field = [b' '; 11];
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    for (slot, b) in field[..8].iter_mut().zip(base.bytes()) {
        *slot = b;
    }
    for (slot, b) in field[8..].iter_mut().zip(ext.bytes()) {
        *slot = b;
    }
    field
}

/// A system file table entry for `handle`, with no process handle (an FCB's).
fn open_fcb_file(m: &mut Machine, handle: Handle) -> Option<usize> {
    let index = m.dos.files.iter().position(Option::is_none)?;
    m.dos.files[index] = Some(OpenFile { handle, refs: 1, inherit: false });
    Some(index)
}

/// DOSBox `DOS_FCB::FileOpen`: the FCB now refers to file `index` (the drive byte made
/// explicit, block 0, 128-byte records, the file's size and time).
fn fill_open_fcb(m: &mut Machine, at: u32, index: usize) {
    let drive = fcb_drive(m, at);
    let size = match &m.dos.files[index] {
        Some(OpenFile { handle: Handle::File { path, .. }, .. }) => m.drive.file(path).map_or(0, <[u8]>::len),
        _ => 0,
    };
    m.mem.write_u8(at, drive + 1);
    m.mem.write_u8(at + fcb::FILE, index as u8);
    m.mem.write_u16(at + fcb::CURRENT_BLOCK, 0);
    m.mem.write_u16(at + fcb::RECORD_SIZE, 128);
    m.mem.write_u32(at + fcb::FILE_SIZE, size as u32);
    m.mem.write_u16(at + fcb::TIME, FILE_TIME);
    m.mem.write_u16(at + fcb::DATE, FILE_DATE);
}

/// AH=0Fh. A name with wildcards opens the first match.
fn fcb_open(m: &mut Machine, segment: u16, offset: u16) -> bool {
    let (at, _) = fcb_at(m, segment, offset);
    let mut name = fcb_name(m, at);
    if name.iter().any(|&b| b == b'?' || b == b'*') {
        if !fcb_find(m, segment, offset, true, false) {
            return false;
        }
        let found = read_asciiz(m, FCB_SEARCH_DTA.0, FCB_SEARCH_DTA.1 + 0x1E);
        name.truncate(2);
        name.extend(found);
    }
    let drive = fcb_drive(m, at);
    let handle = match resolve(m, &name, false) {
        Ok(Target::Device(device)) => device_handle(device),
        Ok(Target::Path(path)) if m.drive.is_file(&path) => {
            Handle::File { path, pos: 0, write: Some(drive) != m.dos.cd_drive, drive }
        }
        _ => return false,
    };
    match open_fcb_file(m, handle) {
        Some(index) => {
            fill_open_fcb(m, at, index);
            true
        }
        None => false,
    }
}

/// AH=16h: creates or truncates the file.
fn fcb_create(m: &mut Machine, segment: u16, offset: u16) -> bool {
    let (at, _) = fcb_at(m, segment, offset);
    let name = fcb_name(m, at);
    let drive = fcb_drive(m, at);
    let handle = match resolve(m, &name, false) {
        Ok(Target::Device(device)) => device_handle(device),
        Ok(Target::Path(path)) => {
            if Some(drive) == m.dos.cd_drive || !m.drive.parent_exists(&path) || m.drive.is_dir(&path) {
                return false;
            }
            if !m.dos.files.iter().any(Option::is_none) {
                return false;
            }
            m.drive.create(&path);
            Handle::File { path, pos: 0, write: true, drive }
        }
        Err(_) => return false,
    };
    match open_fcb_file(m, handle) {
        Some(index) => {
            fill_open_fcb(m, at, index);
            true
        }
        None => false,
    }
}

/// AH=10h.
fn fcb_close(m: &mut Machine, segment: u16, offset: u16) -> bool {
    let (at, _) = fcb_at(m, segment, offset);
    let index = m.mem.read_u8(at + fcb::FILE);
    // DOSBox's validity check (for Oubliette): a name or a file.
    if m.mem.read_u8(at + 1) == 0 && index == 0 {
        return false;
    }
    m.mem.write_u8(at + fcb::FILE, 0xFF);
    if m.dos.files.get(usize::from(index)).is_some_and(Option::is_some) {
        release_file(m, usize::from(index));
    }
    true
}

/// AH=11h and 12h: the next match of the FCB's name (wildcards allowed) goes into the DTA as an
/// FCB: the drive and the 32-byte directory entry, extended if the search FCB is. `report` also
/// writes it there (open and delete only want the hidden DTA's result).
fn fcb_find(m: &mut Machine, segment: u16, offset: u16, first: bool, report: bool) -> bool {
    let (at, attr) = fcb_at(m, segment, offset);
    let dta = m.dos.dta;
    m.dos.dta = FCB_SEARCH_DTA;
    let result = if first {
        let name = fcb_name(m, at);
        search_first(m, &name, attr.unwrap_or(0x20))
    } else {
        search_next(m)
    };
    m.dos.dta = dta;
    if result.is_err() {
        return false;
    }
    if report {
        let search = Machine::lin(FCB_SEARCH_DTA.0, FCB_SEARCH_DTA.1);
        let name = String::from_utf8_lossy(&read_asciiz(m, FCB_SEARCH_DTA.0, FCB_SEARCH_DTA.1 + 0x1E)).into_owned();
        let (found_attr, time, date, size) = (
            m.mem.read_u8(search + 0x15),
            m.mem.read_u16(search + 0x16),
            m.mem.read_u16(search + 0x18),
            m.mem.read_u32(search + 0x1A),
        );
        let out = Machine::lin(dta.0, dta.1);
        let header = if attr.is_some() { 7 } else { 0 };
        for i in 0..33 + header {
            m.mem.write_u8(out + i, 0);
        }
        if let Some(attr) = attr {
            m.mem.write_u8(out, 0xFF);
            m.mem.write_u8(out + 6, attr);
        }
        let entry = out + header;
        m.mem.write_u8(entry, fcb_drive(m, at) + 1);
        m.mem.load(entry + 1, &padded_name(&name));
        m.mem.write_u8(entry + 0x0C, found_attr);
        m.mem.write_u16(entry + 0x17, time);
        m.mem.write_u16(entry + 0x19, date);
        m.mem.write_u32(entry + 0x1D, size);
    }
    true
}

/// The paths of every file matching the FCB's name.
fn fcb_matches_all(m: &mut Machine, segment: u16, offset: u16) -> Vec<String> {
    let (at, _) = fcb_at(m, segment, offset);
    let mut names = Vec::new();
    let mut more = fcb_find(m, segment, offset, true, false);
    while more {
        let name = read_asciiz(m, FCB_SEARCH_DTA.0, FCB_SEARCH_DTA.1 + 0x1E);
        let mut path = vec![b'A' + fcb_drive(m, at), b':'];
        path.extend(name);
        if let Ok(Target::Path(path)) = resolve(m, &path, false)
            && m.drive.is_file(&path)
        {
            names.push(path);
        }
        more = fcb_find(m, segment, offset, false, false);
    }
    names
}

/// AH=13h: deletes every match; true if any was deleted.
fn fcb_delete(m: &mut Machine, segment: u16, offset: u16) -> bool {
    let (at, _) = fcb_at(m, segment, offset);
    if Some(fcb_drive(m, at)) == m.dos.cd_drive {
        return false;
    }
    let mut deleted = false;
    for path in fcb_matches_all(m, segment, offset) {
        deleted |= m.drive.remove_file(&path);
    }
    deleted
}

/// AH=17h: renames every match of the name at 01h to the name at 11h, where `?` keeps the old
/// name's character. Files open under the old name are closed first, as DOSBox does.
fn fcb_rename(m: &mut Machine, segment: u16, offset: u16) -> bool {
    let (at, _) = fcb_at(m, segment, offset);
    let drive = fcb_drive(m, at);
    if Some(drive) == m.dos.cd_drive {
        return false;
    }
    let template = m.mem.read_bytes(at + 0x11, 11);
    let mut renamed = false;
    for from in fcb_matches_all(m, segment, offset) {
        let old = padded_name(vfs::name_of(&from));
        let new: Vec<u8> = template.iter().zip(old).map(|(&t, o)| if t == b'?' { o } else { t }).collect();
        let trimmed = |part: &[u8]| String::from_utf8_lossy(part).trim_end().to_owned();
        let (base, ext) = (trimmed(&new[..8]), trimmed(&new[8..]));
        let name = if ext.is_empty() { base } else { format!("{base}.{ext}") };
        let to = vfs::join(vfs::parent_of(&from), &name);
        for file in m.dos.files.iter_mut() {
            if matches!(file, Some(OpenFile { handle: Handle::File { path, .. }, .. }) if *path == from) {
                *file = None;
            }
        }
        renamed |= !m.drive.is_file(&to) && m.drive.rename_file(&from, &to);
    }
    renamed
}

/// The FCB's record size (0 means 128, which is stored) and its file, reopened by name if it was
/// closed (DOSBox's "Reopened closed FCB").
fn fcb_file(m: &mut Machine, segment: u16, offset: u16, at: u32) -> Option<(Handle, u32)> {
    let mut index = m.mem.read_u8(at + fcb::FILE);
    let mut size = m.mem.read_u16(at + fcb::RECORD_SIZE);
    if index == 0xFF && size != 0 {
        if !fcb_open(m, segment, offset) {
            return None;
        }
        index = m.mem.read_u8(at + fcb::FILE);
        size = m.mem.read_u16(at + fcb::RECORD_SIZE);
    }
    if size == 0 {
        size = 128;
        m.mem.write_u16(at + fcb::RECORD_SIZE, size);
    }
    let handle = m.dos.files.get(usize::from(index))?.as_ref()?.handle.clone();
    Some((handle, u32::from(size)))
}

/// The byte position of the FCB's current record.
fn fcb_position(m: &Machine, at: u32, record_size: u32) -> u32 {
    let block = u32::from(m.mem.read_u16(at + fcb::CURRENT_BLOCK));
    let record = u32::from(m.mem.read_u8(at + fcb::CURRENT_RECORD));
    (block * 128 + record).wrapping_mul(record_size)
}

fn fcb_next_record(m: &mut Machine, at: u32) {
    let mut block = m.mem.read_u16(at + fcb::CURRENT_BLOCK);
    let mut record = m.mem.read_u8(at + fcb::CURRENT_RECORD) + 1;
    if record > 127 {
        block = block.wrapping_add(1);
        record = 0;
    }
    m.mem.write_u16(at + fcb::CURRENT_BLOCK, block);
    m.mem.write_u8(at + fcb::CURRENT_RECORD, record);
}

/// Reads the current record into DTA record `slot` (DOSBox `DOS_FCBRead`): 0 read, 1 nothing
/// left, 3 a partial record (zero-padded).
fn fcb_read(m: &mut Machine, segment: u16, offset: u16, slot: u32) -> u8 {
    let (at, _) = fcb_at(m, segment, offset);
    let Some((handle, record_size)) = fcb_file(m, segment, offset, at) else { return fcb::NO_DATA };
    let Handle::File { path, .. } = handle else { return fcb::NO_DATA };
    let position = fcb_position(m, at, record_size) as usize;
    let data = m.drive.file(&path).unwrap_or(&[]);
    let chunk = data.get(position..).unwrap_or(&[]);
    let read = chunk.len().min(record_size as usize);
    if read == 0 {
        return fcb::NO_DATA;
    }
    let mut record = chunk[..read].to_vec();
    record.resize(record_size as usize, 0);
    let dta = Machine::lin(m.dos.dta.0, m.dos.dta.1) + slot * record_size;
    for (i, &b) in record.iter().enumerate() {
        m.mem.write_u8(dta + i as u32, b);
    }
    fcb_next_record(m, at);
    if read == record_size as usize { 0 } else { fcb::PARTIAL }
}

/// Writes DTA record `slot` to the current record (DOSBox `DOS_FCBWrite`): 0 written, 1 failed.
fn fcb_write(m: &mut Machine, segment: u16, offset: u16, slot: u32) -> u8 {
    let (at, _) = fcb_at(m, segment, offset);
    let Some((handle, record_size)) = fcb_file(m, segment, offset, at) else { return fcb::NO_DATA };
    let dta = Machine::lin(m.dos.dta.0, m.dos.dta.1) + slot * record_size;
    let data = m.mem.read_bytes(dta, record_size as usize);
    let position = fcb_position(m, at, record_size);
    match handle {
        Handle::File { write: false, .. } => return fcb::NO_DATA,
        Handle::File { path, .. } => {
            m.drive.write_at(&path, position as usize, &data);
        }
        Handle::Console => data.iter().for_each(|&b| bios::teletype(m, b)),
        Handle::Null => {}
    }
    let size = m.mem.read_u32(at + fcb::FILE_SIZE).max(position + record_size);
    m.mem.write_u32(at + fcb::FILE_SIZE, size);
    m.mem.write_u16(at + fcb::DATE, FILE_DATE);
    m.mem.write_u16(at + fcb::TIME, FILE_TIME);
    fcb_next_record(m, at);
    0
}

/// A block write of no records sets the file's size to the current record's position (DOSBox
/// `DOS_FCBIncreaseSize`).
fn fcb_set_size(m: &mut Machine, segment: u16, offset: u16) -> u8 {
    let (at, _) = fcb_at(m, segment, offset);
    let Some((handle, record_size)) = fcb_file(m, segment, offset, at) else { return fcb::NO_DATA };
    let position = fcb_position(m, at, record_size);
    match handle {
        Handle::File { write: false, .. } => return fcb::NO_DATA,
        Handle::File { path, .. } => {
            m.drive.set_len(&path, position as usize);
        }
        _ => {}
    }
    let size = m.mem.read_u32(at + fcb::FILE_SIZE).max(position);
    m.mem.write_u32(at + fcb::FILE_SIZE, size);
    0
}

/// AH=21h, 22h (`single`: one record; the current record ends up at the random record) and 27h,
/// 28h (CX records; the random record ends up after them) (DOSBox `DOS_FCBRandomRead`,
/// `DOS_FCBRandomWrite`). Returns AL and the records done.
fn fcb_random(m: &mut Machine, segment: u16, offset: u16, count: u16, single: bool, write: bool) -> (u8, u16) {
    let (at, _) = fcb_at(m, segment, offset);
    let random = m.mem.read_u32(at + fcb::RANDOM_RECORD);
    m.mem.write_u16(at + fcb::CURRENT_BLOCK, (random / 128) as u16);
    m.mem.write_u8(at + fcb::CURRENT_RECORD, (random & 127) as u8);
    let saved = (m.mem.read_u16(at + fcb::CURRENT_BLOCK), m.mem.read_u8(at + fcb::CURRENT_RECORD));
    let (mut error, mut done) = (0, 0);
    if write && count == 0 {
        error = fcb_set_size(m, segment, offset);
    } else {
        while done < count {
            error = if write {
                fcb_write(m, segment, offset, u32::from(done))
            } else {
                fcb_read(m, segment, offset, u32::from(done))
            };
            if error != 0 {
                break;
            }
            done += 1;
        }
        if !write && error == fcb::PARTIAL {
            done += 1; // a partial record counts
        }
    }
    let (block, record) = (m.mem.read_u16(at + fcb::CURRENT_BLOCK), m.mem.read_u8(at + fcb::CURRENT_RECORD));
    if single {
        m.mem.write_u16(at + fcb::CURRENT_BLOCK, saved.0);
        m.mem.write_u8(at + fcb::CURRENT_RECORD, saved.1);
    } else {
        m.mem.write_u32(at + fcb::RANDOM_RECORD, u32::from(block) * 128 + u32::from(record));
    }
    (error, done)
}

/// AH=23h: the file's size in records (rounded up) into the random record.
fn fcb_file_size(m: &mut Machine, segment: u16, offset: u16) -> bool {
    let (at, _) = fcb_at(m, segment, offset);
    let name = fcb_name(m, at);
    let Ok(Target::Path(path)) = resolve(m, &name, false) else { return false };
    let Some(size) = m.drive.file(&path).map(<[u8]>::len) else { return false };
    let record_size = match m.mem.read_u16(at + fcb::RECORD_SIZE) {
        0 => 128,
        size => usize::from(size),
    };
    m.mem.write_u32(at + fcb::RANDOM_RECORD, size.div_ceil(record_size) as u32);
    true
}

/// INT 21h AH=0Fh-17h, 21h-24h, 27h, 28h: the FCB at DS:DX. Results are in AL (and CX for the
/// block functions); flags are left alone.
fn fcb_function(m: &mut Machine) {
    let r = m.cpu.regs;
    let (segment, offset) = (r.ds(), r.dx());
    let flag = |success: bool| if success { 0x00 } else { 0xFF };
    let al = match r.ah() {
        0x0F => flag(fcb_open(m, segment, offset)),
        0x10 => flag(fcb_close(m, segment, offset)),
        0x11 => flag(fcb_find(m, segment, offset, true, true)),
        0x12 => flag(fcb_find(m, segment, offset, false, true)),
        0x13 => flag(fcb_delete(m, segment, offset)),
        0x14 => fcb_read(m, segment, offset, 0),
        0x15 => fcb_write(m, segment, offset, 0),
        0x16 => flag(fcb_create(m, segment, offset)),
        0x17 => flag(fcb_rename(m, segment, offset)),
        0x21 | 0x22 => fcb_random(m, segment, offset, 1, true, r.ah() == 0x22).0,
        0x23 => flag(fcb_file_size(m, segment, offset)),
        0x24 => {
            let (at, _) = fcb_at(m, segment, offset);
            let block = u32::from(m.mem.read_u16(at + fcb::CURRENT_BLOCK));
            let record = u32::from(m.mem.read_u8(at + fcb::CURRENT_RECORD));
            m.mem.write_u32(at + fcb::RANDOM_RECORD, block * 128 + record);
            return;
        }
        _ => {
            let (al, done) = fcb_random(m, segment, offset, r.cx(), false, r.ah() == 0x28);
            m.cpu.regs.set_cx(done);
            al
        }
    };
    m.cpu.regs.set_al(al);
}

// ---- memory control blocks ----

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct Mcb {
    segment: u16,
    kind: u8,
    owner: u16,
    size: u16,
}

fn write_mcb(m: &mut Machine, segment: u16, kind: u8, owner: u16, size: u16) {
    let at = Machine::lin(segment, 0);
    m.mem.write_u8(at, kind);
    m.mem.write_u16(at + 1, owner);
    m.mem.write_u16(at + 3, size);
}

fn mcb_chain(m: &Machine) -> Result<Vec<Mcb>, u16> {
    let mut chain = Vec::new();
    let mut segment = FIRST_MCB;
    loop {
        let at = Machine::lin(segment, 0);
        let kind = m.mem.read_u8(at);
        if kind != b'M' && kind != b'Z' {
            return Err(error::MCB_DESTROYED);
        }
        let mcb = Mcb { segment, kind, owner: m.mem.read_u16(at + 1), size: m.mem.read_u16(at + 3) };
        chain.push(mcb);
        let next = u32::from(segment) + u32::from(mcb.size) + 1;
        if kind == b'Z' {
            return Ok(chain);
        }
        if next >= u32::from(MEMORY_END) || chain.len() > MAX_MCBS {
            return Err(error::MCB_DESTROYED);
        }
        segment = next as u16;
    }
}

/// Merges runs of adjacent free blocks; returns the resulting chain.
fn coalesce(m: &mut Machine) -> Result<Vec<Mcb>, u16> {
    let mut chain = mcb_chain(m)?;
    let mut i = 0;
    while i + 1 < chain.len() {
        if chain[i].owner == 0 && chain[i + 1].owner == 0 {
            let next = chain.remove(i + 1);
            chain[i].size += next.size + 1;
            chain[i].kind = next.kind;
            write_mcb(m, chain[i].segment, chain[i].kind, 0, chain[i].size);
        } else {
            i += 1;
        }
    }
    Ok(chain)
}

/// Allocates `paragraphs` (first fit) and returns the block's segment; on failure the error
/// code and the largest free block.
pub(crate) fn allocate(m: &mut Machine, paragraphs: u16, owner: u16) -> Result<u16, (u16, u16)> {
    let chain = coalesce(m).map_err(|code| (code, 0))?;
    let mut largest = 0;
    for block in chain {
        if block.owner != 0 {
            continue;
        }
        largest = largest.max(block.size);
        if block.size >= paragraphs {
            if block.size > paragraphs {
                write_mcb(m, block.segment + 1 + paragraphs, block.kind, 0, block.size - paragraphs - 1);
                write_mcb(m, block.segment, b'M', owner, paragraphs);
            } else {
                write_mcb(m, block.segment, block.kind, owner, paragraphs);
            }
            return Ok(block.segment + 1);
        }
    }
    Err((error::INSUFFICIENT_MEMORY, largest))
}

pub(crate) fn free(m: &mut Machine, segment: u16) -> Result<(), u16> {
    let chain = mcb_chain(m)?;
    let block = chain.iter().find(|b| b.segment + 1 == segment).ok_or(error::INVALID_BLOCK)?;
    write_mcb(m, block.segment, block.kind, 0, block.size);
    Ok(())
}

pub(crate) fn resize(m: &mut Machine, segment: u16, paragraphs: u16) -> Result<(), (u16, u16)> {
    let chain = coalesce(m).map_err(|code| (code, 0))?;
    let index = chain.iter().position(|b| b.segment + 1 == segment).ok_or((error::INVALID_BLOCK, 0))?;
    let block = chain[index];
    let (mut available, mut kind) = (block.size, block.kind);
    if let Some(next) = chain.get(index + 1).filter(|n| n.owner == 0) {
        available += next.size + 1;
        kind = next.kind;
    }
    if paragraphs > available {
        return Err((error::INSUFFICIENT_MEMORY, available));
    }
    if paragraphs == available {
        write_mcb(m, block.segment, kind, block.owner, paragraphs);
    } else {
        write_mcb(m, block.segment, b'M', block.owner, paragraphs);
        write_mcb(m, block.segment + 1 + paragraphs, kind, 0, available - paragraphs - 1);
    }
    Ok(())
}

fn set_owner(m: &mut Machine, block: u16, owner: u16, name: &[u8]) {
    let at = Machine::lin(block - 1, 0);
    m.mem.write_u16(at + 1, owner);
    let mut padded = [0u8; 8];
    for (slot, &b) in padded.iter_mut().zip(name) {
        *slot = b;
    }
    m.mem.load(at + 8, &padded);
}

// ---- program loading and processes ----
//
// EXEC and termination follow DOSBox Staging `src/dos/dos_execute.cpp` (GPL-2.0-or-later):
// memory sizing, load-high EXEs, the parent's saved stack and registers, the INT 22h return
// address, and what a terminating process gives back.

/// Where a loaded program starts.
struct Entry {
    psp: u16,
    size: u16,
    cs: u16,
    ip: u16,
    ss: u16,
    sp: u16,
}

fn find_program(m: &Machine, raw: &[u8]) -> Result<(String, Vec<u8>), LoadError> {
    let name = String::from_utf8_lossy(raw).into_owned();
    if vfs::name_of(&name).eq_ignore_ascii_case("COMMAND.COM") {
        // The built-in shell, wherever the program looks for it.
        let path = match resolve(m, raw, false) {
            Ok(Target::Path(path)) => path,
            _ => "COMMAND.COM".into(),
        };
        return Ok((path, crate::shell::image()));
    }
    match resolve(m, raw, false) {
        Ok(Target::Path(path)) if m.drive.is_file(&path) => {
            let data = m.drive.file(&path).expect("checked above").to_vec();
            Ok((path, data))
        }
        _ => Err(LoadError::NotFound(name)),
    }
}

// ---- file control blocks ----

/// Characters that end a name or extension in an FCB file name.
fn fcb_name_char(c: u8) -> bool {
    c > 0x1F && !b":.;,=+ \t/\"[]<>|".contains(&c)
}

/// INT 21h AH=29h: parses the file name at DS:SI into the FCB at ES:DI (DOSBox
/// `FCB_Parsename`). AL bit 0 skips a leading separator; bits 1, 2 and 3 keep the FCB's drive,
/// name and extension when the string has none. Returns AL = 0, 1 (wildcards) or FFh (no such
/// drive, though the name is still parsed), with SI past what was parsed.
fn parse_fcb_name(m: &mut Machine) {
    let r = m.cpu.regs;
    let options = r.al();
    let fcb = Machine::lin(r.es(), r.di());
    let text = m.mem.read_bytes(Machine::lin(r.ds(), r.si()), 128);
    let at = |i: usize| text.get(i).copied().unwrap_or(0);
    let mut drive = m.mem.read_u8(fcb);
    let mut name: [u8; 8] = m.mem.read_bytes(fcb + 1, 8).try_into().expect("8 bytes");
    let mut ext: [u8; 3] = m.mem.read_bytes(fcb + 9, 3).try_into().expect("3 bytes");
    let mut result = 0u8;
    let (mut has_drive, mut has_name, mut has_ext) = (false, false, false);

    let mut i = 0;
    let skip_blanks = |i: &mut usize| {
        while matches!(at(*i), b' ' | b'\t') {
            *i += 1;
        }
    };
    skip_blanks(&mut i);
    if options & 1 != 0 && at(i) != 0 && b":;,=+".contains(&at(i)) {
        i += 1;
    }
    skip_blanks(&mut i);
    'parse: {
        if at(i + 1) == b':' {
            let letter = at(i).to_ascii_uppercase();
            i += 2;
            if !fcb_name_char(letter) {
                break 'parse;
            }
            has_drive = true;
            if !(letter.is_ascii_alphabetic() && m.dos.drive_exists(letter - b'A')) {
                result = 0xFF; // reported, but parsing goes on
            }
            drive = letter.wrapping_sub(b'A').wrapping_add(1);
        }
        if at(i) != b'.' {
            if !fcb_name_char(at(i)) {
                break 'parse;
            }
            has_name = true;
            i = parse_fcb_field(&text, i, &mut name, &mut result);
            if at(i) != b'.' {
                break 'parse;
            }
        }
        has_ext = true;
        i = parse_fcb_field(&text, i + 1, &mut ext, &mut result);
    }
    if !has_drive && options & 2 == 0 {
        drive = 0;
    }
    if !has_name && options & 4 == 0 {
        name = [b' '; 8];
    }
    if !has_ext && options & 8 == 0 {
        ext = [b' '; 3];
    }
    m.mem.write_u8(fcb, drive);
    m.mem.load(fcb + 1, &name);
    m.mem.load(fcb + 9, &ext);
    m.mem.write_u16(fcb + 0x0C, 0); // current block and record size, as DOS clears them
    m.mem.write_u16(fcb + 0x0E, 0);
    m.cpu.regs.set_al(result);
    m.cpu.regs.set_si(r.si().wrapping_add(i as u16));
}

/// Copies one FCB name field from `text` at `i`, upper-cased and blank-padded; `*` fills the
/// rest with `?`. Sets `result` to 1 for a wildcard (unless a bad drive was reported). Returns
/// where the field ended.
fn parse_fcb_field(text: &[u8], mut i: usize, field: &mut [u8], result: &mut u8) -> usize {
    let mut fill = b' ';
    let mut index = 0;
    loop {
        let mut c = text.get(i).copied().unwrap_or(0).to_ascii_uppercase();
        if c == b'*' {
            (fill, c) = (b'?', b'?');
        }
        if c == b'?' && *result == 0 && index < field.len() {
            *result = 1;
        }
        if !fcb_name_char(c) {
            field[index..].fill(fill);
            return i;
        }
        if index < field.len() {
            field[index] = if fill == b'?' { fill } else { c };
            index += 1;
        }
        i += 1;
    }
}

fn parse_program(data: &[u8]) -> Result<Option<mz::MzImage<'_>>, LoadError> {
    if data.is_empty() {
        return Err(LoadError::BadFormat("empty file".into()));
    }
    if !mz::is_mz(data) {
        return Ok(None);
    }
    // Like DOS, run the MZ part and ignore any NE/LE/LX/PE header after it: for a DOS-extended
    // program that part is the extender (CauseWay, DOS/4GW's stub), which loads the rest itself.
    mz::parse(data).map(Some).map_err(LoadError::BadFormat)
}

/// Builds an environment block for a program: `strings` (NAME=VALUE entries, each ending in
/// a NUL), an empty string, a word count of 1 and the program's full path.
fn make_environment(m: &mut Machine, strings: &[u8], drive: u8, path: &str) -> Result<u16, LoadError> {
    let mut env = strings.to_vec();
    if env.is_empty() {
        env.push(0);
    }
    env.push(0);
    env.extend(1u16.to_le_bytes());
    let path = guest_path(m, drive, path);
    env.extend(format!("{}:\\{path}\0", char::from(b'A' + drive)).bytes());
    let paragraphs = env.len().div_ceil(16) as u16;
    let segment = allocate(m, paragraphs, 0x0008).map_err(|(_, available)| LoadError::InsufficientMemory {
        needed: paragraphs.into(),
        available: available.into(),
    })?;
    m.mem.load(Machine::lin(segment, 0), &env);
    Ok(segment)
}

/// The NAME=VALUE strings of an environment block (up to, not including, the final NUL).
fn environment_strings(m: &Machine, segment: u16) -> Vec<u8> {
    let mut strings = Vec::new();
    let at = Machine::lin(segment, 0);
    for i in 0..0x8000u32 {
        let byte = m.mem.read_u8(at + i);
        if byte == 0 && (i == 0 || m.mem.read_u8(at + i - 1) == 0) {
            break;
        }
        strings.push(byte);
    }
    strings
}

/// Allocates the program's memory block, builds its PSP and loads its image (DOS EXEC
/// steps); the environment block becomes the new process's.
fn create_process(
    m: &mut Machine,
    path: &str,
    data: &[u8],
    image: Option<&mz::MzImage<'_>>,
    env_segment: u16,
    parent: Option<u16>,
) -> Result<Entry, LoadError> {
    let available = match allocate(m, 0xFFFF, 0) {
        Ok(_) => unreachable!("a 1 MB block can't be free"),
        Err((_, largest)) => largest,
    };
    let (needed, wanted) = match image {
        Some(exe) => {
            let paragraphs = exe.image.len().div_ceil(16) as u32;
            let max = if exe.max_alloc == 0 { 0xFFFF } else { 0x10 + paragraphs + u32::from(exe.max_alloc) };
            (0x10 + paragraphs + u32::from(exe.min_alloc), max)
        }
        None => ((data.len() as u32 + 0x100 + 2).div_ceil(16), 0xFFFF),
    };
    if u32::from(available) < needed {
        return Err(LoadError::InsufficientMemory { needed, available: available.into() });
    }
    let size = wanted.min(u32::from(available)) as u16;
    let psp = allocate(m, size, 0x0008).expect("fits: checked against the largest block");
    let name = vfs::name_of(path).split('.').next().unwrap_or("").to_ascii_uppercase();
    set_owner(m, env_segment, psp, b"");
    set_owner(m, psp, psp, name.as_bytes());
    build_psp(m, psp, psp + size, env_segment, parent.unwrap_or(psp));
    let entry = match image {
        Some(exe) => {
            // An EXE asking for no extra memory at all is loaded at the top of its block.
            let load = if exe.min_alloc == 0 && exe.max_alloc == 0 {
                (psp + size).wrapping_sub(exe.image.len().div_ceil(16) as u16)
            } else {
                psp + 0x10
            };
            load_image(m, data, image, load, load);
            Entry { psp, size, cs: load.wrapping_add(exe.cs), ip: exe.ip, ss: load.wrapping_add(exe.ss), sp: exe.sp }
        }
        None => {
            load_image(m, data, None, psp + 0x10, 0);
            let sp = if size >= 0x1000 { 0xFFFE } else { size * 16 - 2 };
            m.mem.write_u16(Machine::lin(psp, sp), 0); // RET goes to PSP:0000 (INT 20h)
            Entry { psp, size, cs: psp, ip: 0x100, ss: psp, sp }
        }
    };
    m.dos.process_paths.insert(psp, path.to_owned());
    Ok(entry)
}

/// What a program finds uninitialized when it starts (physical ranges): its own memory block past
/// its load image (its BSS, heap and stack), the free memory blocks, and extended memory.
pub(crate) fn uninitialized(m: &Machine) -> Vec<std::ops::Range<u32>> {
    let psp = m.dos.psp;
    // The load image's length, from the program file; an EXE asking for no extra memory is loaded
    // at the top of its block.
    let image = m.dos.process_paths.get(&psp).and_then(|path| m.drive.file(path)).map(|data| {
        match mz::parse(data).ok().filter(|_| mz::is_mz(data)) {
            Some(exe) => (exe.image.len() as u32, exe.min_alloc == 0 && exe.max_alloc == 0),
            None => (data.len().min(0xFF00) as u32, false),
        }
    });
    let mut ranges = Vec::new();
    for mcb in mcb_chain(m).unwrap_or_default() {
        let start = (u32::from(mcb.segment) + 1) << 4;
        let end = start + (u32::from(mcb.size) << 4);
        if mcb.owner == 0 {
            ranges.push(start..end);
        } else if mcb.segment + 1 == psp
            && let Some((length, at_top)) = image
        {
            let image_start = start + 0x100;
            if at_top {
                ranges.push(image_start..end.saturating_sub(length.div_ceil(16) * 16));
            } else {
                ranges.push((image_start + length).min(end)..end);
            }
        }
    }
    ranges.push(0x11_0000..m.mem.size());
    ranges
}

/// Copies a program image to `load` and applies its relocations with `relocation`.
fn load_image(m: &mut Machine, data: &[u8], image: Option<&mz::MzImage<'_>>, load: u16, relocation: u16) {
    match image {
        Some(exe) => {
            m.mem.load(Machine::lin(load, 0), exe.image);
            for &(segment, offset) in &exe.relocations {
                let at = Machine::lin(load.wrapping_add(segment), offset);
                let value = m.mem.read_u16(at).wrapping_add(relocation);
                m.mem.write_u16(at, value);
            }
        }
        None => m.mem.load(Machine::lin(load, 0), &data[..data.len().min(0xFF00)]),
    }
}

/// Sets the registers DOS leaves at program start and points the CPU at the entry.
fn start_program(m: &mut Machine, entry: &Entry, ax: u16) {
    let regs = &mut m.cpu.regs;
    regs.set_cs(entry.cs);
    regs.ip = u32::from(entry.ip);
    regs.set_ss(entry.ss);
    regs.set_sp(entry.sp);
    regs.set_ds(entry.psp);
    regs.set_es(entry.psp);
    regs.set_ax(ax);
    regs.set_bx(ax);
    regs.set_cx(0x00FF);
    regs.set_dx(entry.psp);
    regs.set_si(entry.ip);
    regs.set_di(entry.sp);
    regs.set_bp(0x091C);
    regs.flags = u32::from(cpu::flags::normalize(cpu::flags::IF));
}

impl Machine {
    /// Builds a machine with `drive` as C: and loads `program` (a DOS path on that drive).
    pub fn with_program(config: crate::Config, drive: Vfs, program: &str, args: &str) -> Result<Self, LoadError> {
        let mut machine = Machine::new(config, drive);
        if machine.dos.cd_drive.is_some() && !machine.drive.is_dir(&machine.dos.cd_root) {
            return Err(LoadError::NotFound(format!("CD folder {}", machine.config.cd_root)));
        }
        let start_dir = machine.config.start_dir.clone();
        if !start_dir.is_empty() {
            machine.change_dir(&start_dir)?;
        }
        machine.load_program(program, args)?;
        Ok(machine)
    }

    /// Makes `path` the current directory of its drive, as `CD` does at the DOS prompt.
    pub fn change_dir(&mut self, path: &str) -> Result<(), LoadError> {
        let raw = path.replace('/', "\\");
        let drive = path_drive(self, raw.as_bytes());
        match resolve(self, raw.as_bytes(), false) {
            Ok(Target::Path(dir)) if self.drive.is_dir(&dir) => {
                self.dos.cwd[usize::from(drive)] = guest_path(self, drive, &dir).to_string();
                Ok(())
            }
            _ => Err(LoadError::NotFound(path.to_string())),
        }
    }

    /// Loads an MZ or COM program as the first process (as if started from the DOS prompt)
    /// and points the CPU at its entry.
    pub fn load_program(&mut self, program: &str, args: &str) -> Result<(), LoadError> {
        if program.len() > 4 && program[program.len() - 4..].eq_ignore_ascii_case(".BAT") {
            // A batch file runs in COMMAND.COM.
            let command = format!("/C {program} {args}");
            return self.load_program("C:\\COMMAND.COM", command.trim_end());
        }
        let (path, data) = find_program(self, program.as_bytes())?;
        let image = parse_program(&data)?;
        let mut strings = b"COMSPEC=C:\\COMMAND.COM\0PATH=C:\\\0PROMPT=$P$G\0".to_vec();
        for (name, value) in &self.config.environment {
            strings.extend(format!("{name}={value}\0").bytes());
        }
        if let Some(sb) = self.config.sound_blaster
            && !self.config.environment.iter().any(|(name, _)| name.eq_ignore_ascii_case("BLASTER"))
        {
            strings.extend(format!("BLASTER={}\0", sb.blaster_variable()).bytes());
        }
        let drive = path_drive(self, program.as_bytes());
        let env_segment = make_environment(self, &strings, drive, &path)?;
        // The first process is its own parent, as when started by COMMAND.COM's /P instance.
        let entry = create_process(self, &path, &data, image.as_ref(), env_segment, None)?;
        let mut tail = if args.is_empty() { String::new() } else { format!(" {args}") }.into_bytes();
        tail.truncate(126);
        let mut block = vec![tail.len() as u8];
        block.extend(&tail);
        block.push(0x0D);
        self.mem.load(Machine::lin(entry.psp, 0x80), &block);
        self.dos.psp = entry.psp;
        self.dos.dta = (entry.psp, 0x80);
        self.dos.program = path;
        start_program(self, &entry, 0);
        Ok(())
    }
}

/// DOS error code for a failed load.
fn load_error_code(error: &LoadError) -> u16 {
    match error {
        LoadError::NotFound(_) => error::FILE_NOT_FOUND,
        LoadError::InsufficientMemory { .. } => error::INSUFFICIENT_MEMORY,
        LoadError::BadFormat(_) | LoadError::Unsupported(_) => error::INVALID_FORMAT,
    }
}

/// INT 21h AH=4Bh: load and run a child program (AL=0), load it without running it (AL=1), or
/// load an overlay (AL=3). The parent's registers and stack are saved below its INT 21h frame
/// and come back when the child terminates.
fn exec(m: &mut Machine) {
    let r = m.cpu.regs;
    let raw = read_asciiz(m, r.ds(), r.dx());
    let block = Machine::lin(r.es(), r.bx());
    let mode = r.al();
    if !matches!(mode, 0 | 1 | 3) {
        return fail(m, error::INVALID_FUNCTION);
    }
    let (path, data) = match find_program(m, &raw) {
        Ok(found) => found,
        Err(e) => return fail(m, load_error_code(&e)),
    };
    let image = match parse_program(&data) {
        Ok(image) => image,
        Err(e) => {
            m.diagnostic(format!("EXEC {path}: {e}"));
            return fail(m, load_error_code(&e));
        }
    };
    if mode == 3 {
        let (load, relocation) = (m.mem.read_u16(block), m.mem.read_u16(block + 2));
        load_image(m, &data, image.as_ref(), load, relocation);
        m.cpu.regs.set_ax(0);
        m.cpu.regs.set_dx(0);
        return ok(m);
    }

    // Environment: the given block's strings, or a copy of the parent's.
    let parent = m.dos.psp;
    let source = match m.mem.read_u16(block) {
        0 => m.mem.read_u16(Machine::lin(parent, 0x2C)),
        segment => segment,
    };
    let strings = if source == 0 { Vec::new() } else { environment_strings(m, source) };
    let drive = path_drive(m, &raw);
    let env_segment = match make_environment(m, &strings, drive, &path) {
        Ok(segment) => segment,
        Err(e) => return fail(m, load_error_code(&e)),
    };
    let entry = match create_process(m, &path, &data, image.as_ref(), env_segment, Some(parent)) {
        Ok(entry) => entry,
        Err(e) => {
            let _ = free(m, env_segment);
            return fail(m, load_error_code(&e));
        }
    };
    inherit_handles(m, parent, entry.psp);
    // Command tail and FCBs from the parameter block.
    let far = |m: &Machine, at: u32| Machine::lin(m.mem.read_u16(at + 2), m.mem.read_u16(at));
    let tail = m.mem.read_bytes(far(m, block + 2), 128);
    m.mem.load(Machine::lin(entry.psp, 0x80), &tail);
    for (field, offset) in [(6u32, 0x5Cu16), (10, 0x6C)] {
        let fcb = m.mem.read_bytes(far(m, block + field), 16);
        m.mem.load(Machine::lin(entry.psp, offset), &fcb);
    }

    // The child returns to the caller's CS:IP (from its INT 21h frame) through INT 22h.
    let (ss, sp) = (r.ss(), r.sp());
    let frame = Machine::lin(ss, sp);
    let (return_ip, return_cs) = (m.mem.read_u16(frame), m.mem.read_u16(frame + 2));
    m.mem.write_u16(0x22 * 4, return_ip);
    m.mem.write_u16(0x22 * 4 + 2, return_cs);
    // Save the caller's registers below the frame, and that stack in its PSP.
    let saved_sp = sp.wrapping_sub(18);
    for (i, value) in [r.ax(), r.bx(), r.cx(), r.dx(), r.si(), r.di(), r.bp(), r.ds(), r.es()].into_iter().enumerate() {
        m.mem.write_u16(Machine::lin(ss, saved_sp.wrapping_add(2 * i as u16)), value);
    }
    for psp in [parent, entry.psp] {
        m.mem.write_u16(Machine::lin(psp, 0x2E), saved_sp);
        m.mem.write_u16(Machine::lin(psp, 0x30), ss);
    }
    m.cpu.regs.set_sp(saved_sp);
    // The child's PSP keeps the vectors to restore when it ends.
    save_vectors(m, entry.psp);
    m.dos.psp = entry.psp;
    m.dos.dta = (entry.psp, 0x80);
    m.dos.program = path;
    // BL/BH = FFh when FCB 1/2 names a drive that doesn't exist.
    let fcb_drive = |m: &Machine, offset: u16| {
        let drive = m.mem.read_u8(Machine::lin(entry.psp, offset));
        let valid = drive == 0 || m.dos.drive_exists(drive - 1);
        if valid { 0u16 } else { 0xFF }
    };
    let ax = fcb_drive(m, 0x5C) | (fcb_drive(m, 0x6C) << 8);
    if mode == 1 {
        // Load only: the child's stack gets AX's initial value; the caller gets SS:SP and CS:IP.
        let child_sp = entry.sp.wrapping_sub(2);
        m.mem.write_u16(Machine::lin(entry.ss, child_sp), ax);
        m.mem.write_u16(block + 0x0E, child_sp);
        m.mem.write_u16(block + 0x10, entry.ss);
        m.mem.write_u16(block + 0x12, entry.ip);
        m.mem.write_u16(block + 0x14, entry.cs);
        m.cpu.regs.set_sp(sp);
        m.cpu.regs.set_ax(entry.ip);
        m.cpu.regs.set_bx(entry.size);
        m.cpu.regs.set_dx(0);
        return ok(m);
    }
    start_program(m, &entry, ax);
}

/// Gives a child copies of the parent's inheritable handles.
fn inherit_handles(m: &mut Machine, parent: u16, child: u16) {
    let saved = m.dos.psp;
    m.dos.psp = parent;
    let (table, size) = jft(m);
    let entries: Vec<u8> = (0..size.min(JFT_SIZE)).map(|h| m.mem.read_u8(table + u32::from(h))).collect();
    m.dos.psp = saved;
    for (h, &index) in entries.iter().enumerate() {
        let inherited = match m.dos.files.get_mut(usize::from(index)).and_then(Option::as_mut) {
            Some(file) if file.inherit => {
                file.refs += 1;
                index
            }
            _ => 0xFF,
        };
        m.mem.write_u8(Machine::lin(child, 0x18 + h as u16), inherited);
    }
}

fn save_vectors(m: &mut Machine, psp: u16) {
    for (i, vector) in [0x22u32, 0x23, 0x24].into_iter().enumerate() {
        let at = Machine::lin(psp, 0x0A + 4 * i as u16);
        let (offset, segment) = (m.mem.read_u16(vector * 4), m.mem.read_u16(vector * 4 + 2));
        m.mem.write_u16(at, offset);
        m.mem.write_u16(at + 2, segment);
    }
}

/// Ends the current process (INT 20h, INT 21h AH=00h/31h/4Ch): back to the parent at its INT 22h
/// address with its registers and stack, or the end of the run for the first process.
pub(crate) fn terminate_process(m: &mut Machine, code: u8, resident: bool) {
    let psp = m.dos.psp;
    if !resident {
        m.dos.shells.retain(|s| s.psp != psp);
    }
    crate::dpmi::terminated(m, psp);
    let at = Machine::lin(psp, 0);
    let parent = m.mem.read_u16(at + 0x16);
    m.dos.return_code = u16::from(code) | if resident { 0x0300 } else { 0 };
    if parent == psp || parent == 0 {
        m.terminate(code);
        return;
    }
    if !resident {
        let (table, size) = jft(m);
        for h in 0..size {
            if let Some(index) = sft_index(m, h) {
                m.mem.write_u8(table + u32::from(h), 0xFF);
                release_file(m, index);
            }
        }
    }
    // Restore INT 22h-24h from the PSP; INT 22h is where the parent continues.
    let (return_ip, return_cs) = (m.mem.read_u16(at + 0x0A), m.mem.read_u16(at + 0x0C));
    for (i, vector) in [0x22u32, 0x23, 0x24].into_iter().enumerate() {
        let saved = at + 0x0A + 4 * i as u32;
        let (offset, segment) = (m.mem.read_u16(saved), m.mem.read_u16(saved + 2));
        m.mem.write_u16(vector * 4, offset);
        m.mem.write_u16(vector * 4 + 2, segment);
    }
    m.dos.psp = parent;
    if let Some(path) = m.dos.process_paths.get(&parent) {
        m.dos.program = path.clone();
    }
    m.dos.process_paths.remove(&psp);
    let parent_at = Machine::lin(parent, 0);
    let (sp, ss) = (m.mem.read_u16(parent_at + 0x2E), m.mem.read_u16(parent_at + 0x30));
    let saved: Vec<u16> = (0..9).map(|i| m.mem.read_u16(Machine::lin(ss, sp.wrapping_add(2 * i)))).collect();
    let regs = &mut m.cpu.regs;
    regs.set_ss(ss);
    regs.set_sp(sp.wrapping_add(18));
    regs.set_ax(saved[0]);
    regs.set_bx(saved[1]);
    regs.set_cx(saved[2]);
    regs.set_dx(saved[3]);
    regs.set_si(saved[4]);
    regs.set_di(saved[5]);
    regs.set_bp(saved[6]);
    regs.set_ds(saved[7]);
    regs.set_es(saved[8]);
    // The service stub's IRET resumes the parent: return address and flags (IF set; IOPL 3 and
    // NT as DOSBox sets them, for Strike Commander).
    let frame = Machine::lin(ss, sp.wrapping_add(18));
    m.mem.write_u16(frame, return_ip);
    m.mem.write_u16(frame + 2, return_cs);
    m.mem.write_u16(frame + 4, 0x7202);
    if !resident {
        free_process_memory(m, psp);
    }
}

/// Frees every memory block a process owns.
fn free_process_memory(m: &mut Machine, psp: u16) {
    if let Ok(chain) = mcb_chain(m) {
        for block in chain.iter().filter(|b| b.owner == psp) {
            write_mcb(m, block.segment, block.kind, 0, block.size);
        }
    }
    let _ = coalesce(m);
}

/// INT 21h AH=26h (copy the current PSP to segment DX) and AH=55h (create a child PSP at DX
/// with SI paragraphs, inheriting handles).
fn new_psp(m: &mut Machine, segment: u16, child: bool) {
    let current = m.dos.psp;
    if child {
        let size = m.cpu.regs.si();
        let env = m.mem.read_u16(Machine::lin(current, 0x2C));
        build_psp(m, segment, segment.wrapping_add(size), env, current);
        inherit_handles(m, current, segment);
        let tail = m.mem.read_bytes(Machine::lin(current, 0x80), 128);
        m.mem.load(Machine::lin(segment, 0x80), &tail);
        m.dos.psp = segment;
        m.cpu.regs.set_al(0xF0);
    } else {
        let psp = m.mem.read_bytes(Machine::lin(current, 0), 0x100);
        m.mem.load(Machine::lin(segment, 0), &psp);
        let top = m.mem.read_u16(Machine::lin(current, 2));
        m.mem.write_u16(Machine::lin(segment, 2), top);
        save_vectors(m, segment);
    }
}

/// A new PSP: INT 20h at 0, memory top, saved vectors, parent, the default job file table
/// (CON, CON, CON, AUX, PRN), environment, DOS call stub, empty FCBs and command tail.
fn build_psp(m: &mut Machine, psp: u16, memory_top: u16, env_segment: u16, parent: u16) {
    let mut block = [0u8; 0x100];
    block[0..2].copy_from_slice(&[0xCD, 0x20]);
    block[2..4].copy_from_slice(&memory_top.to_le_bytes());
    for (i, vector) in [0x22u32, 0x23, 0x24].into_iter().enumerate() {
        let at = 0x0A + i * 4;
        block[at..at + 2].copy_from_slice(&m.mem.read_u16(vector * 4).to_le_bytes());
        block[at + 2..at + 4].copy_from_slice(&m.mem.read_u16(vector * 4 + 2).to_le_bytes());
    }
    block[0x16..0x18].copy_from_slice(&parent.to_le_bytes());
    block[0x18..0x2C].copy_from_slice(&[
        1, 1, 1, 0, 2, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    ]);
    block[0x2C..0x2E].copy_from_slice(&env_segment.to_le_bytes());
    block[0x32..0x34].copy_from_slice(&JFT_SIZE.to_le_bytes());
    block[0x34..0x36].copy_from_slice(&0x18u16.to_le_bytes());
    block[0x36..0x38].copy_from_slice(&psp.to_le_bytes());
    block[0x50..0x53].copy_from_slice(&[0xCD, 0x21, 0xCB]);
    for fcb in [0x5C, 0x6C] {
        block[fcb + 1..fcb + 12].fill(b' ');
    }
    block[0x81] = 0x0D;
    m.mem.load(Machine::lin(psp, 0), &block);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fcb_patterns() {
        assert_eq!(&pattern_fcb("*.*"), b"???????????");
        assert_eq!(&pattern_fcb("*.DAT"), b"????????DAT");
        assert_eq!(&pattern_fcb("LEVEL?.D*"), b"LEVEL?  D??");
        assert!(fcb_matches(&pattern_fcb("*.DAT"), &to_fcb("LEVEL1.DAT")));
        assert!(!fcb_matches(&pattern_fcb("*.DAT"), &to_fcb("GAME.EXE")));
        assert!(fcb_matches(&pattern_fcb("LEVEL?.D*"), &to_fcb("LEVEL1.DAT")));
        assert!(!fcb_matches(&pattern_fcb("LEVEL?.D*"), &to_fcb("LEVEL10.DAT")));
    }

    #[test]
    fn name_normalization() {
        assert_eq!(normalize_name(b"longfilename.text", false).as_deref(), Some("LONGFILE.TEX"));
        assert_eq!(normalize_name(b"a?.txt", false), None);
        assert_eq!(normalize_name(b"*.txt", true).as_deref(), Some("*.TXT"));
        assert_eq!(normalize_name(b"bad name", false), None);
    }
}
