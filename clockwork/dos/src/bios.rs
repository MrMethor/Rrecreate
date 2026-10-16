//! BIOS: ROM stubs, interrupt vector table, BIOS data area and the BIOS services as
//! high-level emulation (PROJECT.md §5.2, "HLE services").
//!
//! Each service vector points at a small ROM stub: a host callback followed by real code
//! (`IRET`, `INT 1Ch`, the EOI to the PIC). Programs can hook and chain these vectors exactly as
//! on real hardware.

use cpu::flags::{CF, ZF};

use crate::clock::PIT_HZ;
use crate::keyboard::scancode_ascii;
use crate::machine::Machine;
use crate::vga::TEXT_BASE;

pub const ROM_SEGMENT: u16 = 0xF000;
const ROM_FONT_OFFSET: u16 = 0xA000;
/// Where `DOS` callback stubs and similar code start in the ROM.
const ROM_CODE_OFFSET: u16 = 0x0000;

pub const BDA_EQUIPMENT: u32 = 0x410;
pub const BDA_MEMORY_KB: u32 = 0x413;
pub const BDA_KBD_FLAGS: u32 = 0x417;
pub const BDA_KBD_HEAD: u32 = 0x41A;
pub const BDA_KBD_TAIL: u32 = 0x41C;
pub const BDA_VIDEO_MODE: u32 = 0x449;
pub const BDA_COLUMNS: u32 = 0x44A;
pub const BDA_PAGE_SIZE: u32 = 0x44C;
pub const BDA_PAGE_OFFSET: u32 = 0x44E;
pub const BDA_CURSOR: u32 = 0x450;
pub const BDA_CURSOR_SHAPE: u32 = 0x460;
pub const BDA_ACTIVE_PAGE: u32 = 0x462;
pub const BDA_CRTC_PORT: u32 = 0x463;
pub const BDA_TICKS: u32 = 0x46C;
pub const BDA_MIDNIGHT: u32 = 0x470;
pub const BDA_KBD_START: u32 = 0x480;
pub const BDA_KBD_END: u32 = 0x482;
pub const BDA_ROWS: u32 = 0x484;
pub const BDA_CHAR_HEIGHT: u32 = 0x485;

/// Keyboard buffer bounds as offsets from segment 0040h.
const KBD_BUFFER_START: u16 = 0x1E;
const KBD_BUFFER_END: u16 = 0x3E;
const TICKS_PER_DAY: u32 = 0x18_00B0;

/// Host callback ids. Interrupt services use their vector number.
pub(crate) mod callback {
    pub const TIMER_IRQ: u16 = 0x08;
    pub const KEYBOARD_IRQ: u16 = 0x09;
    pub const VIDEO: u16 = 0x10;
    pub const EQUIPMENT: u16 = 0x11;
    pub const MEMORY_SIZE: u16 = 0x12;
    pub const DISK: u16 = 0x13;
    pub const SYSTEM: u16 = 0x15;
    pub const KEYBOARD: u16 = 0x16;
    pub const TIME: u16 = 0x1A;
    pub const TERMINATE: u16 = 0x20;
    pub const DOS: u16 = 0x21;
    pub const FAST_CONSOLE: u16 = 0x29;
    pub const MULTIPLEX: u16 = 0x2F;
    pub const EMS: u16 = 0x67;
}

/// A device driver header for the emulated CD-ROM (MSCDEX AX=1501h points at it).
pub const ROM_CD_DEVICE: u16 = 0x9000;

struct Rom {
    next: u16,
}

impl Rom {
    fn emit(&mut self, m: &mut Machine, code: &[u8]) -> u16 {
        let at = self.next;
        m.mem.load(Machine::lin(ROM_SEGMENT, at), code);
        self.next += code.len() as u16;
        at
    }

    /// `[STI] callback id; IRET`
    fn service(&mut self, m: &mut Machine, id: u16, sti: bool) -> u16 {
        let mut code = Vec::with_capacity(6);
        if sti {
            code.push(0xFB);
        }
        code.extend(callback_instruction(id));
        code.push(0xCF);
        self.emit(m, &code)
    }
}

