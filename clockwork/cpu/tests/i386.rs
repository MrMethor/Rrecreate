//! 80386 real-mode tests: 32-bit operands and addressing, FS/GS, two-byte opcodes, CPU detection.
//! Code is hand-encoded; each byte group is one instruction.

mod common;

use common::*;
use cpu::flags::{CF, OF, ZF};
use cpu::{Exit, Model, seg};

#[test]
fn alu_on_32_bit_registers() {
    // mov eax,12345678h; add eax,EDCBA988h → 1_0000_0000h: EAX=0, CF and ZF set
    let (cpu, _) = run(&[0x66, 0xB8, 0x78, 0x56, 0x34, 0x12, 0x66, 0x05, 0x88, 0xA9, 0xCB, 0xED, 0xF4]);
    assert_eq!(cpu.regs.eax(), 0);
    assert!(flags_set(&cpu, CF | ZF));
    // mov eax,12345678h; mov ax,ABCDh: a 16-bit write keeps the high half
    let (cpu, _) = run(&[0x66, 0xB8, 0x78, 0x56, 0x34, 0x12, 0xB8, 0xCD, 0xAB, 0xF4]);
    assert_eq!(cpu.regs.eax(), 0x1234_ABCD);
}

#[test]
fn dword_memory_and_sib_addressing() {
    // mov eax,DEADBEEFh; mov [10h],eax; mov ebx,4; mov ecx,3; mov edx,[ebx+ecx*4]
    let (cpu, bus) = run(&[
        0x66, 0xB8, 0xEF, 0xBE, 0xAD, 0xDE, 0x66, 0xA3, 0x10, 0x00, 0x66, 0xBB, 0x04, 0x00, 0x00, 0x00, 0x66, 0xB9,
        0x03, 0x00, 0x00, 0x00, 0x67, 0x66, 0x8B, 0x14, 0x8B, 0xF4,
    ]);
    let at = TestBus::lin(DATA_SEG, 0x10);
    assert_eq!(&bus.mem[at..at + 4], &[0xEF, 0xBE, 0xAD, 0xDE]);
    assert_eq!(cpu.regs.edx(), 0xDEAD_BEEF);
}

#[test]
fn ebp_based_32_bit_addressing_defaults_to_ss() {
    // mov ebp,100h; mov ax,[ebp+8]
    let (mut cpu, mut bus) = setup(&[0x66, 0xBD, 0x00, 0x01, 0x00, 0x00, 0x67, 0x8B, 0x45, 0x08, 0xF4]);
    bus.put(STACK_SEG, 0x108, &[0x11, 0x22]);
    bus.put(DATA_SEG, 0x108, &[0x33, 0x44]);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.ax(), 0x2211);
}

#[test]
fn movzx_movsx_and_setcc() {
    // mov bl,80h; movzx eax,bl; movsx ecx,bl; cmp al,80h; sete dl; setne dh
    let (cpu, _) = run(&[
        0xB3, 0x80, 0x66, 0x0F, 0xB6, 0xC3, 0x66, 0x0F, 0xBE, 0xCB, 0x3C, 0x80, 0x0F, 0x94, 0xC2, 0x0F, 0x95, 0xC6,
        0xF4,
    ]);
    assert_eq!(cpu.regs.eax(), 0x80);
    assert_eq!(cpu.regs.ecx(), 0xFFFF_FF80);
    assert_eq!((cpu.regs.dl(), cpu.regs.dh()), (1, 0));
}

#[test]
fn near_conditional_jump() {
    // xor ax,ax; jz +3 (0F 84 rel16); mov bl,1; nop; hlt
    let (cpu, _) = run(&[0x31, 0xC0, 0x0F, 0x84, 0x03, 0x00, 0xB3, 0x01, 0x90, 0xF4]);
    assert_eq!(cpu.regs.bl(), 0, "jump taken over mov bl,1");
}

#[test]
fn bit_test_instructions() {
    // mov ax,5; mov bx,20h; bt bx,ax; setc cl; bts ax,0; setc ch; btr ax,2; btc ax,3
    let (cpu, _) = run(&[
        0xB8, 0x05, 0x00, 0xBB, 0x20, 0x00, 0x0F, 0xA3, 0xC3, 0x0F, 0x92, 0xC1, 0x0F, 0xBA, 0xE8, 0x00, 0x0F, 0x92,
        0xC5, 0x0F, 0xBA, 0xF0, 0x02, 0x0F, 0xBA, 0xF8, 0x03, 0xF4,
    ]);
    assert_eq!(cpu.regs.cl(), 1, "BT found bit 5 of BX");
    assert_eq!(cpu.regs.ch(), 1, "BTS reported bit 0 was already set");
    assert_eq!(cpu.regs.ax(), 0x0009, "5 → BTR bit 2 → 1 → BTC bit 3 → 9");
    assert!(flags_clear(&cpu, CF), "bit 3 was clear before BTC");
}

