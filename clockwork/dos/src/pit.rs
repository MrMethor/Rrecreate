//! Intel 8253/8254 programmable interval timer and the speaker gate on port 61h.
//!
//! All times are PIT ticks (see [`crate::clock`]). Channel 0 drives IRQ0, channel 2 the PC
//! speaker. Counters are computed from the time they were loaded, so reading them is cheap and
//! deterministic.

/// One state change of the PC speaker, for the audio observation channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpeakerEvent {
    pub pit_tick: u64,
    /// Whether channel 2 is gated through to the speaker.
    pub on: bool,
    /// Channel 2's divisor (tone frequency = 1 193 182 / divisor Hz); 0 means 65536.
    pub divisor: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Channel {
    pub mode: u8,
    /// 1 = low byte only, 2 = high byte only, 3 = low then high.
    access: u8,
    /// Raw divisor as written; 0 means 65536.
    pub divisor: u16,
    write_high_next: bool,
    pending_low: u8,
    read_high_next: bool,
    latch: Option<u16>,
    /// PIT tick at which the current divisor was loaded; `None` until a count is written.
    start: Option<u64>,
}

impl Channel {
    fn new(mode: u8) -> Self {
        Self {
            mode,
            access: 3,
            divisor: 0,
            write_high_next: false,
            pending_low: 0,
            read_high_next: false,
            latch: None,
            start: None,
        }
    }

    pub fn period(&self) -> u64 {
        if self.divisor == 0 { 0x1_0000 } else { u64::from(self.divisor) }
    }

    fn periodic(&self) -> bool {
        matches!(self.mode, 2 | 3)
    }

    fn count_at(&self, now: u64) -> u16 {
        let Some(start) = self.start else { return self.divisor };
        let elapsed = now.saturating_sub(start);
        let period = self.period();
        let count = match self.mode {
            // Mode 3 counts down by two, twice per output period.
            3 => (period - (elapsed * 2) % period) & !1,
            2 => period - elapsed % period,
            _ => (period + 0x1_0000 - elapsed % 0x1_0000) % 0x1_0000,
        };
        count as u16
    }

