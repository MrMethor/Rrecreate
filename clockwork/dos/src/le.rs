//! LE executables, the linear executable format of DOS extenders (DOS/4GW, CauseWay, PMODE/W): what
//! the file says about its objects, and where an extender's loader put them in memory.
//!
//! The format is IBM's "LX - Linear eXecutable Module Format" (LE, its earlier form, has 4-byte page
//! table entries and a last page's size). Only what placing the objects needs is read: the object
//! table, the page table, the data pages and the fixups that refer to the module's own objects.
//!
//! An extender loads each object at a linear address of its choosing and applies the fixups. To find
//! where, the code object (the entry point's) is searched for in memory: its bytes match the file's
//! except at fixup sites, and its fixups that refer to itself hold its own base, which a copy of the
//! file left in a buffer doesn't. The other objects' bases follow from the fixups that refer to them.

use crate::machine::Machine;

/// One object (a segment of the flat program): its size, flags and the bytes the file gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub size: u32,
    /// LE object flags: 1 readable, 2 writable, 4 executable.
    pub flags: u32,
    /// The bytes its pages hold in the file, from its start (the rest is zeros).
    pub data: Vec<u8>,
    /// Its fixups that refer to the module's own objects.
    pub fixups: Vec<Fixup>,
}

/// A place in an object that the loader sets to an address in an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fixup {
    /// Where, in the object.
    pub offset: u32,
    /// Bytes set: 2 or 4.
    pub width: u8,
    /// Relative to the end of the field (a near call or jump), not absolute.
    pub relative: bool,
    /// The object (index from 0) and offset it refers to.
    pub target: usize,
    pub target_offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Le {
    pub objects: Vec<Object>,
    /// The entry point: an object (index from 0) and the offset in it.
    pub entry: (usize, u32),
}

