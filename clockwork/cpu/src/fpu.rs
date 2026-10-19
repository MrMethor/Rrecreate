//! Ported from DOSBox Staging `src/fpu/fpu.cpp` and `src/fpu/fpu_instructions.h`
//! (GPL-2.0-or-later).
//!
//! The x87 floating-point unit: an 80387 (also paired with the 80286 model). Like DOSBox's
//! portable FPU, registers hold 64-bit doubles: arithmetic rounds to nearest in double precision,
//! and the precision and rounding controls only affect integer conversions and FRNDINT. Loads and
//! stores of the 80-bit format round to nearest even (DOSBox's `FPU_FLD80`/`FPU_ST80`), and a
//! 64-bit integer that FILD loads is remembered so FISTP stores it back exactly, for programs
//! that copy memory through the FPU (DOSBox's `regs_memcpy`).
//!
//! Differences from DOSBox, all toward the real chip or toward determinism:
//! - results are the same on every host: sin, cos, tan, atan2, logarithms and exponentials come
//!   from the pure-Rust `libm` crate instead of the platform's C library, and NaN results are
//!   chosen here (the NaN operand quieted, or the x87 "indefinite") rather than by the host CPU;
//! - invalid operations (0/0, ∞−∞, √−1, ...), stack overflow and underflow, and integer stores
//!   that don't fit give the 387's masked responses: the indefinite value with IE (and SF);
//! - tags come from register contents, as on the 387; FSTENV and FSAVE store the last
//!   instruction and operand pointers in the real- or protected-mode layout of the operand size;
//!   FSIN, FCOS, FPTAN and FSINCOS leave operands of 2^63 or more alone and set C2;
//! - CR0.EM or CR0.TS make escape opcodes raise #NM (and CR0.MP with TS makes WAIT raise it), so
//!   software FPU emulators and lazy FPU switching work;
//! - encodings the 387 doesn't have (FCMOVcc, FCOMI, FISTTP, ...) stop the run as unimplemented
//!   instructions instead of being skipped.
//!
//! As in DOSBox, unmasked exceptions only set their status flags (and ES); nothing is signalled
//! through IRQ 13.

use crate::exec::{Prefixes, Rm, Width};
use crate::seg::CS;
use crate::{Bus, Cpu, Fault, Res, cr0, vector};
use std::f64::consts::{LN_2, LOG2_10, LOG2_E, LOG10_2, PI};
use std::hash::{Hash, Hasher};

/// Status word bits.
pub mod status {
    /// Invalid operation.
    pub const IE: u16 = 0x0001;
    pub const DE: u16 = 0x0002;
    /// Division by zero.
    pub const ZE: u16 = 0x0004;
    pub const OE: u16 = 0x0008;
    pub const UE: u16 = 0x0010;
    /// Precision (inexact result).
    pub const PE: u16 = 0x0020;
    /// Stack fault (with IE).
    pub const SF: u16 = 0x0040;
    /// Error summary: an unmasked exception flag is set.
    pub const ES: u16 = 0x0080;
    pub const C0: u16 = 0x0100;
    pub const C1: u16 = 0x0200;
    pub const C2: u16 = 0x0400;
    /// Index of the register that is ST(0).
    pub const TOP: u16 = 0x3800;
    pub const C3: u16 = 0x4000;
    pub const BUSY: u16 = 0x8000;
}

use status::{BUSY, C0, C1, C2, C3, ES, IE, PE, SF, TOP, ZE};

/// The exception flags IE..PE, which the control word masks bit for bit.
const EXCEPTIONS: u16 = 0x003F;

/// The control word after FNINIT: every exception masked, extended precision, round to nearest.
const INIT_CONTROL: u16 = 0x037F;

/// The "real indefinite" QNaN that masked invalid operations produce.
const INDEFINITE: f64 = f64::from_bits(0xFFF8_0000_0000_0000);

/// The quiet bit of a double NaN.
const QUIET: u64 = 1 << 51;

const MANTISSA: u64 = (1 << 52) - 1;

/// FSIN, FCOS, FPTAN and FSINCOS accept operands below 2^63.
const TRIG_LIMIT: f64 = 9_223_372_036_854_775_808.0;

/// FLD1, FLDL2T, FLDL2E, FLDPI, FLDLG2, FLDLN2, FLDZ.
const CONSTANTS: [f64; 7] = [1.0, LOG2_10, LOG2_E, PI, LOG10_2, LN_2, 0.0];

/// Where the last non-control instruction and its memory operand were, for FSTENV and FSAVE.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
struct Pointers {
    cs: u16,
    ip: u32,
    /// The low 11 bits of the opcode: the escape's low 3 bits and the ModRM byte.
    opcode: u16,
    ds: u16,
    dp: u32,
}

/// The FPU's state. Plain data, so cloning the CPU snapshots it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Fpu {
    /// Physical registers R0-R7; ST(i) is R[(TOP + i) mod 8].
    regs: [f64; 8],
    /// The integer a 64-bit FILD loaded into a register, while it holds that value.
    exact: [Option<i64>; 8],
    /// Bit i: R[i] is empty.
    empty: u8,
    cw: u16,
    /// The status word without TOP.
    sw: u16,
    top: u8,
    last: Pointers,
}

impl PartialEq for Fpu {
    fn eq(&self, other: &Self) -> bool {
        self.regs.map(f64::to_bits) == other.regs.map(f64::to_bits)
            && self.exact == other.exact
            && self.empty == other.empty
            && self.cw == other.cw
            && self.sw == other.sw
            && self.top == other.top
            && self.last == other.last
    }
}

impl Eq for Fpu {}

impl Hash for Fpu {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.regs.map(f64::to_bits).hash(state);
        self.exact.hash(state);
        self.empty.hash(state);
        self.cw.hash(state);
        self.sw.hash(state);
        self.top.hash(state);
        self.last.hash(state);
    }
}

