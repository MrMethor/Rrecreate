//! BIOS: ROM stubs, interrupt vector table, BIOS data area and the BIOS services as
//! high-level emulation.
//!
//! Each service vector points at a small ROM stub: a host callback followed by real code
//! (`IRET`, `INT 1Ch`, the EOI to the PIC). Programs can hook and chain these vectors exactly as
//! on real hardware.

use cpu::flags::{CF, ZF};

use crate::clock::PIT_HZ;
use crate::fonts;
use crate::keyboard::scancode_ascii;
use crate::machine::Machine;
use crate::vga::{self, DisplayKind, TEXT_BASE};
use crate::{vesa, vga_params};

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
pub const BDA_CGA_MODE: u32 = 0x465;
pub const BDA_CGA_PALETTE: u32 = 0x466;
pub const BDA_TICKS: u32 = 0x46C;
pub const BDA_MIDNIGHT: u32 = 0x470;
pub const BDA_KBD_START: u32 = 0x480;
pub const BDA_KBD_END: u32 = 0x482;
pub const BDA_ROWS: u32 = 0x484;
pub const BDA_CHAR_HEIGHT: u32 = 0x485;
pub const BDA_VIDEO_CONTROL: u32 = 0x487;

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
    /// The XMS driver's far-call entry point (not an interrupt).
    pub const XMS: u16 = 0x0143;
    /// The VBE far-callable bank switch (`WinFuncPtr` in the mode information).
    pub const VESA_WINDOW: u16 = 0x0144;
    /// COMMAND.COM's step ([`crate::shell`]).
    pub const SHELL: u16 = 0x0145;
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
    for vector in 0x71..=0x77 {
        set_vector(m, vector, ROM_SEGMENT, eoi_both);
    }
    // IRQ 8, the real-time clock: read register C (which acknowledges the periodic interrupt),
    // then acknowledge both PICs.
    let rtc = rom.emit(m, &[0x50, 0xB0, 0x0C, 0xE6, 0x70, 0xE4, 0x71, 0xB0, 0x20, 0xE6, 0xA0, 0xE6, 0x20, 0x58, 0xCF]);
    set_vector(m, 0x70, ROM_SEGMENT, rtc);

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

    assert!(rom.next < crate::dpmi::RM_STUBS, "ROM code overlaps the fixed ROM areas");
    crate::dpmi::install(m);
    vesa::install(m);

    m.mem.load(Machine::lin(ROM_SEGMENT, 0xFFF5), b"06/01/94");
    m.mem.load(Machine::lin(ROM_SEGMENT, 0xFFFE), &[0xFC]); // model byte: AT

    // BIOS data area.
    // One floppy, 80×25 color, and bit 1 for a math coprocessor.
    m.mem.write_u16(BDA_EQUIPMENT, 0x0021 | (u16::from(m.cpu.fpu.is_some()) << 1));
    m.mem.write_u16(BDA_MEMORY_KB, 640);
    m.mem.write_u16(BDA_KBD_HEAD, KBD_BUFFER_START);
    m.mem.write_u16(BDA_KBD_TAIL, KBD_BUFFER_START);
    m.mem.write_u16(BDA_KBD_START, KBD_BUFFER_START);
    m.mem.write_u16(BDA_KBD_END, KBD_BUFFER_END);
    m.mem.write_u16(BDA_CRTC_PORT, 0x3D4);
    let since_midnight = u64::from(m.config.start_time.seconds_since_midnight());
    m.mem.write_u32(BDA_TICKS, (since_midnight * PIT_HZ / 65536) as u32);
    set_video_mode(m, 3);
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
        callback::VESA_WINDOW => vesa::window_function(m),
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
// Ported from DOSBox Staging `src/ints/int10_modes.cpp`, `int10_char.cpp`, `int10_put_pixel.cpp`,
// `int10_memory.cpp` and `int10.cpp` (GPL-2.0-or-later): mode sets load the registers from the
// VGA parameter table (`vga_params.rs`); character output, scrolling and pixels work in text,
// CGA, planar EGA/VGA and 256-color modes; text fonts are loaded into plane 2.

/// Entry of the parameter table a mode number uses (400-line text, as a VGA BIOS sets it).
fn parameter_entry(mode: u8) -> Option<usize> {
    Some(match mode {
        0 | 1 => 0x17,
        2 | 3 => 0x18,
        4..=6 => usize::from(mode),
        7 => 0x19,
        0x0D | 0x0E => usize::from(mode),
        0x0F => 0x11,
        0x10 => 0x12,
        0x11 => 0x1A,
        0x12 => 0x1B,
        0x13 => 0x1C,
        _ => return None,
    })
}

fn display_kind(m: &Machine) -> DisplayKind {
    m.devices.vga.display_kind()
}

fn is_text(m: &Machine) -> bool {
    display_kind(m) == DisplayKind::Text
}

/// Screen size of a BIOS graphics mode in pixels (the mouse driver scales to it).
pub(crate) fn graphics_size(mode: u16) -> (i32, i32) {
    if let Some(block) = vesa::find(mode) {
        return (i32::from(block.width), i32::from(block.height));
    }
    let width = if matches!(mode, 4 | 5 | 0x0D | 0x13) { 320 } else { 640 };
    let height = match mode {
        0x0F | 0x10 => 350,
        0x11 | 0x12 => 480,
        _ => 200,
    };
    (width, height)
}