pub(crate) fn callback_instruction(id: u16) -> [u8; 4] {
    let [lo, hi] = id.to_le_bytes();
    [cpu::CALLBACK_OPCODE[0], cpu::CALLBACK_OPCODE[1], lo, hi]
}

fn set_vector(m: &mut Machine, vector: u8, segment: u16, offset: u16) {
    let at = u32::from(vector) * 4;
    m.mem.write_u16(at, offset);
    m.mem.write_u16(at + 2, segment);
}

pub(crate) fn install(m: &mut Machine) {
    let mut rom = Rom { next: ROM_CODE_OFFSET };
    let iret = rom.emit(m, &[0xCF]);
    for vector in 0..=255u8 {
        set_vector(m, vector, ROM_SEGMENT, iret);
    }

    // Timer IRQ: tick the BIOS clock, call the user hook INT 1Ch, then EOI.
    let mut timer = callback_instruction(callback::TIMER_IRQ).to_vec();
    timer.extend([0xCD, 0x1C, 0x50, 0xB0, 0x20, 0xE6, 0x20, 0x58, 0xCF]);
    let timer = rom.emit(m, &timer);
    set_vector(m, 0x08, ROM_SEGMENT, timer);

    let mut keyboard = callback_instruction(callback::KEYBOARD_IRQ).to_vec();
    keyboard.extend([0x50, 0xB0, 0x20, 0xE6, 0x20, 0x58, 0xCF]);
    let keyboard = rom.emit(m, &keyboard);
    set_vector(m, 0x09, ROM_SEGMENT, keyboard);

    // Other hardware IRQs: acknowledge and return.
    let eoi_master = rom.emit(m, &[0x50, 0xB0, 0x20, 0xE6, 0x20, 0x58, 0xCF]);
    let eoi_both = rom.emit(m, &[0x50, 0xB0, 0x20, 0xE6, 0xA0, 0xE6, 0x20, 0x58, 0xCF]);
    for vector in 0x0A..=0x0F {
        set_vector(m, vector, ROM_SEGMENT, eoi_master);
    }
    for vector in 0x70..=0x77 {
        set_vector(m, vector, ROM_SEGMENT, eoi_both);
    }

    for (vector, id, sti) in [
        (0x10, callback::VIDEO, false),
        (0x11, callback::EQUIPMENT, false),
        (0x12, callback::MEMORY_SIZE, false),
        (0x13, callback::DISK, false),
        (0x15, callback::SYSTEM, false),
        (0x16, callback::KEYBOARD, true),
        (0x1A, callback::TIME, false),
        (0x20, callback::TERMINATE, false),
        (0x21, callback::DOS, true),
        (0x29, callback::FAST_CONSOLE, false),
        (0x2F, callback::MULTIPLEX, false),
    ] {
        let at = rom.service(m, id, sti);
        set_vector(m, vector, ROM_SEGMENT, at);
    }

    // Critical-error handler: answer "fail" (AL=3).
    let critical = rom.emit(m, &[0xB0, 0x03, 0xCF]);
    set_vector(m, 0x24, ROM_SEGMENT, critical);

    // Font pointers (8×8 glyphs for INT 1Fh/43h): an empty font until one is added.
    set_vector(m, 0x1F, ROM_SEGMENT, ROM_FONT_OFFSET + 0x400);
    set_vector(m, 0x43, ROM_SEGMENT, ROM_FONT_OFFSET);

    let mut cd_device = vec![0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xC8, 0, 0, 0, 0];
    cd_device.extend(b"MSCD001 \0\0");
    m.mem.load(Machine::lin(ROM_SEGMENT, ROM_CD_DEVICE), &cd_device);

    m.mem.load(Machine::lin(ROM_SEGMENT, 0xFFF5), b"06/01/94");
    m.mem.load(Machine::lin(ROM_SEGMENT, 0xFFFE), &[0xFC]); // model byte: AT

    // BIOS data area.
    m.mem.write_u16(BDA_EQUIPMENT, 0x0021); // one floppy, 80×25 color
    m.mem.write_u16(BDA_MEMORY_KB, 640);
    m.mem.write_u16(BDA_KBD_HEAD, KBD_BUFFER_START);
    m.mem.write_u16(BDA_KBD_TAIL, KBD_BUFFER_START);
    m.mem.write_u16(BDA_KBD_START, KBD_BUFFER_START);
    m.mem.write_u16(BDA_KBD_END, KBD_BUFFER_END);
    m.mem.write_u16(BDA_CRTC_PORT, 0x3D4);
    let since_midnight = u64::from(m.config.start_time.seconds_since_midnight());
    m.mem.write_u32(BDA_TICKS, (since_midnight * PIT_HZ / 65536) as u32);
    set_video_mode(m, 3, true);
}