impl Default for Fpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Fpu {
    /// The state after FNINIT.
    pub fn new() -> Self {
        Self {
            regs: [0.0; 8],
            exact: [None; 8],
            empty: 0xFF,
            cw: INIT_CONTROL,
            sw: 0,
            top: 0,
            last: Pointers::default(),
        }
    }

    pub fn control_word(&self) -> u16 {
        self.cw
    }

    pub fn status_word(&self) -> u16 {
        (self.sw & !TOP) | (u16::from(self.top) << 11)
    }

    /// Two bits per physical register: 0 valid, 1 zero, 2 special (NaN or infinity), 3 empty.
    pub fn tag_word(&self) -> u16 {
        (0..8).fold(0, |tags, r| {
            let value = self.regs[r];
            let tag = if self.empty & (1 << r) != 0 {
                3
            } else if value == 0.0 {
                1
            } else if value.is_finite() {
                0
            } else {
                2
            };
            tags | (tag << (2 * r))
        })
    }

    /// ST(i), or `None` if it is empty.
    pub fn st(&self, i: u8) -> Option<f64> {
        let r = self.phys(i);
        (self.empty & (1 << r) == 0).then_some(self.regs[r])
    }

    // ---- stack ----

    fn phys(&self, i: u8) -> usize {
        usize::from(self.top.wrapping_add(i) & 7)
    }

    fn is_empty(&self, i: u8) -> bool {
        self.empty & (1 << self.phys(i)) != 0
    }

    /// FNINIT. The registers keep their contents but are all empty.
    fn init(&mut self) {
        self.cw = INIT_CONTROL;
        self.sw = 0;
        self.top = 0;
        self.empty = 0xFF;
        self.last = Pointers::default();
    }

    fn set_status_word(&mut self, word: u16) {
        self.sw = word & !TOP;
        self.top = ((word & TOP) >> 11) as u8;
    }

    fn set_tag_word(&mut self, tags: u16) {
        self.empty = (0..8).filter(|r| (tags >> (2 * r)) & 3 == 3).fold(0, |empty, r| empty | (1 << r));
    }

    /// ST(i) as an operand. An empty register is a stack underflow, whose masked response is
    /// the indefinite value.
    fn read(&mut self, i: u8) -> f64 {
        if self.is_empty(i) {
            self.sw = (self.sw & !C1) | IE | SF;
            INDEFINITE
        } else {
            self.regs[self.phys(i)]
        }
    }

    /// The integer FILD put in ST(i), if it is still there.
    fn exact(&self, i: u8) -> Option<i64> {
        if self.is_empty(i) { None } else { self.exact[self.phys(i)] }
    }

    /// Stores into ST(i), which becomes valid.
    fn write(&mut self, i: u8, value: f64) {
        self.write_exact(i, value, None);
    }

    fn write_exact(&mut self, i: u8, value: f64, exact: Option<i64>) {
        let r = self.phys(i);
        self.regs[r] = value;
        self.exact[r] = exact;
        self.empty &= !(1 << r);
    }

    fn push(&mut self, value: f64) {
        self.push_exact(value, None);
    }

    /// Pushing onto a full register is a stack overflow: the indefinite value goes in instead.
    fn push_exact(&mut self, value: f64, exact: Option<i64>) {
        self.top = self.top.wrapping_sub(1) & 7;
        if self.is_empty(0) {
            self.write_exact(0, value, exact);
        } else {
            self.sw |= IE | SF | C1;
            self.write(0, INDEFINITE);
        }
    }

    fn pop(&mut self) {
        self.empty |= 1 << self.top;
        self.top = (self.top + 1) & 7;
    }

    fn set_flag(&mut self, flag: u16, on: bool) {
        if on {
            self.sw |= flag;
        } else {
            self.sw &= !flag;
        }
    }

    /// After a non-control instruction: ES and B report unmasked exception flags.
    fn summarize(&mut self) {
        if self.sw & !self.cw & EXCEPTIONS != 0 {
            self.sw |= ES | BUSY;
        }
    }

    // ---- results ----

    /// The result of an operation on `a` and `b`. A NaN operand propagates (quieted; of two, the
    /// larger significand), and an invalid operation gives the indefinite with IE, so the host's
    /// own NaN bits never reach the machine state.
    fn result(&mut self, r: f64, a: f64, b: f64) -> f64 {
        if !r.is_nan() {
            return r;
        }
        if is_signaling(a) || is_signaling(b) {
            self.sw |= IE;
        }
        match (a.is_nan(), b.is_nan()) {
            (true, true) => quiet(if a.to_bits() & MANTISSA >= b.to_bits() & MANTISSA { a } else { b }),
            (true, false) => quiet(a),
            (false, true) => quiet(b),
            (false, false) => {
                self.sw |= IE;
                INDEFINITE
            }
        }
    }

    /// A value loaded from a 32- or 64-bit real: a signaling NaN is quieted, with IE.
    fn load(&mut self, value: f64) -> f64 {
        if is_signaling(value) {
            self.sw |= IE;
            quiet(value)
        } else {
            value
        }
    }

    /// `dest op src` for the arithmetic group: 0 FADD, 1 FMUL, 4 FSUB, 5 FSUBR, 6 FDIV, 7 FDIVR.
    fn arith(&mut self, op: u8, dest: f64, src: f64) -> f64 {
        let (a, b) = if op == 5 || op == 7 { (src, dest) } else { (dest, src) };
        let r = match op {
            0 => a + b,
            1 => a * b,
            4 | 5 => a - b,
            _ => {
                if b == 0.0 && a.is_finite() && a != 0.0 {
                    self.sw |= ZE;
                }
                a / b
            }
        };
        self.result(r, a, b)
    }

    /// FCOM and FUCOM: C3 C2 C0 = 000 (greater), 001 (less), 100 (equal) or 111 (unordered).
    /// FCOM treats any NaN as invalid, FUCOM only signaling ones; with IE unmasked, an invalid
    /// comparison leaves the condition codes alone.
    fn compare(&mut self, a: f64, b: f64, unordered: bool) {
        self.sw &= !C1;
        if a.is_nan() || b.is_nan() {
            if !unordered || is_signaling(a) || is_signaling(b) {
                self.sw |= IE;
                if self.cw & IE == 0 {
                    return;
                }
            }
            self.sw |= C3 | C2 | C0;
            return;
        }
        self.sw &= !(C3 | C2 | C0);
        self.set_flag(C3, a == b);
        self.set_flag(C0, a < b);
    }

