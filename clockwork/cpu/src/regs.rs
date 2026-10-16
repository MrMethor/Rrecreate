use crate::flags;

/// Indices into [`Regs::gpr`], in x86 encoding order.
pub mod reg {
    pub const AX: usize = 0;
    pub const CX: usize = 1;
    pub const DX: usize = 2;
    pub const BX: usize = 3;
    pub const SP: usize = 4;
    pub const BP: usize = 5;
    pub const SI: usize = 6;
    pub const DI: usize = 7;
}

/// Indices into [`Regs::seg`], in x86 encoding order.
pub mod seg {
    pub const ES: usize = 0;
    pub const CS: usize = 1;
    pub const SS: usize = 2;
    pub const DS: usize = 3;
    pub const FS: usize = 4;
    pub const GS: usize = 5;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Regs {
    /// EAX, ECX, EDX, EBX, ESP, EBP, ESI, EDI. 16-bit code uses the low halves.
    pub gpr: [u32; 8],
    /// ES, CS, SS, DS, FS, GS.
    pub seg: [u16; 6],
    /// EIP. 16-bit code segments (and real mode) keep it within 64 KB.
    pub ip: u32,
    /// EFLAGS.
    pub flags: u32,
}

impl Default for Regs {
    fn default() -> Self {
        Self { gpr: [0; 8], seg: [0; 6], ip: 0, flags: u32::from(flags::FIXED) }
    }
}

macro_rules! gp_regs {
    ($($get16:ident $set16:ident $get32:ident $set32:ident $idx:expr;)*) => {
        $(
            pub fn $get16(&self) -> u16 { self.gpr[$idx] as u16 }
            pub fn $set16(&mut self, value: u16) { self.gpr[$idx] = (self.gpr[$idx] & 0xFFFF_0000) | u32::from(value); }
            pub fn $get32(&self) -> u32 { self.gpr[$idx] }
            pub fn $set32(&mut self, value: u32) { self.gpr[$idx] = value; }
        )*
    };
}

macro_rules! byte_regs {
    ($($get:ident $set:ident $code:expr;)*) => {
        $(
            pub fn $get(&self) -> u8 { self.reg8($code) }
            pub fn $set(&mut self, value: u8) { self.set_reg8($code, value) }
        )*
    };
}

macro_rules! seg_regs {
    ($($get:ident $set:ident $idx:expr;)*) => {
        $(
            pub fn $get(&self) -> u16 { self.seg[$idx] }
            pub fn $set(&mut self, value: u16) { self.seg[$idx] = value; }
        )*
    };
}

impl Regs {
    gp_regs! {
        ax set_ax eax set_eax reg::AX; cx set_cx ecx set_ecx reg::CX;
        dx set_dx edx set_edx reg::DX; bx set_bx ebx set_ebx reg::BX;
        sp set_sp esp set_esp reg::SP; bp set_bp ebp set_ebp reg::BP;
        si set_si esi set_esi reg::SI; di set_di edi set_edi reg::DI;
    }

    byte_regs! {
        al set_al 0; cl set_cl 1; dl set_dl 2; bl set_bl 3;
        ah set_ah 4; ch set_ch 5; dh set_dh 6; bh set_bh 7;
    }

    seg_regs! {
        es set_es seg::ES; cs set_cs seg::CS; ss set_ss seg::SS;
        ds set_ds seg::DS; fs set_fs seg::FS; gs set_gs seg::GS;
    }

    /// 8-bit register by encoding: 0–3 = AL, CL, DL, BL; 4–7 = AH, CH, DH, BH.
    pub fn reg8(&self, code: u8) -> u8 {
        let word = self.gpr[usize::from(code & 3)];
        if code & 4 == 0 { word as u8 } else { (word >> 8) as u8 }
    }

    pub fn set_reg8(&mut self, code: u8, value: u8) {
        let word = &mut self.gpr[usize::from(code & 3)];
        *word = if code & 4 == 0 {
            (*word & !0xFF) | u32::from(value)
        } else {
            (*word & !0xFF00) | (u32::from(value) << 8)
        };
    }

    /// The low 16 bits of a general register by index.
    pub fn reg16(&self, index: usize) -> u16 {
        self.gpr[index] as u16
    }

    pub fn set_reg16(&mut self, index: usize, value: u16) {
        self.gpr[index] = (self.gpr[index] & 0xFFFF_0000) | u32::from(value);
    }

    pub fn flag(&self, mask: u16) -> bool {
        self.flags & u32::from(mask) != 0
    }

    pub fn set_flag(&mut self, mask: u16, on: bool) {
        if on {
            self.flags |= u32::from(mask);
        } else {
            self.flags &= !u32::from(mask);
        }
    }

    /// The low 16 bits of EFLAGS.
    pub fn flags16(&self) -> u16 {
        self.flags as u16
    }

    /// The I/O privilege level (EFLAGS bits 12-13).
    pub fn iopl(&self) -> u8 {
        ((self.flags >> 12) & 3) as u8
    }
}
