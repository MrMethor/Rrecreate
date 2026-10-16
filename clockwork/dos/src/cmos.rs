//! CMOS RAM and real-time clock (ports 70h/71h). The clock reads the emulated wall clock, so
//! it is deterministic like everything else.

use crate::time::DateTime;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cmos {
    index: u8,
    ram: [u8; 128],
    /// Emulated wall clock at cycle 0, in seconds since 1970.
    start_epoch: i64,
}

fn bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

impl Cmos {
    pub fn new(start: DateTime) -> Self {
        let mut ram = [0u8; 128];
        ram[0x0A] = 0x26; // 32.768 kHz time base, 1024 Hz periodic rate; no update in progress
        ram[0x0B] = 0x02; // 24-hour mode, BCD
        ram[0x0D] = 0x80; // battery good
        ram[0x10] = 0x40; // one 1.44 MB floppy drive
        ram[0x14] = 0x21; // equipment: floppy, 80-column color
        ram[0x15..0x17].copy_from_slice(&640u16.to_le_bytes()); // base memory in KB
        Self { index: 0, ram, start_epoch: start.to_epoch_seconds() }
    }

    pub fn write_index(&mut self, value: u8) {
        self.index = value & 0x7F; // bit 7 is the NMI mask
    }

    pub fn write_data(&mut self, value: u8) {
        if !matches!(self.index, 0x00..=0x09 | 0x0C | 0x0D | 0x32) {
            self.ram[usize::from(self.index)] = value;
        }
    }

    /// Reads the selected register at `seconds` of emulated time.
    pub fn read_data(&self, seconds: u64) -> u8 {
        let now = DateTime::from_epoch_seconds(self.start_epoch + seconds as i64);
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
        let mut cmos = Cmos::new(DateTime::new(1994, 1, 1, 8, 0, 0));
        let read = |cmos: &mut Cmos, index: u8, seconds: u64| {
            cmos.write_index(index);
            cmos.read_data(seconds)
        };
        assert_eq!(read(&mut cmos, 0x04, 0), 0x08);
        assert_eq!(read(&mut cmos, 0x00, 75), 0x15, "8:01:15");
        assert_eq!(read(&mut cmos, 0x02, 75), 0x01);
        assert_eq!(read(&mut cmos, 0x09, 0), 0x94);
        assert_eq!(read(&mut cmos, 0x32, 0), 0x19);
        assert_eq!(read(&mut cmos, 0x15, 0), 0x80, "640 KB, low byte");
        cmos.write_index(0x40);
        cmos.write_data(0x5A);
        assert_eq!(cmos.read_data(0), 0x5A);
    }
}
