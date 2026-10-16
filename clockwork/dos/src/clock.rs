//! Virtual time (PROJECT.md §5.2, "Virtual clock").
//!
//! Time is derived from the CPU's cycle counter, never from the host's clock. One cycle is one
//! executed instruction (or string repetition); `instructions_per_second` sets how fast the
//! emulated PC is. Devices count time in PIT ticks (1 193 182 Hz).

pub const PIT_HZ: u64 = 1_193_182;

/// VGA vertical refresh in modes 3 and 13h: 70 Hz.
pub const FRAME_PIT_TICKS: u64 = PIT_HZ / 70;

/// Scan lines per frame (400 visible + 49 blanking) in the 70 Hz VGA timing.
pub const LINES_PER_FRAME: u64 = 449;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Clock {
    pub instructions_per_second: u64,
}

impl Clock {
    pub fn new(instructions_per_second: u64) -> Self {
        assert!(instructions_per_second > 0, "instructions_per_second must be positive");
        Self { instructions_per_second }
    }

    /// PIT ticks elapsed at `cycles`.
    pub fn pit_ticks(&self, cycles: u64) -> u64 {
        (u128::from(cycles) * u128::from(PIT_HZ) / u128::from(self.instructions_per_second)) as u64
    }

    /// The first cycle at which `pit_ticks(cycle) >= ticks`.
    pub fn cycle_at(&self, ticks: u64) -> u64 {
        let ips = u128::from(self.instructions_per_second);
        u128::from(ticks).saturating_mul(ips).div_ceil(u128::from(PIT_HZ)).min(u128::from(u64::MAX)) as u64
    }

    /// Whole milliseconds elapsed at `cycles`.
    pub fn millis(&self, cycles: u64) -> u64 {
        (u128::from(cycles) * 1000 / u128::from(self.instructions_per_second)) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_at_is_the_inverse_of_pit_ticks() {
        let clock = Clock::new(8_000_000);
        for ticks in [0, 1, 7, 65536, 1_193_182, 123_456_789] {
            let c = clock.cycle_at(ticks);
            assert!(clock.pit_ticks(c) >= ticks);
            assert!(c == 0 || clock.pit_ticks(c - 1) < ticks);
        }
        assert_eq!(clock.millis(8_000_000), 1000);
    }
}