/// ROM offset of the font a mode's INT 43h vector points at.
fn font_for_height(height: u8) -> u16 {
    match height {
        14 => ROM_FONT_8X14,
        16 => ROM_FONT_8X16,
        _ => ROM_FONT_8X8,
    }
}

fn rom_font(height: u8) -> &'static [u8] {
    match height {
        14 => &fonts::FONT_8X14,
        16 => &fonts::FONT_8X16,
        _ => &fonts::FONT_8X8,
    }
}

/// Loads a ROM font into a plane-2 font block, with the 9-dot alternate glyphs when the
/// sequencer is in 9-dot mode (DOSBox `INT10_ReloadFont`).
pub(crate) fn load_rom_font(m: &mut Machine, block: u8, height: u8) {
    let vga = &mut m.devices.vga;
    vga.load_font(block, 0, usize::from(height), rom_font(height));
    let nine_dot = vga.seq[1] & 0x01 == 0;
    let alternates: &[u8] = match height {
        14 if nine_dot => &fonts::FONT_9X14_ALTERNATE,
        16 if nine_dot => &fonts::FONT_9X16_ALTERNATE,
        _ => &[],
    };
    let mut rest = alternates;
    while let [ch, glyph @ ..] = rest
        && *ch != 0
        && glyph.len() >= usize::from(height)
    {
        vga.load_font(block, usize::from(*ch), usize::from(height), &glyph[..usize::from(height)]);
        rest = &glyph[usize::from(height)..];
    }
}

/// CGA mode control register values the BIOS keeps at 40:65h, per mode.
const CGA_MODE_CONTROL: [u8; 8] = [0x2C, 0x28, 0x2D, 0x29, 0x2A, 0x2E, 0x1E, 0x29];

/// INT 10h AH=00h and VBE 4F02h (DOSBox `INT10_SetVideoMode`): bit 7 of a BIOS mode number and
/// bit 15 of a VESA one keep video memory; bit 14 (the linear frame buffer) needs nothing, as
/// the frame buffer is always there. Returns false, reporting it, for a mode the card lacks.
pub(crate) fn set_video_mode(m: &mut Machine, number: u16) -> bool {
    let (mut mode, mut clear) = (number, true);
    if mode >= 0x100 {
        clear = mode & 0x8000 == 0;
        mode &= 0x0FFF;
    }
    if mode <= 0x100 && mode & 0x80 != 0 {
        clear = false;
        mode -= 0x80;
    }
    m.devices.vga.vesa_mode = None;
    if let Some(entry) = u8::try_from(mode).ok().and_then(parameter_entry) {
        set_standard_mode(m, mode as u8, entry, clear);
        true
    } else if let Some(block) = vesa::find(mode) {
        vesa::set_mode(m, block, clear);
        true
    } else {
        m.diagnostic(format!("video mode {mode:02X}h is not emulated"));
        false
    }
}

/// A standard VGA mode, from the BIOS parameter table.
fn set_standard_mode(m: &mut Machine, mode: u8, entry: usize, clear: bool) {
    let params = &vga_params::VGA_PARAMETERS[entry];
    let text = mode <= 3 || mode == 7;
    let vga = &mut m.devices.vga;
    vga.load_parameters(params);
    vga.mode = u16::from(mode);
    if text {
        // Planes 0 and 1 with odd/even addressing, as on IBM's VGA (DOSBox's table dump has
        // all planes enabled, which only matters for its own text-mode memory handler).
        vga.seq[2] = 0x03;
        vga.seq[4] = 0x02;
    }
    match mode {
        0x13 => vga.dac = vga::default_256_palette(),
        4..=6 | 0x0D | 0x0E => vga.dac[..64].copy_from_slice(&vga::cga64_palette()),
        _ => vga.dac[..64].copy_from_slice(&vga::ega_palette()),
    }
    let (columns, rows, height) = (params[0], params[1], params[2]);
    let page_size = u16::from_le_bytes([params[3], params[4]]);
    vesa::s3_mode_registers(m, None, 0, (0, 0));
    if clear {
        if text || mode <= 6 {
            let (base, fill) = match mode {
                7 => (0xB_0000, 0x0720),
                0..=3 => (TEXT_BASE, 0x0720),
                _ => (TEXT_BASE, 0),
            };
            for i in 0..0x4000u32 {
                m.mem.write_u16(base + i * 2, fill);
            }
        } else {
            m.devices.vga.clear_memory();
        }
    }
    if text {
        load_rom_font(m, 0, height);
    }
    m.mem.write_u8(BDA_VIDEO_MODE, mode);
    m.mem.write_u16(BDA_COLUMNS, u16::from(columns));
    m.mem.write_u16(BDA_PAGE_SIZE, page_size);
    m.mem.write_u16(BDA_CRTC_PORT, if matches!(mode, 7 | 0x0F) { 0x3B4 } else { 0x3D4 });
    m.mem.write_u8(BDA_CGA_MODE, CGA_MODE_CONTROL.get(usize::from(mode)).copied().unwrap_or(0x29));
    m.mem.write_u8(BDA_CGA_PALETTE, if mode == 6 { 0x3F } else { 0x30 });
    m.mem.write_u8(BDA_ROWS, rows);
    m.mem.write_u16(BDA_CHAR_HEIGHT, u16::from(height));
    m.mem.write_u8(BDA_VIDEO_CONTROL, 0x60 | if clear { 0 } else { 0x80 });
    after_mode_set(m, text, if text { 8 } else { height });
}

