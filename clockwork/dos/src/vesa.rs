//! VESA BIOS Extensions 2.0 on the emulated S3 Trio64: the SVGA mode list, SVGA mode sets and
//! the INT 10h AH=4Fh functions.
//!
//! Ported from DOSBox Staging `src/ints/int10_vesa.cpp`, the mode list and the S3 parts of
//! `INT10_SetVideoMode` and `finish_set_mode` in `src/ints/int10_modes.cpp`, and the mode filters
//! in `src/hardware/video/vga_s3.cpp` (GPL-2.0-or-later). As with DOSBox's default
//! `vesa_modes = compatible`, the VBE 2.0 modes are the widely compatible ones, and modes that
//! need more than the card's 4 MB are left out.
//!
//! Differences from DOSBox: the protected-mode interface (4F0Ah) is plain 32-bit code in the ROM
//! instead of callbacks, so programs can copy it and run it anywhere; 4F06h's offset is limited to
//! the S3's 10-bit register; and 4F04h (save/restore state) is not implemented (reported).

use crate::bios::{self, ROM_SEGMENT};
use crate::machine::Machine;
use crate::vga::{self, LFB_BASE, VRAM_SIZE};

/// How a mode stores pixels (DOSBox's `VGAModes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ModeType {
    Text,
    Lin4,
    Lin8,
    Lin15,
    Lin16,
    Lin24,
    Lin32,
}

/// Each pixel is shown twice horizontally (DOSBox `VGA_PIXEL_DOUBLE`).
const PIXEL_DOUBLE: u8 = 1;
/// Each line is shown twice (`EGA_LINE_DOUBLE`).
const LINE_DOUBLE: u8 = 2;

/// One SVGA mode: DOSBox's `VideoModeBlock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModeBlock {
    pub mode: u16,
    pub kind: ModeType,
    pub width: u16,
    pub height: u16,
    pub columns: u8,
    pub rows: u8,
    pub char_width: u8,
    pub char_height: u8,
    pub pages: u8,
    pub page_length: u32,
    pub htotal: u16,
    pub vtotal: u16,
    pub hdispend: u16,
    pub vdispend: u16,
    special: u8,
}

const fn mode(
    mode: u16,
    kind: ModeType,
    size: (u16, u16, u8, u8, u8, u8),
    pages: u8,
    page_length: u32,
    timing: (u16, u16, u16, u16),
    special: u8,
) -> ModeBlock {
    let (width, height, columns, rows, char_width, char_height) = size;
    let (htotal, vtotal, hdispend, vdispend) = timing;
    ModeBlock {
        mode,
        kind,
        width,
        height,
        columns,
        rows,
        char_width,
        char_height,
        pages,
        page_length,
        htotal,
        vtotal,
        hdispend,
        vdispend,
        special,
    }
}

use ModeType::{Lin4, Lin8, Lin15, Lin16, Lin24, Lin32, Text};
const PD_LD: u8 = PIXEL_DOUBLE | LINE_DOUBLE;
const P: u32 = 0x1_0000;