    /// The arithmetic group with ST(0) as the destination (D8 and the memory forms).
    fn arith_st0(&mut self, op: u8, src: f64) {
        let st0 = self.read(0);
        match op {
            2 => self.compare(st0, src, false),
            3 => {
                self.compare(st0, src, false);
                self.pop();
            }
            _ => {
                let r = self.arith(op, st0, src);
                self.write(0, r);
            }
        }
    }

    /// Rounds by the control word's rounding field.
    fn round(&self, value: f64) -> f64 {
        match (self.cw >> 10) & 3 {
            0 => value.round_ties_even(),
            1 => value.floor(),
            2 => value.ceil(),
            _ => value.trunc(),
        }
    }

    /// ST(0) as a `bits`-bit integer for FIST/FISTP: the integer indefinite (only the sign bit
    /// set) with IE if it doesn't fit.
    fn integer(&mut self, bits: u32) -> u64 {
        let value = self.read(0);
        if bits == 64
            && let Some(exact) = self.exact(0)
            && exact as f64 == value
        {
            return exact as u64;
        }
        let rounded = self.round(value);
        let limit = (1u64 << (bits - 1)) as f64;
        if rounded >= -limit && rounded < limit {
            if rounded != value {
                self.sw |= PE;
            }
            rounded as i64 as u64
        } else {
            self.sw |= IE;
            1 << (bits - 1)
        }
    }

    /// ST(0) as 18-digit packed BCD for FBSTP: the BCD indefinite with IE if it doesn't fit.
    fn bcd(&mut self) -> [u8; 10] {
        let value = self.read(0);
        let magnitude = self.round(value).abs();
        if magnitude.is_nan() || magnitude >= 1e18 {
            self.sw |= IE;
            return [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF];
        }
        let mut digits = magnitude as u64;
        let mut bytes = [0; 10];
        for byte in &mut bytes[..9] {
            let low = (digits % 10) as u8;
            digits /= 10;
            *byte = low | (((digits % 10) as u8) << 4);
            digits /= 10;
        }
        if value.is_sign_negative() {
            bytes[9] = 0x80;
        }
        bytes
    }

    // ---- register forms ----

    /// A register-form escape (ModRM mod = 3) other than FNSTSW AX: `Some(true)` for a control
    /// instruction, `Some(false)` for others, `None` for an encoding the 387 doesn't have.
    fn exec_reg(&mut self, esc: u8, group: u8, i: u8) -> Option<bool> {
        match (esc, group) {
            // FCOM, FCOMP, and their DC and DE aliases.
            (0 | 4, 2 | 3) | (6, 2) => {
                let (a, b) = (self.read(0), self.read(i));
                self.compare(a, b, false);
                if group == 3 || esc == 6 {
                    self.pop();
                }
            }
            (0, op) => {
                let src = self.read(i);
                self.arith_st0(op, src);
            }
            (6, 3) => {
                // FCOMPP is DE D9; the rest of the row is unused.
                if i != 1 {
                    return None;
                }
                let (a, b) = (self.read(0), self.read(1));
                self.compare(a, b, false);
                self.pop();
                self.pop();
            }
            // ST(i) = ST(i) op ST(0), popping for DE. Here 4 is FSUBR and 6 is FDIVR.
            (4 | 6, op) => {
                let (dest, src) = (self.read(i), self.read(0));
                let r = self.arith(if op >= 4 { op ^ 1 } else { op }, dest, src);
                self.write(i, r);
                if esc == 6 {
                    self.pop();
                }
            }
            (1, 0) => {
                let exact = self.exact(i);
                let value = self.read(i);
                self.push_exact(value, exact);
            }
            (1 | 5 | 7, 1) => self.exchange(i),
            (1, 2) if i == 0 => {} // FNOP
            (5, 2) => self.copy_to(i),
            (1 | 5, 3) | (7, 2 | 3) => {
                self.copy_to(i);
                self.pop();
            }
            (1, 4) => match i {
                // FCHS and FABS only touch the sign; an empty register gives the indefinite.
                0 | 1 => {
                    let value = self.read(0);
                    let r = if self.is_empty(0) {
                        value
                    } else if i == 0 {
                        -value
                    } else {
                        value.abs()
                    };
                    self.write(0, r);
                }
                4 => {
                    let value = self.read(0);
                    self.compare(value, 0.0, false);
                }
                5 => self.examine(),
                _ => return None,
            },
            (1, 5) if i < 7 => self.push(CONSTANTS[usize::from(i)]),
            (1, 6) => match i {
                0 => self.unary(|x| libm::expm1(x * LN_2)),
                1 => self.log2_times(false),
                2 => self.tangent(),
                3 => {
                    let (x, y) = (self.read(0), self.read(1));
                    let r = self.result(libm::atan2(y, x), y, x);
                    self.write(1, r);
                    self.pop();
                }
                4 => self.extract(),
                5 => self.remainder(true),
                6 => {
                    self.top = self.top.wrapping_sub(1) & 7;
                    self.sw &= !C1;
                }
                _ => {
                    self.top = (self.top + 1) & 7;
                    self.sw &= !C1;
                }
            },
            (1, 7) => match i {
                0 => self.remainder(false),
                1 => self.log2_times(true),
                2 => self.unary(f64::sqrt),
                3 => self.sine_cosine(),
                4 => self.round_to_integer(),
                5 => self.scale(),
                6 => self.trig(libm::sin),
                _ => self.trig(libm::cos),
            },
            (2, 5) if i == 1 => {
                let (a, b) = (self.read(0), self.read(1));
                self.compare(a, b, true);
                self.pop();
                self.pop();
            }
            (3, 4) => {
                match i {
                    // FNENI and FNDISI (8087) and FNSETPM (80287) do nothing on a 387.
                    0 | 1 | 4 => {}
                    2 => self.sw &= !(EXCEPTIONS | SF | ES | BUSY),
                    3 => self.init(),
                    _ => return None,
                }
                return Some(true);
            }
            (5, 0) => self.empty |= 1 << self.phys(i),
            (5, 4 | 5) => {
                let (a, b) = (self.read(0), self.read(i));
                self.compare(a, b, true);
                if group == 5 {
                    self.pop();
                }
            }
            (7, 0) => {
                self.empty |= 1 << self.phys(i);
                self.pop();
            }
            _ => return None,
        }
        Some(false)
    }