/// The end of every mode set (DOSBox `finish_set_mode`): the INT 43h font for `font_height`
/// (the standard text modes use the 8×8 one), the cursor shape (0 in graphics modes; some
/// programs test it to detect them), cursor positions, the active page, and the mouse driver.
pub(crate) fn after_mode_set(m: &mut Machine, text: bool, font_height: u8) {
    set_vector(m, 0x43, ROM_SEGMENT, font_for_height(font_height));
    m.mem.write_u16(BDA_CURSOR_SHAPE, if text { 0x0607 } else { 0 });
    m.mem.write_u16(BDA_PAGE_OFFSET, 0);
    for page in 0..8 {
        m.mem.write_u16(BDA_CURSOR + page * 2, 0);
    }
    m.mem.write_u8(BDA_ACTIVE_PAGE, 0);
    crate::mouse::video_mode_changed(m);
}

fn crtc_write(m: &mut Machine, index: usize, value: u8) {
    m.devices.vga.crtc[index] = value;
    m.devices.vga.changed = true;
}

fn cursor(m: &Machine, page: u8) -> (u16, u16) {
    let at = BDA_CURSOR + u32::from(page & 7) * 2;
    (u16::from(m.mem.read_u8(at)), u16::from(m.mem.read_u8(at + 1)))
}

/// Moves a page's cursor; on the active page the CRTC cursor follows (DOSBox
/// `INT10_SetCursorPos`).
fn set_cursor(m: &mut Machine, page: u8, column: u16, row: u16) {
    let at = BDA_CURSOR + u32::from(page & 7) * 2;
    m.mem.write_u8(at, column as u8);
    m.mem.write_u8(at + 1, row as u8);
    if page & 7 == m.mem.read_u8(BDA_ACTIVE_PAGE) {
        let columns = m.mem.read_u16(BDA_COLUMNS);
        let page_start = u16::from(page & 7).wrapping_mul(m.mem.read_u16(BDA_PAGE_SIZE)) / 2;
        let address = page_start.wrapping_add(row.wrapping_mul(columns)).wrapping_add(column);
        crtc_write(m, 0x0E, (address >> 8) as u8);
        crtc_write(m, 0x0F, address as u8);
    }
}

/// INT 10h AH=01h: stores the cursor shape and programs the CRTC, scaling CGA-style shapes to
/// the character height in text modes (DOSBox `INT10_SetCursorShape`).
fn set_cursor_shape(m: &mut Machine, first: u8, last: u8) {
    m.mem.write_u16(BDA_CURSOR_SHAPE, (u16::from(first) << 8) | u16::from(last));
    let (mut first, mut last) = (first, last);
    let emulate = is_text(m) && m.mem.read_u8(BDA_VIDEO_CONTROL) & 0x08 == 0;
    if emulate && (first | last) & 0xE0 == 0 {
        let height = (m.mem.read_u16(BDA_CHAR_HEIGHT) as u8).wrapping_sub(1);
        if last < first {
            if last != 0 {
                first = last;
                last = height;
            }
        } else if ((first | last) >= height || last != height.wrapping_sub(1) || first != height) && last > 3 {
            if first + 2 < last {
                if first > 2 {
                    first = height.div_ceil(2);
                }
                last = height;
            } else {
                first = first.wrapping_sub(last).wrapping_add(height);
                last = height;
                if height > 0x0C {
                    first = first.wrapping_sub(1);
                    last = last.wrapping_sub(1);
                }
            }
        }
    }
    crtc_write(m, 0x0A, first);
    crtc_write(m, 0x0B, last);
}

/// INT 10h AH=05h: shows another page (DOSBox `INT10_SetActivePage`).
fn set_active_page(m: &mut Machine, page: u8) {
    if page > 7 {
        return;
    }
    let offset = u16::from(page).wrapping_mul(m.mem.read_u16(BDA_PAGE_SIZE));
    m.mem.write_u16(BDA_PAGE_OFFSET, offset);
    let start = if m.devices.vga.mode < 8 { offset >> 1 } else { offset };
    crtc_write(m, 0x0C, (start >> 8) as u8);
    crtc_write(m, 0x0D, start as u8);
    m.mem.write_u8(BDA_ACTIVE_PAGE, page);
    let (column, row) = cursor(m, page);
    set_cursor(m, page, column, row);
}

fn text_cell(m: &Machine, page: u8, column: u16, row: u16) -> u32 {
    let columns = u32::from(m.mem.read_u16(BDA_COLUMNS).max(1));
    let page_size = u32::from(m.mem.read_u16(BDA_PAGE_SIZE));
    let base = if m.devices.vga.mode == 7 { 0xB_0000 } else { TEXT_BASE };
    base + u32::from(page & 7) * page_size + (u32::from(row) * columns + u32::from(column)) * 2
}

/// Columns and rows of the current mode's character grid.
fn grid(m: &Machine) -> (u16, u16) {
    (m.mem.read_u16(BDA_COLUMNS).max(1), u16::from(m.mem.read_u8(BDA_ROWS)) + 1)
}

