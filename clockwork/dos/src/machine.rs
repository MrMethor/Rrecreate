//! The emulated PC: CPU, memory, devices, BIOS/DOS state and the run loop (PROJECT.md §5.2).

use std::collections::{BTreeSet, VecDeque};
use std::hash::{Hash, Hasher};

use cpu::flags::IF;
use cpu::{Bus, Cpu, seg};
use machine::Memory;
use machine::hash::Fnv64;

use crate::clock::{Clock, FRAME_PIT_TICKS};
use crate::cmos::Cmos;
use crate::dos::DosState;
use crate::ems::{self, Ems};
use crate::keyboard::{ControllerEffect, Keyboard, LEFT_SHIFT, ascii_to_scancode};
use crate::mouse::{self, Mouse};
use crate::mpu401::Mpu401;
use crate::pic::Pic;
use crate::pit::Pit;
use crate::time::DateTime;
use crate::vfs::Vfs;
use crate::vga::{self, Vga};
use crate::{bios, dos};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Config {
    /// Speed of the emulated PC: one cycle is one instruction (or string repetition).
    pub instructions_per_second: u64,
    /// The emulated wall clock at cycle 0.
    pub start_time: DateTime,
    /// Which processor programs see.
    pub cpu_model: cpu::Model,
    /// Letter of an emulated CD-ROM drive (with MSCDEX) that shows the same files as C:, read-only.
    pub cd_drive: Option<char>,
    /// The drive that is current when the program starts.
    pub start_drive: char,
    /// Extra environment variables for the program, after COMSPEC, PATH and PROMPT.
    pub environment: Vec<(String, String)>,
    /// Expanded memory (LIM EMS 4.0) in KB; 0 means no EMS driver.
    pub ems_kb: u32,
    /// Whether a mouse and its driver (INT 33h) are installed.
    pub mouse: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            instructions_per_second: 8_000_000,
            start_time: DateTime::new(1994, 1, 1, 8, 0, 0),
            cpu_model: cpu::Model::I386,
            cd_drive: None,
            start_drive: 'C',
            environment: Vec::new(),
            ems_kb: 4096,
            mouse: true,
        }
    }
}

/// One scheduled input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Input {
    /// A scancode delivered through the keyboard controller.
    Key(u8),
    /// The mouse is at (x, y) in the driver's coordinates with these buttons held (bit 0 left,
    /// bit 1 right, bit 2 middle).
    Mouse { x: i16, y: i16, buttons: u8 },
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
}

/// One displayed frame, fingerprinted at the start of vertical retrace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Frame {
    pub number: u64,
    pub cycle: u64,
    pub mode: u8,
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
    pub clock: Clock,
    port92: u8,
    /// Ports accessed that nothing emulates (reads return FFh, writes are dropped).
    pub unknown_ports: BTreeSet<u16>,
    /// A device changed in a way that may move the next event earlier.
    dirty: bool,
}