    /// FXCH: an empty register reads as the indefinite.
    fn exchange(&mut self, i: u8) {
        let (exact0, exact_i) = (self.exact(0), self.exact(i));
        let (a, b) = (self.read(0), self.read(i));
        self.write_exact(0, b, exact_i);
        self.write_exact(i, a, exact0);
    }

    /// FST ST(i).
    fn copy_to(&mut self, i: u8) {
        let exact = self.exact(0);
        let value = self.read(0);
        self.write_exact(i, value, exact);
    }

    fn unary(&mut self, f: impl Fn(f64) -> f64) {
        let x = self.read(0);
        let r = self.result(f(x), x, x);
        self.write(0, r);
    }

    /// FSIN, FCOS, FPTAN and FSINCOS: a finite operand of 2^63 or more is out of range, which sets
    /// C2 and changes nothing.
    fn trig_operand(&mut self) -> Option<f64> {
        let x = self.read(0);
        let out_of_range = x.is_finite() && x.abs() >= TRIG_LIMIT;
        self.set_flag(C2, out_of_range);
        (!out_of_range).then_some(x)
    }

    fn trig(&mut self, f: fn(f64) -> f64) {
        if let Some(x) = self.trig_operand() {
            let r = self.result(f(x), x, x);
            self.write(0, r);
        }
    }

    /// FPTAN: ST(0) = tan ST(0), then pushes 1.
    fn tangent(&mut self) {
        if let Some(x) = self.trig_operand() {
            let r = self.result(libm::tan(x), x, x);
            self.write(0, r);
            self.push(if r.is_nan() { r } else { 1.0 });
        }
    }

    /// FSINCOS: ST(0) = sin, then pushes cos.
    fn sine_cosine(&mut self) {
        if let Some(x) = self.trig_operand() {
            let sine = self.result(libm::sin(x), x, x);
            let cosine = self.result(libm::cos(x), x, x);
            self.write(0, sine);
            self.push(cosine);
        }
    }

    /// FYL2X (ST(1) × log2 ST(0)) and FYL2XP1 (ST(1) × log2(ST(0) + 1)), popping.
    fn log2_times(&mut self, plus_one: bool) {
        let (x, y) = (self.read(0), self.read(1));
        let log = if plus_one { libm::log1p(x) * LOG2_E } else { libm::log2(x) };
        if !plus_one && x == 0.0 && y.is_finite() && y != 0.0 {
            self.sw |= ZE;
        }
        let r = self.result(y * log, x, y);
        self.write(1, r);
        self.pop();
    }

    /// FPREM (truncating quotient) and FPREM1 (IEEE remainder, quotient rounded to nearest). The
    /// low three quotient bits go to C0, C3 and C1; the reduction is always complete (C2 = 0).
    fn remainder(&mut self, ieee: bool) {
        let (x, y) = (self.read(0), self.read(1));
        let r = if ieee { libm::remainder(x, y) } else { libm::fmod(x, y) };
        let r = self.result(r, x, y);
        self.sw &= !C2;
        if !r.is_nan() {
            let quotient = x / y;
            let quotient = if ieee { quotient.round_ties_even() } else { quotient.trunc() };
            let q = quotient.abs() as u64;
            self.set_flag(C0, q & 4 != 0);
            self.set_flag(C3, q & 2 != 0);
            self.set_flag(C1, q & 1 != 0);
        }
        self.write(0, r);
    }

    /// FRNDINT: C1 tells whether the value was rounded up.
    fn round_to_integer(&mut self) {
        let x = self.read(0);
        if !x.is_finite() {
            let r = self.result(x, x, x);
            self.write(0, r);
            return;
        }
        let r = self.round(x);
        if r != x {
            self.sw |= PE;
        }
        self.set_flag(C1, r > x);
        self.write(0, r);
    }

    /// FSCALE: ST(0) × 2^trunc(ST(1)).
    fn scale(&mut self) {
        let (x, y) = (self.read(0), self.read(1));
        let r = if x.is_nan() || y.is_nan() {
            f64::NAN
        } else if y.is_infinite() {
            if y > 0.0 { x * f64::INFINITY } else { x * 0.0 }
        } else {
            libm::scalbn(x, y.trunc().clamp(-65536.0, 65536.0) as i32)
        };
        let r = self.result(r, x, y);
        self.write(0, r);
    }

    /// FXTRACT: ST(0) becomes its exponent, then its significand (in [1, 2)) is pushed.
    fn extract(&mut self) {
        let x = self.read(0);
        let (exponent, significand) = if x.is_nan() {
            let q = self.result(x, x, x);
            (q, q)
        } else if x == 0.0 {
            self.sw |= ZE;
            (f64::NEG_INFINITY, x)
        } else if x.is_infinite() {
            (f64::INFINITY, x)
        } else {
            let (mantissa, exponent) = libm::frexp(x);
            (f64::from(exponent - 1), mantissa * 2.0)
        };
        self.write(0, exponent);
        self.push(significand);
    }

