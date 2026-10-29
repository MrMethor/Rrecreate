//! The synthetic test programs in `testbins/` (C programs built with Open Watcom, see
//! `testbins/build.sh`): each runs to its end with the output its source gives, and uses nothing
//! the host lacks.

use std::path::Path;

use dos::{Config, Machine, Stop, Vfs};

const SECOND: u64 = 8_000_000; // cycles per emulated second at the default speed

fn run(name: &str) -> Machine {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbins/bin").join(name);
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut drive = Vfs::new();
    drive.insert_file(name, data);
    let mut m = Machine::with_program(Config::default(), drive, name, "").unwrap();
    let stop = m.run(5 * SECOND);
    assert!(matches!(stop, Stop::Exited(_)), "{name}: {stop:?}");
    assert!(m.diagnostics.is_empty(), "{name}: {:?}", m.diagnostics);
    m
}

fn output(m: &Machine) -> String {
    String::from_utf8_lossy(&m.console.to_vec()).into_owned()
}

#[test]
fn hello_prints_and_exits_with_its_code() {
    let m = run("HELLO.EXE");
    assert_eq!(output(&m), "hello 42\r\n");
    assert_eq!(m.exit_code(), Some(3));
}

#[test]
fn calling_conventions() {
    assert_eq!(output(&run("CALLCONV.EXE")), "123 456 1234 700000 31\r\n");
}

#[test]
fn large_model_far_data_and_callbacks() {
    assert_eq!(output(&run("FARDATA.EXE")), "alpha bravo charlie delta echo 4640\r\n");
}

#[test]
fn file_io_through_the_c_runtime() {
    let m = run("FILEIO.EXE");
    assert_eq!(output(&m), "11 file 0\r\n");
    assert!(!m.drive.is_file("TEST.DAT") && !m.drive.is_file("MOVED.DAT"), "the file was removed");
}

#[test]
fn mode_13h_pattern() {
    let m = run("MODE13.EXE");
    let (width, height, rgb) = m.screen_rgb();
    assert_eq!((width, height), (320, 200));
    // Pixel (x, y) has color x ^ y; color 1 was set to pure red.
    let pixel = |x: usize, y: usize| &rgb[(y * width + x) * 3..][..3];
    assert_eq!(pixel(1, 0), [255, 0, 0]);
    assert_eq!(pixel(0, 1), [255, 0, 0]);
    assert_eq!(pixel(5, 5), [0, 0, 0]);
    assert_ne!(pixel(100, 37), pixel(37, 37));
}

#[test]
fn timer_interrupt_handler() {
    let m = run("TIMER.EXE");
    assert_eq!(output(&m), "ticks 500\r\n");
    let ms = m.elapsed_ms();
    assert!((500..520).contains(&ms), "500 ticks at 1000 Hz: {ms} ms");
}

#[test]
fn floating_point_on_the_8087() {
    assert_eq!(output(&run("FLOAT.EXE")), "1.414214 0.841471 2.718282 2.7188\r\n");
}

#[test]
fn medium_model_far_calls_and_a_jump_table() {
    assert_eq!(output(&run("MEDIUM.EXE")), "151 6765\r\n");
}

#[test]
fn a_game_loads_what_it_shows_and_plays_from_an_archive() {
    // VAULT.EXE: the title from VAULT.DAT (run-length coded) with its palette, then two levels (LZSS
    // coded), the second loaded mid-play; walked through by the keys, it exits with the moves made.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbins/bin");
    let mut drive = Vfs::new();
    for name in ["VAULT.EXE", "VAULT.DAT"] {
        drive.insert_file(name, std::fs::read(dir.join(name)).unwrap());
    }
    let mut m = Machine::with_program(Config::default(), drive, "VAULT.EXE", "").unwrap();
    let (right, up, down) = (0x4D, 0x48, 0x50);
    let mut at = 1500;
    let mut press = |m: &mut Machine, scan: u8, extended: bool| {
        let prefix: &[u8] = if extended { &[0xE0] } else { &[] };
        let cycle = m.cycle_at_ms(at);
        m.queue_scancodes(cycle, &[prefix, &[scan]].concat());
        let cycle = m.cycle_at_ms(at + 60);
        m.queue_scancodes(cycle, &[prefix, &[scan | 0x80]].concat());
        at += 500;
    };
    press(&mut m, 0x1C, false);
    for scan in [right, right, down, right, right, up, right, down, down, down] {
        press(&mut m, scan, true);
    }
    assert_eq!(m.run(10 * SECOND), Stop::Exited(9), "6 moves on level 1 (a wall bumped), 3 on level 2");
    assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
}