/// Handles a BIOS callback; returns false if `id` isn't one.
pub(crate) fn dispatch(m: &mut Machine, id: u16) -> bool {
    match id {
        callback::TIMER_IRQ => timer_irq(m),
        callback::KEYBOARD_IRQ => keyboard_irq(m),
        callback::VIDEO => video(m),
        callback::EQUIPMENT => {
            let equipment = m.mem.read_u16(BDA_EQUIPMENT);
            m.cpu.regs.set_ax(equipment);
        }
        callback::MEMORY_SIZE => {
            let kb = m.mem.read_u16(BDA_MEMORY_KB);
            m.cpu.regs.set_ax(kb);
        }
        callback::DISK => disk(m),
        callback::SYSTEM => system(m),
        callback::KEYBOARD => keyboard(m),
        callback::TIME => time(m),
        _ => return false,
    }
    true
}

fn timer_irq(m: &mut Machine) {
    let ticks = m.mem.read_u32(BDA_TICKS) + 1;
    if ticks >= TICKS_PER_DAY {
        m.mem.write_u32(BDA_TICKS, 0);
        m.mem.write_u8(BDA_MIDNIGHT, 1);
    } else {
        m.mem.write_u32(BDA_TICKS, ticks);
    }
}

// ---- keyboard ----

fn keyboard_irq(m: &mut Machine) {
    let code = m.devices.keyboard.read_data();
    m.events_changed();
    match code {
        0xE0 | 0xE1 => {
            m.keyboard_e0 = true;
            return;
        }
        // Controller replies, not keys. (AAh is also the self-test reply, but after boot it is
        // Left Shift's release code, so it isn't filtered.)
        0x00 | 0xEE | 0xFA | 0xFE | 0xFF => return,
        _ => {}
    }
    let extended = std::mem::take(&mut m.keyboard_e0);
    let make = code & 0x7F;
    let released = code & 0x80 != 0;
    let mut flags = m.mem.read_u8(BDA_KBD_FLAGS);

    let modifier = match make {
        0x36 => Some(0x01),
        0x2A => Some(0x02),
        0x1D => Some(0x04),
        0x38 => Some(0x08),
        _ => None,
    };
    if let Some(bit) = modifier {
        if extended && make == 0x2A {
            return; // fake shift sent around extended keys
        }
        flags = if released { flags & !bit } else { flags | bit };
        m.mem.write_u8(BDA_KBD_FLAGS, flags);
        return;
    }
    if released {
        return;
    }
    let toggle = match make {
        0x3A => Some(0x40),
        0x45 => Some(0x20),
        0x46 => Some(0x10),
        _ => None,
    };
    if let Some(bit) = toggle {
        m.mem.write_u8(BDA_KBD_FLAGS, flags ^ bit);
        return;
    }

    let (plain, shifted) = scancode_ascii(make);
    let shift = flags & 0x03 != 0;
    let mut ascii = if shift { shifted } else { plain };
    if flags & 0x40 != 0 && plain.is_ascii_lowercase() {
        ascii = if shift { plain } else { shifted };
    }
    if flags & 0x04 != 0 && plain.is_ascii_lowercase() {
        ascii = plain - 0x60;
    }
    if flags & 0x08 != 0 || extended {
        ascii = 0;
    }
    push_key(m, (u16::from(make) << 8) | u16::from(ascii));
}

