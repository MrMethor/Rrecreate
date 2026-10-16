//! The x87 FPU: 16-bit real-mode code assembled with iced-x86, with operands in DS (3000:0000).

mod common;

use common::*;
use cpu::fpu::status::{C0, C1, C2, C3, IE, SF};
use cpu::{Cpu, Exit, cr0};
use iced_x86::code_asm::*;
use std::f64::consts::{FRAC_PI_4, PI, SQRT_2};

type Program = fn(&mut CodeAssembler) -> Result<(), IcedError>;

fn assemble(build: impl FnOnce(&mut CodeAssembler) -> Result<(), IcedError>) -> Vec<u8> {
    let mut a = CodeAssembler::new(16).unwrap();
    build(&mut a).unwrap();
    a.hlt().unwrap();
    a.assemble(0).unwrap()
}

/// DS contents: each value at its offset.
fn data(items: &[(u16, &[u8])]) -> Vec<u8> {
    let mut bytes = vec![0; 0x300];
    for (at, value) in items {
        bytes[usize::from(*at)..usize::from(*at) + value.len()].copy_from_slice(value);
    }
    bytes
}

fn run_with(
    init: &[u8],
    configure: impl FnOnce(&mut Cpu),
    build: impl FnOnce(&mut CodeAssembler) -> Result<(), IcedError>,
) -> (Cpu, TestBus) {
    let (mut cpu, mut bus) = setup(&assemble(build));
    bus.put(DATA_SEG, 0, init);
    configure(&mut cpu);
    run_to_hlt(&mut cpu, &mut bus);
    (cpu, bus)
}

fn run_fpu(init: &[u8], build: impl FnOnce(&mut CodeAssembler) -> Result<(), IcedError>) -> (Cpu, TestBus) {
    run_with(init, |_| {}, build)
}

fn bytes<const N: usize>(bus: &TestBus, at: u16) -> [u8; N] {
    let start = TestBus::lin(DATA_SEG, at);
    bus.mem[start..start + N].try_into().unwrap()
}

fn f64_at(bus: &TestBus, at: u16) -> f64 {
    f64::from_le_bytes(bytes(bus, at))
}

fn m(at: u32) -> AsmMemoryOperand {
    ptr(at)
}

fn detect(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.fninit()?;
    a.fnstsw(word_ptr(0x10))?;
    a.fnstcw(word_ptr(0x12))
}

#[test]
fn fninit_and_coprocessor_detection() {
    let init = data(&[(0x10, &[0x5A; 4])]);
    let (cpu, bus) = run_fpu(&init, detect);
    assert_eq!(bus.word(DATA_SEG, 0x10), 0, "status word");
    assert_eq!(bus.word(DATA_SEG, 0x12), 0x037F, "control word");
    assert_eq!(cpu.fpu.as_ref().unwrap().tag_word(), 0xFFFF);

    // Without a coprocessor the stores never happen, which is how programs detect one.
    let (_, bus) = run_with(&init, |cpu| cpu.fpu = None, detect);
    assert_eq!(bus.word(DATA_SEG, 0x10), 0x5A5A);
    assert_eq!(bus.word(DATA_SEG, 0x12), 0x5A5A);
}

/// Where a register-form instruction leaves its result.
#[derive(Clone, Copy, PartialEq)]
enum Out {
    St0,
    St1,
    /// The only entry left on the stack.
    Only,
}