#[test]
fn bit_test_on_memory_reaches_past_the_operand() {
    // mov ax,17; bts [0],ax → word at 2, bit 1. mov ax,-1; bts [0],ax → word at -2 (FFFEh), bit 15
    let (_, bus) =
        run(&[0xB8, 0x11, 0x00, 0x0F, 0xAB, 0x06, 0x00, 0x00, 0xB8, 0xFF, 0xFF, 0x0F, 0xAB, 0x06, 0x00, 0x00, 0xF4]);
    assert_eq!(bus.mem[TestBus::lin(DATA_SEG, 2)], 0x02);
    assert_eq!(bus.mem[TestBus::lin(DATA_SEG, 0xFFFF)], 0x80);
}

#[test]
fn bit_scans() {
    // mov bx,0118h; bsf ax,bx; bsr cx,bx; xor dx,dx; bsf dx,dx (ZF set, DX unchanged)
    let (cpu, _) = run(&[0xBB, 0x18, 0x01, 0x0F, 0xBC, 0xC3, 0x0F, 0xBD, 0xCB, 0x31, 0xD2, 0x0F, 0xBC, 0xD2, 0xF4]);
    assert_eq!((cpu.regs.ax(), cpu.regs.cx()), (3, 8));
    assert!(flags_set(&cpu, ZF));
}

#[test]
fn double_precision_shifts() {
    // mov eax,F0000000h; mov edx,A0000000h; shld eax,edx,4 → 0000000Ah, CF = old bit 28
    let (cpu, _) = run(&[
        0x66, 0xB8, 0x00, 0x00, 0x00, 0xF0, 0x66, 0xBA, 0x00, 0x00, 0x00, 0xA0, 0x66, 0x0F, 0xA4, 0xD0, 0x04, 0xF4,
    ]);
    assert_eq!(cpu.regs.eax(), 0x0000_000A);
    assert!(flags_set(&cpu, CF));
    // mov eax,0Fh; mov edx,5; mov cl,4; shrd eax,edx,cl → 50000000h, CF = old bit 3
    let (cpu, _) = run(&[
        0x66, 0xB8, 0x0F, 0x00, 0x00, 0x00, 0x66, 0xBA, 0x05, 0x00, 0x00, 0x00, 0xB1, 0x04, 0x66, 0x0F, 0xAD, 0xD0,
        0xF4,
    ]);
    assert_eq!(cpu.regs.eax(), 0x5000_0000);
    assert!(flags_set(&cpu, CF));
}

#[test]
fn multiply_and_divide_32_bit() {
    // mov eax,10000h; mov ebx,10000h; mul ebx → EDX:EAX = 1_0000_0000h; div ebx → 10000h r 0
    let mul = [0x66, 0xB8, 0x00, 0x00, 0x01, 0x00, 0x66, 0xBB, 0x00, 0x00, 0x01, 0x00, 0x66, 0xF7, 0xE3];
    let mut code = mul.to_vec();
    code.push(0xF4);
    let (cpu, _) = run(&code);
    assert_eq!((cpu.regs.edx(), cpu.regs.eax()), (1, 0));
    assert!(flags_set(&cpu, CF | OF));
    let mut code = mul.to_vec();
    code.extend([0x66, 0xF7, 0xF3, 0xF4]);
    let (cpu, _) = run(&code);
    assert_eq!((cpu.regs.eax(), cpu.regs.edx()), (0x1_0000, 0));

    // mov eax,-7; cdq; mov ebx,2; idiv ebx → -3 r -1
    let (cpu, _) = run(&[
        0x66, 0xB8, 0xF9, 0xFF, 0xFF, 0xFF, 0x66, 0x99, 0x66, 0xBB, 0x02, 0x00, 0x00, 0x00, 0x66, 0xF7, 0xFB, 0xF4,
    ]);
    assert_eq!((cpu.regs.eax() as i32, cpu.regs.edx() as i32), (-3, -1));

    // mov eax,-2; mov ebx,3; imul eax,ebx → -6
    let (cpu, _) =
        run(&[0x66, 0xB8, 0xFE, 0xFF, 0xFF, 0xFF, 0x66, 0xBB, 0x03, 0x00, 0x00, 0x00, 0x66, 0x0F, 0xAF, 0xC3, 0xF4]);
    assert_eq!(cpu.regs.eax() as i32, -6);
    assert!(flags_clear(&cpu, CF | OF));
}