impl Devices {
    fn new(clock: Clock, start_time: DateTime) -> Self {
        Self {
            // BIOS defaults: IRQ0, 1, 2 and 6 unmasked on the master; vectors 08h and 70h.
            pic: Pic::new(0x08, 0xB8),
            pic2: Pic::new(0x70, 0xFF),
            pit: Pit::new(),
            keyboard: Keyboard::default(),
            vga: Vga::new(),
            mpu: Mpu401::default(),
            cmos: Cmos::new(start_time),
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

    fn port_in(&mut self, port: u16, cycles: u64) -> u8 {
        let now = self.clock.pit_ticks(cycles);
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
            0x3C0..=0x3DF => self.vga.read(port, now),
            0x71 => self.cmos.read_data(cycles / self.clock.instructions_per_second),
            0x330 => self.mpu.read_data(),
            0x331 => self.mpu.read_status(),
            // Not emulated yet: no joystick, Sound Blaster or AdLib. An absent card reads FFh.
            0x201 | 0x220..=0x22F | 0x388..=0x38B => 0xFF,
            _ => {
                self.unknown_ports.insert(port);
                0xFF
            }
        }
    }

    fn port_out(&mut self, port: u16, value: u8, cycles: u64) -> Option<ControllerEffect> {
        let now = self.clock.pit_ticks(cycles);
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
            0x3C0..=0x3DF => self.vga.write(port, value),
            0x70 => self.cmos.write_index(value),
            0x71 => self.cmos.write_data(value),
            0x330 => self.mpu.write_data(value, now),
            0x331 => self.mpu.write_command(value),
            0x80 => {} // POST code port, written for short I/O delays
            0x201 | 0x220..=0x22F | 0x388..=0x38B => {}
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

impl Bus for PcBus<'_> {
    fn read_u8(&mut self, addr: u32) -> u8 {
        self.mem.read_u8(addr)
    }

    fn write_u8(&mut self, addr: u32, value: u8) {
        self.mem.write_u8(addr, value);
    }

    fn port_in_u8(&mut self, port: u16) -> u8 {
        self.dev.port_in(port, self.now)
    }

    fn port_out_u8(&mut self, port: u16, value: u8) {
        if let Some(ControllerEffect::SetA20(on)) = self.dev.port_out(port, value, self.now) {
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
            mem: Memory::new(),
            devices: Devices::new(clock, config.start_time),
            dos: DosState::default(),
            ems: Ems::new(config.ems_kb),
            mouse: Mouse::new(config.mouse),
            drive,
            config,
            console: Vec::new(),
            frames: Vec::new(),
            diagnostics: BTreeSet::new(),
            service_trace: None,
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
        bios::install(&mut machine);
        ems::install(&mut machine);
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
                self.halted = false;
                let (cpu, mut bus) = self.split();
                cpu.interrupt(&mut bus, vector);
                continue;
            }
            if self.halted {
                if let Some(stop) = self.idle(limit) {
                    return stop;
                }
                continue;
            }
            let (cpu, mut bus) = self.split();
            let result = cpu.step(&mut bus);
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

    pub fn exit_code(&self) -> Option<u8> {
        self.exit_code
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
        self.cpu.cycles.hash(&mut h);
        self.mem.hash(&mut h);
        self.devices.hash(&mut h);
        self.dos.hash(&mut h);
        self.ems.hash(&mut h);
        self.mouse.hash(&mut h);
        self.drive.hash(&mut h);
        h.finish()
    }

    /// The mode 13h screen as 320×200 RGB bytes, or `None` in other modes.
    /// The screen as RGB bytes: 320×200 in mode 13h; text modes rendered with the 8×16 ROM font
    /// (640×400, or 320×400 with 40 columns). `None` in modes that aren't displayed yet.
    pub fn screen_rgb(&self) -> Option<(usize, usize, Vec<u8>)> {
        let vga = &self.devices.vga;
        let expand = |c: u8| (c << 2) | (c >> 4);
        if vga.is_chained_256() {
            let pixels = self.mem.slice(vga::GRAPHICS_BASE, 320 * 200);
            let rgb = pixels.iter().flat_map(|&p| vga.dac[usize::from(p & vga.pel_mask)].map(expand)).collect();
            return Some((320, 200, rgb));
        }
        if vga.mode > 3 && vga.mode != 7 {
            return None;
        }
        let columns = if vga.mode < 2 { 40 } else { 80 };
        let (width, height) = (columns * 8, 25 * 16);
        let page_offset = u32::from(self.mem.read_u16(bios::BDA_PAGE_OFFSET));
        // Text colors go through the attribute controller's palette into the DAC.
        let color = |index: u8| vga.dac[usize::from(vga.attr[usize::from(index & 0x0F)] & 0x3F)].map(expand);
        let mut rgb = vec![0u8; width * height * 3];
        for row in 0..25 {
            for column in 0..columns {
                let cell = vga::TEXT_BASE + page_offset + ((row * columns + column) * 2) as u32;
                let (ch, attribute) = (self.mem.read_u8(cell), self.mem.read_u8(cell + 1));
                let (foreground, background) = (color(attribute & 0x0F), color((attribute >> 4) & 0x07));
                for y in 0..16 {
                    let bits = crate::fonts::FONT_8X16[usize::from(ch) * 16 + y];
                    for x in 0..8 {
                        let pixel = if bits & (0x80 >> x) != 0 { foreground } else { background };
                        let at = ((row * 16 + y) * width + column * 8 + x) * 3;
                        rgb[at..at + 3].copy_from_slice(&pixel);
                    }
                }
            }
        }
        Some((width, height, rgb))
    }

    /// The text screen as lines, or `None` in graphics modes.
    pub fn screen_text(&self) -> Option<Vec<String>> {
        let mode = self.devices.vga.mode;
        if mode > 3 && mode != 7 {
            return None;
        }
        let columns = if mode < 2 { 40 } else { 80 };
        let cells = self.mem.slice(vga::TEXT_BASE, columns * 25 * 2);
        Some(
            cells
                .chunks(columns * 2)
                .map(|row| {
                    let line: String = row.chunks(2).map(|cell| cp437_to_char(cell[0])).collect();
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
            }
        }
        if self.devices.keyboard.pump() {
            self.devices.pic.raise(1);
        }
        if self.devices.pit.take_irq0(now) {
            self.devices.pic.raise(0);
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
        self.next_event = next.max(cycles + 1);
    }

    fn capture_frame(&mut self) {
        let vga = &self.devices.vga;
        let mut h = Fnv64::default();
        if vga.is_chained_256() {
            h.write(self.mem.slice(vga::GRAPHICS_BASE, 320 * 200));
            h.write(vga.dac.as_flattened());
            h.write(&[vga.pel_mask]);
        } else if vga.mode <= 3 || vga.mode == 7 {
            h.write(self.mem.slice(vga::TEXT_BASE, 80 * 25 * 2));
            h.write(&self.mem.read_u16(bios::BDA_CURSOR).to_le_bytes());
        } else {
            h.write(self.mem.slice(vga::GRAPHICS_BASE, 0x1_0000));
        }
        self.frames.push(Frame { number: self.next_frame, cycle: self.cpu.cycles, mode: vga.mode, hash: h.finish() });
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
