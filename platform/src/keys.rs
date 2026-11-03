//! The host's keyboard as a PC's: each key pressed or let go since the last look, as the bytes its keyboard
//! controller sends (scan code set 1; a grey key's after E0h). A key held doesn't repeat.

use minifb::{Key, KeyRepeat, Window};

/// The bytes the keyboard sent for the keys pressed and let go since the window's last update: the presses,
/// then the releases.
pub(crate) fn scancodes(window: &Window) -> Vec<u8> {
    let mut out = Vec::new();
    for key in window.get_keys_pressed(KeyRepeat::No) {
        if let Some((grey, code)) = scancode(key) {
            out.extend(grey.then_some(0xE0));
            out.push(code);
        }
    }
    for key in window.get_keys_released() {
        if let Some((grey, code)) = scancode(key) {
            out.extend(grey.then_some(0xE0));
            out.push(code | 0x80);
        }
    }
    out
}

/// A key's scan code (set 1) and whether it is a grey key, sent after E0h; none for a key a PC's keyboard
/// hasn't.
pub(crate) fn scancode(key: Key) -> Option<(bool, u8)> {
    use Key::*;
    let plain = |code: u8| Some((false, code));
    let grey = |code: u8| Some((true, code));
    match key {
        Escape => plain(0x01),
        Key1 => plain(0x02),
        Key2 => plain(0x03),
        Key3 => plain(0x04),
        Key4 => plain(0x05),
        Key5 => plain(0x06),
        Key6 => plain(0x07),
        Key7 => plain(0x08),
        Key8 => plain(0x09),
        Key9 => plain(0x0A),
        Key0 => plain(0x0B),
        Minus => plain(0x0C),
        Equal => plain(0x0D),
        Backspace => plain(0x0E),
        Tab => plain(0x0F),
        Q => plain(0x10),
        W => plain(0x11),
        E => plain(0x12),
        R => plain(0x13),
        T => plain(0x14),
        Y => plain(0x15),
        U => plain(0x16),
        I => plain(0x17),
        O => plain(0x18),
        P => plain(0x19),
        LeftBracket => plain(0x1A),
        RightBracket => plain(0x1B),
        Enter => plain(0x1C),
        LeftCtrl => plain(0x1D),
        A => plain(0x1E),
        S => plain(0x1F),
        D => plain(0x20),
        F => plain(0x21),
        G => plain(0x22),
        H => plain(0x23),
        J => plain(0x24),
        K => plain(0x25),
        L => plain(0x26),
        Semicolon => plain(0x27),
        Apostrophe => plain(0x28),
        Backquote => plain(0x29),
        LeftShift => plain(0x2A),
        Backslash => plain(0x2B),
        Z => plain(0x2C),
        X => plain(0x2D),
        C => plain(0x2E),
        V => plain(0x2F),
        B => plain(0x30),
        N => plain(0x31),
        M => plain(0x32),
        Comma => plain(0x33),
        Period => plain(0x34),
        Slash => plain(0x35),
        RightShift => plain(0x36),
        NumPadAsterisk => plain(0x37),
        LeftAlt => plain(0x38),
        Space => plain(0x39),
        CapsLock => plain(0x3A),
        F1 => plain(0x3B),
        F2 => plain(0x3C),
        F3 => plain(0x3D),
        F4 => plain(0x3E),
        F5 => plain(0x3F),
        F6 => plain(0x40),
        F7 => plain(0x41),
        F8 => plain(0x42),
        F9 => plain(0x43),
        F10 => plain(0x44),
        NumLock => plain(0x45),
        ScrollLock => plain(0x46),
        NumPad7 => plain(0x47),
        NumPad8 => plain(0x48),
        NumPad9 => plain(0x49),
        NumPadMinus => plain(0x4A),
        NumPad4 => plain(0x4B),
        NumPad5 => plain(0x4C),
        NumPad6 => plain(0x4D),
        NumPadPlus => plain(0x4E),
        NumPad1 => plain(0x4F),
        NumPad2 => plain(0x50),
        NumPad3 => plain(0x51),
        NumPad0 => plain(0x52),
        NumPadDot => plain(0x53),
        F11 => plain(0x57),
        F12 => plain(0x58),
        NumPadEnter => grey(0x1C),
        RightCtrl => grey(0x1D),
        NumPadSlash => grey(0x35),
        RightAlt => grey(0x38),
        Home => grey(0x47),
        Up => grey(0x48),
        PageUp => grey(0x49),
        Left => grey(0x4B),
        Right => grey(0x4D),
        End => grey(0x4F),
        Down => grey(0x50),
        PageDown => grey(0x51),
        Insert => grey(0x52),
        Delete => grey(0x53),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_the_pc_s_scan_codes() {
        assert_eq!(scancode(Key::Escape), Some((false, 0x01)));
        assert_eq!(scancode(Key::Space), Some((false, 0x39)));
        assert_eq!(scancode(Key::Left), Some((true, 0x4B)), "the grey arrows, after E0h");
        assert_eq!(scancode(Key::NumPad4), Some((false, 0x4B)), "the keypad's, without");
        assert_eq!(scancode(Key::LeftSuper), None);
    }
}