#[test]
fn stack_with_32_bit_operands() {
    // mov eax,11223344h; push eax; pop bx; pop cx; push eax; pop ebx; pushfd; pop eax; pushad; popad
    let (cpu, _) = run(&[
        0x66, 0xB8, 0x44, 0x33, 0x22, 0x11, 0x66, 0x50, 0x5B, 0x59, 0x66, 0x50, 0x66, 0x5B, 0x66, 0x9C, 0x66, 0x58,
        0x66, 0x60, 0x66, 0x61, 0xF4,
    ]);
    assert_eq!((cpu.regs.bx(), cpu.regs.cx()), (0x3344, 0x1122), "PUSH EAX wrote 4 bytes");
    assert_eq!(cpu.regs.ebx(), 0x1122_3344);
    assert_eq!(cpu.regs.sp(), 0x1000);
    assert_eq!(cpu.regs.eax() >> 16, 0, "EFLAGS high half is 0 in real mode");
}

#[test]
fn fs_and_gs() {
    // mov ax,1234h; mov fs,ax; push fs; pop gs; mov ax,fs:[5]; mov cx,fs
    let (mut cpu, mut bus) =
        setup(&[0xB8, 0x34, 0x12, 0x8E, 0xE0, 0x0F, 0xA0, 0x0F, 0xA9, 0x64, 0xA1, 0x05, 0x00, 0x8C, 0xE1, 0xF4]);
    bus.put(0x1234, 5, &[0xAB, 0xCD]);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.ax(), cpu.regs.cx(), cpu.regs.gs()), (0xCDAB, 0x1234, 0x1234));
    assert_eq!(cpu.regs.seg[seg::FS], 0x1234);
}

#[test]
fn lfs_loads_a_far_pointer() {
    // mov bx,0; lfs di,[bx]
    let (mut cpu, mut bus) = setup(&[0xBB, 0x00, 0x00, 0x0F, 0xB4, 0x3F, 0xF4]);
    bus.put(DATA_SEG, 0, &[0x78, 0x56, 0x34, 0x12]);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.fs(), cpu.regs.di()), (0x1234, 0x5678));
}

#[test]
fn cpu_detection_sees_a_386() {
    // pushf; pop ax; or ax,7000h; push ax; popf; pushf; pop bx → IOPL/NT stick on a 386
    let code = [0x9C, 0x58, 0x0D, 0x00, 0x70, 0x50, 0x9D, 0x9C, 0x5B, 0xF4];
    let (cpu, _) = run(&code);
    assert_eq!(cpu.regs.bx() & 0xF000, 0x7000);
    let (cpu, _) = run_model(&code, Model::I286);
    assert_eq!(cpu.regs.bx() & 0xF000, 0, "a 286 clears bits 12-15 in real mode");

    // pushfd; pop eax; xor eax,40000h; push eax; popfd; pushfd; pop ebx → AC can't be toggled (not a 486)
    let (cpu, _) = run(&[
        0x66, 0x9C, 0x66, 0x58, 0x66, 0x35, 0x00, 0x00, 0x04, 0x00, 0x66, 0x50, 0x66, 0x9D, 0x66, 0x9C, 0x66, 0x5B,
        0xF4,
    ]);
    assert_eq!(cpu.regs.ebx() & 0x4_0000, 0);
}

#[test]
fn control_register_reads_and_the_286_msw() {
    // mov eax,-1; mov eax,cr0 → 0; smsw bx → 0
    let (cpu, _) = run(&[0x66, 0xB8, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F, 0x20, 0xC0, 0x0F, 0x01, 0xE3, 0xF4]);
    assert_eq!((cpu.regs.eax(), cpu.regs.bx()), (0, 0));
    let (cpu, _) = run_model(&[0x0F, 0x01, 0xE3, 0xF4], Model::I286);
    assert_eq!(cpu.regs.bx(), 0xFFF0, "the 286 machine status word reads with its top bits set");
    // An 80286 entering protected mode (mov ax,1; lmsw ax) stops the run.
    let (mut cpu, mut bus) = setup_model(&[0xB8, 0x01, 0x00, 0x0F, 0x01, 0xF0], Model::I286);
    cpu.step(&mut bus).unwrap();
    let exit = cpu.step(&mut bus).unwrap_err();
    assert!(matches!(exit, Exit::Unsupported { ip: 3, .. }), "{exit}");
}

