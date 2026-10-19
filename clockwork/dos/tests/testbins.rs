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
    String::from_utf8_lossy(&m.console).into_owned()
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
        assert_eq!((a, m.state_hash(), &m.console), (b, loaded.state_hash(), &loaded.console), "{name}");
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