/// DOSBox's `ModeList_VGA` from mode 19h on (the standard modes are set from the BIOS parameter
/// table), as `filter_compatible_vesa_modes` leaves it.
pub const MODES: &[ModeBlock] = &[
    mode(0x019, Text, (720, 688, 80, 43, 9, 16), 1, 0x4000, (100, 765, 80, 688), 0),
    mode(0x043, Text, (640, 480, 80, 60, 8, 8), 2, 0x4000, (100, 525, 80, 480), 0),
    mode(0x054, Text, (1056, 344, 132, 43, 8, 8), 1, 0x4000, (160, 449, 132, 344), 0),
    mode(0x055, Text, (1056, 400, 132, 25, 8, 16), 1, 0x2000, (160, 522, 132, 400), 0),
    mode(0x064, Text, (1056, 480, 132, 60, 8, 8), 2, 0x4000, (160, 611, 132, 480), 0),
    mode(0x069, Lin8, (640, 480, 80, 30, 8, 16), 1, P, (100, 525, 80, 480), 0),
    mode(0x06A, Lin4, (800, 600, 100, 37, 8, 16), 1, P, (128, 663, 100, 600), 0),
    mode(0x070, Text, (720, 392, 80, 28, 9, 14), 1, 0x4000, (100, 440, 80, 392), 0),
    mode(0x071, Text, (720, 480, 80, 30, 9, 16), 1, 0x4000, (100, 525, 80, 480), 0),
    mode(0x072, Text, (720, 476, 80, 34, 9, 14), 1, 0x4000, (100, 534, 80, 476), 0),
    mode(0x073, Text, (640, 344, 80, 43, 8, 8), 1, 0x4000, (100, 386, 80, 344), 0),
    mode(0x074, Text, (640, 400, 80, 50, 8, 8), 8, 0x1000, (100, 449, 80, 400), 0),
    // VESA 1.2.
    mode(0x100, Lin8, (640, 400, 80, 25, 8, 16), 1, P, (100, 449, 80, 400), 0),
    mode(0x101, Lin8, (640, 480, 80, 30, 8, 16), 1, P, (100, 525, 80, 480), 0),
    mode(0x102, Lin4, (800, 600, 100, 37, 8, 16), 1, P, (132, 628, 100, 600), 0),
    mode(0x103, Lin8, (800, 600, 100, 37, 8, 16), 1, P, (132, 628, 100, 600), 0),
    mode(0x104, Lin4, (1024, 768, 128, 48, 8, 16), 1, P, (168, 806, 128, 768), 0),
    mode(0x105, Lin8, (1024, 768, 128, 48, 8, 16), 1, P, (168, 806, 128, 768), 0),
    mode(0x106, Lin4, (1280, 1024, 160, 64, 8, 16), 1, P, (212, 1066, 160, 1024), 0),
    mode(0x107, Lin8, (1280, 1024, 160, 64, 8, 16), 1, P, (212, 1066, 160, 1024), 0),
    mode(0x108, Text, (640, 480, 80, 60, 8, 8), 2, 0x4000, (100, 525, 80, 480), 0),
    mode(0x109, Text, (1056, 400, 132, 25, 8, 16), 1, 0x2000, (160, 449, 132, 400), 0),
    mode(0x10A, Text, (1056, 688, 132, 43, 8, 16), 1, 0x4000, (160, 876, 132, 688), 0),
    mode(0x10B, Text, (1056, 400, 132, 50, 8, 8), 1, 0x4000, (160, 512, 132, 400), 0),
    mode(0x10C, Text, (1056, 480, 132, 60, 8, 8), 2, 0x4000, (160, 531, 132, 480), 0),
    mode(0x110, Lin15, (640, 480, 80, 30, 8, 16), 1, P, (200, 525, 160, 480), 0),
    mode(0x111, Lin16, (640, 480, 80, 30, 8, 16), 1, P, (200, 525, 160, 480), 0),
    mode(0x112, Lin32, (640, 480, 80, 30, 8, 16), 1, P, (100, 525, 80, 480), 0),
    mode(0x113, Lin15, (800, 600, 100, 37, 8, 16), 1, P, (264, 628, 200, 600), 0),
    mode(0x114, Lin16, (800, 600, 100, 37, 8, 16), 1, P, (264, 628, 200, 600), 0),
    mode(0x115, Lin32, (800, 600, 100, 37, 8, 16), 1, P, (132, 628, 100, 600), 0),
    mode(0x116, Lin15, (1024, 768, 128, 48, 8, 16), 1, P, (336, 806, 256, 768), 0),
    mode(0x117, Lin16, (1024, 768, 128, 48, 8, 16), 1, P, (336, 806, 256, 768), 0),
    mode(0x118, Lin32, (1024, 768, 128, 48, 8, 16), 1, P, (168, 806, 128, 768), 0),
    mode(0x119, Lin15, (1280, 1024, 160, 64, 8, 16), 1, P, (212, 1066, 320, 1024), 0),
    mode(0x11A, Lin16, (1280, 1024, 160, 64, 8, 16), 1, P, (212, 1066, 320, 1024), 0),
    mode(0x11B, Lin32, (1280, 1024, 160, 64, 8, 16), 1, P, (212, 1066, 160, 1024), 0),
    mode(0x120, Lin8, (1600, 1200, 200, 75, 8, 16), 1, P, (264, 1250, 200, 1200), 0),
    mode(0x121, Lin15, (1600, 1200, 200, 75, 8, 16), 1, P, (264, 1250, 400, 1200), 0),
    mode(0x122, Lin16, (1600, 1200, 200, 75, 8, 16), 1, P, (264, 1250, 400, 1200), 0),
    // VESA 2.0 (the compatible subset).
    mode(0x151, Lin8, (320, 240, 40, 30, 8, 8), 1, P, (100, 525, 80, 480), PD_LD),
    mode(0x155, Lin15, (320, 240, 40, 30, 8, 8), 1, P, (100, 525, 80, 480), PD_LD),
    mode(0x156, Lin16, (320, 240, 40, 30, 8, 8), 1, P, (100, 525, 80, 480), PD_LD),
    mode(0x157, Lin24, (320, 240, 40, 30, 8, 8), 1, P, (100, 525, 40, 480), PD_LD),
    mode(0x158, Lin32, (320, 240, 40, 30, 8, 8), 1, P, (100, 525, 40, 480), PD_LD),
    mode(0x160, Lin15, (320, 240, 40, 30, 8, 8), 1, P, (100, 525, 80, 480), PD_LD),
    mode(0x166, Lin8, (400, 300, 50, 37, 8, 8), 1, P, (132, 628, 100, 600), PD_LD),
    mode(0x167, Lin15, (400, 300, 50, 37, 8, 8), 1, P, (132, 628, 100, 600), PD_LD),
    mode(0x168, Lin16, (400, 300, 50, 37, 8, 8), 1, P, (132, 628, 100, 600), PD_LD),
    mode(0x169, Lin24, (400, 300, 50, 37, 8, 8), 1, P, (132, 628, 50, 600), PD_LD),
    mode(0x16A, Lin32, (400, 300, 50, 37, 8, 8), 1, P, (132, 628, 50, 600), PD_LD),
    mode(0x16C, Lin24, (512, 384, 64, 24, 8, 16), 1, P, (80, 449, 64, 384), 0),
    mode(0x170, Lin16, (320, 240, 40, 30, 8, 8), 1, P, (100, 525, 80, 480), PD_LD),
    mode(0x177, Lin4, (640, 480, 80, 30, 8, 16), 1, P, (100, 525, 80, 480), 0),
    mode(0x178, Lin24, (800, 600, 100, 37, 8, 16), 1, P, (132, 628, 100, 600), 0),
    mode(0x17B, Lin24, (1152, 864, 144, 54, 8, 16), 1, P, (182, 895, 144, 864), 0),
    mode(0x17C, Lin4, (1280, 960, 160, 60, 8, 16), 1, P, (212, 1000, 160, 960), 0),
    mode(0x17D, Lin8, (1280, 960, 160, 60, 8, 16), 1, P, (212, 1000, 160, 960), 0),
    mode(0x17E, Lin15, (1280, 960, 160, 60, 8, 16), 1, P, (212, 1000, 320, 960), 0),
    mode(0x17F, Lin16, (1280, 960, 160, 60, 8, 16), 1, P, (212, 1000, 320, 960), 0),
    mode(0x180, Lin24, (1280, 960, 160, 60, 8, 16), 1, P, (212, 1000, 160, 960), 0),
    mode(0x181, Lin32, (1280, 960, 160, 60, 8, 16), 1, P, (212, 1000, 160, 960), 0),
    mode(0x182, Lin24, (1280, 1024, 160, 64, 8, 16), 1, P, (212, 1066, 160, 1024), 0),
    mode(0x183, Lin4, (1600, 1200, 200, 75, 8, 16), 1, P, (264, 1250, 200, 1200), 0),
    mode(0x184, Lin24, (1600, 1200, 200, 75, 8, 16), 1, P, (264, 1250, 200, 1200), 0),
    mode(0x185, Lin32, (1600, 1200, 200, 75, 8, 16), 1, P, (264, 1250, 200, 1200), 0),
    mode(0x190, Lin32, (320, 240, 40, 30, 8, 8), 1, P, (50, 525, 40, 480), PD_LD),
    mode(0x207, Lin8, (1152, 864, 144, 54, 8, 16), 1, P, (182, 895, 144, 864), 0),
    mode(0x209, Lin15, (1152, 864, 144, 54, 8, 16), 1, P, (182, 895, 288, 864), 0),
    mode(0x20A, Lin16, (1152, 864, 144, 54, 8, 16), 1, P, (182, 895, 288, 864), 0),
    mode(0x20B, Lin32, (1152, 864, 144, 54, 8, 16), 1, P, (182, 895, 144, 864), 0),
    mode(0x212, Lin24, (640, 480, 80, 30, 8, 16), 1, P, (100, 525, 80, 480), 0),
    mode(0x213, Lin32, (640, 400, 80, 25, 8, 16), 1, P, (100, 449, 80, 400), 0),
    mode(0x215, Lin8, (512, 384, 64, 48, 8, 8), 1, P, (168, 806, 128, 768), PD_LD),
    mode(0x216, Lin15, (512, 384, 64, 24, 8, 16), 1, P, (168, 806, 128, 768), PD_LD),
];

