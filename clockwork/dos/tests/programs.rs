//! End-to-end tests: small DOS programs, assembled here with iced-x86, run on the emulated PC.

use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

use dos::{Config, LoadError, Machine, Stop, Vfs};
use iced_x86::BlockEncoderOptions;
use iced_x86::code_asm::*;

const SECOND: u64 = 8_000_000; // cycles per emulated second at the default speed

/// iced's label memory operands are RIP-relative (64-bit only), so 16-bit programs refer to
/// data by absolute offset: the program is assembled twice, the second pass using the label
/// addresses found by the first. Every address-dependent operand must keep its encoded size
/// between passes (true for `mov r16, imm16` and `[disp16]`).
struct Asm {
    a: CodeAssembler,
    labels: BTreeMap<&'static str, CodeLabel>,
    known: BTreeMap<&'static str, u64>,
}

impl Asm {
    /// A label for jumps and calls; created on first use.
    fn l(&mut self, name: &'static str) -> CodeLabel {
        if let Some(&label) = self.labels.get(name) {
            return label;
        }
        let label = self.a.create_label();
        self.labels.insert(name, label);
        label
    }

    /// Places label `name` here.
    fn here(&mut self, name: &'static str) -> Result<(), IcedError> {
        let mut label = self.l(name);
        self.a.set_label(&mut label)?;
        self.labels.insert(name, label);
        Ok(())
    }

    /// The label's offset (0 during the first pass).
    fn addr(&self, name: &str) -> i32 {
        self.known.get(name).map_or(0, |&ip| ip as i32)
    }
}

impl Deref for Asm {
    type Target = CodeAssembler;
    fn deref(&self) -> &CodeAssembler {
        &self.a
    }
}

impl DerefMut for Asm {
    fn deref_mut(&mut self) -> &mut CodeAssembler {
        &mut self.a
    }
}

/// Assembles a COM program (origin 100h).
fn com(build: impl Fn(&mut Asm) -> Result<(), IcedError>) -> Vec<u8> {
    let mut known = BTreeMap::new();
    for pass in 0..2 {
        let mut asm = Asm { a: CodeAssembler::new(16).unwrap(), labels: BTreeMap::new(), known: known.clone() };
        build(&mut asm).unwrap();
        let labels = asm.labels.clone();
        let result = asm.a.assemble_options(0x100, BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS).unwrap();
        let found: BTreeMap<_, _> =
            labels.iter().map(|(&name, label)| (name, result.label_ip(label).unwrap())).collect();
        if pass == 1 {
            assert_eq!(found, known, "label addresses moved between passes");
            return result.inner.code_buffer;
        }
        known = found;
    }
    unreachable!()
}

fn boot(files: &[(&str, &[u8])], program: &str) -> Machine {
    let mut drive = Vfs::new();
    for (path, data) in files {
        drive.insert_file(path, data.to_vec());
    }
    Machine::with_program(Config::default(), drive, program, "").unwrap()
}

fn console(m: &Machine) -> String {
    String::from_utf8_lossy(&m.console.to_vec()).into_owned()
}

/// `mov ah,9; int 21h` for the `$`-terminated string at label `name`.
fn print(a: &mut Asm, name: &str) -> Result<(), IcedError> {
    let at = a.addr(name);
    a.mov(dx, at)?;
    a.mov(ah, 9)?;
    a.int(0x21)
}

/// `mov dx, offset name`
fn load_dx(a: &mut Asm, name: &str) -> Result<(), IcedError> {
    let at = a.addr(name);
    a.mov(dx, at)
}

/// `word ptr [name]`
fn word_at(a: &Asm, name: &str) -> AsmMemoryOperand {
    word_ptr(a.addr(name) as u32)
}

/// `byte ptr [name]`
fn byte_at(a: &Asm, name: &str) -> AsmMemoryOperand {
    byte_ptr(a.addr(name) as u32)
}

fn exit(a: &mut Asm, code: u8) -> Result<(), IcedError> {
    a.mov(ax, 0x4C00 | i32::from(code))?;
    a.int(0x21)
}

