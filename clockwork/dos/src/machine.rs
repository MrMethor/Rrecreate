//! The emulated PC: CPU, memory, devices, BIOS/DOS state and the run loop.

use std::collections::{BTreeSet, VecDeque};
use std::hash::{Hash, Hasher};

use crate::hash::Fnv64;
use crate::memory::Memory;
use cpu::flags::IF;
use cpu::{Bus, Cpu, seg};

use crate::clock::{Clock, FRAME_PIT_TICKS};
use crate::cmos::Cmos;
use crate::dma::Dma;
use crate::dos::DosState;
use crate::ems::{self, Ems};
use crate::joystick::{GamePort, Stick};
use crate::keyboard::{ControllerEffect, Keyboard, LEFT_SHIFT, ascii_to_scancode};
use crate::mouse::{self, Mouse};
use crate::mpu401::Mpu401;
use crate::opl::{Opl, OplMode};
use crate::pic::Pic;
use crate::pit::Pit;
use crate::sblaster::{SbConfig, SoundBlaster};
use crate::time::DateTime;
use crate::vfs::Vfs;
use crate::vga::{self, Vga};
use crate::vga_draw;
use crate::xms::{self, Xms};
use crate::{bios, dos};

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Config {
    /// Speed of the emulated PC: one cycle is one instruction (or string repetition).
    pub instructions_per_second: u64,
    /// The emulated wall clock at cycle 0.
    pub start_time: DateTime,
    /// Which processor programs see.
    pub cpu_model: cpu::Model,
    /// Whether an x87 coprocessor is installed (BIOS and CMOS report it).
    pub fpu: bool,
    /// Letter of an emulated CD-ROM drive (with MSCDEX), read-only.
    pub cd_drive: Option<char>,
    /// The folder of C: that the CD drive shows as its root (empty: all of C:), so a game folder
    /// holding both the installed game and the disc's contents can be set up like DOSBox mounts.
    pub cd_root: String,
    /// The CD's volume label (empty: the name of its folder, or "CDROM"). Some games check it.
    pub cd_label: String,
    /// The drive that is current when the program starts.
    pub start_drive: char,
    /// The current directory when the program starts, as if `CD` ran first (empty: the root).
    /// A relative program path starts there.
    pub start_dir: String,
    /// Extra environment variables for the program, after COMSPEC, PATH and PROMPT.
    pub environment: Vec<(String, String)>,
    /// Expanded memory (LIM EMS 4.0) in KB; 0 means no EMS driver.
    pub ems_kb: u32,
    /// Memory above the first megabyte, in KB (15360 for a 16 MB PC).
    pub extended_kb: u32,
    /// Whether an XMS driver (HIMEM.SYS) manages the extended memory.
    pub xms: bool,
    /// Whether a mouse and its driver (INT 33h) are installed.
    pub mouse: bool,
    /// The Sound Blaster card (with its OPL chips), or `None` for no sound card.
    pub sound_blaster: Option<SbConfig>,
    /// Joysticks on the game port (0-2); with none the port reads FFh, as without a card.
    pub joysticks: u8,
    /// A DPMI host ([`crate::dpmi`]) for programs that look for one. Off by default: DOS
    /// extenders that bring their own (DOS/4GW) would switch to it.
    pub dpmi: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            instructions_per_second: 8_000_000,
            start_time: DateTime::new(1994, 1, 1, 8, 0, 0),
            cpu_model: cpu::Model::I486,
            fpu: true,
            cd_drive: None,
            cd_root: String::new(),
            cd_label: String::new(),
            start_drive: 'C',
            start_dir: String::new(),
            environment: Vec::new(),
            ems_kb: 4096,
            extended_kb: 15 * 1024,
            xms: true,
            mouse: true,
            sound_blaster: Some(SbConfig::default()),
            joysticks: 0,
            dpmi: false,
        }
    }
}

/// Most instructions `exec_watch` records before it stops recording.
pub const MAX_EXEC_HITS: usize = 1_000_000;

/// An instruction that ran in the watched range: when, where, the registers, and the top of the
/// stack (a near or far return address right after a call).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecHit {
    pub cycle: u64,
    pub cs: u16,
    pub ip: u32,
    pub regs: cpu::Regs,
    pub stack: [u16; 4],
}

/// One scheduled input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Input {
    /// A scancode delivered through the keyboard controller.
    Key(u8),
    /// The mouse is at (x, y) in the driver's coordinates with these buttons held (bit 0 left,
    /// bit 1 right, bit 2 middle).
    Mouse { x: i16, y: i16, buttons: u8 },
    /// Joystick `stick` (0 or 1) is at (x, y), each -32767..=32767, with these buttons held
    /// (bit 0 button 1, bit 1 button 2).
    Joystick { stick: u8, x: i16, y: i16, buttons: u8 },
    /// The mouse moved by (dx, dy) mickeys, its buttons as they were.
    MouseMotion { dx: i16, dy: i16 },
}

/// An input device whose state a program reads through a service or a port, never from memory of
/// its own (D49): [`Machine::device_state`] gives it as bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Device {
    /// The mouse driver: [`crate::mouse::Mouse::reported_state`].
    Mouse,
    /// The game port's sticks: [`crate::joystick::GamePort::reported_state`].
    Joystick,
}

impl Device {
    /// `mouse` or `joystick`.
    pub fn parse(name: &str) -> Option<Device> {
        match name {
            "mouse" => Some(Device::Mouse),
            "joystick" => Some(Device::Joystick),
            _ => None,
        }
    }
}

/// Why [`Machine::run`] returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// The program terminated with this return code.
    Exited(u8),
    /// The cycle limit was reached.
    CycleLimit,
    /// A blocking keyboard read is waiting and no more input is scheduled.
    WaitingForInput,
    /// HLT with interrupts disabled: nothing can ever wake the CPU.
    Halted,
    /// The CPU met an instruction it doesn't implement, or one asking for something the host
    /// doesn't emulate (such as protected mode).
    Fault(cpu::Exit),
    /// About to run the instruction at this linear address, a sync point, for the n-th time
    /// ([`Machine::run_to_sync`]).
    Sync { address: u32, hit: u64 },
}

/// Extended memory is limited to what a 24-bit (286/386SX) address bus reaches: 16 MB in all.
pub const MAX_EXTENDED_KB: u32 = 15 * 1024;

/// One displayed frame, fingerprinted at the start of vertical retrace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Frame {
    pub number: u64,
    pub cycle: u64,
    /// The BIOS video mode (VESA modes included).
    pub mode: u16,
    pub hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Devices {
    pub pic: Pic,
    pub pic2: Pic,
    pub pit: Pit,
    pub keyboard: Keyboard,
    pub vga: Vga,
    pub mpu: Mpu401,
    pub cmos: Cmos,
    pub dma: Dma,
    pub opl: Option<Opl>,
    pub sb: Option<SoundBlaster>,
    pub game_port: GamePort,
    pub clock: Clock,
    port92: u8,
    /// Ports accessed that nothing emulates (reads return FFh, writes are dropped).
    pub unknown_ports: BTreeSet<u16>,
    /// A device changed in a way that may move the next event earlier.
    dirty: bool,
}

impl Devices {
    fn new(config: &Config, clock: Clock) -> Self {
        let (start_time, sound_blaster, extended_kb, fpu) =
            (config.start_time, config.sound_blaster, config.extended_kb, config.fpu);
        Self {
            // BIOS defaults: IRQ0, 1, 2 and 6 unmasked on the master; vectors 08h and 70h.
            pic: Pic::new(0x08, 0xB8),
            pic2: Pic::new(0x70, 0xFF),
            pit: Pit::new(),
            keyboard: Keyboard::default(),
            vga: Vga::new(),
            mpu: Mpu401::default(),
            cmos: Cmos::new(start_time, extended_kb, fpu),
            dma: Dma::new(),
            opl: sound_blaster.map(|sb| Opl::new(sb.opl_mode())),
            sb: sound_blaster.map(SoundBlaster::new),
            game_port: GamePort::new(config.joysticks),
            clock,
            port92: 0,
            unknown_ports: BTreeSet::new(),
            dirty: false,
        }
    }

