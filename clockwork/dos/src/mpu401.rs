//! Roland MPU-401 MIDI interface at ports 330h/331h, UART mode only.
//!
//! Commands are acknowledged so drivers detect the card; MIDI bytes the program sends are
//! recorded with their time, which is the audio observation channel for MIDI music
//! (PROJECT.md §5.2). Nothing is synthesized.

use std::collections::VecDeque;

const ACK: u8 = 0xFE;
/// Status register: bit 7 set = no data to read, bit 6 set = not ready for output.
const NO_DATA: u8 = 0x80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MidiByte {
    pub pit_tick: u64,
    pub value: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Mpu401 {
    input: VecDeque<u8>,
    uart_mode: bool,
    pub midi_log: Vec<MidiByte>,
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

#[cfg(test)]
mod tests {
    use super::*;

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