#[test]
fn a_snapshot_on_disk_goes_on_exactly_as_the_machine_would_have() {
    // Files written and renamed, mode 13h and the DAC, a timer interrupt handler, x87 code: half
    // way through, saved and loaded again, each runs on to the same end as without the snapshot.
    for (name, cycles) in
        [("FILEIO.EXE", 4_000), ("MODE13.EXE", 450_000), ("TIMER.EXE", 2_000_000), ("FLOAT.EXE", 5_500)]
    {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbins/bin").join(name);
        let mut drive = Vfs::new();
        drive.insert_file(name, std::fs::read(&path).unwrap());
        let mut m = Machine::with_program(Config::default(), drive.clone(), name, "").unwrap();
        assert_eq!(m.run(cycles), Stop::CycleLimit, "{name} ended before the snapshot");
        let mut file = Vec::new();
        dos::snapshot::save(&m, &mut file).unwrap();
        let mut loaded = dos::snapshot::load(&file[..], drive.clone(), None).unwrap();
        assert_eq!(loaded.state_hash(), m.state_hash(), "{name}");
        let (a, b) = (m.run(5 * SECOND), loaded.run(5 * SECOND));
        assert!(matches!(a, Stop::Exited(_)), "{name}: {a:?}");
        assert_eq!((a, m.state_hash()), (b, loaded.state_hash()), "{name}");
        // Records start empty in a loaded machine: it printed the end of what the machine printed.
        assert!(m.console.to_vec().ends_with(&loaded.console.to_vec()), "{name}");
        // The game's files aren't in the snapshot; another folder doesn't restore the machine.
        let mut other = drive.clone();
        other.insert_file(name, vec![0; 10]);
        assert!(dos::snapshot::load(&file[..], other, None).is_err(), "{name}");
    }
}

#[test]
fn a_snapshot_from_another_format_is_refused() {
    let mut drive = Vfs::new();
    drive.insert_file("A.COM", vec![0xCD, 0x20]);
    let m = Machine::with_program(Config::default(), drive.clone(), "A.COM", "").unwrap();
    let mut file = Vec::new();
    dos::snapshot::save(&m, &mut file).unwrap();
    file[8] ^= 0xFF; // the format number
    let error = dos::snapshot::load(&file[..], drive, None).map(|_| ()).unwrap_err();
    assert!(error.contains("format"), "{error}");
}

#[test]
fn a_flat_program_s_objects_are_found_where_its_extender_put_them() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbins/bin/FLAT32.EXE");
    let data = std::fs::read(&path).unwrap();
    let le = dos::le::Le::parse(&data).unwrap().expect("an LE executable");
    // FLAT32.MAP: _TEXT ends at 0001:3181, DGROUP is 10590h bytes, _cstart_ is at 0001:000000F8.
    assert_eq!(le.objects.iter().map(|o| o.size).collect::<Vec<_>>(), [0x3181, 0x10590]);
    assert_eq!(le.entry, (0, 0xF8));
    assert!(le.objects[0].fixups.iter().any(|f| f.target == 1 && f.width == 4), "the code refers to its data");
    let boot = || {
        let mut drive = Vfs::new();
        drive.insert_file("FLAT32.EXE", data.clone());
        Machine::with_program(Config::default(), drive, "FLAT32.EXE", "").unwrap()
    };
    // CauseWay loads the program somewhere; once it is there, the objects are found.
    let mut m = boot();
    let bases = (0..10).find_map(|_| {
        m.run(SECOND / 20);
        le.bases(&m)
    });
    let bases = bases.expect("the objects in memory");
    assert!(bases[0] % 4 == 0 && bases[1] != bases[0], "{bases:X?}");
    // The same run again stops at the entry point, where the code is the file's.
    let mut m = boot();
    let entry = bases[0] + 0xF8;
    assert_eq!(m.run_to_sync(entry, 1, 2 * SECOND), Stop::Sync { address: entry, hit: 1 });
    let code: Vec<u8> = (0..16).map(|i| m.peek_linear_u8(entry + i).unwrap()).collect();
    assert_eq!(code, le.objects[0].data[0xF8..0xF8 + 16]);
    assert_eq!(m.run(2 * SECOND), Stop::Exited(32));
    assert_eq!(output(&m), "flat32: 1240 from 16 calls\r\n");
}