    /// Delivers the highest-priority pending interrupt, following the master/slave cascade on
    /// IRQ 2. Returns its vector.
    fn acknowledge(&mut self) -> Option<u8> {
        if self.pic2.pending_line().is_some() {
            self.pic.raise(2);
        }
        let vector = self.pic.acknowledge()?;
        if vector == self.pic.vector_base().wrapping_add(2) {
            // The slave supplies the vector; a request that went away reads as spurious IRQ 15.
            return Some(self.pic2.acknowledge().unwrap_or(self.pic2.vector_base().wrapping_add(7)));
        }
        Some(vector)
    }

    /// Brings the sound card up to `cycles` (DMA playback, timed interrupts) and passes its
    /// interrupt line changes to the PIC.
    pub(crate) fn update_sound(&mut self, cycles: u64, mem: &mut Memory) {
        if let Some(sb) = &mut self.sb {
            sb.update(self.clock.nanos(cycles), &mut self.dma, mem);
        }
        self.sync_sound_irq();
    }

    fn sync_sound_irq(&mut self) {
        let Some(sb) = &mut self.sb else { return };
        let Some(raise) = sb.irq_change.take() else { return };
        // IRQ 2 on an AT is the slave's IRQ 9.
        let irq = if sb.irq() == 2 { 9 } else { sb.irq() };
        let pic = if irq < 8 { &mut self.pic } else { &mut self.pic2 };
        if raise {
            pic.raise(irq & 7);
        } else {
            pic.lower(irq & 7);
        }
    }

    /// When the sound card next needs attention, as a cycle.
    fn next_sound_event(&self) -> Option<u64> {
        self.sb.as_ref()?.next_event().map(|ns| self.clock.cycle_at_nanos(ns))
    }

    /// The OPL chips answer at 388h-38Bh, and on the Sound Blaster at base+0..3 (Pro and 16)
    /// and base+8..9.
    fn opl_port(&self, port: u16) -> bool {
        let Some(opl) = &self.opl else { return false };
        if (0x388..=0x38B).contains(&port) {
            return true;
        }
        let Some(sb) = &self.sb else { return false };
        let offset = port.wrapping_sub(sb.config.base);
        offset == 8 || offset == 9 || (offset < 4 && opl.mode != OplMode::Opl2)
    }

    fn port_in(&mut self, port: u16, cycles: u64, mem: &mut Memory) -> u8 {
        let now = self.clock.pit_ticks(cycles);
        if self.opl_port(port) {
            let nanos = self.clock.nanos(cycles);
            return self.opl.as_mut().map_or(0xFF, |opl| opl.read(port, nanos));
        }
        if let Some(sb) = &mut self.sb
            && sb.decodes(port)
        {
            let value = sb.read(port, self.clock.nanos(cycles), &mut self.dma, mem);
            self.sync_sound_irq();
            self.dirty = true;
            return value;
        }
        if Dma::decodes(port) {
            return self.dma.read(port);
        }
        match port {
            0x20 | 0x21 => self.pic.read(port & 1 == 1),
            0xA0 | 0xA1 => self.pic2.read(port & 1 == 1),
            0x40..=0x43 => self.pit.read(port, now),
            0x60 => {
                self.dirty = true;
                self.keyboard.read_data()
            }
            0x61 => self.pit.read_port61(now),
            0x64 => self.keyboard.read_status(),
            0x92 => self.port92,
            0x3B0..=0x3BB | 0x3C0..=0x3DF => self.vga.read(port, now),
            0x71 => {
                self.dirty = true;
                self.cmos.read_data(self.clock.ticks(cycles, crate::cmos::RTC_HZ))
            }
            0x330 => self.mpu.read_data(),
            0x331 => self.mpu.read_status(),
            crate::joystick::PORT => self.game_port.read(self.clock.nanos(cycles)),
            // Absent cards read FFh.
            0x220..=0x22F | 0x388..=0x38B => 0xFF,
            _ => {
                self.unknown_ports.insert(port);
                0xFF
            }
        }
    }

    fn port_out(&mut self, port: u16, value: u8, cycles: u64, mem: &mut Memory) -> Option<ControllerEffect> {
        let now = self.clock.pit_ticks(cycles);
        if self.opl_port(port) {
            let nanos = self.clock.nanos(cycles);
            if let Some(opl) = &mut self.opl {
                opl.write(port, value, nanos);
            }
            return None;
        }
        if let Some(sb) = &mut self.sb
            && sb.decodes(port)
        {
            sb.write(port, value, self.clock.nanos(cycles), &mut self.dma, mem);
            self.sync_sound_irq();
            self.dirty = true;
            return None;
        }
        if Dma::decodes(port) {
            // Masking or reprogramming a channel pauses or resumes Sound Blaster playback.
            self.update_sound(cycles, mem);
            self.dma.write(port, value);
            self.update_sound(cycles, mem);
            self.dirty = true;
            return None;
        }
        match port {
            0x20 | 0x21 => self.pic.write(port & 1 == 1, value),
            0xA0 | 0xA1 => self.pic2.write(port & 1 == 1, value),
            0x40..=0x43 => {
                self.pit.write(port, value, now);
                self.dirty = true;
            }
            0x60 => {
                self.dirty = true;
                return self.keyboard.write_data(value);
            }
            0x61 => self.pit.write_port61(value, now),
            0x64 => {
                self.dirty = true;
                return self.keyboard.write_command(value);
            }
            0x92 => {
                self.port92 = value;
                return Some(ControllerEffect::SetA20(value & 0x02 != 0));
            }
            0x3B0..=0x3BB | 0x3C0..=0x3DF => self.vga.write(port, value),
            0x70 => self.cmos.write_index(value),
            0x71 => {
                self.cmos.write_data(value, self.clock.ticks(cycles, crate::cmos::RTC_HZ));
                self.dirty = true;
            }
            0x330 => self.mpu.write_data(value, now),
            0x331 => self.mpu.write_command(value),
            0x80 => {} // POST code port, written for short I/O delays
            crate::joystick::PORT => self.game_port.write(self.clock.nanos(cycles)),
            0x220..=0x22F | 0x388..=0x38B => {}
            _ => {
                self.unknown_ports.insert(port);
            }
        }
        None
    }
}

pub(crate) struct PcBus<'a> {
    mem: &'a mut Memory,
    dev: &'a mut Devices,
    now: u64,
}

/// Physical addresses from here up can't be RAM (16 MB at most); the linear frame buffer is
/// among them.
const ABOVE_RAM: u32 = 0x0100_0000;

/// `len` bytes at `addr` are all ordinary memory: none in the video window or above RAM.
#[inline(always)]
fn plain_ram(addr: u32, len: u32) -> bool {
    let last = addr.wrapping_add(len - 1);
    last < vga::GRAPHICS_BASE || (addr >= 0xC_0000 && last >= addr && last < ABOVE_RAM)
}

/// A byte at a physical address the way the CPU sees it (the video window, the linear frame
/// buffer, RAM), without side effects.
fn peek_device_u8(mem: &Memory, dev: &Devices, addr: u32) -> u8 {
    let vga = &dev.vga;
    let offset = addr.wrapping_sub(vga::GRAPHICS_BASE);
    match (vga.window_size(), vga.lfb_range()) {
        (Some(size), _) if offset < size => vga.peek(offset),
        (_, Some(lfb)) if lfb.contains(&addr) => vga.lfb_read(addr - lfb.start),
        _ => mem.read_u8(addr),
    }
}