/// The first VBE 2.0 mode number (DOSBox `Vesa20ModesStart`).
const VESA20_MODES_START: u16 = 0x150;

impl ModeBlock {
    /// Bytes the mode's picture needs (DOSBox `VideoModeMemSize`).
    fn memory_size(&self) -> usize {
        let (w, h) = (usize::from(self.width), usize::from(self.height));
        match self.kind {
            Lin4 => w * h / 2,
            Lin8 => w * h,
            Lin15 | Lin16 => w * h * 2,
            Lin24 => w * h * 3,
            Lin32 => w * h * 4,
            Text => usize::from(self.columns) * usize::from(self.rows) * 2,
        }
    }

    /// One page of the mode, rounded up to 64 KB (DOSBox `VESA_GetModePageSize`; text modes 0).
    fn page_size(&self) -> usize {
        let (w, h) = (usize::from(self.width), usize::from(self.height));
        let size = match self.kind {
            Lin4 => w * h / 8,
            Lin8 => w * h,
            Lin15 | Lin16 => w * h * 2,
            // Mode 212h has 128 extra bytes per line for Windows' S3 Trio drivers.
            Lin24 if self.mode == 0x212 => h * (w * 3 + 128),
            Lin24 => w * h * 3,
            Lin32 => w * h * 4,
            Text => 0,
        };
        if size & 0xFFFF != 0 { (size + 0x1_0000) & !0xFFFF } else { size }
    }

    /// The card has the memory for it (DOSBox `SVGA_S3_AcceptsMode`), and for VBE 2.0 modes, for
    /// all its planes (`filter_compatible_vesa_modes`).
    fn fits(&self) -> bool {
        let planes = if self.kind == Lin4 { 4 } else { 1 };
        self.memory_size() < VRAM_SIZE && (self.mode < VESA20_MODES_START || self.page_size() * planes <= VRAM_SIZE)
    }

    fn is_vesa(&self) -> bool {
        self.mode >= 0x100
    }

    /// The 256-color modes get a linear frame buffer only where three pages fit and Build
    /// engine games don't time them wrongly (DOSBox `can_triple_buffer_8bit`,
    /// `on_build_engine_denylist`).
    fn has_lfb(&self) -> bool {
        match self.kind {
            Lin8 => {
                let padding = usize::from(self.htotal);
                let needed = (usize::from(self.width) + padding) * (usize::from(self.vtotal) + padding) * 3;
                let denied = matches!((self.width, self.height), (320, 240) | (400, 300) | (512, 384));
                needed <= VRAM_SIZE && !denied
            }
            Lin15 | Lin16 | Lin24 | Lin32 => true,
            Lin4 | Text => false,
        }
    }
}

/// The SVGA mode `number`, if the card offers it.
pub fn find(number: u16) -> Option<&'static ModeBlock> {
    MODES.iter().find(|m| m.mode == number).filter(|m| m.fits())
}

/// ROM layout of the VBE data and code (BIOS segment).
const ROM_MODE_LIST: u16 = 0x9100;
const ROM_OEM_STRING: u16 = 0x9200;
/// The far-callable bank switch (`WinFuncPtr`): a host callback, then RETF.
const ROM_WINDOW_FUNCTION: u16 = 0x9220;
/// Where 4F07h/4F09h with BL=80h continue: wait for the next vertical retrace, then IRET.
pub(crate) const ROM_WAIT_RETRACE: u16 = 0x9230;
/// The protected-mode interface (4F0Ah): four offsets, then 32-bit code.
const ROM_PM_INTERFACE: u16 = 0x9260;

const OEM_STRING: &str = "S3 Incorporated. Trio64";
const VENDOR_NAME: &str = "Clockwork";
const PRODUCT_NAME: &str = "Clockwork";
const PRODUCT_REVISION: &str = env!("CARGO_PKG_VERSION");

/// Waits until the next vertical retrace starts (DOSBox's `CB_VESA_WAIT` and `CB_VESA_PM`).
const WAIT_RETRACE_16: [u8; 20] = [
    0xFB, // sti
    0x50, // push ax
    0x52, // push dx
    0xBA, 0xDA, 0x03, // mov dx, 3DAh
    0xEC, // in al, dx
    0xA8, 0x08, // test al, 8
    0x75, 0xFB, // jnz: still in the last retrace
    0xEC, // in al, dx
    0xA8, 0x08, // test al, 8
    0x74, 0xFB, // jz: until the next one
    0x5A, // pop dx
    0x58, // pop ax
    0xCF, // iret
    0x90,
];

/// In 32-bit code: if BL bit 7 is set, wait for the next vertical retrace.
const WAIT_IF_BL80_32: [u8; 27] = [
    0xF6, 0xC3, 0x80, // test bl, 80h
    0x74, 0x16, // jz past the wait
    0x66, 0x50, // push ax
    0x66, 0x52, // push dx
    0x66, 0xBA, 0xDA, 0x03, // mov dx, 3DAh
    0xEC, // in al, dx
    0xA8, 0x08, // test al, 8
    0x75, 0xFB, // jnz: still in the last retrace
    0xEC, // in al, dx
    0xA8, 0x08, // test al, 8
    0x74, 0xFB, // jz: until the next one
    0x66, 0x5A, // pop dx
    0x66, 0x58, // pop ax
];