fn keyboard(m: &mut Machine) {
    match m.cpu.regs.ah() {
        0x00 | 0x10 => match pop_key(m) {
            Some(key) => m.cpu.regs.set_ax(key),
            None => m.wait_for_input(),
        },
        0x01 | 0x11 => match peek_key(m) {
            Some(key) => {
                m.cpu.regs.set_ax(key);
                m.set_return_flag(ZF, false);
            }
            None => m.set_return_flag(ZF, true),
        },
        0x02 => {
            let flags = m.mem.read_u8(BDA_KBD_FLAGS);
            m.cpu.regs.set_al(flags);
        }
        0x12 => {
            let flags = m.mem.read_u8(BDA_KBD_FLAGS);
            m.cpu.regs.set_ax(u16::from(flags));
        }
        0x03 => {} // typematic rate
        0x05 => {
            let stored = push_key(m, m.cpu.regs.cx());
            m.cpu.regs.set_al(u8::from(!stored));
        }
        ah => m.diagnostic(format!("INT 16h AH={ah:02X}h is not implemented")),
    }
}

/// Appends a key word to the BIOS keyboard buffer; false if it is full.
pub(crate) fn push_key(m: &mut Machine, key: u16) -> bool {
    let tail = m.mem.read_u16(BDA_KBD_TAIL);
    let next = if tail + 2 >= KBD_BUFFER_END { KBD_BUFFER_START } else { tail + 2 };
    if next == m.mem.read_u16(BDA_KBD_HEAD) {
        return false;
    }
    m.mem.write_u16(0x400 + u32::from(tail), key);
    m.mem.write_u16(BDA_KBD_TAIL, next);
    true
}

pub(crate) fn peek_key(m: &Machine) -> Option<u16> {
    let head = m.mem.read_u16(BDA_KBD_HEAD);
    (head != m.mem.read_u16(BDA_KBD_TAIL)).then(|| m.mem.read_u16(0x400 + u32::from(head)))
}

pub(crate) fn pop_key(m: &mut Machine) -> Option<u16> {
    let key = peek_key(m)?;
    let head = m.mem.read_u16(BDA_KBD_HEAD);
    let next = if head + 2 >= KBD_BUFFER_END { KBD_BUFFER_START } else { head + 2 };
    m.mem.write_u16(BDA_KBD_HEAD, next);
    Some(key)
}

// ---- video ----

fn is_text_mode(mode: u8) -> bool {
    mode <= 3 || mode == 7
}

pub(crate) fn set_video_mode(m: &mut Machine, mode: u8, clear: bool) {
    let supported = is_text_mode(mode) || mode == 0x13;
    if supported {
        m.devices.vga.set_mode(mode);
    } else {
        m.devices.vga.mode = mode;
        m.diagnostic(format!("video mode {mode:02X}h is not emulated; its frames are raw memory hashes"));
    }
    let text = is_text_mode(mode);
    let columns: u16 = if mode < 2 {
        40
    } else if text {
        80
    } else if mode == 0x13 {
        40
    } else {
        80
    };
    m.mem.write_u8(BDA_VIDEO_MODE, mode);
    m.mem.write_u16(BDA_COLUMNS, columns);
    m.mem.write_u16(BDA_PAGE_SIZE, if text { columns * 25 * 2 } else { 0 });
    m.mem.write_u16(BDA_PAGE_OFFSET, 0);
    for page in 0..8 {
        m.mem.write_u16(BDA_CURSOR + page * 2, 0);
    }
    m.mem.write_u16(BDA_CURSOR_SHAPE, 0x0607);
    m.mem.write_u8(BDA_ACTIVE_PAGE, 0);
    m.mem.write_u8(BDA_ROWS, 24);
    m.mem.write_u8(BDA_CHAR_HEIGHT, if text { 16 } else { 8 });
    if clear {
        if text {
            for i in 0..0x4000u32 {
                m.mem.write_u16(TEXT_BASE + i * 2, 0x0720);
            }
        } else {
            for i in 0..0x1_0000u32 {
                m.mem.write_u8(0xA_0000 + i, 0);
            }
        }
    }
}

fn cursor(m: &Machine, page: u8) -> (u16, u16) {
    let at = BDA_CURSOR + u32::from(page & 7) * 2;
    (u16::from(m.mem.read_u8(at)), u16::from(m.mem.read_u8(at + 1)))
}

