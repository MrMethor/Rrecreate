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

/// A COM program that resets the mouse driver, then polls it forever: `poll` (+00004) is
/// `mov ax, 3; int 33h; jmp poll`.
const MOUSE_POLLING: [u8; 11] = [0x31, 0xC0, 0xCD, 0x33, 0xB8, 0x03, 0x00, 0xCD, 0x33, 0xEB, 0xF9];

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
    let dir = std::env::temp_dir().join(format!("rrc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (game, repo) = (dir.join("game"), dir.join("port"));
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join("T.COM"), program).unwrap();
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
    std::fs::write(repo.join("knowledge/ticks.toml"), "[[sync]]\nname = \"loop\"\nat = \"T.COM+0000E\"\n").unwrap();
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
    let inputs = rrc.ok(&["inputs", "short"]);
    assert!(inputs.contains("none"), "no interrupt handler writes anything it reads: {inputs}");
    let counts = rrc.ok(&["trace", "short", "--item", "count"]);
    let lines: Vec<&str> = counts.lines().take(3).collect();
    assert_eq!(lines, ["1         1", "2         2", "3         3"], "the count at each tick, as a u16");
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
    let ticks = "[[sync]]\nname = \"poll\"\nat = \"T.COM+00004\"\nwhen_changed = [\"mouse\"]\n\n\
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
    std::fs::remove_dir_all(&dir).ok();
}