fn char_height(m: &Machine) -> u32 {
    u32::from(m.mem.read_u16(BDA_CHAR_HEIGHT).clamp(1, 32))
}

/// A byte of video memory as the CPU sees it: the A0000 window goes through the VGA (latches,
/// read mode), other addresses are RAM.
fn video_read(m: &mut Machine, addr: u32) -> u8 {
    if addr & 0xFFFF_0000 == vga::GRAPHICS_BASE { m.devices.vga.cpu_read(addr & 0xFFFF) } else { m.mem.read_u8(addr) }
}

fn video_write(m: &mut Machine, addr: u32, value: u8) {
    if addr & 0xFFFF_0000 == vga::GRAPHICS_BASE {
        m.devices.vga.cpu_write(addr & 0xFFFF, value);
    } else {
        m.mem.write_u8(addr, value);
    }
}

/// INT 10h AH=0Ch: color bit 7 XORs the pixel in CGA and planar modes.
pub(crate) fn put_pixel(m: &mut Machine, x: u16, y: u16, page: u8, color: u8) {
    let (x32, y32) = (u32::from(x), u32::from(y));
    match display_kind(m) {
        DisplayKind::Cga4 => {
            let at = TEXT_BASE + (y32 >> 1) * 80 + (x32 >> 2) + (y32 & 1) * 0x2000;
            let shift = 2 * (3 - (x & 3));
            let old = m.mem.read_u8(at);
            let new = if color & 0x80 != 0 {
                old ^ ((color & 3) << shift)
            } else {
                (old & !(3 << shift)) | ((color & 3) << shift)
            };
            m.mem.write_u8(at, new);
        }
        DisplayKind::Cga2 => {
            let at = TEXT_BASE + (y32 >> 1) * 80 + (x32 >> 3) + (y32 & 1) * 0x2000;
            let shift = 7 - (x & 7);
            let old = m.mem.read_u8(at);
            let new = if color & 0x80 != 0 {
                old ^ ((color & 1) << shift)
            } else {
                (old & !(1 << shift)) | ((color & 1) << shift)
            };
            m.mem.write_u8(at, new);
        }
        DisplayKind::Ega => {
            let vga = &mut m.devices.vga;
            vga.seq[2] = 0x0F;
            vga.gc[8] = 0x80 >> (x & 7);
            vga.gc[0] = color;
            vga.gc[1] = 0x0F;
            if color & 0x80 != 0 {
                vga.gc[3] = 0x18;
            }
            let page_size = u32::from(m.mem.read_u16(BDA_PAGE_SIZE));
            let columns = u32::from(m.mem.read_u16(BDA_COLUMNS));
            let offset = page_size * u32::from(page) + ((y32 * columns * 8 + x32) >> 3);
            let vga = &mut m.devices.vga;
            vga.cpu_read(offset);
            vga.cpu_write(offset, 0xFF);
            vga.gc[8] = 0xFF;
            vga.gc[1] = 0;
            if color & 0x80 != 0 {
                vga.gc[3] = 0;
            }
        }
        DisplayKind::Vga256 => m.devices.vga.cpu_write(y32 * 320 + x32, color),
        // Through the linear frame buffer, as DOSBox does.
        DisplayKind::Svga256 => {
            let columns = u32::from(m.mem.read_u16(BDA_COLUMNS));
            m.devices.vga.lfb_write(x32 + y32 * columns * 8, color);
        }
        DisplayKind::Text => {}
        kind => m.diagnostic(format!("INT 10h pixels in {kind:?} modes are not implemented")),
    }
}

/// INT 10h AH=0Dh.
pub(crate) fn get_pixel(m: &mut Machine, x: u16, y: u16, page: u8) -> u8 {
    let (x32, y32) = (u32::from(x), u32::from(y));
    match display_kind(m) {
        DisplayKind::Cga4 => {
            let at = TEXT_BASE + (y32 >> 1) * 80 + (x32 >> 2) + (y32 & 1) * 0x2000;
            (m.mem.read_u8(at) >> (2 * (3 - (x & 3)))) & 3
        }
        DisplayKind::Cga2 => {
            let at = TEXT_BASE + (y32 >> 1) * 80 + (x32 >> 3) + (y32 & 1) * 0x2000;
            (m.mem.read_u8(at) >> (7 - (x & 7))) & 1
        }
        DisplayKind::Ega => {
            let page_size = u32::from(m.mem.read_u16(BDA_PAGE_SIZE));
            let columns = u32::from(m.mem.read_u16(BDA_COLUMNS));
            let offset = page_size * u32::from(page) + ((y32 * columns * 8 + x32) >> 3);
            let shift = 7 - (x & 7);
            let vga = &mut m.devices.vga;
            let mut color = 0;
            for plane in 0..4 {
                vga.gc[4] = plane;
                color |= ((vga.cpu_read(offset) >> shift) & 1) << plane;
            }
            color
        }
        DisplayKind::Vga256 => m.devices.vga.cpu_read(y32 * 320 + x32),
        DisplayKind::Svga256 => {
            let columns = u32::from(m.mem.read_u16(BDA_COLUMNS));
            m.devices.vga.lfb_read(x32 + y32 * columns * 8)
        }
        DisplayKind::Text => 0,
        kind => {
            m.diagnostic(format!("INT 10h pixels in {kind:?} modes are not implemented"));
            0
        }
    }
}

