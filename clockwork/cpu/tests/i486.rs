//! The 80486 additions (`Model::I486`): BSWAP, XADD, CMPXCHG, INVD/WBINVD and the AC flag.

mod common;

use common::*;
use cpu::flags::{CF, ZF};
use cpu::{Exit, Model};

fn run486(code: &[u8]) -> (cpu::Cpu, TestBus) {
    run_model(code, Model::I486)
}

#[test]
fn bswap_reverses_a_32_bit_register_and_clears_a_16_bit_one() {
    // mov eax,11223344h; bswap eax; mov ecx,0AABBh; bswap cx (16-bit: cleared, as in DOSBox)
    let code = [0x66, 0xB8, 0x44, 0x33, 0x22, 0x11, 0x66, 0x0F, 0xC8, 0x66, 0xB9, 0xBB, 0xAA, 0, 0, 0x0F, 0xC9, 0xF4];
    let (cpu, _) = run486(&code);
    assert_eq!(cpu.regs.eax(), 0x4433_2211);
    assert_eq!(cpu.regs.ecx(), 0x0000_0000);
}

#[test]
fn xadd_exchanges_and_adds_with_add_flags() {
    // mov ax,0FFFFh; mov bx,1; xadd ax,bx: AX = 0 (CF, ZF), BX = FFFFh
    let (cpu, _) = run486(&[0xB8, 0xFF, 0xFF, 0xBB, 0x01, 0x00, 0x0F, 0xC1, 0xD8, 0xF4]);
    assert_eq!((cpu.regs.ax(), cpu.regs.bx()), (0, 0xFFFF));
    assert!(flags_set(&cpu, CF | ZF));
    // mov al,5; xadd al,al: 10 (the destination is written last)
    let (cpu, _) = run486(&[0xB0, 0x05, 0x0F, 0xC0, 0xC0, 0xF4]);
    assert_eq!(cpu.regs.al(), 10);
}

#[test]
fn cmpxchg_stores_on_a_match_and_loads_otherwise() {
    // [0200]=1234h; mov ax,1234h; mov bx,5678h; cmpxchg [0200],bx: stored, ZF
    let code = [0xB8, 0x34, 0x12, 0xBB, 0x78, 0x56, 0x0F, 0xB1, 0x1E, 0x00, 0x02, 0xF4];
    let (mut cpu, mut bus) = setup_model(&code, Model::I486);
    bus.put(DATA_SEG, 0x200, &[0x34, 0x12]);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!(bus.word(DATA_SEG, 0x200), 0x5678);
    assert!(flags_set(&cpu, ZF));
    // With AX=1000h the destination is loaded into AX; flags as CMP AX,dest (borrow: CF).
    let code = [0xB8, 0x00, 0x10, 0xBB, 0x78, 0x56, 0x0F, 0xB1, 0x1E, 0x00, 0x02, 0xF4];
    let (mut cpu, mut bus) = setup_model(&code, Model::I486);
    bus.put(DATA_SEG, 0x200, &[0x34, 0x12]);
    run_to_hlt(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.ax(), bus.word(DATA_SEG, 0x200)), (0x1234, 0x1234));
    assert!(flags_clear(&cpu, ZF) && flags_set(&cpu, CF));
}

#[test]
fn the_ac_flag_tells_a_486_from_a_386() {
    // pushfd; pop eax; or eax,40000h; push eax; popfd; pushfd; pop eax
    let code = [
        0x66, 0x9C, 0x66, 0x58, 0x66, 0x0D, 0x00, 0x00, 0x04, 0x00, 0x66, 0x50, 0x66, 0x9D, 0x66, 0x9C, 0x66, 0x58,
        0xF4,
    ];
    let (cpu, _) = run486(&code);
    assert_ne!(cpu.regs.eax() & 0x4_0000, 0, "AC sticks on a 486");
    let (cpu, _) = run_model(&code, Model::I386);
    assert_eq!(cpu.regs.eax() & 0x4_0000, 0, "but not on a 386");
}

#[test]
fn the_386_lacks_them() {
    let (mut cpu, mut bus) = setup_model(&[0x0F, 0xC8, 0xF4], Model::I386); // bswap eax
    assert!(matches!(cpu.step(&mut bus), Err(Exit::Unimplemented { .. })));
    // INVD and WBINVD do nothing on a 486 in real mode.
    let (cpu, _) = run486(&[0x0F, 0x08, 0x0F, 0x09, 0xF4]);
    assert_eq!(cpu.regs.ip, 5);
}
