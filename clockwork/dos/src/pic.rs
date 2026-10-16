//! Intel 8259A programmable interrupt controller, edge-triggered, fully nested mode.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Init {
    Ready,
    Icw2,
    Icw3,
    Icw4,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Pic {
    irr: u8,
    isr: u8,
    imr: u8,
    vector_base: u8,
    init: Init,
    single: bool,
    needs_icw4: bool,
    read_isr: bool,
}

impl Pic {
    pub fn new(vector_base: u8, imr: u8) -> Self {
        Self { irr: 0, isr: 0, imr, vector_base, init: Init::Ready, single: false, needs_icw4: false, read_isr: false }
    }

    /// An edge on input `line` (0–7).
    pub fn raise(&mut self, line: u8) {
        self.irr |= 1 << line;
    }

    /// Withdraws a request on `line` that hasn't been delivered yet (the device deasserted it).
    pub fn lower(&mut self, line: u8) {
        self.irr &= !(1 << line);
    }

    /// The line that would be delivered now: the highest-priority unmasked request that isn't
    /// blocked by an equal or higher-priority interrupt in service.
    pub fn pending_line(&self) -> Option<u8> {
        let requests = self.irr & !self.imr;
        for line in 0..8 {
            let bit = 1 << line;
            if self.isr & bit != 0 {
                return None;
            }
            if requests & bit != 0 {
                return Some(line);
            }
        }
        None
    }

    /// Delivers the pending interrupt, if any, and returns its vector.
    pub fn acknowledge(&mut self) -> Option<u8> {
        let line = self.pending_line()?;
        self.irr &= !(1 << line);
        self.isr |= 1 << line;
        Some(self.vector_base.wrapping_add(line))
    }

    pub fn vector_base(&self) -> u8 {
        self.vector_base
    }

    pub fn imr(&self) -> u8 {
        self.imr
    }

    /// A write to the command port (A0 = 0) or data port (A0 = 1).
    pub fn write(&mut self, data_port: bool, value: u8) {
        if !data_port {
            if value & 0x10 != 0 {
                // ICW1
                self.init = Init::Icw2;
                self.single = value & 0x02 != 0;
                self.needs_icw4 = value & 0x01 != 0;
                self.imr = 0;
                self.isr = 0;
                self.irr = 0;
                self.read_isr = false;
            } else if value & 0x08 == 0 {
                // OCW2: only end-of-interrupt commands matter here.
                if value & 0x20 != 0 {
                    if value & 0x40 != 0 {
                        self.isr &= !(1 << (value & 7));
                    } else if self.isr != 0 {
                        self.isr &= self.isr - 1; // clear the highest-priority (lowest) bit
                    }
                }
            } else if value & 0x02 != 0 {
                // OCW3: choose which register a command-port read returns.
                self.read_isr = value & 0x01 != 0;
            }
            return;
        }
        self.init = match self.init {
            Init::Ready => {
                self.imr = value;
                Init::Ready
            }
            Init::Icw2 => {
                self.vector_base = value & 0xF8;
                if !self.single {
                    Init::Icw3
                } else if self.needs_icw4 {
                    Init::Icw4
                } else {
                    Init::Ready
                }
            }
            Init::Icw3 => {
                if self.needs_icw4 {
                    Init::Icw4
                } else {
                    Init::Ready
                }
            }
            Init::Icw4 => Init::Ready,
        };
    }

    pub fn read(&self, data_port: bool) -> u8 {
        if data_port {
            self.imr
        } else if self.read_isr {
            self.isr
        } else {
            self.irr
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_masking_and_eoi() {
        let mut pic = Pic::new(0x08, 0x00);
        pic.raise(1);
        pic.raise(0);
        assert_eq!(pic.acknowledge(), Some(0x08));
        assert_eq!(pic.acknowledge(), None, "IRQ1 waits for IRQ0's EOI");
        pic.write(false, 0x20);
        assert_eq!(pic.acknowledge(), Some(0x09));
        pic.write(false, 0x20);
        pic.write(true, 0x01); // mask IRQ0
        pic.raise(0);
        assert_eq!(pic.acknowledge(), None);
        assert_eq!(pic.read(false), 0x01, "IRR still shows the masked request");
    }

    #[test]
    fn reinitialization_moves_the_vector_base() {
        let mut pic = Pic::new(0x08, 0xB8);
        for (port, value) in [(false, 0x11), (true, 0x20), (true, 0x04), (true, 0x01), (true, 0xFE)] {
            pic.write(port, value);
        }
        assert_eq!((pic.vector_base(), pic.imr()), (0x20, 0xFE));
        pic.raise(0);
        assert_eq!(pic.acknowledge(), Some(0x20));
    }
}
