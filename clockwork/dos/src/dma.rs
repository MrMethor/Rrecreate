//! Intel 8237 DMA controllers: channels 0-3 (8-bit, ports 00h-0Fh) and 4-7 (16-bit, ports
//! C0h-DFh), with the page registers at 81h-8Fh.
//!
//! Ported from DOSBox Staging `src/hardware/dma.cpp` (GPL-2.0-or-later). Devices pull data
//! through [`Channel::read`] / [`Channel::write`]; terminal count, auto-initialization and the
//! status register work as there. Unlike DOSBox, the address wraps inside its 64 KB (or 128 KB)
//! page during a transfer, as the 8237's 16-bit counter does. Memory-to-memory transfers and
//! address decrement are not emulated (DOSBox doesn't either).

use machine::Memory;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Channel {
    pub number: u8,
    pub page: u8,
    pub base_address: u16,
    pub current_address: u16,
    pub base_count: u16,
    pub current_count: u16,
    pub autoinit: bool,
    pub masked: bool,
    pub terminal_count: bool,
    /// A device asked for service (status register bits 4-7).
    pub request: bool,
}

impl Channel {
    fn new(number: u8) -> Self {
        Self {
            number,
            page: 0,
            base_address: 0,
            current_address: 0,
            base_count: 0,
            current_count: 0,
            autoinit: false,
            masked: true,
            terminal_count: false,
            request: false,
        }
    }

    pub fn is_16bit(&self) -> bool {
        self.number >= 4
    }

    /// Physical address of the current transfer unit (byte or word).
    fn physical(&self) -> u32 {
        if self.is_16bit() {
            (u32::from(self.page >> 1) << 17) | (u32::from(self.current_address) << 1)
        } else {
            (u32::from(self.page) << 16) | u32::from(self.current_address)
        }
    }

    /// Advances past one unit; at terminal count reloads (auto-init) or masks the channel.
    /// Returns false when the transfer ended (channel now masked).
    fn advance(&mut self) -> bool {
        self.current_address = self.current_address.wrapping_add(1);
        let (count, ended) = self.current_count.overflowing_sub(1);
        self.current_count = count;
        if !ended {
            return true;
        }
        self.terminal_count = true;
        if self.autoinit {
            self.current_address = self.base_address;
            self.current_count = self.base_count;
            true
        } else {
            self.masked = true;
            false
        }
    }

    /// Reads up to `units` transfer units (bytes, or words on channels 4-7) from memory into
    /// `out`, stopping at the end of a single-cycle transfer. Returns the units transferred.
    pub fn read(&mut self, mem: &Memory, units: usize, out: &mut Vec<u8>) -> usize {
        let unit_bytes = if self.is_16bit() { 2 } else { 1 };
        let mut done = 0;
        while done < units && !self.masked {
            let at = self.physical();
            for i in 0..unit_bytes {
                out.push(mem.read_u8(at + i));
            }
            done += 1;
            if !self.advance() {
                break;
            }
        }
        done
    }