impl Le {
    /// The LE part of an executable, or `None` if it has none.
    pub fn parse(file: &[u8]) -> Result<Option<Le>, String> {
        let u16_at = |at: usize| file.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let u32_at = |at: usize| file.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        if !file.starts_with(b"MZ") {
            return Ok(None);
        }
        let Some(header) = u32_at(0x3C).map(|h| h as usize) else { return Ok(None) };
        match file.get(header..header + 2) {
            Some(b"LE") => {}
            Some(b"LX") => return Err("an LX executable: only LE is read so far".into()),
            _ => return Ok(None),
        }
        let field = |offset: usize| u32_at(header + offset).ok_or("a truncated LE header");
        let pages = field(0x14)? as usize;
        let entry = (field(0x18)?.checked_sub(1).ok_or("no entry object")? as usize, field(0x1C)?);
        let page_size = field(0x28)? as usize;
        let last_page = field(0x2C)? as usize;
        let object_table = header + field(0x40)? as usize;
        let object_count = field(0x44)? as usize;
        let page_table = header + field(0x48)? as usize;
        let fixup_pages = header + field(0x68)? as usize;
        let fixup_records = header + field(0x6C)? as usize;
        let data_pages = field(0x80)? as usize;
        // Page `n` (from 1) of the file: its bytes.
        let page = |n: usize| -> Result<&[u8], String> {
            let entry = file.get(page_table + 4 * (n - 1)..page_table + 4 * n).ok_or("a truncated page table")?;
            let number = (usize::from(entry[0]) << 16) | (usize::from(entry[1]) << 8) | usize::from(entry[2]);
            if number == 0 {
                return Ok(&[]);
            }
            let start = data_pages + (number - 1) * page_size;
            let len = if number == pages { last_page } else { page_size };
            Ok(file.get(start..start + len).unwrap_or(&file[start.min(file.len())..]))
        };
        let mut objects = Vec::new();
        let mut page_object = vec![(usize::MAX, 0usize); pages + 1];
        for i in 0..object_count {
            let at = object_table + 24 * i;
            let word = |o: usize| u32_at(at + o).ok_or("a truncated object table");
            let (size, flags, first, count) = (word(0)?, word(8)?, word(12)? as usize, word(16)? as usize);
            let mut data = Vec::new();
            for (k, n) in (first..first + count).enumerate() {
                if let Some(slot) = page_object.get_mut(n) {
                    *slot = (i, k);
                }
                data.resize(k * page_size, 0);
                data.extend_from_slice(page(n)?);
            }
            data.truncate(size as usize);
            objects.push(Object { size, flags, data, fixups: Vec::new() });
        }
        // The fixups of each page, which belongs to an object.
        for (n, &(object, index)) in page_object.iter().enumerate().skip(1) {
            let (Some(start), Some(end)) = (u32_at(fixup_pages + 4 * (n - 1)), u32_at(fixup_pages + 4 * n)) else {
                break;
            };
            if object == usize::MAX {
                continue;
            }
            let base = (index * page_size) as i64;
            let mut at = fixup_records + start as usize;
            let end = fixup_records + end as usize;
            while at < end {
                let read = |at: &mut usize, wide: bool| -> Result<u32, String> {
                    let value = if wide { u32_at(*at) } else { u16_at(*at).map(u32::from) };
                    *at += if wide { 4 } else { 2 };
                    value.ok_or_else(|| "a truncated fixup".to_owned())
                };
                let (source, flags) =
                    (*file.get(at).ok_or("a truncated fixup")?, *file.get(at + 1).ok_or("a truncated fixup")?);
                at += 2;
                let list = source & 0x20 != 0;
                let (mut sources, count) = if list {
                    at += 1;
                    (Vec::new(), usize::from(file[at - 1]))
                } else {
                    (vec![i64::from(read(&mut at, false)? as i16)], 0)
                };
                let wide_object = flags & 0x40 != 0;
                let (target, target_offset) = match flags & 3 {
                    0 => {
                        let target = if wide_object {
                            read(&mut at, false)?
                        } else {
                            at += 1;
                            u32::from(file[at - 1])
                        };
                        let offset = if source & 0x0F != 0x02 { read(&mut at, flags & 0x10 != 0)? } else { 0 };
                        (Some(target as usize), offset)
                    }
                    kind => {
                        // An import (by ordinal or name) or an entry: skipped, read past.
                        at += if wide_object { 2 } else { 1 };
                        match kind {
                            1 => {
                                at += if flags & 0x80 != 0 {
                                    1
                                } else if flags & 0x10 != 0 {
                                    4
                                } else {
                                    2
                                }
                            }
                            2 => at += if flags & 0x10 != 0 { 4 } else { 2 },
                            _ => {}
                        }
                        (None, 0)
                    }
                };
                if flags & 0x04 != 0 {
                    at += if flags & 0x20 != 0 { 4 } else { 2 };
                }
                if list {
                    for _ in 0..count {
                        sources.push(i64::from(read(&mut at, false)? as i16));
                    }
                }
                let (width, relative) = match source & 0x0F {
                    0x05 => (2, false),
                    0x07 => (4, false),
                    0x08 => (4, true),
                    _ => (0, false),
                };
                if let Some(target) = target.and_then(|t| t.checked_sub(1)).filter(|_| width > 0) {
                    for offset in sources {
                        if let Ok(offset) = u32::try_from(base + offset) {
                            objects[object].fixups.push(Fixup { offset, width, relative, target, target_offset });
                        }
                    }
                }
            }
        }
        Ok(Some(Le { objects, entry }))
    }