/// The protected-mode interface: its offsets table and 32-bit code, and the code offsets.
fn pm_interface() -> (Vec<u8>, [u16; 3]) {
    let mut code = vec![0u8; 8];
    // Set window: the bank in DL goes to CR6A.
    let window = code.len() as u16;
    code.extend([
        0x52, // push edx
        0x50, // push eax
        0x88, 0xD4, // mov ah, dl
        0xB0, 0x6A, // mov al, 6Ah
        0x66, 0xBA, 0xD4, 0x03, // mov dx, 3D4h
        0x66, 0xEF, // out dx, ax
        0x58, // pop eax
        0x5A, // pop edx
        0xC3, // ret
    ]);
    // Set display start: CX = bits 0-15 (CR0D, CR0C), DX = bits 16-31 (CR69); then the wait.
    let start = code.len() as u16;
    code.extend([
        0x50, // push eax
        0x52, // push edx
        0x66, 0xBA, 0xD4, 0x03, // mov dx, 3D4h
        0xB0, 0x0D, // mov al, 0Dh
        0x88, 0xCC, // mov ah, cl
        0x66, 0xEF, // out dx, ax
        0xB0, 0x0C, // mov al, 0Ch
        0x88, 0xEC, // mov ah, ch
        0x66, 0xEF, // out dx, ax
        0x5A, // pop edx
        0x52, // push edx
        0x88, 0xD4, // mov ah, dl
        0x80, 0xE4, 0x1F, // and ah, 1Fh
        0xB0, 0x69, // mov al, 69h
        0x66, 0xBA, 0xD4, 0x03, // mov dx, 3D4h
        0x66, 0xEF, // out dx, ax
        0x5A, // pop edx
        0x58, // pop eax
    ]);
    code.extend(WAIT_IF_BL80_32);
    code.push(0xC3); // ret
    // Set palette: the wait first, then CX entries (blue, green, red, unused) from ES:EDI to the
    // DAC from index DL.
    let palette = code.len() as u16;
    code.extend(WAIT_IF_BL80_32);
    code.extend([
        0x50, // push eax
        0x51, // push ecx
        0x52, // push edx
        0x57, // push edi
        0x0F, 0xB7, 0xC9, // movzx ecx, cx
        0xE3, 0x1B, // jecxz done
        0x88, 0xD0, // mov al, dl
        0x66, 0xBA, 0xC8, 0x03, // mov dx, 3C8h
        0xEE, // out dx, al
        0x42, // inc edx
        0x26, 0x8A, 0x47, 0x02, // next: mov al, es:[edi+2]
        0xEE, // out dx, al
        0x26, 0x8A, 0x47, 0x01, // mov al, es:[edi+1]
        0xEE, // out dx, al
        0x26, 0x8A, 0x07, // mov al, es:[edi]
        0xEE, // out dx, al
        0x83, 0xC7, 0x04, // add edi, 4
        0xE2, 0xED, // loop next
        0x5F, // done: pop edi
        0x5A, // pop edx
        0x59, // pop ecx
        0x58, // pop eax
        0xC3, // ret
    ]);
    code[0..2].copy_from_slice(&window.to_le_bytes());
    code[2..4].copy_from_slice(&start.to_le_bytes());
    code[4..6].copy_from_slice(&palette.to_le_bytes());
    // Offset 6: no I/O ports or memory list (0).
    (code, [window, start, palette])
}

/// Puts the mode list, OEM string and VBE code into the BIOS ROM.
pub(crate) fn install(m: &mut Machine) {
    let mut list: Vec<u8> =
        MODES.iter().filter(|b| b.is_vesa() && b.fits()).flat_map(|b| b.mode.to_le_bytes()).collect();
    list.extend(0xFFFFu16.to_le_bytes());
    assert!(list.len() <= usize::from(ROM_OEM_STRING - ROM_MODE_LIST));
    m.mem.load(Machine::lin(ROM_SEGMENT, ROM_MODE_LIST), &list);
    let mut oem = OEM_STRING.as_bytes().to_vec();
    oem.push(0);
    m.mem.load(Machine::lin(ROM_SEGMENT, ROM_OEM_STRING), &oem);
    let mut window = bios::callback_instruction(bios::callback::VESA_WINDOW).to_vec();
    window.push(0xCB); // retf
    m.mem.load(Machine::lin(ROM_SEGMENT, ROM_WINDOW_FUNCTION), &window);
    m.mem.load(Machine::lin(ROM_SEGMENT, ROM_WAIT_RETRACE), &WAIT_RETRACE_16);
    let (interface, _) = pm_interface();
    m.mem.load(Machine::lin(ROM_SEGMENT, ROM_PM_INTERFACE), &interface);
}

// ---------------------------------------------------------------------------------------------
// Mode sets (DOSBox `INT10_SetVideoMode` for an S3 card).

fn port(m: &mut Machine, port: u16, value: u8) {
    m.devices.vga.write(port, value);
}

fn crtc(m: &mut Machine, index: u8, value: u8) {
    port(m, 0x3D4, index);
    port(m, 0x3D5, value);
}

fn read_crtc(m: &mut Machine, index: u8) -> u8 {
    port(m, 0x3D4, index);
    m.devices.vga.read(0x3D5, 0)
}

/// S3 accelerator settings in CR50 (DOSBox `S3_XGA_*`).
fn xga_mode(kind: Option<ModeType>, width: u16) -> u8 {
    let depth = match kind {
        Some(Lin15 | Lin16) => 0x10,
        Some(Lin24 | Lin32) => 0x30,
        _ => 0x00,
    };
    let screen = match width {
        640 => 0x40,
        800 => 0x80,
        1024 => 0x00,
        1152 => 0x01,
        1280 => 0xC0,
        1600 => 0x81,
        _ => 0x00,
    };
    depth | screen
}

/// The S3 registers DOSBox's mode set programs for every mode: MMIO off, overflow and offset
/// bits, pixel format, bank 0, the linear frame buffer address, the BIOS flags, the memory
/// configuration and the register locks. `svga` is the SVGA mode being set, if any.
pub(crate) fn s3_mode_registers(m: &mut Machine, svga: Option<&ModeBlock>, offset: u16, overflow: (u8, u8)) {
    let kind = svga.map(|b| b.kind);
    crtc(m, 0x53, 0);
    crtc(m, 0x5D, overflow.0);
    crtc(m, 0x5E, overflow.1);
    crtc(m, 0x51, ((offset & 0x300) >> 4) as u8);
    crtc(m, 0x69, 0);
    let misc_control_2 = match kind {
        Some(Lin15) => 0x30,
        Some(Lin16) => 0x50,
        Some(Lin24) => 0x70,
        Some(Lin32) => 0xD0,
        _ => 0x00,
    };
    crtc(m, 0x67, misc_control_2);
    crtc(m, 0x6A, 0);
    crtc(m, 0x59, (LFB_BASE >> 24) as u8);
    crtc(m, 0x5A, (LFB_BASE >> 16) as u8);
    crtc(m, 0x6B, (LFB_BASE >> 24) as u8);
    crtc(m, 0x41, 0x88);
    crtc(m, 0x52, 0x80);
    port(m, 0x3C4, 0x15);
    port(m, 0x3C5, 0x03);
    crtc(m, 0x50, xga_mode(kind, svga.map_or(0, |b| b.width)));
    let reg_3a = match svga {
        Some(b) if b.kind == Lin8 && b.special & PIXEL_DOUBLE != 0 => 0x05,
        Some(b) if matches!(b.kind, Lin8 | Lin15 | Lin16 | Lin24 | Lin32) => 0x15,
        _ => 0x05,
    };
    let reg_31 = if matches!(kind, Some(Lin4 | Lin8 | Lin15 | Lin16 | Lin24 | Lin32)) { 0x09 } else { 0x05 };
    crtc(m, 0x3A, reg_3a);
    crtc(m, 0x31, reg_31);
    crtc(m, 0x58, 0x03);
    crtc(m, 0x38, 0x48);
    crtc(m, 0x39, 0xA5);
}

