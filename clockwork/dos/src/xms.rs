//! XMS 3.0 (HIMEM.SYS) as high-level emulation: extended memory blocks, the HMA and the A20
//! line, reached through INT 2Fh AX=4300h/4310h and a far-call entry point.
//!
//! Ported from DOSBox Staging `src/ints/xms.cpp` (GPL-2.0-or-later): function numbers, error
//! codes, A20 counting and the move rules follow it. Blocks are contiguous runs of 4 KB pages
//! in [`crate::memory::Memory`]'s extended memory, so a locked block's address is its real physical
//! address. Unlike DOSBox Staging, the HMA can be requested (DOS isn't loaded high here).
//! Upper memory blocks (functions 10h-12h) aren't provided: programs are told "no UMBs".

use std::collections::BTreeMap;

use crate::bios::callback;
use crate::machine::Machine;

/// Where the driver's entry point lives: a hookable stub (short jump, three NOPs, callback,
/// RETF) in the DOS area below the first memory block, so programs can patch it.
pub const ENTRY_SEGMENT: u16 = 0x00F0;
pub const ENTRY_OFFSET: u16 = 0x0000;

/// Extended memory blocks start above the HMA.
pub const POOL_START: u32 = 0x11_0000;
const PAGE: u32 = 4096;
const HANDLES: u16 = 128;
const VERSION: u16 = 0x0300;
const DRIVER_VERSION: u16 = 0x0301;

