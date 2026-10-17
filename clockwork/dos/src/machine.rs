//! The emulated PC: CPU, memory, devices, BIOS/DOS state and the run loop (PROJECT.md §5.2).

use std::collections::{BTreeSet, VecDeque};
use std::hash::{Hash, Hasher};

use cpu::flags::IF;
use cpu::{Bus, Cpu, seg};
use machine::Memory;
use machine::hash::Fnv64;

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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
        }
    }
}

/// Most instructions `exec_watch` records before it stops recording.
pub const MAX_EXEC_HITS: usize = 1_000_000;

/// An instruction that ran in the watched range: when, where, the registers, and the top of the
/// stack (a near or far return address right after a call).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecHit {
    pub cycle: u64,
    pub cs: u16,
    pub ip: u32,
    pub regs: cpu::Regs,
    pub stack: [u16; 4],
}

/// One scheduled input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Input {
    /// A scancode delivered through the keyboard controller.
    Key(u8),
    /// The mouse is at (x, y) in the driver's coordinates with these buttons held (bit 0 left,
    /// bit 1 right, bit 2 middle).
    Mouse { x: i16, y: i16, buttons: u8 },
    /// Joystick `stick` (0 or 1) is at (x, y), each -32767..=32767, with these buttons held
    /// (bit 0 button 1, bit 1 button 2).
    Joystick { stick: u8, x: i16, y: i16, buttons: u8 },
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
    /// A Rust override failed (see [`crate::hybrid`]).
    Override(String),
    /// A nested run reached the return it was waiting for (internal to [`crate::hybrid`]).
    Returned,
}

/// Extended memory is limited to what a 24-bit (286/386SX) address bus reaches: 16 MB in all.
pub const MAX_EXTENDED_KB: u32 = 15 * 1024;

/// One displayed frame, fingerprinted at the start of vertical retrace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Frame {
    pub number: u64,
    pub cycle: u64,
    /// The BIOS video mode (VESA modes included).
    pub mode: u16,
    pub hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
            0x201 => self.game_port.read(self.clock.nanos(cycles)),
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
            0x201 => self.game_port.write(self.clock.nanos(cycles)),
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

impl PcBus<'_> {
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