/// Writes one character at a cell (DOSBox `WriteChar`). In text modes `attribute` is stored
/// only if `use_attribute`; in graphics modes it is the color (bit 7: XOR), and in 256-color
/// modes `page` is the background color.
fn write_char(m: &mut Machine, column: u16, row: u16, page: u8, ch: u8, attribute: u8, use_attribute: bool) {
    let kind = display_kind(m);
    if kind == DisplayKind::Text {
        let cell = text_cell(m, page, column, row);
        m.mem.write_u8(cell, ch);
        if use_attribute {
            m.mem.write_u8(cell + 1, attribute);
        }
        return;
    }
    let height = char_height(m);
    let cga = matches!(kind, DisplayKind::Cga4 | DisplayKind::Cga2);
    let (ch, vector) = if cga && ch >= 0x80 { (ch - 0x80, 0x1F) } else { (ch, 0x43) };
    let (font_offset, font_segment) = (m.mem.read_u16(vector * 4), m.mem.read_u16(vector * 4 + 2));
    let glyph = Machine::lin(font_segment, font_offset) + u32::from(ch) * height;
    let mut attribute = if use_attribute {
        attribute
    } else {
        match kind {
            DisplayKind::Cga4 => 3,
            DisplayKind::Cga2 => 1,
            _ => 0x0F,
        }
    };
    if m.devices.vga.mode == 6 {
        attribute = (attribute & 0x80) | 1;
    }
    let (background, page) = match kind {
        DisplayKind::Vga256 | DisplayKind::Svga256 => (page, 0),
        DisplayKind::Ega => {
            // All planes enabled for EGA modes (DOSBox: "Ultima 1 colour bug").
            m.devices.vga.seq[2] = 0x0F;
            (attribute & 0x80, page)
        }
        _ => (attribute & 0x80, page),
    };
    let (x, y) = (column * 8, row * height as u16);
    for line in 0..height as u16 {
        let bits = m.mem.read_u8(glyph + u32::from(line));
        for bit in 0..8 {
            let color = if bits & (0x80 >> bit) != 0 { attribute } else { background };
            put_pixel(m, x + bit, y + line, page, color);
        }
    }
}

/// Copies character row `from` to `to`, columns `left..=right`, on the page at `base`
/// (DOSBox `*_CopyRow`).
fn copy_row(m: &mut Machine, left: u16, right: u16, from: u16, to: u16, base: u32) {
    let (columns, height) = (u32::from(grid(m).0), char_height(m));
    let (left, count, from, to) = (u32::from(left), u32::from(right - left + 1), u32::from(from), u32::from(to));
    match display_kind(m) {
        DisplayKind::Text => {
            for column in 0..count {
                let source = base + ((from * columns + left + column) * 2);
                let dest = base + ((to * columns + left + column) * 2);
                let value = m.mem.read_u16(source);
                m.mem.write_u16(dest, value);
            }
        }
        kind @ (DisplayKind::Cga4 | DisplayKind::Cga2) => {
            let bytes = if kind == DisplayKind::Cga4 { 2 } else { 1 };
            let pitch = columns * bytes;
            let lines = height / 2;
            for line in 0..lines {
                for bank in [0, 0x2000] {
                    for x in 0..count * bytes {
                        let source = base + bank + (from * lines + line) * pitch + left * bytes + x;
                        let dest = base + bank + (to * lines + line) * pitch + left * bytes + x;
                        let value = m.mem.read_u8(source);
                        m.mem.write_u8(dest, value);
                    }
                }
            }
        }
        DisplayKind::Ega => {
            m.devices.vga.gc[5] = 0x01; // write mode 1: copy the latches
            m.devices.vga.seq[2] = 0x0F;
            for line in 0..height {
                for x in 0..count {
                    let value = video_read(m, base + (from * height + line) * columns + left + x);
                    video_write(m, base + (to * height + line) * columns + left + x, value);
                }
            }
            m.devices.vga.gc[5] = 0x00;
        }
        DisplayKind::Vga256 => {
            let pitch = 8 * columns;
            for line in 0..height {
                for x in 0..8 * count {
                    let value = video_read(m, base + (from * height + line) * pitch + 8 * left + x);
                    video_write(m, base + (to * height + line) * pitch + 8 * left + x, value);
                }
            }
        }
        kind => m.diagnostic(format!("INT 10h scrolling in {kind:?} modes is not implemented")),
    }
}

