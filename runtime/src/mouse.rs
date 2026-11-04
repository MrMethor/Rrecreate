//! A program's own calls to its mouse driver (INT 33h) that change the driver (D93, D133).
//!
//! The `mouse` input is what the driver would report (D49, `docs/formats.md`): 23 little-endian words, the
//! buttons, x and y (03h), the motion in mickeys since the last 0Bh, then the left, right and middle buttons'
//! presses and releases (05h, 06h: a count and where the last one was). A call that changes that state (a
//! position set, a range that moves the position into it, a count or a motion read and so cleared) is the
//! program's own change, so the reads after it see it and a recording doesn't keep them as late reads (D93):
//! the port makes the call where the original did, and the change goes into the tick's inputs as the program's
//! own write ([`Inputs::write`], D97). The driver also keeps what it doesn't report, the ranges positions are kept
//! in and the granularity its video mode reports them with: [`MouseRanges`], which a port keeps in its state
//! (unmapped), and a live platform's driver takes from it with the calls' writes (`rrc_platform::MouseDriver::
//! wrote`).
//!
//! The calls as the Microsoft mouse driver documents them (and as Clockwork's and DOSBox's drivers make them): a
//! position set is kept in the ranges; a range given either way round is its ends; setting one moves a position
//! outside it to its nearer end; a read of the motion or of a button's presses or releases answers and clears it.

use serde::{Deserialize, Serialize};

use crate::Inputs;

/// The device's input.
pub const MOUSE: &str = "mouse";

/// Its words: x and y, the motion, the first press's count (then each button's three words, the presses' then the
/// releases').
const X: usize = 1;
const Y: usize = 2;
const MOTION: usize = 3;
const PRESSES: usize = 5;
const RELEASES: usize = 14;

/// What a driver keeps that its device doesn't report: the ranges a position is kept in (07h, 08h, inclusive) and
/// the granularity its video mode reports positions with (the bits kept).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseRanges {
    pub x: (i32, i32),
    pub y: (i32, i32),
    pub granularity: (u16, u16),
}

impl Default for MouseRanges {
    fn default() -> MouseRanges {
        MouseRanges::MODE_13H
    }
}

impl MouseRanges {
    /// After VGA's mode 13h is set (or the driver reset in it): 640 across, so x is twice the screen's, x even, and
    /// 200 down.
    pub const MODE_13H: MouseRanges = MouseRanges { x: (0, 639), y: (0, 199), granularity: (0xFFFE, 0xFFFF) };

    /// 04h: the position set at (`x`, `y`), kept in the ranges.
    pub fn set_position(&self, inputs: &Inputs, x: i16, y: i16) {
        let x = i32::from(x).clamp(self.x.0, self.x.1);
        let y = i32::from(y).clamp(self.y.0, self.y.1);
        self.put(inputs, x, y);
    }

    /// 07h: the range across, from `a` to `b` either way round; a position outside it moves to its nearer end.
    pub fn set_x_range(&mut self, inputs: &Inputs, a: i16, b: i16) {
        self.x = ends(a, b);
        let (x, y) = position(inputs);
        self.put(inputs, x.clamp(self.x.0, self.x.1), y);
    }

    /// 08h: the range down, as [`MouseRanges::set_x_range`] across.
    pub fn set_y_range(&mut self, inputs: &Inputs, a: i16, b: i16) {
        self.y = ends(a, b);
        let (x, y) = position(inputs);
        self.put(inputs, x, y.clamp(self.y.0, self.y.1));
    }

    /// The position reported, at its granularity.
    fn put(&self, inputs: &Inputs, x: i32, y: i32) {
        let mut bytes = (x as u16 & self.granularity.0).to_le_bytes().to_vec();
        bytes.extend((y as u16 & self.granularity.1).to_le_bytes());
        inputs.write(MOUSE, 2 * X, &bytes);
    }
}

/// The position the device reports now, as the pass knows it ([`Inputs::current`]).
pub fn position(inputs: &Inputs) -> (i32, i32) {
    let state = inputs.current(MOUSE);
    (i32::from(word(&state, X) as i16), i32::from(word(&state, Y) as i16))
}

