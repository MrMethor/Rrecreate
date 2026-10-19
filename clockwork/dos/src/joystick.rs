//! The game port (201h) with up to two analog joysticks, and the BIOS joystick service
//! (INT 15h AH=84h).
//!
//! Ported from DOSBox Staging `src/hardware/input/joystick.cpp` (its default "timed" mode) and
//! the AH=84h handler in `src/ints/bios.cpp` (GPL-2.0-or-later). A write to the port starts
//! each connected axis's one-shot timer; the axis bit reads 1 until it runs out, after a time
//! that grows with the stick's position (DOSBox's rates, measured on real cards with joycheck).
//! Time is the virtual clock's, in nanoseconds. Buttons read 0 while pressed. Like DOSBox
//! without a host joystick, no stick is connected by default, and the port then reads FFh.

/// How long an axis's timer runs at position `p` (-1.0..=1.0): `(p + 1) × scale + offset`,
/// from DOSBox's default axis rates (in nanoseconds).
const X_SCALE_NS: u64 = 556_000;
const Y_SCALE_NS: u64 = 555_000;
const OFFSET_NS: u64 = 20_000;

/// Positions are -32767 (left, up) ..= 32767 (right, down).
pub const AXIS_MAX: i16 = 32767;

/// One joystick: its position and buttons (bit 0: button 1, bit 1: button 2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Stick {
    pub x: i16,
    pub y: i16,
    pub buttons: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct GamePort {
    /// The connected sticks (A and B); `None` is not connected.
    pub sticks: [Option<Stick>; 2],
    /// When each stick's X and Y timers run out (ns of emulated time).
    deadlines: [[u64; 2]; 2],
}

/// Nanoseconds an axis's timer runs at `position`.
fn axis_time(position: i16, scale: u64) -> u64 {
    let from_left = u64::from((i32::from(position.clamp(-AXIS_MAX, AXIS_MAX)) + i32::from(AXIS_MAX)) as u32);
    OFFSET_NS + from_left * scale / AXIS_MAX as u64
}

/// The 0-255 value the BIOS reports for a position (DOSBox: `position × 127 + 128`).
fn bios_value(position: i16) -> u16 {
    let scaled = i32::from(position.clamp(-AXIS_MAX, AXIS_MAX)) * 127 / i32::from(AXIS_MAX);
    (scaled + 128) as u16
}

impl GamePort {
    /// A port with `count` (0-2) centered sticks connected.
    pub fn new(count: u8) -> Self {
        let mut port = Self::default();
        for stick in port.sticks.iter_mut().take(usize::from(count.min(2))) {
            *stick = Some(Stick::default());
        }
        port
    }

    pub fn connected(&self) -> bool {
        self.sticks.iter().any(Option::is_some)
    }

    /// Sets stick `index`'s position and buttons, if it is connected.
    pub fn set(&mut self, index: usize, stick: Stick) {
        if let Some(Some(s)) = self.sticks.get_mut(index) {
            *s = stick;
        }
    }

    /// A write to 201h at `now` (ns): starts the timers.
    pub fn write(&mut self, now: u64) {
        for (stick, deadlines) in self.sticks.iter().zip(self.deadlines.iter_mut()) {
            if let Some(s) = stick {
                *deadlines = [now + axis_time(s.x, X_SCALE_NS), now + axis_time(s.y, Y_SCALE_NS)];
            }
        }
    }

    /// A read of 201h at `now` (ns): bits 0-3 the axes (A X, A Y, B X, B Y; 1 while timing),
    /// bits 4-7 the buttons (A 1, A 2, B 1, B 2; 0 while pressed).
    pub fn read(&self, now: u64) -> u8 {
        let mut value = 0xFF;
        for (i, (stick, deadlines)) in self.sticks.iter().zip(self.deadlines).enumerate() {
            let Some(s) = stick else { continue };
            for (axis, deadline) in deadlines.into_iter().enumerate() {
                if deadline < now {
                    value &= !(1 << (2 * i + axis));
                }
            }
            for button in 0..2 {
                if s.buttons & (1 << button) != 0 {
                    value &= !(1 << (4 + 2 * i + button));
                }
            }
        }
        value
    }

    /// INT 15h AH=84h, DX=0001h: the positions as AX, BX (stick A) and CX, DX (stick B), 0-255,
    /// or `None` when no stick is connected.
    pub fn bios_positions(&self) -> Option<[u16; 4]> {
        if !self.connected() {
            return None;
        }
        let axes = |s: &Option<Stick>| s.map_or([0, 0], |s| [bios_value(s.x), bios_value(s.y)]);
        let ([ax, bx], [cx, dx]) = (axes(&self.sticks[0]), axes(&self.sticks[1]));
        Some([ax, bx, cx, dx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_connected_reads_ff() {
        let mut port = GamePort::new(0);
        port.write(0);
        assert_eq!(port.read(10), 0xFF);
        assert_eq!(port.bios_positions(), None);
    }

    #[test]
    fn axis_bits_time_out_by_position() {
        let mut port = GamePort::new(1);
        port.set(0, Stick { x: -AXIS_MAX, y: AXIS_MAX, buttons: 0b10 });
        assert_eq!(port.read(1) & 0x0F, 0x0C, "before a write the timers have run out");
        port.write(1_000_000);
        assert_eq!(port.read(1_000_000), 0xDF, "both axes timing; button 2 pressed");
        assert_eq!(port.read(1_000_000 + 21_000), 0xDE, "full left: 20 µs");
        assert_eq!(port.read(1_000_000 + 1_000_000), 0xDE, "full down: 1.13 ms");
        assert_eq!(port.read(1_000_000 + 1_131_000), 0xDC);
        assert_eq!(port.bios_positions(), Some([1, 255, 0, 0]));
    }

    #[test]
    fn centered_stick_is_mid_scale() {
        let mut port = GamePort::new(2);
        port.write(0);
        assert_eq!(port.read(575_000) & 0x0F, 0x0F);
        assert_eq!(port.read(577_000) & 0x0F, 0x00, "center: 576 µs (555 + 20 for Y)");
        assert_eq!(port.bios_positions(), Some([128, 128, 128, 128]));
    }
}
