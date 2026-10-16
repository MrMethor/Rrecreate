//! DOS services (INT 20h/21h/29h) as high-level emulation: memory blocks, files on the virtual
//! drive, console I/O, program loading (PROJECT.md §5.2, "HLE services").
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
const MAX_HANDLES: usize = 20;
const MAX_MCBS: usize = 4096;
const DEVICES: [&str; 10] = ["CON", "NUL", "PRN", "AUX", "CLOCK$", "LPT1", "LPT2", "LPT3", "COM1", "COM2"];
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
    pub const INVALID_DATA: u16 = 0x0D;
    pub const INVALID_DRIVE: u16 = 0x0F;
    pub const NO_MORE_FILES: u16 = 0x12;
    pub const FILE_EXISTS: u16 = 0x50;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Handle {
    Console,
    Null,
    File { path: String, pos: u32, write: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DosState {
    pub psp: u16,
    pub dta: (u16, u16),
    /// Current directory on C:, canonical (no drive, no leading backslash).
    pub cwd: String,
    pub handles: Vec<Option<Handle>>,
    pub last_error: u16,
    pub break_check: bool,
    pub allocation_strategy: u16,
    /// DOS path of the running program.
    pub program: String,
    /// Directories referenced by FindFirst/FindNext state stored in DTAs.
    search_dirs: Vec<String>,
}

impl Default for DosState {
    fn default() -> Self {
        let mut handles = vec![None; MAX_HANDLES];
        handles[0] = Some(Handle::Console);
        handles[1] = Some(Handle::Console);
        handles[2] = Some(Handle::Console);
        handles[3] = Some(Handle::Null);
        handles[4] = Some(Handle::Null);
        Self {
            psp: 0,
            dta: (0, 0x80),
            cwd: String::new(),
            handles,
            last_error: 0,
            break_check: false,
            allocation_strategy: 0,
            program: String::new(),
            search_dirs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    NotFound(String),
    BadFormat(String),
    Unsupported(String),
    InsufficientMemory { needed: u32, available: u32 },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::NotFound(p) => write!(f, "{p}: not found on drive C:"),
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
}

/// The emulated wall clock: the configured start time plus elapsed virtual time.
pub(crate) fn wall_clock(m: &Machine) -> DateTime {
    let elapsed = (m.cpu.cycles / m.config.instructions_per_second) as i64;
    DateTime::from_epoch_seconds(m.config.start_time.to_epoch_seconds() + elapsed)
}

pub(crate) fn dispatch(m: &mut Machine, id: u16) -> bool {
    match id {
        callback::TERMINATE => m.terminate(0),
        callback::DOS => int21(m),
        callback::FAST_CONSOLE => bios::teletype(m, m.cpu.regs.al()),
        _ => return false,
    }
    true
}

fn ok(m: &mut Machine) {
    m.set_return_flag(CF, false);
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
        0x00 => m.terminate(0),
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
        0x0E => m.cpu.regs.set_al(3), // drives A–C
        0x19 => m.cpu.regs.set_al(2), // C:
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
        0x2B | 0x2D => m.cpu.regs.set_al(0), // setting date/time: accepted, ignored
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
        0x31 => m.terminate(r.al()), // terminate and stay resident: treated as exit
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
            if matches!(r.dl(), 0 | 3) {
                m.cpu.regs.set_ax(8); // sectors per cluster
                m.cpu.regs.set_bx(0x7FFF); // free clusters (~128 MB)
                m.cpu.regs.set_cx(512);
                m.cpu.regs.set_dx(0xFFF0);
            } else {
                m.cpu.regs.set_ax(0xFFFF);
            }
        }
        0x39 => path_op(m, |m, path| if m.drive.make_dir(&path) { Ok(()) } else { Err(error::ACCESS_DENIED) }),
        0x3A => path_op(m, |m, path| if m.drive.remove_dir(&path) { Ok(()) } else { Err(error::ACCESS_DENIED) }),
        0x3B => path_op(m, |m, path| {
            if m.drive.is_dir(&path) {
                m.dos.cwd = path;
                Ok(())
            } else {
                Err(error::PATH_NOT_FOUND)
            }
        }),
        0x3C => create(m, false),
        0x5B => create(m, true),
        0x3D => open(m, r.al()),
        0x3E => match handle_slot(m, r.bx()) {
            Some(slot) => {
                *slot = None;
                ok(m);
            }
            None => fail(m, error::INVALID_HANDLE),
        },
        0x3F => read(m),
        0x40 => write(m),
        0x41 => path_op(m, |m, path| if m.drive.remove_file(&path) { Ok(()) } else { Err(error::FILE_NOT_FOUND) }),
        0x42 => seek(m),
        0x43 => attributes(m),
        0x44 => ioctl(m),
        0x45 => match m.dos.handles.get(usize::from(r.bx())).cloned().flatten() {
            Some(h) => match free_handle(m) {
                Some(new) => {
                    m.dos.handles[new] = Some(h);
                    m.cpu.regs.set_ax(new as u16);
                    ok(m);
                }
                None => fail(m, error::TOO_MANY_FILES),
            },
            None => fail(m, error::INVALID_HANDLE),
        },
        0x46 => {
            let source = m.dos.handles.get(usize::from(r.bx())).cloned().flatten();
            match (source, usize::from(r.cx())) {
                (Some(h), target) if target < MAX_HANDLES => {
                    m.dos.handles[target] = Some(h);
                    ok(m);
                }
                _ => fail(m, error::INVALID_HANDLE),
            }
        }
        0x47 => {
            if matches!(r.dl(), 0 | 3) {
                let mut cwd = m.dos.cwd.clone().into_bytes();
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
        0x4C => m.terminate(r.al()),
        0x4D => m.cpu.regs.set_ax(0),
        0x4E => find_first(m),
        0x4F => find_next(m),
        0x50 => m.dos.psp = r.bx(),
        0x51 | 0x62 => m.cpu.regs.set_bx(m.dos.psp),
        0x56 => {
            let from = resolve(m, &read_asciiz(m, r.ds(), r.dx()), false);
            let to = resolve(m, &read_asciiz(m, r.es(), r.di()), false);
            match (from, to) {
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
        0x57 => match (r.al(), m.dos.handles.get(usize::from(r.bx())).cloned().flatten()) {
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
        0x67 | 0x68 => ok(m), // set handle count, commit file
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

/// Resolves a guest path against the current directory. Only drive C: exists. Components are
/// upper-cased and truncated to 8.3 like DOS does; wildcards are allowed in the last component
/// only when `wildcards` is set.
pub(crate) fn resolve(m: &Machine, raw: &[u8], wildcards: bool) -> Result<Target, u16> {
    let mut text = raw;
    while let [rest @ .., b' '] = text {
        text = rest;
    }
    if text.is_empty() {
        return Err(error::PATH_NOT_FOUND);
    }
    if text.len() >= 2 && text[1] == b':' {
        if !text[0].eq_ignore_ascii_case(&b'C') {
            return Err(error::PATH_NOT_FOUND);
        }
        text = &text[2..];
    }
    let absolute = matches!(text.first(), Some(b'\\' | b'/'));
    let mut parts: Vec<String> =
        if absolute { Vec::new() } else { m.dos.cwd.split('\\').filter(|p| !p.is_empty()).map(String::from).collect() };
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

fn path_op(m: &mut Machine, op: impl FnOnce(&mut Machine, String) -> Result<(), u16>) {
    let r = m.cpu.regs;
    let raw = read_asciiz(m, r.ds(), r.dx());
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

fn free_handle(m: &Machine) -> Option<usize> {
    m.dos.handles.iter().position(Option::is_none)
}

fn handle_slot(m: &mut Machine, handle: u16) -> Option<&mut Option<Handle>> {
    m.dos.handles.get_mut(usize::from(handle)).filter(|slot| slot.is_some())
}

fn install_handle(m: &mut Machine, handle: Handle) -> Result<u16, u16> {
    let slot = free_handle(m).ok_or(error::TOO_MANY_FILES)?;
    m.dos.handles[slot] = Some(handle);
    Ok(slot as u16)
}

fn device_handle(device: &str) -> Handle {
    if device == "CON" { Handle::Console } else { Handle::Null }
}

fn open_target(m: &mut Machine, raw: &[u8], mode: u8) -> Result<u16, u16> {
    let handle = match resolve(m, raw, false)? {
        Target::Device(device) => device_handle(device),
        Target::Path(path) => {
            if !m.drive.is_file(&path) {
                return Err(if m.drive.parent_exists(&path) { error::FILE_NOT_FOUND } else { error::PATH_NOT_FOUND });
            }
            if mode & 7 > 2 {
                return Err(error::INVALID_FUNCTION);
            }
            Handle::File { path, pos: 0, write: mode & 7 != 0 }
        }
    };
    install_handle(m, handle)
}

fn create_target(m: &mut Machine, raw: &[u8], must_not_exist: bool) -> Result<u16, u16> {
    let handle = match resolve(m, raw, false)? {
        Target::Device(device) => device_handle(device),
        Target::Path(path) => {
            if !m.drive.parent_exists(&path) {
                return Err(error::PATH_NOT_FOUND);
            }
            if m.drive.is_dir(&path) {
                return Err(error::ACCESS_DENIED);
            }
            if must_not_exist && m.drive.is_file(&path) {
                return Err(error::FILE_EXISTS);
            }
            free_handle(m).ok_or(error::TOO_MANY_FILES)?;
            m.drive.create(&path);
            Handle::File { path, pos: 0, write: true }
        }
    };
    install_handle(m, handle)
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
    let Some(Some(handle)) = m.dos.handles.get(usize::from(r.bx())).cloned() else {
        return fail(m, error::INVALID_HANDLE);
    };
    match handle {
        Handle::Null => {
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
            write_bytes(m, r.ds(), r.dx(), &chunk);
            set_position(m, r.bx(), pos + chunk.len() as u32);
            m.cpu.regs.set_ax(chunk.len() as u16);
            ok(m);
        }
    }
}

fn write(m: &mut Machine) {
    let r = m.cpu.regs;
    let Some(Some(handle)) = m.dos.handles.get(usize::from(r.bx())).cloned() else {
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
                m.drive.write_at(&path, pos as usize, &data);
                set_position(m, r.bx(), pos + data.len() as u32);
            }
        }
    }
    m.cpu.regs.set_ax(r.cx());
    ok(m);
}

fn set_position(m: &mut Machine, handle: u16, new_pos: u32) {
    if let Some(Some(Handle::File { pos, .. })) = m.dos.handles.get_mut(usize::from(handle)) {
        *pos = new_pos;
    }
}

fn seek(m: &mut Machine) {
    let r = m.cpu.regs;
    let offset = ((u32::from(r.cx()) << 16) | u32::from(r.dx())) as i32;
    let Some(Some(handle)) = m.dos.handles.get(usize::from(r.bx())).cloned() else {
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
        0x00 => match m.dos.handles.get(usize::from(r.bx())).cloned().flatten() {
            Some(Handle::Console) => {
                m.cpu.regs.set_dx(0x80D3);
                ok(m);
            }
            Some(Handle::Null) => {
                m.cpu.regs.set_dx(0x8084);
                ok(m);
            }
            Some(Handle::File { .. }) => {
                m.cpu.regs.set_dx(0x0002); // a file on drive C:
                ok(m);
            }
            None => fail(m, error::INVALID_HANDLE),
        },
        0x01 => ok(m),
        0x08 => {
            m.cpu.regs.set_ax(1); // fixed disk
            ok(m);
        }
        0x09 => {
            m.cpu.regs.set_dx(0); // local
            ok(m);
        }
        0x0E => {
            m.cpu.regs.set_al(0);
            ok(m);
        }
        al => {
            m.diagnostic(format!("INT 21h AX=44{al:02X}h is not implemented"));
            fail(m, error::INVALID_FUNCTION);
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
    let path = match resolve(m, &raw, true) {
        Ok(Target::Path(path)) => path,
        Ok(Target::Device(_)) => return fail(m, error::NO_MORE_FILES),
        Err(code) => return fail(m, code),
    };
    let dir = vfs::parent_of(&path).to_owned();
    if !m.drive.is_dir(&dir) {
        return fail(m, error::PATH_NOT_FOUND);
    }
    let dir_id = match m.dos.search_dirs.iter().position(|d| *d == dir) {
        Some(i) => i,
        None => {
            m.dos.search_dirs.push(dir);
            m.dos.search_dirs.len() - 1
        }
    };
    let (segment, offset) = m.dos.dta;
    let mut state = vec![3u8];
    state.extend(pattern_fcb(vfs::name_of(&path)));
    state.push(r.cl());
    state.extend(0u16.to_le_bytes());
    state.extend((dir_id as u16).to_le_bytes());
    write_bytes(m, segment, offset, &state);
    find_next(m);
}

fn find_next(m: &mut Machine) {
    let (segment, offset) = m.dos.dta;
    let at = Machine::lin(segment, offset);
    let reserved = m.mem.read_bytes(at, 0x15);
    let pattern: [u8; 11] = reserved[1..12].try_into().expect("11 bytes");
    let attr = reserved[12];
    let mut index = u16::from_le_bytes([reserved[13], reserved[14]]);
    let dir_id = usize::from(u16::from_le_bytes([reserved[15], reserved[16]]));
    let Some(dir) = m.dos.search_dirs.get(dir_id).cloned() else {
        return fail(m, error::NO_MORE_FILES);
    };
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
        m.mem.write_u8(at + 0x15, if entry.is_dir { 0x10 } else { 0x20 });
        m.mem.write_u16(at + 0x16, FILE_TIME);
        m.mem.write_u16(at + 0x18, FILE_DATE);
        m.mem.write_u32(at + 0x1A, entry.size);
        let mut name = entry.name.clone().into_bytes();
        name.resize(13, 0);
        write_bytes(m, segment, offset.wrapping_add(0x1E), &name);
        return ok(m);
    }
    m.mem.write_u16(at + 13, index);
    fail(m, error::NO_MORE_FILES);
}

// ---- memory control blocks ----

#[derive(Debug, Clone, Copy)]
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

// ---- program loading ----

impl Machine {
    /// Builds a machine with `drive` as C: and loads `program` (a DOS path on that drive).
    pub fn with_program(config: crate::Config, drive: Vfs, program: &str, args: &str) -> Result<Self, LoadError> {
        let mut machine = Machine::new(config, drive);
        machine.load_program(program, args)?;
        Ok(machine)
    }

    /// Loads an MZ or COM program the way DOS EXEC does and points the CPU at its entry.
    pub fn load_program(&mut self, program: &str, args: &str) -> Result<(), LoadError> {
        let path = match resolve(self, program.as_bytes(), false) {
            Ok(Target::Path(path)) if self.drive.is_file(&path) => path,
            _ => return Err(LoadError::NotFound(program.to_owned())),
        };
        let data = self.drive.file(&path).expect("checked above").to_vec();
        let name = vfs::name_of(&path).to_owned();
        let is_exe = mz::is_mz(&data);
        let image = if is_exe { Some(mz::parse(&data).map_err(LoadError::BadFormat)?) } else { None };
        if let Some(lfanew) = is_exe.then(|| new_executable_kind(&data)).flatten() {
            return Err(LoadError::Unsupported(format!("{lfanew} executables need a DOS extender or Windows")));
        }

        // Environment block: variables, then a word count and the program's full path.
        let mut env = b"COMSPEC=C:\\COMMAND.COM\0PATH=C:\\\0PROMPT=$P$G\0\0".to_vec();
        env.extend(1u16.to_le_bytes());
        env.extend(format!("C:\\{path}\0").bytes());
        let env_paragraphs = env.len().div_ceil(16) as u16;
        let env_segment = allocate(self, env_paragraphs, 0x0008).map_err(|(_, available)| {
            LoadError::InsufficientMemory { needed: env_paragraphs.into(), available: available.into() }
        })?;
        self.mem.load(Machine::lin(env_segment, 0), &env);

        let available = match allocate(self, 0xFFFF, 0) {
            Ok(_) => unreachable!("a 1 MB block can't be free"),
            Err((_, largest)) => largest,
        };
        let (needed, wanted) = match &image {
            Some(exe) => {
                let image_paragraphs = exe.image.len().div_ceil(16) as u32;
                (0x10 + image_paragraphs + u32::from(exe.min_alloc), 0x10 + image_paragraphs + u32::from(exe.max_alloc))
            }
            None => ((data.len() as u32 + 0x100 + 2).div_ceil(16), u32::MAX),
        };
        if u32::from(available) < needed {
            return Err(LoadError::InsufficientMemory { needed, available: available.into() });
        }
        let size = wanted.min(u32::from(available)) as u16;
        let psp = allocate(self, size, 0x0008).expect("fits: checked against the largest block");
        set_owner(self, env_segment, psp, b"");
        set_owner(self, psp, psp, name.split('.').next().unwrap_or("").as_bytes());
        build_psp(self, psp, psp + size, env_segment, args);

        let regs = &mut self.cpu.regs;
        match image {
            Some(exe) => {
                let load = psp + 0x10;
                self.mem.load(Machine::lin(load, 0), exe.image);
                for &(segment, offset) in &exe.relocations {
                    let at = Machine::lin(load.wrapping_add(segment), offset);
                    let value = self.mem.read_u16(at).wrapping_add(load);
                    self.mem.write_u16(at, value);
                }
                regs.set_cs(load.wrapping_add(exe.cs));
                regs.ip = exe.ip;
                regs.set_ss(load.wrapping_add(exe.ss));
                regs.set_sp(exe.sp);
            }
            None => {
                self.mem.load(Machine::lin(psp, 0x100), &data);
                regs.set_cs(psp);
                regs.ip = 0x100;
                regs.set_ss(psp);
                let top = if size >= 0x1000 { 0xFFFE } else { size * 16 - 2 };
                regs.set_sp(top);
                self.mem.write_u16(Machine::lin(psp, top), 0); // RET goes to PSP:0000 (INT 20h)
            }
        }
        // Register values DOS 5 leaves at program start.
        let regs = &mut self.cpu.regs;
        regs.set_ds(psp);
        regs.set_es(psp);
        regs.set_ax(0);
        regs.set_bx(0);
        regs.set_cx(0x00FF);
        regs.set_dx(psp);
        regs.set_si(regs.ip);
        regs.set_di(regs.sp());
        regs.set_bp(0x091C);
        regs.flags = cpu::flags::normalize(cpu::flags::IF);

        self.dos.psp = psp;
        self.dos.dta = (psp, 0x80);
        self.dos.program = path;
        Ok(())
    }
}

fn new_executable_kind(data: &[u8]) -> Option<&'static str> {
    if data.len() < 0x40 || u16::from_le_bytes([data[0x18], data[0x19]]) < 0x40 {
        return None;
    }
    let lfanew = u32::from_le_bytes([data[0x3C], data[0x3D], data[0x3E], data[0x3F]]) as usize;
    match data.get(lfanew..lfanew + 2)? {
        b"PE" => Some("PE"),
        b"NE" => Some("NE"),
        b"LE" => Some("LE"),
        b"LX" => Some("LX"),
        _ => None,
    }
}

fn build_psp(m: &mut Machine, psp: u16, memory_top: u16, env_segment: u16, args: &str) {
    let mut block = [0u8; 0x100];
    block[0..2].copy_from_slice(&[0xCD, 0x20]);
    block[2..4].copy_from_slice(&memory_top.to_le_bytes());
    for (i, vector) in [0x22u32, 0x23, 0x24].into_iter().enumerate() {
        let at = 0x0A + i * 4;
        block[at..at + 2].copy_from_slice(&m.mem.read_u16(vector * 4).to_le_bytes());
        block[at + 2..at + 4].copy_from_slice(&m.mem.read_u16(vector * 4 + 2).to_le_bytes());
    }
    block[0x16..0x18].copy_from_slice(&psp.to_le_bytes()); // parent: itself, like COMMAND.COM
    block[0x18..0x2C].copy_from_slice(&[
        1, 1, 1, 0, 2, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    ]);
    block[0x2C..0x2E].copy_from_slice(&env_segment.to_le_bytes());
    block[0x32..0x34].copy_from_slice(&(MAX_HANDLES as u16).to_le_bytes());
    block[0x34..0x36].copy_from_slice(&0x18u16.to_le_bytes());
    block[0x36..0x38].copy_from_slice(&psp.to_le_bytes());
    block[0x50..0x53].copy_from_slice(&[0xCD, 0x21, 0xCB]);
    for fcb in [0x5C, 0x6C] {
        block[fcb + 1..fcb + 12].fill(b' ');
    }
    let mut tail = if args.is_empty() { String::new() } else { format!(" {args}") }.into_bytes();
    tail.truncate(126);
    block[0x80] = tail.len() as u8;
    block[0x81..0x81 + tail.len()].copy_from_slice(&tail);
    block[0x81 + tail.len()] = 0x0D;
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