#[test]
fn hello_world_com() {
    let program = com(|a| {
        print(a, "msg")?;
        exit(a, 7)?;
        a.here("msg")?;
        a.db(b"Hello, DOS!\r\n$")
    });
    let mut m = boot(&[("HELLO.COM", &program)], "HELLO.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(7));
    assert_eq!(console(&m), "Hello, DOS!\r\n");
    assert_eq!(m.screen_text().unwrap()[0], "Hello, DOS!");
}

#[test]
fn exe_relocations_point_at_the_load_segment() {
    // Code at image paragraph 0, data at paragraph 2:
    //   mov ax, <data segment, relocated>; mov ds, ax; mov dx, 0; mov ah, 9; int 21h; mov ax, 4C2Ah; int 21h
    let mut image =
        vec![0xB8, 0x02, 0x00, 0x8E, 0xD8, 0xBA, 0x00, 0x00, 0xB4, 0x09, 0xCD, 0x21, 0xB8, 0x2A, 0x4C, 0xCD, 0x21];
    image.resize(32, 0x90);
    image.extend(b"relocated!$");
    let total = 32 + image.len();
    let mut exe = vec![0u8; 32];
    let words: [(usize, u16); 10] = [
        (0x02, (total % 512) as u16),
        (0x04, total.div_ceil(512) as u16),
        (0x06, 1),      // one relocation
        (0x08, 2),      // header paragraphs
        (0x0A, 0x0010), // min alloc
        (0x0C, 0xFFFF), // max alloc
        (0x0E, 0x0010), // SS (relative)
        (0x10, 0x0100), // SP
        (0x14, 0x0000), // IP
        (0x18, 0x001C), // relocation table offset
    ];
    exe[0..2].copy_from_slice(b"MZ");
    for (offset, value) in words {
        exe[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    exe[0x1C..0x20].copy_from_slice(&[0x01, 0x00, 0x00, 0x00]); // the word at 0000:0001
    exe.extend(&image);

    let mut m = boot(&[("GAME.EXE", &exe)], "c:\\game.exe");
    let load_segment = m.cpu.regs.cs();
    assert_eq!(m.cpu.regs.ds(), load_segment - 0x10, "DS = PSP");
    assert_eq!(m.cpu.regs.ss(), load_segment + 0x10);
    assert_eq!(m.run(SECOND), Stop::Exited(0x2A));
    assert_eq!(console(&m), "relocated!");
    assert_eq!(m.dos.program, "GAME.EXE");
}

#[test]
fn file_io_on_the_virtual_drive() {
    let program = com(|a| {
        // create OUT.TXT and write "abc"
        load_dx(a, "out_name")?;
        a.xor(cx, cx)?;
        a.mov(ah, 0x3C)?;
        a.int(0x21)?;
        a.mov(bx, ax)?;
        load_dx(a, "text")?;
        a.mov(cx, 3)?;
        a.mov(ah, 0x40)?;
        a.int(0x21)?;
        a.mov(ah, 0x3E)?;
        a.int(0x21)?;
        // open DATA\IN.DAT, read 5 bytes, write them to stdout
        load_dx(a, "in_name")?;
        a.mov(ax, 0x3D00)?;
        a.int(0x21)?;
        a.mov(bx, ax)?;
        load_dx(a, "buffer")?;
        a.mov(cx, 5)?;
        a.mov(ah, 0x3F)?;
        a.int(0x21)?;
        a.mov(cx, ax)?;
        a.mov(bx, 1)?;
        a.mov(ah, 0x40)?;
        a.int(0x21)?;
        exit(a, 0)?;
        a.here("out_name")?;
        a.db(b"out.txt\0")?;
        a.here("in_name")?;
        a.db(b"data\\in.dat\0")?;
        a.here("text")?;
        a.db(b"abc")?;
        a.here("buffer")?;
        a.db(&[0; 8])
    });
    let mut m = boot(&[("IO.COM", &program), ("DATA\\IN.DAT", b"12345678")], "IO.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    assert_eq!(console(&m), "12345");
    assert_eq!(m.drive.file("OUT.TXT"), Some(&b"abc"[..]));
    assert_eq!(m.drive.changed().collect::<Vec<_>>(), ["OUT.TXT"]);
}

#[test]
fn missing_file_sets_carry_and_error_code() {
    let program = com(|a| {
        load_dx(a, "name")?;
        a.mov(ax, 0x3D00)?;
        a.int(0x21)?;
        let failed = a.l("failed");
        a.jc(failed)?;
        exit(a, 0)?;
        a.here("failed")?;
        a.mov(ah, 0x4C)?; // exit with AL = error code
        a.int(0x21)?;
        a.here("name")?;
        a.db(b"NOPE.DAT\0")
    });
    let mut m = boot(&[("T.COM", &program)], "T.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(2), "file not found");
}

#[test]
fn rtc_periodic_interrupt_runs_on_irq_8() {
    // Hook INT 70h, enable the RTC's periodic interrupt (1024 Hz by default), unmask IRQ 8 and
    // the cascade, then halt until the handler (which reads register C) has run 100 times.
    let program = com(|a| {
        load_dx(a, "handler")?;
        a.mov(ax, 0x2570)?;
        a.int(0x21)?;
        a.mov(al, 0x0B)?;
        a.out(0x70, al)?;
        a.in_(al, 0x71)?;
        a.or(al, 0x40)?;
        a.mov(ah, al)?;
        a.mov(al, 0x0B)?;
        a.out(0x70, al)?;
        a.mov(al, ah)?;
        a.out(0x71, al)?;
        a.in_(al, 0xA1)?;
        a.and(al, 0xFE)?;
        a.out(0xA1, al)?;
        a.in_(al, 0x21)?;
        a.and(al, 0xFB)?;
        a.out(0x21, al)?;
        a.sti()?;
        a.here("wait")?;
        a.hlt()?;
        let count = word_at(a, "count");
        a.cmp(count, 100)?;
        let wait = a.l("wait");
        a.jb(wait)?;
        exit(a, 1)?;
        a.here("handler")?;
        a.push(ax)?;
        a.mov(al, 0x0C)?;
        a.out(0x70, al)?;
        a.in_(al, 0x71)?;
        let count = word_at(a, "count");
        a.inc(count)?;
        a.mov(al, 0x20)?;
        a.out(0xA0, al)?;
        a.out(0x20, al)?;
        a.pop(ax)?;
        a.iret()?;
        a.here("count")?;
        a.dw(&[0])
    });
    let mut m = boot(&[("RTC.COM", &program)], "RTC.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(1));
    let ms = m.elapsed_ms();
    assert!((95..=110).contains(&ms), "100 interrupts at 1024 Hz took {ms} ms");
    assert!((100..=101).contains(&m.probes.irq_counts[8]), "{:?}", m.probes.irq_counts);
}

#[test]
fn timer_interrupt_hook_counts_ticks() {
    // Hook INT 1Ch, HLT until the hook has run 5 times, exit with the count.
    let program = com(|a| {
        load_dx(a, "handler")?;
        a.mov(ax, 0x251C)?;
        a.int(0x21)?;
        a.here("wait")?;
        a.hlt()?;
        let counter = word_at(a, "counter");
        a.cmp(counter, 5)?;
        let wait = a.l("wait");
        a.jb(wait)?;
        let counter = byte_at(a, "counter");
        a.mov(al, counter)?;
        a.mov(ah, 0x4C)?;
        a.int(0x21)?;
        a.here("handler")?;
        let counter = word_at(a, "counter");
        a.inc(counter)?;
        a.iret()?;
        a.here("counter")?;
        a.dw(&[0])
    });
    let mut m = boot(&[("TICK.COM", &program)], "TICK.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(5));
    let ms = m.elapsed_ms();
    assert!((270..=280).contains(&ms), "5 ticks of 54.9 ms, took {ms} ms");
    assert!((5..=6).contains(&m.probes.irq_counts[0]), "IRQ 0 delivered once per tick: {:?}", m.probes.irq_counts);
}

#[test]
fn reprogrammed_pit_speeds_up_irq0() {
    // Program channel 0 for ~1000 Hz, count 100 IRQ0s through our own INT 8 handler.
    let program = com(|a| {
        a.cli()?;
        load_dx(a, "handler")?;
        a.mov(ax, 0x2508)?;
        a.int(0x21)?;
        a.mov(al, 0x36)?;
        a.out(0x43, al)?;
        a.mov(ax, 1193)?;
        a.out(0x40, al)?;
        a.mov(al, ah)?;
        a.out(0x40, al)?;
        a.sti()?;
        a.here("wait")?;
        a.hlt()?;
        let counter = word_at(a, "counter");
        a.cmp(counter, 100)?;
        let wait = a.l("wait");
        a.jb(wait)?;
        exit(a, 0)?;
        a.here("handler")?;
        a.push(ax)?;
        let counter = word_at(a, "counter");
        a.inc(counter)?;
        a.mov(al, 0x20)?;
        a.out(0x20, al)?;
        a.pop(ax)?;
        a.iret()?;
        a.here("counter")?;
        a.dw(&[0])
    });
    let mut m = boot(&[("FAST.COM", &program)], "FAST.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let ms = m.elapsed_ms();
    assert!((99..=101).contains(&ms), "100 ticks at 1 kHz, took {ms} ms");
}

#[test]
fn keyboard_input_through_int_16h() {
    // Read keys with INT 16h AH=00 and echo them until Enter.
    let program = com(|a| {
        a.here("next")?;
        a.mov(ah, 0)?;
        a.int(0x16)?;
        a.cmp(al, 13)?;
        let done = a.l("done");
        a.je(done)?;
        a.mov(dl, al)?;
        a.mov(ah, 2)?;
        a.int(0x21)?;
        let next = a.l("next");
        a.jmp(next)?;
        a.here("done")?;
        exit(a, 0)
    });
    let mut m = boot(&[("KEYS.COM", &program)], "KEYS.COM");
    m.type_text(SECOND / 10, "Hi there!\r", SECOND / 50);
    assert_eq!(m.run(10 * SECOND), Stop::Exited(0));
    assert_eq!(console(&m), "Hi there!");

    let mut idle = boot(&[("KEYS.COM", &program)], "KEYS.COM");
    assert_eq!(idle.run(10 * SECOND), Stop::WaitingForInput);
}

#[test]
fn mode_13h_frames_and_palette() {
    // Set mode 13h, make color 1 pure red, fill the screen with it, wait one retrace, exit.
    let program = com(|a| {
        a.mov(ax, 0x0013)?;
        a.int(0x10)?;
        a.mov(dx, 0x3C8)?;
        a.mov(al, 1)?;
        a.out(dx, al)?;
        a.inc(dx)?;
        a.mov(al, 63)?;
        a.out(dx, al)?;
        a.xor(al, al)?;
        a.out(dx, al)?;
        a.out(dx, al)?;
        a.mov(ax, 0xA000)?;
        a.mov(es, ax)?;
        a.xor(di, di)?;
        a.mov(cx, 32000)?;
        a.mov(ax, 0x0101)?;
        a.rep().stosw()?;
        a.mov(dx, 0x3DA)?;
        a.here("retrace_end")?;
        a.in_(al, dx)?;
        a.test(al, 8)?;
        let retrace_end = a.l("retrace_end");
        a.jnz(retrace_end)?;
        a.here("retrace_start")?;
        a.in_(al, dx)?;
        a.test(al, 8)?;
        let retrace_start = a.l("retrace_start");
        a.jz(retrace_start)?;
        exit(a, 0)
    });
    let mut m = boot(&[("GFX.COM", &program)], "GFX.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let (width, height, rgb) = m.screen_rgb();
    assert_eq!((width, height), (320, 200));
    assert_eq!(&rgb[..3], &[255, 0, 0]);
    assert_eq!(&rgb[rgb.len() - 3..], &[255, 0, 0]);
    let last = m.frames.last().expect("frames were captured");
    assert_eq!(last.mode, 0x13);
    assert!(m.frames.iter().any(|f| f.mode == 3), "the text-mode frame at boot is recorded too");
}

fn interactive_program() -> Vec<u8> {
    // Echo each key plus the number of timer ticks so far, until 'q'.
    com(|a| {
        load_dx(a, "handler")?;
        a.mov(ax, 0x251C)?;
        a.int(0x21)?;
        a.here("next")?;
        a.mov(ah, 0)?;
        a.int(0x16)?;
        a.cmp(al, b'q' as i32)?;
        let done = a.l("done");
        a.je(done)?;
        a.mov(dl, al)?;
        let counter = byte_at(a, "counter");
        a.add(dl, counter)?;
        a.mov(ah, 2)?;
        a.int(0x21)?;
        let next = a.l("next");
        a.jmp(next)?;
        a.here("done")?;
        exit(a, 0)?;
        a.here("handler")?;
        let counter = byte_at(a, "counter");
        a.inc(counter)?;
        a.iret()?;
        a.here("counter")?;
        a.db(&[0])
    })
}

#[test]
fn runs_are_deterministic() {
    let program = interactive_program();
    let run = || {
        let mut m = boot(&[("DET.COM", &program)], "DET.COM");
        m.type_text(SECOND / 3, "abcq", SECOND / 7);
        let stop = m.run(10 * SECOND);
        (stop, m.state_hash(), m.frames.clone(), m.console.clone(), m.cpu.cycles)
    };
    let first = run();
    assert_eq!(first.0, Stop::Exited(0));
    assert_eq!(first.3.len(), 3, "three keys echoed");
    assert_eq!(first, run());
}

#[test]
fn snapshot_and_resume_match_an_uninterrupted_run() {
    let program = com(|a| {
        a.mov(cx, 50)?;
        a.here("spin")?;
        a.hlt()?;
        let spin = a.l("spin");
        a.loop_(spin)?;
        exit(a, 1)
    });
    let mut straight = boot(&[("S.COM", &program)], "S.COM");
    let mut resumed = straight.clone();
    assert_eq!(straight.run(10 * SECOND), Stop::Exited(1));

    assert_eq!(resumed.run(SECOND / 2), Stop::CycleLimit);
    let snapshot = resumed.clone();
    assert_eq!(resumed.run(10 * SECOND), Stop::Exited(1));
    assert_eq!(resumed.state_hash(), straight.state_hash());

    let mut from_snapshot = snapshot;
    assert_eq!(from_snapshot.run(10 * SECOND), Stop::Exited(1));
    assert_eq!(from_snapshot.state_hash(), straight.state_hash());
}

#[test]
fn memory_blocks_shrink_allocate_and_free() {
    // Shrink to 1000h paragraphs, allocate 100h, try to allocate FFFFh (fails, BX = largest),
    // free the 100h block, exit with AL = 0 if all of that behaved.
    let program = com(|a| {
        let bad = a.l("bad");
        a.mov(bx, 0x1000)?;
        a.mov(ah, 0x4A)?;
        a.int(0x21)?;
        a.jc(bad)?;
        a.mov(bx, 0x100)?;
        a.mov(ah, 0x48)?;
        a.int(0x21)?;
        a.jc(bad)?;
        a.mov(si, ax)?;
        a.mov(bx, 0xFFFF)?;
        a.mov(ah, 0x48)?;
        a.int(0x21)?;
        a.jnc(bad)?;
        a.mov(di, bx)?;
        a.mov(es, si)?;
        a.mov(ah, 0x49)?;
        a.int(0x21)?;
        a.jc(bad)?;
        exit(a, 0)?;
        a.here("bad")?;
        exit(a, 1)
    });
    let mut m = boot(&[("MEM.COM", &program)], "MEM.COM");
    let psp = m.dos.psp;
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let block = m.cpu.regs.si();
    assert_eq!(block, psp + 0x1000 + 1, "allocated right after the shrunk program block");
    let largest = m.cpu.regs.di();
    assert_eq!(largest, dos::dos::MEMORY_END - block - 0x100 - 1, "largest free block reported on failure");
}

#[test]
fn find_first_and_next_list_matching_files() {
    let program = com(|a| {
        load_dx(a, "pattern")?;
        a.mov(cx, 0)?;
        a.mov(ah, 0x4E)?;
        a.int(0x21)?;
        let done = a.l("done");
        a.jc(done)?;
        a.here("found")?;
        // print the name at DTA+1Eh (DTA is PSP:80h), then a space
        a.mov(si, 0x9E)?;
        a.here("name_loop")?;
        a.lodsb()?;
        a.or(al, al)?;
        let name_end = a.l("name_end");
        a.jz(name_end)?;
        a.mov(dl, al)?;
        a.mov(ah, 2)?;
        a.int(0x21)?;
        let name_loop = a.l("name_loop");
        a.jmp(name_loop)?;
        a.here("name_end")?;
        a.mov(dl, b' ' as i32)?;
        a.mov(ah, 2)?;
        a.int(0x21)?;
        a.mov(ah, 0x4F)?;
        a.int(0x21)?;
        let found = a.l("found");
        a.jnc(found)?;
        a.here("done")?;
        exit(a, 0)?;
        a.here("pattern")?;
        a.db(b"DATA\\*.DAT\0")
    });
    let mut m =
        boot(&[("F.COM", &program), ("DATA\\B.DAT", b"x"), ("DATA\\A.DAT", b"y"), ("DATA\\C.TXT", b"z")], "F.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    assert_eq!(console(&m), "A.DAT B.DAT ");
}

#[test]
fn unimplemented_instruction_stops_with_a_fault() {
    let program = com(|a| a.db(&[0x0F, 0xA2])); // cpuid: a Pentium instruction
    let mut m = boot(&[("X.COM", &program)], "X.COM");
    match m.run(SECOND) {
        Stop::Fault(cpu::Exit::Unimplemented { ip, bytes, .. }) => {
            assert_eq!(ip, 0x100);
            assert_eq!(&bytes[..2], &[0x0F, 0xA2]);
        }
        other => panic!("expected a fault, got {other:?}"),
    }
}

#[test]
fn unsupported_services_are_reported_once() {
    let program = com(|a| {
        for _ in 0..3 {
            a.mov(ah, 0x5F)?; // network redirection: not implemented
            a.int(0x21)?;
        }
        exit(a, 0)
    });
    let mut m = boot(&[("U.COM", &program)], "U.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    assert_eq!(m.diagnostics.iter().collect::<Vec<_>>(), ["INT 21h AH=5Fh is not implemented"]);
}

#[test]
fn load_errors() {
    let drive = Vfs::new();
    assert!(matches!(Machine::with_program(Config::default(), drive, "GAME.EXE", ""), Err(LoadError::NotFound(_))));
}

/// `mov ah,40h` to stdout: `len` bytes at offset `at`.
fn write_out(a: &mut Asm, at: i32, len: i32) -> Result<(), IcedError> {
    a.mov(dx, at)?;
    a.mov(cx, len)?;
    a.mov(bx, 1)?;
    a.mov(ah, 0x40)?;
    a.int(0x21)
}

#[test]
fn start_directory_cd_folder_and_volume_labels() {
    // Like Steam's DOSBox setup for a CD game: the installed game in C:\GAME (started there), the
    // disc's contents in C:\CD shown as D: with the disc's label.
    let program = com(|a| {
        load_dx(a, "dta")?;
        a.mov(ah, 0x1A)?;
        a.int(0x21)?;
        for (mask, len) in [("cd_mask", 11), ("c_mask", 7)] {
            load_dx(a, mask)?;
            a.mov(cx, 8)?; // volume label only
            a.mov(ah, 0x4E)?;
            a.int(0x21)?;
            let name = a.addr("dta") + 0x1E;
            write_out(a, name, len)?;
        }
        a.mov(dl, 3)?; // C:
        let cwd = a.addr("cwd");
        a.mov(si, cwd)?;
        a.mov(ah, 0x47)?;
        a.int(0x21)?;
        write_out(a, cwd, 4)?;
        let fail = a.l("fail");
        load_dx(a, "on_cd")?;
        a.mov(ax, 0x3D00)?;
        a.int(0x21)?;
        a.jc(fail)?;
        load_dx(a, "escape")?; // ".." can't leave the CD's folder
        a.mov(ax, 0x3D00)?;
        a.int(0x21)?;
        a.jnc(fail)?;
        exit(a, 1)?;
        a.here("fail")?;
        exit(a, 2)?;
        a.here("cd_mask")?;
        a.db(b"D:\\*.*\0")?;
        a.here("c_mask")?;
        a.db(b"C:\\*.*\0")?;
        a.here("on_cd")?;
        a.db(b"D:\\INSTALL.SCR\0")?;
        a.here("escape")?;
        a.db(b"D:\\..\\GAME\\GAME.COM\0")?;
        a.here("cwd")?;
        a.db(&[0; 64])?;
        a.here("dta")?;
        a.db(&[0; 43])
    });
    let config = Config {
        start_dir: "GAME".into(),
        cd_drive: Some('D'),
        cd_root: "CD".into(),
        cd_label: "Daggerfall".into(),
        ..Config::default()
    };
    let mut m = boot_with(config, &[("GAME\\GAME.COM", &program), ("CD\\INSTALL.SCR", b"x")], "GAME.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(1), "{}", console(&m));
    // CD labels keep their case and split after eight characters, as MSCDEX shows them.
    assert_eq!(console(&m), "Daggerfa.llC_DRIVEGAME");

    let config = Config { start_dir: "NOWHERE".into(), ..Config::default() };
    let mut drive = Vfs::new();
    drive.insert_file("GAME.COM", fpu_detection_program());
    assert!(matches!(Machine::with_program(config, drive, "GAME.COM", ""), Err(LoadError::NotFound(_))));
}

#[test]
fn parse_file_names_into_fcbs() {
    // INT 21h AH=29h into FCBs at 400h and 420h; AL results at 440h/441h, SI advances at 442h/444h.
    let program = com(|a| {
        for (i, text) in ["text1", "text2"].into_iter().enumerate() {
            let i = i as u32;
            let at = a.addr(text);
            a.mov(si, at)?;
            a.mov(di, 0x400 + 0x20 * i as i32)?;
            a.mov(ax, 0x2901)?; // skip a leading separator
            a.int(0x21)?;
            a.mov(byte_ptr(0x440 + i), al)?;
            a.mov(bx, at)?; // (sub si, imm would change size between the passes)
            a.sub(si, bx)?;
            a.mov(word_ptr(0x442 + 2 * i), si)?;
        }
        exit(a, 0)?;
        a.here("text1")?;
        a.db(b" c:foo*.c rest\0")?;
        a.here("text2")?;
        a.db(b"x:bar.txt\0")
    });
    let mut m = boot(&[("FCB.COM", &program)], "FCB.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let at = |offset: u32| (u32::from(m.dos.psp) << 4) + offset;
    assert_eq!(m.mem.read_bytes(at(0x400), 12), b"\x03FOO?????C  ");
    assert_eq!(m.mem.read_u8(at(0x440)), 1, "wildcards");
    assert_eq!(m.mem.read_u16(at(0x442)), 9);
    assert_eq!(m.mem.read_bytes(at(0x420), 12), b"\x18BAR     TXT");
    assert_eq!(m.mem.read_u8(at(0x441)), 0xFF, "no drive X:, but the name is parsed");
    assert_eq!(m.mem.read_u16(at(0x444)), 9);
}

#[test]
fn dos_internal_tables() {
    // Results at DS:400h: first MCB, files in the SFT chain, InDOS, the SDA's PSP, temp handle.
    let program = com(|a| {
        a.mov(ah, 0x52)?;
        a.int(0x21)?;
        a.mov(ax, word_ptr(bx - 2).es())?;
        a.mov(word_ptr(0x400), ax)?;
        a.les(di, dword_ptr(bx + 4).es())?;
        a.xor(cx, cx)?;
        let next = a.l("next");
        let done = a.l("done");
        a.here("next")?;
        a.add(cx, word_ptr(di + 4).es())?;
        a.cmp(word_ptr(di).es(), -1)?;
        a.je(done)?;
        a.les(di, dword_ptr(di).es())?;
        a.jmp(next)?;
        a.here("done")?;
        a.mov(word_ptr(0x402), cx)?;
        a.mov(ah, 0x34)?;
        a.int(0x21)?;
        a.mov(al, byte_ptr(bx).es())?;
        a.mov(byte_ptr(0x404), al)?;
        a.mov(ax, 0x5D06)?;
        a.int(0x21)?;
        a.mov(ax, word_ptr(si + 0x10))?; // current PSP, read through DS = the SDA
        a.push(cs)?;
        a.pop(ds)?;
        a.mov(word_ptr(0x406), ax)?;
        // A temporary file in C:\ (the name is appended to the path).
        a.mov(word_ptr(0x500), 0x3A43)?; // "C:"
        a.mov(word_ptr(0x502), 0x005C)?; // "\" and NUL
        a.mov(dx, 0x500)?;
        a.xor(cx, cx)?;
        a.mov(ah, 0x5A)?;
        a.int(0x21)?;
        let fail = a.l("fail");
        a.jc(fail)?;
        exit(a, 0)?;
        a.here("fail")?;
        exit(a, 1)
    });
    let mut m = boot(&[("TABLES.COM", &program)], "TABLES.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let at = |offset: u32| (u32::from(m.dos.psp) << 4) + offset;
    assert_eq!(m.mem.read_u16(at(0x400)), dos::dos::FIRST_MCB);
    assert_eq!(m.mem.read_u16(at(0x402)), 100, "FILES=100 across the SFT chain");
    assert_eq!(m.mem.read_u8(at(0x404)), 0, "never inside DOS between instructions");
    assert_eq!(m.mem.read_u16(at(0x406)), m.dos.psp);
    let name = String::from_utf8(m.mem.read_bytes(at(0x500), 11)).unwrap();
    assert!(name.starts_with("C:\\") && name[3..].chars().all(|c| c.is_ascii_hexdigit()), "{name:?}");
    assert!(m.drive.changed().any(|path| path == &name[3..]), "the temporary file exists");
}

#[test]
fn extended_executables_run_their_dos_stub() {
    // An MZ stub (`mov ax, 4C07h; int 21h`) followed by an LE header, like a DOS-extended game:
    // DOS runs the stub, which would be the extender.
    let mut le = vec![0u8; 0x90];
    le[..2].copy_from_slice(b"MZ");
    le[0x02] = 0x45; // the MZ image ends at 45h
    le[0x04] = 1; // one page
    le[0x08] = 4; // 4 header paragraphs
    le[0x18] = 0x40;
    le[0x3C] = 0x80; // the LE header, past the MZ image
    le[0x40..0x45].copy_from_slice(&[0xB8, 0x07, 0x4C, 0xCD, 0x21]);
    le[0x80..0x82].copy_from_slice(b"LE");
    let mut m = boot(&[("DOOM.EXE", &le)], "DOOM.EXE");
    assert_eq!(m.run(SECOND), Stop::Exited(7));
}

#[test]
fn command_tail_and_environment() {
    // Echo the command tail from PSP:81h to stdout via AH=40h.
    let program = com(|a| {
        a.xor(ch, ch)?;
        a.mov(cl, byte_ptr(0x80))?;
        a.mov(dx, 0x81)?;
        a.mov(bx, 1)?;
        a.mov(ah, 0x40)?;
        a.int(0x21)?;
        exit(a, 0)
    });
    let mut drive = Vfs::new();
    drive.insert_file("ARGS.COM", program);
    let mut m = Machine::with_program(Config::default(), drive, "ARGS.COM", "-level 3").unwrap();
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    assert_eq!(console(&m), " -level 3");
    let lin = |segment: u16, offset: u16| (u32::from(segment) << 4) + u32::from(offset);
    let env_segment = m.mem.read_u16(lin(m.dos.psp, 0x2C));
    let env = m.mem.read_bytes(lin(env_segment, 0), 96);
    let text = String::from_utf8_lossy(&env);
    assert!(text.contains("C:\\ARGS.COM"), "program path after the variables: {text:?}");
}

#[test]
fn live_input_between_chunks_replays_identically() {
    // What `clockwork play` does: run one 70 Hz frame at a time, queue keys at the cycle where
    // the run paused. A replay that queues the same (cycle, scancode) pairs up front must end in
    // exactly the same state.
    let program = interactive_program();
    let frame = SECOND / 70;
    let mut live = boot(&[("DET.COM", &program)], "DET.COM");
    live.stop_on_input_wait = false;
    let mut recorded = Vec::new();
    let presses = [(20u64, 0x1Eu8), (21, 0x9E), (45, 0x30), (47, 0xB0), (90, 0x10), (91, 0x90)];
    let mut stop = Stop::CycleLimit;
    for chunk in 0..200u64 {
        for &(_, code) in presses.iter().filter(|(at, _)| *at == chunk) {
            let now = live.cpu.cycles;
            live.queue_scancodes(now, &[code]);
            recorded.push((now, code));
        }
        stop = live.run(frame);
        if stop == Stop::Exited(0) {
            break;
        }
    }
    assert_eq!(stop, Stop::Exited(0));

    let mut replay = boot(&[("DET.COM", &program)], "DET.COM");
    for &(cycle, code) in &recorded {
        replay.queue_scancodes(cycle, &[code]);
    }
    assert_eq!(replay.run(10 * SECOND), Stop::Exited(0));
    assert_eq!(replay.cpu.cycles, live.cpu.cycles);
    assert_eq!(replay.state_hash(), live.state_hash());
    assert_eq!(replay.frames, live.frames);
    assert_eq!(console(&replay), console(&live));
}

/// Classic CPU detection: exits with 2 for an 80286, 3 for an 80386 (FLAGS bits 12-14 stick).
fn cpu_detection_program() -> Vec<u8> {
    com(|a| {
        a.pushf()?;
        a.pop(ax)?;
        a.or(ax, 0x7000)?;
        a.push(ax)?;
        a.popf()?;
        a.pushf()?;
        a.pop(ax)?;
        a.and(ax, 0x7000)?;
        let is_286 = a.l("is_286");
        a.jz(is_286)?;
        exit(a, 3)?;
        a.here("is_286")?;
        exit(a, 2)
    })
}

#[test]
fn programs_see_the_configured_cpu() {
    let program = cpu_detection_program();
    let mut m = boot(&[("CPU.COM", &program)], "CPU.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(3));

    let mut drive = Vfs::new();
    drive.insert_file("CPU.COM", program);
    let config = Config { cpu_model: dos::CpuModel::I286, ..Config::default() };
    let mut m = Machine::with_program(config, drive, "CPU.COM", "").unwrap();
    assert_eq!(m.run(SECOND), Stop::Exited(2));
}

/// Coprocessor detection: exit code bit 1 from the BIOS equipment word (INT 11h), bit 0 if
/// FNSTSW stored a clear status word after FNINIT.
fn fpu_detection_program() -> Vec<u8> {
    com(|a| {
        a.int(0x11)?;
        a.and(ax, 2)?;
        a.mov(bx, ax)?;
        a.fninit()?;
        let status = word_at(a, "status");
        a.fnstsw(status)?;
        let status = word_at(a, "status");
        a.cmp(status, 0)?;
        let done = a.l("done");
        a.jne(done)?;
        a.or(bx, 1)?;
        a.here("done")?;
        a.mov(al, bl)?;
        a.mov(ah, 0x4C)?;
        a.int(0x21)?;
        a.here("status")?;
        a.dw(&[0x5A5A])
    })
}

#[test]
fn programs_detect_the_coprocessor() {
    let program = fpu_detection_program();
    let mut m = boot(&[("FPU.COM", &program)], "FPU.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(3));

    let mut drive = Vfs::new();
    drive.insert_file("FPU.COM", program);
    let config = Config { fpu: false, ..Config::default() };
    let mut m = Machine::with_program(config, drive, "FPU.COM", "").unwrap();
    assert_eq!(m.run(SECOND), Stop::Exited(0));
}

#[test]
fn thirty_two_bit_code_runs() {
    // Sum 1..=100000 in EAX (doesn't fit 16 bits), store it, print it as 8 hex digits.
    let program = com(|a| {
        a.xor(eax, eax)?;
        a.mov(ecx, 100_000)?;
        a.here("sum")?;
        a.add(eax, ecx)?;
        let sum = a.l("sum");
        a.dec(ecx)?;
        a.jnz(sum)?;
        let result = a.addr("result");
        a.mov(dword_ptr(result as u32), eax)?;
        a.mov(cx, 8)?;
        a.here("digit")?;
        a.rol(eax, 4)?;
        a.mov(dl, al)?;
        a.and(dl, 0x0F)?;
        a.add(dl, b'0' as i32)?;
        a.cmp(dl, b'9' as i32)?;
        let print = a.l("print");
        a.jbe(print)?;
        a.add(dl, 7)?;
        a.here("print")?;
        a.push(eax)?;
        a.mov(ah, 2)?;
        a.int(0x21)?;
        a.pop(eax)?;
        let digit = a.l("digit");
        a.loop_(digit)?;
        exit(a, 0)?;
        a.here("result")?;
        a.dd(&[0])
    });
    let mut m = boot(&[("SUM.COM", &program)], "SUM.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    assert_eq!(console(&m), format!("{:08X}", 5_000_050_000u64 as u32));
}

#[test]
fn raw_switch_to_protected_mode_and_back() {
    // What a DOS extender does without a DPMI host: build a GDT, enter protected mode, write
    // above 1 MB through a flat segment, return to real mode and exit through DOS.
    let program = com(|a| {
        a.xor(ebx, ebx)?;
        a.mov(bx, cs)?;
        let saved_cs = word_at(a, "saved_cs");
        a.mov(saved_cs, bx)?;
        a.shl(ebx, 4)?; // linear base of our segment
        // Code descriptor (selector 8): base = our segment, so offsets stay the same.
        let gdt = a.addr("gdt") as u32;
        a.mov(word_ptr(gdt + 8 + 2), bx)?;
        a.mov(eax, ebx)?;
        a.shr(eax, 16)?;
        a.mov(byte_ptr(gdt + 8 + 4), al)?;
        let gdtr = a.addr("gdtr") as u32;
        a.mov(eax, ebx)?;
        a.mov(ecx, gdt)?; // fixed-size encoding for the two-pass assembly
        a.add(eax, ecx)?;
        a.mov(dword_ptr(gdtr + 2), eax)?;
        a.lgdt(word_ptr(gdtr))?;
        a.cli()?;
        a.mov(eax, cr0)?;
        a.or(al, 1)?;
        a.mov(cr0, eax)?;
        let pm = a.addr("pm") as u16;
        a.db(&[0xEA])?; // jmp far 0008:pm
        a.dw(&[pm, 0x08])?;
        a.here("pm")?;
        a.mov(ax, 0x10)?; // flat 4 GB data
        a.mov(es, ax)?;
        a.mov(edi, 0x20_0000)?;
        a.mov(dword_ptr(edi).es(), 0x2121_4D50)?;
        let saved_cs = word_at(a, "saved_cs");
        a.push(saved_cs)?;
        let back = a.addr("back");
        a.mov(ax, back)?;
        a.push(ax)?;
        a.mov(eax, cr0)?;
        a.and(al, 0xFE)?;
        a.mov(cr0, eax)?;
        a.retf()?;
        a.here("back")?;
        a.mov(ax, cs)?;
        a.mov(ds, ax)?;
        a.mov(es, ax)?;
        a.sti()?;
        exit(a, 0)?;
        a.here("saved_cs")?;
        a.dw(&[0])?;
        a.here("gdtr")?;
        a.dw(&[23, 0, 0])?;
        a.here("gdt")?;
        a.dq(&[0, 0x0000_9A00_0000_FFFF, 0x00CF_9200_0000_FFFF])
    });
    let mut m = boot(&[("PM.COM", &program)], "PM.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let written: Vec<u8> = (0..4).map(|i| m.mem.read_phys(0x20_0000 + i)).collect();
    assert_eq!(written, b"PM!!");
    assert!(!m.cpu.protected_mode());
}

fn boot_with(config: Config, files: &[(&str, &[u8])], program: &str) -> Machine {
    let mut drive = Vfs::new();
    for (path, data) in files {
        drive.insert_file(path, data.to_vec());
    }
    Machine::with_program(config, drive, program, "").unwrap()
}

/// The word at `offset` in the COM program's segment.
fn word_at_offset(m: &Machine, offset: i32) -> u16 {
    m.mem.read_u16((u32::from(m.dos.psp) << 4) + offset as u32)
}

/// Stores AX and the carry flag (as 0 or FFFFh) in the two words at label `slot`.
fn store_result(a: &mut Asm, slot: &'static str) -> Result<(), IcedError> {
    let at = a.addr(slot) as u32;
    a.mov(word_ptr(at), ax)?;
    a.sbb(si, si)?;
    a.mov(word_ptr(at + 2), si)
}

#[test]
fn cd_drive_with_mscdex_is_read_only() {
    let slots = ["count", "letter", "open_d", "write_d", "create_d", "write_c"];
    let program = com(|a| {
        a.mov(ax, 0x1500)?;
        a.int(0x2F)?;
        let (count, letter) = (a.addr("count") as u32, a.addr("letter") as u32);
        a.mov(word_ptr(count), bx)?;
        a.mov(word_ptr(letter), cx)?;
        // Open D:\DATA.TXT read/write: succeeds, read-only underneath.
        load_dx(a, "d_name")?;
        a.mov(ax, 0x3D02)?;
        a.int(0x21)?;
        store_result(a, "open_d")?;
        a.mov(bx, ax)?;
        load_dx(a, "byte")?;
        a.mov(cx, 1)?;
        a.mov(ah, 0x40)?;
        a.int(0x21)?;
        store_result(a, "write_d")?;
        // Create D:\NEW.TXT: access denied.
        load_dx(a, "d_new")?;
        a.xor(cx, cx)?;
        a.mov(ah, 0x3C)?;
        a.int(0x21)?;
        store_result(a, "create_d")?;
        // Open C:\DATA.TXT read/write and write a byte: works.
        load_dx(a, "c_name")?;
        a.mov(ax, 0x3D02)?;
        a.int(0x21)?;
        a.mov(bx, ax)?;
        load_dx(a, "byte")?;
        a.mov(cx, 1)?;
        a.mov(ah, 0x40)?;
        a.int(0x21)?;
        store_result(a, "write_c")?;
        exit(a, 0)?;
        for (label, text) in [
            ("d_name", &b"D:\\DATA.TXT\0"[..]),
            ("d_new", b"D:\\NEW.TXT\0"),
            ("c_name", b"C:\\DATA.TXT\0"),
            ("byte", b"!"),
        ] {
            a.here(label)?;
            a.db(text)?;
        }
        for label in slots {
            a.here(label)?;
            a.dw(&[0, 0])?;
        }
        Ok(())
    });
    let config = Config { cd_drive: Some('D'), ..Config::default() };
    let mut m = boot_with(config, &[("T.COM", &program), ("DATA.TXT", b"abc")], "T.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    // The result slots are the program's last bytes, 4 per slot.
    let base = 0x100 + program.len() as i32 - 4 * slots.len() as i32;
    let slot = |i: i32| (word_at_offset(&m, base + i * 4), word_at_offset(&m, base + i * 4 + 2));
    assert_eq!(slot(0).0, 1, "one CD drive");
    assert_eq!(slot(1).0, 3, "drive D:");
    assert_eq!(slot(2).1, 0, "opening D: read/write succeeds");
    assert_eq!(slot(3), (5, 0xFFFF), "writing to the CD fails with access denied");
    assert_eq!(slot(4), (5, 0xFFFF), "creating on the CD fails");
    assert_eq!(slot(5), (1, 0), "C: is writable");
    assert_eq!(m.drive.file("DATA.TXT"), Some(&b"!bc"[..]), "C: and D: show the same files");
}

#[test]
fn without_a_cd_drive_mscdex_is_absent() {
    let program = com(|a| {
        a.xor(bx, bx)?;
        a.mov(ax, 0x1500)?;
        a.int(0x2F)?;
        a.mov(ax, bx)?;
        a.mov(ah, 0x4C)?;
        a.int(0x21)
    });
    let mut m = boot(&[("T.COM", &program)], "T.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0), "BX stays 0: no CD drives");
}

#[test]
fn start_drive_and_environment_variables() {
    let program = com(|a| {
        a.mov(ah, 0x19)?;
        a.int(0x21)?;
        a.mov(ah, 0x4C)?;
        a.int(0x21)
    });
    let config = Config {
        cd_drive: Some('D'),
        start_drive: 'D',
        environment: vec![("ARENADATA".into(), "C:".into())],
        ..Config::default()
    };
    let mut m = boot_with(config, &[("GAME.COM", &program)], "GAME.COM");
    let env_segment = m.mem.read_u16((u32::from(m.dos.psp) << 4) + 0x2C);
    let env = m.mem.read_bytes(u32::from(env_segment) << 4, 128);
    let text = String::from_utf8_lossy(&env);
    assert!(text.contains("ARENADATA=C:\0"), "{text:?}");
    assert!(text.contains("D:\\GAME.COM"), "program path on the current drive: {text:?}");
    assert_eq!(m.run(SECOND), Stop::Exited(3), "current drive is D:");
}

#[test]
fn expanded_memory_pages_and_moves() {
    // Allocate 4 pages; write 11h through logical page 0 and 22h through logical page 1, both
    // via physical page 0; map them back and read through physical pages 0 and 1. Then move
    // "XYZ" from conventional memory to logical page 2, offset 5 (AH=57h) and read it mapped.
    let program = com(|a| {
        let handle = a.addr("handle") as u32;
        let results: Vec<u32> = ["r0", "r1", "r2", "r3"].iter().map(|r| a.addr(r) as u32).collect();
        let map = |a: &mut Asm, physical: i32, logical: i32| -> Result<(), IcedError> {
            a.mov(ax, 0x4400 | physical)?;
            a.mov(bx, logical)?;
            a.mov(dx, word_ptr(handle))?;
            a.int(0x67)
        };
        let poke = |a: &mut Asm, offset: u32, value: i32| -> Result<(), IcedError> {
            a.mov(ax, 0xE000)?;
            a.mov(ds, ax)?;
            a.mov(byte_ptr(offset), value)?;
            a.push(cs)?;
            a.pop(ds)
        };
        let peek = |a: &mut Asm, offset: u32, slot: u32| -> Result<(), IcedError> {
            a.mov(ax, 0xE000)?;
            a.mov(ds, ax)?;
            a.mov(al, byte_ptr(offset))?;
            a.push(cs)?;
            a.pop(ds)?;
            a.mov(byte_ptr(slot), al)
        };
        a.mov(ah, 0x43)?;
        a.mov(bx, 4)?;
        a.int(0x67)?;
        a.mov(word_ptr(handle), dx)?;
        map(a, 0, 0)?;
        poke(a, 0, 0x11)?;
        map(a, 0, 1)?;
        poke(a, 0, 0x22)?;
        map(a, 0, 0)?;
        peek(a, 0, results[0])?;
        map(a, 1, 1)?;
        peek(a, 0x4000, results[1])?;
        let request = a.addr("request") as u32;
        a.mov(dx, word_ptr(handle))?;
        a.mov(word_ptr(request + 12), dx)?; // destination handle
        a.mov(word_ptr(request + 9), ds)?; // source segment
        a.mov(si, request as i32)?;
        a.mov(ax, 0x5700)?;
        a.int(0x67)?;
        map(a, 2, 2)?;
        peek(a, 0x8000 + 6, results[2])?;
        a.mov(ah, 0x45)?;
        a.mov(dx, word_ptr(handle))?;
        a.int(0x67)?;
        a.mov(byte_ptr(results[3]), ah)?;
        exit(a, 0)?;
        a.here("text")?;
        a.db(b"XYZ")?;
        a.here("request")?;
        // Length 3; source: conventional, handle 0, offset = text, segment (patched);
        // destination: expanded, handle (patched), offset 5, logical page 2.
        let text = a.addr("text") as u16;
        let mut request_bytes = vec![3, 0, 0, 0, 0, 0, 0];
        request_bytes.extend(text.to_le_bytes());
        request_bytes.extend([0, 0, 1, 0, 0, 5, 0, 2, 0]);
        a.db(&request_bytes)?;
        for label in ["handle", "r0", "r1", "r2", "r3"] {
            a.here(label)?;
            a.dw(&[0])?;
        }
        Ok(())
    });
    let mut m = boot(&[("EMS.COM", &program)], "EMS.COM");
    // Detection: the INT 67h vector's segment has the driver name at offset 0Ah.
    let segment = m.mem.read_u16(0x67 * 4 + 2);
    assert_eq!(&m.mem.read_bytes((u32::from(segment) << 4) + 0x0A, 8), b"EMMXXXX0");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let base = 0x100 + program.len() as i32 - 10;
    let result = |i: i32| word_at_offset(&m, base + i * 2) as u8;
    assert_eq!(result(1), 0x11, "logical page 0 kept its data while unmapped");
    assert_eq!(result(2), 0x22, "logical page 1 shows through physical page 1");
    assert_eq!(result(3), b'Y', "AH=57h moved XYZ to page 2, offset 5");
    assert_eq!(result(4), 0, "pages freed");
    assert_eq!(u32::from(m.ems.free_pages()), m.config.ems_kb / 16);
}

#[test]
fn service_trace_records_calls() {
    let program = com(|a| {
        load_dx(a, "name")?;
        a.mov(ax, 0x3D00)?;
        a.int(0x21)?;
        exit(a, 0)?;
        a.here("name")?;
        a.db(b"MISSING.DAT\0")
    });
    let mut m = boot(&[("T.COM", &program)], "T.COM");
    m.probes.service_trace = Some(Vec::new());
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let calls = m.probes.service_trace.as_ref().unwrap();
    let open = calls.iter().find(|c| c.before.ah() == 0x3D).expect("the open was traced");
    assert_eq!(open.path.as_deref(), Some("MISSING.DAT"));
    assert_eq!(open.after.ax(), 2);
    assert!(open.returned_flags);
    assert!(open.to_string().contains("\"MISSING.DAT\" -> AX=0002"), "{open}");
}

#[test]
fn mouse_polling_reports_positions_with_text_mode_granularity() {
    // Reset the driver (exit 1 if absent), poll until the left button is down, exit with CL.
    let program = com(|a| {
        a.xor(ax, ax)?;
        a.int(0x33)?;
        a.cmp(ax, 0xFFFF)?;
        let poll = a.l("poll");
        a.je(poll)?;
        exit(a, 1)?;
        a.here("poll")?;
        a.mov(ax, 3)?;
        a.int(0x33)?;
        a.test(bl, 1)?;
        let poll = a.l("poll");
        a.jz(poll)?;
        a.mov(al, cl)?;
        a.mov(ah, 0x4C)?;
        a.int(0x21)
    });
    let mut m = boot(&[("M.COM", &program)], "M.COM");
    m.queue_mouse(SECOND / 5, 107, 53, 1);
    assert_eq!(m.run(SECOND), Stop::Exited(104), "x 107 reported as 104 (text cells are 8 wide)");
    assert_eq!(m.cpu.regs.dx(), 48, "y 53 reported as 48");

    let mut no_mouse = boot_with(Config { mouse: false, ..Config::default() }, &[("M.COM", &program)], "M.COM");
    assert_eq!(no_mouse.run(SECOND), Stop::Exited(1), "without a driver, reset leaves AX = 0");
}

#[test]
fn the_mouse_state_is_what_int_33h_reports() {
    // Forever: a wait (where the input arrives), then at `read` functions 03h, 0Bh, 05h and 06h (left
    // button) into `got` (11 words), then `done`.
    let offsets = std::cell::RefCell::new(BTreeMap::new());
    let program = com(|a| {
        for name in ["read", "done", "got"] {
            offsets.borrow_mut().insert(name, a.addr(name) as u32);
        }
        a.xor(ax, ax)?;
        a.int(0x33)?;
        a.here("wait")?;
        a.mov(cx, 5000)?;
        a.here("delay")?;
        let delay = a.l("delay");
        a.loop_(delay)?;
        a.here("read")?;
        let got = a.addr("got");
        a.mov(di, got)?;
        a.mov(ax, 3)?;
        a.int(0x33)?;
        for reg in [bx, cx, dx] {
            a.mov(ax, reg)?;
            a.stosw()?;
        }
        a.mov(ax, 0xB)?;
        a.int(0x33)?;
        for reg in [cx, dx] {
            a.mov(ax, reg)?;
            a.stosw()?;
        }
        for function in [5, 6] {
            a.mov(ax, function)?;
            a.xor(bx, bx)?;
            a.int(0x33)?;
            for reg in [bx, cx, dx] {
                a.mov(ax, reg)?;
                a.stosw()?;
            }
        }
        a.here("done")?;
        let wait = a.l("wait");
        a.jmp(wait)?;
        a.here("got")?;
        a.dw(&[0; 11])
    });
    let o = offsets.into_inner();
    let mut m = boot(&[("M.COM", &program)], "M.COM");
    let base = u32::from(m.dos.psp) << 4;
    let (read, done, got) = (base + o["read"], base + o["done"], base + o["got"]);
    m.syncs.gate(read, vec![dos::sync::Source::Device(dos::Device::Mouse)]);
    m.queue_mouse(SECOND / 10, 100, 50, 0);
    m.queue_mouse(SECOND / 5, 123, 61, 1);
    m.queue_mouse(SECOND * 3 / 10, 123, 61, 0);
    let (mut ticks, mut most_skipped) = (0, 0);
    while m.cpu.cycles < SECOND / 2 && m.run_to_sync(read, 1, SECOND / 2) != Stop::CycleLimit {
        let state = m.device_state(dos::Device::Mouse);
        most_skipped = most_skipped.max(m.syncs.get(read).unwrap().skipped);
        let words: Vec<u16> = state.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert!(matches!(m.run_to_sync(done, 1, SECOND), Stop::Sync { .. }));
        let answered: Vec<u16> = (0..11).map(|i| m.mem.read_u16(got + 2 * i)).collect();
        let expected: Vec<u16> = [0, 1, 2, 3, 4, 5, 6, 7, 14, 15, 16].iter().map(|&i| words[i]).collect();
        assert_eq!(answered, expected, "tick {ticks}: the state words of 03h, 0Bh, 05h and 06h");
        ticks += 1;
    }
    // The first hit; then each event, and the hit after it, which sees what the reads cleared (the
    // motion, the press count, the release count).
    assert_eq!(ticks, 7, "a tick when the mouse changed, not once a poll");
    assert!(most_skipped > 100, "the loop polled {most_skipped} times between two ticks");
}

#[test]
fn mouse_event_handler_runs_from_irq_12() {
    // Install a handler for left-button presses that stores CX and sets a flag; HLT until set.
    let program = com(|a| {
        let (x_slot, flag) = (a.addr("x") as u32, a.addr("flag") as u32);
        a.xor(ax, ax)?;
        a.int(0x33)?;
        a.push(cs)?;
        a.pop(es)?;
        load_dx(a, "handler")?;
        a.mov(cx, 0x02)?;
        a.mov(ax, 0x0C)?;
        a.int(0x33)?;
        a.here("wait")?;
        a.hlt()?;
        a.cmp(byte_ptr(flag), 0)?;
        let wait = a.l("wait");
        a.je(wait)?;
        a.mov(al, byte_ptr(x_slot))?;
        a.mov(ah, 0x4C)?;
        a.int(0x21)?;
        a.here("handler")?;
        a.mov(word_ptr(x_slot), cx)?;
        a.mov(byte_ptr(flag), 1)?;
        a.retf()?;
        a.here("x")?;
        a.dw(&[0])?;
        a.here("flag")?;
        a.db(&[0])
    });
    let mut m = boot(&[("H.COM", &program)], "H.COM");
    m.queue_mouse(SECOND / 10, 150, 20, 0); // a move alone: not in the handler's mask
    m.queue_mouse(SECOND / 5, 200, 60, 1);
    assert_eq!(m.run(SECOND), Stop::Exited(200));
}

#[test]
fn mouse_cursor_is_drawn_and_erased_in_mode_13h() {
    let program = com(|a| {
        a.mov(ax, 0x0013)?;
        a.int(0x10)?;
        a.xor(ax, ax)?;
        a.int(0x33)?;
        a.mov(ax, 4)?;
        a.mov(cx, 320)?;
        a.mov(dx, 100)?;
        a.int(0x33)?;
        a.mov(ax, 1)?;
        a.int(0x33)?;
        a.here("idle")?;
        a.hlt()?;
        let idle = a.l("idle");
        a.jmp(idle)
    });
    let pixel = |m: &Machine, x: u32, y: u32| m.devices.vga.peek(y * 320 + x);
    let mut m = boot(&[("C.COM", &program)], "C.COM");
    m.queue_mouse(SECOND / 2, 2, 1, 0);
    assert_eq!(m.run(SECOND / 4), Stop::CycleLimit);
    assert_eq!(pixel(&m, 161, 101), 0x0F, "arrow drawn at x 320/2, y 100");
    assert_eq!(pixel(&m, 160, 100), 0x00, "the arrow's tip row is transparent");
    assert_eq!(m.run(SECOND / 2), Stop::CycleLimit);
    assert_eq!(pixel(&m, 161, 101), 0x00, "old position restored");
    assert_eq!(pixel(&m, 2, 2), 0x0F, "arrow moved to x 2/2, y 1");
}

fn pixel_13h(m: &Machine, x: u32, y: u32) -> u8 {
    m.devices.vga.peek(y * 320 + x)
}

/// The 8×8 glyph of `ch` as drawn at character cell (column, row) in mode 13h: true = foreground.
fn assert_glyph_13h(m: &Machine, character: u8, column: u32, row: u32, foreground: u8, background: u8) {
    for y in 0..8 {
        let bits = dos::fonts::FONT_8X8[usize::from(character) * 8 + y as usize];
        for x in 0..8 {
            let expected = if bits & (0x80 >> x) != 0 { foreground } else { background };
            let actual = pixel_13h(m, column * 8 + x, row * 8 + y);
            assert_eq!(actual, expected, "glyph {:?} pixel ({x},{y})", char::from(character));
        }
    }
}

#[test]
fn bios_text_is_drawn_in_mode_13h() {
    // Mode 13h; teletype 'A' in color 4; DOS prints "B" (color 7); AH=09h writes 'C' in color 2 on
    // background 1 (BH) at row 2; then hold.
    let program = com(|a| {
        a.mov(ax, 0x0013)?;
        a.int(0x10)?;
        a.mov(ax, 0x0E41)?;
        a.mov(bx, 0x0004)?;
        a.int(0x10)?;
        a.mov(dl, b'B' as i32)?;
        a.mov(ah, 2)?;
        a.int(0x21)?;
        a.mov(ah, 2)?;
        a.mov(bh, 0)?;
        a.mov(dx, 0x0200)?; // row 2, column 0
        a.int(0x10)?;
        a.mov(ax, 0x0943)?;
        a.mov(bx, 0x0102)?;
        a.mov(cx, 1)?;
        a.int(0x10)?;
        a.here("hold")?;
        a.hlt()?;
        let hold = a.l("hold");
        a.jmp(hold)
    });
    let mut m = boot(&[("T.COM", &program)], "T.COM");
    assert_eq!(m.run(SECOND / 4), Stop::CycleLimit);
    assert_glyph_13h(&m, b'A', 0, 0, 4, 0);
    assert_glyph_13h(&m, b'B', 1, 0, 7, 0);
    assert_glyph_13h(&m, b'C', 0, 2, 2, 1);
    assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
}

#[test]
fn rom_font_pointers() {
    // AX=1130h BH=06h → ES:BP = 8×16 font, CX = character height, DL = rows - 1 (text mode 3).
    let program = com(|a| {
        a.mov(ax, 0x1130)?;
        a.mov(bh, 6)?;
        a.int(0x10)?;
        a.hlt()?;
        exit(a, 0)
    });
    let mut m = boot(&[("F.COM", &program)], "F.COM");
    m.run(SECOND / 10);
    let r = m.cpu.regs;
    assert_eq!((r.cx(), r.dl()), (16, 24));
    let font = m.mem.read_bytes((u32::from(r.es()) << 4) + u32::from(r.bp()), 4096);
    assert_eq!(font, dos::fonts::FONT_8X16);
    assert_eq!(m.mem.read_bytes(0xF_FA6E, 1024), &dos::fonts::FONT_8X8[..1024], "IBM 8×8 font location");
    let int43 = (u32::from(m.mem.read_u16(0x43 * 4 + 2)) << 4) + u32::from(m.mem.read_u16(0x43 * 4));
    assert_eq!(m.mem.read_bytes(int43, 8), &dos::fonts::FONT_8X8[..8], "INT 43h: 8×8 font in text mode");
}

#[test]
fn graphics_mode_scrolls_text_rows() {
    // Mode 13h: print 'X' at row 1, scroll the window up one row (AH=06h), 'X' is now on row 0.
    let program = com(|a| {
        a.mov(ax, 0x0013)?;
        a.int(0x10)?;
        a.mov(ah, 2)?;
        a.mov(bh, 0)?;
        a.mov(dx, 0x0100)?;
        a.int(0x10)?;
        a.mov(ax, 0x0E58)?;
        a.mov(bl, 0x0F)?;
        a.int(0x10)?;
        a.mov(ax, 0x0601)?;
        a.mov(bh, 0)?;
        a.mov(cx, 0x0000)?;
        a.mov(dx, 0x1827)?; // rows 0-24, columns 0-39
        a.int(0x10)?;
        a.here("hold")?;
        a.hlt()?;
        let hold = a.l("hold");
        a.jmp(hold)
    });
    let mut m = boot(&[("S.COM", &program)], "S.COM");
    m.run(SECOND / 4);
    assert_glyph_13h(&m, b'X', 0, 0, 0x0F, 0);
    assert!((0..8).all(|y| (0..8).all(|x| pixel_13h(&m, x, 8 + y) == 0)), "row 1 blanked");
}

#[test]
fn write_string_with_attributes_and_text_rendering() {
    // AH=13h AL=03h: string of (char, attribute) pairs at row 0, column 0, cursor moved after it.
    let program = com(|a| {
        a.push(cs)?;
        a.pop(es)?;
        let text = a.addr("text");
        a.mov(bp, text)?;
        a.mov(ax, 0x1303)?;
        a.mov(bx, 0x0000)?;
        a.mov(cx, 2)?;
        a.mov(dx, 0x0000)?;
        a.int(0x10)?;
        a.hlt()?;
        exit(a, 0)?;
        a.here("text")?;
        a.db(&[b'H', 0x1F, b'i', 0x4E])
    });
    let mut m = boot(&[("W.COM", &program)], "W.COM");
    m.run(SECOND / 10);
    assert_eq!(m.mem.read_bytes(0xB_8000, 4), &[b'H', 0x1F, b'i', 0x4E]);
    assert_eq!(m.mem.read_u16(0x450), 0x0002, "cursor after the string");
    let (width, height, rgb) = m.screen_rgb();
    assert_eq!((width, height), (720, 400), "80 columns of 9-dot characters");
    let pixel =
        |x: usize, y: usize| [rgb[(y * width + x) * 3], rgb[(y * width + x) * 3 + 1], rgb[(y * width + x) * 3 + 2]];
    assert_eq!(pixel(0, 0), [0, 0, 170], "blue background of attribute 1Fh");
    // Find a lit pixel of 'H' in its cell: white foreground.
    let lit = (0..16)
        .flat_map(|y| (0..8).map(move |x| (x, y)))
        .find(|&(x, y)| dos::fonts::FONT_8X16[usize::from(b'H') * 16 + y] & (0x80 >> x) != 0);
    let (x, y) = lit.expect("H has pixels");
    assert_eq!(pixel(x, y), [255, 255, 255]);
}

/// The displayed picture (DAC indices), rendered from the VGA state.
fn picture(m: &Machine) -> dos::vga_draw::Picture {
    m.devices.vga.picture(m.mem.slice(0xB_0000, dos::vga_draw::LOW_RAM_SIZE))
}

fn hold(a: &mut Asm) -> Result<(), IcedError> {
    a.here("hold")?;
    a.hlt()?;
    let hold = a.l("hold");
    a.jmp(hold)
}

#[test]
fn a_frame_s_picture_is_its_indexed_pixels_and_palette() {
    // Mode 13h; DAC entry 1 = (63, 0, 0); pixel (10, 5) in colour 1; then halt.
    let program = com(|a| {
        a.mov(ax, 0x0013)?;
        a.int(0x10)?;
        a.mov(dx, 0x3C8)?;
        a.mov(al, 1)?;
        a.out(dx, al)?;
        a.mov(dx, 0x3C9)?;
        for value in [63, 0, 0] {
            a.mov(al, value)?;
            a.out(dx, al)?;
        }
        a.mov(ax, 0xA000)?;
        a.mov(es, ax)?;
        a.mov(byte_ptr(5 * 320 + 10).es(), 1)?;
        hold(a)
    });
    let mut m = boot(&[("P.COM", &program)], "P.COM");
    m.run(SECOND / 10);
    m.record_frame_images(true);
    m.run(SECOND / 10);
    let images = m.take_frame_images();
    assert_eq!(images.len(), 1, "the same picture is rendered once");
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::DeflateDecoder::new(&images[0].1[..]), &mut raw).unwrap();
    assert_eq!(&raw[..9], b"FRM1\x40\x01\xC8\x00\x00", "320 × 200, indexed");
    let (palette, pixels) = raw[9..].split_at(768);
    assert_eq!(&palette[3..6], [63, 0, 0]);
    assert_eq!(pixels.len(), 320 * 200);
    assert_eq!((pixels[5 * 320 + 10], pixels[5 * 320 + 11]), (1, 0));
}

#[test]
fn planar_mode_12h_pixels_text_and_scrolling() {
    let program = com(|a| {
        a.mov(ax, 0x0012)?;
        a.int(0x10)?;
        // Yellow pixel at (10, 20), read it back, then XOR it with 15.
        a.mov(ax, 0x0C0E)?;
        a.xor(bx, bx)?;
        a.mov(cx, 10)?;
        a.mov(dx, 20)?;
        a.int(0x10)?;
        a.mov(ah, 0x0D)?;
        a.int(0x10)?;
        store_result(a, "pixel")?;
        a.mov(ax, 0x0C8F)?;
        a.int(0x10)?;
        // 'A' in light red at row 1, column 0, then scroll the screen up one row.
        a.mov(ah, 2)?;
        a.mov(dx, 0x0100)?;
        a.int(0x10)?;
        a.mov(ax, 0x0E41)?;
        a.mov(bl, 0x0C)?;
        a.int(0x10)?;
        a.mov(ax, 0x0601)?;
        a.xor(bx, bx)?;
        a.xor(cx, cx)?;
        a.mov(dx, 0x1D4F)?;
        a.int(0x10)?;
        hold(a)?;
        a.here("pixel")?;
        a.dw(&[0, 0])
    });
    let mut m = boot(&[("P.COM", &program)], "P.COM");
    m.run(SECOND / 4);
    assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
    assert_eq!(word_at_offset(&m, 0x100 + program.len() as i32 - 4) & 0xFF, 0x0E, "AH=0Dh reads the color back");
    let p = picture(&m);
    assert_eq!((p.width, p.height), (640, 480));
    let vga = &m.devices.vga;
    // The pixel scrolled up with its row: (10, 20) is now (10, 4).
    assert_eq!(p.pixels[4 * 640 + 10], vga.attribute_color(0x0E ^ 0x0F));
    for y in 0..16 {
        let bits = dos::fonts::FONT_8X16[usize::from(b'A') * 16 + y];
        for x in 0..8 {
            let expected = vga.attribute_color(if bits & (0x80 >> x) != 0 { 0x0C } else { 0 });
            assert_eq!(p.pixels[y * 640 + x], expected, "'A' moved to row 0: pixel ({x}, {y})");
            assert_eq!(p.pixels[(16 + y) * 640 + x], vga.attribute_color(0), "row 1 cleared");
        }
    }
}

#[test]
fn mode_x_unchained_320x240_with_page_flipping() {
    let program = com(|a| {
        a.mov(ax, 0x0013)?;
        a.int(0x10)?;
        a.mov(dx, 0x3C4)?;
        a.mov(ax, 0x0604)?; // chain-4 off
        a.out(dx, ax)?;
        // Unprotect CRTC 0-7, then the 320×240 timing (Abrash's Mode X table).
        a.mov(dx, 0x3D4)?;
        a.mov(al, 0x11)?;
        a.out(dx, al)?;
        a.inc(dx)?;
        a.in_(al, dx)?;
        a.and(al, 0x7F)?;
        a.out(dx, al)?;
        a.dec(dx)?;
        for word in [0x0D06, 0x3E07, 0x4109, 0xEA10, 0xAC11, 0xDF12, 0x0014, 0xE715, 0x0616, 0xE317] {
            a.mov(ax, word)?;
            a.out(dx, ax)?;
        }
        // Plane 1: pixel (9, 239) on page 0 and (5, 0) on the page at 19200.
        a.mov(dx, 0x3C4)?;
        a.mov(ax, 0x0202)?;
        a.out(dx, ax)?;
        a.push(ds)?;
        a.mov(ax, 0xA000)?;
        a.mov(ds, ax)?;
        a.mov(byte_ptr(80 * 239 + 2), 7)?;
        a.mov(byte_ptr(19200 + 1), 9)?;
        // Read plane 1 back through read map select.
        a.mov(dx, 0x3CE)?;
        a.mov(ax, 0x0104)?;
        a.out(dx, ax)?;
        a.mov(al, byte_ptr(19200 + 1))?;
        a.pop(ds)?;
        let read = byte_at(a, "read");
        a.mov(read, al)?;
        // Show the second page.
        a.mov(dx, 0x3D4)?;
        a.mov(ax, 0x4B0C)?;
        a.out(dx, ax)?;
        a.mov(ax, 0x000D)?;
        a.out(dx, ax)?;
        hold(a)?;
        a.here("read")?;
        a.db(&[0])
    });
    let mut m = boot(&[("X.COM", &program)], "X.COM");
    m.run(SECOND / 4);
    assert_eq!(m.mem.read_u8((u32::from(m.dos.psp) << 4) + 0x100 + program.len() as u32 - 1), 9);
    let p = picture(&m);
    assert_eq!((p.width, p.height), (320, 240));
    assert_eq!(p.pixels[5], 9, "page 2 is displayed");
    assert_eq!(p.pixels[(239 - 48) * 320 + 9], 0, "page 2 doesn't show page 0's pixel");
    let (width, height, _) = m.screen_rgb();
    assert_eq!((width, height), (320, 240));
}

#[test]
fn text_font_load_gives_80x50() {
    let program = com(|a| {
        a.mov(ax, 0x1112)?; // load and activate the 8×8 font
        a.xor(bl, bl)?;
        a.int(0x10)?;
        a.mov(ah, 2)?;
        a.xor(bh, bh)?;
        a.mov(dx, 0x2D00)?; // row 45
        a.int(0x10)?;
        a.mov(ax, 0x0E51)?;
        a.int(0x10)?;
        hold(a)
    });
    let mut m = boot(&[("F.COM", &program)], "F.COM");
    m.run(SECOND / 4);
    assert_eq!((m.mem.read_u8(0x484), m.mem.read_u8(0x485)), (49, 8));
    let text = m.screen_text().expect("text mode");
    assert_eq!(text.len(), 50);
    assert_eq!(text[45], "Q");
    let p = picture(&m);
    assert_eq!((p.width, p.height), (720, 400));
    // The glyph comes from plane 2, now holding the 8×8 font.
    let bits = dos::fonts::FONT_8X8[usize::from(b'Q') * 8 + 1];
    let lit = (0..8).find(|x| bits & (0x80 >> x) != 0).unwrap();
    assert_eq!(p.pixels[(45 * 8 + 1) * 720 + lit], m.devices.vga.attribute_color(7));
}

#[test]
fn cga_mode_4_pixels_and_palette() {
    let program = com(|a| {
        a.mov(ax, 0x0004)?;
        a.int(0x10)?;
        a.mov(ax, 0x0C03)?;
        a.xor(bx, bx)?;
        a.mov(cx, 7)?;
        a.mov(dx, 1)?;
        a.int(0x10)?;
        a.mov(ah, 0x0B)?;
        a.mov(bx, 0x0100)?; // palette 0: green, red, brown
        a.int(0x10)?;
        hold(a)
    });
    let mut m = boot(&[("C.COM", &program)], "C.COM");
    m.run(SECOND / 4);
    assert_eq!(m.mem.read_u8(0xB_8000 + 0x2000 + 1) & 3, 3, "odd lines in the second bank");
    let (width, height, rgb) = m.screen_rgb();
    assert_eq!((width, height), (320, 200));
    let at = (320 + 7) * 3;
    assert_eq!(&rgb[at..at + 3], &[255, 255, 85], "color 3 of high-intensity palette 0 is yellow");
}

#[test]
fn mouse_cursor_in_planar_mode() {
    let program = com(|a| {
        a.mov(ax, 0x0012)?;
        a.int(0x10)?;
        a.xor(ax, ax)?;
        a.int(0x33)?;
        a.mov(ax, 4)?;
        a.mov(cx, 100)?;
        a.mov(dx, 50)?;
        a.int(0x33)?;
        a.mov(ax, 1)?;
        a.int(0x33)?;
        hold(a)
    });
    let mut m = boot(&[("M.COM", &program)], "M.COM");
    m.queue_mouse(SECOND / 2, 300, 300, 0);
    m.run(SECOND / 4);
    let white = m.devices.vga.attribute_color(0x0F);
    assert_eq!(picture(&m).pixels[51 * 640 + 101], white, "arrow at (100, 50)");
    m.run(SECOND / 2);
    let p = picture(&m);
    assert_eq!(p.pixels[51 * 640 + 101], m.devices.vga.attribute_color(0), "background restored");
    assert_eq!(p.pixels[301 * 640 + 301], white, "arrow moved");
    assert_eq!(m.devices.vga.gc[8], 0xFF, "the program's VGA registers are restored");
}

/// `mov dx, 388h; mov al, register; out dx, al; inc dx; mov al, value; out dx, al`
fn adlib_write(a: &mut Asm, register: u8, value: u8) -> Result<(), IcedError> {
    a.mov(dx, 0x388)?;
    a.mov(al, i32::from(register))?;
    a.out(dx, al)?;
    a.inc(dx)?;
    a.mov(al, i32::from(value))?;
    a.out(dx, al)
}

#[test]
fn adlib_detection_with_the_opl_timer() {
    let program = com(|a| {
        adlib_write(a, 0x04, 0x60)?;
        adlib_write(a, 0x04, 0x80)?;
        a.mov(dx, 0x388)?;
        a.in_(al, dx)?;
        let first = byte_at(a, "first");
        a.mov(first, al)?;
        adlib_write(a, 0x02, 0xFF)?;
        adlib_write(a, 0x04, 0x21)?;
        a.mov(dx, 0x388)?;
        a.mov(cx, 1000)?; // well over 80 µs
        a.here("wait")?;
        a.in_(al, dx)?;
        let wait = a.l("wait");
        a.loop_(wait)?;
        let second = byte_at(a, "second");
        a.mov(second, al)?;
        adlib_write(a, 0x04, 0x60)?;
        adlib_write(a, 0x04, 0x80)?;
        exit(a, 0)?;
        a.here("first")?;
        a.db(&[0xFF])?;
        a.here("second")?;
        a.db(&[0])
    });
    let mut m = boot(&[("A.COM", &program)], "A.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let at = |back: u32| m.mem.read_u8((u32::from(m.dos.psp) << 4) + 0x100 + program.len() as u32 - back);
    assert_eq!((at(2) & 0xE0, at(1) & 0xE0), (0x00, 0xC0), "status before and after timer 1 overflows");
}

/// Waits for the DSP to accept a byte at 22Ch, then writes `value`.
fn dsp_write(a: &mut Asm, label: &'static str, value: u8) -> Result<(), IcedError> {
    a.mov(dx, 0x22C)?;
    a.here(label)?;
    a.in_(al, dx)?;
    a.test(al, 0x80)?;
    let busy = a.l(label);
    a.jnz(busy)?;
    a.mov(al, i32::from(value))?;
    a.out(dx, al)
}

/// A program that resets the DSP, installs an IRQ 7 handler counting interrupts at label
/// "count", programs DMA channel 1 for `dma_count + 1` bytes at 5000:0000 (auto-init if
/// `dsp` starts an auto-init transfer), sends `dsp` and waits.
fn sound_blaster_program(dma_mode: u8, dma_count: u16, dsp: &'static [u8]) -> Vec<u8> {
    const LABELS: [&str; 8] = ["d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7"];
    com(move |a| {
        a.mov(dx, 0x226)?;
        a.mov(al, 1)?;
        a.out(dx, al)?;
        a.mov(cx, 50)?;
        a.here("pause")?;
        let pause = a.l("pause");
        a.loop_(pause)?;
        a.xor(al, al)?;
        a.out(dx, al)?;
        a.mov(dx, 0x22E)?;
        a.here("ready")?;
        a.in_(al, dx)?;
        a.test(al, 0x80)?;
        let ready = a.l("ready");
        a.jz(ready)?;
        a.mov(dx, 0x22A)?;
        a.in_(al, dx)?;
        let reset = byte_at(a, "reset");
        a.mov(reset, al)?;
        // INT 0Fh (IRQ 7) handler, unmasked at the PIC.
        load_dx(a, "handler")?;
        a.mov(ax, 0x250F)?;
        a.int(0x21)?;
        a.in_(al, 0x21)?;
        a.and(al, 0x7F)?;
        a.out(0x21, al)?;
        for (port, value) in [(0x0A, 5), (0x0C, 0), (0x0B, dma_mode), (0x02, 0), (0x02, 0), (0x83, 5)] {
            a.mov(al, i32::from(value))?;
            a.out(port, al)?;
        }
        a.mov(al, i32::from(dma_count as u8))?;
        a.out(0x03, al)?;
        a.mov(al, i32::from((dma_count >> 8) as u8))?;
        a.out(0x03, al)?;
        a.mov(al, 1)?;
        a.out(0x0A, al)?;
        for (i, &byte) in dsp.iter().enumerate() {
            dsp_write(a, LABELS[i], byte)?;
        }
        hold(a)?;
        a.here("handler")?;
        a.push(ax)?;
        a.push(dx)?;
        a.mov(dx, 0x22E)?; // acknowledge
        a.in_(al, dx)?;
        a.mov(al, 0x20)?;
        a.out(0x20, al)?;
        let count = byte_at(a, "count");
        a.inc(count)?;
        a.pop(dx)?;
        a.pop(ax)?;
        a.iret()?;
        a.here("reset")?;
        a.db(&[0])?;
        a.here("count")?;
        a.db(&[0])
    })
}

#[test]
fn sound_blaster_single_cycle_dma_interrupts_when_the_block_is_played() {
    // 1000 bytes at 10 kHz (time constant 156): 100 ms.
    let program = sound_blaster_program(0x49, 999, &[0xD1, 0x40, 156, 0x14, 0xE7, 0x03]);
    let mut m = boot(&[("S.COM", &program)], "S.COM");
    let byte =
        |m: &Machine, back: u32| m.mem.read_u8((u32::from(m.dos.psp) << 4) + 0x100 + program.len() as u32 - back);
    m.run(SECOND * 9 / 100);
    assert_eq!(byte(&m, 2), 0xAA, "the DSP answered the reset");
    assert_eq!(byte(&m, 1), 0, "no interrupt before the block has played");
    m.run(SECOND * 2 / 100);
    assert_eq!(byte(&m, 1), 1, "one interrupt at the end of the block");
    let pcm = m.audio_log().pcm;
    assert_eq!(pcm.iter().map(|s| s.samples.len()).sum::<usize>(), 1000);
    assert_eq!(pcm[0].rate_hz, 10_000);
    assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
}

#[test]
fn sound_blaster_auto_init_interrupts_every_block() {
    // A 200-byte auto-init DMA buffer, DSP blocks of 100 bytes at 10 kHz: an IRQ every 10 ms.
    let program = sound_blaster_program(0x59, 199, &[0x40, 156, 0x48, 99, 0x00, 0x1C]);
    let mut m = boot(&[("S.COM", &program)], "S.COM");
    m.run(SECOND * 105 / 1000);
    let count = m.mem.read_u8((u32::from(m.dos.psp) << 4) + 0x100 + program.len() as u32 - 1);
    assert_eq!(count, 10);
    let frames: usize = m.audio_log().pcm.iter().map(|s| s.samples.len()).sum();
    assert!((1000..1050).contains(&frames), "{frames} samples");
}

#[test]
fn sound_blaster_playback_is_logged_as_it_plays() {
    // One 100 ms block at 10 kHz: the samples are read a millisecond at a time, not at its end.
    let program = sound_blaster_program(0x49, 999, &[0xD1, 0x40, 156, 0x14, 0xE7, 0x03]);
    let mut m = boot(&[("S.COM", &program)], "S.COM");
    m.run(SECOND * 5 / 100);
    let logged: usize = m.audio_log().pcm.iter().map(|s| s.samples.len()).sum();
    assert!(logged >= 480, "{logged} samples logged after 50 ms");
    assert!(m.audio_settled_ns() + 3_000_000 >= m.elapsed_ns(), "the log is final up to a few ms ago");
}

#[test]
fn audio_rendered_while_running_matches_the_final_log() {
    let program = sound_blaster_program(0x59, 199, &[0x40, 156, 0x48, 99, 0x00, 0x1C]);
    let mut m = boot(&[("S.COM", &program)], "S.COM");
    let mut renderer = dos::audio::Renderer::default();
    let mut live = Vec::new();
    for step in [SECOND / 70, SECOND / 333, SECOND / 70, SECOND / 7].into_iter().cycle().take(12) {
        m.run(step);
        renderer.render(&m.audio_log(), m.audio_settled_ns(), &mut live);
    }
    assert!(m.elapsed_ns() > 500_000_000);
    let whole = m.audio_log().render(renderer.rendered_ns());
    assert_eq!(live.len(), whole.len());
    assert!(live == whole, "live rendering differs from rendering the final log");
}

#[test]
fn pc_speaker_plays_samples_by_pulse_width_modulation() {
    // Channel 2 in mode 0 with the speaker on, a new count for every sample (a sawtooth), as
    // speaker sample players do.
    let program = com(|a| {
        a.mov(al, 0x90)?; // channel 2, low byte only, mode 0
        a.out(0x43, al)?;
        a.in_(al, 0x61)?;
        a.or(al, 0x03)?;
        a.out(0x61, al)?;
        a.mov(cx, 3000)?;
        a.here("sample")?;
        a.mov(al, cl)?;
        a.and(al, 0x3F)?;
        a.add(al, 4)?;
        a.out(0x42, al)?;
        a.mov(bx, 20)?;
        a.here("wait")?;
        a.dec(bx)?;
        let wait = a.l("wait");
        a.jnz(wait)?;
        let sample = a.l("sample");
        a.loop_(sample)?;
        hold(a)
    });
    let mut m = boot(&[("P.COM", &program)], "P.COM");
    let mut renderer = dos::audio::Renderer::default();
    let mut live = Vec::new();
    for _ in 0..20 {
        m.run(SECOND / 100);
        renderer.render(&m.audio_log(), m.audio_settled_ns(), &mut live);
    }
    let speaker = m.audio_log().speaker;
    let counts = speaker.iter().filter(|e| matches!(e.wave, dos::pit::Wave::LowUntil(_))).count();
    assert_eq!(counts, 3000, "one waveform per count");
    let whole = m.audio_log().render(renderer.rendered_ns());
    assert!(live == whole, "live rendering differs from rendering the final log");
    assert!(whole.iter().any(|s| s.unsigned_abs() > 1000), "it makes a sound");
}

#[test]
fn a_machine_loaded_from_a_snapshot_goes_on_playing() {
    // An FM note and a speaker tone, held. A snapshot holds no sound logs: the loaded machine
    // logs what its devices play again, and renders from the load on as the whole run does.
    let program = com(|a| {
        let note = [(0x20, 0x01), (0x40, 0x10), (0x60, 0xF0), (0x80, 0x77), (0x23, 0x01), (0x63, 0xF0)];
        for (register, value) in note.into_iter().chain([(0x83, 0x77), (0xA0, 0x98), (0xB0, 0x31)]) {
            adlib_write(a, register, value)?;
        }
        a.mov(al, 0xB6)?; // channel 2, both bytes, square wave
        a.out(0x43, al)?;
        a.mov(ax, 1193)?; // 1 kHz
        a.out(0x42, al)?;
        a.mov(al, ah)?;
        a.out(0x42, al)?;
        a.in_(al, 0x61)?;
        a.or(al, 0x03)?;
        a.out(0x61, al)?;
        hold(a)
    });
    let mut drive = Vfs::new();
    drive.insert_file("P.COM", program.clone());
    let mut m = Machine::with_program(Config::default(), drive.clone(), "P.COM", "").unwrap();
    assert_eq!(m.run(SECOND / 10), Stop::CycleLimit);
    let mut file = Vec::new();
    dos::snapshot::save(&m, &mut file).unwrap();
    let mut loaded = dos::snapshot::load(&file[..], drive, None).unwrap();
    m.run(SECOND / 5);
    loaded.run(SECOND / 5);
    assert_eq!(loaded.state_hash(), m.state_hash());
    let whole = m.audio_log().render(m.elapsed_ns());
    let rest = loaded.audio_log().render(loaded.elapsed_ns());
    let part = rest.len() as f64 / whole.len() as f64;
    assert!((0.66..0.67).contains(&part), "the last two thirds: {part}");
    let energy = |s: &[i16]| s.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>();
    let ratio = energy(&rest) / energy(&whole[whole.len() - rest.len()..]);
    assert!((0.9..1.1).contains(&ratio), "as loud as the whole run from there: {ratio}");
}

#[test]
fn blaster_variable_is_in_the_environment() {
    let mut m = boot(&[("E.COM", &com(hold))], "E.COM");
    m.run(SECOND / 100);
    let env_segment = m.mem.read_u16((u32::from(m.dos.psp) << 4) + 0x2C);
    let env = String::from_utf8_lossy(&m.mem.read_bytes(u32::from(env_segment) << 4, 256)).into_owned();
    assert!(env.contains("BLASTER=A220 I7 D1 H5 T6\0"), "{env:?}");
}

#[test]
fn xms_driver_allocates_moves_and_locks_extended_memory() {
    let program = com(|a| {
        a.mov(ax, 0x4300)?;
        a.int(0x2F)?;
        let installed = byte_at(a, "installed");
        a.mov(installed, al)?;
        a.mov(ax, 0x4310)?;
        a.int(0x2F)?;
        let entry = a.addr("entry") as u32;
        a.mov(word_ptr(entry), bx)?;
        a.mov(word_ptr(entry + 2), es)?;
        let call_xms = |a: &mut Asm| a.call(dword_ptr(entry));
        a.mov(ah, 0x00)?;
        call_xms(a)?;
        let version = word_at(a, "version");
        a.mov(version, ax)?;
        a.mov(ah, 0x08)?;
        call_xms(a)?;
        let free = word_at(a, "free");
        a.mov(free, dx)?;
        a.mov(ah, 0x09)?;
        a.mov(dx, 64)?;
        call_xms(a)?;
        let handle = word_at(a, "handle");
        a.mov(handle, dx)?;
        // Conventional memory -> EMB offset 1000h, then back into "buffer".
        let to_emb = a.addr("to_emb") as u32;
        let from_emb = a.addr("from_emb") as u32;
        a.mov(word_ptr(to_emb + 8), ds)?;
        a.mov(word_ptr(to_emb + 10), dx)?;
        a.mov(word_ptr(from_emb + 4), dx)?;
        a.mov(word_ptr(from_emb + 14), ds)?;
        let at = a.addr("to_emb");
        a.mov(si, at)?;
        a.mov(ah, 0x0B)?;
        call_xms(a)?;
        let at = a.addr("from_emb");
        a.mov(si, at)?;
        a.mov(ah, 0x0B)?;
        call_xms(a)?;
        let handle = word_at(a, "handle");
        a.mov(dx, handle)?;
        a.mov(ah, 0x0C)?;
        call_xms(a)?;
        let locked = a.addr("locked") as u32;
        a.mov(word_ptr(locked), bx)?;
        a.mov(word_ptr(locked + 2), dx)?;
        a.mov(ah, 0x03)?; // global enable A20
        call_xms(a)?;
        a.mov(ah, 0x07)?;
        call_xms(a)?;
        let a20 = word_at(a, "a20");
        a.mov(a20, ax)?;
        a.mov(ah, 0x01)?; // request the HMA
        a.mov(dx, 0xFFFF)?;
        call_xms(a)?;
        let hma = word_at(a, "hma");
        a.mov(hma, ax)?;
        a.mov(ah, 0x88)?; // INT 15h: no extended memory left for the BIOS
        a.int(0x15)?;
        let bios = word_at(a, "bios");
        a.mov(bios, ax)?;
        exit(a, 0)?;
        a.here("installed")?;
        a.db(&[0])?;
        a.here("entry")?;
        a.dw(&[0, 0])?;
        for label in ["version", "free", "handle", "a20", "hma", "bios"] {
            a.here(label)?;
            a.dw(&[0xFFFF])?;
        }
        a.here("locked")?;
        a.dw(&[0, 0])?;
        a.here("message")?;
        a.db(b"Extended memory!")?;
        a.here("buffer")?;
        a.db(&[0; 16])?;
        // length 16, source handle 0 + DS:message, destination handle (patched) + offset 1000h
        let message = a.addr("message") as u16;
        let buffer = a.addr("buffer") as u16;
        a.here("to_emb")?;
        a.dw(&[16, 0, 0, message, 0, 0, 0x1000, 0])?;
        a.here("from_emb")?;
        a.dw(&[16, 0, 0, 0x1000, 0, 0, buffer, 0])
    });
    let mut m = boot(&[("X.COM", &program)], "X.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
    let base = (u32::from(m.dos.psp) << 4) + 0x100;
    let find = |m: &Machine, text: &[u8]| {
        let image = m.mem.read_bytes(base, program.len());
        image.windows(text.len()).position(|w| w == text).map(|i| base + i as u32)
    };
    let fields = find(&m, b"Extended memory!").unwrap() - 4 - 12 - 1 - 4;
    let word = |offset: u32| m.mem.read_u16(fields + offset);
    assert_eq!(m.mem.read_u8(fields), 0x80, "installed");
    assert_eq!(word(5), 0x0300, "XMS 3.0");
    assert_eq!(word(7), 15 * 1024 - 64, "free KB: 15 MB minus the HMA");
    assert_eq!(word(9), 1, "first handle");
    assert_eq!(word(11), 1, "A20 enabled");
    assert_eq!(word(13), 1, "HMA granted");
    assert_eq!(word(15), 0, "INT 15h AH=88h reports no free extended memory");
    assert_eq!(m.mem.read_u32(fields + 17), 0x11_0000, "locked block's physical address");
    assert_eq!(m.mem.read_phys(0x11_1000), b'E', "the data is in extended memory");
    let message = find(&m, b"Extended memory!").unwrap();
    assert_eq!(m.mem.read_bytes(message + 16, 16), b"Extended memory!", "and came back");
    assert!(m.mem.a20());
}

#[test]
fn bios_extended_memory_without_xms() {
    // INT 15h AH=88h reports 15 MB, AH=87h copies 8 bytes to 200000h through a GDT.
    let program = com(|a| {
        a.mov(ah, 0x88)?;
        a.int(0x15)?;
        let size = word_at(a, "size");
        a.mov(size, ax)?;
        // Source descriptor base = DS:data (linear), destination base = 200000h.
        a.mov(ax, ds)?;
        a.mov(dx, 16)?;
        a.mul(dx)?; // DX:AX = DS * 16
        let data = a.addr("data");
        a.add(ax, data)?;
        a.adc(dx, 0)?;
        let gdt = a.addr("gdt") as u32;
        a.mov(word_ptr(gdt + 0x12), ax)?;
        a.mov(byte_ptr(gdt + 0x14), dl)?;
        a.push(ds)?;
        a.pop(es)?;
        let at = a.addr("gdt");
        a.mov(si, at)?;
        a.mov(cx, 4)?; // words
        a.mov(ah, 0x87)?;
        a.int(0x15)?;
        exit(a, 0)?;
        a.here("size")?;
        a.dw(&[0])?;
        a.here("data")?;
        a.db(b"BLOCKMOV")?;
        a.here("gdt")?;
        let mut gdt = [0u8; 48];
        gdt[0x10..0x18].copy_from_slice(&[0xFF, 0xFF, 0, 0, 0, 0x93, 0, 0]);
        gdt[0x18..0x20].copy_from_slice(&[0xFF, 0xFF, 0x00, 0x00, 0x20, 0x93, 0, 0]);
        a.db(&gdt)
    });
    let config = Config { xms: false, ..Config::default() };
    let mut m = boot_with(config, &[("B.COM", &program)], "B.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let base = (u32::from(m.dos.psp) << 4) + 0x100 + program.len() as u32 - 48 - 8 - 2;
    assert_eq!(m.mem.read_u16(base), 15 * 1024);
    let copied: Vec<u8> = (0..8).map(|i| m.mem.read_phys(0x20_0000 + i)).collect();
    assert_eq!(copied, b"BLOCKMOV");
}

#[test]
fn exec_runs_a_child_that_returns_to_its_parent() {
    // The child writes "hi" to inherited handle 5 and exits with 10h + its command tail length.
    let child = com(|a| {
        a.mov(ah, 0x40)?;
        a.mov(bx, 5)?;
        a.mov(cx, 2)?;
        load_dx(a, "text")?;
        a.int(0x21)?;
        a.mov(al, byte_ptr(0x80))?;
        a.add(al, 0x10)?;
        a.mov(ah, 0x4C)?;
        a.int(0x21)?;
        a.here("text")?;
        a.db(b"hi")
    });
    let parent = com(|a| {
        a.mov(ah, 0x4A)?; // keep 64 KB
        a.mov(bx, 0x1000)?;
        a.int(0x21)?;
        a.mov(ah, 0x3C)?; // create OUT.TXT: handle 5
        a.xor(cx, cx)?;
        load_dx(a, "out")?;
        a.int(0x21)?;
        a.mov(ah, 0x48)?; // largest free block before
        a.mov(bx, 0xFFFF)?;
        a.int(0x21)?;
        let before = word_at(a, "before");
        a.mov(before, bx)?;
        // Parameter block: copy our environment, tail " abc", our FCBs.
        let block = a.addr("block") as u32;
        a.mov(word_ptr(block + 4), ds)?;
        a.mov(word_ptr(block + 8), ds)?;
        a.mov(word_ptr(block + 12), ds)?;
        a.push(ds)?;
        a.pop(es)?;
        let at = a.addr("block");
        a.mov(bx, at)?;
        load_dx(a, "child")?;
        a.mov(ax, 0x4B00)?;
        a.int(0x21)?;
        a.sbb(cx, cx)?;
        let carry = word_at(a, "carry");
        a.mov(carry, cx)?;
        a.mov(ah, 0x4D)?;
        a.int(0x21)?;
        let code = word_at(a, "code");
        a.mov(code, ax)?;
        a.mov(ah, 0x48)?; // largest free block after
        a.mov(bx, 0xFFFF)?;
        a.int(0x21)?;
        let after = word_at(a, "after");
        a.mov(after, bx)?;
        a.mov(ah, 0x40)?; // "!" after the child's "hi": the file position is shared
        a.mov(bx, 5)?;
        a.mov(cx, 1)?;
        load_dx(a, "bang")?;
        a.int(0x21)?;
        a.mov(ah, 0x3E)?;
        a.int(0x21)?;
        exit(a, 0)?;
        for label in ["before", "after", "carry", "code"] {
            a.here(label)?;
            a.dw(&[0xEEEE])?;
        }
        a.here("out")?;
        a.db(b"OUT.TXT\0")?;
        a.here("child")?;
        a.db(b"CHILD.COM\0")?;
        a.here("bang")?;
        a.db(b"!")?;
        a.here("tail")?;
        a.db(b"\x04 abc\r")?;
        let tail = a.addr("tail") as u16;
        a.here("block")?;
        a.dw(&[0, tail, 0, 0x5C, 0, 0x6C, 0])
    });
    let mut m = boot(&[("PARENT.COM", &parent), ("CHILD.COM", &child)], "PARENT.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
    assert_eq!(m.drive.file("OUT.TXT").unwrap(), b"hi!");
    let base = (u32::from(m.dos.psp) << 4) + 0x100;
    let image = m.mem.read_bytes(base, parent.len());
    let fields = image.windows(8).position(|w| w == b"OUT.TXT\0").unwrap() as u32 - 8;
    let word = |i: u32| m.mem.read_u16(base + fields + 2 * i);
    assert_eq!(word(2), 0, "EXEC succeeded");
    assert_eq!(word(3), 0x0014, "exit code 10h + tail length 4, normal termination");
    assert_eq!(word(0), word(1), "the child's memory was freed");
}

#[test]
fn a_sync_point_counts_in_its_program_and_the_programs_it_started() {
    // PARENT.COM runs ONE.COM, then TWO.COM: the same program, loaded at the same place, which notes
    // its segment at 0000:04F0.
    let child = com(|a| {
        a.mov(ax, cs)?;
        a.xor(bx, bx)?;
        a.mov(ds, bx)?;
        a.mov(word_ptr(0x4F0), ax)?;
        exit(a, 0)
    });
    let parent = com(|a| {
        a.mov(ah, 0x4A)?; // keep 64 KB
        a.mov(bx, 0x1000)?;
        a.int(0x21)?;
        for name in ["one", "two"] {
            a.push(cs)?;
            a.pop(ds)?;
            a.push(cs)?;
            a.pop(es)?;
            let block = a.addr("block") as u32;
            a.mov(word_ptr(block + 4), ds)?;
            a.mov(word_ptr(block + 8), ds)?;
            a.mov(word_ptr(block + 12), ds)?;
            let at = a.addr("block");
            a.mov(bx, at)?;
            load_dx(a, name)?;
            a.mov(ax, 0x4B00)?;
            a.int(0x21)?;
        }
        exit(a, 0)?;
        a.here("one")?;
        a.db(b"ONE.COM\0")?;
        a.here("two")?;
        a.db(b"TWO.COM\0")?;
        a.here("tail")?;
        a.db(b"\x00\r")?;
        let tail = a.addr("tail") as u16;
        a.here("block")?;
        a.dw(&[0, tail, 0, 0x5C, 0, 0x6C, 0])
    });
    let files: [(&str, &[u8]); 3] = [("PARENT.COM", &parent), ("ONE.COM", &child), ("TWO.COM", &child)];
    let mut m = boot(&files, "PARENT.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let at = (u32::from(m.mem.read_u16(0x4F0)) << 4) + 0x100;
    for (program, hits) in [(None, 2), (Some("two.com"), 1), (Some("PARENT.COM"), 2), (Some("THREE.COM"), 0)] {
        let mut m = boot(&files, "PARENT.COM");
        m.syncs.set_program(at, program.map(str::to_owned));
        assert_eq!(m.run(SECOND), Stop::Exited(0));
        assert_eq!(m.syncs.get(at).unwrap().hits, hits, "{program:?}");
    }

    // While TWO.COM runs, PARENT.COM is found by name too (an extender's loader runs a program that
    // way), with its own image.
    let mut m = boot(&files, "PARENT.COM");
    let parent = m.dos.psp;
    let parent_image = m.image_range();
    m.syncs.set_program(at, Some("TWO.COM".into()));
    assert!(matches!(m.run_to_sync(at, 1, SECOND), Stop::Sync { .. }));
    assert_eq!(m.process_named("two.com").map(|(psp, _)| psp), Some(m.dos.psp));
    assert_eq!(m.process_named("PARENT.COM"), Some((parent, "PARENT.COM")));
    assert_eq!(m.image_range_of(parent), parent_image.start..parent_image.start + 0x1000 * 16 - 0x100);
    assert_eq!(m.process_named("ONE.COM"), None, "it ended");
}

#[test]
fn exec_loads_an_overlay() {
    let overlay = com(|a| {
        a.mov(ax, 0x1234)?;
        a.retf()
    });
    let program = com(|a| {
        a.mov(ah, 0x4A)?;
        a.mov(bx, 0x1000)?;
        a.int(0x21)?;
        a.mov(ah, 0x48)?;
        a.mov(bx, 0x100)?;
        a.int(0x21)?;
        let block = a.addr("block") as u32;
        a.mov(word_ptr(block), ax)?; // load segment
        a.mov(word_ptr(block + 2), ax)?; // relocation factor
        let entry = a.addr("entry") as u32;
        a.mov(word_ptr(entry + 2), ax)?;
        a.push(ds)?;
        a.pop(es)?;
        let at = a.addr("block");
        a.mov(bx, at)?;
        load_dx(a, "name")?;
        a.mov(ax, 0x4B03)?;
        a.int(0x21)?;
        a.call(dword_ptr(entry))?;
        let result = word_at(a, "result");
        a.mov(result, ax)?;
        exit(a, 0)?;
        a.here("name")?;
        a.db(b"OVL.COM\0")?;
        a.here("block")?;
        a.dw(&[0, 0])?;
        a.here("entry")?;
        a.dw(&[0, 0])?;
        a.here("result")?;
        a.dw(&[0])
    });
    let mut m = boot(&[("MAIN.COM", &program), ("OVL.COM", &overlay)], "MAIN.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let result = m.mem.read_u16((u32::from(m.dos.psp) << 4) + 0x100 + program.len() as u32 - 2);
    assert_eq!(result, 0x1234);
}

// --- VESA BIOS Extensions -------------------------------------------------------------------

/// Where the program's trailing data lives: `results` (16 bytes), `info` (256), `buf` (512).
struct VbeData {
    results: u32,
    info: u32,
    buf: u32,
}

fn vbe_data(m: &Machine, program: &[u8]) -> VbeData {
    let end = (u32::from(m.dos.psp) << 4) + 0x100 + program.len() as u32;
    VbeData { results: end - 784, info: end - 768, buf: end - 512 }
}

/// Stores a 16-bit register in result word `slot`.
fn store<R>(a: &mut Asm, slot: u32, value: R) -> Result<(), IcedError>
where
    CodeAssembler: iced_x86::code_asm::asm_traits::CodeAsmMov<AsmMemoryOperand, R>,
{
    let at = word_ptr(a.addr("results") as u32 + 2 * slot);
    a.mov(at, value)
}

fn vbe_data_area(a: &mut Asm) -> Result<(), IcedError> {
    a.here("results")?;
    a.db(&[0; 16])?;
    a.here("info")?;
    a.db(&[0; 256])?;
    a.here("buf")?;
    a.db(&[0; 512])
}

fn far_to_linear(far: u32) -> u32 {
    ((far >> 16) << 4) + (far & 0xFFFF)
}

fn c_string(m: &Machine, at: u32) -> String {
    let bytes = m.mem.read_bytes(at, 64);
    String::from_utf8_lossy(&bytes[..bytes.iter().position(|&b| b == 0).unwrap()]).into_owned()
}

#[test]
fn vbe_controller_and_mode_information() {
    let program = com(|a| {
        a.push(cs)?;
        a.pop(es)?;
        let buf = a.addr("buf");
        a.mov(dword_ptr(buf as u32), u32::from_le_bytes(*b"VBE2"))?;
        a.mov(di, buf)?;
        a.mov(ax, 0x4F00)?;
        a.int(0x10)?;
        store(a, 0, ax)?;
        let info = a.addr("info");
        a.mov(di, info)?;
        a.mov(ax, 0x4F01)?;
        a.mov(cx, 0x111)?;
        a.int(0x10)?;
        store(a, 1, ax)?;
        a.mov(ax, 0x4F01)?;
        a.mov(cx, 0x10D)?; // 320x200 15-bit: not among the compatible modes
        a.int(0x10)?;
        store(a, 2, ax)?;
        a.mov(ax, 0x4F08)?; // DAC width: not supported (AL = 0)
        a.int(0x10)?;
        store(a, 3, ax)?;
        hold(a)?;
        vbe_data_area(a)
    });
    let mut m = boot(&[("V.COM", &program)], "V.COM");
    m.run(SECOND / 10);
    let VbeData { results, info, buf } = vbe_data(&m, &program);
    let word = |at: u32| m.mem.read_u16(at);
    assert_eq!(word(results), 0x004F);
    assert_eq!(m.mem.read_bytes(buf, 4), b"VESA");
    assert_eq!(word(buf + 4), 0x0200, "VBE 2.0");
    assert_eq!(c_string(&m, far_to_linear(m.mem.read_u32(buf + 6))), "S3 Incorporated. Trio64");
    assert_eq!(c_string(&m, far_to_linear(m.mem.read_u32(buf + 0x16))), "Clockwork", "vendor");
    assert_eq!(word(buf + 0x12), 64, "4 MB in 64 KB blocks");
    let list = far_to_linear(m.mem.read_u32(buf + 0x0E));
    let modes: Vec<u16> = (0..).map(|i| word(list + 2 * i)).take_while(|&mode| mode != 0xFFFF).collect();
    assert_eq!(&modes[..3], [0x100, 0x101, 0x102]);
    assert!(modes.contains(&0x111) && modes.contains(&0x215));
    assert!(!modes.contains(&0x10D) && !modes.contains(&0x150), "left out by DOSBox's compatible filter");
    assert!(!modes.contains(&0x11B), "1280x1024x32 needs more than 4 MB");

    assert_eq!(word(results + 2), 0x004F);
    assert_eq!(word(info) & 0x9B, 0x9B, "supported, color, graphics, linear frame buffer");
    assert_eq!((word(info + 0x10), word(info + 0x12), word(info + 0x14)), (1280, 640, 480));
    assert_eq!((word(info + 0x04), word(info + 0x06), word(info + 0x08)), (64, 64, 0xA000), "64 KB window at A000h");
    assert_eq!(m.mem.read_u32(info + 0x0C), 0xF000_9220, "window function");
    assert_eq!(m.mem.read_u8(info + 0x19), 16, "bits per pixel");
    assert_eq!(m.mem.read_bytes(info + 0x1F, 6), [5, 11, 6, 5, 5, 0], "5:6:5 masks");
    assert_eq!(m.mem.read_u32(info + 0x28), 0xC000_0000, "linear frame buffer");
    assert_eq!(word(results + 4), 0x014F);
    assert_eq!(word(results + 6) & 0xFF, 0x00, "unsupported functions return AL = 0");
}

#[test]
fn vbe_256_color_mode_through_banks() {
    let program = com(|a| {
        a.mov(ax, 0x4F02)?;
        a.mov(bx, 0x101)?; // 640x480, 256 colors
        a.int(0x10)?;
        store(a, 0, ax)?;
        a.push(cs)?;
        a.pop(es)?;
        let info = a.addr("info");
        a.mov(di, info)?;
        a.mov(ax, 0x4F01)?;
        a.mov(cx, 0x101)?;
        a.int(0x10)?;
        a.mov(ax, 0xA000)?;
        a.mov(es, ax)?;
        a.mov(byte_ptr(5u32).es(), 7)?; // bank 0: (5, 0)
        a.mov(ax, 0x4F05)?;
        a.xor(bx, bx)?;
        a.mov(dx, 1)?;
        a.int(0x10)?;
        a.mov(byte_ptr(10u32).es(), 9)?; // bank 1: byte 65546 = (266, 102)
        a.xor(bx, bx)?;
        a.mov(dx, 2)?;
        a.call(dword_ptr(info as u32 + 0x0C))?; // the far-callable window function
        a.mov(byte_ptr(20u32).es(), 11)?; // bank 2: byte 131092 = (532, 204)
        a.mov(ax, 0x4F05)?;
        a.mov(bx, 0x0100)?; // get the bank
        a.int(0x10)?;
        store(a, 1, dx)?;
        a.mov(ax, 0x4F03)?;
        a.int(0x10)?;
        store(a, 2, bx)?;
        hold(a)?;
        vbe_data_area(a)
    });
    let mut m = boot(&[("V.COM", &program)], "V.COM");
    m.run(SECOND / 10);
    let results = vbe_data(&m, &program).results;
    assert_eq!(m.mem.read_u16(results), 0x004F);
    assert_eq!(m.mem.read_u16(results + 2), 2, "the window function switched to bank 2");
    assert_eq!(m.mem.read_u16(results + 4), 0x101, "4F03h reports the mode");
    assert_eq!(m.mem.read_u8(0x449), 0x69, "BIOS mode byte: 101h - 98h, as S3 BIOSes keep it");
    let vga = &m.devices.vga;
    assert_eq!(vga.display_kind(), dos::vga::DisplayKind::Svga256);
    let picture = vga.picture(&[0; dos::vga_draw::LOW_RAM_SIZE]);
    assert_eq!((picture.width, picture.height), (640, 480));
    assert_eq!(picture.pixels[5], 7);
    assert_eq!(picture.pixels[102 * 640 + 266], 9);
    assert_eq!(picture.pixels[204 * 640 + 532], 11);
    assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
}

#[test]
fn vbe_high_color_scan_lines_and_display_start() {
    let program = com(|a| {
        a.mov(ax, 0x4F02)?;
        a.mov(bx, 0x111)?; // 640x480, 5:6:5
        a.int(0x10)?;
        a.mov(ax, 0xA000)?;
        a.mov(es, ax)?;
        a.mov(word_ptr(2u32).es(), 0xF800)?; // (1, 0) red
        a.mov(word_ptr(1280u32).es(), 0x07E0)?; // (0, 1) green
        a.mov(ax, 0x4F06)?;
        a.mov(bl, 1)?; // get the scan line length
        a.int(0x10)?;
        store(a, 0, bx)?;
        store(a, 1, cx)?;
        store(a, 2, dx)?;
        a.mov(ax, 0x4F07)?;
        a.xor(bx, bx)?;
        a.xor(cx, cx)?;
        a.mov(dx, 1)?; // start the display at line 1
        a.int(0x10)?;
        store(a, 3, ax)?;
        a.mov(ax, 0x4F07)?;
        a.mov(bx, 1)?;
        a.int(0x10)?;
        store(a, 4, cx)?;
        store(a, 5, dx)?;
        hold(a)?;
        vbe_data_area(a)
    });
    let mut m = boot(&[("V.COM", &program)], "V.COM");
    m.run(SECOND / 10);
    let results = vbe_data(&m, &program).results;
    let word = |i: u32| m.mem.read_u16(results + 2 * i);
    assert_eq!((word(0), word(1), word(2)), (1280, 640, 3276), "bytes, pixels, lines that fit in 4 MB");
    assert_eq!(word(3), 0x004F);
    assert_eq!((word(4), word(5)), (0, 1), "display start read back");
    let (width, height, rgb) = m.screen_rgb();
    assert_eq!((width, height), (640, 480));
    assert_eq!(rgb[..3], [0, 255, 0], "line 1 is now at the top");
    assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
}

#[test]
fn vbe_linear_frame_buffer_from_unreal_mode() {
    let program = com(|a| {
        a.mov(ax, 0x4F02)?;
        a.mov(bx, 0x4101)?; // 640x480x8 with the linear frame buffer
        a.int(0x10)?;
        a.cli()?;
        // A flat 4 GB data segment in FS, kept after returning to real mode.
        a.xor(eax, eax)?;
        a.mov(ax, cs)?;
        a.shl(eax, 4)?;
        let gdt = a.addr("gdt");
        a.add(eax, gdt)?;
        let gdtr = a.addr("gdtr") as u32;
        a.mov(dword_ptr(gdtr + 2), eax)?;
        a.lgdt(ptr(gdtr))?;
        a.mov(eax, cr0)?;
        a.or(al, 1)?;
        a.mov(cr0, eax)?;
        a.mov(bx, 8)?;
        a.mov(fs, bx)?;
        a.and(al, 0xFE)?;
        a.mov(cr0, eax)?;
        a.sti()?;
        a.mov(esi, 0xC000_0000u32 + 2 * 640 + 3)?;
        a.mov(byte_ptr(esi).fs(), 13)?; // (3, 2)
        hold(a)?;
        a.here("gdtr")?;
        a.dw(&[15, 0, 0])?;
        a.here("gdt")?;
        a.dq(&[0, 0x00CF_9200_0000_FFFF])
    });
    let mut m = boot(&[("V.COM", &program)], "V.COM");
    m.run(SECOND / 10);
    let picture = m.devices.vga.picture(&[0; dos::vga_draw::LOW_RAM_SIZE]);
    assert_eq!(picture.pixels[2 * 640 + 3], 13);
    assert_eq!(m.devices.vga.vesa_mode, Some(0x4101));
    assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
}

#[test]
fn standard_mode_after_vesa_mode_resets_the_card() {
    let program = com(|a| {
        a.mov(ax, 0x4F02)?;
        a.mov(bx, 0x112)?; // 640x480x32
        a.int(0x10)?;
        a.mov(ax, 0x0003)?;
        a.int(0x10)?;
        print(a, "msg")?;
        hold(a)?;
        a.here("msg")?;
        a.db(b"back$")
    });
    let mut m = boot(&[("V.COM", &program)], "V.COM");
    m.run(SECOND / 10);
    let vga = &m.devices.vga;
    assert_eq!(vga.display_kind(), dos::vga::DisplayKind::Text);
    assert_eq!((vga.s3.reg_31, vga.s3.misc_control_2, vga.s3.bank), (0x05, 0, 0));
    assert_eq!(vga.vesa_mode, None);
    assert_eq!(m.screen_text().unwrap()[0].trim_end(), "back");
}

#[test]
fn vbe_protected_mode_interface_is_valid_32_bit_code() {
    use iced_x86::{Decoder, DecoderOptions, Mnemonic};
    let program = com(|a| {
        a.mov(ax, 0x4F0A)?;
        a.xor(bx, bx)?;
        a.int(0x10)?;
        store(a, 0, ax)?;
        store(a, 1, es)?;
        store(a, 2, di)?;
        store(a, 3, cx)?;
        hold(a)?;
        vbe_data_area(a)
    });
    let mut m = boot(&[("V.COM", &program)], "V.COM");
    m.run(SECOND / 10);
    let results = vbe_data(&m, &program).results;
    let word = |i: u32| m.mem.read_u16(results + 2 * i);
    assert_eq!(word(0), 0x004F);
    let table = (u32::from(word(1)) << 4) + u32::from(word(2));
    let code = m.mem.read_bytes(table, usize::from(word(3)));
    let offset = |i: usize| usize::from(u16::from_le_bytes([code[2 * i], code[2 * i + 1]]));
    let (window, start, palette) = (offset(0), offset(1), offset(2));
    assert_eq!(offset(3), 0, "no port list");
    assert!(8 <= window && window < start && start < palette && palette < code.len());
    for (from, to) in [(window, start), (start, palette), (palette, code.len())] {
        let mut decoder = Decoder::with_ip(32, &code[from..to], from as u64, DecoderOptions::NONE);
        let instructions: Vec<_> = decoder.iter().collect();
        let boundaries: Vec<u64> = instructions.iter().map(|i| i.ip()).chain([to as u64]).collect();
        for i in &instructions {
            assert!(!i.is_invalid(), "invalid code at {:#x}", i.ip());
            if matches!(i.mnemonic(), Mnemonic::Je | Mnemonic::Jne | Mnemonic::Jecxz | Mnemonic::Loop) {
                assert!(boundaries.contains(&i.near_branch_target()), "branch at {:#x} into an instruction", i.ip());
            }
        }
        assert_eq!(instructions.last().unwrap().mnemonic(), Mnemonic::Ret);
        let pushes = instructions.iter().filter(|i| i.mnemonic() == Mnemonic::Push).count();
        let pops = instructions.iter().filter(|i| i.mnemonic() == Mnemonic::Pop).count();
        assert_eq!(pushes, pops, "balanced stack in the function at {from:#x}");
    }
}

// --- Game port --------------------------------------------------------------------------------

/// Times joystick A's X axis by polling 201h after a write, then asks the BIOS (INT 15h AH=84h)
/// for the positions and the buttons. Results: loops, AX, BX, flags, buttons AX.
fn game_port_program() -> Vec<u8> {
    com(|a| {
        a.mov(dx, 0x201)?;
        a.out(dx, al)?;
        a.xor(cx, cx)?;
        a.here("poll")?;
        a.in_(al, dx)?;
        a.test(al, 1)?;
        let done = a.l("done");
        a.jz(done)?;
        a.inc(cx)?;
        let poll = a.l("poll");
        a.jnz(poll)?;
        a.here("done")?;
        store(a, 0, cx)?;
        a.mov(ah, 0x84)?;
        a.mov(dx, 1)?;
        a.int(0x15)?;
        store(a, 1, ax)?;
        store(a, 2, bx)?;
        a.pushf()?;
        a.pop(ax)?;
        store(a, 3, ax)?;
        a.mov(ah, 0x84)?;
        a.xor(dx, dx)?;
        a.int(0x15)?;
        store(a, 4, ax)?;
        hold(a)?;
        vbe_data_area(a)
    })
}

fn run_game_port(joysticks: u8, stick: Option<dos::Input>) -> [u16; 5] {
    let program = game_port_program();
    let mut drive = Vfs::new();
    drive.insert_file("J.COM", program.clone());
    let mut m = Machine::with_program(Config { joysticks, ..Config::default() }, drive, "J.COM", "").unwrap();
    if let Some(input) = stick {
        m.queue_input(0, input);
    }
    m.run(SECOND / 4);
    let results = vbe_data(&m, &program).results;
    std::array::from_fn(|i| m.mem.read_u16(results + 2 * i as u32))
}

#[test]
fn joystick_axes_time_out_by_position() {
    let right = run_game_port(1, Some(dos::Input::Joystick { stick: 0, x: 32767, y: 0, buttons: 1 }));
    let left = run_game_port(1, Some(dos::Input::Joystick { stick: 0, x: -32767, y: 0, buttons: 0 }));
    // Five instructions and a 1 µs port read a loop at 8 M instructions/s (13 cycles): 1.13 ms is
    // about 700 loops, 20 µs about 12.
    assert!((650..750).contains(&right[0]), "full right: {} loops", right[0]);
    assert!(left[0] < 20, "full left: {} loops", left[0]);
    assert_eq!((right[1], right[2]), (255, 128), "BIOS positions");
    assert_eq!(right[3] & 1, 0, "CF clear");
    assert_eq!(right[4] & 0xFF, 0xE0, "button 1 of stick A pressed (bit 4 clear)");
}

#[test]
fn no_joystick_reads_ff_and_the_bios_says_so() {
    let none = run_game_port(0, None);
    assert_eq!(none[0], 0, "the axis bit never clears: the loop ends when CX wraps");
    assert_eq!(none[3] & 1, 1, "CF set: no joystick");
    assert_eq!(none[4], 0x00F0);
}

// --- FCB file functions -----------------------------------------------------------------------

/// `mov ah, function; int 21h` with DX = the FCB at `info`, storing AX in result `slot`.
fn fcb_call(a: &mut Asm, function: u8, slot: Option<u32>) -> Result<(), IcedError> {
    let fcb = a.addr("info");
    a.mov(dx, fcb)?;
    a.mov(ah, u32::from(function))?;
    a.int(0x21)?;
    match slot {
        Some(slot) => store(a, slot, ax),
        None => Ok(()),
    }
}

/// Parses the ASCIIZ name at `name` into the FCB at `info` + `at` (INT 21h AH=29h).
fn fcb_parse(a: &mut Asm, name: &'static str, at: u32) -> Result<(), IcedError> {
    a.push(cs)?;
    a.pop(es)?;
    let (source, fcb) = (a.addr(name), a.addr("info"));
    a.mov(si, source)?;
    a.mov(di, fcb + at as i32)?;
    a.mov(ax, 0x2900)?;
    a.int(0x21)
}

#[test]
fn fcb_create_write_read_and_rename() {
    let program = com(|a| {
        fcb_parse(a, "name", 0)?;
        let (buf, info) = (a.addr("buf") as u32, a.addr("info") as u32);
        a.mov(dx, buf as i32)?;
        a.mov(ah, 0x1A)?; // the DTA is the record buffer
        a.int(0x21)?;
        fcb_call(a, 0x16, Some(0))?; // create
        a.mov(word_ptr(info + 0x0E), 4)?; // 4-byte records
        a.mov(dword_ptr(buf), u32::from_le_bytes(*b"ABCD"))?;
        fcb_call(a, 0x15, Some(1))?; // sequential write
        a.mov(dword_ptr(buf), u32::from_le_bytes(*b"EFGH"))?;
        fcb_call(a, 0x15, None)?;
        fcb_call(a, 0x10, Some(2))?; // close
        fcb_call(a, 0x0F, Some(3))?; // open (128-byte records again)
        a.mov(word_ptr(info + 0x0E), 4)?;
        fcb_call(a, 0x23, None)?; // file size: 2 records into the random record
        a.mov(eax, dword_ptr(info + 0x21))?;
        a.mov(dword_ptr(buf + 0x100), eax)?;
        a.mov(dword_ptr(info + 0x21), 1)?;
        fcb_call(a, 0x21, Some(4))?; // random read of record 1
        a.mov(eax, dword_ptr(buf))?;
        a.mov(dword_ptr(buf + 0x104), eax)?;
        a.mov(dword_ptr(info + 0x21), 0)?;
        a.mov(cx, 3)?;
        fcb_call(a, 0x27, Some(5))?; // block read of 3 records: 2 there
        store(a, 6, cx)?;
        fcb_call(a, 0x10, None)?;
        fcb_parse(a, "renamed", 0x10)?; // the new name at 11h
        fcb_call(a, 0x17, Some(7))?;
        hold(a)?;
        a.here("name")?;
        a.db(b"TEST.DAT\0")?;
        a.here("renamed")?;
        a.db(b"RENAMED.DAT\0")?;
        vbe_data_area(a)
    });
    let mut m = boot(&[("F.COM", &program)], "F.COM");
    m.run(SECOND / 10);
    let VbeData { results, info, buf } = vbe_data(&m, &program);
    let result = |i: u32| m.mem.read_u8(results + 2 * i);
    assert_eq!([result(0), result(1), result(2), result(3)], [0, 0, 0, 0], "create, write, close, open");
    assert_eq!(m.mem.read_u32(buf + 0x100), 2, "file size in 4-byte records");
    assert_eq!((result(4), m.mem.read_bytes(buf + 0x104, 4)), (0, b"EFGH".to_vec()), "random read");
    assert_eq!((result(5), m.mem.read_u16(results + 12)), (1, 2), "block read: 2 records, then no data");
    assert_eq!(m.mem.read_bytes(buf, 8), b"ABCDEFGH");
    assert_eq!(m.mem.read_u32(info + 0x21), 2, "the random record follows the block read");
    assert_eq!(result(7), 0, "rename");
    assert_eq!(m.drive.file("RENAMED.DAT"), Some(&b"ABCDEFGH"[..]));
    assert!(!m.drive.is_file("TEST.DAT"));
    assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
}

#[test]
fn fcb_search_and_delete_with_wildcards() {
    let program = com(|a| {
        fcb_parse(a, "pattern", 0)?;
        let buf = a.addr("buf");
        a.mov(dx, buf)?;
        a.mov(ah, 0x1A)?;
        a.int(0x21)?;
        fcb_call(a, 0x11, Some(0))?; // find first: A.TXT
        a.mov(eax, dword_ptr(buf as u32 + 1))?;
        a.mov(dword_ptr(buf as u32 + 0x100), eax)?;
        fcb_call(a, 0x12, Some(1))?; // B.TXT
        a.mov(eax, dword_ptr(buf as u32 + 1))?;
        a.mov(dword_ptr(buf as u32 + 0x104), eax)?;
        fcb_call(a, 0x12, Some(2))?; // no more
        fcb_call(a, 0x13, Some(3))?; // delete both
        hold(a)?;
        a.here("pattern")?;
        a.db(b"*.TXT\0")?;
        vbe_data_area(a)
    });
    let mut m = boot(&[("S.COM", &program), ("A.TXT", b"1"), ("B.TXT", b"22"), ("C.DAT", b"333")], "S.COM");
    m.run(SECOND / 10);
    let VbeData { results, buf, .. } = vbe_data(&m, &program);
    let result = |i: u32| m.mem.read_u8(results + 2 * i);
    assert_eq!([result(0), result(1), result(2), result(3)], [0, 0, 0xFF, 0]);
    assert_eq!(m.mem.read_bytes(buf + 0x100, 4), b"A   ", "the DTA holds an FCB with the name padded");
    assert_eq!(m.mem.read_bytes(buf + 0x104, 4), b"B   ");
    assert_eq!(m.mem.read_u32(buf + 0x1D), 2, "B.TXT's size in the directory entry");
    assert!(!m.drive.is_file("A.TXT") && !m.drive.is_file("B.TXT") && m.drive.is_file("C.DAT"));
}

#[test]
fn reading_no_bytes_from_the_console_does_not_wait() {
    // Arena polls the console this way (INT 21h AH=3Fh on handle 2 with CX=0) before its first
    // dungeon; DOS returns at once.
    let program = com(|a| {
        a.mov(ah, 0x3F)?;
        a.mov(bx, 2)?;
        a.xor(cx, cx)?;
        a.mov(dx, 0x200)?;
        a.int(0x21)?;
        a.mov(ah, 0x4C)?; // exit code: the bytes read (AL)
        a.int(0x21)
    });
    let mut m = boot(&[("R.COM", &program)], "R.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
}

#[test]
fn contracts_from_a_run() {
    let offsets = std::cell::RefCell::new(BTreeMap::new());
    let program = com(|a| {
        for name in ["add_arg", "get_flag", "keeps", "pascal_one", "read_global", "result"] {
            offsets.borrow_mut().insert(name, (a.addr(name) as u32).wrapping_sub(0x100));
        }
        // The stack starts at the top of the segment: the first push wraps SP to FFFEh.
        a.xor(sp, sp)?;
        a.mov(ax, 5)?;
        a.mov(bx, 7)?;
        a.push(bx)?;
        let target = a.l("add_arg");
        a.call(target)?;
        a.add(sp, 2)?;
        let result = word_at(a, "result");
        a.mov(result, ax)?;
        a.clc()?; // (ADD SP,2 wrapped and set it)
        let target = a.l("get_flag");
        a.call(target)?;
        let target = a.l("after_flag");
        a.jc(target)?;
        a.here("after_flag")?;
        let target = a.l("keeps");
        a.call(target)?;
        a.xor(ax, ax)?; // (a result is seen when it changes)
        let target = a.l("read_global");
        a.call(target)?;
        a.push(ax)?; // a result pushed as an argument is used
        let target = a.l("pascal_one");
        a.call(target)?;
        exit(a, 0)?;
        // add_arg(AX, word): AX + the argument
        a.here("add_arg")?;
        a.push(bp)?;
        a.mov(bp, sp)?;
        a.add(ax, word_ptr(bp + 4))?;
        a.leave()?;
        a.ret()?;
        // get_flag: CF set
        a.here("get_flag")?;
        a.stc()?;
        a.ret()?;
        // keeps: uses SI, but saves it
        a.here("keeps")?;
        a.push(si)?;
        a.mov(si, 1)?;
        a.pop(si)?;
        a.ret()?;
        // read_global: AX = result
        a.here("read_global")?;
        let result = word_at(a, "result");
        a.mov(ax, result)?;
        a.ret()?;
        // pascal_one(word): pops its argument
        a.here("pascal_one")?;
        a.push(bp)?;
        a.mov(bp, sp)?;
        a.mov(dx, word_ptr(bp + 4))?;
        a.pop(bp)?;
        a.ret_1(2)?;
        a.here("result")?;
        a.dw(&[0])
    });
    let mut m = boot(&[("C.COM", &program)], "C.COM");
    m.start_contracts();
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let contracts = m.probes.contracts.take().unwrap();
    let offsets = offsets.into_inner();
    let contract = |name: &str| contracts.functions[&offsets[name]].clone();
    use dos::contracts::{NEAR, flag_names, register_names};
    let add = contract("add_arg");
    assert_eq!((add.calls, add.abnormal, add.returns), (1, 0, NEAR), "the frame closed despite the wrap");
    assert_eq!(register_names(add.inputs), ["AX"]);
    assert_eq!(register_names(add.outputs), ["AL"], "AH didn't change (5 + 7)");
    assert_eq!((add.arg_bytes, add.pops.iter().copied().collect::<Vec<_>>()), (2, vec![0]));
    let flag = contract("get_flag");
    assert_eq!((register_names(flag.inputs).len(), flag_names(flag.outputs)), (0, vec!["CF"]));
    let keeps = contract("keeps");
    let si_units = dos::contracts::units::word(6);
    assert!(keeps.inputs == 0 && keeps.preserved & si_units == si_units, "{keeps:?}");
    let global = contract("read_global");
    assert_eq!(global.reads.iter().copied().collect::<Vec<_>>(), [offsets["result"], offsets["result"] + 1]);
    assert_eq!(register_names(global.inputs), ["DS"]);
    assert_eq!(register_names(global.outputs), ["AL"], "pushed as pascal_one's argument, and read there");
    let pascal = contract("pascal_one");
    assert_eq!((pascal.arg_bytes, pascal.pops.iter().copied().collect::<Vec<_>>()), (2, vec![2]));
    assert_eq!(pascal.inputs, 0);
}

/// Prints its command tail and exits with code 3.
fn tail_echo() -> Vec<u8> {
    com(|a| {
        a.mov(ah, 0x40)?;
        a.mov(bx, 1)?;
        a.xor(ch, ch)?;
        a.mov(cl, byte_ptr(0x80))?;
        a.mov(dx, 0x81)?;
        a.int(0x21)?;
        exit(a, 3)
    })
}

/// Copies standard input to standard output.
fn cat() -> Vec<u8> {
    com(|a| {
        a.here("loop")?;
        a.mov(ah, 0x3F)?;
        a.xor(bx, bx)?;
        a.mov(cx, 1)?;
        load_dx(a, "byte")?;
        a.int(0x21)?;
        let done = a.l("done");
        a.jc(done)?;
        a.or(ax, ax)?;
        a.jz(done)?;
        a.mov(ah, 0x40)?;
        a.mov(bx, 1)?;
        a.mov(cx, 1)?;
        load_dx(a, "byte")?;
        a.int(0x21)?;
        let back = a.l("loop");
        a.jmp(back)?;
        a.here("done")?;
        exit(a, 0)?;
        a.here("byte")?;
        a.db(&[0])
    })
}

const GAME_BAT: &str = "@ECHO OFF\r
ECHO Hello\r
SET X=1\r
IF \"%X%\"==\"1\" ECHO X is one\r
IF EXIST TAIL.COM ECHO found\r
TAIL arg1\r
IF ERRORLEVEL 3 ECHO three\r
IF NOT ERRORLEVEL 4 ECHO not four\r
FOR %%F IN (A B) DO ECHO item %%F\r
GOTO skip\r
ECHO not shown\r
:skip\r
CALL SUB.BAT x y\r
ECHO back in %0\r
ECHO line> OUT.TXT\r
TYPE OUT.TXT\r
ECHO piped | CAT\r
MD NEWDIR\r
CD NEWDIR\r
CD\r
CD ..\r
COPY OUT.TXT NEWDIR\\COPY.TXT\r
DEL OUT.TXT\r
IF NOT EXIST OUT.TXT ECHO deleted\r
NOSUCH\r
";

#[test]
fn batch_files_run_in_the_built_in_shell() {
    let (tail, cat) = (tail_echo(), cat());
    let mut m = boot(
        &[
            ("GAME.BAT", GAME_BAT.as_bytes()),
            ("SUB.BAT", b"ECHO sub %1 %2\r\nSHIFT\r\nECHO shifted %1\r\n"),
            ("TAIL.COM", &tail),
            ("CAT.COM", &cat),
        ],
        "GAME.BAT",
    );
    assert_eq!(m.run(10 * SECOND), Stop::Exited(0));
    assert_eq!(
        console(&m),
        "Hello\r\nX is one\r\nfound\r\n arg1three\r\nnot four\r\nitem A\r\nitem B\r\nsub x y\r\nshifted y\r\n\
         back in GAME.BAT\r\nline\r\npiped\r\nC:\\NEWDIR\r\n        1 file(s) copied\r\ndeleted\r\n\
         Bad command or file name\r\n"
    );
    assert!(m.drive.is_file("NEWDIR\\COPY.TXT") && !m.drive.is_file("OUT.TXT"));
    assert_eq!(m.drive.file("NEWDIR\\COPY.TXT"), Some(&b"line\r\n"[..]));
    assert!(!m.drive.is_file("PIPE0001.TMP"), "the pipe's file is gone");
}

#[test]
fn the_shell_shows_and_sets_the_date_and_time_and_attributes() {
    let script = "@ECHO OFF\r\nDATE /T\r\nDATE\r\nDATE 02-29-2000\r\nDATE /T\r\nDATE 02-30-2000\r\nTIME 13:14:15\r\n\
                  TIME /T\r\nATTRIB +R NOTE.TXT\r\nATTRIB SUB\\*.*\r\nATTRIB -R NOTE.TXT\r\nATTRIB NONE.TXT\r\n";
    let mut m = boot(&[("T.BAT", script.as_bytes()), ("NOTE.TXT", b"note"), ("SUB\\INNER.TXT", b"inner")], "T.BAT");
    assert_eq!(m.run(10 * SECOND), Stop::Exited(0));
    assert_eq!(
        console(&m),
        "01-01-1994\r\nCurrent date: Sat 01-01-1994\r\nRun date MM-DD-YYYY to change the current date.\r\n\
         02-29-2000\r\nThe specified date is not correct.\r\n13:14:15\r\n\
         \x20 A    R     C:\\NOTE.TXT\r\n  A          C:\\SUB\\INNER.TXT\r\n\
         \x20 A          C:\\NOTE.TXT\r\nFile not found - 'NONE.TXT'\r\n"
    );
}

#[test]
fn dos_gives_country_information() {
    // AH=38h into a buffer; the exit code is the date separator.
    let program = com(|a| {
        a.mov(ax, 0x3800)?;
        let info = a.addr("info");
        a.mov(dx, info)?;
        a.int(0x21)?;
        a.mov(al, byte_ptr(info as u32 + 0x0B))?;
        a.mov(ah, 0x4C)?;
        a.int(0x21)?;
        a.here("info")?;
        a.db(&[0; 0x22])
    });
    let mut m = boot(&[("P.COM", &program)], "P.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(b'-'));
    assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
}

#[test]
fn a_program_runs_a_command_through_command_com() {
    // system("ECHO from shell"): shrink, EXEC C:\COMMAND.COM /C ..., then go on.
    let program = com(|a| {
        a.mov(ah, 0x4A)?;
        a.mov(bx, 0x1000)?;
        a.int(0x21)?;
        let (block, tail) = (a.addr("block") as u32, a.addr("tail"));
        a.mov(word_ptr(block + 2), tail)?;
        a.mov(word_ptr(block + 4), cs)?;
        a.mov(word_ptr(block + 6), 0x5C)?;
        a.mov(word_ptr(block + 8), cs)?;
        a.mov(word_ptr(block + 10), 0x6C)?;
        a.mov(word_ptr(block + 12), cs)?;
        a.mov(ax, 0x4B00)?;
        load_dx(a, "path")?;
        a.mov(bx, block as i32)?;
        a.int(0x21)?;
        let failed = a.l("failed");
        a.jc(failed)?;
        print(a, "back")?;
        exit(a, 0)?;
        a.here("failed")?;
        exit(a, 1)?;
        a.here("path")?;
        a.db(b"C:\\COMMAND.COM\0")?;
        a.here("tail")?;
        let text = b" /C ECHO from shell";
        a.db(&[text.len() as u8])?;
        a.db(text)?;
        a.db(&[0x0D])?;
        a.here("back")?;
        a.db(b"back$")?;
        a.here("block")?;
        a.db(&[0; 14])
    });
    let mut m = boot(&[("SYS.COM", &program)], "SYS.COM");
    assert_eq!(m.run(10 * SECOND), Stop::Exited(0));
    assert_eq!(console(&m), "from shell\r\nback");
    assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
}

#[test]
fn the_shell_reads_commands_at_its_prompt() {
    let mut m = boot(&[], "COMMAND.COM");
    m.type_text(SECOND / 10, "echo typed\rver\rexit\r", SECOND / 50);
    assert_eq!(m.run(20 * SECOND), Stop::Exited(0));
    let text = console(&m);
    assert!(text.starts_with("C:\\>echo typed\r\ntyped\r\nC:\\>"), "{text:?}");
    assert!(text.contains("MS-DOS Version 5.00"), "{text:?}");
}

/// A DPMI client: finds the host, enters protected mode (as a 32-bit client if `bits32`) and
/// uses it: reflected INT 21h, INT 31h version, a descriptor on a memory block, a simulated
/// real-mode INT 21h AH=09h, timer interrupts reflected to the BIOS, then its own handler for
/// IRQ 0. With `fault`, it loads DS with an empty descriptor after entering.
fn dpmi_client(bits32: bool, fault: bool) -> Vec<u8> {
    com(|a| {
        let fail = a.l("fail");
        a.mov(ah, 0x4A)?;
        a.mov(bx, 0x1000)?;
        a.int(0x21)?;
        a.mov(ax, 0x1687)?;
        a.int(0x2F)?;
        a.or(ax, ax)?;
        a.jnz(fail)?;
        let entry = a.addr("entry") as u32;
        a.mov(word_ptr(entry), di)?;
        a.mov(word_ptr(entry + 2), es)?;
        let rmseg = a.addr("rmseg") as u32;
        a.mov(word_ptr(rmseg), cs)?;
        a.mov(bx, si)?;
        a.mov(ah, 0x48)?;
        a.int(0x21)?;
        a.jc(fail)?;
        a.mov(es, ax)?;
        a.mov(ax, i32::from(bits32))?;
        a.call(dword_ptr(entry))?;
        a.jc(fail)?;
        // Protected mode.
        a.mov(ah, 2)?;
        a.mov(dl, b'P' as i32)?;
        a.int(0x21)?;
        if fault {
            a.mov(ax, 0x38)?; // an empty GDT entry: #GP
            a.mov(ds, ax)?;
        }
        a.mov(ax, 0x0400)?;
        a.int(0x31)?;
        a.cmp(ax, 0x005A)?;
        a.jne(fail)?;
        // A descriptor for a 64 KB block.
        let sel = a.addr("sel") as u32;
        a.xor(ax, ax)?;
        a.mov(cx, 1)?;
        a.int(0x31)?;
        a.jc(fail)?;
        a.mov(word_ptr(sel), ax)?;
        a.mov(ax, 0x0501)?;
        a.mov(bx, 1)?;
        a.xor(cx, cx)?;
        a.int(0x31)?;
        a.jc(fail)?;
        a.mov(dx, cx)?;
        a.mov(cx, bx)?;
        a.mov(bx, word_ptr(sel))?;
        a.mov(ax, 7)?;
        a.int(0x31)?;
        a.jc(fail)?;
        a.mov(ax, 8)?;
        a.mov(bx, word_ptr(sel))?;
        a.xor(cx, cx)?;
        a.mov(dx, 0xFFFF)?;
        a.int(0x31)?;
        a.jc(fail)?;
        a.mov(es, word_ptr(sel))?;
        a.mov(word_ptr(0x1234).es(), 0x5A5A)?;
        a.mov(ax, word_ptr(0x1234).es())?;
        a.cmp(ax, 0x5A5A)?;
        a.jne(fail)?;
        a.mov(ah, 2)?;
        a.mov(dl, b'M' as i32)?;
        a.int(0x21)?;
        // INT 21h AH=09h in real mode: DS:DX = the program's segment and the message.
        let regs = a.addr("regs") as u32;
        a.mov(dword_ptr(regs + 0x1C), 0x0900)?;
        let msg = a.addr("msg");
        a.mov(dword_ptr(regs + 0x14), msg)?;
        a.mov(ax, word_ptr(rmseg))?;
        a.mov(word_ptr(regs + 0x24), ax)?;
        a.push(ds)?;
        a.pop(es)?;
        a.xor(edi, edi)?;
        a.mov(di, regs as i32)?;
        a.mov(bx, 0x21)?;
        a.xor(cx, cx)?;
        a.mov(ax, 0x0300)?;
        a.int(0x31)?;
        a.jc(fail)?;
        // The BIOS tick count moves: timer interrupts reach the real-mode handler.
        a.mov(ax, 2)?;
        a.mov(bx, 0x40)?;
        a.int(0x31)?;
        a.jc(fail)?;
        a.mov(es, ax)?;
        a.mov(si, word_ptr(0x6C).es())?;
        a.sti()?;
        a.here("tick")?;
        a.mov(ax, word_ptr(0x6C).es())?;
        a.cmp(ax, si)?;
        let tick = a.l("tick");
        a.je(tick)?;
        a.mov(ah, 2)?;
        a.mov(dl, b'T' as i32)?;
        a.int(0x21)?;
        // The client's own IRQ 0 handler (0205h): it counts.
        a.mov(ax, 0x0205)?;
        a.mov(bl, 8)?;
        a.mov(cx, cs)?;
        a.xor(edx, edx)?;
        let handler = a.addr("handler");
        a.mov(dx, handler)?;
        a.int(0x31)?;
        a.jc(fail)?;
        a.here("count")?;
        let counter = a.addr("counter") as u32;
        a.cmp(word_ptr(counter), 3)?;
        let count = a.l("count");
        a.jb(count)?;
        a.mov(ah, 2)?;
        a.mov(dl, b'H' as i32)?;
        a.int(0x21)?;
        exit(a, 0)?;
        a.here("fail")?;
        exit(a, 1)?;
        a.here("handler")?;
        a.push(ax)?;
        a.inc(word_ptr(counter).ss())?;
        a.mov(al, 0x20)?;
        a.out(0x20, al)?;
        a.pop(ax)?;
        if bits32 {
            a.iretd()?;
        } else {
            a.iret()?;
        }
        a.here("entry")?;
        a.dw(&[0, 0])?;
        a.here("rmseg")?;
        a.dw(&[0])?;
        a.here("sel")?;
        a.dw(&[0])?;
        a.here("counter")?;
        a.dw(&[0])?;
        a.here("msg")?;
        a.db(b"hello from RM$")?;
        a.here("regs")?;
        a.db(&[0; 0x32])
    })
}

/// A 16-bit DPMI client whose handlers chain: it hooks INT 21h and prints through its hook, which
/// counts and chains to the handler 0204h gave it; then it hooks IRQ 0 the same way and calls a
/// real-mode procedure (0301h) that waits until the protected-mode handler has counted three
/// ticks: interrupts that come in real mode must reach it.
fn dpmi_hooks_client() -> Vec<u8> {
    com(|a| {
        let fail = a.l("fail");
        a.mov(ah, 0x4A)?;
        a.mov(bx, 0x1000)?;
        a.int(0x21)?;
        a.mov(ax, 0x1687)?;
        a.int(0x2F)?;
        a.or(ax, ax)?;
        a.jnz(fail)?;
        let entry = a.addr("entry") as u32;
        a.mov(word_ptr(entry), di)?;
        a.mov(word_ptr(entry + 2), es)?;
        let rmseg = a.addr("rmseg") as u32;
        a.mov(word_ptr(rmseg), cs)?;
        a.mov(bx, si)?;
        a.mov(ah, 0x48)?;
        a.int(0x21)?;
        a.jc(fail)?;
        a.mov(es, ax)?;
        a.xor(ax, ax)?;
        a.call(dword_ptr(entry))?;
        a.jc(fail)?;
        // The handlers run on the host's stack: they find the data through the code segment.
        let dsel = a.addr("dsel") as u32;
        a.mov(word_ptr(dsel), ds)?;
        // Hooks: the previous handler (0204h), then this one (0205h).
        for (vector, previous, handler) in [(0x21, "prev21", "hook21"), (0x08, "prev08", "hook08")] {
            let previous = a.addr(previous) as u32;
            a.mov(ax, 0x0204)?;
            a.mov(bl, vector)?;
            a.int(0x31)?;
            a.mov(word_ptr(previous), dx)?;
            a.mov(word_ptr(previous + 2), cx)?;
            a.mov(ax, 0x0205)?;
            a.mov(bl, vector)?;
            a.mov(cx, cs)?;
            let handler = a.addr(handler);
            a.mov(dx, handler)?;
            a.int(0x31)?;
            a.jc(fail)?;
            if vector == 0x21 {
                a.mov(ah, 2)?;
                a.mov(dl, b'C' as i32)?;
                a.int(0x21)?;
                let hooked = a.addr("hooked21") as u32;
                a.cmp(word_ptr(hooked), 1)?;
                a.jne(fail)?;
            }
        }
        // The real-mode wait (0301h, interrupts on).
        let regs = a.addr("regs") as u32;
        a.mov(word_ptr(regs + 0x20), 0x0202)?;
        let rmwait = a.addr("rmwait");
        a.mov(word_ptr(regs + 0x2A), rmwait)?;
        a.mov(ax, word_ptr(rmseg))?;
        a.mov(word_ptr(regs + 0x2C), ax)?;
        a.push(ds)?;
        a.pop(es)?;
        a.mov(di, regs as i32)?;
        a.xor(bh, bh)?;
        a.xor(cx, cx)?;
        a.mov(ax, 0x0301)?;
        a.sti()?;
        a.int(0x31)?;
        a.jc(fail)?;
        let count = a.addr("count08") as u32;
        a.cmp(word_ptr(count), 3)?;
        a.jb(fail)?;
        a.mov(ah, 2)?;
        a.mov(dl, b'R' as i32)?;
        a.int(0x21)?;
        exit(a, 0)?;
        a.here("fail")?;
        exit(a, 1)?;
        for (handler, counter, previous) in [("hook21", "hooked21", "prev21"), ("hook08", "count08", "prev08")] {
            a.here(handler)?;
            a.push(ds)?;
            a.mov(ds, word_ptr(dsel).cs())?;
            let counter = a.addr(counter) as u32;
            a.inc(word_ptr(counter))?;
            a.pop(ds)?;
            a.pushf()?;
            let previous = a.addr(previous) as u32;
            a.call(dword_ptr(previous).cs())?;
            a.iret()?;
        }
        // Real mode: CS is the program's segment.
        a.here("rmwait")?;
        let counter = a.addr("count08") as u32;
        a.cmp(word_ptr(counter).cs(), 3)?;
        let rmwait = a.l("rmwait");
        a.jb(rmwait)?;
        a.retf()?;
        for name in ["entry", "prev21", "prev08"] {
            a.here(name)?;
            a.dw(&[0, 0])?;
        }
        for name in ["rmseg", "hooked21", "count08", "dsel"] {
            a.here(name)?;
            a.dw(&[0])?;
        }
        a.here("regs")?;
        a.db(&[0; 0x32])
    })
}

#[test]
fn a_dpmi_client_s_handlers_chain_and_hear_interrupts_in_real_mode() {
    let mut m = boot_dpmi(&dpmi_hooks_client(), true);
    assert_eq!(m.run(10 * SECOND), Stop::Exited(0), "{}, {:?}", console(&m), m.diagnostics);
    assert_eq!(console(&m), "CR");
    assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
}

fn boot_dpmi(program: &[u8], dpmi: bool) -> Machine {
    let mut drive = Vfs::new();
    drive.insert_file("CLIENT.COM", program.to_vec());
    Machine::with_program(Config { dpmi, ..Config::default() }, drive, "CLIENT.COM", "").unwrap()
}

#[test]
fn a_dpmi_client_runs_in_protected_mode() {
    for bits32 in [false, true] {
        let mut m = boot_dpmi(&dpmi_client(bits32, false), true);
        assert_eq!(m.run(10 * SECOND), Stop::Exited(0), "32-bit: {bits32}, {}, {:?}", console(&m), m.diagnostics);
        assert_eq!(console(&m), "PMhello from RMTH", "32-bit: {bits32}");
        assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
        assert!(!m.cpu.protected_mode(), "back in real mode after the exit");
    }
}

#[test]
fn a_faulting_dpmi_client_is_ended() {
    let mut m = boot_dpmi(&dpmi_client(false, true), true);
    assert_eq!(m.run(10 * SECOND), Stop::Exited(0xFF), "{}", console(&m));
    assert_eq!(console(&m), "P");
    assert!(m.diagnostics.iter().any(|d| d.contains("exception 0Dh")), "{:?}", m.diagnostics);
}

#[test]
fn without_dpmi_the_program_is_told_there_is_none() {
    let mut m = boot_dpmi(&dpmi_client(false, false), false);
    assert_eq!(m.run(10 * SECOND), Stop::Exited(1));
    assert!(m.diagnostics.iter().any(|d| d.contains("--dpmi")), "{:?}", m.diagnostics);
}

/// A watch's hits as (byte, value, old, the instruction's PSP-relative offset, the chain as
/// (entry, return, interrupt) offsets), for comparing.
type HitSummary = (u32, u8, u8, u32, Vec<(u32, u32, bool)>);

fn hit_summary(hits: &[dos::watch::Hit], base: u32, start: u32) -> Vec<HitSummary> {
    hits.iter()
        .map(|h| {
            let at = match h.by {
                dos::watch::By::Instruction { linear, .. } => linear - base,
                _ => 0,
            };
            let chain = h.chain.iter().map(|f| (f.entry - base, f.from - base, f.interrupt)).collect();
            (h.address - start, h.value, h.old, at, chain)
        })
        .collect()
}

#[test]
fn a_watch_names_the_instruction_and_its_callers() {
    // `value` is written by the main program, then two calls deep.
    let offsets = std::cell::RefCell::new(BTreeMap::new());
    let program = com(|a| {
        for name in ["main_write", "outer", "after_outer", "setter", "after_setter", "value"] {
            offsets.borrow_mut().insert(name, a.addr(name) as u32);
        }
        a.here("main_write")?;
        let value = word_at(a, "value");
        a.mov(value, 1)?;
        let outer = a.l("outer");
        a.call(outer)?;
        a.here("after_outer")?;
        exit(a, 0)?;
        a.here("outer")?;
        let setter = a.l("setter");
        a.call(setter)?;
        a.here("after_setter")?;
        a.ret()?;
        a.here("setter")?;
        let value = word_at(a, "value");
        a.mov(value, 0x0102)?;
        a.ret()?;
        a.here("value")?;
        a.dw(&[0])
    });
    let o = offsets.into_inner();
    let mut m = boot(&[("W.COM", &program)], "W.COM");
    let base = u32::from(m.dos.psp) << 4;
    let value = base + o["value"];
    m.start_watch(dos::watch::Watch::range(value..value + 2, false));
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let watch = m.take_watch().unwrap();
    let callers = vec![(o["setter"], o["after_setter"], false), (o["outer"], o["after_outer"], false)];
    assert_eq!(
        hit_summary(&watch.hits, base, value),
        [
            (0, 1, 0, o["main_write"], vec![]),
            (1, 0, 0, o["main_write"], vec![]),
            (0, 2, 1, o["setter"], callers.clone()),
            (1, 1, 0, o["setter"], callers),
        ]
    );
}

#[test]
fn a_watch_names_the_service_that_wrote() {
    // INT 21h AH=47h writes the current directory ("" at the root) into the buffer.
    let offsets = std::cell::RefCell::new(BTreeMap::new());
    let program = com(|a| {
        for name in ["after_int", "buffer"] {
            offsets.borrow_mut().insert(name, a.addr(name) as u32);
        }
        a.mov(ah, 0x47)?;
        a.mov(dl, 0)?;
        let buffer = a.addr("buffer");
        a.mov(si, buffer)?;
        a.int(0x21)?;
        a.here("after_int")?;
        exit(a, 0)?;
        a.here("buffer")?;
        a.db(&[0xFF; 4])
    });
    let o = offsets.into_inner();
    let mut m = boot(&[("W.COM", &program)], "W.COM");
    let base = u32::from(m.dos.psp) << 4;
    let buffer = base + o["buffer"];
    m.start_watch(dos::watch::Watch::range(buffer..buffer + 4, false));
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let watch = m.take_watch().unwrap();
    let [hit] = &watch.hits[..] else { panic!("{:?}", watch.hits) };
    assert_eq!((hit.address, hit.value, hit.old), (buffer, 0, 0xFF));
    assert!(
        matches!(hit.by, dos::watch::By::Service { id: 0x21, ax: entry_ax } if entry_ax >> 8 == 0x47),
        "{:?}",
        hit.by
    );
    let [frame] = &hit.chain[..] else { panic!("{:?}", hit.chain) };
    assert!(frame.interrupt && frame.from - base == o["after_int"], "{frame:?}");
}

#[test]
fn a_watch_sees_an_interrupt_handler_and_reads() {
    // The timer handler counts ticks; the main program reads the count until it reaches 20.
    let offsets = std::cell::RefCell::new(BTreeMap::new());
    let program = com(|a| {
        for name in ["compare", "handler", "increment", "counter"] {
            offsets.borrow_mut().insert(name, a.addr(name) as u32);
        }
        a.cli()?;
        load_dx(a, "handler")?;
        a.mov(ax, 0x2508)?;
        a.int(0x21)?;
        a.mov(al, 0x36)?;
        a.out(0x43, al)?;
        a.mov(ax, 1193)?;
        a.out(0x40, al)?;
        a.mov(al, ah)?;
        a.out(0x40, al)?;
        a.sti()?;
        a.here("wait")?;
        a.hlt()?;
        a.here("compare")?;
        let counter = word_at(a, "counter");
        a.cmp(counter, 20)?;
        let wait = a.l("wait");
        a.jb(wait)?;
        exit(a, 0)?;
        a.here("handler")?;
        a.push(ax)?;
        a.here("increment")?;
        let counter = word_at(a, "counter");
        a.inc(counter)?;
        a.mov(al, 0x20)?;
        a.out(0x20, al)?;
        a.pop(ax)?;
        a.iret()?;
        a.here("counter")?;
        a.dw(&[0])
    });
    let o = offsets.into_inner();
    let mut m = boot(&[("W.COM", &program)], "W.COM");
    let base = u32::from(m.dos.psp) << 4;
    let counter = base + o["counter"];
    m.start_watch(dos::watch::Watch::range(counter..counter + 2, true));
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let watch = m.take_watch().unwrap();
    let writes: Vec<_> = watch.hits.iter().filter(|h| h.kind == dos::watch::Kind::Write).collect();
    assert_eq!(writes.len(), 40, "20 increments of a word");
    for hit in &writes {
        assert!(matches!(hit.by, dos::watch::By::Instruction { linear, .. } if linear - base == o["increment"]));
        assert!(hit.chain[0].interrupt && hit.chain[0].entry - base == o["handler"], "{:?}", hit.chain);
    }
    let reads = watch.hits.iter().filter(|h| h.kind == dos::watch::Kind::Read);
    assert!(
        reads
            .clone()
            .any(|h| matches!(h.by, dos::watch::By::Instruction { linear, .. } if linear - base == o["compare"])
                && h.chain.is_empty())
    );
}

#[test]
fn a_summary_watch_tells_what_only_an_interrupt_handler_writes() {
    let (program, o) = timer_counting();
    let mut m = boot(&[("S.COM", &program)], "S.COM");
    let base = u32::from(m.dos.psp) << 4;
    let (counter, copy) = (base + o["counter"], base + o["copy"]);
    m.start_watch(dos::watch::Watch::summary(std::iter::once(counter..copy + 2).collect(), true));
    assert_eq!(m.run(SECOND * 2), Stop::Exited(0));
    let watch = m.take_watch().unwrap();
    assert!(
        watch.services.contains_key(&0x21) && !watch.services.contains_key(&0x33),
        "its exit, through DOS; no mouse"
    );
    let touches = watch.touches.unwrap();
    // The handler's INC (its first instruction) reads it too.
    let only_interrupt = dos::watch::Touch {
        read: true,
        read_in_interrupt: true,
        interrupt_writer: Some(base + o["handler"]),
        interrupt_write_by: Some(base + o["handler"]),
        ..Default::default()
    };
    assert_eq!((touches[&counter], touches[&(counter + 1)]), (only_interrupt, only_interrupt), "the program reads it");
    let main = dos::watch::Touch { written: true, ..Default::default() };
    assert_eq!((touches[&copy], touches[&(copy + 1)]), (main, main));
}

#[test]
fn a_launcher_s_program_runs_inside_no_interrupt_handler() {
    // The timer-counting program started by a launcher, with calls tracked: DOS's EXEC runs it inside
    // the launcher's INT 21h, a software interrupt, so what it reads and writes is its own.
    let (program, o) = timer_counting();
    let launcher = com(|a| {
        a.mov(ah, 0x4A)?; // keep 64 KB
        a.mov(bx, 0x1000)?;
        a.int(0x21)?;
        let block = a.addr("block") as u32;
        for at in [block + 4, block + 8, block + 12] {
            a.mov(word_ptr(at), ds)?;
        }
        a.push(ds)?;
        a.pop(es)?;
        let at = a.addr("block");
        a.mov(bx, at)?;
        load_dx(a, "child")?;
        a.mov(ax, 0x4B00)?;
        a.int(0x21)?;
        exit(a, 0)?;
        a.here("child")?;
        a.db(b"S.COM\0")?;
        a.here("tail")?;
        a.db(b"\x00\r")?;
        let tail = a.addr("tail") as u16;
        a.here("block")?;
        a.dw(&[0, tail, 0, 0x5C, 0, 0x6C, 0])
    });
    let files: [(&str, &[u8]); 2] = [("L.COM", &launcher), ("S.COM", &program)];
    // Where the launcher loads it.
    let mut m = boot(&files, "L.COM");
    let psp = (0..100).find_map(|_| {
        m.run(SECOND / 100);
        m.process_named("S.COM").map(|(psp, _)| psp)
    });
    let base = u32::from(psp.expect("S.COM runs")) << 4;
    let (counter, copy) = (base + o["counter"], base + o["copy"]);
    let mut m = boot(&files, "L.COM");
    m.track_calls(true);
    m.start_watch(dos::watch::Watch::summary(std::iter::once(counter..copy + 2).collect(), true));
    assert_eq!(m.run(SECOND * 2), Stop::Exited(0));
    let touches = m.take_watch().unwrap().touches.unwrap();
    assert_eq!(touches[&counter].interrupt_writer, Some(base + o["handler"]), "the timer's handler writes it");
    assert!(touches[&counter].read, "the program reads it: {:?}", touches[&counter]);
    let main = dos::watch::Touch { written: true, ..Default::default() };
    assert_eq!((touches[&copy], touches[&(copy + 1)]), (main, main), "the program writes it");
}

/// A program whose timer handler (on INT 1Ch) counts ticks while the main program copies the count
/// until it reaches 20.
fn timer_counting() -> (Vec<u8>, BTreeMap<&'static str, u32>) {
    let offsets = std::cell::RefCell::new(BTreeMap::new());
    let program = com(|a| {
        for name in ["handler", "counter", "copy"] {
            offsets.borrow_mut().insert(name, a.addr(name) as u32);
        }
        load_dx(a, "handler")?;
        a.mov(ax, 0x251C)?;
        a.int(0x21)?;
        a.here("wait")?;
        a.hlt()?;
        let counter = word_at(a, "counter");
        a.mov(ax, counter)?;
        let copy = word_at(a, "copy");
        a.mov(copy, ax)?;
        a.cmp(ax, 20)?;
        let wait = a.l("wait");
        a.jb(wait)?;
        exit(a, 0)?;
        a.here("handler")?;
        let counter = word_at(a, "counter");
        a.inc(counter)?;
        a.iret()?;
        a.here("counter")?;
        a.dw(&[0])?;
        a.here("copy")?;
        a.dw(&[0])
    });
    (program, offsets.into_inner())
}

/// A program that calls `step` ten times (from `again`); `step` counts its calls in `count`.
fn ten_steps() -> (Vec<u8>, BTreeMap<&'static str, u32>) {
    let offsets = std::cell::RefCell::new(BTreeMap::new());
    let program = com(|a| {
        for name in ["again", "step", "count"] {
            offsets.borrow_mut().insert(name, a.addr(name) as u32);
        }
        a.mov(cx, 10)?;
        a.here("again")?;
        let step = a.l("step");
        a.call(step)?;
        let again = a.l("again");
        a.loop_(again)?;
        exit(a, 0)?;
        a.here("step")?;
        let count = word_at(a, "count");
        a.inc(count)?;
        a.ret()?;
        a.here("count")?;
        a.dw(&[0])
    });
    (program, offsets.into_inner())
}

#[test]
fn a_run_stops_at_a_sync_point_s_hit() {
    let (program, o) = ten_steps();
    let mut m = boot(&[("S.COM", &program)], "S.COM");
    let base = u32::from(m.dos.psp) << 4;
    let step = base + o["step"];
    let count = || (base + o["count"]) as usize;
    assert_eq!(m.run_to_sync(step, 3, SECOND), Stop::Sync { address: step, hit: 3 });
    assert_eq!(m.mem.read_u16(count() as u32), 2, "stopped before the third call ran");
    assert_eq!(m.cpu.segment_base(cpu::seg::CS).wrapping_add(m.cpu.regs.ip), step);
    // Going on runs the instruction it stopped at without counting it again.
    assert_eq!(m.run_to_sync(step, 1, SECOND), Stop::Sync { address: step, hit: 4 });
    assert_eq!(m.mem.read_u16(count() as u32), 3);
    let snapshot = m.clone();
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    assert_eq!(m.syncs.get(step).unwrap().hits, 10);
    assert_eq!(snapshot.syncs.get(step).unwrap().hits, 4, "a snapshot keeps the counts");
}

#[test]
fn a_run_stops_at_the_next_hit_of_any_sync_point() {
    let (program, o) = ten_steps();
    let mut m = boot(&[("S.COM", &program)], "S.COM");
    let base = u32::from(m.dos.psp) << 4;
    let (step, again) = (base + o["step"], base + o["again"]);
    m.syncs.add(step, false);
    m.syncs.add(again, false);
    let mut timeline = Vec::new();
    while let Stop::Sync { address, hit } = m.run_to_next_sync(SECOND) {
        timeline.push((address == step, hit));
    }
    let expected: Vec<_> = (1..=10).flat_map(|n| [(false, n), (true, n)]).collect();
    assert_eq!(timeline, expected, "each loop's call, then the step it calls");
    assert_eq!(m.exit_code(), Some(0));
}

/// A busy-polling loop: 1000 calls of `poll`; every 128th pass changes the byte at `input`.
fn busy_poll() -> (Vec<u8>, BTreeMap<&'static str, u32>) {
    let offsets = std::cell::RefCell::new(BTreeMap::new());
    let program = com(|a| {
        for name in ["poll", "input"] {
            offsets.borrow_mut().insert(name, a.addr(name) as u32);
        }
        a.mov(cx, 1000)?;
        a.here("again")?;
        let poll = a.l("poll");
        a.call(poll)?;
        a.test(cl, 0x7F)?;
        let next = a.l("next");
        a.jnz(next)?;
        let input = byte_at(a, "input");
        a.inc(input)?;
        a.here("next")?;
        let again = a.l("again");
        a.loop_(again)?;
        exit(a, 0)?;
        a.here("poll")?;
        a.ret()?;
        a.here("input")?;
        a.db(&[0])
    });
    (program, offsets.into_inner())
}

#[test]
fn a_gated_sync_point_counts_a_hit_when_what_it_reads_changed() {
    let (program, o) = busy_poll();
    let mut m = boot(&[("P.COM", &program)], "P.COM");
    let base = u32::from(m.dos.psp) << 4;
    let poll = base + o["poll"];
    m.syncs.gate(poll, vec![dos::sync::Source::Memory { at: base + o["input"], len: 1 }]);
    let mut timeline = Vec::new();
    while let Stop::Sync { hit, .. } = m.run_to_next_sync(SECOND) {
        timeline.push((hit, m.syncs.get(poll).unwrap().skipped, m.mem.read_u8(base + o["input"])));
    }
    assert_eq!(m.exit_code(), Some(0));
    // The first poll, then the poll after each of the 7 changes (CX = 896, 768, ... 128).
    let expected: Vec<_> = [(1, 0, 0), (2, 104, 1)].into_iter().chain((3..=8).map(|n| (n, 127, n as u8 - 1))).collect();
    assert_eq!(timeline, expected);
    assert_eq!(m.syncs.get(poll).unwrap().skipped, 127);
}

#[test]
fn a_sync_point_gated_on_a_timer_count_ticks_with_time() {
    // poll: call it forever; the gate is the BIOS's timer count (0040:006C), 18.2 a second.
    let poll_offset = std::cell::Cell::new(0);
    let program = com(|a| {
        poll_offset.set(a.addr("poll") as u32);
        a.here("again")?;
        let poll = a.l("poll");
        a.call(poll)?;
        let again = a.l("again");
        a.jmp(again)?;
        a.here("poll")?;
        a.ret()
    });
    let mut m = boot(&[("T.COM", &program)], "T.COM");
    let poll = (u32::from(m.dos.psp) << 4) + poll_offset.get();
    m.syncs.gate(poll, vec![dos::sync::Source::Memory { at: 0x46C, len: 4 }]);
    let end = m.cpu.cycles + SECOND;
    let mut ticks = 0;
    while let Stop::Sync { .. } = m.run_to_next_sync(end - m.cpu.cycles) {
        ticks += 1;
    }
    assert!((18..=20).contains(&ticks), "{ticks} ticks in a second");
    assert!(m.syncs.get(poll).unwrap().skipped > 1000, "the loop polls far more often");
}

#[test]
fn the_call_tracker_counts_calls_by_target() {
    // Ten calls of `step`; the INT 21h that exits isn't a call.
    let (program, o) = ten_steps();
    let mut m = boot(&[("S.COM", &program)], "S.COM");
    let base = u32::from(m.dos.psp) << 4;
    m.track_calls(true);
    m.probes.calls.as_deref_mut().unwrap().count(true);
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let counts = m.probes.calls.as_deref().unwrap().counts().unwrap();
    assert_eq!(counts.iter().collect::<Vec<_>>(), [(&(base + o["step"]), &10)]);

    // A PUSH of a value that looks like a return address just past it isn't a call.
    let step = std::cell::Cell::new(0);
    let program = com(|a| {
        step.set(a.addr("step") as u32);
        let looks_like_a_return = a.addr("push") + 3;
        a.mov(si, looks_like_a_return)?;
        a.here("push")?;
        a.push(si)?;
        a.pop(si)?;
        let target = a.l("step");
        a.call(target)?;
        exit(a, 0)?;
        a.here("step")?;
        a.ret()
    });
    let mut m = boot(&[("P.COM", &program)], "P.COM");
    m.track_calls(true);
    m.probes.calls.as_deref_mut().unwrap().count(true);
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let counts = m.probes.calls.as_deref().unwrap().counts().unwrap();
    let step = (u32::from(m.dos.psp) << 4) + step.get();
    assert_eq!(counts.iter().collect::<Vec<_>>(), [(&step, &1)]);

    // Calls with SP at 0, the top of the stack's segment: the push wraps SP to FFFEh, the return
    // brings it back to 0, and the frame ends.
    let offsets = std::cell::RefCell::new(BTreeMap::new());
    let program = com(|a| {
        for name in ["step", "done"] {
            offsets.borrow_mut().insert(name, a.addr(name) as u32);
        }
        a.xor(sp, sp)?;
        for _ in 0..2 {
            let target = a.l("step");
            a.call(target)?;
        }
        a.here("done")?;
        exit(a, 0)?;
        a.here("step")?;
        a.ret()
    });
    let mut m = boot(&[("W.COM", &program)], "W.COM");
    let base = u32::from(m.dos.psp) << 4;
    let o = offsets.into_inner();
    m.track_calls(true);
    m.probes.calls.as_deref_mut().unwrap().count(true);
    assert!(matches!(m.run_to_sync(base + o["done"], 1, SECOND), Stop::Sync { .. }));
    let calls = m.probes.calls.as_deref().unwrap();
    assert_eq!(calls.counts().unwrap().iter().collect::<Vec<_>>(), [(&(base + o["step"]), &2)]);
    assert!(calls.chain().is_empty(), "{:?}", calls.chain());

    // Handlers that run on a stack of their own and go back to their caller's stack without returning
    // on theirs (as an extender's do). `away` calls `back` on another stack; `back` goes back to the
    // first stack and on where it was called from: its frame ends there. Then `away` calls `gone` on
    // the other stack, which goes back to the first stack and returns from `away`: both frames end.
    let offsets = std::cell::RefCell::new(BTreeMap::new());
    let program = com(|a| {
        for name in ["resume", "done"] {
            offsets.borrow_mut().insert(name, a.addr(name) as u32);
        }
        let target = a.l("away");
        a.call(target)?;
        a.here("done")?;
        exit(a, 0)?;
        a.here("away")?;
        let saved = word_at(a, "saved");
        a.mov(saved, sp)?;
        for (callee, after) in [("back", "resume"), ("gone", "unreached")] {
            a.mov(ax, cs)?;
            a.add(ax, 0x1000)?;
            a.mov(ss, ax)?;
            a.mov(sp, 0x100)?;
            let target = a.l(callee);
            a.call(target)?;
            a.here(after)?;
        }
        a.nop()?;
        // back: to the first stack, on after its call.
        a.here("back")?;
        a.mov(ax, cs)?;
        a.mov(ss, ax)?;
        let saved = word_at(a, "saved");
        a.mov(sp, saved)?;
        let target = a.l("resume");
        a.jmp(target)?;
        // gone: to the first stack, returning from `away`.
        a.here("gone")?;
        a.mov(ax, cs)?;
        a.mov(ss, ax)?;
        let saved = word_at(a, "saved");
        a.mov(sp, saved)?;
        a.ret()?;
        a.here("saved")?;
        a.dw(&[0])
    });
    let mut m = boot(&[("S.COM", &program)], "S.COM");
    let base = u32::from(m.dos.psp) << 4;
    let o = offsets.into_inner();
    m.track_calls(true);
    m.probes.calls.as_deref_mut().unwrap().count(true);
    assert!(matches!(m.run_to_sync(base + o["resume"], 1, SECOND), Stop::Sync { .. }));
    let chain = m.probes.calls.as_deref().unwrap().chain();
    assert_eq!(chain.len(), 1, "back's frame ended where it was called from: {chain:?}");
    assert!(matches!(m.run_to_sync(base + o["done"], 1, SECOND), Stop::Sync { .. }));
    let calls = m.probes.calls.as_deref().unwrap();
    assert_eq!(calls.counts().unwrap().len(), 3, "away, back and gone");
    assert!(calls.chain().is_empty(), "{:?}", calls.chain());
}

#[test]
fn a_snapshot_keeps_the_calls_tracked_so_far() {
    // Stopped inside `step` (its third call): a watch from the snapshot sees the call to it.
    let (program, o) = ten_steps();
    let mut drive = Vfs::new();
    drive.insert_file("S.COM", program);
    let mut m = Machine::with_program(Config::default(), drive.clone(), "S.COM", "").unwrap();
    let base = u32::from(m.dos.psp) << 4;
    m.track_calls(true);
    assert!(matches!(m.run_to_sync(base + o["step"], 3, SECOND), Stop::Sync { .. }));
    let mut file = Vec::new();
    dos::snapshot::save(&m, &mut file).unwrap();
    let mut loaded = dos::snapshot::load(&file[..], drive, None).unwrap();
    let count = base + o["count"];
    loaded.start_watch(dos::watch::Watch::range(count..count + 2, false));
    assert_eq!(loaded.run(SECOND), Stop::Exited(0));
    let watch = loaded.take_watch().unwrap();
    let first = &watch.hits[0];
    assert_eq!(first.chain.first().map(|f| f.entry), Some(base + o["step"]), "{:?}", first.chain);
}

#[test]
fn a_sync_point_logs_its_timeline() {
    let (program, o) = ten_steps();
    let mut m = boot(&[("S.COM", &program)], "S.COM");
    let step = (u32::from(m.dos.psp) << 4) + o["step"];
    m.syncs.add(step, true);
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let cycles = m.syncs.get(step).unwrap().cycles.clone().unwrap();
    assert_eq!(cycles.len(), 10);
    assert!(cycles.windows(2).all(|w| w[0] < w[1]), "{cycles:?}");
}

#[test]
fn bounds_checks_report_an_index_past_a_table() {
    // The loop reads table[0] to table[4] of a 4-byte table: one past its end. `after`, the byte
    // following the table, is also read on its own, which isn't an index past the table.
    let offsets = std::cell::RefCell::new(BTreeMap::new());
    let program = com(|a| {
        for name in ["read", "table"] {
            offsets.borrow_mut().insert(name, a.addr(name) as u32);
        }
        a.xor(bx, bx)?;
        let table = a.addr("table");
        a.mov(si, table)?;
        a.here("read")?;
        a.mov(al, byte_ptr(bx + si))?;
        a.inc(bx)?;
        a.cmp(bx, 5)?;
        let again = a.l("read");
        a.jb(again)?;
        let after = byte_at(a, "after");
        a.mov(al, after)?;
        exit(a, 0)?;
        a.here("table")?;
        a.db(&[1, 2, 3, 4])?;
        a.here("after")?;
        a.db(&[5])
    });
    let o = offsets.into_inner();
    let mut m = boot(&[("B.COM", &program)], "B.COM");
    let base = u32::from(m.dos.psp) << 4;
    m.start_bounds(&[dos::bounds::Object { start: base + o["table"], size: 4 }]);
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let findings = m.take_bounds().unwrap().findings();
    let [finding] = &findings[..] else { panic!("{findings:?}") };
    assert_eq!(
        (finding.instruction - base, finding.object, finding.outside, &finding.offsets[..]),
        (o["read"], 0, 1, &[4][..])
    );
}

#[test]
fn uninitialized_reads_are_the_bytes_nothing_wrote() {
    // Past the end of the program file: `unset` is read without being written, `set` is written
    // first, and `buffer` is filled by DOS (AH=47h, the current directory) before it is read.
    const UNSET: u16 = 0x4000;
    const SET: u16 = 0x4002;
    const BUFFER: u16 = 0x4100;
    let offsets = std::cell::RefCell::new(BTreeMap::new());
    let program = com(|a| {
        offsets.borrow_mut().insert("read_unset", a.addr("read_unset") as u32);
        a.here("read_unset")?;
        a.mov(ax, word_ptr(u32::from(UNSET)))?;
        a.mov(word_ptr(u32::from(SET)), 7)?;
        a.mov(ax, word_ptr(u32::from(SET)))?;
        a.mov(ah, 0x47)?;
        a.mov(dl, 0)?;
        a.mov(si, i32::from(BUFFER))?;
        a.int(0x21)?;
        a.mov(al, byte_ptr(u32::from(BUFFER)))?;
        exit(a, 0)
    });
    let o = offsets.into_inner();
    let mut m = boot(&[("U.COM", &program)], "U.COM");
    let base = u32::from(m.dos.psp) << 4;
    m.start_uninit(&[]);
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let reads = m.take_uninit().unwrap().reads;
    let found: Vec<(u32, u32)> = reads.iter().map(|r| (r.address - base, r.instruction - base)).collect();
    let unset = u32::from(UNSET);
    assert_eq!(found, [(unset, o["read_unset"]), (unset + 1, o["read_unset"])]);
    assert!(m.mem.was_written(0), "tracking ended");
}

#[test]
fn calls_tracked_from_the_start_give_a_later_watch_the_whole_chain() {
    // The same program as `a_watch_names_the_instruction_and_its_callers`; the watch starts only
    // when `setter` is reached, two calls deep.
    let offsets = std::cell::RefCell::new(BTreeMap::new());
    let program = com(|a| {
        for name in ["outer", "after_outer", "setter", "after_setter", "value"] {
            offsets.borrow_mut().insert(name, a.addr(name) as u32);
        }
        let outer = a.l("outer");
        a.call(outer)?;
        a.here("after_outer")?;
        exit(a, 0)?;
        a.here("outer")?;
        let setter = a.l("setter");
        a.call(setter)?;
        a.here("after_setter")?;
        a.ret()?;
        a.here("setter")?;
        let value = word_at(a, "value");
        a.mov(value, 0x0102)?;
        a.ret()?;
        a.here("value")?;
        a.dw(&[0])
    });
    let o = offsets.into_inner();
    let chain_seen = |track: bool| {
        let mut m = boot(&[("W.COM", &program)], "W.COM");
        let base = u32::from(m.dos.psp) << 4;
        m.track_calls(track);
        assert!(matches!(m.run_to_sync(base + o["setter"], 1, SECOND), Stop::Sync { .. }));
        m.start_watch(dos::watch::Watch::range(base + o["value"]..base + o["value"] + 1, false));
        assert_eq!(m.run(SECOND), Stop::Exited(0));
        let hits = m.take_watch().unwrap().hits;
        hits[0].chain.iter().map(|f| (f.entry - base, f.from - base)).collect::<Vec<_>>()
    };
    assert_eq!(chain_seen(true), [(o["setter"], o["after_setter"]), (o["outer"], o["after_outer"])]);
    assert_eq!(chain_seen(false), [], "without tracking, only the calls made since the watch began");
}