/// Sets an SVGA (or extended text) mode from the table (DOSBox `INT10_SetVideoMode`, VGA path).
pub(crate) fn set_mode(m: &mut Machine, block: &ModeBlock, clear: bool) {
    let b = *block;
    let text = b.kind == Text;
    let mut misc_output: u8 = 0x03;
    if text && b.char_width == 9 {
        misc_output |= 0x04; // 28 MHz clock for 9-pixel characters
    }
    misc_output |= match b.vdispend {
        480 => 0xE0,
        350 => 0xA0,
        _ => 0x60,
    };
    port(m, 0x3C2, misc_output);

    // Sequencer.
    let mut seq = [0u8; 5];
    seq[1] |= 0x01;
    seq[4] |= 0x02;
    match b.kind {
        Text => {
            if b.char_width == 9 {
                seq[1] &= !0x01;
            }
            seq[2] |= 0x03;
            seq[4] |= 0x01;
        }
        Lin4 => {
            seq[2] |= 0x0F;
            seq[4] |= 0x04;
        }
        _ => {
            seq[2] |= 0x0F;
            seq[4] |= 0x0C;
        }
    }
    for (index, value) in seq.into_iter().enumerate() {
        port(m, 0x3C4, index as u8);
        port(m, 0x3C5, value);
    }

    // CRTC timing.
    let protect = read_crtc(m, 0x11);
    crtc(m, 0x11, protect & 0x7F);
    for index in 0..=0x18 {
        crtc(m, index, 0);
    }
    let (mut overflow, mut max_scanline, mut ver_overflow, mut hor_overflow) = (0u8, 0u8, 0u8, 0u8);
    let bit = |value: u16, from: u16, to: u8| (((value >> from) & 1) as u8) << to;
    crtc(m, 0x00, (b.htotal - 5) as u8);
    hor_overflow |= bit(b.htotal - 5, 8, 0);
    crtc(m, 0x01, (b.hdispend - 1) as u8);
    hor_overflow |= bit(b.hdispend - 1, 8, 1);
    crtc(m, 0x02, b.hdispend as u8);
    hor_overflow |= bit(b.hdispend, 8, 2);
    let blank_end = (b.htotal - 2) & 0x7F;
    crtc(m, 0x03, 0x80 | (blank_end & 0x1F) as u8);
    let retrace_start = if text { b.hdispend + 5 } else { b.hdispend + 4 };
    crtc(m, 0x04, retrace_start as u8);
    hor_overflow |= bit(retrace_start, 8, 4);
    let retrace_end = if text { (b.htotal - 3) & 0x1F } else { (b.htotal - 4) & 0x1F };
    crtc(m, 0x05, (retrace_end | ((blank_end & 0x20) << 2)) as u8);
    let vtotal = b.vtotal - 2;
    crtc(m, 0x06, vtotal as u8);
    overflow |= bit(vtotal, 8, 0) | bit(vtotal, 9, 5);
    ver_overflow |= bit(vtotal, 10, 0);
    let vretrace = b.vdispend
        + match b.vdispend {
            480 => 10,
            350 => 37,
            _ => 12,
        };
    crtc(m, 0x10, vretrace as u8);
    overflow |= bit(vretrace, 8, 2) | bit(vretrace, 9, 7);
    ver_overflow |= bit(vretrace, 10, 4);
    crtc(m, 0x11, ((vretrace + 2) & 0x0F) as u8);
    let vdispend = b.vdispend - 1;
    crtc(m, 0x12, vdispend as u8);
    overflow |= bit(vdispend, 8, 1) | bit(vdispend, 9, 6);
    ver_overflow |= bit(vdispend, 10, 1);
    let trim = match b.vdispend {
        400 => 6,
        480 => 7,
        350 => 5,
        _ => 8,
    };
    let vblank = b.vdispend + trim;
    crtc(m, 0x15, vblank as u8);
    overflow |= bit(vblank, 8, 3);
    max_scanline |= bit(vblank, 9, 5);
    ver_overflow |= bit(vblank, 10, 2);
    crtc(m, 0x16, (b.vtotal - trim - 2) as u8);
    let line_compare: u16 = if b.vtotal < 1024 { 1023 } else { 2047 };
    crtc(m, 0x18, line_compare as u8);
    overflow |= bit(line_compare, 8, 4);
    max_scanline |= bit(line_compare, 9, 6);
    ver_overflow |= bit(line_compare, 10, 6);
    if b.special & LINE_DOUBLE != 0 {
        max_scanline |= 0x80;
    }
    let underline = match b.kind {
        Text => {
            max_scanline |= b.char_height - 1;
            0x1F
        }
        Lin8 | Lin15 | Lin16 | Lin24 | Lin32 => 0x60,
        Lin4 if b.vdispend == 350 => 0x0F,
        Lin4 => 0,
    };
    crtc(m, 0x09, max_scanline);
    crtc(m, 0x14, underline);
    crtc(m, 0x07, overflow);
    let offset = match b.kind {
        Lin8 => b.width / 8,
        Lin15 | Lin16 => 2 * b.width / 8,
        Lin24 if b.mode == 0x212 => 3 * b.width / 8 + 16,
        Lin24 => 3 * b.width / 8,
        Lin32 => 4 * b.width / 8,
        Text | Lin4 => b.hdispend / 2,
    };
    crtc(m, 0x13, offset as u8);
    let mut mode_control = if b.kind == Lin4 { 0xE3 } else { 0xA3 };
    if b.special & PIXEL_DOUBLE != 0 {
        mode_control |= 0x08;
    }
    crtc(m, 0x17, mode_control);
    let protect = read_crtc(m, 0x11);
    crtc(m, 0x11, protect | 0x80);
    if b.is_vesa() {
        if b.vdispend > 480 {
            misc_output |= 0xC0;
        }
        misc_output |= 0x0C; // clock 3
    }
    port(m, 0x3C2, misc_output);

    // Graphics controller.
    let mut gfx = [0u8; 9];
    gfx[7] = 0x0F;
    gfx[8] = 0xFF;
    match b.kind {
        Text => {
            gfx[5] |= 0x10;
            gfx[6] |= 0x0E;
        }
        Lin4 => gfx[6] |= 0x05,
        _ => {
            gfx[5] |= 0x40;
            gfx[6] |= if b.is_vesa() { 0x01 } else { 0x05 };
        }
    }
    for (index, value) in gfx.into_iter().enumerate() {
        port(m, 0x3CE, index as u8);
        port(m, 0x3CF, value);
    }

    // Attribute controller and DAC.
    let mut attr = [0u8; 0x15];
    attr[0x12] = 0x0F;
    match b.kind {
        Lin8 | Lin15 | Lin16 | Lin24 | Lin32 => {
            for (i, a) in attr.iter_mut().take(16).enumerate() {
                *a = i as u8;
            }
            attr[0x10] = 0x41;
        }
        Text | Lin4 => {
            if b.kind == Text {
                if b.char_width == 9 {
                    attr[0x13] = 0x08;
                    attr[0x10] = 0x0C;
                } else {
                    attr[0x10] = 0x08;
                }
                m.mem.write_u8(bios::BDA_CGA_PALETTE, 0x30);
            } else {
                attr[0x10] = 0x01;
            }
            for i in 0..8u8 {
                attr[usize::from(i)] = i;
                attr[usize::from(i) + 8] = i + 0x38;
            }
            attr[0x06] = 0x14;
        }
    }
    m.devices.vga.read(0x3DA, 0);
    for (index, value) in attr.into_iter().enumerate() {
        port(m, 0x3C0, index as u8);
        port(m, 0x3C0, value);
    }
    port(m, 0x3C0, 0x20);
    port(m, 0x3C6, 0xFF);
    let vga = &mut m.devices.vga;
    match b.kind {
        Text | Lin4 => vga.dac[..64].copy_from_slice(&vga::ega_palette()),
        _ => vga.dac = vga::default_256_palette(),
    }

    s3_mode_registers(m, Some(&b), offset, (hor_overflow, ver_overflow));

    // finish_set_mode.
    if clear {
        if text {
            for i in 0..0x4000u32 {
                m.mem.write_u16(vga::TEXT_BASE + i * 2, 0x0720);
            }
        } else {
            m.devices.vga.clear_memory();
        }
    }
    let bda_mode = if b.mode < 128 { b.mode as u8 } else { (b.mode - 0x98) as u8 };
    m.mem.write_u8(bios::BDA_VIDEO_MODE, bda_mode);
    m.mem.write_u16(bios::BDA_COLUMNS, u16::from(b.columns));
    m.mem.write_u16(bios::BDA_PAGE_SIZE, b.page_length as u16);
    m.mem.write_u16(bios::BDA_CRTC_PORT, 0x3D4);
    m.mem.write_u8(bios::BDA_ROWS, b.rows - 1);
    m.mem.write_u16(bios::BDA_CHAR_HEIGHT, u16::from(b.char_height));
    m.mem.write_u8(bios::BDA_VIDEO_CONTROL, 0x60 | if clear { 0 } else { 0x80 });
    m.devices.vga.mode = b.mode;
    if text {
        bios::load_rom_font(m, 0, b.char_height);
    }
    bios::after_mode_set(m, text, b.char_height);
}

