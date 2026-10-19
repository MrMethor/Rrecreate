//! Mouse driver (INT 33h) as high-level emulation, with a PS/2 mouse on IRQ 12.
//!
//! The host supplies absolute positions in the driver's coordinate space (640×200 in text and
//! 320×200 modes, like the Microsoft driver). Programs poll with function 03h or install an
//! event handler (0Ch/14h); the handler is called from the IRQ 12 ROM stub with the usual
//! registers. In graphics modes the cursor is drawn with the driver's masks through the BIOS
//! pixel functions, saving and restoring the pixels under it; in text modes the cell is masked.
//!
//! Ranges and reporting granularity per video mode follow the Microsoft driver as DOSBox
//! Staging (`src/hardware/input/mouseif_dos_driver.cpp`, GPL-2.0-or-later) implements it; the
//! graphics cursor drawing is ported from its `draw_cursor`.

use crate::bios::{self, ROM_SEGMENT};
use crate::machine::Machine;
use crate::vga::{DisplayKind, TEXT_BASE};

/// ROM offset of the far pointer the IRQ stub calls through (written by the host).
pub(crate) const HANDLER_SLOT: u16 = 0x9100;
pub(crate) const IRQ_VECTOR: u8 = 0x74;
/// Bytes of the `CALL FAR [CS:slot]` in the IRQ stub, skipped when no handler call is due.
pub(crate) const CALL_LENGTH: u16 = 5;

const DEFAULT_SCREEN_MASK: [u16; 16] = [
    0x3FFF, 0x1FFF, 0x0FFF, 0x07FF, 0x03FF, 0x01FF, 0x00FF, 0x007F, 0x003F, 0x001F, 0x01FF, 0x00FF, 0x30FF, 0xF87F,
    0xF87F, 0xFCFF,
];
const DEFAULT_CURSOR_MASK: [u16; 16] = [
    0x0000, 0x4000, 0x6000, 0x7000, 0x7800, 0x7C00, 0x7E00, 0x7F00, 0x7F80, 0x7C00, 0x6C00, 0x4600, 0x0600, 0x0300,
    0x0300, 0x0000,
];

/// INT 33h functions of other vendors' drivers, answered by leaving registers unchanged.
const IGNORED_VENDOR_FUNCTIONS: [u16; 31] = [
    0x0070, 0x0072, 0x0073, 0x00A0, 0x00A1, 0x00A6, 0x00B0, 0x00D6, 0x00F0, 0x00F1, 0x00F2, 0x00F3, 0x0100, 0x0101,
    0x0102, 0x0103, 0x0104, 0x0666, 0x3000, 0x3001, 0x3002, 0x3003, 0x3004, 0x3005, 0x4F00, 0x4F01, 0x5301, 0x5330,
    0x53C0, 0x53C1, 0x53C2,
];

