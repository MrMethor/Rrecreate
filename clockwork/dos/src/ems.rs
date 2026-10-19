//! Expanded memory: a LIM EMS 4.0 driver as high-level emulation (INT 67h).
//!
//! Logical pages are 16 KB blocks kept outside conventional memory. Mapping one into the page
//! frame (four physical pages at E000h) copies it into emulated memory; mapping it out copies
//! the frame back. While a page is mapped, the frame is its only copy, so accesses through the
//! frame and through EMS functions always agree. A logical page mapped into two physical pages
//! at once (aliasing) can't be represented this way and is reported as a diagnostic.

use std::collections::BTreeMap;

use crate::bios::callback;
use crate::machine::Machine;

pub const PAGE_SIZE: usize = 16 * 1024;
pub const FRAME_SEGMENT: u16 = 0xE000;
const PHYSICAL_PAGES: usize = 4;
const MAX_HANDLES: u16 = 255;
/// Segment of the driver: a device header named EMMXXXX0 at offset 0Ah, which is how programs
/// detect EMS, followed by the INT 67h entry stub.
pub const DRIVER_SEGMENT: u16 = 0xF100;
pub const ENTRY_OFFSET: u16 = 0x0012;

mod status {
    pub const OK: u8 = 0x00;
    pub const INVALID_HANDLE: u8 = 0x83;
    pub const UNDEFINED_FUNCTION: u8 = 0x84;
    pub const NO_HANDLES: u8 = 0x85;
    pub const CONTEXT_ERROR: u8 = 0x86;
    pub const TOO_MANY_PAGES: u8 = 0x87;
    pub const NOT_ENOUGH_PAGES: u8 = 0x88;
    pub const ZERO_PAGES: u8 = 0x89;
    pub const LOGICAL_PAGE_OUT_OF_RANGE: u8 = 0x8A;
    pub const PHYSICAL_PAGE_OUT_OF_RANGE: u8 = 0x8B;
    pub const CONTEXT_EXISTS: u8 = 0x8D;
    pub const NO_CONTEXT: u8 = 0x8E;
    pub const UNDEFINED_SUBFUNCTION: u8 = 0x8F;
    pub const REGION_TOO_LONG: u8 = 0x93;
    pub const OFFSET_OUT_OF_RANGE: u8 = 0x95;
    pub const BAD_MEMORY_TYPE: u8 = 0x98;
    pub const NAME_EXISTS: u8 = 0xA1;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
struct Handle {
    /// Storage page ids, in logical page order.
    pages: Vec<u16>,
    name: [u8; 8],
    saved_map: Option<[Option<(u16, u16)>; PHYSICAL_PAGES]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Ems {
    total_pages: u16,
    /// Contents of storage pages that aren't currently mapped; allocated on first use.
    storage: BTreeMap<u16, Vec<u8>>,
    free: Vec<u16>,
    handles: BTreeMap<u16, Handle>,
    /// (handle, logical page) shown in each physical page.
    mapped: [Option<(u16, u16)>; PHYSICAL_PAGES],
}

impl Ems {
    pub fn new(kilobytes: u32) -> Self {
        let total_pages = (kilobytes / 16).min(2048) as u16;
        let mut handles = BTreeMap::new();
        handles.insert(0, Handle::default()); // handle 0 belongs to the operating system
        Self {
            total_pages,
            storage: BTreeMap::new(),
            free: (0..total_pages).rev().collect(),
            handles,
            mapped: [None; 4],
        }
    }

    pub fn enabled(&self) -> bool {
        self.total_pages > 0
    }