    /// FXAM: C1 is the sign; C3 C2 C0 = 001 NaN, 010 normal, 011 infinity, 100 zero, 101 empty.
    /// Doubles too small to be normal are normal 80-bit numbers, so nothing reads as denormal.
    fn examine(&mut self) {
        let value = self.regs[self.phys(0)];
        self.set_flag(C1, value.is_sign_negative());
        let (c3, c2, c0) = if self.is_empty(0) {
            (true, false, true)
        } else if value.is_nan() {
            (false, false, true)
        } else if value.is_infinite() {
            (false, true, true)
        } else if value == 0.0 {
            (true, false, false)
        } else {
            (false, true, false)
        };
        self.set_flag(C3, c3);
        self.set_flag(C2, c2);
        self.set_flag(C0, c0);
    }
}

fn quiet(x: f64) -> f64 {
    f64::from_bits(x.to_bits() | QUIET)
}

fn is_signaling(x: f64) -> bool {
    x.is_nan() && x.to_bits() & QUIET == 0
}

/// A 32-bit real as a double (exact; NaN payloads kept by hand, not by the host's conversion).
fn from_single(bits: u32) -> f64 {
    let single = f32::from_bits(bits);
    if single.is_nan() {
        f64::from_bits((u64::from(bits >> 31) << 63) | (0x7FF << 52) | (u64::from(bits & 0x7F_FFFF) << 29))
    } else {
        f64::from(single)
    }
}

/// A double rounded to a 32-bit real.
fn to_single(value: f64) -> u32 {
    if value.is_nan() {
        let bits = value.to_bits();
        (((bits >> 63) as u32) << 31) | 0x7F80_0000 | (((bits >> 29) as u32) & 0x7F_FFFF) | 0x40_0000
    } else {
        (value as f32).to_bits()
    }
}

/// Rounds `significand` right by `shift` bits to nearest even.
fn round_shift(significand: u64, shift: u32) -> u64 {
    if shift == 0 {
        return significand;
    }
    if shift > 64 {
        // Less than half of the last place (DOSBox rounds these up to it).
        return 0;
    }
    let (high, rest, half) = if shift >= 64 {
        (0, significand, 1 << 63)
    } else {
        (significand >> shift, significand & ((1 << shift) - 1), 1 << (shift - 1))
    };
    if rest > half || (rest == half && high & 1 != 0) { high + 1 } else { high }
}

/// An 80-bit real (`significand` with its explicit integer bit, sign and exponent in `top`) as
/// the nearest double (DOSBox `FPU_FLD80`).
fn from_extended(significand: u64, top: u16) -> f64 {
    let sign = u64::from(top >> 15) << 63;
    let exponent = top & 0x7FFF;
    let infinity = sign | (0x7FF << 52);
    if exponent == 0x7FFF {
        // Infinity, or a NaN (made quiet).
        return f64::from_bits(if significand << 1 == 0 {
            infinity
        } else {
            infinity | QUIET | ((significand >> 11) & MANTISSA)
        });
    }
    let (significand, unbiased) = if exponent == 0 {
        if significand == 0 {
            return f64::from_bits(sign);
        }
        // Denormal: normalize so the integer bit is bit 63.
        let shift = significand.leading_zeros();
        (significand << shift, 1 - 16383 - i64::from(shift))
    } else {
        // Normal (an unnormal gets its integer bit back).
        (significand | (1 << 63), i64::from(exponent) - 16383)
    };
    let biased = unbiased + 1023;
    if biased <= 0 {
        // Below the normal range: a double denormal, or zero.
        let q = round_shift(significand, (12 - biased) as u32);
        return f64::from_bits(sign | q);
    }
    let q = round_shift(significand, 11);
    let (q, biased) = if q == 1 << 53 { (q >> 1, biased + 1) } else { (q, biased) };
    if biased >= 0x7FF {
        return f64::from_bits(infinity);
    }
    f64::from_bits(sign | ((biased as u64) << 52) | (q & MANTISSA))
}

/// A double as an 80-bit real: (significand, sign and exponent) (DOSBox `FPU_ST80`).
fn to_extended(value: f64) -> (u64, u16) {
    let bits = value.to_bits();
    let sign = ((bits >> 63) as u16) << 15;
    let exponent = ((bits >> 52) & 0x7FF) as u16;
    let mantissa = bits & MANTISSA;
    match exponent {
        0x7FF if mantissa == 0 => (1 << 63, sign | 0x7FFF),
        0x7FF => ((1 << 63) | (1 << 62) | (mantissa << 11), sign | 0x7FFF),
        0 if mantissa == 0 => (0, sign),
        0 => {
            // A double denormal is a normal 80-bit number: m × 2^-1074 with m's top bit at
            // 51 - shift has the exponent -1023 - shift (DOSBox's is one too high).
            let shift = (mantissa << 12).leading_zeros();
            (mantissa << (12 + shift), sign | (16383 - 1023 - shift as u16))
        }
        _ => ((1 << 63) | (mantissa << 11), sign | (exponent + 16383 - 1023)),
    }
}

/// 18-digit packed BCD: nine bytes of two digits, then the sign in bit 7 of the tenth.
fn from_bcd(bytes: [u8; 10]) -> f64 {
    let value =
        bytes[..9].iter().rev().fold(0u64, |value, &b| value * 100 + u64::from(b >> 4) * 10 + u64::from(b & 0xF));
    let value = value as f64;
    if bytes[9] & 0x80 != 0 { -value } else { value }
}

impl Cpu {
    fn x87(&mut self) -> &mut Fpu {
        self.fpu.as_mut().expect("the escape handlers run only with an FPU")
    }

    /// WAIT: #NM when CR0.MP and CR0.TS are both set.
    pub(crate) fn fwait(&mut self) -> Res<()> {
        if self.cr0 & (cr0::MP | cr0::TS) == cr0::MP | cr0::TS {
            return Err(Fault::new(vector::DEVICE_NOT_AVAILABLE).into());
        }
        Ok(())
    }

