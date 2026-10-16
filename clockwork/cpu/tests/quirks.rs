//! Behaviors found by running the SingleStepTests hardware recordings (see
//! `examples/singlestep.rs`), each pinned with one hand-encoded case taken from the recordings.

mod common;

use common::*;
use cpu::Model;

/// The #UD handler at 1000:0100 writes 1234h to DS:0080 and halts.
fn with_ud_handler(code: &[u8]) -> TestBus {
    let (mut cpu, mut bus) = setup(code);
    bus.put(CODE_SEG, 0x100, &[0xC7, 0x06, 0x80, 0x00, 0x34, 0x12, 0xF4]);
    bus.set_vector(6, CODE_SEG, 0x100);
    run_to_hlt(&mut cpu, &mut bus);
    bus
}

#[test]
fn lock_on_a_register_destination_is_invalid() {
    let bus = with_ud_handler(&[0xF0, 0x01, 0xC0, 0xF4]); // lock add ax,ax
    assert_eq!(bus.word(DATA_SEG, 0x80), 0x1234);
    let bus = with_ud_handler(&[0xF0, 0x01, 0x06, 0x00, 0x02, 0xF4]); // lock add [0200],ax
    assert_eq!(bus.word(DATA_SEG, 0x80), 0, "a memory destination may be locked");
}

#[test]
fn register_forms_of_memory_instructions_are_invalid() {
    for code in [
        &[0x8D, 0xC0, 0xF4][..],   // lea ax,ax
        &[0xC4, 0xC0, 0xF4],       // les ax,ax
        &[0x0F, 0xB2, 0xC0, 0xF4], // lss ax,ax
        &[0xC7, 0xC8, 0, 0, 0xF4], // mov r/m,imm with reg field 1
        &[0x8E, 0xC8, 0xF4],       // mov cs,ax
        &[0xFF, 0xD8, 0xF4],       // call far ax
    ] {
        assert_eq!(with_ud_handler(code).word(DATA_SEG, 0x80), 0x1234, "{code:02X?}");
    }
}

#[test]
fn sixteen_bit_shld_past_16_shifts_through_the_source_twice() {
    // shld ax,bx,20 with AX=950Ah, BX=0001h gives 0010h on a 386 (dest:src:src).
    let (cpu, _) = run(&[0xB8, 0x0A, 0x95, 0xBB, 0x01, 0x00, 0x0F, 0xA4, 0xD8, 0x14, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0x0010);
    // shld ax,bx,25 with AX=9474h, BX=1140h: 8022h.
    let (cpu, _) = run(&[0xB8, 0x74, 0x94, 0xBB, 0x40, 0x11, 0x0F, 0xA4, 0xD8, 0x19, 0xF4]);
    assert_eq!(cpu.regs.ax(), 0x8022);
}

#[test]
fn sib_without_an_index_scales_the_base() {
    // mov ebp,10h; mov ax,[ss:ebp*4+78h] (SIB A5: scale 4, no index, base EBP) reads SS:00B8.
    let code = [0x66, 0xBD, 0x10, 0, 0, 0, 0x67, 0x8B, 0x44, 0xA5, 0x78, 0xF4];
    let (mut cpu, mut bus) = setup(&code);
    bus.put(STACK_SEG, 0x88, &[0xAD, 0xDE]);
    bus.put(STACK_SEG, 0xB8, &[0xEF, 0xBE]);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.ax(), 0xBEEF);
}

#[test]
fn popad_on_a_16_bit_stack_keeps_the_saved_esp_high_half() {
    let (mut cpu, mut bus) = setup(&[0x66, 0x61, 0xF4]);
    cpu.regs.set_sp(0x0FE0);
    let mut image = [0u8; 32];
    image[12..16].copy_from_slice(&0x5A04_0000u32.to_le_bytes()); // the saved ESP
    bus.put(STACK_SEG, 0x0FE0, &image);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.gpr[cpu::reg::SP], 0x5A04_1000);
}

#[test]
fn byte_shifts_past_the_width() {
    // shr al,cl with AL=A9h, CL=24: zero, and on a 386 CF is bit 7 (as for a shift by 8).
    let code = [0xB0, 0xA9, 0xB1, 0x18, 0xD2, 0xE8, 0xF4];
    let (cpu, _) = run(&code);
    assert_eq!((cpu.regs.al(), cpu.regs.flags & 1), (0, 1));
    let (cpu, _) = run_model(&code, Model::I286);
    assert_eq!((cpu.regs.al(), cpu.regs.flags & 1), (0, 0), "the 286 clears CF");
    // CL=22: CF clear on both.
    let (cpu, _) = run(&[0xB0, 0xA9, 0xB1, 0x16, 0xD2, 0xE8, 0xF4]);
    assert_eq!(cpu.regs.flags & 1, 0);
}

#[test]
fn rcr_by_a_multiple_of_nine_still_sets_of() {
    // rcr al,cl with CL=9 moves nothing, but OF becomes bit 7 ^ bit 6 of AL: 1 for 80h, 0 for C0h.
    let (cpu, _) = run(&[0xF8, 0xB0, 0x80, 0xB1, 0x09, 0xD2, 0xD8, 0xF4]);
    assert_eq!((cpu.regs.al(), cpu.regs.flags & 0x800), (0x80, 0x800));
    let (cpu, _) = run(&[0xF8, 0xB0, 0xC0, 0xB1, 0x09, 0xD2, 0xD8, 0xF4]);
    assert_eq!(cpu.regs.flags & 0x800, 0);
}

#[test]
fn the_vector_is_read_before_the_frame_overwrites_it() {
    // SS:SP = 0000:0008, so the divide error's frame lands on vector 0's own segment word.
    let code = [0x31, 0xC0, 0x8E, 0xD0, 0xBC, 0x08, 0x00, 0xB1, 0x00, 0xF6, 0xF1, 0xF4];
    let (mut cpu, mut bus) = setup(&code);
    bus.put(CODE_SEG, 0x100, &[0xC7, 0x06, 0x80, 0x00, 0x34, 0x12, 0xF4]);
    bus.set_vector(0, CODE_SEG, 0x100);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!(bus.word(DATA_SEG, 0x80), 0x1234, "the handler at 1000:0100 ran");
}