/// 0Bh, called at `site` (where its read is, `Inputs::at_site`): the motion in mickeys since the last, which the
/// call clears.
pub fn read_motion(inputs: &Inputs, site: &str) -> (i16, i16) {
    let state = inputs.at_site(site).bytes(MOUSE).to_vec();
    inputs.write(MOUSE, 2 * MOTION, &[0; 4]);
    (word(&state, MOTION) as i16, word(&state, MOTION + 1) as i16)
}

/// 05h for `button` (0 left, 1 right, 2 middle), called at `site`: how many presses since the last call and where
/// the last was, which the call clears.
pub fn read_presses(inputs: &Inputs, site: &str, button: usize) -> (u16, i16, i16) {
    read_button(inputs, site, PRESSES + 3 * button.min(2))
}

/// 06h, as [`read_presses`] for releases.
pub fn read_releases(inputs: &Inputs, site: &str, button: usize) -> (u16, i16, i16) {
    read_button(inputs, site, RELEASES + 3 * button.min(2))
}

fn read_button(inputs: &Inputs, site: &str, at: usize) -> (u16, i16, i16) {
    let state = inputs.at_site(site).bytes(MOUSE).to_vec();
    inputs.write(MOUSE, 2 * at, &[0; 6]);
    (word(&state, at), word(&state, at + 1) as i16, word(&state, at + 2) as i16)
}

fn ends(a: i16, b: i16) -> (i32, i32) {
    let (a, b) = (i32::from(a), i32::from(b));
    (a.min(b), a.max(b))
}

fn word(state: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([state.get(2 * i).copied().unwrap_or(0), state.get(2 * i + 1).copied().unwrap_or(0)])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mouse input: the buttons, x and y, and the words after them.
    fn mouse(words: &[u16]) -> Inputs {
        let mut bytes = vec![0; 46];
        for (i, w) in words.iter().enumerate() {
            bytes[2 * i..2 * i + 2].copy_from_slice(&w.to_le_bytes());
        }
        Inputs::new([(MOUSE.to_owned(), bytes)].into())
    }

    #[test]
    fn ranges_keep_the_position_as_the_driver_does() {
        // Arena's choice box (+053AE): x 162 to 471, y 91 to 137, the mouse at (330, 60) moves to y 91; set at the
        // bounds' middle (158, 114), it is kept at x 162.
        let inputs = mouse(&[0, 330, 60]);
        let mut ranges = MouseRanges::MODE_13H;
        ranges.set_x_range(&inputs, 162, 471);
        ranges.set_y_range(&inputs, 137, 91);
        assert_eq!(ranges.y, (91, 137), "a range given either way round");
        assert_eq!(position(&inputs), (330, 91));
        ranges.set_position(&inputs, 158, 114);
        assert_eq!(position(&inputs), (162, 114));
        // A read sees the program's change (the tick's value with the pass's writes).
        assert_eq!(inputs.at_site("GAME.EXE+1000").u16_at(MOUSE, X), 162);
        // Mode 13h reports x even.
        MouseRanges::MODE_13H.set_position(&inputs, 301, 9);
        assert_eq!(position(&inputs), (300, 9));
    }

    #[test]
    fn a_read_of_the_motion_or_a_count_clears_it() {
        // Motion (5, -3); the left button pressed twice, last at (40, 50).
        let inputs = mouse(&[1, 40, 50, 5, (-3i16) as u16, 2, 40, 50]);
        assert_eq!(read_motion(&inputs, "GAME.EXE+2000"), (5, -3));
        assert_eq!(read_motion(&inputs, "GAME.EXE+2000"), (0, 0), "the second read finds it cleared");
        assert_eq!(read_presses(&inputs, "GAME.EXE+2010", 0), (2, 40, 50));
        assert_eq!(read_presses(&inputs, "GAME.EXE+2010", 0), (0, 0, 0));
        assert_eq!(read_releases(&inputs, "GAME.EXE+2020", 0), (0, 0, 0));
        assert_eq!(inputs.written(MOUSE).len(), 4 + 6 + 6, "the cleared words are the program's writes");
    }
}