    /// The escape opcodes D8-DF. Without an FPU they decode their operand and do nothing, which
    /// is how a PC without a coprocessor behaves (and how programs detect one).
    pub(crate) fn escape<B: Bus>(&mut self, bus: &mut B, p: &Prefixes, op: u8, start_ip: u32) -> Res<()> {
        if self.cr0 & (cr0::EM | cr0::TS) != 0 {
            return Err(Fault::new(vector::DEVICE_NOT_AVAILABLE).into());
        }
        let modrm = self.fetch8(bus)?;
        let operand = self.decode_modrm(bus, p, modrm)?;
        if self.fpu.is_none() {
            return Ok(());
        }
        let esc = op & 7;
        let control = match operand.rm {
            Rm::Reg(i) if (esc, operand.reg, i) == (7, 4, 0) => {
                // FNSTSW AX
                let sw = self.x87().status_word();
                self.regs.set_ax(sw);
                true
            }
            Rm::Reg(i) => match self.x87().exec_reg(esc, operand.reg, i) {
                Some(control) => control,
                None => return Err(self.unimplemented(bus, start_ip)),
            },
            Rm::Mem { seg, off, mask } => {
                let at = Operand { seg, off, mask, op32: p.op32 };
                let Some(control) = self.escape_mem(bus, esc, operand.reg, at)? else {
                    return Err(self.unimplemented(bus, start_ip));
                };
                if !control {
                    let ds = self.regs.seg[seg];
                    let fpu = self.x87();
                    fpu.last.ds = ds;
                    fpu.last.dp = off;
                }
                control
            }
        };
        if !control {
            let cs = self.regs.seg[CS];
            let fpu = self.x87();
            fpu.last.cs = cs;
            fpu.last.ip = start_ip;
            fpu.last.opcode = (u16::from(esc) << 8) | u16::from(modrm);
            fpu.summarize();
        }
        Ok(())
    }

    /// A memory-form escape: `Some(true)` for a control instruction, `None` for an encoding the
    /// 387 doesn't have. Loads read memory before touching the stack, and stores pop only after
    /// writing, so an access that faults leaves the stack as it was (at most a sticky exception
    /// flag is set early) and the restarted instruction does it all again.
    fn escape_mem<B: Bus>(&mut self, bus: &mut B, esc: u8, group: u8, at: Operand) -> Res<Option<bool>> {
        match (esc, group) {
            // The arithmetic group on m32real, m32int, m64real and m16int.
            (0 | 2 | 4 | 6, op) => {
                let src = match esc {
                    0 => from_single(self.read_m32(bus, at, 0)?),
                    2 => f64::from(self.read_m32(bus, at, 0)? as i32),
                    4 => f64::from_bits(self.read_m64(bus, at)?),
                    _ => f64::from(self.read_m16(bus, at, 0)? as i16),
                };
                self.x87().arith_st0(op, src);
            }
            (1, 0) => {
                let value = from_single(self.read_m32(bus, at, 0)?);
                let fpu = self.x87();
                let value = fpu.load(value);
                fpu.push(value);
            }
            (5, 0) => {
                let value = f64::from_bits(self.read_m64(bus, at)?);
                let fpu = self.x87();
                let value = fpu.load(value);
                fpu.push(value);
            }
            (3, 0) => {
                let value = self.read_m32(bus, at, 0)? as i32;
                self.x87().push(f64::from(value));
            }
            (7, 0) => {
                let value = self.read_m16(bus, at, 0)? as i16;
                self.x87().push(f64::from(value));
            }
            (7, 5) => {
                let value = self.read_m64(bus, at)? as i64;
                self.x87().push_exact(value as f64, Some(value));
            }
            (3, 5) => {
                let significand = self.read_m64(bus, at)?;
                let top = self.read_m16(bus, at, 8)?;
                self.x87().push(from_extended(significand, top));
            }
            (7, 4) => {
                let mut bytes = [0; 10];
                for (i, byte) in bytes.iter_mut().enumerate() {
                    *byte = self.read_mem(bus, at.seg, at.off.wrapping_add(i as u32), at.mask, Width::Byte)? as u8;
                }
                self.x87().push(from_bcd(bytes));
            }
            // Stores; group 3 (and FBSTP, FISTP m64, FSTP m80) pop afterwards.
            (1 | 3 | 5 | 7, 2 | 3) | (3 | 7, 7) | (7, 6) => {
                let fpu = self.x87();
                match (esc, group) {
                    (1, _) => {
                        let value = fpu.read(0);
                        self.write_m32(bus, at, 0, to_single(value))?;
                    }
                    (5, _) => {
                        let value = fpu.read(0);
                        self.write_m64(bus, at, value.to_bits())?;
                    }
                    (3, 7) => {
                        let value = fpu.read(0);
                        let (significand, top) = to_extended(value);
                        self.write_m64(bus, at, significand)?;
                        self.write_m16(bus, at, 8, top)?;
                    }
                    (3, _) => {
                        let value = fpu.integer(32);
                        self.write_m32(bus, at, 0, value as u32)?;
                    }
                    (7, 6) => {
                        let bytes = fpu.bcd();
                        for (i, &byte) in bytes.iter().enumerate() {
                            let off = at.off.wrapping_add(i as u32);
                            self.write_mem(bus, at.seg, off, at.mask, Width::Byte, u32::from(byte))?;
                        }
                    }
                    (_, 7) => {
                        let value = fpu.integer(64);
                        self.write_m64(bus, at, value)?;
                    }
                    _ => {
                        let value = fpu.integer(16);
                        self.write_m16(bus, at, 0, value as u16)?;
                    }
                }
                if group != 2 {
                    self.x87().pop();
                }
            }
            (1, 4) => {
                let env = self.read_environment(bus, at)?;
                self.load_environment(env);
                return Ok(Some(true));
            }
            (1, 5) => {
                let cw = self.read_m16(bus, at, 0)?;
                self.x87().cw = cw;
                return Ok(Some(true));
            }
            (1, 6) => {
                self.store_environment(bus, at)?;
                self.x87().cw |= EXCEPTIONS;
                return Ok(Some(true));
            }
            (1, 7) => {
                let cw = self.x87().cw;
                self.write_m16(bus, at, 0, cw)?;
                return Ok(Some(true));
            }
            (5, 4) => {
                let env = self.read_environment(bus, at)?;
                let start = at.environment_size();
                let mut regs = [0.0; 8];
                for (i, reg) in regs.iter_mut().enumerate() {
                    let offset = start + 10 * i as u32;
                    let significand = self.read_m64(bus, at.plus(offset))?;
                    let top = self.read_m16(bus, at, offset + 8)?;
                    *reg = from_extended(significand, top);
                }
                self.load_environment(env);
                let fpu = self.x87();
                for (i, value) in (0..8).zip(regs) {
                    let r = fpu.phys(i);
                    fpu.regs[r] = value;
                    fpu.exact[r] = None;
                }
                return Ok(Some(true));
            }
            (5, 6) => {
                self.store_environment(bus, at)?;
                let start = at.environment_size();
                for i in 0..8 {
                    let fpu = self.x87();
                    let (significand, top) = to_extended(fpu.regs[fpu.phys(i)]);
                    let offset = start + 10 * u32::from(i);
                    self.write_m64(bus, at.plus(offset), significand)?;
                    self.write_m16(bus, at, offset + 8, top)?;
                }
                self.x87().init();
                return Ok(Some(true));
            }
            (5, 7) => {
                let sw = self.x87().status_word();
                self.write_m16(bus, at, 0, sw)?;
                return Ok(Some(true));
            }
            _ => return Ok(None),
        }
        Ok(Some(false))
    }

