//! Intel 8253/8254 programmable interval timer and the speaker gate on port 61h.
//!
//! All times are PIT ticks (see [`crate::clock`]). Channel 0 drives IRQ0, channel 2 the PC
//! speaker. Counters are computed from the time they were loaded, so reading them is cheap and
//! deterministic. Channel 2's gate is port 61h bit 0, with the 8254's gate rules for each mode
//! (as in DOSBox Staging's `pcspeaker_pit.cpp`), and the speaker is driven by channel 2's output
//! AND port 61h bit 1. [`Pit::speaker_log`] records the speaker's waveform whenever it changes,
//! which is what lets programs play samples on it: by writing a count per sample in mode 0
//! (pulse-width modulation), or by toggling bit 1 themselves.

/// The speaker's input from a moment on, while nothing changes: high (pushed out) or low (at
/// rest) as a function of the PIT tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Wave {
    Steady(bool),
    /// Low until this tick, high from it on: modes 0 and 1 counting down.
    LowUntil(u64),
    /// High but for this one tick: the strobe of modes 4 and 5.
    PulseAt(u64),
    /// From tick `origin` on, high for `high` ticks and then low for the rest of each
    /// `period`: modes 2 and 3.
    Periodic {
        origin: u64,
        period: u32,
        high: u32,
    },
}

impl Wave {
    /// The level during tick `t` (for `Periodic`, `t >= origin`).
    pub fn level_at(&self, t: u64) -> bool {
        match *self {
            Wave::Steady(level) => level,
            Wave::LowUntil(until) => t >= until,
            Wave::PulseAt(at) => t != at,
            Wave::Periodic { origin, period, high } => t.saturating_sub(origin) % u64::from(period) < u64::from(high),
        }
    }

    /// How many ticks before `t` are high, counted from tick 0 (from `origin` for `Periodic`).
    /// The difference of two counts is the high time between them.
    pub fn high_ticks(&self, t: u64) -> u64 {
        match *self {
            Wave::Steady(level) => {
                if level {
                    t
                } else {
                    0
                }
            }
            Wave::LowUntil(until) => t.saturating_sub(until),
            Wave::PulseAt(at) => t - u64::from(t > at),
            Wave::Periodic { origin, period, high } => {
                let elapsed = t.saturating_sub(origin);
                let (period, high) = (u64::from(period), u64::from(high));
                elapsed / period * high + (elapsed % period).min(high)
            }
        }
    }
}

/// The speaker's waveform from `pit_tick` on, until the next event (before the first one, it is
/// at rest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SpeakerEvent {
    pub pit_tick: u64,
    pub wave: Wave,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
    /// PIT tick at which the current count started; `None` while it isn't counting (no count
    /// written since the control word, or waiting for the gate).
    start: Option<u64>,
    /// The gate input: high for channels 0 and 1, port 61h bit 0 for channel 2.
    gate: bool,
    /// A count was written since the control word.
    loaded: bool,
    /// Modes 0 and 4 with the gate low: the ticks counted before it went low.
    paused: Option<u64>,
}

impl Channel {
    fn new(mode: u8, gate: bool) -> Self {
        Self {
            mode,
            access: 3,
            divisor: 0,
            write_high_next: false,
            pending_low: 0,
            read_high_next: false,
            latch: None,
            start: None,
            gate,
            loaded: false,
            paused: None,
        }
    }

    pub fn period(&self) -> u64 {
        if self.divisor == 0 { 0x1_0000 } else { u64::from(self.divisor) }
    }

    fn periodic(&self) -> bool {
        matches!(self.mode, 2 | 3)
    }

    /// Ticks counted so far, if the count was started.
    fn elapsed(&self, now: u64) -> Option<u64> {
        self.start.map(|start| now.saturating_sub(start)).or(self.paused)
    }

    fn count_at(&self, now: u64) -> u16 {
        let Some(elapsed) = self.elapsed(now) else { return self.divisor };
        let period = self.period();
        let count = match self.mode {
            // Mode 3 counts down by two, twice per output period.
            3 => (period - (elapsed * 2) % period) & !1,
            2 => period - elapsed % period,
            _ => (period + 0x1_0000 - elapsed % 0x1_0000) % 0x1_0000,
        };
        count as u16
    }