#[test]
fn arithmetic_on_memory_and_registers() {
    let init = data(&[
        (0x00, &1.5f32.to_le_bytes()),
        (0x08, &2.25f64.to_le_bytes()),
        (0x10, &3i16.to_le_bytes()),
        (0x14, &(-4i32).to_le_bytes()),
        (0x30, &2.0f64.to_le_bytes()),
        (0x38, &8.0f64.to_le_bytes()),
    ]);
    let (cpu, bus) = run_fpu(&init, |a| {
        a.fld(dword_ptr(0x00))?; // 1.5
        a.fld(qword_ptr(0x08))?; // 2.25, 1.5
        a.faddp(st1, st0)?; // 3.75
        a.fmul(dword_ptr(0x00))?; // 5.625
        a.fisub(word_ptr(0x10))?; // 2.625
        a.fidiv(dword_ptr(0x14))?; // -0.65625
        a.fstp(qword_ptr(0x20))?;
        // Register forms with ST(0) = 8 and ST(1) = 2, and where each leaves its result.
        let forms: [(Program, Out); 6] = [
            (|a| a.fdiv_2(st1, st0), Out::St1),  // DC F8+i: ST(1) = ST(1) / ST(0)
            (|a| a.fdivr_2(st1, st0), Out::St1), // DC F0+i: ST(1) = ST(0) / ST(1)
            (|a| a.fsub_2(st1, st0), Out::St1),  // DC E8+i: ST(1) = ST(1) - ST(0)
            (|a| a.fsubr_2(st1, st0), Out::St1), // DC E0+i: ST(1) = ST(0) - ST(1)
            (|a| a.fsubp(st1, st0), Out::Only),  // DE E8+i: ST(1) - ST(0), popped
            (|a| a.fsubr_2(st0, st1), Out::St0), // D8 E8+i: ST(0) = ST(1) - ST(0)
        ];
        for (at, (form, result)) in (0x40..).step_by(8).zip(forms) {
            a.fld(qword_ptr(0x30))?;
            a.fld(qword_ptr(0x38))?;
            form(a)?;
            if result == Out::St1 {
                a.fstp(st0)?;
            }
            a.fstp(qword_ptr(at))?;
            if result == Out::St0 {
                a.fstp(st0)?;
            }
        }
        Ok(())
    });
    assert_eq!(f64_at(&bus, 0x20), -0.65625);
    let results: Vec<f64> = (0..6).map(|i| f64_at(&bus, 0x40 + 8 * i)).collect();
    assert_eq!(results, [0.25, 4.0, -6.0, 6.0, -6.0, -6.0]);
    assert_eq!(cpu.fpu.as_ref().unwrap().tag_word(), 0xFFFF, "the stack is balanced");
}

/// The status word after `FLD b; FLD a; <compare>; FNSTSW`.
fn compare_status(a_value: f64, b_value: f64, compare: Program) -> u16 {
    let init = data(&[(0x00, &a_value.to_le_bytes()), (0x08, &b_value.to_le_bytes())]);
    let (_, bus) = run_fpu(&init, |a| {
        a.fld(qword_ptr(0x08))?;
        a.fld(qword_ptr(0x00))?;
        compare(a)?;
        a.fnstsw(ax)?;
        a.mov(word_ptr(0x20), ax)
    });
    bus.word(DATA_SEG, 0x20)
}

#[test]
fn comparisons_set_the_condition_codes() {
    let codes = |a, b, compare| compare_status(a, b, compare) & (C3 | C2 | C0);
    let fcompp: Program = |a| a.fcompp();
    assert_eq!(codes(1.0, 2.0, fcompp), C0, "less");
    assert_eq!(codes(2.0, 1.0, fcompp), 0, "greater");
    assert_eq!(codes(2.0, 2.0, fcompp), C3, "equal");
    assert_eq!(codes(f64::NAN, 1.0, fcompp), C3 | C2 | C0, "unordered");
    // FCOM flags any NaN as invalid; FUCOM only signaling ones.
    assert_eq!(compare_status(f64::NAN, 1.0, fcompp) & IE, IE);
    assert_eq!(compare_status(f64::NAN, 1.0, |a| a.fucompp()) & IE, 0);
    // FTST compares with zero; FXAM classifies.
    assert_eq!(codes(-3.0, 0.0, |a| a.ftst()), C0);
    assert_eq!(codes(f64::NEG_INFINITY, 0.0, |a| a.fxam()), C2 | C0);
    assert_eq!(compare_status(-0.0, 0.0, |a| a.fxam()) & (C3 | C2 | C1 | C0), C3 | C1, "negative zero");
    assert_eq!(compare_status(1e-310, 0.0, |a| a.fxam()) & (C3 | C2 | C0), C2, "normal in 80 bits");
}

#[test]
fn integer_stores_round_by_the_control_word() {
    let cases = [
        (0, 2.5, 2),
        (0, 3.5, 4),
        (0, -2.5, -2),
        (1, 2.7, 2),
        (1, -2.3, -3),
        (2, 2.1, 3),
        (2, -2.7, -2),
        (3, 2.7, 2),
        (3, -2.7, -2),
    ];
    for (rc, value, expected) in cases {
        let control = 0x037Fu16 | (rc << 10);
        let init = data(&[(0x00, &control.to_le_bytes()), (0x08, &f64::to_le_bytes(value))]);
        let (_, bus) = run_fpu(&init, |a| {
            a.fldcw(word_ptr(0x00))?;
            a.fld(qword_ptr(0x08))?;
            a.fistp(word_ptr(0x10))?;
            a.fld(qword_ptr(0x08))?;
            a.frndint()?;
            a.fstp(qword_ptr(0x18))
        });
        assert_eq!(bus.word(DATA_SEG, 0x10) as i16, expected, "FISTP {value} with RC={rc}");
        assert_eq!(f64_at(&bus, 0x18), f64::from(expected), "FRNDINT {value} with RC={rc}");
    }
}