    // ---- environment (FLDENV, FNSTENV, FRSTOR, FNSAVE) ----

    /// FNSTENV's image: seven words (16-bit operand size) or doublewords (32-bit), in the
    /// real-mode layout in real and V86 mode (linear instruction and operand addresses, the
    /// opcode beside the instruction address) and the protected-mode one otherwise (selectors
    /// and offsets). Unused upper halves of 32-bit fields read as FFFFh, as on the chip.
    fn store_environment<B: Bus>(&mut self, bus: &mut B, at: Operand) -> Res<()> {
        let real = self.real_addressing();
        let fpu = self.x87();
        let (cw, sw, tags, last) = (fpu.cw, fpu.status_word(), fpu.tag_word(), fpu.last);
        let fields: [u32; 7] = if real {
            let ip = (u32::from(last.cs) << 4).wrapping_add(last.ip);
            let dp = (u32::from(last.ds) << 4).wrapping_add(last.dp);
            let high = if at.op32 { 0xFFFF_0000 } else { 0 };
            [
                high | u32::from(cw),
                high | u32::from(sw),
                high | u32::from(tags),
                high | (ip & 0xFFFF),
                ((ip >> 16) << 12) | u32::from(last.opcode),
                high | (dp & 0xFFFF),
                (dp >> 16) << 12,
            ]
        } else if at.op32 {
            [
                0xFFFF_0000 | u32::from(cw),
                0xFFFF_0000 | u32::from(sw),
                0xFFFF_0000 | u32::from(tags),
                last.ip,
                u32::from(last.cs) | (u32::from(last.opcode) << 16),
                last.dp,
                0xFFFF_0000 | u32::from(last.ds),
            ]
        } else {
            [
                u32::from(cw),
                u32::from(sw),
                u32::from(tags),
                last.ip & 0xFFFF,
                u32::from(last.cs),
                last.dp & 0xFFFF,
                u32::from(last.ds),
            ]
        };
        for (i, field) in (0..).zip(fields) {
            if at.op32 {
                self.write_m32(bus, at, 4 * i, field)?;
            } else {
                self.write_m16(bus, at, 2 * i, field as u16)?;
            }
        }
        Ok(())
    }

    fn read_environment<B: Bus>(&mut self, bus: &mut B, at: Operand) -> Res<Environment> {
        let mut fields = [0u32; 7];
        for (i, field) in (0..).zip(fields.iter_mut()) {
            *field = if at.op32 { self.read_m32(bus, at, 4 * i)? } else { u32::from(self.read_m16(bus, at, 2 * i)?) };
        }
        let last = if self.real_addressing() {
            let mask = if at.op32 { 0xFFFF } else { 0xF };
            Pointers {
                cs: 0,
                ip: (fields[3] & 0xFFFF) | (((fields[4] >> 12) & mask) << 16),
                opcode: (fields[4] & 0x7FF) as u16,
                ds: 0,
                dp: (fields[5] & 0xFFFF) | (((fields[6] >> 12) & mask) << 16),
            }
        } else {
            Pointers {
                cs: fields[4] as u16,
                ip: fields[3],
                opcode: if at.op32 { ((fields[4] >> 16) & 0x7FF) as u16 } else { 0 },
                ds: fields[6] as u16,
                dp: fields[5],
            }
        };
        Ok(Environment { cw: fields[0] as u16, sw: fields[1] as u16, tags: fields[2] as u16, last })
    }

    fn load_environment(&mut self, env: Environment) {
        let fpu = self.x87();
        fpu.cw = env.cw;
        fpu.set_status_word(env.sw);
        fpu.set_tag_word(env.tags);
        fpu.last = env.last;
    }

    // ---- memory operands ----

    fn read_m16<B: Bus>(&mut self, bus: &mut B, at: Operand, offset: u32) -> Res<u16> {
        Ok(self.read_mem(bus, at.seg, at.off.wrapping_add(offset), at.mask, Width::Word)? as u16)
    }

    fn read_m32<B: Bus>(&mut self, bus: &mut B, at: Operand, offset: u32) -> Res<u32> {
        self.read_mem(bus, at.seg, at.off.wrapping_add(offset), at.mask, Width::Dword)
    }

    fn read_m64<B: Bus>(&mut self, bus: &mut B, at: Operand) -> Res<u64> {
        let low = self.read_m32(bus, at, 0)?;
        let high = self.read_m32(bus, at, 4)?;
        Ok(u64::from(low) | (u64::from(high) << 32))
    }