    /// The output from `now` on, while nothing changes (8254 data sheet, mode definitions).
    fn wave(&self, now: u64) -> Wave {
        let period = self.period();
        let Some(start) = self.start else {
            // Mode 0 is low from the control word to the terminal count; a stopped count in
            // the other modes leaves the output high.
            return Wave::Steady(self.mode != 0 || self.paused.is_some_and(|counted| counted >= period));
        };
        match self.mode {
            0 | 1 if now >= start + period => Wave::Steady(true),
            0 | 1 => Wave::LowUntil(start + period),
            4 | 5 if now > start + period => Wave::Steady(true),
            4 | 5 => Wave::PulseAt(start + period),
            // Rate generator: low for the last tick of each period.
            2 if period >= 2 => Wave::Periodic { origin: start, period: period as u32, high: period as u32 - 1 },
            3 => Wave::Periodic { origin: start, period: period as u32, high: period.div_ceil(2) as u32 },
            _ => Wave::Steady(true),
        }
    }

    fn output_at(&self, now: u64) -> bool {
        self.wave(now).level_at(now)
    }

    /// A count was written: modes 0, 2, 3 and 4 start counting (when the gate is high),
    /// modes 1 and 5 wait for the gate to rise.
    fn load(&mut self, now: u64) {
        self.loaded = true;
        match self.mode {
            0 | 4 if self.gate => (self.start, self.paused) = (Some(now), None),
            0 | 4 => (self.start, self.paused) = (None, Some(0)),
            2 | 3 => self.start = self.gate.then_some(now),
            _ => {}
        }
    }

