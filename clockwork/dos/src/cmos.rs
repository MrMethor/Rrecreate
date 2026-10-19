//! CMOS RAM and real-time clock (ports 70h/71h). The clock reads the emulated wall clock, so
//! it is deterministic like everything else.
//!
//! The periodic interrupt is ported from DOSBox Staging `src/hardware/cmos.cpp`
//! (GPL-2.0-or-later): register A's rate and register B's PIE bit raise IRQ 8 on a 32.768 kHz
//! time base that runs from power-on, register C reports and clears the flags, and, as in
//! DOSBox, a new interrupt is raised only after register C was read since the last one.
//! Programs that poll register C with the interrupt off see the periodic and update-ended flags
//! come up on time. Time is the virtual clock's, counted in [`RTC_HZ`] ticks.

use crate::time::DateTime;

/// The real-time clock's time base.
pub const RTC_HZ: u64 = 32_768;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Cmos {
    index: u8,
    #[serde(with = "serde_big_array::BigArray")]
    ram: [u8; 128],
    /// Emulated wall clock at cycle 0, in seconds since 1970.
    start_epoch: i64,
    /// When the next periodic interrupt is due (RTC ticks), while it is enabled.
    next_periodic: Option<u64>,
    /// Register C was read since the last periodic interrupt, so the next one raises IRQ 8.
    acknowledged: bool,
    /// For programs polling register C with the interrupt off: when it last showed the periodic
    /// and the update-ended flag.
    polled_periodic: u64,
    polled_update: u64,
}

fn bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

impl Cmos {
    /// CMOS contents for a PC with `extended_kb` of memory above 1 MB, with or without a math
    /// coprocessor.
    pub fn new(start: DateTime, extended_kb: u32, fpu: bool) -> Self {
        let mut ram = [0u8; 128];
        ram[0x0A] = 0x26; // 32.768 kHz time base, 1024 Hz periodic rate; no update in progress
        ram[0x0B] = 0x02; // 24-hour mode, BCD
        ram[0x0D] = 0x80; // battery good
        ram[0x10] = 0x40; // one 1.44 MB floppy drive
        ram[0x14] = 0x21 | (u8::from(fpu) << 1); // equipment: floppy, 80-column color, coprocessor
        ram[0x15..0x17].copy_from_slice(&640u16.to_le_bytes()); // base memory in KB
        let extended = (extended_kb.min(0xFFFF) as u16).to_le_bytes();
        ram[0x17..0x19].copy_from_slice(&extended); // extended memory in KB
        ram[0x30..0x32].copy_from_slice(&extended); // the same, as found at POST
        Self {
            index: 0,
            ram,
            start_epoch: start.to_epoch_seconds(),
            next_periodic: None,
            acknowledged: true,
            polled_periodic: 0,
            polled_update: 0,
        }
    }

    /// The periodic interrupt's period in RTC ticks, from register A's rate select: 3-15 give
    /// 8192 Hz down to 2 Hz, 1 and 2 act as 8 and 9 (256 and 128 Hz, as in DOSBox), 0 is off.
    fn period(&self) -> Option<u64> {
        match self.ram[0x0A] & 0x0F {
            0 => None,
            rate @ 1..=2 => Some(1 << (rate + 6)),
            rate => Some(1 << (rate - 1)),
        }
    }

    fn periodic_enabled(&self) -> bool {
        self.ram[0x0B] & 0x40 != 0
    }

    /// Schedules the next periodic interrupt on the clock's grid of periods since power-on.
    fn reschedule(&mut self, now: u64) {
        self.next_periodic = match self.period() {
            Some(period) if self.periodic_enabled() => Some((now / period + 1) * period),
            _ => None,
        };
    }

    /// When the next periodic interrupt is due, in RTC ticks.
    pub fn next_periodic(&self) -> Option<u64> {
        self.next_periodic
    }

    /// Brings the periodic interrupt up to `now` (RTC ticks). Returns whether IRQ 8 should be
    /// raised: one is due and register C was read since the last one.
    pub fn update(&mut self, now: u64) -> bool {
        let (Some(due), Some(period)) = (self.next_periodic, self.period()) else { return false };
        if now < due {
            return false;
        }
        self.next_periodic = Some((now / period + 1) * period);
        self.ram[0x0C] = 0xC0; // IRQF and PF
        std::mem::replace(&mut self.acknowledged, false)
    }

    pub fn write_index(&mut self, value: u8) {
        self.index = value & 0x7F; // bit 7 is the NMI mask
    }