// ---------------------------------------------------------------------------------------------
// INT 10h AH=4Fh.

const SUCCESS: u8 = 0x00;
const FAIL: u8 = 0x01;
const HW_UNSUPPORTED: u8 = 0x02;
const MODE_UNSUPPORTED: u8 = 0x03;
const UNIMPLEMENTED: u8 = 0xFF;

/// The current mode's type, size and character height, for 4F06h and 4F07h (DOSBox's
/// `CurMode`). Standard modes other than text have no VESA type.
fn current_mode(m: &Machine) -> Option<ModeBlock> {
    let number = m.devices.vga.mode;
    if let Some(block) = find(number) {
        return Some(*block);
    }
    let text = matches!(number, 0..=3 | 7);
    text.then(|| {
        let columns = m.mem.read_u16(bios::BDA_COLUMNS) as u8;
        let rows = m.mem.read_u8(bios::BDA_ROWS) + 1;
        let height = m.mem.read_u8(bios::BDA_CHAR_HEIGHT);
        mode(
            number,
            Text,
            (u16::from(columns) * 9, u16::from(rows) * u16::from(height), columns, rows, 9, height),
            8,
            0x1000,
            (0, 0, 0, 0),
            0,
        )
    })
}

/// INT 10h AH=4Fh: the VBE functions.
pub(crate) fn service(m: &mut Machine) {
    let r = m.cpu.regs;
    let status = match r.al() {
        0x00 => controller_info(m, r.es(), r.di()),
        0x01 => mode_info(m, r.cx(), r.es(), r.di()),
        0x02 => {
            if bios::set_video_mode(m, r.bx()) {
                m.devices.vga.vesa_mode = Some(r.bx() & 0x7FFF);
                SUCCESS
            } else {
                FAIL
            }
        }
        0x03 => {
            let mode = m.devices.vga.vesa_mode.unwrap_or(m.devices.vga.mode);
            m.cpu.regs.set_bx(mode);
            SUCCESS
        }
        0x04 => {
            m.diagnostic("VBE function 4F04h (save/restore state) is not implemented".into());
            FAIL
        }
        0x05 => window(m),
        0x06 => scan_line_length(m),
        0x07 => display_start(m),
        0x09 => palette(m),
        0x0A => pm_interface_info(m),
        al => {
            m.diagnostic(format!("VBE function 4F{al:02X}h is not implemented"));
            m.cpu.regs.set_al(0);
            return;
        }
    };
    m.cpu.regs.set_al(0x4F);
    m.cpu.regs.set_ah(status);
}

fn write_string(m: &mut Machine, segment: u16, at: &mut u16, text: &str) {
    for &byte in text.as_bytes().iter().chain([&0]) {
        m.mem.write_u8(Machine::lin(segment, *at), byte);
        *at = at.wrapping_add(1);
    }
}

fn far(segment: u16, offset: u16) -> u32 {
    (u32::from(segment) << 16) | u32::from(offset)
}

/// 4F00h: the VbeInfoBlock at ES:DI (DOSBox `VESA_GetSVGAInformation`).
fn controller_info(m: &mut Machine, segment: u16, offset: u16) -> u8 {
    let buffer = Machine::lin(segment, offset);
    let signature = m.mem.read_u32(buffer);
    let vbe2 = signature == u32::from_le_bytes(*b"VBE2") || signature == u32::from_le_bytes(*b"2EBV");
    for i in 0..if vbe2 { 0x200 } else { 0x100 } {
        m.mem.write_u8(buffer + i, 0);
    }
    m.mem.load(buffer, b"VESA");
    m.mem.write_u16(buffer + 0x04, 0x0200);
    if vbe2 {
        let mut at = offset.wrapping_add(0x100);
        m.mem.write_u32(buffer + 0x06, far(segment, at));
        write_string(m, segment, &mut at, OEM_STRING);
        m.mem.write_u16(buffer + 0x14, 0x0200);
        m.mem.write_u32(buffer + 0x16, far(segment, at));
        write_string(m, segment, &mut at, VENDOR_NAME);
        m.mem.write_u32(buffer + 0x1A, far(segment, at));
        write_string(m, segment, &mut at, PRODUCT_NAME);
        m.mem.write_u32(buffer + 0x1E, far(segment, at));
        write_string(m, segment, &mut at, PRODUCT_REVISION);
    } else {
        m.mem.write_u32(buffer + 0x06, far(ROM_SEGMENT, ROM_OEM_STRING));
    }
    m.mem.write_u32(buffer + 0x0A, 0);
    m.mem.write_u32(buffer + 0x0E, far(ROM_SEGMENT, ROM_MODE_LIST));
    m.mem.write_u16(buffer + 0x12, (VRAM_SIZE / 0x1_0000) as u16);
    SUCCESS
}