    /// Writes `data` to memory, one unit at a time, stopping at the end of a single-cycle
    /// transfer. Returns the units transferred.
    pub fn write(&mut self, mem: &mut Memory, data: &[u8]) -> usize {
        let unit_bytes = if self.is_16bit() { 2 } else { 1 };
        let mut done = 0;
        for unit in data.chunks(unit_bytes) {
            if self.masked {
                break;
            }
            let at = self.physical();
            for (i, &byte) in unit.iter().enumerate() {
                mem.write_u8(at + i as u32, byte);
            }
            done += 1;
            if !self.advance() {
                break;
            }
        }
        done
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Controller {
    flip_flop: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Dma {
    pub channels: [Channel; 8],
    controllers: [Controller; 2],
}

impl Default for Dma {
    fn default() -> Self {
        Self::new()
    }
}

/// The channel whose page register is at `port`.
fn page_channel(port: u16) -> Option<usize> {
    Some(match port {
        0x87 => 0,
        0x83 => 1,
        0x81 => 2,
        0x82 => 3,
        0x8F => 4,
        0x8B => 5,
        0x89 => 6,
        0x8A => 7,
        _ => return None,
    })
}

impl Dma {
    pub fn new() -> Self {
        Self {
            channels: std::array::from_fn(|n| Channel::new(n as u8)),
            controllers: [Controller { flip_flop: false }, Controller { flip_flop: false }],
        }
    }

    /// Whether `port` belongs to the DMA controllers or page registers.
    pub fn decodes(port: u16) -> bool {
        port < 0x10 || (0xC0..=0xDF).contains(&port) || page_channel(port).is_some()
    }

    /// Controller index and register (0-15) for a controller port.
    fn register(port: u16) -> Option<(usize, u16)> {
        match port {
            0x00..=0x0F => Some((0, port)),
            0xC0..=0xDF => Some((1, (port - 0xC0) >> 1)),
            _ => None,
        }
    }

    pub fn read(&mut self, port: u16) -> u8 {
        if let Some(n) = page_channel(port) {
            return self.channels[n].page;
        }
        let Some((controller, register)) = Self::register(port) else { return 0xFF };
        let first = controller * 4;
        let flip_flop = &mut self.controllers[controller].flip_flop;
        match register {
            0..=7 => {
                let channel = &self.channels[first + usize::from(register >> 1)];
                let value = if register & 1 == 0 { channel.current_address } else { channel.current_count };
                *flip_flop = !*flip_flop;
                if *flip_flop { value as u8 } else { (value >> 8) as u8 }
            }
            8 => {
                // Status: terminal count reached (cleared by reading) and pending requests.
                let mut status = 0;
                for (i, channel) in self.channels[first..first + 4].iter_mut().enumerate() {
                    if channel.terminal_count {
                        status |= 1 << i;
                    }
                    channel.terminal_count = false;
                    if channel.request {
                        status |= 1 << (4 + i);
                    }
                }
                status
            }
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, port: u16, value: u8) {
        if let Some(n) = page_channel(port) {
            self.channels[n].page = value;
            return;
        }
        let Some((controller, register)) = Self::register(port) else { return };
        let first = controller * 4;
        match register {
            0..=7 => {
                let flip_flop = &mut self.controllers[controller].flip_flop;
                *flip_flop = !*flip_flop;
                let low = *flip_flop;
                let channel = &mut self.channels[first + usize::from(register >> 1)];
                let set = |word: &mut u16| {
                    *word = if low {
                        (*word & 0xFF00) | u16::from(value)
                    } else {
                        (*word & 0x00FF) | (u16::from(value) << 8)
                    }
                };
                if register & 1 == 0 {
                    set(&mut channel.base_address);
                    set(&mut channel.current_address);
                } else {
                    set(&mut channel.base_count);
                    set(&mut channel.current_count);
                }
            }
            0x0A => self.channels[first + usize::from(value & 3)].masked = value & 0x04 != 0,
            0x0B => {
                let channel = &mut self.channels[first + usize::from(value & 3)];
                channel.autoinit = value & 0x10 != 0;
            }
            0x0C => self.controllers[controller].flip_flop = false,
            0x0D => {
                for channel in &mut self.channels[first..first + 4] {
                    channel.masked = true;
                    channel.terminal_count = false;
                }
                self.controllers[controller].flip_flop = false;
            }
            0x0E => self.channels[first..first + 4].iter_mut().for_each(|c| c.masked = false),
            0x0F => {
                for (i, channel) in self.channels[first..first + 4].iter_mut().enumerate() {
                    channel.masked = value & (1 << i) != 0;
                }
            }
            _ => {} // command and request registers: not used by PC software
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Programs channel 1 for `count + 1` bytes at 2000h:0010h, as a Sound Blaster driver does.
    fn program_channel_1(dma: &mut Dma, count: u16, autoinit: bool) {
        dma.write(0x0A, 0x05); // mask channel 1
        dma.write(0x0C, 0); // clear flip-flop
        dma.write(0x0B, if autoinit { 0x59 } else { 0x49 }); // single, read from memory
        dma.write(0x02, 0x10);
        dma.write(0x02, 0x00);
        dma.write(0x83, 0x02);
        dma.write(0x03, count as u8);
        dma.write(0x03, (count >> 8) as u8);
        dma.write(0x0A, 0x01); // unmask
    }

    #[test]
    fn single_cycle_read_stops_at_terminal_count() {
        let mut mem = Memory::new();
        for i in 0..8u32 {
            mem.write_u8(0x2_0010 + i, i as u8 + 1);
        }
        let mut dma = Dma::new();
        program_channel_1(&mut dma, 3, false);
        let mut out = Vec::new();
        assert_eq!(dma.channels[1].read(&mem, 10, &mut out), 4);
        assert_eq!(out, [1, 2, 3, 4]);
        assert!(dma.channels[1].masked);
        assert_eq!(dma.read(0x08) & 0x02, 0x02, "terminal count in the status register");
        assert_eq!(dma.read(0x08) & 0x02, 0, "cleared by reading");
        // The count reads back as FFFFh, low byte first.
        dma.write(0x0C, 0);
        assert_eq!((dma.read(0x03), dma.read(0x03)), (0xFF, 0xFF));
    }

    #[test]
    fn auto_init_wraps_to_the_start() {
        let mut mem = Memory::new();
        for i in 0..4u32 {
            mem.write_u8(0x2_0010 + i, i as u8 + 1);
        }
        let mut dma = Dma::new();
        program_channel_1(&mut dma, 1, true);
        let mut out = Vec::new();
        assert_eq!(dma.channels[1].read(&mem, 5, &mut out), 5);
        assert_eq!(out, [1, 2, 1, 2, 1]);
        assert!(!dma.channels[1].masked);
    }

    #[test]
    fn sixteen_bit_channels_address_words() {
        let mut mem = Memory::new();
        mem.write_u16(0x2_0020, 0xBEEF);
        let mut dma = Dma::new();
        dma.write(0xD8, 0); // clear flip-flop (controller 2)
        dma.write(0xC4, 0x10); // channel 5 address 0010h words = byte 20h
        dma.write(0xC4, 0x00);
        dma.write(0x8B, 0x02); // page 2 (bit 0 ignored)
        dma.write(0xC6, 0x00);
        dma.write(0xC6, 0x00);
        dma.write(0xD4, 0x01); // unmask channel 5
        let mut out = Vec::new();
        assert_eq!(dma.channels[5].read(&mem, 1, &mut out), 1);
        assert_eq!(out, [0xEF, 0xBE]);
    }
}
