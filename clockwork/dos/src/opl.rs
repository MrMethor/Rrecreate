//! Yamaha OPL2 (YM3812) and OPL3 (YMF262) FM chips as the PC sees them: address/data ports,
//! the two timers and the status register, and dual OPL2 (Sound Blaster Pro 1).
//!
//! Ported from DOSBox Staging `src/hardware/audio/opl.cpp` (GPL-2.0-or-later). The chips aren't
//! synthesized here: every register write is logged with its time in [`Opl::writes`], and
//! `audio.rs` renders the log with Nuked OPL3. Timers run on virtual time, so AdLib detection
//! (start timer 1, wait, read the status) behaves as on hardware.

/// Which chips the card has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum OplMode {
    /// One OPL2: AdLib, Sound Blaster 1.x/2.0.
    Opl2,
    /// Two OPL2s, left and right: Sound Blaster Pro 1.
    DualOpl2,
    /// One OPL3: Sound Blaster Pro 2, Sound Blaster 16.
    Opl3,
}

/// A register write as the OPL3 synthesizer receives it: bank 1 registers are 100h-1FFh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct OplWrite {
    pub nanos: u64,
    pub register: u16,
    pub value: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
struct Timer {
    /// Length of one counter step: 80 µs (timer 1) or 320 µs (timer 2).
    step_ns: u64,
    start: u64,
    trigger: u64,
    period: u64,
    counter: u8,
    enabled: bool,
    overflow: bool,
    masked: bool,
}

impl Timer {
    fn new(step_ns: u64) -> Self {
        let mut timer = Self {
            step_ns,
            start: 0,
            trigger: 0,
            period: 0,
            counter: 0,
            enabled: false,
            overflow: false,
            masked: false,
        };
        timer.set_counter(0);
        timer
    }

    /// Catches up to `now`; returns whether the overflow flag is set.
    fn update(&mut self, now: u64) -> bool {
        if self.enabled && now >= self.trigger {
            let into_next = (now - self.trigger) % self.period;
            self.start = now - into_next;
            self.trigger = self.start + self.period;
            if !self.masked {
                self.overflow = true;
            }
        }
        self.overflow
    }

    fn set_counter(&mut self, value: u8) {
        self.counter = value;
        self.period = (256 - u64::from(value)) * self.step_ns;
    }

    fn set_mask(&mut self, masked: bool) {
        self.masked = masked;
        if masked {
            self.overflow = false;
        }
    }

    fn start(&mut self, now: u64) {
        if !self.enabled {
            self.enabled = true;
            self.overflow = false;
            self.start = now - now % self.step_ns;
            self.trigger = self.start + self.period;
        }
    }
}

/// The timer part of one chip.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
struct Chip {
    timers: [Timer; 2],
}

impl Chip {
    fn new() -> Self {
        Self { timers: [Timer::new(80_000), Timer::new(320_000)] }
    }

    /// Handles the timer registers (2, 3, 4); returns false for any other register.
    fn write(&mut self, register: u16, value: u8, now: u64) -> bool {
        match register {
            0x02 | 0x03 => {
                let timer = &mut self.timers[usize::from(register - 2)];
                timer.update(now);
                timer.set_counter(value);
            }
            0x04 => {
                if value & 0x80 != 0 {
                    // Reset both overflow flags.
                    self.timers.iter_mut().for_each(|t| t.overflow = false);
                } else {
                    for (i, timer) in self.timers.iter_mut().enumerate() {
                        if value & (1 << i) != 0 {
                            timer.start(now);
                        } else {
                            timer.enabled = false;
                        }
                    }
                    self.timers[0].set_mask(value & 0x40 != 0);
                    self.timers[1].set_mask(value & 0x20 != 0);
                }
            }
            _ => return false,
        }
        true
    }