    fn write_m16<B: Bus>(&mut self, bus: &mut B, at: Operand, offset: u32, value: u16) -> Res<()> {
        self.write_mem(bus, at.seg, at.off.wrapping_add(offset), at.mask, Width::Word, u32::from(value))
    }

    fn write_m32<B: Bus>(&mut self, bus: &mut B, at: Operand, offset: u32, value: u32) -> Res<()> {
        self.write_mem(bus, at.seg, at.off.wrapping_add(offset), at.mask, Width::Dword, value)
    }

    fn write_m64<B: Bus>(&mut self, bus: &mut B, at: Operand, value: u64) -> Res<()> {
        self.write_m32(bus, at, 0, value as u32)?;
        self.write_m32(bus, at, 4, (value >> 32) as u32)
    }
}

/// A memory operand of an escape instruction.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct Operand {
    seg: usize,
    off: u32,
    mask: u32,
    /// 32-bit operand size: selects the 28-byte environment layout.
    op32: bool,
}

impl Operand {
    fn plus(self, offset: u32) -> Self {
        Self { off: self.off.wrapping_add(offset), ..self }
    }

    fn environment_size(&self) -> u32 {
        if self.op32 { 28 } else { 14 }
    }
}

/// The control registers from an FLDENV or FRSTOR image.
struct Environment {
    cw: u16,
    sw: u16,
    tags: u16,
    last: Pointers,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_round_trip() {
        for value in [0.0, -0.0, 1.0, -2.5, PI, 1e300, -1e-300, 5e-324, f64::MAX, f64::MIN_POSITIVE, f64::INFINITY] {
            let (significand, top) = to_extended(value);
            assert_eq!(from_extended(significand, top).to_bits(), value.to_bits(), "{value}");
        }
    }

    #[test]
    fn extended_encoding() {
        assert_eq!(to_extended(1.0), (0x8000_0000_0000_0000, 0x3FFF));
        assert_eq!(to_extended(-2.0), (0x8000_0000_0000_0000, 0xC000));
        assert_eq!(to_extended(f64::NEG_INFINITY), (0x8000_0000_0000_0000, 0xFFFF));
        // The smallest double denormal is a normal 80-bit number.
        assert_eq!(to_extended(5e-324), (0x8000_0000_0000_0000, 16383 - 1074));
        assert_eq!(to_extended(INDEFINITE), (0xC000_0000_0000_0000, 0xFFFF));
    }

    #[test]
    fn extended_to_double_rounds_to_nearest_even() {
        // 1 + 2^-53 is halfway between 1 and the next double: ties go to the even significand.
        assert_eq!(from_extended(0x8000_0000_0000_0400, 0x3FFF), 1.0);
        // 1 + 3 × 2^-53 is halfway between two doubles whose even neighbor is above.
        assert_eq!(from_extended(0x8000_0000_0000_0C00, 0x3FFF), 1.0 + 4.0 * f64::EPSILON / 2.0);
        // Rounding up the largest significand carries into the exponent.
        assert_eq!(from_extended(0xFFFF_FFFF_FFFF_FFFF, 0x3FFF), 2.0);
        // Too large for a double: infinity; too small: zero.
        assert_eq!(from_extended(0x8000_0000_0000_0000, 0x7FFE), f64::INFINITY);
        assert_eq!(from_extended(0x8000_0000_0000_0000, 0x0001).to_bits(), 0);
        // A denormal 80-bit number.
        assert_eq!(from_extended(1, 0).to_bits(), 0);
    }

    #[test]
    fn singles_keep_nan_payloads() {
        let snan = 0x7F80_0001;
        let value = from_single(snan);
        assert!(is_signaling(value));
        assert_eq!(to_single(quiet(value)), 0x7FC0_0001);
        assert_eq!(to_single(INDEFINITE), 0xFFC0_0000);
        assert_eq!(from_single(0x3F80_0000), 1.0);
    }

    #[test]
    fn bcd_conversions() {
        let bytes = [0x21, 0x43, 0x65, 0x87, 0x09, 0, 0, 0, 0, 0x80];
        assert_eq!(from_bcd(bytes), -987654321.0);
        let mut fpu = Fpu::new();
        fpu.push(-987654321.4);
        assert_eq!(fpu.bcd(), bytes);
        fpu.write(0, 1e18);
        assert_eq!(fpu.bcd(), [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]);
        assert_ne!(fpu.sw & IE, 0);
    }

    #[test]
    fn invalid_operations_give_the_indefinite() {
        let mut fpu = Fpu::new();
        assert_eq!(fpu.arith(6, 0.0, 0.0).to_bits(), INDEFINITE.to_bits());
        assert_eq!(fpu.sw & (IE | ZE), IE);
        fpu.sw = 0;
        assert_eq!(fpu.arith(6, 1.0, -0.0), f64::NEG_INFINITY);
        assert_eq!(fpu.sw & (IE | ZE), ZE);
        fpu.sw = 0;
        assert_eq!(fpu.arith(4, f64::INFINITY, f64::INFINITY).to_bits(), INDEFINITE.to_bits());
        // A NaN operand propagates, quieted, without IE.
        fpu.sw = 0;
        let nan = f64::from_bits(0x7FF8_0000_0000_1234);
        assert_eq!(fpu.arith(0, 1.0, nan).to_bits(), nan.to_bits());
        assert_eq!(fpu.sw & IE, 0);
    }

    #[test]
    fn stack_overflow_and_underflow() {
        let mut fpu = Fpu::new();
        for i in 0..8 {
            fpu.push(f64::from(i));
        }
        assert_eq!(fpu.sw & (IE | SF), 0);
        fpu.push(8.0);
        assert_eq!(fpu.sw & (IE | SF | C1), IE | SF | C1);
        assert_eq!(fpu.st(0).map(f64::to_bits), Some(INDEFINITE.to_bits()));
        let mut fpu = Fpu::new();
        assert_eq!(fpu.read(0).to_bits(), INDEFINITE.to_bits());
        assert_eq!(fpu.sw & (IE | SF | C1), IE | SF);
    }
}