#[test]
fn string_instructions_with_dwords_and_32_bit_addressing() {
    // mov si,0; mov di,100h; mov cx,2; rep movsd
    let (mut cpu, mut bus) = setup(&[0xBE, 0x00, 0x00, 0xBF, 0x00, 0x01, 0xB9, 0x02, 0x00, 0x66, 0xF3, 0xA5, 0xF4]);
    bus.put(DATA_SEG, 0, b"ABCDEFGH");
    run_to_hlt(&mut cpu, &mut bus);
    let dst = TestBus::lin(EXTRA_SEG, 0x100);
    assert_eq!(&bus.mem[dst..dst + 8], b"ABCDEFGH");
    assert_eq!((cpu.regs.si(), cpu.regs.di()), (8, 0x108));

    // mov esi,0; mov edi,200h; mov ecx,3; a32 rep movsb
    let (mut cpu, mut bus) = setup(&[
        0x66, 0xBE, 0x00, 0x00, 0x00, 0x00, 0x66, 0xBF, 0x00, 0x02, 0x00, 0x00, 0x66, 0xB9, 0x03, 0x00, 0x00, 0x00,
        0x67, 0xF3, 0xA4, 0xF4,
    ]);
    bus.put(DATA_SEG, 0, b"xyz");
    run_to_hlt(&mut cpu, &mut bus);
    let dst = TestBus::lin(EXTRA_SEG, 0x200);
    assert_eq!(&bus.mem[dst..dst + 3], b"xyz");
    assert_eq!((cpu.regs.esi(), cpu.regs.edi(), cpu.regs.ecx()), (3, 0x203, 0));
}

#[test]
fn loop_counter_follows_the_address_size() {
    // mov ecx,10000h; L: inc eax; a32 loop L → 65536 iterations
    let (cpu, _) = run(&[0x66, 0xB9, 0x00, 0x00, 0x01, 0x00, 0x66, 0x40, 0x67, 0xE2, 0xFB, 0xF4]);
    assert_eq!((cpu.regs.eax(), cpu.regs.ecx()), (0x1_0000, 0));
    // mov ecx,10002h; L: inc ax; loop L → only CX counts: 2 iterations
    let (cpu, _) = run(&[0x66, 0xB9, 0x02, 0x00, 0x01, 0x00, 0x40, 0xE2, 0xFD, 0xF4]);
    assert_eq!((cpu.regs.ax(), cpu.regs.ecx()), (2, 0x1_0000));
}

#[test]
fn sign_extension_to_32_bits() {
    // mov ax,8000h; cwde; cdq
    let (cpu, _) = run(&[0xB8, 0x00, 0x80, 0x66, 0x98, 0x66, 0x99, 0xF4]);
    assert_eq!((cpu.regs.eax(), cpu.regs.edx()), (0xFFFF_8000, 0xFFFF_FFFF));
}

#[test]
fn call_and_ret_with_32_bit_operands() {
    // 0: call +3 (66 E8 rel32) → 9; 6: hlt; 9: mov eax,1; 15: ret (66 C3)
    let (cpu, bus) =
        run(&[0x66, 0xE8, 0x03, 0x00, 0x00, 0x00, 0xF4, 0x90, 0x90, 0x66, 0xB8, 0x01, 0x00, 0x00, 0x00, 0x66, 0xC3]);
    assert_eq!(cpu.regs.eax(), 1);
    assert_eq!(cpu.regs.sp(), 0x1000);
    assert_eq!((bus.word(STACK_SEG, 0x0FFC), bus.word(STACK_SEG, 0x0FFE)), (6, 0), "4-byte return address");
}

#[test]
fn shifts_on_32_bit_operands() {
    // mov eax,1; shl eax,31; sar eax,1
    let (cpu, _) = run(&[0x66, 0xB8, 0x01, 0x00, 0x00, 0x00, 0x66, 0xC1, 0xE0, 0x1F, 0x66, 0xD1, 0xF8, 0xF4]);
    assert_eq!(cpu.regs.eax(), 0xC000_0000);
}

#[test]
fn dword_port_input() {
    // in eax,40h → four byte reads
    let (cpu, bus) = run(&[0x66, 0xE5, 0x40, 0xF4]);
    assert_eq!(bus.port_reads, vec![0x40, 0x41, 0x42, 0x43]);
    assert_eq!(cpu.regs.eax().to_le_bytes(), [0x40 ^ 0x5A, 0x41 ^ 0x5A, 0x42 ^ 0x5A, 0x43 ^ 0x5A]);
}

#[test]
fn a_286_rejects_386_instructions() {
    for code in [&[0x66, 0x40, 0xF4][..], &[0x0F, 0xB6, 0xC3, 0xF4], &[0x64, 0xA1, 0x00, 0x00, 0xF4]] {
        let (mut cpu, mut bus) = setup_model(code, Model::I286);
        assert!(matches!(cpu.step(&mut bus), Err(Exit::Unimplemented { ip: 0, .. })), "{code:02X?}");
    }
}