/// Blanks character row `row`, columns `left..=right`, with `attribute` (a color in graphics)
/// (DOSBox `*_FillRow`).
fn fill_row(m: &mut Machine, left: u16, right: u16, row: u16, attribute: u8, base: u32) {
    let (columns, height) = (u32::from(grid(m).0), char_height(m));
    let (left, count, row) = (u32::from(left), u32::from(right - left + 1), u32::from(row));
    match display_kind(m) {
        DisplayKind::Text => {
            for column in 0..count {
                m.mem.write_u16(base + (row * columns + left + column) * 2, (u16::from(attribute) << 8) | 0x20);
            }
        }
        kind @ (DisplayKind::Cga4 | DisplayKind::Cga2) => {
            let bytes = if kind == DisplayKind::Cga4 { 2 } else { 1 };
            let pitch = columns * bytes;
            let lines = height / 2;
            let pattern = (attribute & 3) * 0x55;
            for line in 0..lines {
                for bank in [0, 0x2000] {
                    for x in 0..count * bytes {
                        m.mem.write_u8(base + bank + (row * lines + line) * pitch + left * bytes + x, pattern);
                    }
                }
            }
        }
        DisplayKind::Ega => {
            let vga = &mut m.devices.vga;
            vga.gc[8] = 0xFF;
            vga.gc[0] = attribute;
            vga.gc[1] = 0x0F;
            vga.seq[2] = 0x0F;
            for line in 0..height {
                for x in 0..count {
                    video_write(m, base + (row * height + line) * columns + left + x, 0xFF);
                }
            }
            m.devices.vga.gc[1] = 0;
        }
        DisplayKind::Vga256 => {
            let pitch = 8 * columns;
            for line in 0..height {
                for x in 0..8 * count {
                    video_write(m, base + (row * height + line) * pitch + 8 * left + x, attribute);
                }
            }
        }
        kind => m.diagnostic(format!("INT 10h scrolling in {kind:?} modes is not implemented")),
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

/// Scrolls a window up (or down) by `lines`; 0 clears it. Blank rows get `attribute`. `page`
/// is a text page, or `None` for the displayed one (DOSBox `INT10_ScrollWindow`).
fn scroll(m: &mut Machine, window: Window, lines: u8, attribute: u8, up: bool, page: Option<u8>) {
    let (columns, rows) = grid(m);
    let Window { top, left, bottom, right } = window;
    let (bottom, right) = (bottom.min(rows - 1), right.min(columns - 1));
    if top > bottom || left > right {
        return;
    }
    let kind = display_kind(m);
    let page = if kind == DisplayKind::Text { page } else { None };
    let start = match kind {
        DisplayKind::Text if m.devices.vga.mode == 7 => 0xB_0000,
        DisplayKind::Text | DisplayKind::Cga4 | DisplayKind::Cga2 => TEXT_BASE,
        _ => vga::GRAPHICS_BASE,
    };
    let base = start
        + match page {
            Some(page) => u32::from(page) * u32::from(m.mem.read_u16(BDA_PAGE_SIZE)),
            None => u32::from(m.mem.read_u16(BDA_PAGE_OFFSET)),
        };
    let height = bottom - top + 1;
    let lines = if lines == 0 || u16::from(lines) > height { height } else { u16::from(lines) };
    if up {
        for row in top..=bottom - lines {
            copy_row(m, left, right, row + lines, row, base);
        }
        for row in bottom + 1 - lines..=bottom {
            fill_row(m, left, right, row, attribute, base);
        }
    } else {
        for row in (top + lines..=bottom).rev() {
            copy_row(m, left, right, row - lines, row, base);
        }
        for row in top..top + lines {
            fill_row(m, left, right, row, attribute, base);
        }
    }
}

/// Console output as DOS does it: teletype on the active page in light gray.
pub(crate) fn teletype(m: &mut Machine, ch: u8) {
    let text = is_text(m);
    let page = m.mem.read_u8(BDA_ACTIVE_PAGE);
    teletype_output(m, ch, 0x07, !text, page);
}

/// BIOS teletype: control characters move the cursor, others are written and advance it,
/// scrolling at the bottom. In graphics modes the attribute is the color.
fn teletype_output(m: &mut Machine, ch: u8, attribute: u8, use_attribute: bool, page: u8) {
    m.console.push(ch);
    let text = is_text(m);
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
        scroll(m, Window { top: 0, left: 0, bottom: rows - 1, right: columns - 1 }, 1, fill, true, Some(page));
        row -= 1;
    }
    set_cursor(m, page, column, row);
}

fn video(m: &mut Machine) {
    let r = m.cpu.regs;
    let mode = m.devices.vga.mode;
    let text = is_text(m);
    match r.ah() {
        0x00 => {
            set_video_mode(m, u16::from(r.al()));
        }
        0x01 => set_cursor_shape(m, r.ch(), r.cl()),
        0x02 => set_cursor(m, r.bh(), u16::from(r.dl()), u16::from(r.dh())),
        0x03 => {
            let (column, row) = cursor(m, r.bh());
            m.cpu.regs.set_dx((row << 8) | column);
            let shape = m.mem.read_u16(BDA_CURSOR_SHAPE);
            m.cpu.regs.set_cx(shape);
        }
        0x05 => set_active_page(m, r.al()),
        0x06 | 0x07 => {
            let window = Window {
                top: u16::from(r.ch()),
                left: u16::from(r.cl()),
                bottom: u16::from(r.dh()),
                right: u16::from(r.dl()),
            };
            scroll(m, window, r.al(), r.bh(), r.ah() == 0x06, None);
        }
        0x08 if text => {
            let (column, row) = cursor(m, r.bh());
            let cell = m.mem.read_u16(text_cell(m, r.bh(), column, row));
            m.cpu.regs.set_ax(cell);
        }
        0x09 | 0x0A => {
            // Graphics modes always use the attribute as color; 256-color modes take the cursor of
            // page 0 and use BH as the background.
            let cursor_page =
                if matches!(display_kind(m), DisplayKind::Vga256 | DisplayKind::Svga256) { 0 } else { r.bh() };
            let (mut column, mut row) = cursor(m, cursor_page);
            let (columns, _) = grid(m);
            let use_attribute = r.ah() == 0x09 || !text;
            let attribute = if r.ah() == 0x09 && mode == 0x11 { (r.bl() & 0x80) | 0x3F } else { r.bl() };
            for _ in 0..r.cx() {
                write_char(m, column, row, r.bh(), r.al(), attribute, use_attribute);
                column += 1;
                if column == columns {
                    column = 0;
                    row += 1;
                }
            }
        }
        0x0B => cga_palette(m),
        0x0C => put_pixel(m, r.cx(), r.dx(), r.bh(), r.al()),
        0x0D => {
            let pixel = get_pixel(m, r.cx(), r.dx(), r.bh());
            m.cpu.regs.set_al(pixel);
        }
        0x0E => {
            let page = m.mem.read_u8(BDA_ACTIVE_PAGE);
            teletype_output(m, r.al(), r.bl(), !text, page);
        }
        0x0F => {
            // The mode byte of the BIOS data area, with bit 7 set if memory was kept.
            let columns = m.mem.read_u16(BDA_COLUMNS) as u8;
            let page = m.mem.read_u8(BDA_ACTIVE_PAGE);
            let mode = m.mem.read_u8(BDA_VIDEO_MODE) | (m.mem.read_u8(BDA_VIDEO_CONTROL) & 0x80);
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
        0x4F => vesa::service(m),
        ah => m.diagnostic(format!("INT 10h AH={ah:02X}h AL={:02X}h in mode {mode:02X}h is not implemented", r.al())),
    }
}

/// INT 10h AH=0Bh: CGA background/border color (BH=0) and palette (BH=1), through the
/// attribute controller as a VGA BIOS does it (DOSBox `INT10_SetBackgroundBorder` and
/// `INT10_SetColorSelect`).
fn cga_palette(m: &mut Machine) {
    let r = m.cpu.regs;
    let value = r.bl();
    let mut select = m.mem.read_u8(BDA_CGA_PALETTE);
    let first = if r.bh() == 0 {
        select = (select & 0xE0) | (value & 0x1F);
        (select & 0x10) | 2 | ((select & 0x20) >> 5)
    } else {
        select = (select & 0xDF) | if value & 1 != 0 { 0x20 } else { 0 };
        (select & 0x10) | 2 | value
    };
    m.mem.write_u8(BDA_CGA_PALETTE, select);
    let text = m.devices.vga.mode <= 3;
    let vga = &mut m.devices.vga;
    vga.changed = true;
    if r.bh() == 0 {
        let color = ((value << 1) & 0x10) | (value & 7);
        vga.attr[0x11] = color; // overscan
        if text {
            return;
        }
        vga.attr[0] = color;
    } else if text {
        return;
    }
    for i in 0..3u8 {
        vga.attr[usize::from(i) + 1] = first.wrapping_add(2 * i);
    }
}

/// INT 10h AH=11h: text-mode font loading into plane 2 (DOSBox `INT10_LoadFont`), the
/// character map select, graphics font selection and font information.
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
        0x00 | 0x10 => {
            let height = r.bh().max(1);
            let glyphs = m.mem.read_bytes(Machine::lin(r.es(), r.bp()), usize::from(r.cx()) * usize::from(height));
            m.devices.vga.load_font(r.bl(), usize::from(r.dx()), usize::from(height), &glyphs);
            if r.al() == 0x10 {
                activate_font(m, height);
            }
        }
        0x01 | 0x11 | 0x02 | 0x12 | 0x04 | 0x14 => {
            let height = match r.al() & 0x0F {
                0x01 => 14,
                0x02 => 8,
                _ => 16,
            };
            m.devices.vga.load_font(r.bl(), 0, usize::from(height), rom_font(height));
            if r.al() & 0x10 != 0 {
                activate_font(m, height);
            }
        }
        0x03 => {
            m.devices.vga.seq[3] = r.bl();
            m.devices.vga.changed = true;
        }
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
        al => m.diagnostic(format!("INT 10h AX=11{al:02X}h is not implemented")),
    }
}

