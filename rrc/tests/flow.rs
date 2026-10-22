//! The toolkit's flow on a small program, as an agent runs it: a port repository, the game's
//! version, knowledge, a session, its trace. It drives the `clockwork` program, built from this
//! workspace.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A COM program: forever `again: inc word [count]; mov cx, 1000; loop $; call step; jmp again`, with
/// `step` (+0000E) a `ret` and `count` at +0000F.
const PROGRAM: [u8; 17] =
    [0xFF, 0x06, 0x0F, 0x01, 0xB9, 0xE8, 0x03, 0xE2, 0xFE, 0xE8, 0x02, 0x00, 0xEB, 0xF2, 0xC3, 0x00, 0x00];

/// A COM program that polls: 1000 times `call poll` (+00016, a `ret`); every 128th pass (CX = 896,
/// 768, ... 128) adds 1 to the byte `input` (+00017); then it exits.
const POLLING: [u8; 24] = [
    0xB9, 0xE8, 0x03, 0xE8, 0x10, 0x00, 0xF6, 0xC1, 0x7F, 0x75, 0x04, 0xFE, 0x06, 0x17, 0x01, 0xE2, 0xF2, 0xB8, 0x00,
    0x4C, 0xCD, 0x21, 0xC3, 0x00,
];

/// A COM program that resets the mouse driver, then forever waits (`mov cx, 10000; loop $`) and
/// polls it: `poll` (+00009) is `mov ax, 3; int 33h`.
const MOUSE_POLLING: [u8; 16] =
    [0x31, 0xC0, 0xCD, 0x33, 0xB9, 0x10, 0x27, 0xE2, 0xFE, 0xB8, 0x03, 0x00, 0xCD, 0x33, 0xEB, 0xF4];

/// A COM program that reads the BIOS's timer count (0040:006C) forever: `mov ax, 40h; mov es, ax;
/// mov ax, es:[6Ch]; call poll; jmp $-14`, `poll` (+0000E) a `ret`.
const BIOS_TIMER: [u8; 15] = [0xB8, 0x40, 0x00, 0x8E, 0xC0, 0x26, 0xA1, 0x6C, 0x00, 0xE8, 0x02, 0x00, 0xEB, 0xF2, 0xC3];

/// A COM program that reads the game port forever: `mov dx, 201h; again: in al, dx; call poll; jmp
/// again`, `poll` (+00009) a `ret`.
const GAME_PORT: [u8; 10] = [0xBA, 0x01, 0x02, 0xEC, 0xE8, 0x02, 0x00, 0xEB, 0xFA, 0xC3];

/// The workspace's `clockwork`, built if it isn't.
fn clockwork() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("target"));
    let exe = target.join("debug").join(if cfg!(windows) { "clockwork.exe" } else { "clockwork" });
    if !exe.is_file() {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let built = Command::new(cargo).args(["build", "-p", "clockwork"]).status().expect("cargo");
        assert!(built.success(), "building clockwork");
    }
    exe
}

struct Rrc {
    repo: PathBuf,
    cache: PathBuf,
    clockwork: PathBuf,
}

impl Rrc {
    /// Runs `rrc args` in the repository; returns its output and whether it succeeded.
    fn run(&self, args: &[&str]) -> (String, bool) {
        let output = Command::new(env!("CARGO_BIN_EXE_rrc"))
            .args(args)
            .current_dir(&self.repo)
            .env("RRC_CACHE", &self.cache)
            .env("RRC_CLOCKWORK", &self.clockwork)
            .env("RRC_SNAPSHOT_MS", "1")
            .env_remove("RRC_BY")
            .output()
            .expect("rrc");
        let text = String::from_utf8_lossy(&output.stdout).into_owned() + &String::from_utf8_lossy(&output.stderr);
        (text, output.status.success())
    }

    fn ok(&self, args: &[&str]) -> String {
        let (text, ok) = self.run(args);
        assert!(ok, "rrc {args:?}:\n{text}");
        text
    }
}

/// A folder for a test (`name`) with the game (`program` as T.COM) and an empty port repository.
fn setup(name: &str, program: &[u8]) -> (PathBuf, Rrc) {
    setup_file(name, "T.COM", program)
}