#[derive(Clone)]
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
    pub console: Vec<u8>,
    pub frames: Vec<Frame>,
    /// Things the emulation doesn't support that the program used; each message appears once.
    pub diagnostics: BTreeSet<String>,
    /// When set, every host service call is recorded here (BIOS, DOS, EMS, MSCDEX); timer and
    /// keyboard interrupts are left out. Not part of the machine state.
    pub service_trace: Option<Vec<crate::trace::ServiceCall>>,
    /// Hardware interrupts delivered to the CPU, per IRQ line (0-15). Not part of the machine
    /// state; for debugging.
    pub irq_counts: [u64; 16],
    /// When set, every instruction that starts inside this linear range is recorded in
    /// `exec_hits` (for debugging; not part of the machine state).
    pub exec_watch: Option<std::ops::Range<u32>>,
    pub exec_hits: Vec<ExecHit>,
    /// What the started program's code executed, once [`Machine::start_coverage`] was called
    /// (for the analysis pipeline; not part of the machine state).
    pub coverage: Option<crate::coverage::Coverage>,
    /// Rust overrides of the started program's functions, once [`Machine::start_hybrid`] was
    /// called (not part of the machine state).
    pub hybrid: Option<Box<crate::hybrid::Hybrid>>,
    /// Where a nested run (an override calling the program, a dual-run) stops.
    pub(crate) return_trap: Option<crate::hybrid::ReturnTrap>,
    /// While a dual-run's original runs: what interrupt handlers write.
    pub(crate) isr_watch: Option<crate::hybrid::IsrWatch>,
    /// While a captured call runs: its accesses.
    pub(crate) tracer: Option<Box<crate::capture::Tracer>>,
    /// Stop with [`Stop::WaitingForInput`] when a blocking keyboard read has nothing left to read
    /// (headless runs). Interactive frontends clear it so time keeps passing while waiting.
    pub stop_on_input_wait: bool,
    /// Scheduled keyboard input: (cycle, scancode), sorted by cycle.
    inputs: VecDeque<(u64, Input)>,
    halted: bool,
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
            console: Vec::new(),
            frames: Vec::new(),
            diagnostics: BTreeSet::new(),
            service_trace: None,
            irq_counts: [0; 16],
            exec_watch: None,
            exec_hits: Vec::new(),
            coverage: None,
            hybrid: None,
            return_trap: None,
            isr_watch: None,
            tracer: None,
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

    /// Runs until the cycle counter reaches `limit` or the program stops.
    pub fn run_until(&mut self, limit: u64) -> Stop {
        // Memory may have changed since the last run (tools, tests, the frontend).
        self.cpu.flush_code_cache();
        loop {
            if let Some(code) = self.exit_code {
                return Stop::Exited(code);
            }
            if let Some(watch) = &mut self.isr_watch {
                watch.step(&self.cpu.regs, &self.mem);
            }
            if let Some(trap) = &mut self.return_trap
                && trap.reached(&self.cpu.regs)
            {
                return Stop::Returned;
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
                self.halted = false;
                self.count_irq(vector);
                if let Some(watch) = &mut self.isr_watch {
                    watch.enter(&self.cpu.regs, &self.mem);
                }
                if let Some(tracer) = &mut self.tracer {
                    tracer.interrupt(&self.cpu.regs);
                }
                let (cpu, mut bus) = self.split();
                if let Err(exit) = cpu.interrupt(&mut bus, vector) {
                    return Stop::Fault(exit);
                }
                continue;
            }
            if self.halted {
                if let Some(stop) = self.idle(limit) {
                    return stop;
                }
                continue;
            }
            if self.hybrid.is_some() {
                match self.hybrid_entry() {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(stop) => return stop,
                }
            }
            if self.exec_watch.is_some() {
                self.watch_exec();
            }
            if self.coverage.is_some() {
                self.coverage_before();
            }
            let result = if let Some(tracer) = self.tracer.as_deref_mut()
                && tracer.active(&self.cpu.regs)
            {
                let now = self.cpu.cycles;
                let inner = PcBus { mem: &mut self.mem, dev: &mut self.devices, now };
                let mut bus = crate::capture::TracingBus { inner, log: &mut tracer.log };
                self.cpu.step(&mut bus)
            } else {
                let (cpu, mut bus) = self.split();
                cpu.step(&mut bus)
            };
            if let Some(coverage) = &mut self.coverage {
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
                Err(cpu::Exit::Callback(id)) => {
                    if let Some(tracer) = self.tracer.as_deref_mut()
                        && tracer.active(&self.cpu.regs)
                    {
                        tracer.services += 1;
                    }
                    self.callback(id);
                }
                Err(cpu::Exit::Halt) => self.halted = true,
                Err(fault) => return Stop::Fault(fault),
            }
        }
    }

    /// A byte read the way the CPU reads it: video memory at A0000-BFFFF, everything else from
    /// RAM (for Rust overrides).
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

    /// Hooks Rust overrides into the program just loaded (all in [`crate::hybrid::Mode::Original`]
    /// until set otherwise); its load image starts at its PSP's paragraph 10h.
    pub fn start_hybrid(
        &mut self,
        module: &str,
        overrides: Vec<crate::hybrid::Override>,
    ) -> &mut crate::hybrid::Hybrid {
        let base = (u32::from(self.dos.psp) + 0x10) << 4;
        self.hybrid.insert(Box::new(crate::hybrid::Hybrid::new(module, base, overrides)))
    }

    /// Starts recording coverage of the program just loaded: its load image runs from its PSP's
    /// paragraph 10h to the end of its memory block.
    pub fn start_coverage(&mut self) {
        let psp = u32::from(self.dos.psp);
        let base = (psp + 0x10) << 4;
        let end = (psp + u32::from(self.mem.read_u16(((psp - 1) << 4) + 3))) << 4;
        self.coverage = Some(crate::coverage::Coverage::new(base, end.saturating_sub(base)));
    }

    fn coverage_before(&mut self) {
        let Some(coverage) = &mut self.coverage else { return };
        if self.cpu.protected_mode() {
            coverage.cancel();
            return;
        }
        let linear = self.cpu.segment_base(seg::CS).wrapping_add(self.cpu.regs.ip);
        let opcode = std::array::from_fn(|i| self.mem.read_u8(linear.wrapping_add(i as u32)));
        coverage.before(linear, opcode);
    }

    /// Records the instruction about to run if it starts in the watched range.
    fn watch_exec(&mut self) {
        let Some(range) = &self.exec_watch else { return };
        let linear = self.cpu.segment_base(seg::CS).wrapping_add(self.cpu.regs.ip);
        if !range.contains(&linear) || self.exec_hits.len() >= MAX_EXEC_HITS {
            return;
        }
        let sp = self.cpu.segment_base(seg::SS).wrapping_add(self.cpu.regs.sp().into());
        let stack = std::array::from_fn(|i| self.mem.read_u16(sp + 2 * i as u32));
        self.exec_hits.push(ExecHit {
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
                self.irq_counts[usize::from(first_line + line)] += 1;
                return;
            }
        }
    }

    pub fn exit_code(&self) -> Option<u8> {
        self.exit_code
    }

    /// What the sound devices played so far (render it with [`crate::audio::AudioLog::render`]).
    pub fn audio_log(&self) -> crate::audio::AudioLog<'_> {
        let d = &self.devices;
        crate::audio::AudioLog {
            opl: d.opl.as_ref().map_or(&[], |opl| &opl.writes),
            pcm: d.sb.as_ref().map_or(&[], |sb| &sb.pcm),
            dac: d.sb.as_ref().map_or(&[], |sb| &sb.dac),
            speaker: &d.pit.speaker_log,
        }
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

    /// Fingerprint of the complete machine state (CPU, memory, devices, DOS, drive). Two runs
    /// with the same program and input must produce the same value at the same cycle.
    pub fn state_hash(&self) -> u64 {
        let mut h = Fnv64::default();
        self.cpu.regs.hash(&mut h);
        self.cpu.fpu.hash(&mut h);
        self.cpu.cycles.hash(&mut h);
        self.mem.hash(&mut h);
        self.devices.hash(&mut h);
        self.dos.hash(&mut h);
        self.ems.hash(&mut h);
        self.mouse.hash(&mut h);
        self.drive.hash(&mut h);
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
        let vga = &self.devices.vga;
        (0..len as u32)
            .map(|i| {
                let at = addr.wrapping_add(i);
                let offset = at.wrapping_sub(vga::GRAPHICS_BASE);
                match (vga.window_size(), vga.lfb_range()) {
                    (Some(size), _) if offset < size => vga.peek(offset),
                    (_, Some(lfb)) if lfb.contains(&at) => vga.lfb_read(at - lfb.start),
                    _ => self.mem.read_u8(at),
                }
            })
            .collect()
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
        let traced =
            self.service_trace.is_some() && !matches!(id, bios::callback::TIMER_IRQ | bios::callback::KEYBOARD_IRQ);
        let call = traced.then(|| crate::trace::ServiceCall::begin(self, id));
        if !bios::dispatch(self, id) && !dos::dispatch(self, id) {
            self.diagnostic(format!("unknown host callback {id:#06x}"));
        }
        if let (Some(mut call), Some(trace)) = (call, self.service_trace.as_mut()) {
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
        self.cpu.cycles = self.cpu.cycles.max(self.next_event.min(limit));
        None
    }

    fn service_events(&mut self) {
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
                Input::Joystick { stick, x, y, buttons } => {
                    self.devices.game_port.set(usize::from(stick), Stick { x, y, buttons });
                }
            }
        }
        if self.devices.keyboard.pump() {
            self.devices.pic.raise(1);
        }
        if self.devices.pit.take_irq0(now) {
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
