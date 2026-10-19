//! MZ executable header parsing for the loader.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MzImage<'a> {
    /// The load image: everything after the header, up to the size the header declares.
    pub image: &'a [u8],
    /// (segment, offset) of each relocated word, relative to the load segment.
    pub relocations: Vec<(u16, u16)>,
    pub min_alloc: u16,
    pub max_alloc: u16,
    pub ss: u16,
    pub sp: u16,
    pub cs: u16,
    pub ip: u16,
}

pub fn is_mz(data: &[u8]) -> bool {
    data.len() >= 2 && (&data[..2] == b"MZ" || &data[..2] == b"ZM")
}

pub fn parse(data: &[u8]) -> Result<MzImage<'_>, String> {
    if data.len() < 0x1C || !is_mz(data) {
        return Err("not an MZ executable".into());
    }
    let word = |offset: usize| u16::from_le_bytes([data[offset], data[offset + 1]]);
    let (last_page, pages, reloc_count, header_paras) = (word(2), word(4), word(6), word(8));
    let reloc_table = usize::from(word(0x18));

    let mut end = usize::from(pages) * 512;
    if last_page != 0 {
        end = end.saturating_sub(512 - usize::from(last_page.min(512)));
    }
    let end = end.min(data.len());
    let header_size = usize::from(header_paras) * 16;
    if header_size > end {
        return Err(format!("header ({header_size} bytes) is larger than the image ({end} bytes)"));
    }

    let mut relocations = Vec::with_capacity(usize::from(reloc_count));
    for i in 0..usize::from(reloc_count) {
        let at = reloc_table + i * 4;
        if at + 4 > data.len() {
            return Err(format!("relocation table entry {i} is past the end of the file"));
        }
        relocations.push((word(at + 2), word(at)));
    }

    Ok(MzImage {
        image: &data[header_size..end],
        relocations,
        min_alloc: word(0x0A),
        max_alloc: word(0x0C),
        ss: word(0x0E),
        sp: word(0x10),
        ip: word(0x14),
        cs: word(0x16),
    })
}
