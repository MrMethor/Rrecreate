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
    m.service_trace = Some(Vec::new());
    assert_eq!(m.run(SECOND), Stop::Exited(0));
    let calls = m.service_trace.as_ref().unwrap();
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
    let pixel = |m: &Machine, x: u32, y: u32| m.mem.read_u8(0xA_0000 + y * 320 + x);
    let mut m = boot(&[("C.COM", &program)], "C.COM");
    m.queue_mouse(SECOND / 2, 2, 1, 0);
    assert_eq!(m.run(SECOND / 4), Stop::CycleLimit);
    assert_eq!(pixel(&m, 161, 101), 0x0F, "arrow drawn at x 320/2, y 100");
    assert_eq!(pixel(&m, 160, 100), 0x00, "the arrow's tip row is transparent");
    assert_eq!(m.run(SECOND / 2), Stop::CycleLimit);
    assert_eq!(pixel(&m, 161, 101), 0x00, "old position restored");
    assert_eq!(pixel(&m, 2, 2), 0x0F, "arrow moved to x 2/2, y 1");
}