fn set_cursor(m: &mut Machine, page: u8, column: u16, row: u16) {
    let at = BDA_CURSOR + u32::from(page & 7) * 2;
    m.mem.write_u8(at, column as u8);
    m.mem.write_u8(at + 1, row as u8);
}

fn text_cell(m: &Machine, page: u8, column: u16, row: u16) -> u32 {
    let columns = u32::from(m.mem.read_u16(BDA_COLUMNS).max(1));
    let page_size = u32::from(m.mem.read_u16(BDA_PAGE_SIZE));
    TEXT_BASE + u32::from(page & 7) * page_size + (u32::from(row) * columns + u32::from(column)) * 2
}

/// A text-mode rectangle, inclusive, in character cells.
#[derive(Clone, Copy)]
struct Window {
    top: u16,
    left: u16,
    bottom: u16,
    right: u16,
}

/// Scrolls a text window up (or down) by `lines`; 0 clears it. Blank lines get `attribute`.
fn scroll(m: &mut Machine, window: Window, lines: u8, attribute: u8, up: bool) {
    let Window { top, left, bottom, right } = window;
    let columns = m.mem.read_u16(BDA_COLUMNS);
    let right = right.min(columns.saturating_sub(1));
    let bottom = bottom.min(24);
    if top > bottom || left > right {
        return;
    }
    let height = bottom - top + 1;
    let lines = if lines == 0 || u16::from(lines) > height { height } else { u16::from(lines) };
    let page = m.mem.read_u8(BDA_ACTIVE_PAGE);
    let blank = (u16::from(attribute) << 8) | 0x20;
    for i in 0..height {
        let row = if up { top + i } else { bottom - i };
        let source = if up { row + lines } else { row.wrapping_sub(lines) };
        let source_valid = if up { source <= bottom } else { i + lines < height };
        for column in left..=right {
            let value = if source_valid { m.mem.read_u16(text_cell(m, page, column, source)) } else { blank };
            m.mem.write_u16(text_cell(m, page, column, row), value);
        }
    }
}

/// BIOS teletype output: the path DOS console output takes too.
pub(crate) fn teletype(m: &mut Machine, ch: u8) {
    m.console.push(ch);
    let mode = m.devices.vga.mode;
    let text = is_text_mode(mode);
    let page = m.mem.read_u8(BDA_ACTIVE_PAGE);
    let columns = m.mem.read_u16(BDA_COLUMNS).max(1);
    let (mut column, mut row) = cursor(m, page);
    match ch {
        b'\r' => column = 0,
        b'\n' => row += 1,
        0x08 => column = column.saturating_sub(1),
        0x07 => {}
        _ => {
            if text {
                let cell = text_cell(m, page, column, row);
                m.mem.write_u8(cell, ch);
            } else {
                m.diagnostic("BIOS text output in graphics modes isn't drawn (no font yet)".into());
            }
            column += 1;
            if column >= columns {
                column = 0;
                row += 1;
            }
        }
    }
    if row > 24 {
        if text {
            scroll(m, Window { top: 0, left: 0, bottom: 24, right: columns - 1 }, 1, 0x07, true);
        }
        row = 24;
    }
    set_cursor(m, page, column, row);
}