    /// The status register: bit 7 = any overflow, bit 6 = timer 1, bit 5 = timer 2.
    fn status(&mut self, now: u64) -> u8 {
        let mut status = 0;
        if self.timers[0].update(now) {
            status |= 0xC0;
        }
        if self.timers[1].update(now) {
            status |= 0xA0;
        }
        status
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Opl {
    pub mode: OplMode,
    chips: [Chip; 2],
    address: u16,
    dual_address: [u8; 2],
    /// OPL3 mode (the NEW bit, register 105h bit 0).
    new_mode: bool,
    /// The value last written to each sound register (as logged: bank 1 is 100h-1FFh).
    registers: Vec<u8>,
    pub writes: crate::record::Record<crate::record::Log<OplWrite>>,
}

impl Opl {
    pub fn new(mode: OplMode) -> Self {
        let mut opl = Self {
            mode,
            chips: [Chip::new(), Chip::new()],
            address: 0,
            dual_address: [0; 2],
            new_mode: false,
            registers: vec![0; 0x200],
            writes: Default::default(),
        };
        if mode == OplMode::DualOpl2 {
            // The two OPL2s are an OPL3 with its channels panned left and right.
            opl.log(0, 0x105, 1);
        }
        opl
    }

    fn log(&mut self, nanos: u64, register: u16, value: u8) {
        if register == 0x105 {
            self.new_mode = value & 1 != 0;
        }
        self.registers[usize::from(register)] = value;
        self.writes.push(OplWrite { nanos, register, value });
    }

    /// Logs the registers again at `nanos`, for a machine loaded from a snapshot (whose log starts
    /// empty): a chip rendered from the log then plays what this one plays. The mode registers go
    /// first and the key-on and rhythm registers last, so notes start with their voices set up.
    pub(crate) fn relog(&mut self, nanos: u64) {
        let key = |r: &u16| matches!(r & 0xFF, 0xB0..=0xB8 | 0xBD);
        let mode = |r: &u16| matches!(r, 0x104 | 0x105);
        let order =
            [0x105, 0x104].into_iter().chain((0..0x200).filter(|r| !mode(r) && !key(r))).chain((0..0x200).filter(key));
        for register in order {
            let value = self.registers[usize::from(register)];
            if value != 0 {
                self.writes.push(OplWrite { nanos, register, value });
            }
        }
    }

    /// The register an address-port write selects; ports with bit 1 set address bank 1 in OPL3
    /// mode (and register 105h, which enables it).
    fn select(&self, port: u16, value: u8) -> u16 {
        let mut register = u16::from(value);
        if port & 2 != 0 && (register == 0x05 || self.new_mode) {
            register |= 0x100;
        }
        register
    }

    fn dual_write(&mut self, index: usize, register: u8, value: u8, now: u64) {
        // OPL3 features stay off: no NEW bit, only the four OPL2 waveforms.
        if register == 0x05 {
            return;
        }
        let mut value = if register >= 0xE0 { value & 3 } else { value };
        if self.chips[index].write(u16::from(register), value, now) {
            return;
        }
        if (0xC0..=0xC8).contains(&register) {
            value = (value & 0x0F) | if index == 1 { 0xA0 } else { 0x50 };
        }
        self.log(now, u16::from(register) + if index == 1 { 0x100 } else { 0 }, value);
    }

    pub fn write(&mut self, port: u16, value: u8, now: u64) {
        if port & 1 != 0 {
            match self.mode {
                OplMode::Opl2 | OplMode::Opl3 => {
                    if !self.chips[0].write(self.address, value, now) {
                        self.log(now, self.address, value);
                    }
                }
                OplMode::DualOpl2 => {
                    if port & 8 == 0 {
                        let index = usize::from((port & 2) >> 1);
                        self.dual_write(index, self.dual_address[index], value, now);
                    } else {
                        self.dual_write(0, self.dual_address[0], value, now);
                        self.dual_write(1, self.dual_address[1], value, now);
                    }
                }
            }
        } else {
            match self.mode {
                OplMode::Opl2 => self.address = self.select(port, value) & 0xFF,
                OplMode::Opl3 => self.address = self.select(port, value) & 0x1FF,
                OplMode::DualOpl2 => {
                    if port & 8 == 0 {
                        self.dual_address[usize::from((port & 2) >> 1)] = value;
                    } else {
                        self.dual_address = [value; 2];
                    }
                }
            }
        }
    }

    pub fn read(&mut self, port: u16, now: u64) -> u8 {
        match self.mode {
            // OPL2 status has bits 1-2 set; OPL3 detection relies on them being clear there.
            OplMode::Opl2 if port & 3 == 0 => self.chips[0].status(now) | 0x06,
            OplMode::DualOpl2 if port & 1 == 0 => self.chips[usize::from((port >> 1) & 1)].status(now) | 0x06,
            OplMode::Opl3 if port & 3 == 0 => self.chips[0].status(now),
            _ => 0xFF,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(opl: &mut Opl, register: u8, value: u8, now: u64) {
        opl.write(0x388, register, now);
        opl.write(0x389, value, now);
    }

    #[test]
    fn adlib_detection_sequence() {
        // The classic test: reset timers, read status (0), start timer 1 at FFh (80 µs), wait
        // over 80 µs, read status (C0h).
        let mut opl = Opl::new(OplMode::Opl2);
        write(&mut opl, 0x04, 0x60, 0);
        write(&mut opl, 0x04, 0x80, 0);
        assert_eq!(opl.read(0x388, 0) & 0xE0, 0x00);
        write(&mut opl, 0x02, 0xFF, 1_000);
        write(&mut opl, 0x04, 0x21, 1_000);
        assert_eq!(opl.read(0x388, 50_000) & 0xE0, 0x00, "not yet");
        assert_eq!(opl.read(0x388, 200_000) & 0xE0, 0xC0);
        write(&mut opl, 0x04, 0x60, 200_000);
        write(&mut opl, 0x04, 0x80, 200_000);
        assert!(opl.writes.is_empty(), "timer registers aren't sound registers");
    }

    #[test]
    fn opl3_bank_1_needs_the_new_bit() {
        let mut opl = Opl::new(OplMode::Opl3);
        opl.write(0x38A, 0xB0, 5);
        opl.write(0x38B, 0x20, 5);
        opl.write(0x38A, 0x05, 6);
        opl.write(0x38B, 0x01, 6);
        opl.write(0x38A, 0xB0, 7);
        opl.write(0x38B, 0x21, 7);
        let logged: Vec<(u16, u8)> = opl.writes.iter().map(|w| (w.register, w.value)).collect();
        assert_eq!(logged, [(0xB0, 0x20), (0x105, 0x01), (0x1B0, 0x21)]);
        assert_eq!(opl.read(0x388, 0), 0x00, "OPL3 status has bits 1-2 clear");
    }

    #[test]
    fn dual_opl2_pans_each_chip() {
        let mut opl = Opl::new(OplMode::DualOpl2);
        opl.write(0x222, 0xC0, 9); // right chip
        opl.write(0x223, 0x31, 9);
        opl.write(0x228, 0xC1, 10); // both chips
        opl.write(0x229, 0x01, 10);
        let logged: Vec<(u16, u8)> = opl.writes.iter().map(|w| (w.register, w.value)).collect();
        assert_eq!(logged, [(0x105, 1), (0x1C0, 0xA1), (0xC1, 0x51), (0x1C1, 0xA1)]);
    }
}