#[test]
fn integer_overflow_stores_the_integer_indefinite() {
    let init = data(&[(0x00, &40000.0f64.to_le_bytes())]);
    let (_, bus) = run_fpu(&init, |a| {
        a.fld(qword_ptr(0x00))?;
        a.fist(word_ptr(0x10))?;
        a.fistp(dword_ptr(0x14))?;
        a.fnstsw(word_ptr(0x20))
    });
    assert_eq!(bus.word(DATA_SEG, 0x10), 0x8000);
    assert_eq!(u32::from_le_bytes(bytes(&bus, 0x14)), 40000);
    assert_eq!(bus.word(DATA_SEG, 0x20) & IE, IE);
}

#[test]
fn sixty_four_bit_integers_copy_exactly() {
    // 2^60 + 1 doesn't fit a double's 53 bits, but FILD/FISTP copies it unchanged (some
    // programs copy memory this way), also through FXCH.
    let value: i64 = (1 << 60) + 1;
    let init = data(&[(0x00, &value.to_le_bytes())]);
    let (_, bus) = run_fpu(&init, |a| {
        a.fild(qword_ptr(0x00))?;
        a.fld1()?;
        a.fxch(st0, st1)?;
        a.fistp(qword_ptr(0x10))?;
        a.fstp(st0)?;
        // Arithmetic on the value ends the exact copy.
        a.fild(qword_ptr(0x00))?;
        a.fld1()?;
        a.faddp(st1, st0)?;
        a.fistp(qword_ptr(0x18))
    });
    assert_eq!(i64::from_le_bytes(bytes(&bus, 0x10)), value);
    assert_eq!(i64::from_le_bytes(bytes(&bus, 0x18)), 1 << 60, "rounded to a double");
}

#[test]
fn extended_reals_load_and_store() {
    let mut pi80 = 0xC90F_DAA2_2168_C235u64.to_le_bytes().to_vec();
    pi80.extend(0x4000u16.to_le_bytes());
    let init = data(&[(0x00, &pi80)]);
    let (_, bus) = run_fpu(&init, |a| {
        a.fld(tword_ptr(0x00))?;
        a.fstp(qword_ptr(0x10))?;
        a.fldpi()?;
        a.fstp(tword_ptr(0x20))
    });
    assert_eq!(f64_at(&bus, 0x10), PI);
    let stored: [u8; 10] = bytes(&bus, 0x20);
    assert_eq!(stored[..8], 0xC90F_DAA2_2168_C000u64.to_le_bytes());
    assert_eq!(stored[8..], [0x00, 0x40]);
}

#[test]
fn packed_bcd_load_and_store() {
    let bcd = [0x34, 0x12, 0, 0, 0, 0, 0, 0, 0, 0x80]; // -1234
    let init = data(&[(0x00, &bcd)]);
    let (_, bus) = run_fpu(&init, |a| {
        a.fbld(tword_ptr(0x00))?;
        a.fld(st0)?;
        a.fistp(word_ptr(0x10))?;
        a.fbstp(tword_ptr(0x20))
    });
    assert_eq!(bus.word(DATA_SEG, 0x10) as i16, -1234);
    assert_eq!(bytes::<10>(&bus, 0x20), bcd);
}