    /// Where the loader put each object (linear addresses), if the code object is in memory now.
    pub fn bases(&self, m: &Machine) -> Option<Vec<u32>> {
        let (code, entry) = self.entry;
        let object = self.objects.get(code)?;
        let fixed = fixed_bytes(object);
        // The bytes at the entry point (those no fixup sets), searched for in what is mapped.
        let anchor: Vec<(usize, u8)> = (entry as usize..object.data.len().min(entry as usize + 32))
            .filter(|&i| !fixed[i])
            .map(|i| (i - entry as usize, object.data[i]))
            .collect();
        if anchor.len() < 8 {
            return None;
        }
        for (start, bytes) in mapped(m) {
            for at in 0..bytes.len().saturating_sub(32) {
                if anchor.iter().any(|&(i, b)| bytes[at + i] != b) {
                    continue;
                }
                let base = (start + at as u32).wrapping_sub(entry);
                if let Some(bases) = self.placed(m, code, base) {
                    return Some(bases);
                }
            }
        }
        None
    }

    /// The objects' bases if the code object at `base` is the loaded one: its bytes are the file's
    /// but at fixup sites (and a few the program may have written since: runtimes keep variables in
    /// their code), its fixups to itself hold `base`, and the others' bases are what its fixups to
    /// them hold.
    fn placed(&self, m: &Machine, code: usize, base: u32) -> Option<Vec<u32>> {
        let object = &self.objects[code];
        let fixed = fixed_bytes(object);
        let byte = |at: u32| m.peek_linear_u8(at);
        let (mut changed, allowed) = (0, 16 + object.data.len() / 1024);
        for (i, &b) in object.data.iter().enumerate() {
            if !fixed[i] && byte(base.wrapping_add(i as u32))? != b {
                changed += 1;
                if changed > allowed {
                    return None;
                }
            }
        }
        let word = |at: u32| -> Option<u32> {
            Some(u32::from_le_bytes([byte(at)?, byte(at + 1)?, byte(at + 2)?, byte(at + 3)?]))
        };
        let mut bases: Vec<Option<u32>> = vec![None; self.objects.len()];
        bases[code] = Some(base);
        let mut checked = 0;
        for f in object.fixups.iter().filter(|f| f.width == 4 && !f.relative) {
            let target = word(base.wrapping_add(f.offset))?.wrapping_sub(f.target_offset);
            match bases[f.target] {
                Some(known) if known != target => return None,
                Some(_) => checked += 1,
                None => bases[f.target] = Some(target),
            }
        }
        // A program without fixups to its own code can't be told from a copy of the file.
        let self_fixups = object.fixups.iter().filter(|f| f.width == 4 && !f.relative && f.target == code).count();
        if self_fixups > 0 && checked == 0 {
            return None;
        }
        bases.into_iter().collect()
    }
}

/// Which bytes of an object its fixups set.
fn fixed_bytes(object: &Object) -> Vec<bool> {
    let mut fixed = vec![false; object.data.len()];
    for f in &object.fixups {
        for i in 0..usize::from(f.width) {
            if let Some(b) = fixed.get_mut(f.offset as usize + i) {
                *b = true;
            }
        }
    }
    fixed
}

/// The linear memory mapped now, as runs of consecutive present pages: (start, bytes).
fn mapped(m: &Machine) -> Vec<(u32, Vec<u8>)> {
    let pages: Vec<u32> = if m.cpu.cr0 & 0x8000_0001 == 0x8000_0001 {
        let read = |at: u32| m.mem.read_u32(at);
        let mut pages = Vec::new();
        for directory in 0..1024u32 {
            let pde = read((m.cpu.cr3 & !0xFFF) | (directory << 2));
            if pde & 1 == 0 {
                continue;
            }
            for table in 0..1024u32 {
                if read((pde & !0xFFF) | (table << 2)) & 1 != 0 {
                    pages.push((directory << 22) | (table << 12));
                }
            }
        }
        pages
    } else {
        (0..m.mem.size() / 4096).map(|p| p << 12).collect()
    };
    let mut runs: Vec<(u32, Vec<u8>)> = Vec::new();
    for page in pages {
        let bytes = (0..4096).map(|i| m.peek_linear_u8(page + i).unwrap_or(0));
        match runs.last_mut() {
            Some((start, run)) if start.wrapping_add(run.len() as u32) == page => run.extend(bytes),
            _ => runs.push((page, bytes.collect())),
        }
    }
    runs
}