/// 4F01h: the ModeInfoBlock of mode CX at ES:DI (DOSBox `VESA_GetSVGAModeInformation`).
fn mode_info(m: &mut Machine, number: u16, segment: u16, offset: u16) -> u8 {
    let number = number & 0x3FFF;
    let Some(b) = find(number).filter(|b| b.is_vesa()).copied() else { return FAIL };
    let mut info = [0u8; 256];
    let put16 = |info: &mut [u8; 256], at: usize, value: u16| info[at..at + 2].copy_from_slice(&value.to_le_bytes());
    let width = b.width;
    // (bytes per line, planes, bits per pixel, memory model, masks: size/position r, g, b, x)
    let (bytes_per_line, planes, bits, model, masks): (u16, u8, u8, u8, [u8; 8]) = match b.kind {
        Lin4 => (width / 8, 4, 4, 3, [0; 8]),
        Lin8 => (width, 1, 8, 4, [0; 8]),
        Lin15 => (width * 2, 1, 15, 6, [5, 10, 5, 5, 5, 0, 1, 15]),
        Lin16 => (width * 2, 1, 16, 6, [5, 11, 6, 5, 5, 0, 0, 0]),
        Lin24 if b.mode == 0x212 => (width * 3 + 128, 1, 24, 6, [8, 16, 8, 8, 8, 0, 0, 0]),
        Lin24 => (width * 3, 1, 24, 6, [8, 16, 8, 8, 8, 0, 0, 0]),
        Lin32 => (width * 4, 1, 32, 6, [8, 16, 8, 8, 8, 0, 8, 24]),
        Text => (u16::from(b.columns) * 2, 4, 4, 0, [0; 8]),
    };
    let mut attributes: u16 = if b.kind == Text { 0x0F } else { 0x1B };
    if b.has_lfb() {
        attributes |= 0x80;
    }
    let page_size = b.page_size();
    if page_size > VRAM_SIZE {
        attributes &= !0x01;
    }
    let pages = VRAM_SIZE.checked_div(page_size).map_or(0, |n| n.saturating_sub(1));
    put16(&mut info, 0x00, attributes);
    info[0x02] = 0x07; // window A: exists, readable, writable
    if b.kind == Text {
        put16(&mut info, 0x04, 32);
        put16(&mut info, 0x06, 32);
        put16(&mut info, 0x08, 0xB800);
        put16(&mut info, 0x12, u16::from(b.columns));
        put16(&mut info, 0x14, u16::from(b.rows));
    } else {
        put16(&mut info, 0x04, 64);
        put16(&mut info, 0x06, 64);
        put16(&mut info, 0x08, 0xA000);
        put16(&mut info, 0x12, b.width);
        put16(&mut info, 0x14, b.height);
    }
    info[0x0C..0x10].copy_from_slice(&far(ROM_SEGMENT, ROM_WINDOW_FUNCTION).to_le_bytes());
    put16(&mut info, 0x10, bytes_per_line);
    info[0x16] = b.char_width;
    info[0x17] = b.char_height;
    info[0x18] = planes;
    info[0x19] = bits;
    info[0x1A] = 1; // banks
    info[0x1B] = model;
    info[0x1D] = pages as u8;
    info[0x1E] = 1;
    info[0x1F..0x27].copy_from_slice(&masks);
    if attributes & 0x80 != 0 {
        info[0x28..0x2C].copy_from_slice(&LFB_BASE.to_le_bytes());
    }
    m.mem.load(Machine::lin(segment, offset), &info);
    SUCCESS
}

/// 4F05h: set (BH=0) or get (BH=1) the bank of window BL (only window A), in DX.
fn window(m: &mut Machine) -> u8 {
    let r = m.cpu.regs;
    match r.bh() {
        0 => set_window(m, r.bl(), r.dl()),
        1 if r.bl() == 0 => {
            let bank = read_crtc(m, 0x6A);
            m.cpu.regs.set_dx(u16::from(bank));
            SUCCESS
        }
        1 => FAIL,
        _ => {
            m.diagnostic(format!("VBE 4F05h subfunction BH={:02X}h is not implemented", r.bh()));
            FAIL
        }
    }
}

fn set_window(m: &mut Machine, window: u8, bank: u8) -> u8 {
    if window != 0 || usize::from(bank) * 0x1_0000 >= VRAM_SIZE {
        return FAIL;
    }
    crtc(m, 0x6A, bank);
    SUCCESS
}

/// The far-callable window function (`WinFuncPtr`): 4F05h's registers, returning like it.
pub(crate) fn window_function(m: &mut Machine) {
    let status = window(m);
    m.cpu.regs.set_al(0x4F);
    m.cpu.regs.set_ah(status);
}