#[test]
fn transcendental_functions() {
    let init = data(&[
        (0x100, &8.0f64.to_le_bytes()),
        (0x108, &4.0f64.to_le_bytes()),
        (0x110, &3.0f64.to_le_bytes()),
        (0x118, &12.0f64.to_le_bytes()),
        (0x120, &7.0f64.to_le_bytes()),
        (0x128, &18_446_744_073_709_551_616.0f64.to_le_bytes()), // 2^64
        (0x130, &0.5f64.to_le_bytes()),
        (0x138, &2.0f64.to_le_bytes()),
    ]);
    let (cpu, bus) = run_fpu(&init, |a| {
        // sin(π/2)
        a.fldpi()?;
        a.fdiv(qword_ptr(0x138))?;
        a.fsin()?;
        a.fstp(qword_ptr(0x00))?;
        // atan2(1, 1)
        a.fld1()?;
        a.fld1()?;
        a.fpatan()?;
        a.fstp(qword_ptr(0x08))?;
        // 1 × log2 8
        a.fld1()?;
        a.fld(qword_ptr(0x100))?;
        a.fyl2x()?;
        a.fstp(qword_ptr(0x10))?;
        // 2^0.5 - 1
        a.fld(qword_ptr(0x130))?;
        a.f2xm1()?;
        a.fstp(qword_ptr(0x18))?;
        // 3 × 2^4
        a.fld(qword_ptr(0x108))?;
        a.fld(qword_ptr(0x110))?;
        a.fscale()?;
        a.fstp(qword_ptr(0x20))?;
        a.fstp(st0)?;
        // √2
        a.fld(qword_ptr(0x138))?;
        a.fsqrt()?;
        a.fstp(qword_ptr(0x28))?;
        // 12 = 1.5 × 2^3
        a.fld(qword_ptr(0x118))?;
        a.fxtract()?;
        a.fstp(qword_ptr(0x30))?;
        a.fstp(qword_ptr(0x38))?;
        // 7 mod 3 = 1, quotient 2
        a.fld(qword_ptr(0x110))?;
        a.fld(qword_ptr(0x120))?;
        a.fprem()?;
        a.fnstsw(word_ptr(0x40))?;
        a.fstp(qword_ptr(0x48))?;
        a.fstp(st0)?;
        // 2^64 is out of FSIN's range: C2 set, operand unchanged.
        a.fld(qword_ptr(0x128))?;
        a.fsin()?;
        a.fnstsw(word_ptr(0x50))?;
        a.fstp(qword_ptr(0x58))?;
        // tan 0.5, then 1
        a.fld(qword_ptr(0x130))?;
        a.fptan()?;
        a.fstp(qword_ptr(0x60))?;
        a.fstp(qword_ptr(0x68))?;
        // sin and cos of 0.5
        a.fld(qword_ptr(0x130))?;
        a.fsincos()?;
        a.fstp(qword_ptr(0x70))?;
        a.fstp(qword_ptr(0x78))
    });
    assert_eq!(f64_at(&bus, 0x00), 1.0);
    assert_eq!(f64_at(&bus, 0x08), FRAC_PI_4);
    assert_eq!(f64_at(&bus, 0x10), 3.0);
    assert_eq!(f64_at(&bus, 0x18), libm::expm1(0.5 * std::f64::consts::LN_2));
    assert!((f64_at(&bus, 0x18) - (SQRT_2 - 1.0)).abs() < 1e-15);
    assert_eq!(f64_at(&bus, 0x20), 48.0);
    assert_eq!(f64_at(&bus, 0x28), SQRT_2);
    assert_eq!((f64_at(&bus, 0x30), f64_at(&bus, 0x38)), (1.5, 3.0));
    assert_eq!(bus.word(DATA_SEG, 0x40) & (C3 | C2 | C1 | C0), C3, "quotient 2 in C3 C1 C0 = 1 0 0");
    assert_eq!(f64_at(&bus, 0x48), 1.0);
    assert_eq!(bus.word(DATA_SEG, 0x50) & C2, C2);
    assert_eq!(f64_at(&bus, 0x58), 18_446_744_073_709_551_616.0);
    assert_eq!((f64_at(&bus, 0x60), f64_at(&bus, 0x68)), (1.0, libm::tan(0.5)));
    assert_eq!((f64_at(&bus, 0x70), f64_at(&bus, 0x78)), (libm::cos(0.5), libm::sin(0.5)));
    assert_eq!(cpu.fpu.as_ref().unwrap().tag_word(), 0xFFFF, "the stack is balanced");
}

#[test]
fn stack_overflow_gives_the_indefinite() {
    let (cpu, bus) = run_fpu(&[], |a| {
        for _ in 0..9 {
            a.fld1()?;
        }
        a.fnstsw(word_ptr(0x10))
    });
    assert_eq!(bus.word(DATA_SEG, 0x10) & (IE | SF | C1), IE | SF | C1);
    assert!(cpu.fpu.as_ref().unwrap().st(0).unwrap().is_nan());
}