fn video(m: &mut Machine) {
    let r = m.cpu.regs;
    let mode = m.devices.vga.mode;
    match r.ah() {
        0x00 => set_video_mode(m, r.al() & 0x7F, r.al() & 0x80 == 0),
        0x01 => m.mem.write_u16(BDA_CURSOR_SHAPE, r.cx()),
        0x02 => set_cursor(m, r.bh(), u16::from(r.dl()), u16::from(r.dh())),
        0x03 => {
            let (column, row) = cursor(m, r.bh());
            m.cpu.regs.set_dx((row << 8) | column);
            let shape = m.mem.read_u16(BDA_CURSOR_SHAPE);
            m.cpu.regs.set_cx(shape);
        }
        0x05 => {
            m.mem.write_u8(BDA_ACTIVE_PAGE, r.al() & 7);
            let page_size = m.mem.read_u16(BDA_PAGE_SIZE);
            m.mem.write_u16(BDA_PAGE_OFFSET, u16::from(r.al() & 7).wrapping_mul(page_size));
        }
        0x06 | 0x07 if is_text_mode(mode) => {
            let window = Window {
                top: u16::from(r.ch()),
                left: u16::from(r.cl()),
                bottom: u16::from(r.dh()),
                right: u16::from(r.dl()),
            };
            scroll(m, window, r.al(), r.bh(), r.ah() == 0x06);
        }
        0x08 if is_text_mode(mode) => {
            let (column, row) = cursor(m, r.bh());
            let cell = m.mem.read_u16(text_cell(m, r.bh(), column, row));
            m.cpu.regs.set_ax(cell);
        }
        0x09 | 0x0A if is_text_mode(mode) => {
            let (column, row) = cursor(m, r.bh());
            let mut at = text_cell(m, r.bh(), column, row);
            for _ in 0..r.cx() {
                m.mem.write_u8(at, r.al());
                if r.ah() == 0x09 {
                    m.mem.write_u8(at + 1, r.bl());
                }
                at += 2;
            }
        }
        0x0C if mode == 0x13 => {
            if r.cx() < 320 && r.dx() < 200 {
                m.mem.write_u8(0xA_0000 + u32::from(r.dx()) * 320 + u32::from(r.cx()), r.al());
            }
        }
        0x0D if mode == 0x13 => {
            let pixel = if r.cx() < 320 && r.dx() < 200 {
                m.mem.read_u8(0xA_0000 + u32::from(r.dx()) * 320 + u32::from(r.cx()))
            } else {
                0
            };
            m.cpu.regs.set_al(pixel);
        }
        0x0E => teletype(m, r.al()),
        0x0F => {
            let columns = m.mem.read_u16(BDA_COLUMNS) as u8;
            let page = m.mem.read_u8(BDA_ACTIVE_PAGE);
            m.cpu.regs.set_ax((u16::from(columns) << 8) | u16::from(mode));
            m.cpu.regs.set_bh(page);
        }
        0x10 => palette(m),
        0x11 if r.al() == 0x30 => {
            m.cpu.regs.set_es(ROM_SEGMENT);
            m.cpu.regs.set_bp(ROM_FONT_OFFSET);
            m.cpu.regs.set_cx(u16::from(m.mem.read_u8(BDA_CHAR_HEIGHT)));
            m.cpu.regs.set_dl(24);
        }
        0x12 if r.bl() == 0x10 => {
            m.cpu.regs.set_bx(0x0003); // color, 256 KB
            m.cpu.regs.set_cx(0x0009);
        }
        0x13 => {
            let (column, row) = (u16::from(r.dl()), u16::from(r.dh()));
            set_cursor(m, r.bh(), column, row);
            for i in 0..r.cx() {
                let ch = m.mem.read_u8(Machine::lin(r.es(), r.bp().wrapping_add(i)));
                teletype(m, ch);
            }
            if r.al() & 1 == 0 {
                set_cursor(m, r.bh(), column, row);
            }
        }
        0x1A if r.al() == 0x00 => {
            m.cpu.regs.set_al(0x1A);
            m.cpu.regs.set_bx(0x0008); // VGA with analog color display
        }
        ah => m.diagnostic(format!("INT 10h AH={ah:02X}h AL={:02X}h in mode {mode:02X}h is not implemented", r.al())),
    }
}