/// The same, with the program as `file`.
fn setup_file(name: &str, file: &str, program: &[u8]) -> (PathBuf, Rrc) {
    let dir = std::env::temp_dir().join(format!("rrc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (game, repo) = (dir.join("game"), dir.join("port"));
    std::fs::create_dir_all(game.join(file).parent().unwrap()).unwrap();
    std::fs::write(game.join(file), program).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let rrc = Rrc { repo, cache: dir.join("cache"), clockwork: clockwork() };
    (dir, rrc)
}

#[test]
fn a_session_is_traced_tick_by_tick() {
    let (dir, rrc) = setup("flow", &PROGRAM);
    let (game, repo) = (dir.join("game"), rrc.repo.clone());

    assert!(rrc.ok(&["init", ".", "--program", "T.COM", "--name", "t"]).contains("next      rrc game"));
    assert!(repo.join(".gitattributes").is_file() && repo.join(".gitignore").is_file());
    rrc.ok(&["game", game.to_str().unwrap(), "--add-version", "1.0"]);
    assert!(rrc.ok(&["doctor"]).contains("game      ") && rrc.ok(&["game"]).contains("version   1.0"));
    rrc.ok(&["name", "T.COM+0000E", "step", "--by", "human"]);
    rrc.ok(&["data", "T.COM+0000F", "count", "--size", "2", "--type", "u16"]);
    let (taken, ok) = rrc.run(&["data", "T.COM+00011", "count", "--size", "2"]);
    assert!(!ok && taken.contains("count names T.COM+0000F already"), "{taken}");
    rrc.ok(&["sync", "T.COM+0000E", "loop", "--by", "human", "--evidence", "step runs once a pass"]);
    let ticks = std::fs::read_to_string(repo.join("knowledge/ticks.toml")).unwrap();
    assert!(ticks.contains("provenance = \"human\"") && ticks.contains("evidence = \"step runs once"), "{ticks}");
    assert!(rrc.ok(&["sync"]).contains("sync      loop T.COM+0000E (human)"));
    let settings = std::fs::read_to_string(repo.join("rrc.toml")).unwrap();
    std::fs::write(repo.join("rrc.toml"), format!("{settings}\n[port]\nimports = false\n")).unwrap();
    let (refused, ok) = rrc.run(&["import", "recreate", "."]);
    assert!(!ok && refused.contains("takes no imports"), "{refused}");
    let check = rrc.ok(&["check"]);
    assert!(check.contains("1 functions, 1 data, 0 types, 1 sync points"), "{check}");
    assert!(rrc.ok(&["show", "count"]).contains("count (agent)"));

    rrc.ok(&["session", "add", "short", "--seconds", "0.005"]);
    let calls = rrc.ok(&["calls", "short", "--min", "10"]);
    assert!(calls.contains("1 functions: step"), "the loop calls step once a pass: {calls}");
    assert!(calls.contains("phase     before tick 1") && calls.contains("phase     loop, ticks 1-"), "{calls}");
    let step = rrc.ok(&["calls", "short", "step"]);
    assert!(step.contains("calls     step (T.COM+0000E): ") && step.contains("phase     loop, ticks 1-"), "{step}");
    // Analysis comes first (it needs Ghidra, which tests don't have).
    assert!(rrc.ok(&["status"]).contains("next      rrc analyze"));
    let recorded = rrc.ok(&["record", "short"]);
    assert!(recorded.contains("ticks"), "{recorded}");
    assert!(rrc.ok(&["record", "short"]).contains("same      as the trace recorded before"), "deterministic");
    // Reading more is another trace of the same run.
    rrc.ok(&["data", "T.COM+00005", "delay", "--size", "2"]);
    let again = rrc.ok(&["record", "short"]);
    assert!(again.contains("same      run as before; the trace differs"), "{again}");
    std::fs::write(
        repo.join("knowledge/data.tsv"),
        std::fs::read_to_string(repo.join("knowledge/data.tsv"))
            .unwrap()
            .lines()
            .filter(|l| !l.contains("delay"))
            .map(|l| format!("{l}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let summary = rrc.ok(&["trace", "short"]);
    assert!(summary.contains("syncs     loop "), "{summary}");
    let watched = rrc.ok(&["watch", "short", "count", "--tick", "2", "--ticks", "1"]);
    assert!(watched.contains("watch     count (T.COM+0000F, 2 bytes), tick 2"), "{watched}");
    assert!(watched.contains("write count = 0200 (was 0100) by T.COM+00000"), "{watched}");
    // From the snapshot record kept, the same answer.
    let later = rrc.ok(&["watch", "short", "count", "--tick", "30", "--ticks", "1"]);
    assert!(later.contains("start     from the snapshot at tick 2"), "{later}");
    assert!(later.contains("write count = 1E00 (was 1D00) by T.COM+00000"), "{later}");
    let inputs = rrc.ok(&["inputs", "short"]);
    assert!(inputs.contains("none"), "no interrupt handler writes anything it reads: {inputs}");
    let again = rrc.ok(&["inputs", "short"]);
    assert!(again.starts_with(&inputs) && again.contains("kept      this answer"), "{again}");
    let counts = rrc.ok(&["trace", "short", "--item", "count"]);
    let lines: Vec<&str> = counts.lines().take(3).collect();
    assert_eq!(lines, ["1         1", "2         2", "3         3"], "the count at each tick, as a u16");

    // With an analysis (a stand-in: tests have no Ghidra), status walks through ticks and traces.
    let export = r#"{"record":"program","name":"T.COM","entry":{"linear":65536,"text":"1000:0000"},"ghidra":"-"}"#;
    let export = rrc::cache::Cache::at(&rrc.cache).unwrap().put(export.as_bytes()).unwrap();
    let index = format!(
        "original = \"-\"\nprogram = \"-\"\nunpacked = false\nexport = \"{export}\"\nentry = 0\nghidra = \"-\"\n"
    );
    std::fs::write(repo.join(".rrc/analysis.toml"), index).unwrap();
    assert!(rrc.ok(&["status"]).contains("next      find what a tick reads from outside: rrc inputs short"));
    let added = rrc.ok(&["input", "T.COM+0000F", "tally", "--note", "the pass count"]);
    assert!(added.contains("input     tally T.COM+0000F, 2 bytes (agent)"), "the named data's size: {added}");
    let status = rrc.ok(&["status"]);
    assert!(status.contains("rrc record short: the knowledge changed since it was recorded"), "{status}");
    assert!(rrc.ok(&["record", "short"]).contains("same      run as before"));
    assert!(rrc.ok(&["status"]).contains("every session is recorded with the knowledge as it is"));
    let (refused, ok) = rrc.run(&["input", "T.COM+00011", "other"]);
    assert!(!ok && refused.contains("give --size N"), "{refused}");
    assert!(rrc.ok(&["input", "tally", "--remove"]).contains("removed   tally"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_busy_polling_loop_ticks_when_what_it_reads_changed() {
    let (dir, rrc) = setup("polling", &POLLING);
    rrc.ok(&["init", ".", "--program", "T.COM", "--name", "t"]);
    rrc.ok(&["game", dir.join("game").to_str().unwrap(), "--add-version", "1.0"]);
    rrc.ok(&["data", "T.COM+00017", "input", "--size", "1", "--type", "u8"]);
    let ticks = "[[sync]]\nname = \"poll\"\nat = \"T.COM+00016\"\nwhen_changed = [\"input\"]\n";
    std::fs::write(rrc.repo.join("knowledge/ticks.toml"), ticks).unwrap();
    rrc.ok(&["check"]);
    rrc.ok(&["session", "add", "polls", "--seconds", "0.1"]);
    let recorded = rrc.ok(&["record", "polls"]);
    // The first poll, then the poll after each of the 7 changes: not 1000.
    assert!(recorded.contains("trace     8 ticks"), "{recorded}");
    let summary = rrc.ok(&["trace", "polls"]);
    assert!(summary.contains("syncs     poll 8 (866 hits skipped)"), "{summary}");
    let tick = rrc.ok(&["trace", "polls", "--tick", "3"]);
    assert!(tick.contains(", 127 hits skipped before it"), "{tick}");
    let inputs = rrc.ok(&["trace", "polls", "--item", "input"]);
    let lines: Vec<&str> = inputs.lines().take(3).collect();
    assert_eq!(lines, ["1         0", "2         1", "3         2"], "a change a tick");
    std::fs::write(rrc.repo.join("knowledge/ticks.toml"), ticks.replace("[\"input\"]", "[\"other\"]")).unwrap();
    let (refused, ok) = rrc.run(&["check"]);
    assert!(!ok && refused.contains("sync poll: when_changed: other is neither"), "{refused}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_mouse_is_an_input_and_a_gate() {
    let (dir, rrc) = setup("mouse", &MOUSE_POLLING);
    rrc.ok(&["init", ".", "--program", "T.COM", "--name", "t"]);
    rrc.ok(&["game", dir.join("game").to_str().unwrap(), "--add-version", "1.0"]);
    let ticks = "[[sync]]\nname = \"poll\"\nat = \"T.COM+00009\"\nwhen_changed = [\"mouse\"]\n\n\
                 [[input]]\nname = \"mouse\"\ndevice = \"mouse\"\n";
    std::fs::write(rrc.repo.join("knowledge/ticks.toml"), ticks).unwrap();
    assert!(rrc.ok(&["check"]).contains("1 sync points, 1 inputs"));
    let script = dir.join("click.inputs");
    std::fs::write(&script, "100:{click 160 100}\n").unwrap();
    rrc.ok(&["session", "add", "click", "--seconds", "0.6", "--inputs", script.to_str().unwrap()]);
    // The first poll, then the move, the press and the release.
    assert!(rrc.ok(&["record", "click"]).contains("trace     4 ticks"));
    let mouse = rrc.ok(&["trace", "click", "--item", "mouse"]);
    let lines: Vec<&str> = mouse.lines().collect();
    assert_eq!(
        lines,
        [
            "1         buttons 0, at (320, 96), motion (0, 0)",
            "2         buttons 0, at (160, 96), motion (-160, 0)",
            "3         buttons 1, at (160, 96), motion (-160, 0), left pressed 1 (last at 160, 96)",
            "4         buttons 0, at (160, 96), motion (-160, 0), left pressed 1 (last at 160, 96), left released 1 \
             (last at 160, 96)",
        ],
        "{mouse}"
    );
    let inputs = rrc.ok(&["inputs", "click"]);
    assert!(inputs.contains("unseen    INT 33h (the mouse driver) called"), "{inputs}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn memory_outside_the_image_is_mem() {
    let (dir, rrc) = setup("mem", &BIOS_TIMER);
    rrc.ok(&["init", ".", "--program", "T.COM", "--name", "t"]);
    rrc.ok(&["game", dir.join("game").to_str().unwrap(), "--add-version", "1.0"]);
    rrc.ok(&["input", "MEM+0046C", "bios_ticks", "--size", "4", "--note", "the BIOS's timer count"]);
    rrc.ok(&["sync", "T.COM+0000E", "poll", "--when-changed", "bios_ticks"]);
    assert!(rrc.ok(&["check"]).contains("1 sync points, 1 inputs"));
    rrc.ok(&["session", "add", "wait", "--seconds", "0.5"]);
    let recorded = rrc.ok(&["record", "wait"]);
    // The first poll, then one a timer tick: 18.2 a second.
    assert!(recorded.contains("trace     10 ticks"), "{recorded}");
    let ticks = rrc.ok(&["trace", "wait", "--item", "bios_ticks"]);
    assert_eq!(ticks.lines().count(), 10, "a new count each tick: {ticks}");
    // The screen at a tick, from the pictures the trace keeps: DOS's text screen.
    let frame = rrc.ok(&["frame", "wait", "--tick", "5"]);
    assert!(frame.contains("video mode 3h, 720 × 400"), "{frame}");
    let png = std::fs::read(rrc.repo.join(".rrc/frames/wait-5.png")).unwrap();
    assert!(png.starts_with(b"\x89PNG"));
    // The BIOS's data is scanned with the data segment.
    let scanned = rrc.ok(&["inputs", "wait"]);
    assert!(scanned.contains("bytes; MEM+00400, 256 bytes, from tick 2"), "{scanned}");
    assert!(scanned.contains("MEM+0046C bios_ticks (an input, 4 bytes)"), "{scanned}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_scripted_joystick_is_an_input() {
    let (dir, rrc) = setup("joystick", &GAME_PORT);
    rrc.ok(&["init", ".", "--program", "T.COM", "--name", "t"]);
    rrc.ok(&["game", dir.join("game").to_str().unwrap(), "--add-version", "1.0"]);
    rrc.ok(&["input", "joystick", "stick"]);
    rrc.ok(&["sync", "T.COM+00009", "poll", "--when-changed", "stick"]);
    let script = dir.join("stick.inputs");
    std::fs::write(&script, "100:{stick 1000 -2000 1}{stick 0 0 0}\n").unwrap();
    let session = ["session", "add", "stick", "--seconds", "0.4", "--inputs", script.to_str().unwrap()];
    rrc.ok(&[&session[..], &["--clockwork", "--joystick 1"]].concat());
    // The first poll, then the stick's two moves.
    assert!(rrc.ok(&["record", "stick"]).contains("trace     3 ticks"));
    let stick = rrc.ok(&["trace", "stick", "--item", "stick"]);
    let lines: Vec<&str> = stick.lines().collect();
    assert_eq!(
        lines,
        [
            "1         A: buttons 0, at (0, 0)",
            "2         A: buttons 1, at (1000, -2000)",
            "3         A: buttons 0, at (0, 0)"
        ],
        "{stick}"
    );
    assert!(rrc.ok(&["inputs", "stick"]).contains("unseen    port 201h (the joystick) read"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_flat_program_s_objects_have_ids() {
    let program = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../testbins/bin/FLAT32.EXE")).unwrap();
    let (dir, rrc) = setup_file("flat", "FLAT32.EXE", &program);
    rrc.ok(&["init", ".", "--program", "FLAT32.EXE", "--name", "flat"]);
    rrc.ok(&["game", dir.join("game").to_str().unwrap(), "--add-version", "1.0"]);
    // FLAT32.MAP: _squares at 0002:0000041C (16 ints); square, the first function, at 0001:00000010.
    rrc.ok(&["data", "FLAT32.EXE#2+0041C", "squares", "--size", "64", "--type", "i32[16]"]);
    rrc.ok(&["sync", "FLAT32.EXE#1+00010", "square"]);
    rrc.ok(&["session", "add", "run", "--seconds", "0.5"]);
    let recorded = rrc.ok(&["record", "run"]);
    assert!(recorded.contains("trace     16 ticks"), "a tick a call: {recorded}");
    let squares = rrc.ok(&["trace", "run", "--item", "squares"]);
    let last = squares.lines().last().unwrap_or("");
    assert!(last.starts_with("16        [0x1]=1 [0x2]=4 [0x3]=9") && last.ends_with("[0xE]=196"), "{squares}");
    let watched = rrc.ok(&["watch", "run", "squares", "--tick", "3", "--ticks", "1"]);
    assert!(watched.contains("write squares[+4] = 01000000 (was 00000000) by FLAT32.EXE#1+"), "{watched}");
    let inputs = rrc.ok(&["inputs", "run"]);
    assert!(inputs.contains("scanned   FLAT32.EXE#2+00000, 66960 bytes"), "its data object: {inputs}");
    // Calls in 32-bit code, the program's listed and the extender's counted.
    let calls = rrc.ok(&["calls", "run", "--min", "16"]);
    assert!(calls.contains("functions of FLAT32.EXE in run, in 2 phases; ") && !calls.contains("MEM+"), "{calls}");
    let square = rrc.ok(&["calls", "run", "FLAT32.EXE#1+00010"]);
    assert!(square.contains("calls     FLAT32.EXE#1+00010: 16 in run"), "square, in its object: {square}");

    // Clockwork's listing of 32-bit code, from the ELF file `clockwork unpack` writes (an analysis
    // with a stand-in for Ghidra's export, which tests don't have).
    let elf = dir.join("FLAT32.ELF");
    let unpacked = Command::new(&rrc.clockwork)
        .arg("unpack")
        .arg(dir.join("game/FLAT32.EXE"))
        .arg("--out")
        .arg(&elf)
        .output()
        .unwrap();
    assert!(unpacked.status.success(), "{}", String::from_utf8_lossy(&unpacked.stderr));
    let elf = std::fs::read(&elf).unwrap();
    let word = |at: usize| u32::from_le_bytes(elf[at..at + 4].try_into().unwrap());
    let objects: Vec<String> =
        (0..2).map(|i| format!("[{}, {}]", word(0x34 + 32 * i + 8), word(0x34 + 32 * i + 20))).collect();
    let cache = rrc::cache::Cache::at(&rrc.cache).unwrap();
    let export = format!(
        r#"{{"record":"program","name":"FLAT32.EXE","entry":{{"linear":{},"text":"-"}},"ghidra":"-"}}"#,
        word(0x34 + 8) + 0xF8
    );
    let (program, export) = (cache.put(&elf).unwrap(), cache.put(export.as_bytes()).unwrap());
    let index = format!(
        "original = \"-\"\nprogram = \"{program}\"\nunpacked = true\nexport = \"{export}\"\nentry = 0\n\
         objects = [{}]\nghidra = \"-\"\n",
        objects.join(", ")
    );
    std::fs::write(rrc.repo.join(".rrc/analysis.toml"), index).unwrap();
    let listing = rrc.ok(&["show", "FLAT32.EXE#1+00024", "--listing"]);
    for line in [
        "FLAT32.EXE#1+0003B  jge FLAT32.EXE#1+0004D",
        "FLAT32.EXE#1+00041  mov [edx*4+squares],eax",
        "FLAT32.EXE#1+00052  ret",
    ] {
        assert!(listing.lines().any(|l| l == line), "{line:?} in:\n{listing}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_dos4gw_program_s_objects_are_found_through_its_stub() {
    // DOS4GW.EXE from the local Open Watcom (`WATCOM`): it isn't committed (testbins/README.md).
    let watcom = std::env::var_os("WATCOM").map(PathBuf::from);
    let Some(dos4gw) = watcom.and_then(|w| std::fs::read(w.join("binw").join("dos4gw.exe")).ok()) else {
        eprintln!("skipped: no DOS4GW.EXE (set WATCOM to Open Watcom's folder)");
        return;
    };
    let program = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../testbins/bin/FLAT4GW.EXE")).unwrap();
    let (dir, rrc) = setup_file("dos4gw", "FLAT4GW.EXE", &program);
    std::fs::write(dir.join("game/DOS4GW.EXE"), dos4gw).unwrap();
    rrc.ok(&["init", ".", "--program", "FLAT4GW.EXE", "--name", "flat4gw"]);
    rrc.ok(&["game", dir.join("game").to_str().unwrap(), "--add-version", "1.0"]);
    // FLAT4GW.MAP: _squares at 0002:0000042C; square, the first function, at 0001:00000010. Its stub
    // runs DOS4GW.EXE, which runs it: the objects are its, found through DOS4GW.EXE's parent.
    rrc.ok(&["data", "FLAT4GW.EXE#2+0042C", "squares", "--size", "64", "--type", "i32[16]"]);
    rrc.ok(&["sync", "FLAT4GW.EXE#1+00010", "square"]);
    rrc.ok(&["session", "add", "run", "--seconds", "1"]);
    let recorded = rrc.ok(&["record", "run"]);
    assert!(recorded.contains("trace     16 ticks"), "a tick a call: {recorded}");
    let squares = rrc.ok(&["trace", "run", "--item", "squares"]);
    let last = squares.lines().last().unwrap_or("");
    assert!(last.starts_with("16        [0x1]=1 [0x2]=4 [0x3]=9") && last.ends_with("[0xE]=196"), "{squares}");
    let square = rrc.ok(&["calls", "run", "FLAT4GW.EXE#1+00010"]);
    assert!(square.contains("calls     FLAT4GW.EXE#1+00010: 16 in run"), "{square}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_launcher_s_program_is_the_module() {
    // GAME\GO.BAT runs OTHER.EXE, then FLAT32.EXE: the module. OTHER.EXE is FLAT32.EXE with a data object
    // 4 KB larger, another LE program whose objects aren't the module's.
    let program = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../testbins/bin/FLAT32.EXE")).unwrap();
    let (dir, rrc) = setup_file("launcher", "GAME/FLAT32.EXE", &program);
    let mut other = program.clone();
    let le = u32::from_le_bytes(other[0x3C..0x40].try_into().unwrap()) as usize;
    let objects = le + u32::from_le_bytes(other[le + 0x40..le + 0x44].try_into().unwrap()) as usize;
    let size = u32::from_le_bytes(other[objects + 24..objects + 28].try_into().unwrap());
    other[objects + 24..objects + 28].copy_from_slice(&(size + 0x1000).to_le_bytes());
    let game = dir.join("game");
    std::fs::write(game.join("GAME/OTHER.EXE"), other).unwrap();
    std::fs::write(game.join("GAME/GO.BAT"), "OTHER\r\nFLAT32\r\n").unwrap();
    rrc.ok(&["init", ".", "--program", "GO.BAT", "--name", "launcher"]);
    let settings = std::fs::read_to_string(rrc.repo.join("rrc.toml")).unwrap().replace(
        "program = \"GO.BAT\"",
        "program = \"GO.BAT\"\noptions = [\"--cwd\", \"GAME\"]\nmodule = \"FLAT32.EXE\"",
    );
    std::fs::write(rrc.repo.join("rrc.toml"), settings).unwrap();
    rrc.ok(&["game", game.to_str().unwrap(), "--add-version", "1.0"]);
    rrc.ok(&["data", "FLAT32.EXE#2+0041C", "squares", "--size", "64", "--type", "i32[16]"]);
    rrc.ok(&["sync", "FLAT32.EXE#1+00010", "square"]);
    rrc.ok(&["session", "add", "run", "--seconds", "1"]);
    let recorded = rrc.ok(&["record", "run"]);
    assert!(recorded.contains("trace     16 ticks"), "OTHER.EXE runs the same code at the same place: {recorded}");
    let inputs = rrc.ok(&["inputs", "run"]);
    assert!(inputs.contains("scanned   FLAT32.EXE#2+00000, 66960 bytes"), "the module's data object: {inputs}");

    // A 16-bit program under a launcher (after OTHER.EXE): +OFF counts from where the session's run
    // loads it.
    std::fs::write(game.join("GAME/T.COM"), PROGRAM).unwrap();
    std::fs::write(game.join("GAME/GO.BAT"), "OTHER\r\nT\r\n").unwrap();
    let settings = std::fs::read_to_string(rrc.repo.join("rrc.toml")).unwrap();
    std::fs::write(rrc.repo.join("rrc.toml"), settings.replace("module = \"FLAT32.EXE\"", "module = \"T.COM\""))
        .unwrap();
    std::fs::write(rrc.repo.join("knowledge/ticks.toml"), "").unwrap();
    let data = std::fs::read_to_string(rrc.repo.join("knowledge/data.tsv")).unwrap();
    let data: String = data.lines().filter(|l| !l.contains("squares")).map(|l| format!("{l}\n")).collect();
    std::fs::write(rrc.repo.join("knowledge/data.tsv"), data).unwrap();
    rrc.ok(&["sync", "T.COM+0000E", "step"]);
    rrc.ok(&["data", "T.COM+0000F", "count", "--size", "2", "--type", "u16"]);
    rrc.ok(&["session", "add", "t", "--seconds", "0.25"]);
    let recorded = rrc.ok(&["record", "t"]);
    assert!(recorded.contains(" ticks, "), "{recorded}");
    let counts = rrc.ok(&["trace", "t", "--item", "count"]);
    let lines: Vec<&str> = counts.lines().take(3).collect();
    assert_eq!(lines, ["1         1", "2         2", "3         3"], "T.COM's count, where the launcher put it");
    let watched = rrc.ok(&["watch", "t", "count", "--tick", "2", "--ticks", "1"]);
    assert!(watched.contains("write count = 0200 (was 0100) by T.COM+00000"), "{watched}");
    let check = rrc.ok(&["check"]);
    assert!(check.contains("image     ") && !check.contains("unknown"), "{check}");
    std::fs::remove_dir_all(&dir).ok();
}
