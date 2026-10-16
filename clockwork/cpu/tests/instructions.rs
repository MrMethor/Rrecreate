//! Instruction-level tests for the 8086/80186/80286 instruction set. Code is hand-encoded; each
//! byte group is one instruction.

mod common;

use common::*;
use cpu::flags::{AF, CF, DF, IF, OF, PF, SF, TF, ZF};
use cpu::{Exit, Model, seg};

#[test]
fn add_sets_overflow_sign_and_aux_carry() {
    let (cpu, _) = run(&[0xB0, 0x7F, 0x04, 0x01, 0xF4]); // mov al,7Fh; add al,1
    assert_eq!(cpu.regs.al(), 0x80);
    assert!(flags_set(&cpu, OF | SF | AF));
    assert!(flags_clear(&cpu, CF | ZF | PF));
}

#[test]
fn add_and_adc_chain_a_32_bit_sum() {
    // mov ax,FFFFh; mov dx,1; add ax,1; adc dx,0
    let (cpu, _) = run(&[0xB8, 0xFF, 0xFF, 0xBA, 0x01, 0x00, 0x05, 0x01, 0x00, 0x83, 0xD2, 0x00, 0xF4]);
    assert_eq!((cpu.regs.dx(), cpu.regs.ax()), (2, 0));
    assert!(flags_clear(&cpu, CF));
}

#[test]
fn sub_and_sbb_borrow() {
    // mov ax,0; mov dx,1; sub ax,1; sbb dx,0
    let (cpu, _) = run(&[0xB8, 0x00, 0x00, 0xBA, 0x01, 0x00, 0x2D, 0x01, 0x00, 0x81, 0xDA, 0x00, 0x00, 0xF4]);
    assert_eq!((cpu.regs.dx(), cpu.regs.ax()), (0, 0xFFFF));
    assert!(flags_clear(&cpu, CF));
    assert!(flags_set(&cpu, ZF | PF));
}