/// Event mask bits passed to handlers (AX).
pub mod event {
    pub const MOVED: u16 = 0x01;
    pub const LEFT_DOWN: u16 = 0x02;
    pub const LEFT_UP: u16 = 0x04;
    pub const RIGHT_DOWN: u16 = 0x08;
    pub const RIGHT_UP: u16 = 0x10;
    pub const MIDDLE_DOWN: u16 = 0x20;
    pub const MIDDLE_UP: u16 = 0x40;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
struct Handler {
    mask: u16,
    segment: u16,
    offset: u16,
}

/// What a drawn cursor covers: a text cell (linear address, character and attribute) or
/// graphics pixels (x, y, color).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
enum Background {
    Text(u32, u16),
    Pixels(Vec<(u16, u16, u8)>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Mouse {
    pub enabled: bool,
    pub x: i32,
    pub y: i32,
    pub buttons: u8,
    pub min_x: i32,
    pub max_x: i32,
    pub min_y: i32,
    pub max_y: i32,
    /// Masks applied to reported positions: text cells are 8 units, mode 13h pixels 2.
    granularity: (u16, u16),
    /// The cursor is shown while this is 0 (functions 01h/02h count).
    visible: i32,
    presses: [(u16, i32, i32); 3],
    releases: [(u16, i32, i32); 3],
    /// Motion in mickeys since the last function 0Bh call.
    mickeys: (i32, i32),
    mickeys_per_8_pixels: (i32, i32),
    sensitivity: (u16, u16, u16),
    handler: Option<Handler>,
    /// Events not yet delivered to the handler, with the registers they were raised with.
    pending: u16,
    screen_mask: [u16; 16],
    cursor_mask: [u16; 16],
    hot_spot: (i32, i32),
    text_masks: (u16, u16),
    drawn: Option<Background>,
}

impl Mouse {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            x: 320,
            y: 100,
            buttons: 0,
            min_x: 0,
            max_x: 639,
            min_y: 0,
            max_y: 199,
            granularity: (0xFFFF, 0xFFFF),
            visible: -1,
            presses: [(0, 0, 0); 3],
            releases: [(0, 0, 0); 3],
            mickeys: (0, 0),
            mickeys_per_8_pixels: (8, 16),
            sensitivity: (50, 50, 50),
            handler: None,
            pending: 0,
            screen_mask: DEFAULT_SCREEN_MASK,
            cursor_mask: DEFAULT_CURSOR_MASK,
            hot_spot: (0, 0),
            text_masks: (0xFFFF, 0x7700),
            drawn: None,
        }
    }
}

pub(crate) fn install(m: &mut Machine) {
    if !m.mouse.enabled {
        return;
    }
    // Unmask IRQ 12 on the slave PIC, as a PS/2 BIOS does.
    let imr = m.devices.pic2.imr() & !0x10;
    m.devices.pic2.write(true, imr);
}

/// Maximum position and granularity masks for the current video mode.
fn range_for_mode(m: &Machine) -> ((i32, i32), (u16, u16)) {
    let mode = m.devices.vga.mode;
    let text_size = || {
        let columns = i32::from(m.mem.read_u16(bios::BDA_COLUMNS).clamp(1, 250));
        let rows = i32::from(m.mem.read_u8(bios::BDA_ROWS).clamp(0, 249)) + 1;
        (8 * columns - 1, 8 * rows - 1)
    };
    match mode {
        0x00 | 0x01 => {
            let (max_x, max_y) = text_size();
            ((max_x * 2 + 1, max_y), (0xFFF0, 0xFFF8))
        }
        0x02 | 0x03 | 0x07 => (text_size(), (0xFFF8, 0xFFF8)),
        0x0D | 0x13 => ((639, 199), (0xFFFE, 0xFFFF)),
        0x0F | 0x10 => ((639, 349), (0xFFFF, 0xFFFF)),
        0x11 | 0x12 => ((639, 479), (0xFFFF, 0xFFFF)),
        // SVGA modes: their size in pixels, or in characters for text (DOSBox's SVGA branch).
        _ => match crate::vesa::find(mode) {
            Some(b) if b.kind == crate::vesa::ModeType::Text => (text_size(), (0xFFF8, 0xFFF8)),
            Some(b) => ((i32::from(b.width) - 1, i32::from(b.height) - 1), (0xFFFF, 0xFFFF)),
            None => ((639, 199), (0xFFFF, 0xFFFF)),
        },
    }
}

fn apply_mode_range(m: &mut Machine) {
    let ((max_x, max_y), granularity) = range_for_mode(m);
    let mouse = &mut m.mouse;
    (mouse.min_x, mouse.max_x, mouse.min_y, mouse.max_y) = (0, max_x, 0, max_y);
    mouse.granularity = granularity;
    mouse.x = mouse.x.clamp(0, max_x);
    mouse.y = mouse.y.clamp(0, max_y);
}

/// Called after a video mode set: the driver hides its cursor and adapts its range.
pub(crate) fn video_mode_changed(m: &mut Machine) {
    m.mouse.drawn = None;
    m.mouse.visible = -1;
    apply_mode_range(m);
}

fn reset(m: &mut Machine) {
    erase(m);
    let enabled = m.mouse.enabled;
    m.mouse = Mouse::new(enabled);
    apply_mode_range(m);
    m.mouse.x = (m.mouse.max_x + 1) / 2;
    m.mouse.y = (m.mouse.max_y + 1) / 2;
}

/// A position as the driver reports it, rounded to the mode's granularity.
fn reported(m: &Machine, x: i32, y: i32) -> (u16, u16) {
    (x as u16 & m.mouse.granularity.0, y as u16 & m.mouse.granularity.1)
}

/// Host input: the mouse is now at (x, y) with `buttons` (bit 0 left, 1 right, 2 middle).
pub(crate) fn input(m: &mut Machine, x: i32, y: i32, buttons: u8) {
    if !m.mouse.enabled {
        return;
    }
    let mouse = &mut m.mouse;
    let (x, y) = (x.clamp(mouse.min_x, mouse.max_x), y.clamp(mouse.min_y, mouse.max_y));
    let mut events = 0;
    if (x, y) != (mouse.x, mouse.y) {
        // Mickeys: 8 pixels move the ratio's worth (defaults 8 horizontally, 16 vertically).
        mouse.mickeys.0 += (x - mouse.x) * mouse.mickeys_per_8_pixels.0 / 8;
        mouse.mickeys.1 += (y - mouse.y) * mouse.mickeys_per_8_pixels.1 / 8;
        events |= event::MOVED;
    }
    let changed = buttons ^ mouse.buttons;
    for button in 0..3 {
        if changed & (1 << button) == 0 {
            continue;
        }
        let down = buttons & (1 << button) != 0;
        let record = if down { &mut mouse.presses[button] } else { &mut mouse.releases[button] };
        *record = (record.0.wrapping_add(1), x, y);
        events |= match (button, down) {
            (0, true) => event::LEFT_DOWN,
            (0, false) => event::LEFT_UP,
            (1, true) => event::RIGHT_DOWN,
            (1, false) => event::RIGHT_UP,
            (_, true) => event::MIDDLE_DOWN,
            (_, false) => event::MIDDLE_UP,
        };
    }
    mouse.x = x;
    mouse.y = y;
    mouse.buttons = buttons & 7;
    if events & event::MOVED != 0 && mouse.visible >= 0 {
        erase(m);
        draw(m);
    }
    let mouse = &mut m.mouse;
    if let Some(handler) = &mouse.handler
        && handler.mask & events != 0
    {
        mouse.pending |= events & handler.mask;
        m.devices.pic2.raise(4);
    }
}

/// IRQ 12 stub callback: sets up the handler call, or skips it when nothing is pending.
pub(crate) fn irq(m: &mut Machine) {
    let pending = std::mem::take(&mut m.mouse.pending);
    let Some(handler) = m.mouse.handler.clone().filter(|_| pending != 0) else {
        m.cpu.regs.ip = m.cpu.regs.ip.wrapping_add(u32::from(CALL_LENGTH));
        return;
    };
    m.mem.load(Machine::lin(ROM_SEGMENT, HANDLER_SLOT), &handler.offset.to_le_bytes());
    m.mem.load(Machine::lin(ROM_SEGMENT, HANDLER_SLOT + 2), &handler.segment.to_le_bytes());
    let (x, y) = reported(m, m.mouse.x, m.mouse.y);
    let (buttons, mickeys) = (m.mouse.buttons, m.mouse.mickeys);
    let r = &mut m.cpu.regs;
    r.set_ax(pending);
    r.set_bx(u16::from(buttons));
    r.set_cx(x);
    r.set_dx(y);
    r.set_si(mickeys.0 as u16);
    r.set_di(mickeys.1 as u16);
}

/// INT 33h.
pub(crate) fn service(m: &mut Machine) {
    if !m.mouse.enabled {
        return;
    }
    let r = m.cpu.regs;
    match r.ax() {
        0x0000 => {
            reset(m);
            m.cpu.regs.set_ax(0xFFFF);
            m.cpu.regs.set_bx(0xFFFF); // two buttons
        }
        0x0001 => {
            if m.mouse.visible < 0 {
                m.mouse.visible += 1;
                if m.mouse.visible == 0 {
                    draw(m);
                }
            }
        }
        0x0002 => {
            if m.mouse.visible == 0 {
                erase(m);
            }
            m.mouse.visible -= 1;
        }
        0x0003 => {
            let (x, y) = reported(m, m.mouse.x, m.mouse.y);
            let buttons = m.mouse.buttons;
            m.cpu.regs.set_bx(u16::from(buttons));
            m.cpu.regs.set_cx(x);
            m.cpu.regs.set_dx(y);
        }
        0x0004 => {
            let mouse = &mut m.mouse;
            mouse.x = i32::from(r.cx() as i16).clamp(mouse.min_x, mouse.max_x);
            mouse.y = i32::from(r.dx() as i16).clamp(mouse.min_y, mouse.max_y);
            if mouse.visible >= 0 {
                erase(m);
                draw(m);
            }
        }
        0x0005 | 0x0006 => {
            let button = usize::from(r.bx().min(2));
            let mouse = &mut m.mouse;
            let record = if r.ax() == 5 { &mut mouse.presses[button] } else { &mut mouse.releases[button] };
            let (count, x, y) = std::mem::take(record);
            let buttons = mouse.buttons;
            let (x, y) = reported(m, x, y);
            m.cpu.regs.set_ax(u16::from(buttons));
            m.cpu.regs.set_bx(count);
            m.cpu.regs.set_cx(x);
            m.cpu.regs.set_dx(y);
        }
        0x0007 | 0x0008 => {
            let (a, b) = (i32::from(r.cx() as i16), i32::from(r.dx() as i16));
            let (low, high) = (a.min(b), a.max(b));
            let mouse = &mut m.mouse;
            if r.ax() == 7 {
                (mouse.min_x, mouse.max_x) = (low, high);
                mouse.x = mouse.x.clamp(low, high);
            } else {
                (mouse.min_y, mouse.max_y) = (low, high);
                mouse.y = mouse.y.clamp(low, high);
            }
        }
        0x0009 => {
            let visible = m.mouse.visible >= 0;
            if visible {
                erase(m);
            }
            let base = Machine::lin(r.es(), r.dx());
            for i in 0..16 {
                m.mouse.screen_mask[i] = m.mem.read_u16(base + i as u32 * 2);
                m.mouse.cursor_mask[i] = m.mem.read_u16(base + 32 + i as u32 * 2);
            }
            m.mouse.hot_spot = (i32::from(r.bx() as i16), i32::from(r.cx() as i16));
            if visible {
                draw(m);
            }
        }
        0x000A => m.mouse.text_masks = (r.cx(), r.dx()),
        0x000B => {
            let (dx, dy) = std::mem::take(&mut m.mouse.mickeys);
            m.cpu.regs.set_cx(dx as u16);
            m.cpu.regs.set_dx(dy as u16);
        }
        0x000C => m.mouse.handler = handler_from(r.cx(), r.es(), r.dx()),
        0x000F => m.mouse.mickeys_per_8_pixels = (i32::from(r.cx().max(1)), i32::from(r.dx().max(1))),
        0x0010 | 0x0013 | 0x001D => {} // exclusion area, speed threshold, display page
        0x0014 => {
            let old = m.mouse.handler.clone();
            m.mouse.handler = handler_from(r.cx(), r.es(), r.dx());
            let (mask, segment, offset) = old.map_or((0, 0, 0), |h| (h.mask, h.segment, h.offset));
            m.cpu.regs.set_cx(mask);
            m.cpu.regs.set_es(segment);
            m.cpu.regs.set_dx(offset);
        }
        0x0015 => m.cpu.regs.set_bx(0), // driver state size: nothing to save
        0x0016 | 0x0017 => {}
        0x001A => m.mouse.sensitivity = (r.bx(), r.cx(), r.dx()),
        0x001B => {
            let (h, v, threshold) = m.mouse.sensitivity;
            m.cpu.regs.set_bx(h);
            m.cpu.regs.set_cx(v);
            m.cpu.regs.set_dx(threshold);
        }
        0x001E => m.cpu.regs.set_bx(0),
        0x0021 => {
            m.cpu.regs.set_ax(0xFFFF);
            m.cpu.regs.set_bx(0xFFFF);
        }
        0x0024 => {
            m.cpu.regs.set_bx(0x0805); // driver version 8.05
            m.cpu.regs.set_cx(0x040C); // PS/2 mouse on IRQ 12
        }
        // Other vendors' extensions (Mouse Systems, Logitech CyberMan, Genius, ...) that a Microsoft
        // driver doesn't have: programs probe them, and the answer is "not supported" (registers
        // unchanged). The list of function numbers is DOSBox Staging's.
        ax if IGNORED_VENDOR_FUNCTIONS.contains(&ax) => {}
        ax => m.diagnostic(format!("INT 33h AX={ax:04X}h is not implemented")),
    }
}

fn handler_from(mask: u16, segment: u16, offset: u16) -> Option<Handler> {
    (mask != 0 && (segment, offset) != (0, 0)).then_some(Handler { mask, segment, offset })
}

/// VGA registers the graphics cursor changes, saved around drawing (DOSBox
/// `save_vga_registers`): graphics controller 0-8 and the map mask.
struct SavedRegisters {
    gc: [u8; 9],
    map_mask: u8,
}

fn save_vga_registers(m: &mut Machine) -> SavedRegisters {
    let vga = &mut m.devices.vga;
    let saved = SavedRegisters { gc: vga.gc, map_mask: vga.seq[2] };
    vga.gc[3] = 0; // no rotation or logical operation
    vga.gc[5] &= 0xF0; // read and write mode 0
    vga.seq[2] = 0x0F;
    saved
}

fn restore_vga_registers(m: &mut Machine, saved: SavedRegisters) {
    m.devices.vga.gc = saved.gc;
    m.devices.vga.seq[2] = saved.map_mask;
}

/// Draws the cursor at the current position, saving what it covers. Graphics cursors go
/// through the BIOS pixel functions in any graphics mode, as DOSBox's `draw_cursor` does.
fn draw(m: &mut Machine) {
    if m.devices.vga.display_kind() == DisplayKind::Text {
        let columns = i32::from(m.mem.read_u16(bios::BDA_COLUMNS).max(1));
        let (column, row) = (m.mouse.x / 8, m.mouse.y / 8);
        let addr = TEXT_BASE + ((row * columns + column) * 2) as u32;
        let cell = m.mem.read_u16(addr);
        let (screen, cursor) = m.mouse.text_masks;
        m.mem.write_u16(addr, (cell & screen) ^ cursor);
        m.mouse.drawn = Some(Background::Text(addr, cell));
        return;
    }
    let (width, height) = bios::graphics_size(m.devices.vga.mode);
    let x_ratio = (640 / width).max(1);
    let (hx, hy) = m.mouse.hot_spot;
    let (left, top) = (m.mouse.x / x_ratio - hx, m.mouse.y - hy);
    let registers = save_vga_registers(m);
    let mut saved = Vec::new();
    for row in 0..16 {
        for column in 0..16 {
            let (x, y) = (left + column, top + row);
            if (0..width).contains(&x) && (0..height).contains(&y) {
                saved.push((x as u16, y as u16, bios::get_pixel(m, x as u16, y as u16, 0)));
            }
        }
    }
    for &(x, y, old) in &saved {
        let (row, bit) = ((i32::from(y) - top) as usize, 0x8000u16 >> (i32::from(x) - left));
        let keep = m.mouse.screen_mask[row] & bit != 0;
        let set = m.mouse.cursor_mask[row] & bit != 0;
        let pixel = (if keep { old } else { 0 }) ^ if set { 0x0F } else { 0 };
        bios::put_pixel(m, x, y, 0, pixel);
    }
    restore_vga_registers(m, registers);
    m.mouse.drawn = Some(Background::Pixels(saved));
}

/// Restores what the cursor covered.
fn erase(m: &mut Machine) {
    match m.mouse.drawn.take() {
        Some(Background::Text(addr, cell)) => m.mem.write_u16(addr, cell),
        Some(Background::Pixels(saved)) => {
            let registers = save_vga_registers(m);
            for (x, y, color) in saved {
                bios::put_pixel(m, x, y, 0, color);
            }
            restore_vga_registers(m, registers);
        }
        None => {}
    }
}