impl PcBus<'_> {
    /// A byte the way the CPU would read it, without side effects (no VGA latches loaded).
    pub(crate) fn peek_u8(&self, addr: u32) -> u8 {
        peek_device_u8(self.mem, self.dev, addr)
    }

    /// The game port's sticks as a program reads them ([`Device::Joystick`]).
    pub(crate) fn joystick_state(&self) -> [u8; crate::joystick::STATE_SIZE] {
        self.dev.game_port.reported_state()
    }

    /// Whether memory tracking writes saw this byte written (true when nothing is tracked).
    pub(crate) fn was_written(&self, addr: u32) -> bool {
        self.mem.was_written(addr)
    }

    // The video window and the frame buffer are kept out of line so the common RAM path stays
    // small enough to inline.
    #[inline(never)]
    fn window_read(&mut self, addr: u32) -> u8 {
        let offset = addr - vga::GRAPHICS_BASE;
        match self.dev.vga.window_size() {
            Some(size) if offset < size => self.dev.vga.cpu_read(offset),
            _ => self.mem.read_u8(addr),
        }
    }

    #[inline(never)]
    fn window_write(&mut self, addr: u32, value: u8) {
        let offset = addr - vga::GRAPHICS_BASE;
        match self.dev.vga.window_size() {
            Some(size) if offset < size => self.dev.vga.cpu_write(offset, value),
            _ => self.mem.write_u8(addr, value),
        }
    }

    /// `N` bytes written in the window, when they all fall inside it.
    #[inline(never)]
    fn window_write_n<const N: usize>(&mut self, addr: u32, value: [u8; N]) -> bool {
        let offset = addr.wrapping_sub(vga::GRAPHICS_BASE);
        match self.dev.vga.window_size() {
            Some(size) if offset + N as u32 <= size => {
                self.dev.vga.cpu_write_n(offset, value);
                true
            }
            _ => false,
        }
    }

    #[inline(never)]
    fn high_read(&mut self, addr: u32) -> u8 {
        match self.dev.vga.lfb_range() {
            Some(lfb) if lfb.contains(&addr) => self.dev.vga.lfb_read(addr - lfb.start),
            _ => self.mem.read_u8(addr),
        }
    }

    #[inline(never)]
    fn high_write(&mut self, addr: u32, value: u8) {
        match self.dev.vga.lfb_range() {
            Some(lfb) if lfb.contains(&addr) => self.dev.vga.lfb_write(addr - lfb.start, value),
            _ => self.mem.write_u8(addr, value),
        }
    }
}