    /// Output level, used for channel 2 (port 61h bit 5).
    fn output_at(&self, now: u64) -> bool {
        let Some(start) = self.start else { return true };
        let elapsed = now.saturating_sub(start);
        match self.mode {
            3 => elapsed % self.period() < self.period().div_ceil(2),
            0 => elapsed >= self.period(),
            _ => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Pit {
    pub channels: [Channel; 3],
    /// PIT tick of channel 0's next IRQ0, if it is counting.
    next_irq0: Option<u64>,
    port61: u8,
    pub speaker_log: Vec<SpeakerEvent>,
    speaker_state: (bool, u16),
}

impl Default for Pit {
    fn default() -> Self {
        Self::new()
    }
}

impl Pit {
    /// The state the BIOS leaves behind: channel 0 in mode 3 with divisor 65536 (18.2 Hz),
    /// started at tick 0.
    pub fn new() -> Self {
        let mut pit = Self {
            channels: [Channel::new(3), Channel::new(2), Channel::new(3)],
            next_irq0: None,
            port61: 0,
            speaker_log: Vec::new(),
            speaker_state: (false, 0),
        };
        pit.load(0, 0);
        pit
    }

    pub fn next_irq0(&self) -> Option<u64> {
        self.next_irq0
    }

    /// Call when `now >= next_irq0()`; returns whether IRQ0 should be raised.
    pub fn take_irq0(&mut self, now: u64) -> bool {
        let Some(due) = self.next_irq0 else { return false };
        if now < due {
            return false;
        }
        let ch = &self.channels[0];
        self.next_irq0 = if ch.periodic() {
            let period = ch.period();
            // Several periods may have passed (one long REP instruction); the PIC latches one edge.
            Some(due + period * ((now - due) / period + 1))
        } else {
            None
        };
        true
    }

    pub fn write(&mut self, port: u16, value: u8, now: u64) {
        match port {
            0x43 => self.write_control(value, now),
            0x40..=0x42 => {
                let index = usize::from(port - 0x40);
                let ch = &mut self.channels[index];
                let complete = match ch.access {
                    1 => Some(u16::from(value)),
                    2 => Some(u16::from(value) << 8),
                    _ if !ch.write_high_next => {
                        ch.pending_low = value;
                        ch.write_high_next = true;
                        None
                    }
                    _ => {
                        ch.write_high_next = false;
                        Some(u16::from(ch.pending_low) | (u16::from(value) << 8))
                    }
                };
                if let Some(divisor) = complete {
                    self.channels[index].divisor = divisor;
                    self.load(index, now);
                }
            }
            _ => {}
        }
    }

    pub fn read(&mut self, port: u16, now: u64) -> u8 {
        let index = usize::from(port.wrapping_sub(0x40));
        if index > 2 {
            return 0xFF;
        }
        let ch = &mut self.channels[index];
        let value = ch.latch.unwrap_or_else(|| ch.count_at(now));
        let byte = match ch.access {
            1 => value as u8,
            2 => (value >> 8) as u8,
            _ => {
                let high = ch.read_high_next;
                ch.read_high_next = !high;
                if high { (value >> 8) as u8 } else { value as u8 }
            }
        };
        if ch.latch.is_some() && (ch.access != 3 || !ch.read_high_next) {
            ch.latch = None;
        }
        byte
    }

    /// Port 61h read: speaker gate and data bits, the DRAM refresh toggle (bit 4, every ~15 µs)
    /// and channel 2's output (bit 5). Some games time short delays with bits 4 and 5.
    pub fn read_port61(&self, now: u64) -> u8 {
        let refresh = if (now / 18) % 2 == 1 { 0x10 } else { 0 };
        let out2 = if self.channels[2].output_at(now) { 0x20 } else { 0 };
        (self.port61 & 0x0F) | refresh | out2
    }

    pub fn write_port61(&mut self, value: u8, now: u64) {
        self.port61 = value;
        self.log_speaker(now);
    }

    fn write_control(&mut self, value: u8, now: u64) {
        let index = usize::from(value >> 6);
        if index == 3 {
            return; // 8254 read-back command: not needed so far
        }
        let access = (value >> 4) & 3;
        let ch = &mut self.channels[index];
        if access == 0 {
            if ch.latch.is_none() {
                ch.latch = Some(ch.count_at(now));
                ch.read_high_next = false;
            }
            return;
        }
        let mode = (value >> 1) & 7;
        ch.mode = if mode >= 6 { mode - 4 } else { mode };
        ch.access = access;
        ch.write_high_next = false;
        ch.read_high_next = false;
        ch.latch = None;
        ch.start = None; // counting stops until a new count is written
        if index == 0 {
            self.next_irq0 = None;
        }
    }

    fn load(&mut self, index: usize, now: u64) {
        let ch = &mut self.channels[index];
        ch.start = Some(now);
        if index == 0 {
            self.next_irq0 = Some(now + ch.period());
        }
        if index == 2 {
            self.log_speaker(now);
        }
    }

    fn log_speaker(&mut self, now: u64) {
        let on = self.port61 & 0x03 == 0x03 && self.channels[2].start.is_some();
        let state = (on, self.channels[2].divisor);
        if state != self.speaker_state {
            self.speaker_state = state;
            self.speaker_log.push(SpeakerEvent { pit_tick: now, on, divisor: state.1 });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bios_default_is_18_hz() {
        let mut pit = Pit::new();
        assert_eq!(pit.next_irq0(), Some(65536));
        assert!(!pit.take_irq0(65535));
        assert!(pit.take_irq0(65536));
        assert_eq!(pit.next_irq0(), Some(131072));
    }

    #[test]
    fn reprogramming_channel_0() {
        let mut pit = Pit::new();
        pit.write(0x43, 0x36, 100); // channel 0, lo/hi, mode 3
        assert_eq!(pit.next_irq0(), None, "stopped until the count is written");
        pit.write(0x40, 0x9C, 100);
        pit.write(0x40, 0x2E, 100); // 0x2E9C = 11932 → 100 Hz
        assert_eq!(pit.next_irq0(), Some(100 + 11932));
        assert!(pit.take_irq0(100 + 3 * 11932 + 5), "late: one edge for several periods");
        assert_eq!(pit.next_irq0(), Some(100 + 4 * 11932));
    }

    #[test]
    fn latched_reads() {
        let mut pit = Pit::new();
        pit.write(0x43, 0x34, 0); // channel 0, lo/hi, mode 2
        pit.write(0x40, 0x00, 0);
        pit.write(0x40, 0x10, 0); // 4096
        pit.write(0x43, 0x00, 1000); // latch channel 0
        let lo = pit.read(0x40, 2000);
        let hi = pit.read(0x40, 2000);
        assert_eq!(u16::from(lo) | (u16::from(hi) << 8), 4096 - 1000);
        let lo = pit.read(0x40, 2000);
        let hi = pit.read(0x40, 2000);
        assert_eq!(u16::from(lo) | (u16::from(hi) << 8), 4096 - 2000, "latch released after both bytes");
    }

    #[test]
    fn speaker_events() {
        let mut pit = Pit::new();
        pit.write(0x43, 0xB6, 10); // channel 2, lo/hi, mode 3
        pit.write(0x42, 0xA9, 10);
        pit.write(0x42, 0x04, 10); // 0x04A9 = 1193 → 1 kHz
        pit.write_port61(0x03, 20);
        pit.write_port61(0x00, 30);
        assert_eq!(
            pit.speaker_log,
            vec![
                SpeakerEvent { pit_tick: 10, on: false, divisor: 0x04A9 },
                SpeakerEvent { pit_tick: 20, on: true, divisor: 0x04A9 },
                SpeakerEvent { pit_tick: 30, on: false, divisor: 0x04A9 },
            ]
        );
    }
}