#[test]
fn cmp_signed_overflow_and_unsigned_borrow() {
    // mov ax,8000h; cmp ax,1 → 8000h-1 = 7FFFh: OF set (neg - pos = pos), no borrow
    let (cpu, _) = run(&[0xB8, 0x00, 0x80, 0x3D, 0x01, 0x00, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0x8000, "CMP must not write");
    assert!(flags_set(&cpu, OF | AF));
    assert!(flags_clear(&cpu, CF | SF | ZF));
    // mov ax,1; cmp ax,2 → borrow
    let (cpu, _) = run(&[0xB8, 0x01, 0x00, 0x3D, 0x02, 0x00, 0xF4]);
    assert!(flags_set(&cpu, CF | SF));
    assert!(flags_clear(&cpu, OF));
}

#[test]
fn inc_and_dec_preserve_carry() {
    // stc; mov ax,FFFFh; inc ax
    let (cpu, _) = run(&[0xF9, 0xB8, 0xFF, 0xFF, 0x40, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0);
    assert!(flags_set(&cpu, CF | ZF | AF));
    let (cpu, _) = run(&[0xF8, 0x4B, 0xF4]); // clc; dec bx (0 → FFFF)
    assert_eq!(cpu.regs.bx(), 0xFFFF);
    assert!(flags_clear(&cpu, CF));
    assert!(flags_set(&cpu, SF));
}

#[test]
fn neg_and_not() {
    // mov ax,1; neg ax
    let (cpu, _) = run(&[0xB8, 0x01, 0x00, 0xF7, 0xD8, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0xFFFF);
    assert!(flags_set(&cpu, CF | SF));
    // mov ax,8000h; neg ax → overflow
    let (cpu, _) = run(&[0xB8, 0x00, 0x80, 0xF7, 0xD8, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0x8000);
    assert!(flags_set(&cpu, OF | CF));
    // xor ax,ax; neg ax → CF clear
    let (cpu, _) = run(&[0x31, 0xC0, 0xF7, 0xD8, 0xF4]);
    assert!(flags_clear(&cpu, CF));
    // mov ax,0F0Fh; not ax (flags untouched: stc first)
    let (cpu, _) = run(&[0xF9, 0xB8, 0x0F, 0x0F, 0xF7, 0xD0, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0xF0F0);
    assert!(flags_set(&cpu, CF));
}

#[test]
fn logic_clears_carry_and_overflow() {
    // stc; mov ax,8001h; or ax,ax
    let (cpu, _) = run(&[0xF9, 0xB8, 0x01, 0x80, 0x09, 0xC0, 0xF4]);
    assert!(flags_clear(&cpu, CF | OF | ZF));
    assert!(flags_set(&cpu, SF));
    // mov al,3; test al,80h
    let (cpu, _) = run(&[0xB0, 0x03, 0xA8, 0x80, 0xF4]);
    assert!(flags_set(&cpu, ZF | PF));
}

#[test]
fn multiply() {
    // mov ax,1234h; mov bx,100h; mul bx → DX:AX = 0012:3400
    let (cpu, _) = run(&[0xB8, 0x34, 0x12, 0xBB, 0x00, 0x01, 0xF7, 0xE3, 0xF4]);
    assert_eq!((cpu.regs.dx(), cpu.regs.ax()), (0x0012, 0x3400));
    assert!(flags_set(&cpu, CF | OF));
    // mov ax,-2; mov bx,3; imul bx → -6 fits in 16 bits
    let (cpu, _) = run(&[0xB8, 0xFE, 0xFF, 0xBB, 0x03, 0x00, 0xF7, 0xEB, 0xF4]);
    assert_eq!((cpu.regs.dx(), cpu.regs.ax()), (0xFFFF, 0xFFFA));
    assert!(flags_clear(&cpu, CF | OF));
    // mov bx,1000h; imul ax,bx,-3 (ax = -3 * 1000h = -3000h, fits)
    let (cpu, _) = run(&[0xBB, 0x00, 0x10, 0x6B, 0xC3, 0xFD, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0xD000);
    assert!(flags_clear(&cpu, OF));
    // mov al,-128; mov bl,2; imul bl → AX=-256, doesn't fit in AL
    let (cpu, _) = run(&[0xB0, 0x80, 0xB3, 0x02, 0xF6, 0xEB, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0xFF00);
    assert!(flags_set(&cpu, CF | OF));
}

#[test]
fn divide() {
    // mov dx,1; mov ax,2; mov bx,10h; div bx → 10002h / 10h = 1000h r 2
    let (cpu, _) = run(&[0xBA, 0x01, 0x00, 0xB8, 0x02, 0x00, 0xBB, 0x10, 0x00, 0xF7, 0xF3, 0xF4]);
    assert_eq!((cpu.regs.ax(), cpu.regs.dx()), (0x1000, 2));
    // mov ax,-7; cwd; mov bx,2; idiv bx → -3 r -1 (truncates toward zero)
    let (cpu, _) = run(&[0xB8, 0xF9, 0xFF, 0x99, 0xBB, 0x02, 0x00, 0xF7, 0xFB, 0xF4]);
    assert_eq!((cpu.regs.ax(), cpu.regs.dx()), (0xFFFD, 0xFFFF));
    // mov ax,-128; mov bl,1; idiv bl → -128 is allowed on 286+
    let (cpu, _) = run(&[0xB8, 0x80, 0xFF, 0xB3, 0x01, 0xF6, 0xFB, 0xF4]);
    assert_eq!(cpu.regs.al(), 0x80);
}

#[test]
fn divide_error_returns_to_the_faulting_instruction() {
    // handler at 0x500:0: inc cx; mov bl,1; iret. Main: mov ax,10; mov bl,0; div bl; hlt
    let (mut cpu, mut bus) = setup(&[0xB8, 0x0A, 0x00, 0xB3, 0x00, 0xF6, 0xF3, 0xF4]);
    bus.put(0x500, 0, &[0x41, 0xB3, 0x01, 0xCF]);
    bus.set_vector(0, 0x500, 0);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.cx(), 1, "handler ran once");
    assert_eq!(cpu.regs.ax(), 0x000A, "retried DIV by 1: AL=10, AH=0");
}

#[test]
fn shifts_and_rotates() {
    // mov ax,8001h; rol ax,1 → 0003h, CF=1
    let (cpu, _) = run(&[0xB8, 0x01, 0x80, 0xD1, 0xC0, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0x0003);
    assert!(flags_set(&cpu, CF | OF));
    // clc; mov ax,1; rcr ax,1 → 0, CF=1
    let (cpu, _) = run(&[0xF8, 0xB8, 0x01, 0x00, 0xD1, 0xD8, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0);
    assert!(flags_set(&cpu, CF));
    // mov ax,F000h; mov cl,4; sar ax,cl → FF00h
    let (cpu, _) = run(&[0xB8, 0x00, 0xF0, 0xB1, 0x04, 0xD3, 0xF8, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0xFF00);
    // mov ax,8000h; mov cl,0Fh; shr ax,cl → 1
    let (cpu, _) = run(&[0xB8, 0x00, 0x80, 0xB1, 0x0F, 0xD3, 0xE8, 0xF4]);
    assert_eq!(cpu.regs.ax(), 1);
    // mov ax,1; shl ax,16 → 0, CF = old bit 0
    let (cpu, _) = run(&[0xB8, 0x01, 0x00, 0xC1, 0xE0, 0x10, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0);
    assert!(flags_set(&cpu, CF | ZF));
    // mov ax,1; shl ax,17 → 0, CF clear
    let (cpu, _) = run(&[0xB8, 0x01, 0x00, 0xC1, 0xE0, 0x11, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0);
    assert!(flags_clear(&cpu, CF));
}

#[test]
fn shift_count_is_masked_to_five_bits() {
    // stc; mov ax,1; mov cl,32; shl ax,cl → count 0: nothing changes, CF stays set
    let (cpu, _) = run(&[0xF9, 0xB8, 0x01, 0x00, 0xB1, 0x20, 0xD3, 0xE0, 0xF4]);
    assert_eq!(cpu.regs.ax(), 1);
    assert!(flags_set(&cpu, CF));
    // mov ax,1; mov cl,33; shl ax,cl → shifted by 1
    let (cpu, _) = run(&[0xB8, 0x01, 0x00, 0xB1, 0x21, 0xD3, 0xE0, 0xF4]);
    assert_eq!(cpu.regs.ax(), 2);
}

#[test]
fn bcd_adjustments() {
    // mov al,79h; add al,35h; daa → 14h, CF=1 (79 + 35 = 114 decimal)
    let (cpu, _) = run(&[0xB0, 0x79, 0x04, 0x35, 0x27, 0xF4]);
    assert_eq!(cpu.regs.al(), 0x14);
    assert!(flags_set(&cpu, CF));
    // mov al,35h; sub al,47h; das → 88h, CF=1 (35 - 47 = -12 → 88 with borrow)
    let (cpu, _) = run(&[0xB0, 0x35, 0x2C, 0x47, 0x2F, 0xF4]);
    assert_eq!(cpu.regs.al(), 0x88);
    assert!(flags_set(&cpu, CF));
    // mov ax,0009h; add al,8; aaa → AX=0107h
    let (cpu, _) = run(&[0xB8, 0x09, 0x00, 0x04, 0x08, 0x37, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0x0107);
    // mov ax,0102h; sub al,5; aas → AX=0007h
    let (cpu, _) = run(&[0xB8, 0x02, 0x01, 0x2C, 0x05, 0x3F, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0x0007);
    // mov al,63; aam → AH=6, AL=3; aad → AL=63
    let (cpu, _) = run(&[0xB0, 63, 0xD4, 0x0A, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0x0603);
    let (cpu, _) = run(&[0xB8, 0x03, 0x06, 0xD5, 0x0A, 0xF4]);
    assert_eq!(cpu.regs.ax(), 63);
}

#[test]
fn stack_push_pop_and_push_sp() {
    // mov ax,1234h; push ax; pop bx; push sp; pop cx
    let (cpu, bus) = run(&[0xB8, 0x34, 0x12, 0x50, 0x5B, 0x54, 0x59, 0xF4]);
    assert_eq!(cpu.regs.bx(), 0x1234);
    assert_eq!(cpu.regs.cx(), 0x1000, "PUSH SP pushes the old SP on 286+");
    assert_eq!(cpu.regs.sp(), 0x1000);
    assert_eq!(bus.word(STACK_SEG, 0x0FFE), 0x1000);
}

#[test]
fn pusha_popa_round_trip() {
    // mov ax..di to 1..7 (sp stays), pusha, zero them, popa
    let code = [
        0xB8, 1, 0, 0xB9, 2, 0, 0xBA, 3, 0, 0xBB, 4, 0, 0xBD, 5, 0, 0xBE, 6, 0, 0xBF, 7, 0,    // movs
        0x60, // pusha
        0x31, 0xC0, 0x31, 0xC9, 0x31, 0xD2, 0x31, 0xDB, 0x31, 0xED, 0x31, 0xF6, 0x31, 0xFF, // xors
        0x61, 0xF4, // popa; hlt
    ];
    let (cpu, bus) = run(&code);
    assert_eq!(cpu.regs.gpr, [1, 2, 3, 4, 0x1000, 5, 6, 7]);
    assert_eq!(bus.word(STACK_SEG, 0x1000 - 10), 0x1000, "saved SP");
}

#[test]
fn near_and_far_calls() {
    // call +3 (to 0006); hlt; ... at 0006: mov ax,1; ret
    let (cpu, _) = run(&[0xE8, 0x03, 0x00, 0xF4, 0x90, 0x90, 0xB8, 0x01, 0x00, 0xC3]);
    assert_eq!(cpu.regs.ax(), 1);
    assert_eq!(cpu.regs.sp(), 0x1000);
    // push 7; call far 0500:0000; hlt.  At 0500:0000: mov ax,cs; retf 2
    let (mut cpu, mut bus) = setup(&[0x6A, 0x07, 0x9A, 0x00, 0x00, 0x00, 0x05, 0xF4]);
    bus.put(0x500, 0, &[0x8C, 0xC8, 0xCA, 0x02, 0x00]);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.ax(), 0x500);
    assert_eq!(cpu.regs.cs(), CODE_SEG);
    assert_eq!(cpu.regs.sp(), 0x1000, "RETF 2 released the pushed argument");
}

#[test]
fn indirect_calls_and_jumps() {
    // mov bx,0009h; call bx; hlt; ... at 0009: mov ax,2; ret
    let (cpu, _) = run(&[0xBB, 0x09, 0x00, 0xFF, 0xD3, 0xF4, 0x90, 0x90, 0x90, 0xB8, 0x02, 0x00, 0xC3]);
    assert_eq!(cpu.regs.ax(), 2);
    // far pointer at DS:0010 → 0500:0004; jmp far [0010h]
    let (mut cpu, mut bus) = setup(&[0xFF, 0x2E, 0x10, 0x00]);
    bus.put(DATA_SEG, 0x10, &[0x04, 0x00, 0x00, 0x05]);
    bus.put(0x500, 4, &[0xB8, 0x03, 0x00, 0xF4]);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.cs(), cpu.regs.ax()), (0x500, 3));
}

#[test]
fn software_interrupt_and_iret() {
    // stc; sti; int 21h; hlt.  Handler: mov ax,cs; iret
    let (mut cpu, mut bus) = setup(&[0xF9, 0xFB, 0xCD, 0x21, 0xF4]);
    bus.put(0x600, 0, &[0x9C, 0x5A, 0x8C, 0xC8, 0xCF]); // pushf; pop dx; mov ax,cs; iret
    bus.set_vector(0x21, 0x600, 0);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.ax(), 0x600);
    assert_eq!(cpu.regs.dx() & (IF | TF), 0, "IF and TF cleared inside the handler");
    assert!(flags_set(&cpu, CF | IF), "IRET restored the caller's flags");
    assert_eq!(cpu.regs.sp(), 0x1000);
}

#[test]
fn conditional_jumps_signed_and_unsigned() {
    // mov ax,FFFFh; cmp ax,1; jl +2; mov bl,1; jb +2; mov cl,1; hlt
    // Signed, -1 < 1: JL is taken and skips "mov bl,1". Unsigned, FFFFh > 1: JB is not taken.
    let (cpu, _) = run(&[0xB8, 0xFF, 0xFF, 0x3D, 0x01, 0x00, 0x7C, 0x02, 0xB3, 0x01, 0x72, 0x02, 0xB1, 0x01, 0xF4]);
    assert_eq!(cpu.regs.bl(), 0, "JL taken");
    assert_eq!(cpu.regs.cl(), 1, "JB not taken");
}

#[test]
fn loop_and_jcxz() {
    // mov cx,5; xor ax,ax; L: inc ax; loop L; jcxz +2; mov bl,1; hlt
    let (cpu, _) = run(&[0xB9, 0x05, 0x00, 0x31, 0xC0, 0x40, 0xE2, 0xFD, 0xE3, 0x02, 0xB3, 0x01, 0xF4]);
    assert_eq!(cpu.regs.ax(), 5);
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.regs.bl(), 0, "JCXZ taken");
}

#[test]
fn rep_movsb_with_segment_override_and_cycle_count() {
    // mov si,0; mov di,100h; mov cx,4; rep movsb (DS:SI → ES:DI)
    let (mut cpu, mut bus) = setup(&[0xBE, 0x00, 0x00, 0xBF, 0x00, 0x01, 0xB9, 0x04, 0x00, 0xF3, 0xA4, 0xF4]);
    bus.put(DATA_SEG, 0, b"ABCD");
    run_to_hlt(&mut cpu, &mut bus);
    let dst = TestBus::lin(EXTRA_SEG, 0x100);
    assert_eq!(&bus.mem[dst..dst + 4], b"ABCD");
    assert_eq!((cpu.regs.si(), cpu.regs.di(), cpu.regs.cx()), (4, 0x104, 0));
    assert_eq!(cpu.cycles, 4 + 1 + 4, "3 movs + rep movsb + hlt, plus 4 repetitions");

    // cs: rep movsb copies from the code segment
    let (mut cpu, mut bus) = setup(&[0xBE, 0x00, 0x00, 0xBF, 0x00, 0x00, 0xB9, 0x02, 0x00, 0x2E, 0xF3, 0xA4, 0xF4]);
    run_to_hlt(&mut cpu, &mut bus);
    let dst = TestBus::lin(EXTRA_SEG, 0);
    assert_eq!(&bus.mem[dst..dst + 2], &[0xBE, 0x00]);
}

#[test]
fn rep_stosw_backwards() {
    // std; mov di,10h; mov ax,ABCDh; mov cx,3; rep stosw
    let (cpu, bus) = run(&[0xFD, 0xBF, 0x10, 0x00, 0xB8, 0xCD, 0xAB, 0xB9, 0x03, 0x00, 0xF3, 0xAB, 0xF4]);
    assert_eq!(cpu.regs.di(), 0x10 - 6);
    for off in [0x10, 0x0E, 0x0C] {
        assert_eq!(bus.word(EXTRA_SEG, off), 0xABCD);
    }
    assert!(flags_set(&cpu, DF));
}

#[test]
fn repe_cmpsb_stops_at_first_difference() {
    // mov si,0; mov di,0; mov cx,8; repe cmpsb
    let (mut cpu, mut bus) = setup(&[0xBE, 0x00, 0x00, 0xBF, 0x00, 0x00, 0xB9, 0x08, 0x00, 0xF3, 0xA6, 0xF4]);
    bus.put(DATA_SEG, 0, b"HELLO!!!");
    bus.put(EXTRA_SEG, 0, b"HELP!!!!");
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.si(), cpu.regs.cx()), (4, 4), "stopped after comparing 'L' with 'P'");
    assert!(flags_clear(&cpu, ZF));
}

#[test]
fn repne_scasb_finds_a_byte() {
    // mov di,0; mov al,0; mov cx,FFFFh; repne scasb  → strlen
    let (mut cpu, mut bus) = setup(&[0xBF, 0x00, 0x00, 0xB0, 0x00, 0xB9, 0xFF, 0xFF, 0xF2, 0xAE, 0xF4]);
    bus.put(EXTRA_SEG, 0, b"hello\0");
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.di(), 6);
    assert_eq!(0xFFFF - cpu.regs.cx() - 1, 5);
    assert!(flags_set(&cpu, ZF));
}

#[test]
fn addressing_modes_and_default_segments() {
    // mov bp,10h; mov si,2; mov ax,[bp+si+4] (SS default); mov bx,[si+4] (DS default); mov cx,es:[si+4]
    let (mut cpu, mut bus) =
        setup(&[0xBD, 0x10, 0x00, 0xBE, 0x02, 0x00, 0x8B, 0x42, 0x04, 0x8B, 0x5C, 0x04, 0x26, 0x8B, 0x4C, 0x04, 0xF4]);
    bus.put(STACK_SEG, 0x16, &[0x11, 0x11]);
    bus.put(DATA_SEG, 0x06, &[0x22, 0x22]);
    bus.put(EXTRA_SEG, 0x06, &[0x33, 0x33]);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.ax(), cpu.regs.bx(), cpu.regs.cx()), (0x1111, 0x2222, 0x3333));
}

#[test]
fn word_access_wraps_within_the_segment() {
    // mov bx,FFFFh; mov ax,[bx]
    let (mut cpu, mut bus) = setup(&[0xBB, 0xFF, 0xFF, 0x8B, 0x07, 0xF4]);
    bus.put(DATA_SEG, 0xFFFF, &[0x34]);
    bus.put(DATA_SEG, 0x0000, &[0x12]);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.ax(), 0x1234);
}

#[test]
fn lea_les_xlat_xchg() {
    // mov bx,100h; mov si,20h; lea ax,[bx+si+10h]
    let (cpu, _) = run(&[0xBB, 0x00, 0x01, 0xBE, 0x20, 0x00, 0x8D, 0x40, 0x10, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0x130);
    // mov bx,0; les di,[bx]
    let (mut cpu, mut bus) = setup(&[0xBB, 0x00, 0x00, 0xC4, 0x3F, 0xF4]);
    bus.put(DATA_SEG, 0, &[0x78, 0x56, 0x34, 0x12]);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.es(), cpu.regs.di()), (0x1234, 0x5678));
    // mov bx,0; mov al,2; xlat
    let (mut cpu, mut bus) = setup(&[0xBB, 0x00, 0x00, 0xB0, 0x02, 0xD7, 0xF4]);
    bus.put(DATA_SEG, 0, b"xyz");
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.al(), b'z');
    // mov ax,1; mov bx,2; xchg ax,bx
    let (cpu, _) = run(&[0xB8, 0x01, 0x00, 0xBB, 0x02, 0x00, 0x93, 0xF4]);
    assert_eq!((cpu.regs.ax(), cpu.regs.bx()), (2, 1));
}

#[test]
fn enter_and_leave() {
    // mov bp,1234h; enter 4,0; mov ax,bp; leave
    let (cpu, bus) = run(&[0xBD, 0x34, 0x12, 0xC8, 0x04, 0x00, 0x00, 0x8B, 0xC5, 0xC9, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0x0FFE, "BP pointed at the saved BP");
    assert_eq!(cpu.regs.bp(), 0x1234);
    assert_eq!(cpu.regs.sp(), 0x1000);
    assert_eq!(bus.word(STACK_SEG, 0x0FFE), 0x1234);
}

#[test]
fn bound_raises_interrupt_5_when_out_of_range() {
    // mov bx,0; mov ax,20; bound ax,[bx] with bounds 0..10 → INT 5 handler sets ax=5, iret, retry passes
    let (mut cpu, mut bus) = setup(&[0xBB, 0x00, 0x00, 0xB8, 0x14, 0x00, 0x62, 0x07, 0xF4]);
    bus.put(DATA_SEG, 0, &[0x00, 0x00, 0x0A, 0x00]);
    bus.put(0x500, 0, &[0xB8, 0x05, 0x00, 0xCF]);
    bus.set_vector(5, 0x500, 0);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.ax(), 5);
}

#[test]
fn port_io() {
    // in al,60h; mov dx,3C9h; out dx,al; mov ax,1234h; out 40h,ax
    let (_, bus) = run(&[0xE4, 0x60, 0xBA, 0xC9, 0x03, 0xEE, 0xB8, 0x34, 0x12, 0xE7, 0x40, 0xF4]);
    assert_eq!(bus.port_reads, vec![0x60]);
    assert_eq!(bus.port_writes, vec![(0x3C9, 0x60 ^ 0x5A), (0x40, 0x34), (0x41, 0x12)]);
}

#[test]
fn flags_register_behaves_like_a_286() {
    // pushf; pop ax; or ax,F028h; push ax; popf; pushf; pop bx
    let (cpu, _) = run_model(&[0x9C, 0x58, 0x0D, 0x28, 0xF0, 0x50, 0x9D, 0x9C, 0x5B, 0xF4], Model::I286);
    assert_eq!(cpu.regs.bx() & 0xF028, 0, "bits 3, 5 and 12-15 read as 0");
    assert_eq!(cpu.regs.bx() & 0x0002, 0x0002, "bit 1 reads as 1");
    // mov ah,FFh; sahf; mov ah,0; lahf
    let (cpu, _) = run(&[0xB4, 0xFF, 0x9E, 0xB4, 0x00, 0x9F, 0xF4]);
    assert_eq!(cpu.regs.ah(), 0xD7, "SF ZF AF PF CF + fixed bit 1");
}

#[test]
fn sign_extension() {
    let (cpu, _) = run(&[0xB0, 0x80, 0x98, 0x99, 0xF4]); // mov al,80h; cbw; cwd
    assert_eq!((cpu.regs.dx(), cpu.regs.ax()), (0xFFFF, 0xFF80));
}

#[test]
fn host_callback_exits_with_its_id() {
    let (mut cpu, mut bus) = setup(&[0xFE, 0x38, 0x34, 0x12, 0xF4]);
    assert_eq!(cpu.step(&mut bus), Err(Exit::Callback(0x1234)));
    assert_eq!(cpu.regs.ip, 4);
    assert_eq!(cpu.step(&mut bus), Err(Exit::Halt));
    assert_eq!(cpu.regs.ip, 5);
}

#[test]
fn unimplemented_instruction_changes_nothing() {
    // es: 0F FF (UD0, not implemented)
    let (mut cpu, mut bus) = setup(&[0x26, 0x0F, 0xFF, 0xE0, 0xF4]);
    let before = cpu.clone();
    let exit = cpu.step(&mut bus).unwrap_err();
    assert_eq!(exit, Exit::Unimplemented { cs: CODE_SEG, ip: 0, bytes: [0x26, 0x0F, 0xFF, 0xE0, 0xF4, 0x00] });
    assert_eq!(cpu, before);
    assert!(exit.to_string().contains("1000:0000"));
}

#[test]
fn interrupts_are_inhibited_after_sti_and_mov_ss() {
    let (mut cpu, mut bus) = setup(&[0xFB, 0x90, 0x8E, 0xD0, 0x90, 0xF4]); // sti; nop; mov ss,ax; nop
    assert!(!cpu.interrupts_enabled());
    cpu.step(&mut bus).unwrap(); // sti
    assert!(!cpu.interrupts_enabled(), "not before the instruction after STI");
    cpu.step(&mut bus).unwrap(); // nop
    assert!(cpu.interrupts_enabled());
    cpu.step(&mut bus).unwrap(); // mov ss,ax
    assert!(!cpu.interrupts_enabled());
    cpu.step(&mut bus).unwrap(); // nop
    assert!(cpu.interrupts_enabled());
}

#[test]
fn hardware_interrupt_entry() {
    let (mut cpu, mut bus) = setup(&[0xFB, 0x90, 0x90, 0xF4]);
    bus.put(0x700, 0, &[0xB8, 0x08, 0x00, 0xCF]); // mov ax,8; iret
    bus.set_vector(8, 0x700, 0);
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    assert!(cpu.interrupts_enabled());
    cpu.interrupt(&mut bus, 8);
    assert_eq!((cpu.regs.cs(), cpu.regs.ip), (0x700, 0));
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.ax(), 8);
    assert_eq!(cpu.regs.ip, 4, "returned to the interrupted instruction stream");
}

#[test]
fn trap_flag_single_steps() {
    // pushf; pop ax; or ax,100h; push ax; popf; nop; hlt. INT 1 handler: inc bx; iret
    let (mut cpu, mut bus) = setup(&[0x9C, 0x58, 0x0D, 0x00, 0x01, 0x50, 0x9D, 0x90, 0xF4]);
    bus.put(0x500, 0, &[0x43, 0xCF]);
    bus.set_vector(1, 0x500, 0);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.bx(), 1, "one trap: after the NOP following POPF");
}

#[test]
fn snapshots_are_plain_clones() {
    let (mut cpu, mut bus) = setup(&[0x40, 0x40, 0x40, 0xF4]);
    cpu.step(&mut bus).unwrap();
    let snapshot = cpu.clone();
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.ax(), 3);
    let mut replay = snapshot;
    run_to_hlt(&mut replay, &mut bus);
    assert_eq!(replay, cpu);
    assert_eq!(cpu.regs.seg[seg::CS], CODE_SEG);
}