mod error {
    pub const NOT_IMPLEMENTED: u8 = 0x80;
    pub const A20: u8 = 0x82;
    pub const HMA_IN_USE: u8 = 0x91;
    pub const HMA_NOT_ALLOCATED: u8 = 0x93;
    pub const A20_STILL_ENABLED: u8 = 0x94;
    pub const OUT_OF_SPACE: u8 = 0xA0;
    pub const OUT_OF_HANDLES: u8 = 0xA1;
    pub const INVALID_HANDLE: u8 = 0xA2;
    pub const INVALID_SOURCE_HANDLE: u8 = 0xA3;
    pub const INVALID_SOURCE_OFFSET: u8 = 0xA4;
    pub const INVALID_DEST_HANDLE: u8 = 0xA5;
    pub const INVALID_DEST_OFFSET: u8 = 0xA6;
    pub const INVALID_LENGTH: u8 = 0xA7;
    pub const PARITY: u8 = 0xA9;
    pub const NOT_LOCKED: u8 = 0xAA;
    pub const LOCKED: u8 = 0xAB;
    pub const LOCK_OVERFLOW: u8 = 0xAC;
    pub const NO_UMBS: u8 = 0xB1;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
struct Block {
    first_page: u32,
    pages: u32,
    size_kb: u32,
    locks: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub struct Xms {
    enabled: bool,
    /// Page after the last page of memory.
    end_page: u32,
    blocks: BTreeMap<u16, Block>,
    a20_global: bool,
    a20_count: u32,
    hma_taken: bool,
}

fn pages_for(size_kb: u32) -> u32 {
    size_kb.div_ceil(4)
}

impl Xms {
    /// A driver managing physical memory from [`POOL_START`] up to `memory_size` bytes.
    pub fn new(enabled: bool, memory_size: u32) -> Self {
        Self { enabled, end_page: memory_size / PAGE, ..Self::default() }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Free runs of pages between the blocks, as (first page, length), excluding `except`.
    fn gaps(&self, except: Option<u16>) -> Vec<(u32, u32)> {
        let mut used: Vec<(u32, u32)> = self
            .blocks
            .iter()
            .filter(|&(&h, b)| Some(h) != except && b.pages > 0)
            .map(|(_, b)| (b.first_page, b.pages))
            .collect();
        used.sort_unstable();
        let mut gaps = Vec::new();
        let mut at = POOL_START / PAGE;
        for (first, pages) in used {
            if first > at {
                gaps.push((at, first - at));
            }
            at = at.max(first + pages);
        }
        if self.end_page > at {
            gaps.push((at, self.end_page - at));
        }
        gaps
    }

    /// Free memory: (largest block, total), in KB.
    pub fn free_kb(&self) -> (u32, u32) {
        let gaps = self.gaps(None);
        let largest = gaps.iter().map(|&(_, n)| n).max().unwrap_or(0);
        (largest * 4, gaps.iter().map(|&(_, n)| n).sum::<u32>() * 4)
    }

    /// First free run of at least `pages` pages.
    fn find(&self, pages: u32, except: Option<u16>) -> Option<u32> {
        self.gaps(except).into_iter().find(|&(_, n)| n >= pages).map(|(first, _)| first)
    }

    fn block(&self, handle: u16) -> Option<&Block> {
        self.blocks.get(&handle)
    }

    fn allocate(&mut self, size_kb: u32) -> Result<u16, u8> {
        let handle = (1..HANDLES).find(|h| !self.blocks.contains_key(h)).ok_or(error::OUT_OF_HANDLES)?;
        let pages = pages_for(size_kb);
        let first_page = if pages == 0 {
            self.find(0, None).unwrap_or(self.end_page)
        } else {
            self.find(pages, None).ok_or(error::OUT_OF_SPACE)?
        };
        self.blocks.insert(handle, Block { first_page, pages, size_kb, locks: 0 });
        Ok(handle)
    }

    fn free(&mut self, handle: u16) -> Result<(), u8> {
        let block = self.block(handle).ok_or(error::INVALID_HANDLE)?;
        if block.locks != 0 {
            return Err(error::LOCKED);
        }
        self.blocks.remove(&handle);
        Ok(())
    }

    /// Physical address of `offset` in `handle`'s block, checking `length` fits.
    fn address(&self, handle: u16, offset: u32, length: u32, codes: (u8, u8)) -> Result<u32, u8> {
        let block = self.block(handle).ok_or(codes.0)?;
        let size = block.size_kb * 1024;
        if offset >= size {
            return Err(codes.1);
        }
        if length > size - offset {
            return Err(error::INVALID_LENGTH);
        }
        Ok(block.first_page * PAGE + offset)
    }
}

/// For the DPMI host: a block of at least `bytes`, locked (it doesn't move); its handle and
/// physical address.
pub(crate) fn allocate_locked(m: &mut Machine, bytes: u32) -> Result<(u16, u32), u8> {
    let handle = m.xms.allocate(bytes.div_ceil(1024).max(1))?;
    let block = m.xms.blocks.get_mut(&handle).expect("just allocated");
    block.locks = 1;
    Ok((handle, block.first_page * PAGE))
}

pub(crate) fn free_locked(m: &mut Machine, handle: u16) {
    if let Some(block) = m.xms.blocks.get_mut(&handle) {
        block.locks = 0;
    }
    let _ = m.xms.free(handle);
}

/// Resizes a locked block (it may move); its new physical address.
pub(crate) fn resize_locked(m: &mut Machine, handle: u16, bytes: u32) -> Result<u32, u8> {
    m.xms.blocks.get_mut(&handle).ok_or(error::INVALID_HANDLE)?.locks = 0;
    let result = resize(m, handle, bytes.div_ceil(1024).max(1));
    let block = m.xms.blocks.get_mut(&handle).expect("still there");
    block.locks = 1;
    result.map(|()| block.first_page * PAGE)
}

pub(crate) fn install(m: &mut Machine) {
    if !m.xms.enabled() {
        return;
    }
    let mut stub = vec![0xEB, 0x03, 0x90, 0x90, 0x90];
    stub.extend(crate::bios::callback_instruction(callback::XMS));
    stub.push(0xCB); // RETF
    m.mem.load(Machine::lin(ENTRY_SEGMENT, ENTRY_OFFSET), &stub);
}

/// INT 2Fh AH=43h: installation check and entry point. Returns whether it was an XMS call.
pub(crate) fn multiplex(m: &mut Machine) -> bool {
    if !m.xms.enabled() {
        return false;
    }
    match m.cpu.regs.ax() {
        0x4300 => m.cpu.regs.set_al(0x80),
        0x4310 => {
            m.cpu.regs.set_es(ENTRY_SEGMENT);
            m.cpu.regs.set_bx(ENTRY_OFFSET);
        }
        _ => return false,
    }
    true
}

/// AX = 1 on success; on failure AX = 0 and BL = the error code.
fn finish(m: &mut Machine, result: Result<(), u8>) {
    match result {
        Ok(()) => {
            m.cpu.regs.set_ax(1);
            m.cpu.regs.set_bl(0);
        }
        Err(code) => {
            m.cpu.regs.set_ax(0);
            m.cpu.regs.set_bl(code);
        }
    }
}

fn a20_local_enable(m: &mut Machine) -> Result<(), u8> {
    if m.xms.a20_count == u32::MAX {
        return Err(error::A20);
    }
    m.xms.a20_count += 1;
    if m.xms.a20_count == 1 {
        m.set_a20(true);
    }
    Ok(())
}

fn a20_local_disable(m: &mut Machine) -> Result<(), u8> {
    if m.xms.a20_count == 0 {
        return Err(error::A20);
    }
    m.xms.a20_count -= 1;
    if m.xms.a20_count != 0 {
        return Err(error::A20_STILL_ENABLED);
    }
    m.set_a20(false);
    Ok(())
}

/// Function 0Bh: the move structure at DS:SI.
fn move_block(m: &mut Machine) -> Result<(), u8> {
    let at = Machine::lin(m.cpu.regs.ds(), m.cpu.regs.si());
    let length = m.mem.read_u32(at);
    let (source_handle, source) = (m.mem.read_u16(at + 4), m.mem.read_u32(at + 6));
    let (dest_handle, dest) = (m.mem.read_u16(at + 10), m.mem.read_u32(at + 12));
    if length % 2 != 0 {
        return Err(error::PARITY);
    }
    // Handle 0 means a real-mode segment:offset pointer.
    let real = |pointer: u32| ((pointer >> 16) << 4) + (pointer & 0xFFFF);
    let source = if source_handle == 0 {
        let address = real(source);
        if address + length > 0x10_FFF0 {
            return Err(error::INVALID_LENGTH);
        }
        address
    } else {
        m.xms.address(source_handle, source, length, (error::INVALID_SOURCE_HANDLE, error::INVALID_SOURCE_OFFSET))?
    };
    let dest = if dest_handle == 0 {
        let address = real(dest);
        if address + length > 0x10_FFF0 {
            return Err(error::INVALID_LENGTH);
        }
        address
    } else {
        m.xms.address(dest_handle, dest, length, (error::INVALID_DEST_HANDLE, error::INVALID_DEST_OFFSET))?
    };
    m.mem.copy_phys(dest, source, length);
    Ok(())
}

fn resize(m: &mut Machine, handle: u16, size_kb: u32) -> Result<(), u8> {
    let block = m.xms.block(handle).ok_or(error::INVALID_HANDLE)?.clone();
    if block.locks > 0 {
        return Err(error::LOCKED);
    }
    let pages = pages_for(size_kb);
    // Grow or shrink in place when the pages after the block are free; otherwise move it.
    let in_place = m
        .xms
        .gaps(Some(handle))
        .iter()
        .any(|&(first, n)| first <= block.first_page && block.first_page + pages <= first + n);
    let first_page = if in_place || pages <= block.pages {
        block.first_page
    } else {
        let first = m.xms.find(pages, Some(handle)).ok_or(error::OUT_OF_SPACE)?;
        m.mem.copy_phys(first * PAGE, block.first_page * PAGE, block.pages.min(pages) * PAGE);
        first
    };
    m.xms.blocks.insert(handle, Block { first_page, pages, size_kb, ..block });
    Ok(())
}

pub(crate) fn service(m: &mut Machine) {
    let r = m.cpu.regs;
    match r.ah() {
        0x00 => {
            m.cpu.regs.set_ax(VERSION);
            m.cpu.regs.set_bx(DRIVER_VERSION);
            m.cpu.regs.set_dx(1); // the HMA exists
        }
        0x01 => {
            let result = if m.xms.hma_taken { Err(error::HMA_IN_USE) } else { Ok(()) };
            m.xms.hma_taken = true;
            finish(m, result);
        }
        0x02 => {
            let result = if m.xms.hma_taken { Ok(()) } else { Err(error::HMA_NOT_ALLOCATED) };
            m.xms.hma_taken = false;
            finish(m, result);
        }
        0x03 => {
            let result = if m.xms.a20_global {
                Ok(())
            } else {
                let result = a20_local_enable(m);
                m.xms.a20_global |= result.is_ok();
                result
            };
            finish(m, result);
        }
        0x04 => {
            let result = if m.xms.a20_global {
                let result = a20_local_disable(m);
                m.xms.a20_global &= result.is_err();
                result
            } else {
                Ok(())
            };
            finish(m, result);
        }
        0x05 => {
            let result = a20_local_enable(m);
            finish(m, result);
        }
        0x06 => {
            let result = a20_local_disable(m);
            finish(m, result);
        }
        0x07 => {
            let on = m.mem.a20();
            m.cpu.regs.set_ax(u16::from(on));
            m.cpu.regs.set_bl(0);
        }
        0x08 | 0x88 => {
            let (largest, total) = m.xms.free_kb();
            let code = if total == 0 { error::OUT_OF_SPACE } else { 0 };
            if r.ah() == 0x08 {
                m.cpu.regs.set_ax(largest.min(0xFFFF) as u16);
                m.cpu.regs.set_dx(total.min(0xFFFF) as u16);
            } else {
                m.cpu.regs.set_eax(largest);
                m.cpu.regs.set_edx(total);
                m.cpu.regs.set_ecx(m.mem.size() - 1);
            }
            m.cpu.regs.set_bl(code);
        }
        0x09 | 0x89 => {
            let size = if r.ah() == 0x09 { u32::from(r.dx()) } else { r.edx() };
            match m.xms.allocate(size) {
                Ok(handle) => {
                    finish(m, Ok(()));
                    m.cpu.regs.set_dx(handle);
                }
                Err(code) => {
                    finish(m, Err(code));
                    m.cpu.regs.set_dx(0);
                }
            }
        }
        0x0A => {
            let result = m.xms.free(r.dx());
            finish(m, result);
        }
        0x0B => {
            let result = move_block(m);
            // On success BL is left alone (HIMEM.SYS behavior, as in DOSBox).
            let bl = m.cpu.regs.bl();
            finish(m, result);
            if result.is_ok() {
                m.cpu.regs.set_bl(bl);
            }
        }
        0x0C => {
            let result = match m.xms.blocks.get_mut(&r.dx()) {
                None => Err(error::INVALID_HANDLE),
                Some(block) if block.locks == u8::MAX => Err(error::LOCK_OVERFLOW),
                Some(block) => {
                    block.locks += 1;
                    Ok(block.first_page * PAGE)
                }
            };
            finish(m, result.map(|_| ()));
            if let Ok(address) = result {
                m.cpu.regs.set_dx((address >> 16) as u16);
                m.cpu.regs.set_bx(address as u16);
            }
        }
        0x0D => {
            let result = match m.xms.blocks.get_mut(&r.dx()) {
                None => Err(error::INVALID_HANDLE),
                Some(block) if block.locks == 0 => Err(error::NOT_LOCKED),
                Some(block) => {
                    block.locks -= 1;
                    Ok(())
                }
            };
            finish(m, result);
        }
        0x0E | 0x8E => match m.xms.block(r.dx()).cloned() {
            None => finish(m, Err(error::INVALID_HANDLE)),
            Some(block) => {
                let free_handles = (1..HANDLES).filter(|h| !m.xms.blocks.contains_key(h)).count() as u8;
                m.cpu.regs.set_ax(1);
                m.cpu.regs.set_bh(block.locks);
                if r.ah() == 0x0E {
                    m.cpu.regs.set_bl(free_handles);
                    m.cpu.regs.set_edx(block.size_kb & 0xFFFF);
                } else {
                    m.cpu.regs.set_cx(u16::from(free_handles));
                    m.cpu.regs.set_edx(block.size_kb);
                }
            }
        },
        0x0F | 0x8F => {
            let size = if r.ah() == 0x0F { u32::from(r.bx()) } else { r.ebx() };
            let result = resize(m, r.dx(), size);
            finish(m, result);
        }
        0x10 => {
            finish(m, Err(error::NO_UMBS));
            m.cpu.regs.set_dx(0); // largest available UMB
        }
        0x11 | 0x12 => finish(m, Err(error::NOT_IMPLEMENTED)),
        ah => {
            m.diagnostic(format!("XMS function {ah:02X}h is not implemented"));
            finish(m, Err(error::NOT_IMPLEMENTED));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_finds_free_runs() {
        let mut xms = Xms::new(true, 2 << 20); // 2 MB: 960 KB above the HMA
        assert_eq!(xms.free_kb(), (960, 960));
        let a = xms.allocate(100).unwrap();
        let b = xms.allocate(200).unwrap();
        assert_eq!(xms.block(a).unwrap().first_page, 0x110);
        assert_eq!(xms.block(b).unwrap().first_page, 0x110 + 25);
        xms.free(a).unwrap();
        assert_eq!(xms.free_kb(), (960 - 300, 960 - 200));
        assert_eq!(xms.allocate(2000), Err(error::OUT_OF_SPACE));
        assert_eq!(xms.free(a), Err(error::INVALID_HANDLE));
    }
}