    fn set_gate(&mut self, gate: bool, now: u64) {
        let rising = gate && !self.gate;
        let falling = !gate && self.gate;
        self.gate = gate;
        match self.mode {
            // Gate low stops the count, gate high resumes it.
            0 | 4 if rising => {
                if let Some(counted) = self.paused.take() {
                    self.start = Some(now - counted.min(now));
                }
            }
            0 | 4 if falling => {
                if let Some(start) = self.start.take() {
                    self.paused = Some(now.saturating_sub(start));
                }
            }
            // A rising gate (re)triggers the one-shot.
            1 | 5 if rising && self.loaded => self.start = Some(now),
            // Gate low forces the output high and stops counting; gate high starts over.
            2 | 3 if rising && self.loaded => self.start = Some(now),
            2 | 3 if falling => self.start = None,
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Pit {
    pub channels: [Channel; 3],
    /// PIT tick of channel 0's next IRQ0, if it is counting.
    next_irq0: Option<u64>,
    port61: u8,
    pub speaker_log: Vec<SpeakerEvent>,
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
            channels: [Channel::new(3, true), Channel::new(2, true), Channel::new(3, false)],
            next_irq0: None,
            port61: 0,
            speaker_log: Vec::new(),
        };
        pit.load(0, 0);
        pit
    }

    pub fn next_irq0(&self) -> Option<u64> {
        self.next_irq0
    }

    /// Call when `now >= next_irq0()`; returns how many edges IRQ0 had since (0: none). Several
    /// periods may have passed (one long REP instruction): the PIC latches one edge.
    pub fn take_irq0(&mut self, now: u64) -> u64 {
        let Some(due) = self.next_irq0 else { return 0 };
        if now < due {
            return 0;
        }
        let ch = &self.channels[0];
        let (edges, next) = if ch.periodic() {
            let period = ch.period();
            let edges = (now - due) / period + 1;
            (edges, Some(due + period * edges))
        } else {
            (1, None)
        };
        self.next_irq0 = next;
        edges
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
        self.channels[2].set_gate(value & 0x01 != 0, now);
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
        // Counting stops until a new count is written.
        (ch.start, ch.loaded, ch.paused) = (None, false, None);
        if index == 0 {
            self.next_irq0 = None;
        }
        if index == 2 {
            self.log_speaker(now);
        }
    }

    fn load(&mut self, index: usize, now: u64) {
        let ch = &mut self.channels[index];
        ch.load(now);
        if index == 0 {
            self.next_irq0 = Some(now + ch.period());
        }
        if index == 2 {
            self.log_speaker(now);
        }
    }

    fn log_speaker(&mut self, now: u64) {
        let wave = if self.port61 & 0x02 == 0 { Wave::Steady(false) } else { self.channels[2].wave(now) };
        let current = self.speaker_log.last().map_or(Wave::Steady(false), |e| e.wave);
        if wave != current {
            self.speaker_log.push(SpeakerEvent { pit_tick: now, wave });
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
        assert_eq!(pit.take_irq0(65535), 0);
        assert_eq!(pit.take_irq0(65536), 1);
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
        assert_eq!(pit.take_irq0(100 + 3 * 11932 + 5), 3, "late: the edges of several periods");
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
        pit.write_port61(0x03, 20); // gate and speaker on: the square wave starts
        pit.write_port61(0x00, 30);
        let square = Wave::Periodic { origin: 20, period: 1193, high: 597 };
        assert_eq!(
            pit.speaker_log,
            vec![SpeakerEvent { pit_tick: 20, wave: square }, SpeakerEvent { pit_tick: 30, wave: Wave::Steady(false) }]
        );
    }

    #[test]
    fn channel_2_follows_its_gate() {
        let mut pit = Pit::new();
        pit.write(0x43, 0xB6, 0); // mode 3
        pit.write(0x42, 100, 0);
        pit.write(0x42, 0, 0);
        assert_eq!(pit.read_port61(10) & 0x20, 0x20, "gate low: output high");
        assert_eq!(pit.read_port61(70) & 0x20, 0x20);
        pit.write_port61(0x01, 100); // gate high: the square wave starts over
        assert_eq!(pit.read_port61(149) & 0x20, 0x20);
        assert_eq!(pit.read_port61(150) & 0x20, 0, "low for the second half");
        assert_eq!(pit.read_port61(200) & 0x20, 0x20);
        // Mode 0, gate high: low from the count until it runs out; gate low pauses it.
        pit.write(0x43, 0xB0, 300);
        assert_eq!(pit.read_port61(300) & 0x20, 0);
        pit.write(0x42, 50, 310);
        pit.write(0x42, 0, 310);
        pit.write_port61(0x00, 330); // 20 counted
        assert_eq!(pit.read_port61(400) & 0x20, 0);
        pit.write_port61(0x01, 500);
        assert_eq!(pit.read_port61(529) & 0x20, 0);
        assert_eq!(pit.read_port61(530) & 0x20, 0x20, "30 more ticks");
        // Mode 1: the gate's rising edge starts the one-shot.
        pit.write(0x43, 0xB2, 600);
        pit.write(0x42, 10, 600);
        pit.write(0x42, 0, 600);
        assert_eq!(pit.read_port61(700) & 0x20, 0x20, "waiting for the trigger");
        pit.write_port61(0x00, 700);
        pit.write_port61(0x01, 710);
        assert_eq!(pit.read_port61(715) & 0x20, 0);
        assert_eq!(pit.read_port61(720) & 0x20, 0x20);
    }

    #[test]
    fn pulse_width_modulation_logs_each_count() {
        // Mode 0 with the speaker on: each count sets how long the output stays low.
        let mut pit = Pit::new();
        pit.write_port61(0x03, 0);
        pit.write(0x43, 0x90, 0); // channel 2, low byte only, mode 0
        for (i, count) in [30u8, 50, 10].into_iter().enumerate() {
            pit.write(0x42, count, 100 * i as u64);
        }
        let waves: Vec<Wave> = pit.speaker_log.iter().map(|e| e.wave).collect();
        assert_eq!(
            waves,
            [Wave::Steady(true), Wave::Steady(false), Wave::LowUntil(30), Wave::LowUntil(150), Wave::LowUntil(210)]
        );
        assert_eq!(pit.speaker_log[2].pit_tick, 0);
    }

    #[test]
    fn waves_count_their_high_ticks() {
        let square = Wave::Periodic { origin: 10, period: 5, high: 3 };
        assert_eq!(square.high_ticks(10), 0);
        assert_eq!(square.high_ticks(13), 3);
        assert_eq!(square.high_ticks(15), 3);
        assert_eq!(square.high_ticks(22), 3 + 3 + 2);
        assert!(square.level_at(20) && !square.level_at(23));
        assert_eq!(Wave::LowUntil(8).high_ticks(10) - Wave::LowUntil(8).high_ticks(5), 2);
        assert_eq!(Wave::PulseAt(4).high_ticks(10), 9);
        assert_eq!(Wave::Steady(false).high_ticks(10), 0);
    }
}