#[test]
fn save_and_restore_in_real_mode() {
    let init = data(&[(0x30, &0.5f64.to_le_bytes())]);
    let (_, bus) = run_fpu(&init, |a| {
        a.fld(qword_ptr(0x30))?; // 0000: DD 06 30 00
        a.fstp(st0)?; // 0004: DD D8
        a.fld1()?; // 0006: D9 E8
        a.fldpi()?; // 0008: D9 EB
        a.fnsave(m(0x100))?;
        a.fnstsw(word_ptr(0x10))?;
        a.frstor(m(0x100))?;
        a.fstp(qword_ptr(0x20))?;
        a.fstp(qword_ptr(0x28))?;
        a.fld1()?;
        a.db(&[0x66])?; // 32-bit operand size: the 28-byte layout
        a.fnstenv(m(0x200))
    });
    let word = |at| bus.word(DATA_SEG, at);
    assert_eq!(word(0x100), 0x037F, "control word");
    assert_eq!(word(0x102), 6 << 11, "status word with TOP = 6");
    assert_eq!(word(0x104), 0x0FFF, "R6 and R7 valid, the rest empty");
    // FLDPI at 1000:0008 (linear 10008h), opcode D9 EB.
    assert_eq!((word(0x106), word(0x108)), (0x0008, 0x1000 | 0x1EB));
    // The last memory operand: 3000:0030 (linear 30030h).
    assert_eq!((word(0x10A), word(0x10C)), (0x0030, 0x3000));
    assert_eq!(bytes::<10>(&bus, 0x10E)[8..], [0x00, 0x40], "ST(0) = π");
    assert_eq!(word(0x10), 0, "FNSAVE reinitializes");
    assert_eq!((f64_at(&bus, 0x20), f64_at(&bus, 0x28)), (PI, 1.0));

    let dword = |at| u32::from_le_bytes(bytes(&bus, at));
    assert_eq!(dword(0x200), 0xFFFF_037F);
    assert_eq!(dword(0x204), 0xFFFF_0000 | (7 << 11));
    assert_eq!(dword(0x208), 0xFFFF_3FFF, "only R7 valid");
    // After FLD m64 (4 bytes), FSTP, FLD1, FLDPI (2 each), FNSAVE, FNSTSW, FRSTOR, FSTP, FSTP (4 each).
    let fld1_at: u32 = 0x10000 + 0x1E;
    assert_eq!(dword(0x20C), 0xFFFF_0000 | (fld1_at & 0xFFFF));
    assert_eq!(dword(0x210), ((fld1_at >> 16) << 12) | 0x1E8);
}

#[test]
fn em_and_ts_raise_device_not_available() {
    // The #NM handler records that it ran and halts; the pushed IP is the faulting instruction.
    let handler = assemble(|a| a.mov(word_ptr(0x80), 0x1234));
    let run = |cr0_bits: u32, has_fpu: bool, instruction: Program| {
        let code = assemble(|a| {
            a.mov(eax, cr0)?;
            a.or(eax, cr0_bits as i32)?;
            a.mov(cr0, eax)?;
            instruction(a)
        });
        let (mut cpu, mut bus) = setup(&code);
        if !has_fpu {
            cpu.fpu = None;
        }
        bus.put(CODE_SEG, 0x100, &handler);
        bus.set_vector(7, CODE_SEG, 0x100);
        run_to_hlt(&mut cpu, &mut bus);
        let ip = bus.word(STACK_SEG, 0x1000 - 6);
        (bus.word(DATA_SEG, 0x80) == 0x1234, bus.mem[TestBus::lin(CODE_SEG, ip)])
    };
    let fld1: Program = |a| a.fld1();
    let wait: Program = |a| a.wait();
    assert_eq!(run(cr0::EM, true, fld1), (true, 0xD9));
    assert_eq!(run(cr0::EM, false, fld1), (true, 0xD9), "an emulator can run without a coprocessor");
    assert_eq!(run(cr0::TS, true, fld1), (true, 0xD9));
    assert_eq!(run(cr0::TS | cr0::MP, true, wait), (true, 0x9B));
    assert!(!run(cr0::TS, true, wait).0, "WAIT ignores TS without MP");
}

#[test]
fn encodings_the_387_lacks_stop_the_run() {
    let code = assemble(|a| a.fcmovb(st0, st1));
    let (mut cpu, mut bus) = setup(&code);
    match cpu.step(&mut bus) {
        Err(Exit::Unimplemented { bytes, .. }) => assert_eq!(bytes[..2], [0xDA, 0xC1]),
        other => panic!("expected an unimplemented instruction, got {other:?}"),
    }
}