/// 4F06h: set or get the logical scan line length (DOSBox `VESA_ScanLineLength`).
fn scan_line_length(m: &mut Machine) -> u8 {
    let r = m.cpu.regs;
    let Some(current) = current_mode(m) else { return MODE_UNSUPPORTED };
    let mut screen_height = u32::from(current.height);
    let mut usable = VRAM_SIZE as u32;
    let (mut bytes_per_offset, bits): (u32, u32) = (
        8,
        match current.kind {
            Text | Lin4 => 4,
            Lin8 => 8,
            Lin15 | Lin16 => 16,
            Lin24 => 24,
            Lin32 => 32,
        },
    );
    match current.kind {
        Text => {
            usable = 32 * 1024;
            screen_height = u32::from(current.rows);
            bytes_per_offset = 4;
        }
        Lin4 => {
            bytes_per_offset = 2;
            usable /= 4;
        }
        _ => {}
    }
    const GCD: u32 = 64;
    // The S3's offset register has 10 bits.
    let max_offset = (0xFFFF / bytes_per_offset).min(0xFFFF * bits / GCD).min(0x3FF);
    let mut new_offset = m.devices.vga.scan_length();
    match r.bl() {
        0x00 => {
            new_offset = u32::from(r.cx()) * bits / GCD;
            if current.kind == Lin24 {
                new_offset -= new_offset % 3;
            }
        }
        0x01 => {}
        0x02 => new_offset = u32::from(r.cx()).div_ceil(bytes_per_offset),
        0x03 => {
            new_offset = max_offset;
            if new_offset * bytes_per_offset * screen_height > usable {
                new_offset = usable / (bytes_per_offset * screen_height);
            }
        }
        _ => return UNIMPLEMENTED,
    }
    if matches!(r.bl(), 0x00 | 0x02) {
        if new_offset > max_offset {
            return HW_UNSUPPORTED;
        }
        crtc(m, 0x13, new_offset as u8);
        let ext = read_crtc(m, 0x51) & !0x30;
        crtc(m, 0x51, ext | ((new_offset >> 4) & 0x30) as u8);
    }
    let bytes = new_offset * bytes_per_offset;
    if bytes == 0 {
        return FAIL;
    }
    let supported_lines = usable / bytes;
    let gap = supported_lines % screen_height.max(1);
    let mut lines = if gap < 8 { screen_height } else { supported_lines };
    if current.kind == Text {
        lines *= u32::from(current.char_height);
    }
    m.cpu.regs.set_bx(bytes as u16);
    m.cpu.regs.set_cx((new_offset * GCD / bits) as u16);
    m.cpu.regs.set_dx(lines as u16);
    SUCCESS
}

/// Continues the interrupted INT 10h at the ROM's wait-for-retrace code, which then returns.
fn wait_for_retrace(m: &mut Machine) {
    m.cpu.regs.ip = u32::from(ROM_WAIT_RETRACE);
}

/// 4F07h: set (BL=00h, 80h: during retrace) or get (BL=01h) the display start (DOSBox
/// `VESA_SetDisplayStart`, `VESA_GetDisplayStart`).
fn display_start(m: &mut Machine) -> u8 {
    let r = m.cpu.regs;
    let Some(current) = current_mode(m) else { return MODE_UNSUPPORTED };
    match r.bl() {
        0x00 | 0x80 => {
            let (bits, panning_factor) = match current.kind {
                Text | Lin4 => (4, 1),
                Lin8 => (8, 2),
                Lin15 | Lin16 => (16, 2),
                Lin24 => (24, 1),
                Lin32 => (32, 1),
            };
            const LCF: u32 = 32;
            let (x, y) = (u32::from(r.cx()), u32::from(r.dx()));
            let scan = m.devices.vga.scan_length();
            let mut start = (scan * LCF * 2 * y + x * bits) / LCF;
            if current.kind == Lin24 {
                start -= start % 3;
            }
            crtc(m, 0x0C, (start >> 8) as u8);
            crtc(m, 0x0D, start as u8);
            crtc(m, 0x69, ((start >> 16) & 0x1F) as u8);
            m.devices.vga.read(0x3DA, 0);
            port(m, 0x3C0, 0x13 | 0x20);
            let panning = x % (LCF / bits);
            port(m, 0x3C0, (panning * panning_factor) as u8);
            if r.bl() == 0x80 {
                wait_for_retrace(m);
            }
            SUCCESS
        }
        0x01 => {
            let (pixels_per_offset, panning_factor) = match current.kind {
                Text | Lin4 => (16, 1),
                Lin8 => (8, 2),
                Lin15 | Lin16 => (4, 2),
                Lin32 => (2, 1),
                Lin24 => return MODE_UNSUPPORTED,
            };
            m.devices.vga.read(0x3DA, 0);
            port(m, 0x3C0, 0x13 | 0x20);
            let mut panning = u32::from(m.devices.vga.read(0x3C1, 0));
            if current.kind == Text && panning > 7 {
                panning = 0;
            }
            let vga = &m.devices.vga;
            let virtual_width = (vga.scan_length() * pixels_per_offset).max(1);
            let start = vga.start_address() * (pixels_per_offset / 2) + panning / panning_factor;
            m.cpu.regs.set_bh(0);
            m.cpu.regs.set_cx((start % virtual_width) as u16);
            m.cpu.regs.set_dx((start / virtual_width) as u16);
            SUCCESS
        }
        bl => {
            m.diagnostic(format!("VBE 4F07h subfunction BL={bl:02X}h is not implemented"));
            FAIL
        }
    }
}

/// 4F09h: set (BL=00h, 80h: during retrace) or get (BL=01h) CX palette entries from index DX,
/// as blue, green, red, unused at ES:DI (DOSBox `VESA_SetPalette`, `VESA_GetPalette`).
fn palette(m: &mut Machine) -> u8 {
    let r = m.cpu.regs;
    let (first, count) = (usize::from(r.dx()), usize::from(r.cx()));
    if first > 255 || first + count > 256 {
        return FAIL;
    }
    let data = Machine::lin(r.es(), r.di());
    match r.bl() {
        0x00 | 0x80 => {
            port(m, 0x3C8, first as u8);
            for i in 0..count as u32 {
                let entry = data + i * 4;
                let (blue, green, red) = (m.mem.read_u8(entry), m.mem.read_u8(entry + 1), m.mem.read_u8(entry + 2));
                for value in [red, green, blue] {
                    port(m, 0x3C9, value);
                }
            }
            if r.bl() == 0x80 {
                wait_for_retrace(m);
            }
            SUCCESS
        }
        0x01 => {
            port(m, 0x3C7, first as u8);
            for i in 0..count as u32 {
                let entry = data + i * 4;
                let [red, green, blue] = [0; 3].map(|_| m.devices.vga.read(0x3C9, 0));
                m.mem.write_u8(entry, blue);
                m.mem.write_u8(entry + 1, green);
                m.mem.write_u8(entry + 2, red);
            }
            SUCCESS
        }
        bl => {
            m.diagnostic(format!("VBE 4F09h subfunction BL={bl:02X}h is not implemented"));
            FAIL
        }
    }
}

/// 4F0Ah: where the protected-mode interface is (BL=0: the table; 1-3: one function's code).
fn pm_interface_info(m: &mut Machine) -> u8 {
    let (code, [window, start, palette]) = pm_interface();
    let size = code.len() as u16;
    let (offset, length) = match m.cpu.regs.bl() {
        0 => (0, size),
        1 => (window, start - window),
        2 => (start, palette - start),
        3 => (palette, size - palette),
        _ => return FAIL,
    };
    m.cpu.regs.set_es(ROM_SEGMENT);
    m.cpu.regs.set_di(ROM_PM_INTERFACE + offset);
    m.cpu.regs.set_cx(length);
    SUCCESS
}
