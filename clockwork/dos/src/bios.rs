//! BIOS: ROM stubs, interrupt vector table, BIOS data area and the BIOS services as
//! high-level emulation (PROJECT.md §5.2, "HLE services").
//!
//! Each service vector points at a small ROM stub: a host callback followed by real code
//! (`IRET`, `INT 1Ch`, the EOI to the PIC). Programs can hook and chain these vectors exactly as
//! on real hardware.

use cpu::flags::{CF, ZF};

use crate::clock::PIT_HZ;
use crate::fonts;
use crate::keyboard::scancode_ascii;
use crate::machine::Machine;
use crate::vga::TEXT_BASE;

pub const ROM_SEGMENT: u16 = 0xF000;
/// ROM fonts (offsets in the BIOS segment). The first half of the 8×8 font is also at FA6Eh,
/// where the IBM BIOS keeps it and some programs expect it.
pub const ROM_FONT_8X8: u16 = 0xA000;
pub const ROM_FONT_8X14: u16 = 0xA800;
pub const ROM_FONT_9X14: u16 = 0xB600;
pub const ROM_FONT_8X16: u16 = 0xB800;
pub const ROM_FONT_9X16: u16 = 0xC800;
const ROM_FONT_8X8_IBM: u16 = 0xFA6E;
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
    pub const MOUSE: u16 = 0x33;
    pub const MOUSE_IRQ: u16 = 0x74;
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

    if m.mouse.enabled {
        let service = rom.service(m, callback::MOUSE, false);
        set_vector(m, 0x33, ROM_SEGMENT, service);
        // IRQ 12: save registers, let the driver set up the handler call (or skip it), call the
        // handler through the far pointer slot, restore, acknowledge both PICs.
        let mut stub = vec![0x60, 0x1E, 0x06];
        stub.extend(callback_instruction(callback::MOUSE_IRQ));
        let [lo, hi] = crate::mouse::HANDLER_SLOT.to_le_bytes();
        stub.extend([0x2E, 0xFF, 0x1E, lo, hi]);
        stub.extend([0x07, 0x1F, 0x61, 0x50, 0xB0, 0x20, 0xE6, 0xA0, 0xE6, 0x20, 0x58, 0xCF]);
        let irq = rom.emit(m, &stub);
        set_vector(m, crate::mouse::IRQ_VECTOR, ROM_SEGMENT, irq);
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

    // ROM fonts. INT 1Fh points at the upper half of the 8×8 font; INT 43h is set per video mode.
    for (offset, font) in [
        (ROM_FONT_8X8, &fonts::FONT_8X8[..]),
        (ROM_FONT_8X14, &fonts::FONT_8X14[..]),
        (ROM_FONT_9X14, &fonts::FONT_9X14_ALTERNATE[..]),
        (ROM_FONT_8X16, &fonts::FONT_8X16[..]),
        (ROM_FONT_9X16, &fonts::FONT_9X16_ALTERNATE[..]),
        (ROM_FONT_8X8_IBM, &fonts::FONT_8X8[..0x400]),
    ] {
        m.mem.load(Machine::lin(ROM_SEGMENT, offset), font);
    }
    set_vector(m, 0x1F, ROM_SEGMENT, ROM_FONT_8X8 + 0x400);

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
        callback::MOUSE => crate::mouse::service(m),
        callback::MOUSE_IRQ => crate::mouse::irq(m),
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
//
// Character output in graphics modes, graphics scrolling, the ROM font pointers and the INT 10h
// font functions follow DOSBox Staging `src/ints/int10_char.cpp`, `int10.cpp` and
// `int10_modes.cpp` (GPL-2.0-or-later).

fn is_text_mode(mode: u8) -> bool {
    mode <= 3 || mode == 7
}

/// Character height of a mode's ROM font, and its number of text rows.
fn font_metrics(mode: u8) -> (u8, u8) {
    match mode {
        0..=3 | 7 => (16, 25),
        0x0F | 0x10 => (14, 25),
        0x11 | 0x12 => (16, 30),
        _ => (8, 25),
    }
}

/// ROM offset of the font a mode's INT 43h vector points at.
fn font_for_height(height: u8) -> u16 {
    match height {
        14 => ROM_FONT_8X14,
        16 => ROM_FONT_8X16,
        _ => ROM_FONT_8X8,
    }
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
    let columns: u16 = match mode {
        0 | 1 | 4 | 5 | 0x0D | 0x13 => 40,
        _ => 80,
    };
    let (height, rows) = font_metrics(mode);
    m.mem.write_u8(BDA_VIDEO_MODE, mode);
    m.mem.write_u16(BDA_COLUMNS, columns);
    m.mem.write_u16(BDA_PAGE_SIZE, if text { columns * 25 * 2 } else { 0 });
    m.mem.write_u16(BDA_PAGE_OFFSET, 0);
    for page in 0..8 {
        m.mem.write_u16(BDA_CURSOR + page * 2, 0);
    }
    // Graphics modes leave the cursor shape word 0; some programs test it to detect them.
    m.mem.write_u16(BDA_CURSOR_SHAPE, if text { 0x0607 } else { 0 });
    m.mem.write_u8(BDA_ACTIVE_PAGE, 0);
    m.mem.write_u8(BDA_ROWS, rows - 1);
    m.mem.write_u16(BDA_CHAR_HEIGHT, u16::from(height));
    let font = if text { ROM_FONT_8X8 } else { font_for_height(height) };
    set_vector(m, 0x43, ROM_SEGMENT, font);
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
    crate::mouse::video_mode_changed(m);
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

/// Columns and rows of the current mode's character grid.
fn grid(m: &Machine) -> (u16, u16) {
    (m.mem.read_u16(BDA_COLUMNS).max(1), u16::from(m.mem.read_u8(BDA_ROWS)) + 1)
}

fn char_height(m: &Machine) -> u32 {
    u32::from(m.mem.read_u16(BDA_CHAR_HEIGHT).clamp(1, 32))
}

fn put_pixel_256(m: &mut Machine, x: u32, y: u32, color: u8) {
    if x < 320 && y < 200 {
        m.mem.write_u8(0xA_0000 + y * 320 + x, color);
    }
}

/// Writes one character at a cell. In text modes `attribute` is stored only if `use_attribute`;
/// in graphics modes it is the foreground color, and in 256-color modes `page` is the
/// background color (as on a real VGA BIOS).
fn write_char(m: &mut Machine, column: u16, row: u16, page: u8, ch: u8, attribute: u8, use_attribute: bool) {
    if is_text_mode(m.devices.vga.mode) {
        let cell = text_cell(m, page, column, row);
        m.mem.write_u8(cell, ch);
        if use_attribute {
            m.mem.write_u8(cell + 1, attribute);
        }
        return;
    }
    if !m.devices.vga.is_chained_256() {
        let mode = m.devices.vga.mode;
        m.diagnostic(format!("BIOS text output in video mode {mode:02X}h isn't drawn yet"));
        return;
    }
    let height = char_height(m);
    let (font_offset, font_segment) = (m.mem.read_u16(0x43 * 4), m.mem.read_u16(0x43 * 4 + 2));
    let glyph = Machine::lin(font_segment, font_offset) + u32::from(ch) * height;
    let (x0, y0) = (u32::from(column) * 8, u32::from(row) * height);
    for line in 0..height {
        let bits = m.mem.read_u8(glyph + line);
        for bit in 0..8 {
            let color = if bits & (0x80 >> bit) != 0 { attribute } else { page };
            put_pixel_256(m, x0 + bit, y0 + line, color);
        }
    }
}

/// Copies character row `from` to `to`, columns `left..=right`.
fn copy_row(m: &mut Machine, left: u16, right: u16, from: u16, to: u16, page: u8) {
    if is_text_mode(m.devices.vga.mode) {
        for column in left..=right {
            let value = m.mem.read_u16(text_cell(m, page, column, from));
            m.mem.write_u16(text_cell(m, page, column, to), value);
        }
    } else if m.devices.vga.is_chained_256() {
        let height = char_height(m);
        let pitch = 8 * u32::from(grid(m).0);
        for line in 0..height {
            let source = 0xA_0000 + (u32::from(from) * height + line) * pitch + 8 * u32::from(left);
            let dest = 0xA_0000 + (u32::from(to) * height + line) * pitch + 8 * u32::from(left);
            for x in 0..8 * u32::from(right - left + 1) {
                let value = m.mem.read_u8(source + x);
                m.mem.write_u8(dest + x, value);
            }
        }
    }
}

/// Blanks character row `row`, columns `left..=right`, with `attribute` (a color in graphics).
fn fill_row(m: &mut Machine, left: u16, right: u16, row: u16, attribute: u8, page: u8) {
    if is_text_mode(m.devices.vga.mode) {
        for column in left..=right {
            m.mem.write_u16(text_cell(m, page, column, row), (u16::from(attribute) << 8) | 0x20);
        }
    } else if m.devices.vga.is_chained_256() {
        let height = char_height(m);
        let pitch = 8 * u32::from(grid(m).0);
        for line in 0..height {
            let dest = 0xA_0000 + (u32::from(row) * height + line) * pitch + 8 * u32::from(left);
            for x in 0..8 * u32::from(right - left + 1) {
                m.mem.write_u8(dest + x, attribute);
            }
        }
    } else {
        let mode = m.devices.vga.mode;
        m.diagnostic(format!("BIOS scrolling in video mode {mode:02X}h isn't emulated yet"));
    }
}

/// A rectangle of character cells, inclusive.
#[derive(Clone, Copy)]
struct Window {
    top: u16,
    left: u16,
    bottom: u16,
    right: u16,
}

/// Scrolls a window up (or down) by `lines`; 0 clears it. Blank rows get `attribute`.
fn scroll(m: &mut Machine, window: Window, lines: u8, attribute: u8, up: bool) {
    let (columns, rows) = grid(m);
    let Window { top, left, bottom, right } = window;
    let (bottom, right) = (bottom.min(rows - 1), right.min(columns - 1));
    if top > bottom || left > right {
        return;
    }
    let page = m.mem.read_u8(BDA_ACTIVE_PAGE);
    let height = bottom - top + 1;
    let lines = if lines == 0 || u16::from(lines) > height { height } else { u16::from(lines) };
    if up {
        for row in top..=bottom - lines {
            copy_row(m, left, right, row + lines, row, page);
        }
        for row in bottom + 1 - lines..=bottom {
            fill_row(m, left, right, row, attribute, page);
        }
    } else {
        for row in (top + lines..=bottom).rev() {
            copy_row(m, left, right, row - lines, row, page);
        }
        for row in top..top + lines {
            fill_row(m, left, right, row, attribute, page);
        }
    }
}

/// Console output as DOS does it: teletype on the active page in light gray.
pub(crate) fn teletype(m: &mut Machine, ch: u8) {
    let text = is_text_mode(m.devices.vga.mode);
    let page = m.mem.read_u8(BDA_ACTIVE_PAGE);
    teletype_output(m, ch, 0x07, !text, page);
}

/// BIOS teletype: control characters move the cursor, others are written and advance it,
/// scrolling at the bottom. In graphics modes the attribute is the color.
fn teletype_output(m: &mut Machine, ch: u8, attribute: u8, use_attribute: bool, page: u8) {
    m.console.push(ch);
    let text = is_text_mode(m.devices.vga.mode);
    let (columns, rows) = grid(m);
    let (mut column, mut row) = cursor(m, page);
    match ch {
        0x07 => return, // bell: no position change
        0x08 => column = column.saturating_sub(1),
        b'\r' => column = 0,
        b'\n' => row += 1,
        _ => {
            write_char(m, column, row, page, ch, attribute, use_attribute);
            column += 1;
        }
    }
    if column >= columns {
        column = 0;
        row += 1;
    }
    if row == rows {
        // Text modes fill with the attribute under the cursor, graphics modes with black.
        let fill = if text { (m.mem.read_u16(text_cell(m, page, column, row - 1)) >> 8) as u8 } else { 0 };
        scroll(m, Window { top: 0, left: 0, bottom: rows - 1, right: columns - 1 }, 1, fill, true);
        row -= 1;
    }
    set_cursor(m, page, column, row);
}

fn video(m: &mut Machine) {
    let r = m.cpu.regs;
    let mode = m.devices.vga.mode;
    let text = is_text_mode(mode);
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
        0x06 | 0x07 => {
            let window = Window {
                top: u16::from(r.ch()),
                left: u16::from(r.cl()),
                bottom: u16::from(r.dh()),
                right: u16::from(r.dl()),
            };
            scroll(m, window, r.al(), r.bh(), r.ah() == 0x06);
        }
        0x08 if text => {
            let (column, row) = cursor(m, r.bh());
            let cell = m.mem.read_u16(text_cell(m, r.bh(), column, row));
            m.cpu.regs.set_ax(cell);
        }
        0x09 | 0x0A => {
            // Graphics modes always use the attribute as color; 256-color modes take the cursor of
            // page 0 and use BH as the background.
            let cursor_page = if text { r.bh() } else { 0 };
            let (mut column, mut row) = cursor(m, cursor_page);
            let (columns, _) = grid(m);
            let use_attribute = r.ah() == 0x09 || !text;
            for _ in 0..r.cx() {
                write_char(m, column, row, r.bh(), r.al(), r.bl(), use_attribute);
                column += 1;
                if column == columns {
                    column = 0;
                    row += 1;
                }
            }
        }
        0x0C if mode == 0x13 => put_pixel_256(m, u32::from(r.cx()), u32::from(r.dx()), r.al()),
        0x0D if mode == 0x13 => {
            let pixel = if r.cx() < 320 && r.dx() < 200 {
                m.mem.read_u8(0xA_0000 + u32::from(r.dx()) * 320 + u32::from(r.cx()))
            } else {
                0
            };
            m.cpu.regs.set_al(pixel);
        }
        0x0E => {
            let page = m.mem.read_u8(BDA_ACTIVE_PAGE);
            teletype_output(m, r.al(), r.bl(), !text, page);
        }
        0x0F => {
            let columns = m.mem.read_u16(BDA_COLUMNS) as u8;
            let page = m.mem.read_u8(BDA_ACTIVE_PAGE);
            m.cpu.regs.set_ax((u16::from(columns) << 8) | u16::from(mode));
            m.cpu.regs.set_bh(page);
        }
        0x10 => palette(m),
        0x11 => fonts(m),
        0x12 if r.bl() == 0x10 => {
            m.cpu.regs.set_bx(0x0003); // color, 256 KB
            m.cpu.regs.set_cx(0x0009);
        }
        0x13 => {
            // Write string at DH,DL (FFh: at the cursor) on page BH. AL bit 0 keeps the cursor
            // after the string; bit 1 means the string is (character, attribute) pairs.
            let page = r.bh();
            let saved = cursor(m, page);
            if r.dh() == 0xFF {
                set_cursor(m, page, saved.0, saved.1);
            } else {
                set_cursor(m, page, u16::from(r.dl()), u16::from(r.dh()));
            }
            let pairs = r.al() & 2 != 0;
            let mut at = r.bp();
            for _ in 0..r.cx() {
                let ch = m.mem.read_u8(Machine::lin(r.es(), at));
                at = at.wrapping_add(1);
                let attribute = if pairs {
                    let attribute = m.mem.read_u8(Machine::lin(r.es(), at));
                    at = at.wrapping_add(1);
                    attribute
                } else {
                    r.bl()
                };
                teletype_output(m, ch, attribute, true, page);
            }
            if r.al() & 1 == 0 {
                set_cursor(m, page, saved.0, saved.1);
            }
        }
        0x1A if r.al() == 0x00 => {
            m.cpu.regs.set_al(0x1A);
            m.cpu.regs.set_bx(0x0008); // VGA with analog color display
        }
        ah => m.diagnostic(format!("INT 10h AH={ah:02X}h AL={:02X}h in mode {mode:02X}h is not implemented", r.al())),
    }
}

/// INT 10h AH=11h: font information and graphics font selection. Loading fonts into the text
/// mode character generator isn't emulated.
fn fonts(m: &mut Machine) {
    let r = m.cpu.regs;
    let set_graphics_font = |m: &mut Machine, segment: u16, offset: u16, height: u16| {
        set_vector(m, 0x43, segment, offset);
        m.mem.write_u16(BDA_CHAR_HEIGHT, height);
        let rows = match r.bl() {
            0x00 => r.dl(),
            0x01 => 14,
            0x03 => 43,
            _ => 25,
        };
        m.mem.write_u8(BDA_ROWS, rows.saturating_sub(1));
    };
    match r.al() {
        0x30 => {
            let (segment, offset) = match r.bh() {
                0x00 => (m.mem.read_u16(0x1F * 4 + 2), m.mem.read_u16(0x1F * 4)),
                0x01 => (m.mem.read_u16(0x43 * 4 + 2), m.mem.read_u16(0x43 * 4)),
                0x02 => (ROM_SEGMENT, ROM_FONT_8X14),
                0x03 => (ROM_SEGMENT, ROM_FONT_8X8),
                0x04 => (ROM_SEGMENT, ROM_FONT_8X8 + 0x400),
                0x05 => (ROM_SEGMENT, ROM_FONT_9X14),
                0x06 => (ROM_SEGMENT, ROM_FONT_8X16),
                0x07 => (ROM_SEGMENT, ROM_FONT_9X16),
                _ => (r.es(), r.bp()),
            };
            m.cpu.regs.set_es(segment);
            m.cpu.regs.set_bp(offset);
            m.cpu.regs.set_cx(m.mem.read_u16(BDA_CHAR_HEIGHT));
            m.cpu.regs.set_dl(m.mem.read_u8(BDA_ROWS));
        }
        0x20 => set_vector(m, 0x1F, r.es(), r.bp()),
        0x21 => set_graphics_font(m, r.es(), r.bp(), r.cx()),
        0x22 => set_graphics_font(m, ROM_SEGMENT, ROM_FONT_8X14, 14),
        0x23 => set_graphics_font(m, ROM_SEGMENT, ROM_FONT_8X8, 8),
        0x24 => set_graphics_font(m, ROM_SEGMENT, ROM_FONT_8X16, 16),
        al => m.diagnostic(format!("INT 10h AX=11{al:02X}h (text-mode font loading) is not emulated")),
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
