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
    String::from_utf8_lossy(&m.console).into_owned()
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
    let (width, height, rgb) = m.screen_rgb().expect("mode 13h");
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
            a.mov(ah, 0x0F)?; // FCB open: not implemented
            a.int(0x21)?;
        }
        exit(a, 0)
    });
    let mut m = boot(&[("U.COM", &program)], "U.COM");
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    assert_eq!(m.diagnostics.iter().collect::<Vec<_>>(), ["INT 21h AH=0Fh is not implemented"]);
}

#[test]
fn load_errors() {
    let drive = Vfs::new();
    assert!(matches!(Machine::with_program(Config::default(), drive, "GAME.EXE", ""), Err(LoadError::NotFound(_))));
    let mut le = vec![0u8; 0x80];
    le[..2].copy_from_slice(b"MZ");
    le[0x04] = 1; // one page
    le[0x08] = 4; // 4 header paragraphs
    le[0x18] = 0x40;
    le[0x3C] = 0x40;
    le[0x40..0x42].copy_from_slice(b"LE");
    let mut drive = Vfs::new();
    drive.insert_file("DOOM.EXE", le);
    assert!(matches!(Machine::with_program(Config::default(), drive, "DOOM.EXE", ""), Err(LoadError::Unsupported(_))));
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
    // What `rec-host play` does: run one 70 Hz frame at a time, queue keys at the cycle where
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
fn entering_protected_mode_stops_the_run() {
    let program = com(|a| {
        a.mov(eax, cr0)?;
        a.or(eax, 1)?;
        a.mov(cr0, eax)?;
        exit(a, 0)
    });
    let mut m = boot(&[("PM.COM", &program)], "PM.COM");
    match m.run(SECOND) {
        Stop::Fault(cpu::Exit::Unsupported { feature, .. }) => assert_eq!(feature, "protected mode (MOV CR0)"),
        other => panic!("expected an unsupported-feature fault, got {other:?}"),
    }
}