    /// Writes the selected register at `now` (RTC ticks).
    pub fn write_data(&mut self, value: u8, now: u64) {
        match self.index {
            0x00..=0x09 | 0x0C | 0x0D | 0x32 => {}
            0x0A | 0x0B => {
                self.ram[usize::from(self.index)] = value & 0x7F;
                self.reschedule(now);
            }
            index => self.ram[usize::from(index)] = value,
        }
    }

    /// Reads the selected register at `now` (RTC ticks). Reading register C acknowledges the
    /// periodic interrupt and clears its flags.
    pub fn read_data(&mut self, now: u64) -> u8 {
        if self.index == 0x0C {
            self.acknowledged = true;
            if self.periodic_enabled() {
                return std::mem::take(&mut self.ram[0x0C]);
            }
            let mut flags = 0;
            if let Some(period) = self.period()
                && now >= self.polled_periodic + period
            {
                self.polled_periodic = now;
                flags |= 0x40;
            }
            if now >= self.polled_update + RTC_HZ {
                self.polled_update = now;
                flags |= 0x10;
            }
            return flags;
        }
        let now = DateTime::from_epoch_seconds(self.start_epoch + (now / RTC_HZ) as i64);
        match self.index {
            0x00 => bcd(now.second),
            0x02 => bcd(now.minute),
            0x04 => bcd(now.hour),
            0x06 => now.weekday() + 1,
            0x07 => bcd(now.day),
            0x08 => bcd(now.month),
            0x09 => bcd((now.year % 100) as u8),
            0x32 => bcd((now.year / 100) as u8),
            index => self.ram[usize::from(index)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_registers_in_bcd() {
        let mut cmos = Cmos::new(DateTime::new(1994, 1, 1, 8, 0, 0), 15 * 1024, true);
        let read = |cmos: &mut Cmos, index: u8, seconds: u64| {
            cmos.write_index(index);
            cmos.read_data(seconds * RTC_HZ)
        };
        assert_eq!(read(&mut cmos, 0x04, 0), 0x08);
        assert_eq!(read(&mut cmos, 0x00, 75), 0x15, "8:01:15");
        assert_eq!(read(&mut cmos, 0x02, 75), 0x01);
        assert_eq!(read(&mut cmos, 0x09, 0), 0x94);
        assert_eq!(read(&mut cmos, 0x32, 0), 0x19);
        assert_eq!(read(&mut cmos, 0x15, 0), 0x80, "640 KB, low byte");
        assert_eq!((read(&mut cmos, 0x17, 0), read(&mut cmos, 0x18, 0)), (0x00, 0x3C), "15 MB extended");
        assert_eq!(read(&mut cmos, 0x14, 0), 0x23, "equipment with a coprocessor");
        cmos.write_index(0x40);
        cmos.write_data(0x5A, 0);
        assert_eq!(cmos.read_data(0), 0x5A);
    }

    #[test]
    fn periodic_interrupt_needs_register_c_read() {
        let mut cmos = Cmos::new(DateTime::new(1994, 1, 1, 8, 0, 0), 0, false);
        assert_eq!(cmos.next_periodic(), None, "off at power-on");
        cmos.write_index(0x0B);
        cmos.write_data(0x42, 100); // PIE, 24-hour mode
        // Register A's default rate 6 is 1024 Hz: every 32 ticks on the grid since power-on.
        assert_eq!(cmos.next_periodic(), Some(128));
        assert!(!cmos.update(127));
        assert!(cmos.update(128), "the first interrupt");
        assert!(!cmos.update(160), "not acknowledged yet");
        cmos.write_index(0x0C);
        assert_eq!(cmos.read_data(161), 0xC0);
        assert_eq!(cmos.read_data(161), 0, "reading clears the flags");
        assert!(cmos.update(192));
        cmos.write_index(0x0A);
        cmos.write_data(0x2F, 200); // 2 Hz
        assert_eq!(cmos.next_periodic(), Some(16384));
        cmos.write_index(0x0B);
        cmos.write_data(0x02, 300);
        assert_eq!(cmos.next_periodic(), None);
    }

    #[test]
    fn register_c_polled_with_the_interrupt_off() {
        let mut cmos = Cmos::new(DateTime::new(1994, 1, 1, 8, 0, 0), 0, false);
        cmos.write_index(0x0C);
        assert_eq!(cmos.read_data(10), 0);
        assert_eq!(cmos.read_data(40), 0x40, "a 1024 Hz period passed");
        assert_eq!(cmos.read_data(RTC_HZ), 0x50, "and a second");
    }
}
