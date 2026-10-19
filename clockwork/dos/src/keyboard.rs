//! 8042 keyboard controller (ports 60h/64h) and US scancode tables.

use std::collections::VecDeque;

/// Side effects of a controller write that reach beyond the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ControllerEffect {
    SetA20(bool),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub struct Keyboard {
    queue: VecDeque<u8>,
    /// Byte in the output buffer, not yet read from port 60h.
    output: Option<u8>,
    /// Port 60h keeps returning the last byte after it has been read.
    last: u8,
    pending_command: Option<u8>,
    a20: bool,
}

impl Keyboard {
    pub fn push(&mut self, byte: u8) {
        self.queue.push_back(byte);
    }

    pub fn has_queued(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Moves the next queued byte into an empty output buffer. Returns true when that happens;
    /// the caller raises IRQ1.
    pub fn pump(&mut self) -> bool {
        if self.output.is_some() {
            return false;
        }
        match self.queue.pop_front() {
            Some(byte) => {
                self.output = Some(byte);
                true
            }
            None => false,
        }
    }

    pub fn read_data(&mut self) -> u8 {
        if let Some(byte) = self.output.take() {
            self.last = byte;
        }
        self.last
    }

    /// Status register: output buffer full (bit 0), system flag (bit 2), not inhibited (bit 4).
    pub fn read_status(&self) -> u8 {
        0x14 | u8::from(self.output.is_some())
    }

    pub fn write_data(&mut self, value: u8) -> Option<ControllerEffect> {
        if let Some(command) = self.pending_command.take() {
            if command == 0xD1 {
                self.a20 = value & 0x02 != 0;
                return Some(ControllerEffect::SetA20(self.a20));
            }
            return None;
        }
        // A command for the keyboard itself: acknowledge, plus the replies some commands send.
        self.queue.push_back(0xFA);
        match value {
            0xFF => self.queue.push_back(0xAA),
            0xF2 => self.queue.extend([0xAB, 0x83]),
            0xEE => {
                self.queue.pop_back();
                self.queue.push_back(0xEE);
            }
            _ => {}
        }
        None
    }

    pub fn write_command(&mut self, value: u8) -> Option<ControllerEffect> {
        match value {
            0xD1 => self.pending_command = Some(value),
            0xD0 => self.queue.push_back(if self.a20 { 0x03 } else { 0x01 }),
            0xDD => {
                self.a20 = false;
                return Some(ControllerEffect::SetA20(false));
            }
            0xDF => {
                self.a20 = true;
                return Some(ControllerEffect::SetA20(true));
            }
            _ => {}
        }
        None
    }
}

/// ASCII for a make code without and with Shift (US layout); 0 for keys without ASCII.
pub fn scancode_ascii(make: u8) -> (u8, u8) {
    const PLAIN: &[u8; 0x3A] = b"\0\x1b1234567890-=\x08\tqwertyuiop[]\r\0asdfghjkl;'`\0\\zxcvbnm,./\0*\0 ";
    const SHIFT: &[u8; 0x3A] = b"\0\x1b!@#$%^&*()_+\x08\tQWERTYUIOP{}\r\0ASDFGHJKL:\"~\0|ZXCVBNM<>?\0*\0 ";
    match make {
        0x4A => (b'-', b'-'),
        0x4E => (b'+', b'+'),
        m if usize::from(m) < PLAIN.len() => (PLAIN[usize::from(m)], SHIFT[usize::from(m)]),
        _ => (0, 0),
    }
}

/// Make code and whether Shift is needed to type `c` on a US keyboard.
pub fn ascii_to_scancode(c: u8) -> Option<(u8, bool)> {
    (1..0x3A).find_map(|make| {
        let (plain, shifted) = scancode_ascii(make);
        if plain == c && c != 0 {
            Some((make, false))
        } else if shifted == c && c != 0 {
            Some((make, true))
        } else {
            None
        }
    })
}

pub const LEFT_SHIFT: u8 = 0x2A;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scancode_tables() {
        assert_eq!(scancode_ascii(0x1E), (b'a', b'A'));
        assert_eq!(scancode_ascii(0x02), (b'1', b'!'));
        assert_eq!(scancode_ascii(0x1C), (b'\r', b'\r'));
        assert_eq!(scancode_ascii(0x39), (b' ', b' '));
        assert_eq!(scancode_ascii(0x3B), (0, 0), "F1 has no ASCII");
        assert_eq!(ascii_to_scancode(b'Q'), Some((0x10, true)));
        assert_eq!(ascii_to_scancode(b'\r'), Some((0x1C, false)));
        assert_eq!(ascii_to_scancode(b'~'), Some((0x29, true)));
    }

    #[test]
    fn output_buffer_and_commands() {
        let mut kb = Keyboard::default();
        kb.push(0x1E);
        kb.push(0x9E);
        assert!(kb.pump());
        assert!(!kb.pump(), "buffer still full");
        assert_eq!(kb.read_status() & 1, 1);
        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.read_data(), 0x1E, "port 60h repeats the last byte");
        assert!(kb.pump());
        assert_eq!(kb.read_data(), 0x9E);
        assert_eq!(kb.write_command(0xD1), None);
        assert_eq!(kb.write_data(0xDF), Some(ControllerEffect::SetA20(true)));
        kb.write_data(0xED); // set LEDs → ACK
        assert!(kb.pump());
        assert_eq!(kb.read_data(), 0xFA);
    }
}