/// After loading a text font with AL=1xh: character height, rows and cursor follow the new
/// font (DOSBox `load_font` with `reload`). Text modes are 400 lines tall.
fn activate_font(m: &mut Machine, height: u8) {
    const SCREEN_LINES: u16 = 400;
    let crtc9 = m.devices.vga.crtc[9];
    crtc_write(m, 9, (crtc9 & 0xE0) | (height - 1));
    let rows = SCREEN_LINES / u16::from(height);
    crtc_write(m, 0x12, (rows * u16::from(height) - 1) as u8);
    m.mem.write_u8(BDA_ROWS, (rows - 1) as u8);
    m.mem.write_u16(BDA_CHAR_HEIGHT, u16::from(height));
    let columns = m.mem.read_u16(BDA_COLUMNS);
    m.mem.write_u16(BDA_PAGE_SIZE, rows * columns * 2 + 0x100);
    let cursor_height = if height >= 14 { height - 1 } else { height };
    set_cursor_shape(m, cursor_height - 2, cursor_height - 1);
}

fn palette(m: &mut Machine) {
    let r = m.cpu.regs;
    m.devices.vga.changed = true;
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
        0x03 => {
            // BL=0: intensity backgrounds, BL=1: blinking.
            vga.attr[0x10] = (vga.attr[0x10] & !0x08) | if r.bl() & 1 != 0 { 0x08 } else { 0 };
        }
        0x07 => {
            let value = vga.attr.get(usize::from(r.bl())).copied().unwrap_or(0);
            m.cpu.regs.set_bh(value);
        }
        0x08 => {
            let value = vga.attr[0x11];
            m.cpu.regs.set_bh(value);
        }
        0x09 => {
            for i in 0..17u16 {
                let value = m.devices.vga.attr[usize::from(i)];
                m.mem.write_u8(Machine::lin(r.es(), r.dx().wrapping_add(i)), value);
            }
        }
        0x10 => vga.dac[usize::from(r.bl())] = [r.dh() & 0x3F, r.ch() & 0x3F, r.cl() & 0x3F],
        0x12 => {
            for i in 0..r.cx() {
                let at = Machine::lin(r.es(), r.dx().wrapping_add(i * 3));
                let index = usize::from(r.bx().wrapping_add(i) as u8);
                vga.dac[index] = [0, 1, 2].map(|c| m.mem.read_u8(at + c) & 0x3F);
            }
        }
        0x13 => {
            // Color page state: BL=0 selects 4×64 or 16×16 paging (attribute mode bit 7), BL=1
            // the page (color select register).
            if r.bl() == 0 {
                vga.attr[0x10] = (vga.attr[0x10] & 0x7F) | ((r.bh() & 1) << 7);
            } else {
                let page = if vga.attr[0x10] & 0x80 != 0 { r.bh() & 0x0F } else { (r.bh() & 3) << 2 };
                vga.attr[0x14] = page;
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
        0x18 => vga.pel_mask = r.bl(),
        0x19 => {
            let mask = vga.pel_mask;
            m.cpu.regs.set_bl(mask);
        }
        0x1A => {
            let (paging, page) = (vga.attr[0x10] >> 7, vga.attr[0x14]);
            let page = if paging != 0 { page & 0x0F } else { (page >> 2) & 3 };
            m.cpu.regs.set_bx((u16::from(page) << 8) | u16::from(paging));
        }
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
        0x84 => joystick(m),
        0x87 => {
            // Block move through a GDT at ES:SI: source descriptor at 10h, destination at 18h.
            let gdt = Machine::lin(m.cpu.regs.es(), m.cpu.regs.si());
            let base = |m: &Machine, at: u32| {
                (m.mem.read_u32(at + 2) & 0x00FF_FFFF) | (u32::from(m.mem.read_u8(at + 7)) << 24)
            };
            let (source, dest) = (base(m, gdt + 0x10), base(m, gdt + 0x18));
            let bytes = u32::from(m.cpu.regs.cx()) * 2;
            m.mem.copy_phys(dest, source, bytes);
            m.cpu.regs.set_ah(0);
            m.set_return_flag(CF, false);
            m.set_return_flag(ZF, true);
        }
        0x88 => {
            // With an XMS driver loaded, it owns extended memory and the BIOS reports none.
            let kb = if m.xms.enabled() { 0 } else { m.config.extended_kb.min(0xFFFF) as u16 };
            m.cpu.regs.set_ax(kb);
            m.set_return_flag(CF, false);
        }
        0xE8 if m.cpu.regs.al() == 0x01 => {
            // Memory between 1 and 16 MB in KB (AX, CX) and above 16 MB in 64 KB blocks (BX, DX).
            let kb = m.config.extended_kb.min(15 * 1024) as u16;
            m.cpu.regs.set_ax(kb);
            m.cpu.regs.set_cx(kb);
            m.cpu.regs.set_bx(0);
            m.cpu.regs.set_dx(0);
            m.set_return_flag(CF, false);
        }
        0x24 => {
            let al = m.cpu.regs.al();
            match al {
                0 | 1 => m.set_a20(al == 1),
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

/// INT 15h AH=84h: joystick buttons (DX=0) or positions (DX=1), as DOSBox answers it.
fn joystick(m: &mut Machine) {
    let port = &m.devices.game_port;
    match m.cpu.regs.dx() {
        0 if port.connected() => {
            let now = m.devices.clock.nanos(m.cpu.cycles);
            let buttons = m.devices.game_port.read(now) & 0xF0;
            m.cpu.regs.set_al(buttons);
            m.set_return_flag(CF, false);
        }
        0 => {
            m.cpu.regs.set_ax(0x00F0);
            m.cpu.regs.set_dx(0x0201);
            m.set_return_flag(CF, true);
        }
        1 => {
            let positions = port.bios_positions();
            let [ax, bx, cx, dx] = positions.unwrap_or([0; 4]);
            let r = &mut m.cpu.regs;
            r.set_ax(ax);
            r.set_bx(bx);
            r.set_cx(cx);
            r.set_dx(dx);
            m.set_return_flag(CF, positions.is_none());
        }
        dx => m.diagnostic(format!("INT 15h AH=84h DX={dx:04X}h is not implemented")),
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