/// DOS4GW.EXE from the local Open Watcom (`WATCOM`): it isn't committed, as nothing says it may be
/// redistributed. Without it, the tests that need it skip.
pub fn dos4gw() -> Option<Vec<u8>> {
    let watcom = std::env::var_os("WATCOM")?;
    std::fs::read(Path::new(&watcom).join("binw").join("dos4gw.exe")).ok()
}

#[test]
fn a_dos4gw_program_s_objects_are_found_while_dos4gw_runs_it() {
    let Some(dos4gw) = dos4gw() else {
        eprintln!("skipped: no DOS4GW.EXE (set WATCOM to Open Watcom's folder)");
        return;
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbins/bin/FLAT4GW.EXE");
    let data = std::fs::read(&path).unwrap();
    let le = dos::le::Le::parse(&data).unwrap().expect("an LE executable");
    // FLAT4GW.MAP: _cstart_ is at 0001:000001C8.
    assert_eq!(le.entry, (0, 0x1C8));
    let mut drive = Vfs::new();
    drive.insert_file("FLAT4GW.EXE", data);
    drive.insert_file("DOS4GW.EXE", dos4gw);
    let mut m = Machine::with_program(Config::default(), drive, "FLAT4GW.EXE", "").unwrap();
    // Its stub runs DOS4GW.EXE, which loads it: the objects are found while DOS4GW.EXE runs, and the
    // program by name through its parent.
    let bases = (0..20).find_map(|_| {
        m.run(SECOND / 20);
        le.bases(&m)
    });
    let bases = bases.expect("the objects in memory");
    assert!(m.dos.program.ends_with("DOS4GW.EXE"), "{}", m.dos.program);
    assert_eq!(m.process_named("flat4gw.exe").map(|(_, path)| path), Some("FLAT4GW.EXE"));
    assert!(bases[0] % 4 == 0 && bases[1] != bases[0], "{bases:X?}");
    assert_eq!(m.run(2 * SECOND), Stop::Exited(32));
    // After DOS/4GW's banner.
    assert!(output(&m).ends_with("1990-1994\r\nflat4gw: 1240 from 16 calls\r\n"), "{}", output(&m));
}

#[test]
fn the_32_bit_runtime_s_program_runs() {
    // CLIB32 links Open Watcom's 32-bit runtime for its signature pack; it runs too.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbins/bin/CLIB32.EXE");
    let mut drive = Vfs::new();
    drive.insert_file("CLIB32.EXE", std::fs::read(&path).unwrap());
    let mut m = Machine::with_program(Config::default(), drive, "CLIB32.EXE", "").unwrap();
    assert_eq!(m.run(2 * SECOND), Stop::Exited(0));
    assert_eq!(output(&m), "clib32: 190 functions; 1 3 5 7 9; 1.414\r\n");
}

#[test]
fn coverage_records_a_flat_program_s_32_bit_code() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbins/bin/FLAT32.EXE");
    let data = std::fs::read(&path).unwrap();
    let le = dos::le::Le::parse(&data).unwrap().expect("an LE executable");
    let mut drive = Vfs::new();
    drive.insert_file("FLAT32.EXE", data);
    let mut m = Machine::with_program(Config::default(), drive, "FLAT32.EXE", "").unwrap();
    m.start_coverage();
    let bases = (0..10).find_map(|_| {
        m.run(SECOND / 20);
        le.bases(&m)
    });
    let bases = bases.expect("the objects in memory");
    assert_eq!(m.run(2 * SECOND), Stop::Exited(32));
    let coverage = m.probes.coverage.take().unwrap();
    // FLAT32.MAP: square at 0001:00000010, called 16 times through a pointer from sum_ (0001:00000024).
    let square = bases[0] + 0x10;
    assert!(coverage.executed().any(|at| at == square), "its 32-bit code ran");
    let calls: u64 = coverage.calls.iter().filter(|((_, target), _)| *target == square).map(|(_, n)| n).sum();
    assert_eq!(calls, 16);
    assert!(coverage.executed().any(|at| coverage.image.contains(&at)), "and its 16-bit stub");
}
