//! Roland MPU-401 MIDI interface at ports 330h/331h, UART mode only.
//!
//! Commands are acknowledged so drivers detect the card; MIDI bytes the program sends are
//! recorded with their time, which is how MIDI music is observed. Nothing is synthesized.
//!
//! [`MidiParser`], which splits the recorded bytes into whole messages for a MIDI device, is
//! ported from DOSBox Staging `src/midi/midi.cpp` (`MIDI_RawOutByte`, GPL-2.0-or-later).

use std::collections::VecDeque;

const ACK: u8 = 0xFE;
/// Status register: bit 7 set = no data to read, bit 6 set = not ready for output.
const NO_DATA: u8 = 0x80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct MidiByte {
    pub pit_tick: u64,
    pub value: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub struct Mpu401 {
    input: VecDeque<u8>,
    uart_mode: bool,
    pub midi_log: crate::record::Record<Vec<MidiByte>>,
}

impl Mpu401 {
    pub fn read_data(&mut self) -> u8 {
        self.input.pop_front().unwrap_or(ACK)
    }

    pub fn read_status(&self) -> u8 {
        0x3F | if self.input.is_empty() { NO_DATA } else { 0 }
    }

    pub fn write_data(&mut self, value: u8, pit_tick: u64) {
        self.midi_log.push(MidiByte { pit_tick, value });
    }

    pub fn write_command(&mut self, command: u8) {
        match command {
            0xFF => {
                // Reset: leaves UART mode; acknowledged.
                self.uart_mode = false;
                self.input.clear();
                self.input.push_back(ACK);
            }
            0x3F => {
                self.uart_mode = true;
                self.input.push_back(ACK);
            }
            // Intelligent-mode commands aren't emulated; outside UART mode they are still
            // acknowledged so detection succeeds, in UART mode they are ignored like on the card.
            _ if !self.uart_mode => self.input.push_back(ACK),
            _ => {}
        }
    }

    pub fn uart_mode(&self) -> bool {
        self.uart_mode
    }
}

/// Message lengths by status byte, the status byte included (0 for data bytes and for SysEx,
/// which ends at the next status byte).
const MESSAGE_LENGTH: [u8; 16] = [
    3, 3, 3, 3, 3, 3, 3, 3, // data bytes: unused
    3, 3, 3, 3, 2, 2, 3, 0, // 8x-Ex channel messages; Fx below
];
const SYSTEM_LENGTH: [u8; 16] = [0, 2, 3, 2, 0, 0, 1, 0, 1, 0, 1, 1, 1, 0, 1, 0];
/// Longest SysEx message kept (as in DOSBox); longer ones are cut short.
const MAX_SYSEX: usize = 20 * 1024;

/// Splits a stream of MIDI bytes, as programs send them to the MPU-401, into whole messages:
/// running status is expanded (each message carries its status byte), SysEx messages are
/// collected up to their end, and real-time bytes pass through on their own.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MidiParser {
    status: u8,
    message: [u8; 3],
    position: usize,
    length: usize,
    sysex: Vec<u8>,
}

impl MidiParser {
    /// Takes the next byte and passes each message it completes to `emit`.
    pub fn push(&mut self, byte: u8, mut emit: impl FnMut(&[u8])) {
        if byte >= 0xF8 {
            emit(&[byte]);
            return;
        }
        if self.status == 0xF0 {
            if byte < 0x80 {
                if self.sysex.len() < MAX_SYSEX - 1 {
                    self.sysex.push(byte);
                }
                return;
            }
            // Any status byte ends the SysEx message (F7 normally).
            self.sysex.push(0xF7);
            emit(&self.sysex);
        }
        if byte >= 0x80 {
            self.status = byte;
            self.position = 0;
            self.length = usize::from(if byte >= 0xF0 {
                SYSTEM_LENGTH[usize::from(byte & 0x0F)]
            } else {
                MESSAGE_LENGTH[usize::from(byte >> 4)]
            });
            if byte == 0xF0 {
                self.sysex.clear();
                self.sysex.push(0xF0);
            }
        }
        if self.length > 0 {
            self.message[self.position] = byte;
            self.position += 1;
            if self.position >= self.length {
                emit(&self.message[..self.length]);
                self.position = 1; // running status
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut parser = MidiParser::default();
        let mut out = Vec::new();
        for &b in bytes {
            parser.push(b, |m| out.push(m.to_vec()));
        }
        out
    }

    #[test]
    fn running_status_and_short_messages() {
        let out = messages(&[0x45, 0x90, 60, 100, 64, 90, 0xC1, 5, 6, 0xE0, 0, 0x40]);
        assert_eq!(out, [vec![0x90, 60, 100], vec![0x90, 64, 90], vec![0xC1, 5], vec![0xC1, 6], vec![0xE0, 0, 0x40]]);
    }

    #[test]
    fn sysex_and_real_time_bytes() {
        let out = messages(&[0xF0, 0x41, 0x10, 0xF8, 0x42, 0xF7, 0x80, 60, 0, 0xF0, 1, 0xB0, 7, 100]);
        assert_eq!(
            out,
            [
                vec![0xF8],
                vec![0xF0, 0x41, 0x10, 0x42, 0xF7],
                vec![0x80, 60, 0],
                vec![0xF0, 1, 0xF7], // ended by the next status byte
                vec![0xB0, 7, 100],
            ]
        );
    }

    #[test]
    fn reset_and_uart_handshake() {
        let mut mpu = Mpu401::default();
        assert_eq!(mpu.read_status() & NO_DATA, NO_DATA);
        mpu.write_command(0xFF);
        assert_eq!(mpu.read_status() & NO_DATA, 0, "ACK waiting");
        assert_eq!(mpu.read_data(), ACK);
        mpu.write_command(0x3F);
        assert_eq!(mpu.read_data(), ACK);
        assert!(mpu.uart_mode());
        assert_eq!(mpu.read_status() & 0x40, 0, "always ready for output");
        mpu.write_data(0x90, 10);
        mpu.write_data(60, 10);
        mpu.write_data(100, 11);
        assert_eq!(mpu.midi_log.iter().map(|b| b.value).collect::<Vec<_>>(), [0x90, 60, 100]);
    }
}