    pub fn free_pages(&self) -> u16 {
        self.free.len() as u16
    }
}

fn frame_address(physical: usize) -> u32 {
    (u32::from(FRAME_SEGMENT) << 4) + (physical * PAGE_SIZE) as u32
}

pub(crate) fn install(m: &mut Machine) {
    if !m.ems.enabled() {
        return;
    }
    let mut driver = vec![0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xC0, 0, 0, 0, 0];
    driver.extend(b"EMMXXXX0");
    driver.extend(crate::bios::callback_instruction(callback::EMS));
    driver.push(0xCF); // IRET
    m.mem.load(Machine::lin(DRIVER_SEGMENT, 0), &driver);
    m.mem.write_u16(0x67 * 4, ENTRY_OFFSET);
    m.mem.write_u16(0x67 * 4 + 2, DRIVER_SEGMENT);
}

fn set_status(m: &mut Machine, code: u8) {
    m.cpu.regs.set_ah(code);
}

pub(crate) fn service(m: &mut Machine) {
    let r = m.cpu.regs;
    let code = match r.ah() {
        0x40 => status::OK,
        0x41 => {
            m.cpu.regs.set_bx(FRAME_SEGMENT);
            status::OK
        }
        0x42 => {
            let (free, total) = (m.ems.free_pages(), m.ems.total_pages);
            m.cpu.regs.set_bx(free);
            m.cpu.regs.set_dx(total);
            status::OK
        }
        0x43 | 0x5A => match allocate(m, r.bx(), r.ah() == 0x5A) {
            Ok(handle) => {
                m.cpu.regs.set_dx(handle);
                status::OK
            }
            Err(code) => code,
        },
        0x44 => map_page(m, usize::from(r.al()), r.dx(), r.bx()),
        0x45 => deallocate(m, r.dx()),
        0x46 => {
            m.cpu.regs.set_al(0x40); // version 4.0
            status::OK
        }
        0x47 => match m.ems.handles.get_mut(&r.dx()) {
            None => status::INVALID_HANDLE,
            Some(h) if h.saved_map.is_some() => status::CONTEXT_EXISTS,
            Some(h) => {
                h.saved_map = Some(m.ems.mapped);
                status::OK
            }
        },
        0x48 => match m.ems.handles.get_mut(&r.dx()).map(|h| h.saved_map.take()) {
            None => status::INVALID_HANDLE,
            Some(None) => status::NO_CONTEXT,
            Some(Some(saved)) => restore_map(m, saved),
        },
        0x4B => {
            let count = m.ems.handles.len() as u16;
            m.cpu.regs.set_bx(count);
            status::OK
        }
        0x4C => match m.ems.handles.get(&r.dx()) {
            Some(h) => {
                let pages = h.pages.len() as u16;
                m.cpu.regs.set_bx(pages);
                status::OK
            }
            None => status::INVALID_HANDLE,
        },
        0x4D => {
            let entries: Vec<(u16, u16)> = m.ems.handles.iter().map(|(&id, h)| (id, h.pages.len() as u16)).collect();
            let base = Machine::lin(r.es(), r.di());
            for (i, (id, pages)) in entries.iter().enumerate() {
                m.mem.write_u16(base + i as u32 * 4, *id);
                m.mem.write_u16(base + i as u32 * 4 + 2, *pages);
            }
            m.cpu.regs.set_bx(entries.len() as u16);
            status::OK
        }
        0x4E => page_map(m),
        0x50 => map_multiple(m),
        0x51 => reallocate(m, r.dx(), r.bx()),
        0x53 => handle_name(m),
        0x57 => move_region(m),
        0x58 => match r.al() {
            0 => {
                let base = Machine::lin(r.es(), r.di());
                for i in 0..PHYSICAL_PAGES as u32 {
                    m.mem.write_u16(base + i * 4, FRAME_SEGMENT + i as u16 * 0x400);
                    m.mem.write_u16(base + i * 4 + 2, i as u16);
                }
                m.cpu.regs.set_cx(PHYSICAL_PAGES as u16);
                status::OK
            }
            1 => {
                m.cpu.regs.set_cx(PHYSICAL_PAGES as u16);
                status::OK
            }
            _ => status::UNDEFINED_SUBFUNCTION,
        },
        0x59 if r.al() == 1 => {
            let (free, total) = (m.ems.free_pages(), m.ems.total_pages);
            m.cpu.regs.set_bx(free);
            m.cpu.regs.set_dx(total);
            status::OK
        }
        ah => {
            m.diagnostic(format!("INT 67h AH={ah:02X}h (EMS) is not implemented"));
            status::UNDEFINED_FUNCTION
        }
    };
    set_status(m, code);
}

fn allocate(m: &mut Machine, count: u16, zero_allowed: bool) -> Result<u16, u8> {
    if count == 0 && !zero_allowed {
        return Err(status::ZERO_PAGES);
    }
    if count > m.ems.total_pages {
        return Err(status::TOO_MANY_PAGES);
    }
    if count > m.ems.free_pages() {
        return Err(status::NOT_ENOUGH_PAGES);
    }
    let id = (1..=MAX_HANDLES).find(|id| !m.ems.handles.contains_key(id)).ok_or(status::NO_HANDLES)?;
    let pages = (0..count).map(|_| m.ems.free.pop().expect("checked above")).collect();
    m.ems.handles.insert(id, Handle { pages, ..Handle::default() });
    Ok(id)
}

fn deallocate(m: &mut Machine, id: u16) -> u8 {
    let Some(handle) = m.ems.handles.get(&id) else { return status::INVALID_HANDLE };
    if handle.saved_map.is_some() {
        return status::CONTEXT_EXISTS;
    }
    for physical in 0..PHYSICAL_PAGES {
        if matches!(m.ems.mapped[physical], Some((h, _)) if h == id) {
            m.ems.mapped[physical] = None;
        }
    }
    if id == 0 {
        return status::OK; // the OS handle stays, with no pages
    }
    let handle = m.ems.handles.remove(&id).expect("checked above");
    for page in handle.pages {
        m.ems.storage.remove(&page);
        m.ems.free.push(page);
    }
    status::OK
}

fn reallocate(m: &mut Machine, id: u16, count: u16) -> u8 {
    let Some(current) = m.ems.handles.get(&id).map(|h| h.pages.len() as u16) else {
        return status::INVALID_HANDLE;
    };
    if count > current && count - current > m.ems.free_pages() {
        return status::NOT_ENOUGH_PAGES;
    }
    if count < current {
        // Pages being dropped must not stay mapped.
        for physical in 0..PHYSICAL_PAGES {
            if matches!(m.ems.mapped[physical], Some((h, l)) if h == id && l >= count) {
                m.ems.mapped[physical] = None;
            }
        }
    }
    let ems = &mut m.ems;
    let handle = ems.handles.get_mut(&id).expect("checked above");
    while handle.pages.len() > usize::from(count) {
        let page = handle.pages.pop().expect("non-empty");
        ems.storage.remove(&page);
        ems.free.push(page);
    }
    while handle.pages.len() < usize::from(count) {
        handle.pages.push(ems.free.pop().expect("checked above"));
    }
    m.cpu.regs.set_bx(count);
    status::OK
}

fn storage_page(m: &Machine, handle: u16, logical: u16) -> Option<u16> {
    m.ems.handles.get(&handle)?.pages.get(usize::from(logical)).copied()
}

/// Copies physical page `physical` back to the logical page it shows, and marks it unmapped.
fn unmap(m: &mut Machine, physical: usize) {
    if let Some((handle, logical)) = m.ems.mapped[physical].take()
        && let Some(page) = storage_page(m, handle, logical)
    {
        let data = m.mem.read_bytes(frame_address(physical), PAGE_SIZE);
        m.ems.storage.insert(page, data);
    }
}

/// AH=44h. Logical page FFFFh unmaps.
fn map_page(m: &mut Machine, physical: usize, handle: u16, logical: u16) -> u8 {
    if !m.ems.handles.contains_key(&handle) {
        return status::INVALID_HANDLE;
    }
    if physical >= PHYSICAL_PAGES {
        return status::PHYSICAL_PAGE_OUT_OF_RANGE;
    }
    if logical == 0xFFFF {
        unmap(m, physical);
        return status::OK;
    }
    let Some(page) = storage_page(m, handle, logical) else { return status::LOGICAL_PAGE_OUT_OF_RANGE };
    if m.ems.mapped[physical] == Some((handle, logical)) {
        return status::OK;
    }
    if let Some(other) = (0..PHYSICAL_PAGES).find(|&p| p != physical && m.ems.mapped[p] == Some((handle, logical))) {
        // Aliasing: move the page instead of showing it twice.
        m.diagnostic("EMS logical page mapped into two physical pages at once (aliasing isn't emulated)".into());
        unmap(m, other);
    }
    unmap(m, physical);
    let data = m.ems.storage.remove(&page).unwrap_or_else(|| vec![0; PAGE_SIZE]);
    m.mem.load(frame_address(physical), &data);
    m.ems.mapped[physical] = Some((handle, logical));
    status::OK
}

fn restore_map(m: &mut Machine, saved: [Option<(u16, u16)>; PHYSICAL_PAGES]) -> u8 {
    for (physical, entry) in saved.into_iter().enumerate() {
        let code = match entry {
            Some((handle, logical)) => map_page(m, physical, handle, logical),
            None => {
                unmap(m, physical);
                status::OK
            }
        };
        if code != status::OK {
            return status::CONTEXT_ERROR;
        }
    }
    status::OK
}

/// AH=4Eh: get/set the whole mapping. The saved form is 4 × (handle, logical page) words, with
/// FFFFh for unmapped pages.
fn page_map(m: &mut Machine) -> u8 {
    let r = m.cpu.regs;
    const SIZE: u8 = (PHYSICAL_PAGES * 4) as u8;
    let save = |m: &mut Machine| {
        let base = Machine::lin(r.es(), r.di());
        for (i, entry) in m.ems.mapped.into_iter().enumerate() {
            let (handle, logical) = entry.unwrap_or((0xFFFF, 0xFFFF));
            m.mem.write_u16(base + i as u32 * 4, handle);
            m.mem.write_u16(base + i as u32 * 4 + 2, logical);
        }
    };
    let load = |m: &mut Machine| {
        let base = Machine::lin(r.ds(), r.si());
        let saved = std::array::from_fn(|i| {
            let handle = m.mem.read_u16(base + i as u32 * 4);
            let logical = m.mem.read_u16(base + i as u32 * 4 + 2);
            (handle != 0xFFFF).then_some((handle, logical))
        });
        restore_map(m, saved)
    };
    match r.al() {
        0 => {
            save(m);
            status::OK
        }
        1 => load(m),
        2 => {
            save(m);
            load(m)
        }
        3 => {
            m.cpu.regs.set_al(SIZE);
            status::OK
        }
        _ => status::UNDEFINED_SUBFUNCTION,
    }
}

/// AH=50h: map several pages from DS:SI, given by physical page number (AL=0) or segment (AL=1).
fn map_multiple(m: &mut Machine) -> u8 {
    let r = m.cpu.regs;
    if r.al() > 1 {
        return status::UNDEFINED_SUBFUNCTION;
    }
    let base = Machine::lin(r.ds(), r.si());
    for i in 0..u32::from(r.cx()) {
        let logical = m.mem.read_u16(base + i * 4);
        let target = m.mem.read_u16(base + i * 4 + 2);
        let physical = if r.al() == 0 {
            usize::from(target)
        } else {
            match target.checked_sub(FRAME_SEGMENT) {
                Some(delta) if delta % 0x400 == 0 => usize::from(delta / 0x400),
                _ => return status::PHYSICAL_PAGE_OUT_OF_RANGE,
            }
        };
        let code = map_page(m, physical, r.dx(), logical);
        if code != status::OK {
            return code;
        }
    }
    status::OK
}

/// AH=53h: get (AL=0, to ES:DI) or set (AL=1, from DS:SI) a handle's 8-byte name.
fn handle_name(m: &mut Machine) -> u8 {
    let r = m.cpu.regs;
    if !m.ems.handles.contains_key(&r.dx()) {
        return status::INVALID_HANDLE;
    }
    match r.al() {
        0 => {
            let name = m.ems.handles[&r.dx()].name;
            m.mem.load(Machine::lin(r.es(), r.di()), &name);
            status::OK
        }
        1 => {
            let bytes = m.mem.read_bytes(Machine::lin(r.ds(), r.si()), 8);
            let name: [u8; 8] = bytes.try_into().expect("8 bytes");
            if name != [0; 8] && m.ems.handles.iter().any(|(&id, h)| id != r.dx() && h.name == name) {
                return status::NAME_EXISTS;
            }
            m.ems.handles.get_mut(&r.dx()).expect("checked above").name = name;
            status::OK
        }
        _ => status::UNDEFINED_SUBFUNCTION,
    }
}

/// Where one byte of a move/exchange region lives.
enum Location {
    Memory(u32),
    Page(u16, usize),
}

/// The byte `index` bytes into a region described at `at` in a move/exchange request.
fn locate(m: &Machine, at: u32, index: u32) -> Result<Location, u8> {
    let kind = m.mem.read_u8(at);
    let handle = m.mem.read_u16(at + 1);
    let offset = u32::from(m.mem.read_u16(at + 3));
    let base = u32::from(m.mem.read_u16(at + 5));
    match kind {
        0 => Ok(Location::Memory((base << 4) + offset + index)),
        1 => {
            if offset >= PAGE_SIZE as u32 {
                return Err(status::OFFSET_OUT_OF_RANGE);
            }
            let position = base as usize * PAGE_SIZE + (offset + index) as usize;
            let logical = (position / PAGE_SIZE) as u16;
            let page = storage_page(m, handle, logical).ok_or(if m.ems.handles.contains_key(&handle) {
                status::REGION_TOO_LONG
            } else {
                status::INVALID_HANDLE
            })?;
            match m.ems.mapped.iter().position(|&e| e == Some((handle, logical))) {
                Some(physical) => Ok(Location::Memory(frame_address(physical) + (position % PAGE_SIZE) as u32)),
                None => Ok(Location::Page(page, position % PAGE_SIZE)),
            }
        }
        _ => Err(status::BAD_MEMORY_TYPE),
    }
}

fn read_location(m: &Machine, location: &Location) -> u8 {
    match *location {
        Location::Memory(addr) => m.mem.read_u8(addr),
        Location::Page(page, offset) => m.ems.storage.get(&page).map_or(0, |data| data[offset]),
    }
}

fn write_location(m: &mut Machine, location: &Location, value: u8) {
    match *location {
        Location::Memory(addr) => m.mem.write_u8(addr, value),
        Location::Page(page, offset) => {
            m.ems.storage.entry(page).or_insert_with(|| vec![0; PAGE_SIZE])[offset] = value;
        }
    }
}

/// AH=57h: move (AL=0) or exchange (AL=1) a region between conventional and expanded memory.
fn move_region(m: &mut Machine) -> u8 {
    let r = m.cpu.regs;
    if r.al() > 1 {
        return status::UNDEFINED_SUBFUNCTION;
    }
    let request = Machine::lin(r.ds(), r.si());
    let length = m.mem.read_u32(request);
    if length > 0x10_0000 {
        return 0x96; // region longer than 1 MB
    }
    let (source, dest) = (request + 4, request + 11);
    let mut from = Vec::with_capacity(length as usize);
    let mut to = Vec::with_capacity(length as usize);
    for i in 0..length {
        match (locate(m, source, i), locate(m, dest, i)) {
            (Ok(s), Ok(d)) => {
                from.push(s);
                to.push(d);
            }
            (Err(code), _) | (_, Err(code)) => return code,
        }
    }
    // Read everything first so overlapping regions behave like memmove.
    let source_bytes: Vec<u8> = from.iter().map(|l| read_location(m, l)).collect();
    if r.al() == 1 {
        let dest_bytes: Vec<u8> = to.iter().map(|l| read_location(m, l)).collect();
        for (location, value) in from.iter().zip(dest_bytes) {
            write_location(m, location, value);
        }
    }
    for (location, value) in to.iter().zip(source_bytes) {
        write_location(m, location, value);
    }
    status::OK
}