fn palette(m: &mut Machine) {
    let r = m.cpu.regs;
    let vga = &mut m.devices.vga;
    match r.al() {
        0x00 => {
            if let Some(reg) = vga.attr.get_mut(usize::from(r.bl())) {
                *reg = r.bh();
            }
        }
        0x01 => vga.attr[0x11] = r.bh(),
        0x02 => {
            for i in 0..17u16 {
                vga.attr[usize::from(i)] = m.mem.read_u8(Machine::lin(r.es(), r.dx().wrapping_add(i)));
            }
        }
        0x03 => {}
        0x07 => {
            let value = vga.attr.get(usize::from(r.bl())).copied().unwrap_or(0);
            m.cpu.regs.set_bh(value);
        }
        0x10 => vga.dac[usize::from(r.bl())] = [r.dh() & 0x3F, r.ch() & 0x3F, r.cl() & 0x3F],
        0x12 => {
            for i in 0..r.cx() {
                let at = Machine::lin(r.es(), r.dx().wrapping_add(i * 3));
                let index = usize::from(r.bx().wrapping_add(i) as u8);
                vga.dac[index] = [0, 1, 2].map(|c| m.mem.read_u8(at + c) & 0x3F);
            }
        }
        0x15 => {
            let [red, green, blue] = vga.dac[usize::from(r.bl())];
            m.cpu.regs.set_dh(red);
            m.cpu.regs.set_ch(green);
            m.cpu.regs.set_cl(blue);
        }
        0x17 => {
            for i in 0..r.cx() {
                let color = m.devices.vga.dac[usize::from(r.bx().wrapping_add(i) as u8)];
                let at = Machine::lin(r.es(), r.dx().wrapping_add(i * 3));
                for (c, value) in color.into_iter().enumerate() {
                    m.mem.write_u8(at + c as u32, value);
                }
            }
        }
        0x1A => m.cpu.regs.set_bx(0),
        al => m.diagnostic(format!("INT 10h AX=10{al:02X}h is not implemented")),
    }
}

// ---- other services ----

fn disk(m: &mut Machine) {
    // No disk drives are emulated: programs only see DOS files on C:.
    match m.cpu.regs.ah() {
        0x00 => {
            m.cpu.regs.set_ah(0);
            m.set_return_flag(CF, false);
        }
        0x15 => {
            m.cpu.regs.set_ah(0); // no such drive
            m.set_return_flag(CF, false);
        }
        _ => {
            m.cpu.regs.set_ah(0x01);
            m.set_return_flag(CF, true);
        }
    }
}

fn system(m: &mut Machine) {
    match m.cpu.regs.ah() {
        0x4F => m.set_return_flag(CF, true), // keyboard intercept: process the key
        0x88 => {
            m.cpu.regs.set_ax(0); // no extended memory
            m.set_return_flag(CF, false);
        }
        0x24 => {
            let al = m.cpu.regs.al();
            match al {
                0 | 1 => m.mem.set_a20(al == 1),
                2 => {
                    let a20 = m.mem.a20();
                    m.cpu.regs.set_al(u8::from(a20));
                }
                _ => {}
            }
            m.cpu.regs.set_ah(0);
            m.set_return_flag(CF, false);
        }
        0x86 => {
            m.diagnostic("INT 15h AH=86h (wait) returns immediately".into());
            m.set_return_flag(CF, false);
        }
        _ => {
            m.cpu.regs.set_ah(0x86);
            m.set_return_flag(CF, true);
        }
    }
}

fn bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

fn time(m: &mut Machine) {
    let now = crate::dos::wall_clock(m);
    match m.cpu.regs.ah() {
        0x00 => {
            let ticks = m.mem.read_u32(BDA_TICKS);
            let midnight = m.mem.read_u8(BDA_MIDNIGHT);
            m.mem.write_u8(BDA_MIDNIGHT, 0);
            m.cpu.regs.set_cx((ticks >> 16) as u16);
            m.cpu.regs.set_dx(ticks as u16);
            m.cpu.regs.set_al(midnight);
        }
        0x01 => {
            let ticks = (u32::from(m.cpu.regs.cx()) << 16) | u32::from(m.cpu.regs.dx());
            m.mem.write_u32(BDA_TICKS, ticks);
        }
        0x02 => {
            m.cpu.regs.set_ch(bcd(now.hour));
            m.cpu.regs.set_cl(bcd(now.minute));
            m.cpu.regs.set_dh(bcd(now.second));
            m.cpu.regs.set_dl(0);
            m.set_return_flag(CF, false);
        }
        0x04 => {
            m.cpu.regs.set_ch(bcd((now.year / 100) as u8));
            m.cpu.regs.set_cl(bcd((now.year % 100) as u8));
            m.cpu.regs.set_dh(bcd(now.month));
            m.cpu.regs.set_dl(bcd(now.day));
            m.set_return_flag(CF, false);
        }
        0x03 | 0x05 => m.set_return_flag(CF, false),
        ah => m.diagnostic(format!("INT 1Ah AH={ah:02X}h is not implemented")),
    }
}