impl Bus for PcBus<'_> {
    #[inline(always)]
    fn read_u8(&mut self, addr: u32) -> u8 {
        if addr.wrapping_sub(vga::GRAPHICS_BASE) < 0x2_0000 {
            return self.window_read(addr);
        }
        if addr >= ABOVE_RAM {
            return self.high_read(addr);
        }
        self.mem.read_u8(addr)
    }

    #[inline(always)]
    fn write_u8(&mut self, addr: u32, value: u8) {
        if addr.wrapping_sub(vga::GRAPHICS_BASE) < 0x2_0000 {
            return self.window_write(addr, value);
        }
        if addr >= ABOVE_RAM {
            return self.high_write(addr, value);
        }
        self.mem.write_u8(addr, value);
    }

    #[inline(always)]
    fn code_window(&mut self, addr: u32) -> Option<[u8; 16]> {
        plain_ram(addr, 16).then(|| self.mem.read_n(addr))
    }

    #[inline(always)]
    fn read_u16(&mut self, addr: u32) -> u16 {
        if plain_ram(addr, 2) {
            return u16::from_le_bytes(self.mem.read_n(addr));
        }
        u16::from_le_bytes([self.read_u8(addr), self.read_u8(addr.wrapping_add(1))])
    }

    #[inline(always)]
    fn read_u32(&mut self, addr: u32) -> u32 {
        if plain_ram(addr, 4) {
            return u32::from_le_bytes(self.mem.read_n(addr));
        }
        u32::from_le_bytes(std::array::from_fn(|i| self.read_u8(addr.wrapping_add(i as u32))))
    }

    #[inline(always)]
    fn write_u16(&mut self, addr: u32, value: u16) {
        if plain_ram(addr, 2) {
            return self.mem.write_n(addr, value.to_le_bytes());
        }
        if addr.wrapping_sub(vga::GRAPHICS_BASE) < 0x2_0000 && self.window_write_n(addr, value.to_le_bytes()) {
            return;
        }
        let [lo, hi] = value.to_le_bytes();
        self.write_u8(addr, lo);
        self.write_u8(addr.wrapping_add(1), hi);
    }

    #[inline(always)]
    fn write_u32(&mut self, addr: u32, value: u32) {
        if plain_ram(addr, 4) {
            return self.mem.write_n(addr, value.to_le_bytes());
        }
        if addr.wrapping_sub(vga::GRAPHICS_BASE) < 0x2_0000 && self.window_write_n(addr, value.to_le_bytes()) {
            return;
        }
        for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
            self.write_u8(addr.wrapping_add(i as u32), byte);
        }
    }

    fn port_in_u8(&mut self, port: u16) -> u8 {
        self.dev.port_in(port, self.now, self.mem)
    }

    fn port_out_u8(&mut self, port: u16, value: u8) {
        if let Some(ControllerEffect::SetA20(on)) = self.dev.port_out(port, value, self.now, self.mem) {
            self.mem.set_a20(on);
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Machine {
    pub cpu: Cpu,
    pub mem: Memory,
    pub devices: Devices,
    pub dos: DosState,
    pub ems: Ems,
    pub xms: Xms,
    pub mouse: Mouse,
    pub drive: Vfs,
    pub config: Config,
    /// Text the program wrote through DOS or the BIOS (teletype output).
    pub console: crate::record::Record<crate::record::Log<u8>>,
    pub frames: crate::record::Record<crate::record::Log<Frame>>,
    /// Things the emulation doesn't support that the program used; each message appears once.
    pub diagnostics: crate::record::Record<BTreeSet<String>>,
    /// The frames' pictures, when asked for ([`Machine::record_frame_images`]): a record, not state.
    #[serde(skip)]
    pub frame_images: Option<crate::frame_image::FrameImages>,
    /// When the records start: 0, or when the machine was loaded from a snapshot.
    #[serde(skip)]
    pub records_since_ns: u64,
    /// The analysis tools running (not part of the machine state).
    #[serde(skip)]
    pub probes: crate::probe::Probes,
    /// Sync points and their hit counts ([`crate::sync`]; not part of the machine state, but a
    /// snapshot keeps them).
    pub syncs: crate::sync::Syncs,
    /// The CD drive's disc image, if it shows one (an input, like the drive's files: not part
    /// of the machine state). Without one the drive's folder is a disc with one data track.
    /// A loaded snapshot needs it given again.
    #[serde(skip)]
    pub cd_image: Option<std::sync::Arc<crate::cdrom::CdImage>>,
    /// Stop with [`Stop::WaitingForInput`] when a blocking keyboard read has nothing left to read
    /// (headless runs). Interactive frontends clear it so time keeps passing while waiting.
    pub stop_on_input_wait: bool,
    /// Scheduled keyboard input: (cycle, scancode), sorted by cycle.
    inputs: VecDeque<(u64, Input)>,
    pub(crate) halted: bool,
    waiting_for_input: bool,
    exit_code: Option<u8>,
    next_event: u64,
    next_frame: u64,
    pub(crate) keyboard_e0: bool,
    /// Set when the current service returned flags through the interrupt frame.
    pub(crate) returned_flags: bool,
}

impl Machine {
    /// A powered-on PC with BIOS and DOS initialized and `drive` as C:. Load a program next.
    pub fn new(config: Config, drive: Vfs) -> Self {
        let clock = Clock::new(config.instructions_per_second);
        let mut machine = Self {
            cpu: Cpu::with_model(config.cpu_model),
            mem: Memory::with_size(0x10_0000 + config.extended_kb.min(MAX_EXTENDED_KB) * 1024),
            devices: Devices::new(&config, clock),
            dos: DosState::default(),
            ems: Ems::new(config.ems_kb),
            xms: Xms::new(config.xms, 0x10_0000 + config.extended_kb.min(MAX_EXTENDED_KB) * 1024),
            mouse: Mouse::new(config.mouse),
            drive,
            config,
            console: Default::default(),
            frames: Default::default(),
            diagnostics: Default::default(),
            frame_images: None,
            records_since_ns: 0,
            probes: Default::default(),
            syncs: crate::sync::Syncs::default(),
            cd_image: None,
            stop_on_input_wait: true,
            inputs: VecDeque::new(),
            halted: false,
            waiting_for_input: false,
            exit_code: None,
            next_event: 0,
            next_frame: 0,
            keyboard_e0: false,
            returned_flags: false,
        };
        if !machine.config.fpu {
            machine.cpu.fpu = None;
        }
        // Port accesses take ISA bus time: 1 µs per read, 0.75 µs per write (DOSBox `port.cpp`).
        let ips = machine.config.instructions_per_second;
        machine.cpu.io_read_cycles = ips / 1_000_000;
        machine.cpu.io_write_cycles = ips * 3 / 4_000_000;
        bios::install(&mut machine);
        ems::install(&mut machine);
        xms::install(&mut machine);
        mouse::install(&mut machine);
        dos::install(&mut machine);
        machine
    }

    /// Runs for up to `max_cycles` more cycles.
    pub fn run(&mut self, max_cycles: u64) -> Stop {
        self.run_until(self.cpu.cycles.saturating_add(max_cycles))
    }

    /// Runs until the instruction at linear address `at`, a sync point (added if it isn't one),
    /// is about to run for the `n`-th time from now (`n` at least 1), or for up to `max_cycles`
    /// (or until the program stops) if that comes first.
    pub fn run_to_sync(&mut self, at: u32, n: u64, max_cycles: u64) -> Stop {
        self.syncs.add(at, false);
        let hit = self.syncs.get(at).map_or(0, |p| p.hits) + n.max(1);
        self.syncs.set_target(at, hit);
        let stop = self.run(max_cycles);
        self.syncs.clear_target();
        stop
    }

    /// Runs until just before the next hit of any sync point (a session's next tick), for at most
    /// `max_cycles`.
    pub fn run_to_next_sync(&mut self, max_cycles: u64) -> Stop {
        self.syncs.set_next();
        let stop = self.run(max_cycles);
        self.syncs.clear_target();
        stop
    }

    /// Runs until the cycle counter reaches `limit` or the program stops.
    pub fn run_until(&mut self, limit: u64) -> Stop {
        // Memory may have changed since the last run (tools, tests, the frontend).
        self.cpu.flush_code_cache();
        // Analyses are set up between runs: whether any is on is decided once.
        let syncs = !self.syncs.is_empty();
        let probes = self.probes.contracts.is_some()
            || self.probes.watch.is_some()
            || self.probes.bounds.is_some()
            || self.probes.uninit.is_some()
            || self.probes.late.is_some()
            || self.probes.calls.is_some();
        loop {
            if let Some(code) = self.exit_code {
                return Stop::Exited(code);
            }
            if self.cpu.cycles >= limit {
                return Stop::CycleLimit;
            }
            if self.cpu.cycles >= self.next_event {
                self.service_events();
            }
            if self.cpu.interrupts_enabled()
                && let Some(vector) = self.devices.acknowledge()
            {
                let vector = crate::dpmi::irq_vector(self, vector);
                self.halted = false;
                self.count_irq(vector);
                let from = self.cpu.segment_base(seg::CS).wrapping_add(self.cpu.regs.ip);
                // A DPMI client's protected-mode handler may take it, though it came in real mode.
                if !crate::dpmi::real_mode_irq(self, vector) {
                    let (cpu, mut bus) = self.split();
                    if let Err(exit) = cpu.interrupt(&mut bus, vector) {
                        return Stop::Fault(exit);
                    }
                }
                if let Some(contracts) = self.probes.contracts.as_deref_mut()
                    && !self.cpu.protected_mode()
                {
                    contracts.hardware_interrupt(&self.cpu.regs);
                }
                if let Some(calls) = self.probes.calls.as_deref_mut() {
                    calls.interrupt(&self.cpu, from);
                }
                continue;
            }
            if self.halted {
                if let Some(stop) = self.idle(limit) {
                    return stop;
                }
                continue;
            }
            if syncs {
                let at = self.cpu.segment_base(seg::CS).wrapping_add(self.cpu.regs.ip);
                if self.syncs.is_point(at) && self.syncs.program(at).is_none_or(|program| self.running_in(program)) {
                    if let Some(late) = self.probes.late.as_deref_mut() {
                        late.hit(at, self.probes.calls.as_deref().and_then(|c| c.frame(0)));
                    }
                    let hits = self.syncs.get(at).map_or(0, |p| p.hits);
                    let gate = self.syncs.gate_sources(at).map(|s| self.gate_state(s));
                    let stop = self.syncs.reached(at, self.cpu.cycles, gate);
                    let hit = self.syncs.get(at).map_or(0, |p| p.hits);
                    if hit != hits {
                        self.edit();
                        if self.probes.late.is_some() {
                            self.late_tick(at);
                        }
                    }
                    if stop {
                        return Stop::Sync { address: at, hit };
                    }
                }
            }
            if self.probes.exec_watch.is_some() {
                self.watch_exec();
            }
            if self.probes.coverage.is_some() {
                self.coverage_before();
            }
            if let Some(contracts) = self.probes.contracts.as_deref_mut()
                && !self.cpu.protected_mode()
            {
                let linear = self.cpu.segment_base(seg::CS).wrapping_add(self.cpu.regs.ip);
                contracts.before(&self.cpu.regs, &self.mem, linear);
            }
            let now = self.cpu.cycles;
            let result = if probes {
                let tracing = self.probes.contracts.as_deref().is_some_and(|c| c.tracing());
                let at = self.cpu.segment_base(seg::CS).wrapping_add(self.cpu.regs.ip);
                if let Some(calls) = self.probes.calls.as_deref_mut() {
                    calls.before(&self.cpu);
                }
                let mut bus = crate::probe::ProbeBus {
                    inner: PcBus { mem: &mut self.mem, dev: &mut self.devices, now },
                    log: self.probes.contracts.as_deref_mut().filter(|_| tracing).map(|c| &mut c.log),
                    watch: self.probes.watch.as_deref_mut(),
                    bounds: self.probes.bounds.as_deref_mut(),
                    uninit: self.probes.uninit.as_deref_mut(),
                    late: self.probes.late.as_deref_mut(),
                    calls: self.probes.calls.as_deref(),
                    pending: Default::default(),
                    cs: self.cpu.regs.cs(),
                    ip: self.cpu.regs.ip,
                    at,
                    now,
                };
                self.cpu.step(&mut bus)
            } else {
                let (cpu, mut bus) = self.split();
                cpu.step(&mut bus)
            };
            if let Some(contracts) = self.probes.contracts.as_deref_mut() {
                contracts.after(&self.cpu.regs);
            }
            if let Some(mut calls) = self.probes.calls.take() {
                calls.after(&self.cpu, |at| self.peek_linear_u8(at));
                if let Some(late) = self.probes.late.as_deref_mut()
                    && !calls.in_hardware_interrupt()
                {
                    late.step();
                }
                self.probes.calls = Some(calls);
            }
            if let Some(coverage) = &mut self.probes.coverage {
                match result {
                    Ok(()) => coverage.after(self.cpu.segment_base(seg::CS).wrapping_add(self.cpu.regs.ip)),
                    Err(_) => coverage.cancel(),
                }
            }
            if self.devices.dirty {
                self.devices.dirty = false;
                self.next_event = self.cpu.cycles;
            }
            match result {
                Ok(()) => {}
                Err(cpu::Exit::Callback(id)) => self.callback(id),
                Err(cpu::Exit::Halt) => self.halted = true,
                Err(fault) => return Stop::Fault(fault),
            }
        }
    }

    /// A byte read the way the CPU reads it: video memory at A0000-BFFFF, everything else from
    /// RAM.
    pub fn read_u8(&mut self, addr: u32) -> u8 {
        let (_, mut bus) = self.split();
        bus.read_u8(addr)
    }

    /// A byte write the way the CPU writes it (see [`Machine::read_u8`]).
    pub fn write_u8(&mut self, addr: u32, value: u8) {
        let (_, mut bus) = self.split();
        bus.write_u8(addr, value);
    }

    /// A port read as `IN AL,DX` does it.
    pub fn port_in(&mut self, port: u16) -> u8 {
        let (_, mut bus) = self.split();
        let value = bus.port_in_u8(port);
        self.device_changed();
        value
    }

    /// A port write as `OUT DX,AL` does it.
    pub fn port_out(&mut self, port: u16, value: u8) {
        let (_, mut bus) = self.split();
        bus.port_out_u8(port, value);
        self.device_changed();
    }

    fn device_changed(&mut self) {
        if self.devices.dirty {
            self.devices.dirty = false;
            self.next_event = self.cpu.cycles;
        }
    }

    /// Where the running program's load image is, as linear addresses: from its PSP's paragraph
    /// 10h (where load-image offsets count from) to the end of its memory block.
    pub fn image_range(&self) -> std::ops::Range<u32> {
        self.image_range_of(self.dos.psp)
    }

    /// The load image of the process whose PSP is at paragraph `psp` ([`Machine::image_range`]).
    pub fn image_range_of(&self, psp: u16) -> std::ops::Range<u32> {
        let psp = u32::from(psp);
        let paragraphs = u32::from(self.mem.read_u16((psp.wrapping_sub(1) << 4) + 3));
        let start = (psp + 0x10) << 4;
        start..((psp + paragraphs) << 4).max(start)
    }

    /// Whether the program running now is `program` (a file name, without case), or one it started.
    pub fn running_in(&self, program: &str) -> bool {
        self.process_named(program).is_some()
    }

    /// The PSP of the program the machine started with: the first process, its own parent, from which
    /// the one running now descends.
    pub fn first_psp(&self) -> u16 {
        let mut psp = self.dos.psp;
        // A broken chain of parents ends the walk too.
        for _ in 0..32 {
            let parent = self.mem.read_u16(Self::lin(psp, 0x16));
            if parent == psp || !self.dos.process_paths.contains_key(&parent) {
                break;
            }
            psp = parent;
        }
        psp
    }

    /// The process of `program` (a file name, without case), if it runs now or started the one that
    /// does (its PSP's parents, up to the first process, its own parent): its PSP and path.
    pub fn process_named(&self, program: &str) -> Option<(u16, &str)> {
        let named = |path: &str| path.rsplit(['\\', '/']).next().is_some_and(|name| name.eq_ignore_ascii_case(program));
        if named(&self.dos.program) {
            return Some((self.dos.psp, &self.dos.program));
        }
        let mut psp = self.dos.psp;
        // A broken chain of parents ends the walk too.
        for _ in 0..32 {
            let parent = self.mem.read_u16(Self::lin(psp, 0x16));
            if parent == psp {
                return None;
            }
            psp = parent;
            if let Some(path) = self.dos.process_paths.get(&psp).filter(|path| named(path)) {
                return Some((psp, path));
            }
        }
        None
    }

    /// Starts recording coverage ([`crate::coverage`]), with the image of the program running now.
    pub fn start_coverage(&mut self) {
        self.probes.coverage = Some(crate::coverage::Coverage::new(self.image_range()));
    }

    /// Starts contract inference ([`crate::contracts`]) for the program the machine started with.
    pub fn start_contracts(&mut self) {
        self.start_contracts_in(self.image_range());
    }

    /// Starts contract inference for the code in `image` (linear addresses): a program a launcher
    /// started, say.
    pub fn start_contracts_in(&mut self, image: std::ops::Range<u32>) {
        self.probes.contracts = Some(Box::new(crate::contracts::Contracts::new(image.start, image.len() as u32)));
    }

    fn coverage_before(&mut self) {
        if self.probes.coverage.is_none() {
            return;
        }
        let linear = self.cpu.segment_base(seg::CS).wrapping_add(self.cpu.regs.ip);
        let opcode = self.peek_code(linear);
        self.probes.coverage.as_mut().expect("checked").before(linear, opcode);
    }

    /// The 4 bytes at linear address `linear` (zeros where nothing is mapped), translated once when
    /// they are on one page.
    fn peek_code(&self, linear: u32) -> [u8; 4] {
        if linear & 0xFFF <= 0xFFC
            && let Some(physical) = self.cpu.peek_translate(|at| self.mem.read_u32(at), linear)
        {
            return std::array::from_fn(|i| self.peek_u8(physical + i as u32));
        }
        std::array::from_fn(|i| self.peek_linear_u8(linear.wrapping_add(i as u32)).unwrap_or(0))
    }

    /// Records the instruction about to run if it starts in the watched range.
    fn watch_exec(&mut self) {
        let Some(range) = &self.probes.exec_watch else { return };
        let linear = self.cpu.segment_base(seg::CS).wrapping_add(self.cpu.regs.ip);
        if !range.contains(&linear) || self.probes.exec_hits.len() >= MAX_EXEC_HITS {
            return;
        }
        let sp = self.cpu.segment_base(seg::SS).wrapping_add(self.cpu.regs.sp().into());
        let stack = std::array::from_fn(|i| self.mem.read_u16(sp + 2 * i as u32));
        self.probes.exec_hits.push(ExecHit {
            cycle: self.cpu.cycles,
            cs: self.cpu.regs.cs(),
            ip: self.cpu.regs.ip,
            regs: self.cpu.regs,
            stack,
        });
    }

    fn count_irq(&mut self, vector: u8) {
        for (pic, first_line) in [(&self.devices.pic, 0), (&self.devices.pic2, 8)] {
            let line = vector.wrapping_sub(pic.vector_base());
            if line < 8 {
                self.probes.irq_counts[usize::from(first_line + line)] += 1;
                return;
            }
        }
    }

    pub fn exit_code(&self) -> Option<u8> {
        self.exit_code
    }

    /// What the sound devices played so far (render it with [`crate::audio::AudioLog::render`]).
    pub fn audio_log(&self) -> crate::audio::AudioLog<'_> {
        let (d, none) = (&self.devices, crate::audio::AudioLog::default());
        crate::audio::AudioLog {
            opl: d.opl.as_ref().map_or(none.opl, |opl| &opl.writes),
            pcm: d.sb.as_ref().map_or(none.pcm, |sb| &sb.pcm),
            dac: d.sb.as_ref().map_or(none.dac, |sb| &sb.dac),
            speaker: &d.pit.speaker_log,
            cd: &self.dos.cd_audio.segments,
            cd_image: self.cd_image.as_deref(),
            from_ns: self.records_since_ns,
        }
    }

    /// Starts the records of a machine loaded from a snapshot, which holds none: from now, with
    /// what the sound devices are putting out logged again, so its sound renders from here on.
    pub(crate) fn restart_records(&mut self) {
        let now = self.elapsed_ns();
        self.records_since_ns = now;
        if let Some(opl) = &mut self.devices.opl {
            opl.relog(now);
        }
        let tick = self.devices.clock.pit_ticks(self.cpu.cycles);
        self.devices.pit.relog(tick);
        self.dos.cd_audio.relog();
    }

    /// Up to when [`Machine::audio_log`] is final: what the devices log later all happens after
    /// this time, so audio rendered up to it never changes. A little behind [`Machine::elapsed_ns`].
    pub fn audio_settled_ns(&self) -> u64 {
        // PIT ticks round down, so a speaker event logged later can be up to a tick (0.84 µs)
        // before the time it happened at: stay a microsecond back.
        let now = self.elapsed_ns().saturating_sub(1_000);
        self.devices.sb.as_ref().map_or(now, |sb| sb.settled_ns(now))
    }

    /// Nanoseconds of emulated time elapsed.
    pub fn elapsed_ns(&self) -> u64 {
        self.devices.clock.nanos(self.cpu.cycles)
    }

    /// Sets the A20 gate the way port 92h does, keeping the port's bit 1 in step.
    pub(crate) fn set_a20(&mut self, on: bool) {
        self.mem.set_a20(on);
        self.devices.port92 = (self.devices.port92 & !0x02) | if on { 0x02 } else { 0 };
    }

    /// Milliseconds of emulated time elapsed.
    pub fn elapsed_ms(&self) -> u64 {
        self.devices.clock.millis(self.cpu.cycles)
    }

    /// Cycle count `ms` milliseconds into the run.
    pub fn cycle_at_ms(&self, ms: u64) -> u64 {
        (u128::from(ms) * u128::from(self.config.instructions_per_second) / 1000) as u64
    }

    /// Schedules raw scancodes for delivery at `cycle`. The keyboard controller hands them to
    /// the program one at a time, each with its own IRQ1.
    pub fn queue_scancodes(&mut self, cycle: u64, codes: &[u8]) {
        for &code in codes {
            self.queue_input(cycle, Input::Key(code));
        }
    }

    /// Schedules the mouse to be at (x, y), in the driver's coordinates, with `buttons` held.
    pub fn queue_mouse(&mut self, cycle: u64, x: i16, y: i16, buttons: u8) {
        self.queue_input(cycle, Input::Mouse { x, y, buttons });
    }

    /// Schedules any input event; events at the same cycle keep their order.
    pub fn queue_input(&mut self, cycle: u64, input: Input) {
        let at = self.inputs.partition_point(|&(t, _)| t <= cycle);
        self.inputs.insert(at, (cycle, input));
        self.next_event = self.next_event.min(cycle);
    }

    /// Types ASCII text (US layout) starting at `cycle`, one key every `spacing` cycles, each
    /// held for half that time. Returns the cycle after the last key is released. Characters
    /// that have no key are skipped.
    pub fn type_text(&mut self, cycle: u64, text: &str, spacing: u64) -> u64 {
        let mut at = cycle;
        for byte in text.bytes() {
            let Some((make, shift)) = ascii_to_scancode(byte) else { continue };
            let release = at + spacing / 2;
            if shift {
                self.queue_scancodes(at, &[LEFT_SHIFT, make]);
                self.queue_scancodes(release, &[make | 0x80, LEFT_SHIFT | 0x80]);
            } else {
                self.queue_scancodes(at, &[make]);
                self.queue_scancodes(release, &[make | 0x80]);
            }
            at += spacing;
        }
        at
    }

    /// Fingerprint of the complete machine state (CPU, memory, devices, DOS, EMS, XMS, the
    /// drive). Two runs with the same program and input must produce the same value at the same
    /// cycle. Inputs (the configuration, scheduled input, what the game's files held as loaded),
    /// records and the analysis tools aren't state.
    pub fn state_hash(&self) -> u64 {
        let mut h = Fnv64::default();
        self.cpu.hash(&mut h);
        self.mem.hash(&mut h);
        self.devices.hash(&mut h);
        self.dos.hash(&mut h);
        self.ems.hash(&mut h);
        self.xms.hash(&mut h);
        self.mouse.hash(&mut h);
        self.drive.hash(&mut h);
        let flags = (self.halted, self.waiting_for_input, self.keyboard_e0, self.returned_flags);
        (flags, self.exit_code, self.next_event, self.next_frame).hash(&mut h);
        // A scenario's edits still to come (D64); none leaves the hash as it was before edits.
        if !self.syncs.edits().is_empty() {
            self.syncs.edits().hash(&mut h);
        }
        h.finish()
    }

    /// Memory B0000-BFFFF, where text and CGA screens live.
    fn low_video_ram(&self) -> &[u8] {
        self.mem.slice(0xB_0000, vga_draw::LOW_RAM_SIZE)
    }

    /// The displayed screen as RGB bytes (width, height, pixels), rendered from the VGA registers
    /// and memory the way the card would show it.
    pub fn screen_rgb(&self) -> (usize, usize, Vec<u8>) {
        self.devices.vga.rgb(self.low_video_ram())
    }

    /// Reads memory the way the CPU sees it, with the A0000 window going to video memory
    /// (without loading the VGA latches).
    /// Reads `len` bytes at a linear address the way the CPU sees them now: through the page
    /// tables when paging is on, without side effects. Bytes on pages that aren't present are
    /// `None`.
    pub fn peek_linear(&self, linear: u32, len: usize) -> Vec<Option<u8>> {
        (0..len as u32)
            .map(|i| {
                let physical = self.cpu.peek_translate(|at| self.mem.read_u32(at), linear.wrapping_add(i))?;
                Some(self.peek_bytes(physical, 1)[0])
            })
            .collect()
    }

    /// Where `selector:offset` is now, as a linear address, and whether its segment is 32-bit
    /// (for debuggers; see [`cpu::Cpu::peek_segment`]).
    pub fn peek_address(&self, selector: u16, offset: u32) -> Option<(u32, bool)> {
        let (base, big) = self.cpu.peek_segment(selector, |at| self.peek_linear(at, 1)[0])?;
        Some((base.wrapping_add(offset), big))
    }

    pub fn peek_bytes(&self, addr: u32, len: usize) -> Vec<u8> {
        (0..len as u32).map(|i| self.peek_u8(addr.wrapping_add(i))).collect()
    }

    /// A byte at a physical address the way the CPU sees it, without side effects.
    pub fn peek_u8(&self, addr: u32) -> u8 {
        peek_device_u8(&self.mem, &self.devices, addr)
    }

    /// A byte at a linear address (through the page tables when paging is on), without side
    /// effects; `None` on a page that isn't present.
    pub fn peek_linear_u8(&self, linear: u32) -> Option<u8> {
        let physical = self.cpu.peek_translate(|at| self.mem.read_u32(at), linear)?;
        Some(self.peek_u8(physical))
    }

    /// An input device's state now, as bytes ([`Device`]).
    pub fn device_state(&self, device: Device) -> Vec<u8> {
        match device {
            Device::Mouse => self.mouse.reported_state().to_vec(),
            Device::Joystick => self.devices.game_port.reported_state().to_vec(),
        }
    }

    /// What a sync point's gate compares, now ([`crate::sync`]).
    fn gate_state(&self, sources: &[crate::sync::Source]) -> Vec<Option<u8>> {
        let mut state = Vec::new();
        for source in sources {
            match *source {
                crate::sync::Source::Memory { at, len } => {
                    state.extend((0..len).map(|i| self.peek_linear_u8(at.wrapping_add(i))));
                }
                crate::sync::Source::Device(device) => state.extend(self.device_state(device).into_iter().map(Some)),
            }
        }
        state
    }

    /// Starts watching memory (see [`crate::watch`]); a watch already on is replaced. It turns call
    /// tracking on if nothing did, for the calls made from now on.
    pub fn start_watch(&mut self, watch: crate::watch::Watch) {
        self.probes.watch = Some(Box::new(watch));
        self.calls_for(crate::probe::CALLS_FOR_WATCH);
    }

    /// Ends the watch and returns it, with its hits.
    pub fn take_watch(&mut self) -> Option<crate::watch::Watch> {
        self.calls_done(crate::probe::CALLS_FOR_WATCH);
        self.probes.watch.take().map(|w| *w)
    }

    /// Tracks calls from now on (see [`crate::calls`]), so a watch started later knows the whole
    /// call chain; or stops tracking.
    pub fn track_calls(&mut self, on: bool) {
        self.probes.calls_for = 0;
        self.probes.calls = on.then(|| self.probes.calls.take().unwrap_or_default());
    }

    /// A probe needs call tracking: on if it is off, until the probes that turned it on are done.
    fn calls_for(&mut self, probe: u8) {
        if self.probes.calls.is_none() || self.probes.calls_for != 0 {
            self.probes.calls.get_or_insert_default();
            self.probes.calls_for |= probe;
        }
    }

    fn calls_done(&mut self, probe: u8) {
        if self.probes.calls_for & probe != 0 {
            self.probes.calls_for &= !probe;
            if self.probes.calls_for == 0 {
                self.probes.calls = None;
            }
        }
    }

    /// Starts looking for input that changes within a tick (see [`crate::late`]); what was on is
    /// replaced. It turns call tracking on if nothing did.
    pub fn start_late_inputs(&mut self, late: crate::late::LateInputs) {
        self.probes.late = Some(Box::new(late));
        self.calls_for(crate::probe::CALLS_FOR_LATE);
    }

    pub fn take_late_inputs(&mut self) -> Option<crate::late::LateInputs> {
        self.calls_done(crate::probe::CALLS_FOR_LATE);
        self.probes.late.take().map(|l| l.finish())
    }

    /// A counted hit of the sync point at `at`: the late-input sources it ticks keep what they hold.
    fn late_tick(&mut self, at: u32) {
        let Some(mut late) = self.probes.late.take() else { return };
        let ticking: Vec<(usize, crate::late::LateSource)> = late.ticking(at).map(|(i, s)| (i, s.clone())).collect();
        for (i, source) in ticking {
            let (spans, values) = match source {
                crate::late::LateSource::Memory { at, len } => {
                    let mut spans: Vec<std::ops::Range<u32>> = Vec::new();
                    let mut values = Vec::new();
                    for linear in (0..len).map(|i| at.wrapping_add(i)) {
                        let Some(physical) = self.cpu.peek_translate(|a| self.mem.read_u32(a), linear) else {
                            continue;
                        };
                        values.push(self.peek_u8(physical));
                        match spans.last_mut() {
                            Some(span) if span.end == physical => span.end += 1,
                            _ => spans.push(physical..physical + 1),
                        }
                    }
                    (spans, values)
                }
                crate::late::LateSource::Device(device) => (Vec::new(), self.device_state(device)),
            };
            late.tick(i, self.cpu.cycles, spans, values);
        }
        self.probes.late = Some(late);
    }

    /// The program read `device` through a service (the call tracker has its call).
    pub(crate) fn late_device_read(&mut self, device: Device) {
        let Some(calls) = self.probes.calls.as_deref().filter(|_| self.probes.late.is_some()) else { return };
        if calls.in_hardware_interrupt() {
            return;
        }
        // The service's call, and the one it was called in.
        let read = crate::late::Read {
            cycle: self.cpu.cycles,
            at: calls.frame(0).map_or(0, |f| f.from),
            call: calls.frame(1),
        };
        let state = self.device_state(device);
        self.probes.late.as_deref_mut().expect("checked").device_read(device, &state, read);
    }

    /// Starts bounds checks on these objects (physical addresses; see [`crate::bounds`]).
    /// Starts recording the program's reads and writes of files (see [`crate::trace::FileAccess`]).
    pub fn start_files(&mut self) {
        self.probes.files = Some(Vec::new());
    }

    pub fn take_files(&mut self) -> Option<Vec<crate::trace::FileAccess>> {
        self.probes.files.take()
    }

    pub fn start_bounds(&mut self, objects: &[crate::bounds::Object]) {
        self.probes.bounds = Some(Box::new(crate::bounds::Bounds::new(objects)));
    }

    pub fn take_bounds(&mut self) -> Option<crate::bounds::Bounds> {
        self.probes.bounds.take().map(|b| *b)
    }

    /// Starts tracking uninitialized reads (see [`crate::uninit`]): what the started program finds
    /// uninitialized when it starts, plus `more` (physical ranges), counts as never written.
    pub fn start_uninit(&mut self, more: &[std::ops::Range<u32>]) {
        let mut ranges = crate::dos::uninitialized(self);
        ranges.extend_from_slice(more);
        self.mem.track_writes(&ranges);
        self.probes.uninit = Some(Box::default());
    }

    pub fn take_uninit(&mut self) -> Option<crate::uninit::Uninit> {
        self.mem.untrack_writes();
        self.probes.uninit.take().map(|u| *u)
    }

    /// The text screen as lines, or `None` in graphics modes.
    pub fn screen_text(&self) -> Option<Vec<String>> {
        let vga = &self.devices.vga;
        if vga.display_kind() != vga::DisplayKind::Text {
            return None;
        }
        let cells: Vec<(u8, u8)> = vga.text_cells(self.low_video_ram()).collect();
        Some(
            cells
                .chunks(vga.char_clocks())
                .map(|row| {
                    let line: String = row.iter().map(|&(ch, _)| cp437_to_char(ch)).collect();
                    line.trim_end().to_owned()
                })
                .collect(),
        )
    }

    // ---- internals shared by the BIOS and DOS modules ----

    pub(crate) fn split(&mut self) -> (&mut Cpu, PcBus<'_>) {
        let now = self.cpu.cycles;
        (&mut self.cpu, PcBus { mem: &mut self.mem, dev: &mut self.devices, now })
    }

    pub(crate) fn lin(segment: u16, offset: u16) -> u32 {
        (u32::from(segment) << 4) + u32::from(offset)
    }

    /// Sets a flag in the FLAGS image an `IRET` will restore. Valid inside a service entered
    /// through a ROM stub (`[STI] callback; IRET`), where SS:SP points at the interrupt frame.
    pub(crate) fn set_return_flag(&mut self, mask: u16, on: bool) {
        self.returned_flags = true;
        self.cpu.regs.set_flag(mask, on);
        let addr = self.cpu.linear(seg::SS, self.cpu.regs.sp().wrapping_add(4));
        let flags = self.mem.read_u16(addr);
        self.mem.write_u16(addr, if on { flags | mask } else { flags & !mask });
    }

    /// Makes the current service wait for keyboard input: the callback runs again after the
    /// next interrupt. The ROM stub executed STI first, so the CPU can be woken.
    pub(crate) fn wait_for_input(&mut self) {
        self.cpu.regs.ip = self.cpu.regs.ip.wrapping_sub(4);
        self.halted = true;
        self.waiting_for_input = true;
    }

    pub(crate) fn diagnostic(&mut self, message: String) {
        self.diagnostics.insert(message);
    }

    pub(crate) fn terminate(&mut self, code: u8) {
        self.exit_code = Some(code);
    }

    /// Re-arms event processing after a service changed device state (e.g. read port 60h).
    pub(crate) fn events_changed(&mut self) {
        self.next_event = self.cpu.cycles;
    }

    fn callback(&mut self, id: u16) {
        // Services write guest memory (file reads, BIOS data, the mouse cursor).
        self.cpu.flush_code_cache();
        self.waiting_for_input = false;
        self.returned_flags = false;
        let traced = self.probes.service_trace.is_some()
            && !matches!(id, bios::callback::TIMER_IRQ | bios::callback::KEYBOARD_IRQ);
        let call = traced.then(|| crate::trace::ServiceCall::begin(self, id));
        if let Some(watch) = self.probes.watch.as_deref_mut() {
            watch.service_called(id);
        }
        if id == bios::callback::MOUSE {
            self.late_device_read(Device::Mouse);
        }
        let watched = self.watch_before();
        let ax = self.cpu.regs.ax();
        if !bios::dispatch(self, id) && !dos::dispatch(self, id) {
            self.diagnostic(format!("unknown host callback {id:#06x}"));
        }
        if let Some(video) = watched {
            self.watch_changes(video, crate::watch::By::Service { id, ax });
        }
        if let (Some(mut call), Some(trace)) = (call, self.probes.service_trace.as_mut()) {
            call.after = self.cpu.regs;
            call.returned_flags = self.returned_flags;
            trace.push(call);
        }
    }

    /// Advances time to the next event while the CPU is halted.
    fn idle(&mut self, limit: u64) -> Option<Stop> {
        if !self.cpu.regs.flag(IF) {
            return Some(Stop::Halted);
        }
        if self.stop_on_input_wait
            && self.waiting_for_input
            && self.inputs.is_empty()
            && !self.devices.keyboard.has_queued()
        {
            return Some(Stop::WaitingForInput);
        }
        // To the next device event.
        self.cpu.cycles = self.cpu.cycles.max(self.next_event.min(limit));
        None
    }

    /// Writes a scenario's edits due at the tick just counted (D64), as the program sees memory; a
    /// byte where nothing is mapped is left out.
    fn edit(&mut self) {
        let due = self.syncs.due_edits();
        if due.is_empty() {
            return;
        }
        let watched = self.watch_before();
        for edit in due {
            for (i, &byte) in edit.bytes.iter().enumerate() {
                let linear = edit.at.wrapping_add(i as u32);
                match self.cpu.peek_translate(|at| self.mem.read_u32(at), linear) {
                    Some(physical) => self.write_u8(physical, byte),
                    None => self.diagnostic(format!("edit at tick {}: @{linear:X} isn't mapped", edit.tick)),
                }
            }
        }
        if let Some(video) = watched {
            self.watch_changes(video, crate::watch::By::Edit);
        }
    }

    /// Before the host acts (a service, a device event) with a watch on: memory logs its writes, and
    /// the watched video memory is kept to compare. `None` without a watch.
    fn watch_before(&mut self) -> Option<Option<Vec<u8>>> {
        let video = self.probes.watch.as_deref().map(|w| w.video_bytes(|at| self.peek_u8(at)))?;
        self.mem.start_log();
        Some(video)
    }

    /// After the host acted: records the watched bytes it changed as written by `by`.
    fn watch_changes(&mut self, video_before: Option<Vec<u8>>, by: crate::watch::By) {
        let log = self.mem.take_log();
        if let Some(mut watch) = self.probes.watch.take() {
            let calls = self.probes.calls.as_deref();
            watch.changed(self.cpu.cycles, &log, video_before.as_deref(), |at| self.peek_u8(at), by, calls);
            self.probes.watch = Some(watch);
        }
    }

    fn service_events(&mut self) {
        let watched = self.watch_before();
        self.service_devices();
        if let Some(video) = watched {
            self.watch_changes(video, crate::watch::By::Device);
        }
    }

    fn service_devices(&mut self) {
        // Devices may write memory (Sound Blaster DMA, the mouse cursor drawn on input).
        self.cpu.flush_code_cache();
        let cycles = self.cpu.cycles;
        let clock = self.devices.clock;
        let now = clock.pit_ticks(cycles);
        while let Some(&(at, input)) = self.inputs.front() {
            if at > cycles {
                break;
            }
            self.inputs.pop_front();
            match input {
                Input::Key(code) => self.devices.keyboard.push(code),
                Input::Mouse { x, y, buttons } => mouse::input(self, i32::from(x), i32::from(y), buttons),
                Input::MouseMotion { dx, dy } => mouse::motion(self, i32::from(dx), i32::from(dy)),
                Input::Joystick { stick, x, y, buttons } => {
                    self.devices.game_port.set(usize::from(stick), Stick { x, y, buttons });
                }
            }
        }
        if self.devices.keyboard.pump() {
            self.devices.pic.raise(1);
        }
        if self.devices.pit.take_irq0(now) > 0 {
            self.devices.pic.raise(0);
        }
        // The real-time clock's periodic interrupt is IRQ 8: line 0 of the slave PIC.
        if self.devices.cmos.update(clock.ticks(cycles, crate::cmos::RTC_HZ)) {
            self.devices.pic2.raise(0);
        }
        self.devices.update_sound(cycles, &mut self.mem);
        if let Some(sb) = &mut self.devices.sb {
            self.diagnostics.append(&mut sb.unsupported);
        }
        while self.next_frame * FRAME_PIT_TICKS <= now {
            self.capture_frame();
            self.next_frame += 1;
        }
        let mut next = clock.cycle_at(self.next_frame * FRAME_PIT_TICKS);
        if let Some(tick) = self.devices.pit.next_irq0() {
            next = next.min(clock.cycle_at(tick));
        }
        if let Some(&(at, _)) = self.inputs.front() {
            next = next.min(at);
        }
        if let Some(at) = self.devices.next_sound_event() {
            next = next.min(at);
        }
        if let Some(tick) = self.devices.cmos.next_periodic() {
            next = next.min(clock.cycle_at_ticks(tick, crate::cmos::RTC_HZ));
        }
        self.next_event = next.max(cycles + 1);
    }

    fn capture_frame(&mut self) {
        let low_ram = self.mem.slice(0xB_0000, vga_draw::LOW_RAM_SIZE);
        let hash = self.devices.vga.fingerprint(low_ram);
        let mode = self.devices.vga.mode;
        self.frames.push(Frame { number: self.next_frame, cycle: self.cpu.cycles, mode, hash });
        if let Some(images) = &mut self.frame_images {
            images.frame(self.next_frame, hash, &self.devices.vga, low_ram);
        }
    }

    /// Renders the picture of each frame shown from now on whose picture changed (or stops), to be
    /// taken with [`Machine::take_frame_images`] ([`crate::frame_image`]).
    pub fn record_frame_images(&mut self, on: bool) {
        self.frame_images = on.then(|| self.frame_images.take().unwrap_or_default());
    }

    /// The pictures rendered since they were last taken: (frame number, picture).
    pub fn take_frame_images(&mut self) -> Vec<(u64, Vec<u8>)> {
        self.frame_images.as_mut().map(|images| std::mem::take(&mut images.pending)).unwrap_or_default()
    }
}

/// Code page 437 for the printable range; other bytes shown as '·'.
pub fn cp437_to_char(byte: u8) -> char {
    match byte {
        0x20..=0x7E => char::from(byte),
        0 => ' ',
        _ => '·',
    }
}
