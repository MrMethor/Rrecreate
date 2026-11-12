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

/// A COM program that resets the mouse driver, then forever waits (+00004: `mov cx, 10000; loop $`),
/// polls it (`poll`, +00009: `mov ax, 3; int 33h`) and works a while (`mov cx, 100; loop $`).
const MOUSE_POLLING: [u8; 21] = [
    0x31, 0xC0, 0xCD, 0x33, 0xB9, 0x10, 0x27, 0xE2, 0xFE, 0xB8, 0x03, 0x00, 0xCD, 0x33, 0xB9, 0x64, 0x00, 0xE2, 0xFE,
    0xEB, 0xEF,
];

/// A COM program that reads the BIOS's timer count (0040:006C) forever: `mov ax, 40h; mov es, ax;
/// mov ax, es:[6Ch]; call poll; jmp $-14`, `poll` (+0000E) a `ret`.
const BIOS_TIMER: [u8; 15] = [0xB8, 0x40, 0x00, 0x8E, 0xC0, 0x26, 0xA1, 0x6C, 0x00, 0xE8, 0x02, 0x00, 0xEB, 0xF2, 0xC3];

/// A COM program that reads the game port forever: `mov dx, 201h; again: in al, dx; call poll; jmp
/// again`, `poll` (+00009) a `ret`.
const GAME_PORT: [u8; 10] = [0xBA, 0x01, 0x02, 0xEC, 0xE8, 0x02, 0x00, 0xEB, 0xFA, 0xC3];

/// A COM program with two quirk candidates: `mov cx, 5; xor bx, bx; again: mov [bx+table], bl; inc bx;
/// loop again` writes 5 bytes into the 4-byte `table` (+00014) at +00005, then `mov al, [400h]` (+0000C)
/// reads memory nothing wrote; then it exits.
const OVERRUN: [u8; 25] = [
    0xB9, 0x05, 0x00, 0x31, 0xDB, 0x88, 0x9F, 0x14, 0x01, 0x43, 0xE2, 0xF9, 0xA0, 0x00, 0x04, 0xB8, 0x00, 0x4C, 0xCD,
    0x21, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// A COM program that reads a file: `mov dx, name; mov ax, 3D00h; int 21h; mov bx, ax; mov dx, buffer;
/// mov cx, 4; mov ah, 3Fh; int 21h` (returning to +00014), then exits; `name` "DATA.BIN" at +0001A, the
/// buffer at +00023.
const READS_A_FILE: [u8; 39] = [
    0xBA, 0x1A, 0x01, 0xB8, 0x00, 0x3D, 0xCD, 0x21, 0x89, 0xC3, 0xBA, 0x23, 0x01, 0xB9, 0x04, 0x00, 0xB4, 0x3F, 0xCD,
    0x21, 0xB8, 0x00, 0x4C, 0xCD, 0x21, 0x00, b'D', b'A', b'T', b'A', b'.', b'B', b'I', b'N', 0, 0, 0, 0, 0,
];

/// The workspace's program `name` (of package `package`), built if it isn't, or always (`fresh`).
fn built(package: &str, name: &str, fresh: bool) -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("target"));
    let exe = target.join("debug").join(if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() });
    if fresh || !exe.is_file() {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let built = Command::new(cargo).args(["build", "-p", package]).status().expect("cargo");
        assert!(built.success(), "building {package}");
    }
    exe
}

/// The workspace's `clockwork`, built if it isn't.
fn clockwork() -> PathBuf {
    built("clockwork", "clockwork", false)
}

struct Rrc {
    repo: PathBuf,
    cache: PathBuf,
    clockwork: PathBuf,
    /// More environment variables for it.
    env: Vec<(&'static str, &'static str)>,
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
            .envs(self.env.iter().copied())
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
    let rrc = Rrc { repo, cache: dir.join("cache"), clockwork: clockwork(), env: Vec::new() };
    (dir, rrc)
}

#[test]
fn a_session_is_traced_tick_by_tick() {
    let (dir, rrc) = setup("flow", &PROGRAM);
    let (game, repo) = (dir.join("game"), rrc.repo.clone());

    assert!(rrc.ok(&["init", ".", "--program", "T.COM", "--name", "t"]).contains("next      rrc game"));
    assert!(repo.join(".gitattributes").is_file() && repo.join(".gitignore").is_file());
    assert!(repo.join(".git").is_dir(), "a port is a git repository");
    // What an agent meets first (D155): the person's rules to ask for, the agent's page, the notes, the session hook.
    for file in ["RULES.md", "AGENTS.md", "CLAUDE.md", "notes/README.md", ".claude/settings.local.json"] {
        assert!(repo.join(file).is_file(), "{file}");
    }
    assert!(rrc.ok(&["rules"]).contains("ASK: which stretch of the game is this port to reproduce?"));
    // A saved game isn't part of the version.
    std::fs::write(game.join("SAVEGAM0.SAV"), b"saved").unwrap();
    let added = rrc.ok(&["game", game.to_str().unwrap(), "--add-version", "1.0", "--without", "*.sav"]);
    assert!(added.contains("without   1 files left out"), "{added}");
    std::fs::write(game.join("SAVEGAM0.SAV"), b"saved again").unwrap();
    let doctor = rrc.ok(&["doctor"]);
    assert!(doctor.contains("game      ") && doctor.contains(", protocol 16, dos-pc ("), "{doctor}");
    assert!(doctor.contains("rules     RULES.md: 5 questions for the person not answered yet"), "{doctor}");
    // The stage the port is at, and what its gate lacks (D158): the person's answers, in place of the questions.
    let asking = rrc.ok(&["status"]);
    assert!(asking.contains("stage     0 Set up, of 0 Set up to 7 Play"), "{asking}");
    assert!(
        asking.contains("lacks     RULES.md: 5 questions for the person not answered; the first: which"),
        "{asking}"
    );
    std::fs::write(repo.join("RULES.md"), "## Rules the person set for this port\n\n- The loop.\n").unwrap();
    assert!(rrc.ok(&["game"]).contains("version   1.0"));
    rrc.ok(&["name", "T.COM+0000E", "step", "--by", "human"]);
    rrc.ok(&["data", "T.COM+0000F", "count", "--size", "2", "--evidence", "step adds 1", "--confidence", "0.5"]);
    // Given a type later, the rest is kept.
    rrc.ok(&["data", "T.COM+0000F", "count", "--type", "u16"]);
    let shown = rrc.ok(&["show", "count"]);
    assert!(shown.contains("step adds 1") && shown.contains("0.5"), "{shown}");
    // The outside sources are in the port's books (D156): found by what they cover, met at the thing they cover, and
    // a sweep's gate is each kind found or looked for.
    assert!(rrc.ok(&["sources"]).contains("swept     never: search for each kind"));
    assert!(rrc.ok(&["status"]).contains("lacks     the outside sources aren't swept"));
    let wiki =
        ["sources", "add", "the-wiki", "--kind", "wiki", "--at", "https://example.org/t", "--covers", "count,steps"];
    let added = rrc.ok(&wiki);
    assert!(added.contains("source    the-wiki (wiki): covers count, steps; at https://example.org/t"), "{added}");
    rrc.ok(&["sources", "exclude", "its-source", "--why", "the person: it grades the toolkit"]);
    assert!(rrc.ok(&["sources", "Count"]).contains("source    the-wiki (wiki)"));
    assert!(rrc.ok(&["sources", "enemies"]).contains("none      no source listed covers enemies"));
    assert!(rrc.ok(&["show", "count"]).contains("sources   the-wiki (wiki): rrc sources count"));
    let (refused, ok) = rrc.run(&["sources", "sweep"]);
    assert!(!ok && refused.contains("--searched says what was searched for"), "{refused}");
    let sweep =
        ["sources", "sweep", "--searched", "t.com; its formats", "--none", "formats,tools", "--date", "2026-11-11"];
    let swept = rrc.ok(&sweep);
    assert!(swept.contains("swept     2026-11-11: 2 searches"), "{swept}");
    assert!(swept.contains("missing   neither found nor looked for: reimplementation, patches, "), "{swept}");
    assert!(rrc.ok(&["status"]).contains("lacks     sources neither found nor looked for: reimplementation, "));
    let listed = rrc.ok(&["sources"]);
    assert!(listed.contains("excluded  its-source: the person: it grades the toolkit"), "{listed}");
    assert!(rrc.ok(&["check"]).contains("sources   1 outside sources, 1 kept out, 1 sweeps"));
    // A port that reads none says so (an honest test bench, D46), and is asked for no sweep (D159).
    assert!(rrc.ok(&["sources", "none", "--why", "a test bench"]).contains("none      no outside source is read"));
    assert!(!rrc.ok(&["status"]).contains("sources neither found"));
    // The best-known routes by a word (D159): the playbook's recipes and questions, and the port's own.
    let how = rrc.ok(&["how", "tick", "ends"]);
    assert!(how.contains("recipe    Where a tick ends: `rrc calls SESSION`: the large groups are loops"), "{how}");
    assert!(rrc.ok(&["how"]).contains("question  What does this value mean?"));
    assert!(rrc.ok(&["how", "zebra"]).contains("none      no recipe has zebra"));
    let recipes = "# Recipes\n\n| Problem | Route | Cost |\n|---|---|---|\n| A zebra crossing | its table | 1 run |\n";
    std::fs::write(repo.join("notes/recipes.md"), recipes).unwrap();
    assert!(rrc.ok(&["how", "zebra"]).contains("noted     A zebra crossing: its table; 1 run"));
    let (taken, ok) = rrc.run(&["data", "T.COM+00011", "count", "--size", "2"]);
    assert!(!ok && taken.contains("count names T.COM+0000F already"), "{taken}");
    // A name taken back, by its name or ID.
    rrc.ok(&["data", "T.COM+00011", "spare", "--size", "2"]);
    assert!(rrc.ok(&["forget", "spare"]).contains("forgot    data T.COM+00011 spare"));
    let (gone, ok) = rrc.run(&["forget", "T.COM+00011"]);
    assert!(!ok && gone.contains("T.COM+00011 isn't named"), "{gone}");
    rrc.ok(&["sync", "T.COM+0000E", "loop", "--by", "human", "--evidence", "step runs once a pass"]);
    let ticks = std::fs::read_to_string(repo.join("knowledge/ticks.toml")).unwrap();
    assert!(ticks.contains("provenance = \"human\"") && ticks.contains("evidence = \"step runs once"), "{ticks}");
    assert!(rrc.ok(&["sync"]).contains("sync      loop T.COM+0000E (human)"));
    let settings = std::fs::read_to_string(repo.join("rrc.toml")).unwrap();
    std::fs::write(repo.join("rrc.toml"), format!("{settings}\n[port]\nimports = false\n")).unwrap();
    let map = Path::new(env!("CARGO_MANIFEST_DIR")).join("../testbins/bin/FLAT32.MAP");
    let (refused, ok) = rrc.run(&["sigs", "name", map.to_str().unwrap()]);
    assert!(!ok && refused.contains("takes no imports"), "a map is a build's truth: {refused}");
    let check = rrc.ok(&["check"]);
    assert!(check.contains("1 functions, 1 data, 0 types, 1 sync points"), "{check}");
    assert!(rrc.ok(&["show", "count"]).contains("count (agent, 0.5)"));

    rrc.ok(&["session", "add", "short", "--seconds", "0.005"]);
    // The scope (D156): the sessions the port is to reproduce, as the person said.
    assert!(rrc.ok(&["scope"]).contains("scope     not set: every session counts (1)"));
    let scope = rrc.ok(&["scope", "sh*", "boss"]);
    assert!(scope.contains("scope     sh*, boss: 1 of 1 sessions"), "{scope}");
    assert!(scope.contains("unmatched boss: no session of that name yet"), "{scope}");
    assert!(std::fs::read_to_string(repo.join("rrc.toml")).unwrap().contains("scope = ["));
    assert!(rrc.ok(&["scope", "--clear"]).contains("scope     not set"));
    let calls = rrc.ok(&["calls", "short", "--min", "10"]);
    assert!(calls.contains("1 functions: step"), "the loop calls step once a pass: {calls}");
    assert!(calls.contains("phase     before tick 1") && calls.contains("phase     loop, ticks 1-"), "{calls}");
    let step = rrc.ok(&["calls", "short", "step"]);
    assert!(step.contains("calls     step (T.COM+0000E): ") && step.contains("phase     loop, ticks 1-"), "{step}");
    // Analysis comes first (it needs Ghidra, which tests don't have).
    assert!(rrc.ok(&["status"]).contains("next      rrc analyze"));
    let recorded = rrc.ok(&["record", "short"]);
    assert!(recorded.contains("ticks"), "{recorded}");
    // Nothing to add: no run (D111); `--run` runs it again, which must give the same trace.
    let kept = rrc.ok(&["record", "short"]);
    assert!(kept.contains("kept      the trace recorded before has every item"), "{kept}");
    assert!(rrc.ok(&["record", "short", "--run"]).contains("same      as the trace recorded before"), "deterministic");
    // A run asked otherwise (its length, input or options) is run again, not kept (D123).
    rrc.ok(&["session", "set", "short", "--seconds", "0.006"]);
    let longer = rrc.ok(&["record", "short"]);
    assert!(longer.contains("runs      the session: its input, options or length changed"), "{longer}");
    rrc.ok(&["session", "set", "short", "--seconds", "0.005"]);
    let back = rrc.ok(&["record", "short"]);
    assert!(back.contains("runs      the session: its input") && back.contains("snapshots "), "{back}");
    // Reading more is another trace of the same run, from the memory kept with the last (D92).
    rrc.ok(&["data", "T.COM+00005", "delay", "--size", "2"]);
    let again = rrc.ok(&["record", "short"]);
    assert!(again.contains("same      run as before; the trace differs"), "{again}");
    assert!(again.contains("from      the run recorded before"), "no emulator: {again}");
    let run = rrc.ok(&["record", "short", "--run"]);
    assert!(run.contains("same      as the trace recorded before"), "what a run reads: {run}");
    // Memory at a tick, named or not, without recording (D87).
    let peeked = rrc.ok(&["peek", "short", "--tick", "2", "count", "T.COM+0000F", "--len", "1"]);
    assert!(peeked.contains("tick      2: the state at its sync point"), "{peeked}");
    assert!(peeked.matches("value     count").count() == 2, "by name and by ID: {peeked}");
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
    assert!(watched.contains("write count = 2 (was 1) by T.COM+00000"), "as its type reads: {watched}");
    // A pass's calls (D83); tick 1's is the start-up's, from power-on (D134).
    let first = rrc.ok(&["pass", "short", "--tick", "1"]);
    assert!(first.contains("pass      tick 1, from power-on: "), "{first}");
    let second = rrc.ok(&["pass", "short", "--tick", "2"]);
    assert!(second.contains("pass      tick 2, in ") && second.contains("step"), "{second}");
    // From the snapshot record kept, the same answer.
    let later = rrc.ok(&["watch", "short", "count", "--tick", "30", "--ticks", "1"]);
    assert!(later.contains("start     from the snapshot at tick 2"), "{later}");
    assert!(later.contains("write count = 30 (was 29) by T.COM+00000"), "{later}");
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
    // The survey's gate holds: the next stage wants the scope, and the one after it the mapping (D158).
    let sessions = rrc.ok(&["status"]);
    assert!(sessions.contains("stage     2 Sessions, of 0 Set up to 7 Play; 0 to 1 hold"), "{sessions}");
    assert!(sessions.contains("lacks     no scope: the sessions the port is to reproduce"), "{sessions}");
    rrc.ok(&["scope", "short"]);
    let skeleton = rrc.ok(&["status"]);
    assert!(skeleton.contains("stage     3 Skeleton, of 0 Set up to 7 Play; 0 to 2 hold"), "{skeleton}");
    assert!(skeleton.contains("next      map the state: mapping.toml names the port's fields"), "{skeleton}");
    let (refused, ok) = rrc.run(&["input", "T.COM+00011", "other"]);
    assert!(!ok && refused.contains("give --size N"), "{refused}");
    // What a part not ported writes, read where the ported code reads it (D96).
    let part = rrc.ok(&["input", "T.COM+0000F", "tally", "--part", "the loop", "--read-at", "T.COM+00000"]);
    assert!(part.contains("written by the loop, read at T.COM+00000"), "{part}");
    assert!(rrc.ok(&["record", "short"]).contains("same      run as before"), "Clockwork takes the source");
    let (refused, ok) = rrc.run(&["input", "T.COM+00011", "other", "--size", "2", "--read-at", "T.COM+00000"]);
    assert!(!ok && refused.contains("--read-at goes with --part"), "{refused}");
    // A part not ported calling ported code: each call in a pass with its registers (D98).
    let hook = rrc.ok(&["hook", "T.COM+0000E", "steps", "--part", "the loop", "--registers", "ax,cl"]);
    assert!(hook.contains("hook      steps T.COM+0000E, called by the loop, with ax, cl"), "{hook}");
    let (refused, ok) = rrc.run(&["hook", "T.COM+0000E", "other", "--part", "x", "--registers", "zz"]);
    assert!(!ok && refused.contains("no register zz"), "{refused}");
    rrc.ok(&["record", "short"]);
    let tick = rrc.ok(&["trace", "short", "--tick", "2"]);
    assert!(tick.contains("hook      steps (T.COM+0000E): ax="), "the call in tick 2's pass: {tick}");
    assert!(rrc.ok(&["hook", "steps", "--remove"]).contains("removed   steps"));
    assert!(rrc.ok(&["input", "tally", "--remove"]).contains("removed   tally"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_files_a_program_reads_are_traced() {
    // D78: which file, where in it, where in memory, by which code.
    let (dir, rrc) = setup("files", &READS_A_FILE);
    std::fs::write(dir.join("game/DATA.BIN"), b"abcdefgh").unwrap();
    rrc.ok(&["init", ".", "--program", "T.COM", "--name", "t"]);
    rrc.ok(&["game", dir.join("game").to_str().unwrap(), "--add-version", "1.0"]);
    rrc.ok(&["data", "T.COM+00023", "buffer", "--size", "4"]);
    rrc.ok(&["session", "add", "s", "--seconds", "0.1"]);
    let all = rrc.ok(&["files", "s"]);
    assert!(all.contains("files     s: 1 files read, 0 written"), "{all}");
    assert!(all.contains("read      DATA.BIN: 1 times, 4 bytes, by T.COM+00014, first at "), "{all}");
    let one = rrc.ok(&["files", "s", "--file", "data.bin"]);
    assert!(one.contains("read      +0, 4 bytes, to buffer by T.COM+00014, at "), "{one}");
    let (none, ok) = rrc.run(&["files", "s", "--file", "OTHER.DAT"]);
    assert!(!ok && none.contains("reads or writes no file or archive member named OTHER.DAT"), "{none}");
    // An archive's accesses are named by their members (D112): DATA.BIN made one, two members of 4 bytes and
    // its directory at its end.
    let mut archive = b"abcdefgh".to_vec();
    for (name, size) in [(b"ONE\0", 4u32), (b"TWO\0", 4)] {
        archive.extend_from_slice(name);
        archive.extend_from_slice(&size.to_le_bytes());
    }
    std::fs::write(dir.join("game/DATA.BIN"), &archive).unwrap();
    std::fs::write(
        rrc.repo.join("knowledge/types.toml"),
        "[[struct]]\nname = \"pack_entry\"\nsize = 8\nfields = [\n    { offset = 0, size = 4, name = \"name\", type = \"u8[4]\" },\n    { offset = 4, size = 4, name = \"size\", type = \"u32\" },\n]\n",
    )
    .unwrap();
    std::fs::write(
        rrc.repo.join("knowledge/formats.toml"),
        "[[format]]\nname = \"pack\"\nfiles = [\"DATA.BIN\"]\nkind = \"archive\"\ndirectory = \"end\"\ncount = 2\nentry = \"pack_entry\"\n",
    )
    .unwrap();
    let all = rrc.ok(&["files", "s"]);
    assert!(all.contains("read      DATA.BIN: 1 times, 4 bytes, 1 members (--file DATA.BIN names them), by "), "{all}");
    let named = rrc.ok(&["files", "s", "--file", "data.bin"]);
    assert!(named.contains("read      +0 (ONE), 4 bytes, to buffer by T.COM+00014, at "), "{named}");
    let member = rrc.ok(&["files", "s", "--file", "one"]);
    assert!(member.contains("file      DATA.BIN/ONE: 1 accesses"), "{member}");
    let (other, ok) = rrc.run(&["files", "s", "--file", "TWO"]);
    assert!(!ok && other.contains("no file or archive member named TWO"), "{other}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn clockwork_says_what_decides_how_it_behaves() {
    // Runs and snapshots are kept by Clockwork's sources, not its program's bytes (D76).
    let source = rrc::clockwork::source(&clockwork()).unwrap();
    assert!(source.starts_with("source ") && source.len() == 7 + 64, "{source}");
}

#[test]
fn quirk_candidates_are_found_and_registered() {
    let (dir, rrc) = setup("quirks", &OVERRUN);
    rrc.ok(&["init", ".", "--program", "T.COM", "--name", "t"]);
    rrc.ok(&["game", dir.join("game").to_str().unwrap(), "--add-version", "1.0"]);
    rrc.ok(&["data", "T.COM+00014", "table", "--size", "4", "--type", "u8[4]"]);
    rrc.ok(&["session", "add", "s", "--seconds", "0.1"]);
    let found = rrc.ok(&["suspects", "s"]);
    assert!(found.contains("suspects  s: 1 instruction reaching past named data, 1 instruction reading"), "{found}");
    assert!(
        found.contains("past      table (T.COM+00014, 4 bytes) by T.COM+00005: offsets 4, 1 time, first"),
        "{found}"
    );
    assert!(found.contains("uninit    1 byte by T.COM+0000C: T.COM+00300 first (it held 00)"), "{found}");
    // What the port's code names and no session ran (D151): +00005 ran, +00012 is the exit and ran, +00014 didn't.
    std::fs::create_dir_all(rrc.repo.join("src")).unwrap();
    std::fs::write(rrc.repo.join("src/named.rs"), "// T.COM+00005, +00012 and T.COM+00014\n").unwrap();
    let unrun = rrc.ok(&["unrun"]);
    assert!(unrun.contains("sessions  1 looked through"), "{unrun}");
    assert!(unrun.contains("unrun     1 of 3 places"), "{unrun}");
    let list = std::fs::read_to_string(rrc.repo.join(".rrc/unrun.tsv")).unwrap();
    assert!(list.starts_with("T.COM+00014\t\tsrc/named.rs:1"), "{list}");
    std::fs::remove_file(rrc.repo.join("src/named.rs")).unwrap();

    // Registered, then named in the port's code.
    // Every bug has both sides (D160): what its switch does is asked for with it.
    let overrun = ["quirk", "overrun", "--at", "T.COM+00005", "--what", "writes a byte past table"];
    let (refused, ok) = rrc.run(&overrun);
    assert!(!ok && refused.contains("--fix (what its switch does when the bug is fixed)"), "{refused}");
    let added = rrc.ok(&[&overrun[..], &["--fix", "stops at the table's end"]].concat());
    assert!(added.contains("quirk     overrun at T.COM+00005"), "{added}");
    assert!(added.contains("  fix     stops at the table's end"), "{added}");
    assert!(added.contains("  shown   not yet: rrc compare SESSION --fix overrun"), "{added}");
    // Shown by a test of the port's own where no comparison reaches it; a new fix is to be shown again.
    let by_test = rrc.ok(&["quirk", "overrun", "--shown", "test sim::overrun_both_ways"]);
    assert!(by_test.contains("  shown   test sim::overrun_both_ways"), "{by_test}");
    let again = rrc.ok(&["quirk", "overrun", "--fix", "stops at the table's end, and says so"]);
    assert!(again.contains("  shown   not yet"), "{again}");
    assert!(added.contains("no code names it yet: branch on rrc_runtime::keeps(\"overrun\")"), "{added}");
    assert!(rrc.ok(&["suspects", "s"]).contains("by T.COM+00005 (quirk overrun)"));
    std::fs::create_dir_all(rrc.repo.join("src")).unwrap();
    std::fs::write(rrc.repo.join("src/sim.rs"), "if rrc_runtime::keeps(\"overrun\") {}\n").unwrap();
    rrc.ok(&["suspects", "--judge", "T.COM+0000C", "--not-a-bug", "the test's own byte, read once"]);
    let listed = rrc.ok(&["quirk"]);
    assert!(listed.contains("quirk     overrun at T.COM+00005: writes a byte past table"), "{listed}");
    assert!(listed.contains("  port    src/sim.rs:1"), "{listed}");
    assert!(rrc.ok(&["check"]).contains("quirks    1"));
    let (refused, ok) = rrc.run(&["quirk", "other", "--at", "nowhere", "--what", "x", "--fix", "y"]);
    assert!(!ok && refused.contains("quirk other: at:"), "{refused}");
    // The rule (D148): every bug registered and branched on, every suspect judged, or check refuses.
    std::fs::write(rrc.repo.join("src/more.rs"), "// a bug: it reads on past the end\n").unwrap();
    let (refused, ok) = rrc.run(&["check"]);
    assert!(!ok && refused.contains("src/more.rs:1: says a bug or quirk"), "{refused}");
    assert!(rrc.ok(&["status"]).contains("src/more.rs:1"), "status names it");
    std::fs::write(rrc.repo.join("src/more.rs"), "// a bug: quirk overrun\n").unwrap();
    let check = rrc.ok(&["check"]);
    // The first rule is said at every check, and by status first of all (D160).
    let rule = "bugs      every bug of the original that changes what the game does is reproduced and has a switch, registered when found (rrc quirk): 1 \
                registered, 1 with a switch not shown yet, 0 things rrc check refuses";
    assert!(check.contains(rule), "{check}");
    assert!(rrc.ok(&["status"]).starts_with(rule));
    // The notes and the knowledge are read too: a bug written there and nowhere else is a bug the port can lose.
    std::fs::write(rrc.repo.join("notes/loops.md"), "The copy runs one past the table, a bug.\n").unwrap();
    let (refused, ok) = rrc.run(&["check"]);
    assert!(
        !ok && refused.contains("notes/loops.md:1: says a bug or quirk without naming a registered one"),
        "{refused}"
    );
    std::fs::write(rrc.repo.join("notes/loops.md"), "The copy runs one past the table: quirk overrun.\n").unwrap();
    rrc.ok(&["check"]);
    std::fs::write(rrc.repo.join("notes/loops.md"), "The table's end looked like a bug; not a bug: it is longer.\n")
        .unwrap();
    rrc.ok(&["check"]);
    std::fs::write(rrc.repo.join("src/more.rs"), "if rrc_runtime::keeps(\"unheard\") {}\n").unwrap();
    let (refused, ok) = rrc.run(&["check"]);
    assert!(!ok && refused.contains("keeps(\"unheard\") isn't registered"), "{refused}");
    std::fs::remove_file(rrc.repo.join("src/more.rs")).unwrap();
    assert!(rrc.ok(&["quirk", "overrun", "--remove"]).contains("removed   overrun"));
    let (refused, ok) = rrc.run(&["check"]);
    assert!(!ok && refused.contains("keeps(\"overrun\") isn't registered"), "{refused}");
    assert!(refused.contains("judge it: rrc suspects --judge T.COM+00005"), "no quirk there now: {refused}");
    std::fs::remove_file(rrc.repo.join("src/sim.rs")).unwrap();
    let (refused, ok) = rrc.run(&["suspects", "--judge", "T.COM+00005", "--quirk", "nothing"]);
    assert!(!ok && refused.contains("no quirk nothing"), "{refused}");
    assert!(rrc.ok(&["suspects", "--judge", "T.COM+00005", "--not-a-bug", "the table is longer"]).contains("judged"));
    // A bug of the original's that touches nothing the game does is judged so, not registered (D163).
    let (both, ok) = rrc.run(&["suspects", "--judge", "T.COM+00005", "--not-a-bug", "x", "--changes-nothing", "y"]);
    assert!(!ok && both.contains("--changes-nothing WHY"), "{both}");
    rrc.ok(&["suspects", "--judge", "T.COM+00005", "--changes-nothing", "what it reads is never used"]);
    let kept = std::fs::read_to_string(rrc.repo.join("suspects.toml")).unwrap();
    assert!(kept.contains("changes nothing: what it reads is never used"), "{kept}");
    rrc.ok(&["check"]);
    // The rules as written; and the commit hook.
    let rules = rrc.ok(&["rules"]);
    assert!(rules.contains("Plan before running") && rules.contains("## Quirks"), "{rules}");
    let first = rules
        .find("The first rule, the person's (D160, D163): every bug of the original that changes what the game does");
    assert!(
        first.is_some_and(|at| at < rules.find("Plan before running").unwrap()),
        "the first rule comes first: {rules}"
    );
    std::fs::write(rrc.repo.join("RULES.md"), "Never cross the streams.\n").unwrap();
    assert!(rrc.ok(&["rules"]).contains("Never cross the streams."));
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
    // A sync point before the wait: each change lands in a pass's wait, which then reads it (D60).
    std::fs::write(rrc.repo.join("knowledge/ticks.toml"), ticks.replace("T.COM+00009", "T.COM+00004")).unwrap();
    let recorded = rrc.ok(&["record", "click"]);
    let late = "late      3 ticks read an input after it changed since the sync point: mouse at T.COM+0000E in 3 (first \
                tick 1)";
    assert!(recorded.contains(late), "the instruction after INT 33h: {recorded}");
    let tick = rrc.ok(&["trace", "click", "--tick", "2"]);
    assert!(tick.contains("late      mouse: T.COM+0000E read buttons "), "what the read saw (D68): {tick}");
    assert!(!rrc.ok(&["trace", "click", "--tick", "4"]).contains("late"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn memory_outside_the_image_is_mem() {
    let (dir, rrc) = setup("mem", &BIOS_TIMER);
    rrc.ok(&["init", ".", "--program", "T.COM", "--name", "t"]);
    rrc.ok(&["game", dir.join("game").to_str().unwrap(), "--add-version", "1.0"]);
    // A gate's input comes first, naming the sync point to come; rrc record waits for it.
    let input = rrc.ok(&["input", "MEM+0046C", "bios_ticks", "--size", "4", "--sync", "poll"]);
    assert!(input.contains("note      no sync point poll yet"), "{input}");
    rrc.ok(&["session", "add", "wait", "--seconds", "0.5"]);
    let (refused, ok) = rrc.run(&["record", "wait"]);
    assert!(!ok && refused.contains("no sync points"), "{refused}");
    rrc.ok(&["sync", "T.COM+0000E", "poll", "--when-changed", "bios_ticks"]);
    assert!(rrc.ok(&["check"]).contains("1 sync points, 1 inputs"));
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
    // A span of time instead, as for a loop with no sync point yet.
    let span = rrc.ok(&["inputs", "wait", "--from-ms", "100", "--to-ms", "300"]);
    assert!(span.contains("256 bytes, from 100 ms to 300 ms") && span.contains("bios_ticks"), "{span}");
    let path = rrc.ok(&["trace", "wait", "--path"]);
    assert!(Path::new(path.trim()).is_file(), "the path alone: {path}");
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
    // Where: the IN at +00003, every pass.
    let ports = rrc.ok(&["ports", "stick", "201"]);
    assert!(ports.contains("201h      read by T.COM+00003 ("), "{ports}");
    assert!(!rrc.ok(&["ports", "stick"]).contains("none"), "the timer's handler ends its interrupts too");
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
    let check = rrc.ok(&["check"]);
    assert!(check.contains("objects   #1 12673, #2 66960 bytes"), "its object table: {check}");
    rrc.ok(&["session", "add", "run", "--seconds", "0.5"]);
    let recorded = rrc.ok(&["record", "run"]);
    assert!(recorded.contains("trace     16 ticks"), "a tick a call: {recorded}");
    let squares = rrc.ok(&["trace", "run", "--item", "squares"]);
    let last = squares.lines().last().unwrap_or("");
    assert!(last.starts_with("16        [0x1]=1 [0x2]=4 [0x3]=9") && last.ends_with("[0xE]=196"), "{squares}");
    let watched = rrc.ok(&["watch", "run", "squares", "--tick", "3", "--ticks", "1"]);
    assert!(watched.contains("write squares[1] = 1 (was 0) by FLAT32.EXE#1+"), "an element: {watched}");
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
    // --asm is the same disassembly (D86).
    let asm = rrc.ok(&["show", "FLAT32.EXE#1+00024", "--asm"]);
    assert!(asm.contains("FLAT32.EXE#1+00041  mov [edx*4+squares],eax"), "{asm}");

    // A linker map's names, as in a port made to build a signature pack (sigs/README.md).
    let map = Path::new(env!("CARGO_MANIFEST_DIR")).join("../testbins/bin/FLAT32.MAP");
    let named = rrc.ok(&["sigs", "name", map.to_str().unwrap()]);
    assert!(named.contains("named     "), "{named}");
    let functions = std::fs::read_to_string(rrc.repo.join("knowledge/functions.tsv")).unwrap();
    assert!(functions.contains("FLAT32.EXE#1+000E0\tprintf_\tmap\t"), "{functions}");
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
    assert!(watched.contains("write count = 2 (was 1) by T.COM+00000"), "as its type reads: {watched}");
    // Its sessions aren't looked through for suspects here (D148): check says so, and the image is what's asked.
    let (check, _) = rrc.run(&["check"]);
    assert!(check.contains("image     ") && !check.contains("unknown"), "{check}");
    std::fs::remove_dir_all(&dir).ok();
}

/// The example port `example` in a test's port repository, with the workspace's `port` as its program.
fn example_repository(rrc: &Rrc, example: &str, port: &Path, files: &[&str]) {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("examples").join(example);
    for file in files {
        std::fs::create_dir_all(rrc.repo.join(file).parent().unwrap()).unwrap();
        std::fs::copy(example.join(file), rrc.repo.join(file)).unwrap();
    }
    let settings = std::fs::read_to_string(example.join("rrc.toml")).unwrap();
    let command = format!("command = [{:?}]", port.to_str().unwrap());
    let settings: Vec<String> =
        settings.lines().map(|l| if l.starts_with("command = ") { command.clone() } else { l.to_owned() }).collect();
    std::fs::write(rrc.repo.join("rrc.toml"), settings.join("\n")).unwrap();
}

#[test]
fn a_port_from_power_on_loads_the_game_s_files() {
    // The example port (examples/vault) of the vault game (testbins/src/vault.c): from power-on, through
    // the title's loop and the play's, the levels decoded from the game's VAULT.DAT as the original
    // loads them (M5).
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let (dir, mut rrc) = setup_file("vault", "VAULT.EXE", &std::fs::read(root.join("testbins/bin/VAULT.EXE")).unwrap());
    std::fs::copy(root.join("testbins/bin/VAULT.DAT"), dir.join("game").join("VAULT.DAT")).unwrap();
    let port = built("vault-port", "vault-port", true);
    let files = ["mapping.toml", "sessions/play.toml", "sessions/play.inputs", "src/main.rs"];
    let knowledge =
        ["functions.tsv", "data.tsv", "types.toml", "ticks.toml", "formats.toml"].map(|f| format!("knowledge/{f}"));
    example_repository(&rrc, "vault", &port, &[&files[..], &knowledge.each_ref().map(String::as_str)[..]].concat());
    rrc.env.push(("RRC_SNAPSHOT_MS", "1000"));
    assert!(rrc.ok(&["game", dir.join("game").to_str().unwrap()]).contains("version   1.0"));
    // Six seconds: Enter at 1.5 s loads the first level, its exit at 5.5 s the second.
    rrc.ok(&["session", "set", "play", "--seconds", "6"]);
    let recorded = rrc.ok(&["record", "play"]);
    assert!(recorded.contains("trace     400 ticks"), "{recorded}");
    let compared = rrc.ok(&["compare", "play"]);
    assert!(compared.contains("match     every mapped field at all 400 ticks"), "{compared}");
    // A quirk's switch reaches the port's run (D149): only a registered one, and it isn't kept as the port's progress.
    let (refused, ok) = rrc.run(&["compare", "play", "--fix", "nothing"]);
    assert!(!ok && refused.contains("no quirk nothing"), "{refused}");
    let (refused, ok) = rrc.run(&["compare", "play", "--lockstep", "--fix", "nothing"]);
    assert!(!ok && refused.contains("not --lockstep"), "{refused}");
    rrc.ok(&["quirk", "tried", "--at", "VAULT.EXE+00000", "--what", "for the test", "--fix", "none"]);
    let tried = rrc.ok(&["compare", "play", "--fix", "tried"]);
    assert!(tried.contains("fixed     tried:"), "{tried}");
    assert!(tried.contains("shown     not here: tried's fix changes nothing in these ticks"), "{tried}");
    rrc.ok(&["quirk", "tried", "--remove"]);
    // Until the first level, the map's buffer holds the far heap's links, which the port leaves unset
    // (D102): who wrote them is the C library's allocator.
    assert!(
        compared.contains(
            "unset     map at ticks 1-100: the original's held its value of tick 1, last written in the pass to \
             tick 1 by VAULT.EXE+014E1 (__AllocSeg < _fmalloc < main)"
        ),
        "{compared}"
    );
    // Its frames (D107): the title, then the levels as the player walks, the original's screen at every
    // tick's sync point. A wall drawn in the wrong colour: the first frame of play differs, and who drew the
    // pixel is the original's draw_cell, which the port's names.
    assert!(compared.contains("screen    the port's frames are the original's screen at all 400 ticks"), "{compared}");
    rrc.env.push(("VAULT_BUG", "colour"));
    let (drawn, ok) = rrc.run(&["compare", "play"]);
    assert!(!ok && drawn.contains("diverges  at tick 102 (play): the screen: 5580 pixels differ"), "{drawn}");
    assert!(drawn.contains("pixel   (64, 4): original 40, port 41"), "{drawn}");
    assert!(drawn.contains("write MEM+A0540 = 28h (was 20h) by VAULT.EXE+00581 in draw_cell < draw < main"), "{drawn}");
    assert!(drawn.contains("port      draw_cell (VAULT.EXE+004DD) is src/main.rs:"), "{drawn}");
    assert!(
        drawn.contains("next      its code: rrc show VAULT.EXE+004DD --c; the drawing code's own state"),
        "{drawn}"
    );
    // A write's chain, as many calls of it as asked.
    let one = rrc.ok(&["watch", "play", "MEM+A0540", "--len", "1", "--tick", "102", "--ticks", "1", "--chain", "1"]);
    assert!(one.contains("by VAULT.EXE+00581 in draw_cell < …"), "{one}");
    // The fields alone (D136): the logic matches past the screen it draws otherwise.
    let fields = rrc.ok(&["compare", "play", "--fields"]);
    assert!(fields.contains("match     every mapped field at all 400 ticks"), "{fields}");
    assert!(fields.contains("screen    not compared, nor the sound (--fields)"), "{fields}");
    rrc.env.pop();
    // From a transplant mid-play (D110): the presentation is given what the original's draw keeps, the count
    // its player's colour blinks by among it. Without it the screen can't show that count, and the colour
    // blinks out of step.
    let given = rrc.ok(&["compare", "play", "--transplant", "200"]);
    assert!(given.contains("given     the presentation's own state at tick 200: 4 fields (D110)"), "{given}");
    assert!(given.contains("screen    the port's frames are the original's screen at all 201 ticks"), "{given}");
    let presented = std::fs::read_to_string(rrc.repo.join(".rrc/compare/play/presentation.json")).unwrap();
    assert!(presented.contains(r#""format":"rrc-presentation""#) && presented.contains(r#""drawn.blink":"#));
    let mapping = std::fs::read_to_string(rrc.repo.join("mapping.toml")).unwrap();
    let unpresented = mapping.split("# The presentation's own state").next().unwrap();
    std::fs::write(rrc.repo.join("mapping.toml"), unpresented).unwrap();
    let (guessed, ok) = rrc.run(&["compare", "play", "--transplant", "200"]);
    assert!(!ok && guessed.contains("the screen: 36 pixels differ"), "{guessed}");
    std::fs::write(rrc.repo.join("mapping.toml"), mapping).unwrap();
    // Where the original is at a tick, for the port to play from without it (D120).
    let start = rrc.ok(&["start", "play", "--tick", "200"]);
    assert!(start.contains("play's tick 200: 16 fields mapped, the presentation's own 4, the screen"), "{start}");
    let folder = rrc.repo.join(".rrc/start/play-200");
    let state = std::fs::read_to_string(folder.join("state.json")).unwrap();
    assert!(state.contains(r#""format":"rrc-state""#) && state.contains(r#""tick":200"#), "{state}");
    assert!(folder.join("presentation.json").is_file() && folder.join("screen.frm").is_file());
    // From power-on (D141): what the start-up began with, for the port to play the whole game from.
    let power_on = rrc.ok(&["start", "play", "--tick", "0"]);
    assert!(power_on.contains("start     play's power-on: "), "{power_on}");
    let at_power_on = rrc.repo.join(".rrc/start/play-0");
    let state = std::fs::read_to_string(at_power_on.join("state.json")).unwrap();
    assert!(state.contains(r#""format":"rrc-state""#) && state.contains(r#""tick":0"#), "{state}");
    let inputs = std::fs::read_to_string(at_power_on.join("power-on.json")).unwrap();
    assert!(inputs.contains(r#""format":"rrc-power-on""#), "{inputs}");
    assert!(!at_power_on.join("screen.frm").exists() && at_power_on.join("saves").is_dir());
    // Memory the port carries as it was (D126): in the start's state, read at the tick, never compared.
    let mapping = std::fs::read_to_string(rrc.repo.join("mapping.toml")).unwrap();
    let carry = "\n[[carry]]\nfield = \"carried.counts\"\nat = \"pass_count\"\ntype = \"u8[2]\"\n";
    std::fs::write(rrc.repo.join("mapping.toml"), format!("{mapping}{carry}")).unwrap();
    let start = rrc.ok(&["start", "play", "--tick", "200"]);
    assert!(start.contains("16 fields mapped, 1 carried as they were, the presentation's own 4"), "{start}");
    let state = std::fs::read_to_string(folder.join("state.json")).unwrap();
    assert!(state.contains(r#""carried.counts":["#), "{state}");
    std::fs::write(rrc.repo.join("mapping.toml"), mapping).unwrap();

    // The archive by its format (D103): its members; the title as PNG with its palette; a level decoded as
    // the original loaded it, found in its memory at the tick after the read.
    assert!(rrc.run(&["check"]).0.contains("formats   4"));
    let listed = rrc.ok(&["asset", "VAULT.DAT"]);
    assert!(listed.contains("member    LEVEL2: +10FA, 144 bytes, lzss, 1024 decoded, level (map)"), "{listed}");
    let title = rrc.ok(&["asset", "VAULT.DAT/TITLE", "--out", dir.join("title.png").to_str().unwrap()]);
    assert!(title.contains("picture   320 × 200, palette VAULT.DAT/PALETTE"), "{title}");
    assert!(std::fs::read(dir.join("title.png")).unwrap().starts_with(b"\x89PNG"));
    let checked = rrc.ok(&["asset", "VAULT.DAT/LEVEL2", "--session", "play"]);
    assert!(
        checked.contains("read      in the pass to tick 374, by VAULT.EXE+00A0C < load < load_level < play_pass"),
        "{checked}"
    );
    assert!(
        checked.contains(
            "original  equal: 2 ms after the read (in the pass to tick 374), its memory holds these 1024 bytes at \
             VAULT.EXE+"
        ),
        "{checked}"
    );
    let palette = rrc.ok(&["asset", "VAULT.DAT/PALETTE", "--session", "play"]);
    assert!(
        palette.contains(
            "original  equal: 1 ms after the read (before tick 1), its memory holds these 768 bytes at palette"
        ),
        "{palette}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_port_s_sound_is_the_original_s() {
    // The example port (examples/chime) of the chime (testbins/src/chime.c): its notes on the speaker and the
    // OPL, made by its presentation, compared with the original's event by event, tick by tick (D108).
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let (dir, mut rrc) = setup_file("chime", "CHIME.EXE", &std::fs::read(root.join("testbins/bin/CHIME.EXE")).unwrap());
    let port = built("chime-port", "chime-port", true);
    let files = ["mapping.toml", "sessions/play.toml", "sessions/play.inputs", "src/main.rs"];
    let knowledge = ["functions.tsv", "data.tsv", "types.toml", "ticks.toml"].map(|f| format!("knowledge/{f}"));
    example_repository(&rrc, "chime", &port, &[&files[..], &knowledge.each_ref().map(String::as_str)[..]].concat());
    assert!(rrc.ok(&["game", dir.join("game").to_str().unwrap()]).contains("version   1.0"));
    let recorded = rrc.ok(&["record", "play"]);
    assert!(recorded.contains("trace     182 ticks"), "{recorded}");
    // What the trace shows of a note's start: the OPL's writes and the speaker's tone.
    let tick = rrc.ok(&["trace", "play", "--tick", "36"]);
    assert!(tick.contains("sound     opl: A0=59 B0=35") && tick.contains("speaker: tone 2281/1141"), "{tick}");
    let compared = rrc.ok(&["compare", "play"]);
    assert!(compared.contains("match     every mapped field at all 182 ticks"), "{compared}");
    assert!(compared.contains("sound     the port's sound is the original's at all 182 ticks"), "{compared}");
    assert!(compared.contains("screen    not compared: the port draws no frames"), "{compared}");
    // A note keyed an octave low: its key on differs, and who wrote the OPL's ports is named.
    rrc.env.push(("CHIME_BUG", "block"));
    let (heard, ok) = rrc.run(&["compare", "play"]);
    assert!(!ok && heard.contains("diverges  at tick 36 (pass): the opl sound: 2 events, the port's 2"), "{heard}");
    assert!(heard.contains("event   2: original B0=35, port B0=31"), "{heard}");
    assert!(heard.contains("389h      written by CHIME.EXE+0030F (2)"), "{heard}");
    assert!(heard.contains("next      the events in order: rrc trace play --tick 36"), "{heard}");
    rrc.env.pop();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_port_matches_the_original_tick_by_tick() {
    // The example port (examples/toy) of the toy game (testbins/src/toy.c), built here.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let (dir, mut rrc) = setup_file("toy", "TOY.EXE", &std::fs::read(root.join("testbins/bin/TOY.EXE")).unwrap());
    let port = built("toy-port", "toy-port", true);
    example_repository(
        &rrc,
        "toy",
        &port,
        &[
            "mapping.toml",
            "quirks.toml",
            "knowledge/functions.tsv",
            "knowledge/data.tsv",
            "knowledge/types.toml",
            "knowledge/ticks.toml",
            "sessions/play.toml",
            "sessions/play.inputs",
            "src/main.rs",
        ],
    );
    rrc.env.push(("RRC_SNAPSHOT_MS", "1000"));
    assert!(rrc.ok(&["game", dir.join("game").to_str().unwrap()]).contains("version   1.0"));
    assert!(rrc.ok(&["check"]).contains("mapping   15 fields mapped"));
    // Three seconds: through the first bounce, at tick 194.
    rrc.ok(&["session", "set", "play", "--seconds", "3"]);
    let recorded = rrc.ok(&["record", "play"]);
    assert!(recorded.contains("trace     209 ticks"), "{recorded}");

    let compared = rrc.ok(&["compare", "play"]);
    assert!(compared.contains("compare   play, ticks 1-209 of 209: 15 fields mapped (147 bytes)"), "{compared}");
    // The player's x found by how it behaves (D75): still before the keys, less while left is held, more
    // while right is.
    let found = rrc.ok(&["search", "play", "same:2-30", "less:40-130", "more:160-200", "--type", "i16"]);
    assert!(found.contains("found     player_x  2: 2560, 30: 2560, 40: "), "{found}");
    // The recording kept each tick's data segment (D92), so the search runs no emulator.
    assert!(found.contains("memory    ticks 1-209, kept (by the recording or a search)"), "{found}");
    // Another search within those ticks reads the kept memory too (D76): the same places.
    let again = rrc.ok(&["search", "play", "same:2-30", "less:40-130", "more:160-200", "--type", "i16"]);
    assert!(again.contains("memory    ticks 1-209, kept"), "{again}");
    let lines = |text: &str| text.lines().filter(|l| l.starts_with("found") || l.starts_with("search")).count();
    assert!(
        lines(&again) > 1
            && found
                .lines()
                .filter(|l| !l.starts_with("memory"))
                .eq(again.lines().filter(|l| !l.starts_with("memory")))
    );
    assert!(compared.contains("match     every mapped field at all 209 ticks"), "{compared}");
    // How far the port is (D117): the functions the original ran in passes that match and those the port
    // names; the state that changes, and how much of it is mapped.
    let progress = rrc.ok(&["progress"]);
    assert!(
        progress.contains("session   play: ticks 1-209 of 209 match, from the port's start, no feed"),
        "{progress}"
    );
    let code = "code      TOY.EXE ran 25 functions in the sessions; 25 (100%) in passes that match, and the port \
                names 4 (16%) of those";
    assert!(progress.contains(code), "{progress}");
    assert!(progress.contains("unnamed   wait_tick (TOY.EXE+000F0): 210 calls in the sessions"), "{progress}");
    // The trail on the heap is mapped through its pointer, where it pointed at each tick.
    let state = "state     play: 107 bytes kept at every tick change; mapped 99 (92%), ignored 0 (0%), neither 8 (7%)";
    assert!(progress.contains(state), "{progress}");
    // A bug: the port doesn't count a bounce. The divergence, and who wrote the original's value; a quirk
    // registered at that code is named (D74).
    // The first rule (D160): the original's own bug is kept and has a switch, and a session that reaches it shows
    // the switch: the port matches with the bug kept, differs with it fixed, and where is kept with the quirk.
    let quirks = rrc.repo.join("quirks.toml");
    let register = std::fs::read_to_string(&quirks).unwrap();
    assert!(register.contains("shown = \"play tick 194: enemies[1].x\""), "{register}");
    std::fs::write(&quirks, register.replace("shown = \"play tick 194: enemies[1].x\"\n", "")).unwrap();
    assert!(rrc.ok(&["quirk"]).contains("  shown   not yet: rrc compare SESSION --fix bounce_step"));
    let (fixed, ok) = rrc.run(&["compare", "play", "--fix", "bounce_step"]);
    assert!(!ok && fixed.contains("diverges  at tick 194 (play): 1 field differ"), "{fixed}");
    assert!(fixed.contains("  field   enemies[1].x: original 3, port 0"), "the enemy put on the wall: {fixed}");
    let shown = "shown     bounce_step: its switch changes play tick 194: enemies[1].x; kept in quirks.toml";
    assert!(fixed.contains(shown), "{fixed}");
    assert_eq!(std::fs::read_to_string(&quirks).unwrap(), register, "as the example has it");
    assert!(rrc.ok(&["quirk"]).contains("  shown   play tick 194: enemies[1].x"));
    rrc.ok(&["quirk", "bounces", "--at", "TOY.EXE+002FF", "--what", "counts a bounce", "--fix", "doesn't count it"]);
    rrc.env.push(("TOY_BUG", "1"));
    let (diverged, ok) = rrc.run(&["compare", "play"]);
    assert!(!ok, "a divergence fails: {diverged}");
    assert!(diverged.contains("diverges  at tick 194 (play): 1 field differ"), "{diverged}");
    // A field a line, with how many of its values differ when more than one does (D140).
    assert!(diverged.contains("  field   enemies[1].hits: original 1, port 0\n"), "one of its values: {diverged}");
    assert!(diverged.contains("write enemies[1].hits = 1 (was 0) by TOY.EXE+002FF in update < main"), "{diverged}");
    // The port's code for the function that wrote it (D73).
    assert!(diverged.contains("port      update (TOY.EXE+00114) is src/main.rs:"), "{diverged}");
    assert!(diverged.contains("quirk     bounces is at TOY.EXE+002FF"), "{diverged}");
    // The next moves, of more than one kind (D159): the writer's code, the value over the pass, what is written.
    let next = "next      its code: rrc show TOY.EXE+00114 --c; the value over the pass: rrc watch play TOY.EXE+";
    assert!(diverged.contains(next) && diverged.contains("what is written of it: rrc sources enemies"), "{diverged}");
    // And what else is held about a place: the quirk in it, the port's field mapped to it, the port's code.
    let shown = rrc.ok(&["show", "TOY.EXE+002FF"]);
    assert!(shown.contains("quirk     bounces at TOY.EXE+002FF: counts a bounce"), "{shown}");
    assert!(rrc.ok(&["show", "enemies"]).contains("mapped    the port's enemies (mapping.toml)"));
    assert!(rrc.ok(&["show", "update"]).contains("port      named in src/main.rs:"));
    let progress = rrc.ok(&["progress"]);
    assert!(progress.contains("session   play: ticks 1-193 of 209 match, then its state differs"), "{progress}");
    // What the port noted in the pass to it (D115).
    assert!(diverged.contains("noted     in the pass to tick 194: a bounce not counted (TOY.EXE+002FF)"), "{diverged}");
    let (all, _) = rrc.run(&["compare", "play", "--notes"]);
    assert!(
        all.contains("note      a bounce not counted (TOY.EXE+002FF): first in the pass to tick 194, in 1 ticks"),
        "{all}"
    );
    // The same first difference a third time running is a route that stopped paying (D157): rrc says so, with the
    // kinds of evidence nobody looked at since it first showed. The watch between is one of them.
    assert!(!all.contains("stuck     "), "twice isn't stuck: {all}");
    rrc.ok(&["watch", "play", "enemies", "--tick", "194", "--ticks", "1"]);
    let (third, _) = rrc.run(&["compare", "play"]);
    assert!(
        third.contains("stuck     the same first difference 3 times running: tick 194: enemies[1].hits"),
        "{third}"
    );
    // The code was looked at since (the rrc show above) and so was the run: what is left is the sources and the files.
    assert!(
        third.contains("untried   since it first showed: an outside source (rrc sources WORD); the game"),
        "{third}"
    );
    assert!(!third.contains("the code (rrc show ID --c") && !third.contains("the value over the run"), "{third}");
    let journal = rrc.ok(&["journal", "--lines", "2"]);
    assert!(journal.contains("stuck     1 times a comparison ended at the same first difference 3 times running"));
    assert!(journal.contains("ran       compare play -> tick 194: enemies[1].hits"), "{journal}");
    assert!(journal.contains("mix       compare 5, code 3, sources 0, files 0, runs "), "{journal}");
    rrc.ok(&["quirk", "bounces", "--remove"]);
    // The enemies fed from the original (D84): the rest is compared over the whole session.
    let fed = rrc.ok(&["compare", "play", "--feed", "enemies"]);
    assert!(fed.contains("fed       enemies: the original's values at every tick"), "{fed}");
    assert!(fed.contains("match     every mapped field at all 209 ticks"), "{fed}");
    // What the feed hid (D114): the bounce the port didn't count.
    assert!(fed.contains("fed       enemies: the port's own differed at "), "{fed}");
    assert!(fed.contains("first at tick 194 (enemies[1].hits)"), "{fed}");
    let (unknown, ok) = rrc.run(&["compare", "play", "--feed", "nothing"]);
    assert!(!ok && unknown.contains("--feed nothing: no mapped field is it or in it"), "{unknown}");
    // In lockstep (D124): the original's state before every pass, every difference counted, none stopping it.
    let (census, ok) = rrc.run(&["compare", "play", "--lockstep"]);
    assert!(!ok, "a difference fails: {census}");
    assert!(census.contains("differs   enemies: in "), "{census}");
    assert!(census.contains("first at play's tick 194 (enemies[1].hits: original 1, port 0)"), "{census}");
    assert!(census.contains("note      a bounce not counted (TOY.EXE+002FF): in "), "{census}");
    assert!(census.contains("census    ") && census.contains("census.json"), "{census}");
    assert!(census.contains("next      rrc work: this with every other session's census, ranked"), "{census}");
    rrc.env.pop();
    let matched = rrc.ok(&["compare", "play", "--lockstep"]);
    assert!(matched.contains("match     play: every mapped field, frame and sound in all 208 passes"), "{matched}");
    let needless = rrc.ok(&["compare", "play", "--feed", "enemies"]);
    assert!(
        needless.contains("fed       enemies: the port's own was the original's at all 208 ticks fed: the feed can go"),
        "{needless}"
    );
    // Another, in the trail on the heap (D72): who wrote it, where the far pointer pointed then.
    rrc.env.push(("TOY_BUG", "trail"));
    let (diverged, ok) = rrc.run(&["compare", "play"]);
    assert!(!ok && diverged.contains("trail[1]: original 2560, port 3168"), "{diverged}");
    assert!(
        diverged.contains("write TOY.EXE+01CCA = 0A00h (was 0000h) by TOY.EXE+00236 in update < main"),
        "{diverged}"
    );
    rrc.env.pop();

    // From the original's values at a tick; a paired checkpoint, and from it.
    let transplanted = rrc.ok(&["compare", "play", "--transplant", "100"]);
    assert!(transplanted.contains("match     every mapped field at all 110 ticks"), "{transplanted}");
    let paired = rrc.ok(&["compare", "play", "--pair", "150", "--to", "160"]);
    assert!(paired.contains("pair      kept at tick 150"), "{paired}");
    let session = std::fs::read_to_string(rrc.repo.join("sessions/play.toml")).unwrap();
    assert!(
        session.contains(
            "[[pair]]
tick = 150"
        ),
        "{session}"
    );
    let resumed = rrc.ok(&["compare", "play", "--resume", "150"]);
    assert!(resumed.contains("ticks 150-209 of 209") && resumed.contains("match "), "{resumed}");

    // A scenario (D64): edits at tick 100. The trace lists them, a watch names the edit as the writer,
    // and the port gets them with the inputs.
    let edited = rrc.ok(&["session", "edit", "play", "100", "score", "5000"]);
    assert!(edited.contains("edit      tick 100: score = 5000 (TOY.EXE+0147A 8813)"), "{edited}");
    rrc.ok(&["session", "edit", "play", "100", "enemies[0]", r#"{"alive": 0}"#]);
    let (refused, ok) = rrc.run(&["session", "edit", "play", "100", "lives", "300"]);
    assert!(!ok && refused.contains("isn't in 0 to 255"), "{refused}");
    let recorded = rrc.ok(&["record", "play"]);
    assert!(recorded.contains("edits     2") && recorded.contains("pairs     1 dropped"), "{recorded}");
    let tick = rrc.ok(&["trace", "play", "--tick", "100"]);
    assert!(tick.contains("edit      score = 5000 (TOY.EXE+0147A: 8813)"), "{tick}");
    let watched = rrc.ok(&["watch", "play", "score", "--tick", "100", "--ticks", "1"]);
    assert!(watched.contains("write score = 5000 (was 99) by the session's edit"), "{watched}");
    let compared = rrc.ok(&["compare", "play"]);
    assert!(compared.contains("match     every mapped field at all 209 ticks"), "{compared}");
    // In lockstep, what the edits wrote at their tick isn't a difference of that pass (D146).
    let stepped = rrc.ok(&["compare", "play", "--lockstep"]);
    assert!(!stepped.contains("differs   score") && !stepped.contains("differs   enemies"), "{stepped}");

    // A field the port's state hasn't: missing at the first tick.
    let mapping = std::fs::read_to_string(rrc.repo.join("mapping.toml")).unwrap();
    std::fs::write(rrc.repo.join("mapping.toml"), mapping.replace("field = \"lives\"", "field = \"lifes\"")).unwrap();
    rrc.ok(&["record", "play"]);
    let (missing, ok) = rrc.run(&["compare", "play"]);
    assert!(!ok && missing.contains("lifes: original 3, port missing"), "{missing}");
    assert!(missing.contains("start     at the port's first tick"), "{missing}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_workflow_s_stages_are_walked() {
    // The toy port taken through the workflow (D154, D158): at each stage `rrc status` names it, what its gate lacks
    // and the next step, worked out from the repository, and the step makes the gate hold.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let (dir, mut rrc) = setup_file("stages", "TOY.EXE", &std::fs::read(root.join("testbins/bin/TOY.EXE")).unwrap());
    let port = built("toy-port", "toy-port", true);
    let knowledge = ["functions.tsv", "data.tsv", "types.toml", "ticks.toml"].map(|f| format!("knowledge/{f}"));
    let files = ["mapping.toml", "quirks.toml", "sessions/play.toml", "sessions/play.inputs", "src/main.rs"];
    example_repository(&rrc, "toy", &port, &[&files[..], &knowledge.each_ref().map(String::as_str)[..]].concat());
    rrc.env.push(("RRC_SNAPSHOT_MS", "1000"));
    let status = |rrc: &Rrc| rrc.ok(&["status"]);
    let at = |text: &str, stage: &str| assert!(text.contains(&format!("stage     {stage}, of 0 Set up")), "{text}");

    // 0 Set up: the game's folder, then the person's rules: asked for by `rrc hooks` in a port made before them.
    let s = status(&rrc);
    at(&s, "0 Set up");
    assert!(
        s.contains("lacks     the game's folder isn't set") && s.contains("next      rrc game <the game's folder>")
    );
    assert!(s.contains("lacks     no RULES.md: the person's rules for this port"), "{s}");
    assert!(rrc.ok(&["game", dir.join("game").to_str().unwrap()]).contains("version   1.0"));
    assert!(rrc.ok(&["hooks"]).contains("entry     RULES.md, AGENTS.md, CLAUDE.md, notes/README.md"));
    let s = status(&rrc);
    assert!(s.contains("next      ask the person, and write each answer in its place in RULES.md"), "{s}");
    std::fs::write(rrc.repo.join("RULES.md"), "## Rules the person set for this port\n\n- All of it.\n").unwrap();

    // 1 Survey: the analysis (a stand-in: tests have no Ghidra), the session recorded, the outside sources swept.
    let s = status(&rrc);
    at(&s, "1 Survey");
    assert!(s.contains("lacks     the program isn't analysed") && s.contains("next      rrc analyze"), "{s}");
    assert!(s.contains("lacks     the outside sources aren't swept"), "{s}");
    let export = r#"{"record":"program","name":"TOY.EXE","entry":{"linear":65536,"text":"1000:0000"},"ghidra":"-"}"#;
    let cache = rrc::cache::Cache::at(&rrc.cache).unwrap();
    let export = cache.put(export.as_bytes()).unwrap();
    let program = cache.put(&std::fs::read(root.join("testbins/bin/TOY.EXE")).unwrap()).unwrap();
    let index = format!(
        "original = \"{program}\"\nprogram = \"{program}\"\nunpacked = false\nexport = \"{export}\"\nentry = 0\nghidra \
         = \"-\"\n"
    );
    std::fs::write(rrc.repo.join(".rrc/analysis.toml"), index).unwrap();
    let s = status(&rrc);
    assert!(
        s.contains("lacks     play isn't recorded as the knowledge is now") && s.contains("next      rrc record play")
    );
    rrc.ok(&["session", "set", "play", "--seconds", "3"]);
    rrc.ok(&["record", "play"]);
    let none = "wiki,formats,reimplementation,tools,patches,walkthrough,bugs,library,extender,sound,compression";
    rrc.ok(&["sources", "sweep", "--searched", "the toy game", "--none", none]);

    // 2 Sessions: the scope, as the person said.
    let s = status(&rrc);
    at(&s, "2 Sessions");
    assert!(s.contains("; 0 to 1 hold") && s.contains("lacks     no scope: the sessions the port is to reproduce"));
    rrc.ok(&["scope", "play"]);

    // 3 Skeleton: a census of every session in scope. The port doesn't count a bounce yet.
    let s = status(&rrc);
    at(&s, "3 Skeleton");
    assert!(s.contains("lacks     the session without a census of the recording: play"), "{s}");
    assert!(s.contains("next      rrc compare play --lockstep (rrc work --run: every one)"), "{s}");
    rrc.env.push(("TOY_BUG", "1"));
    let counted = rrc.ok(&["work", "--run"]);
    assert!(counted.contains("counted   play: 208 passes, 1 fields, 1 notes"), "{counted}");
    assert!(counted.contains("work      2 items in the censuses of 1 of 1 sessions in scope"), "{counted}");
    assert!(counted.contains("differs   enemies: in ") && counted.contains("first at play's tick 194"), "{counted}");
    assert!(counted.contains("note      a bounce not counted (TOY.EXE+002FF): not ported, taken in "), "{counted}");
    assert!(counted.contains("next      rrc work take \"enemies\" --question TEXT --route TEXT --other TEXT"));

    // 4 Logic: the work list's top item, taken with its question, two routes and a budget; the comparisons count
    // against it, and rrc says when they are spent.
    let s = status(&rrc);
    at(&s, "4 Logic");
    assert!(s.contains("lacks     the session with fields in the census the port leaves otherwise or unset: play (1)"));
    assert!(s.contains("next      rrc work: the list, the most passes first"), "{s}");
    let (refused, ok) = rrc.run(&["work", "take", "enemies", "--question", "who counts a bounce?"]);
    assert!(!ok && refused.contains("the route chosen and one other considered"), "{refused}");
    let take = ["work", "take", "enemies", "--question", "who counts a bounce?", "--route", "the writer's code"];
    let taken = rrc.ok(&[&take[..], &["--other", "a watch of hits over the session", "--budget", "2"]].concat());
    assert!(taken.contains("card      enemies: who counts a bounce?"), "{taken}");
    assert!(taken.contains("budget    2 comparisons, then the route changes"), "{taken}");
    let (first, _) = rrc.run(&["compare", "play", "--fields"]);
    assert!(first.contains("diverges  at tick 194") && !first.contains("budget    "), "{first}");
    let (second, _) = rrc.run(&["compare", "play", "--fields"]);
    assert!(second.contains("budget    spent: this route was to change after 2 comparisons"), "{second}");
    assert!(second.contains("The other route: a watch of hits over the session"), "{second}");
    let s = status(&rrc);
    assert!(s.contains("card      enemies: who counts a bounce?"), "{s}");
    assert!(s.contains("spent     2 of 2 comparisons; compare 2, code 0, sources 0, files 0, runs 0"), "{s}");
    // The port counts it: the census is clean, the card closes with what it took, and the fields match.
    rrc.env.pop();
    rrc.ok(&["compare", "play", "--lockstep"]);
    // Closing asks which bugs of the original the item met (D160): none, or their registered names.
    let (refused, ok) = rrc.run(&["work", "done"]);
    assert!(
        !ok && refused.contains(
            "the first rule: every bug of the original that changes what the game does is reproduced and has a switch"
        )
    );
    let (refused, ok) = rrc.run(&["work", "done", "--bugs", "bounce_wall"]);
    assert!(!ok && refused.contains("no quirk bounce_wall: register it first"), "{refused}");
    let done = rrc.ok(&["work", "done", "--bugs", "bounce_step"]);
    assert!(done.contains("; bugs of the original met: bounce_step"), "{done}");
    assert!(done.contains("closed    enemies: not in the censuses as they stand"), "{done}");
    assert!(done.contains("took      enemies: 5 commands over "), "{done}");
    assert!(rrc.ok(&["journal"]).contains("item      enemies: 5 commands over "));
    let s = status(&rrc);
    at(&s, "4 Logic");
    assert!(s.contains(
        "not matching in the mapped fields to the end with nothing fed: play (its state differs at tick 194)"
    ));
    assert!(s.contains("next      rrc compare play --fields") && !s.contains("card      "), "{s}");
    rrc.ok(&["compare", "play", "--fields"]);
    let s = status(&rrc);
    assert!(s.contains("lacks     rrc check refuses 1 things; the first: session play: its run hasn't been"), "{s}");
    rrc.ok(&["suspects", "play"]);

    // 5 Data and start-up holds with it: that comparison was from the port's own start. 6 Presentation is where
    // the toy port stands: it draws nothing, and the next step is to write that, not to compare again.
    let s = status(&rrc);
    at(&s, "6 Presentation");
    assert!(
        s.contains("; 0 to 5 hold") && s.contains("play (not compared)") && s.contains("next      rrc compare play")
    );
    rrc.ok(&["compare", "play"]);
    let all = rrc.ok(&["status", "--all"]);
    assert!(all.contains("gate      5 Data and start-up: holds"), "{all}");
    let lacks = "gate      6 Presentation: lacks: the session not matching in frames and sound: play (the port draws no \
                 frames)";
    assert!(all.contains(lacks) && all.contains("gate      7 Play: lacks: play isn't tested by rrc"), "{all}");
    let play = "session   play (209 ticks): fields match from the port's start; census: 0 fields, 0 unset, 0 notes";
    assert!(all.contains(play) && all.contains("work      0 items in the censuses (rrc work)"), "{all}");
    assert!(all.contains("quirks    1 registered") && all.contains("mix       compare "), "{all}");
    assert!(
        all.starts_with(
            "bugs      every bug of the original that changes what the game does is reproduced and has a switch"
        ),
        "{all}"
    );
    assert!(all.contains("next      the port's presentation draws no frames yet: Presentation::draw"), "{all}");
    // The port's code changed since: what was compared before it is said to be, and counted again.
    let main = rrc.repo.join("src/main.rs");
    std::fs::write(&main, std::fs::read_to_string(&main).unwrap() + "\n// A change.\n").unwrap();
    let s = status(&rrc);
    let stale = "stale     the session with a census or a comparison made before the port's code changed: play";
    assert!(s.contains(stale), "{s}");
    // One command brings every gate up to the code: the census counted again, the session compared again.
    let again = rrc.ok(&["work", "--run"]);
    assert!(again.contains("counted   play: 208 passes, 0 fields, 0 notes"), "{again}");
    assert!(again.contains("compared  play: ticks 1-209 of 209 match, from the port's start, no feed"), "{again}");
    assert!(!status(&rrc).contains("stale     "));
    assert!(rrc.ok(&["work", "--run"]).contains("counted   nothing: every census and comparison is of its recording"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_session_starts_from_another_s_tick() {
    // D118: the toy game from play's tick 100 (1.4 s from power-on, left held), with play's input from there,
    // with none, and with an edit.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let (dir, mut rrc) = setup_file("based", "TOY.EXE", &std::fs::read(root.join("testbins/bin/TOY.EXE")).unwrap());
    let port = built("toy-port", "toy-port", true);
    example_repository(
        &rrc,
        "toy",
        &port,
        &[
            "mapping.toml",
            "quirks.toml",
            "knowledge/functions.tsv",
            "knowledge/data.tsv",
            "knowledge/types.toml",
            "knowledge/ticks.toml",
            "sessions/play.toml",
            "sessions/play.inputs",
            "src/main.rs",
        ],
    );
    rrc.env.push(("RRC_SNAPSHOT_MS", "1000"));
    rrc.ok(&["game", dir.join("game").to_str().unwrap()]);
    rrc.ok(&["session", "set", "play", "--seconds", "3"]);
    rrc.ok(&["record", "play"]);
    // Play's input after tick 100: its times count from power-on.
    let rest = dir.join("rest.inputs");
    std::fs::write(&rest, "2200:{hold right 3000}\n").unwrap();
    let (early, ok) = rrc.run(&["session", "add", "early", "--from", "play", "--tick", "100", "--seconds", "1"]);
    assert!(!ok && early.contains("is at 1.4 s from power-on: --seconds counts from power-on too"), "{early}");
    let rest_arg = rest.to_str().unwrap();
    let args = ["session", "add", "later", "--from", "play", "--tick", "100", "--seconds", "3", "--inputs", rest_arg];
    let added = rrc.ok(&args);
    assert!(added.contains("from      play's tick 100 (1.4 s from power-on) is its tick 1"), "{added}");
    let recorded = rrc.ok(&["record", "later"]);
    assert!(recorded.contains("trace     110 ticks"), "{recorded}");
    // Its tick 1 is play's tick 100; it goes on as play did, the left key let go when play let go of it.
    let values = |text: String| text.lines().filter(|l| l.starts_with("value")).map(str::to_owned).collect::<Vec<_>>();
    for (t, u) in [("1", "100"), ("50", "149"), ("100", "199")] {
        let later = values(rrc.ok(&["peek", "later", "--tick", t, "player_x", "score", "rng_seed"]));
        let play = values(rrc.ok(&["peek", "play", "--tick", u, "player_x", "score", "rng_seed"]));
        assert!(!later.is_empty() && later == play, "tick {t} of later, {u} of play: {later:?} {play:?}");
    }
    // Without input from there the player stops when left is let go, and right is never held.
    rrc.ok(&["session", "add", "still", "--from", "play", "--tick", "100", "--seconds", "3"]);
    rrc.ok(&["record", "still"]);
    let x = |session: &str, tick: &str| values(rrc.ok(&["peek", session, "--tick", tick, "player_x"]));
    assert_eq!(x("still", "50"), x("play", "149"));
    assert_eq!(x("still", "100"), x("still", "50"), "still");
    assert_ne!(x("still", "100"), x("play", "199"), "play went right");
    // What the run before it wrote isn't a read of nothing in it (D150): it finds none that play, from power-on, doesn't.
    let unwritten_reads = |session: &str| {
        let found = rrc.ok(&["suspects", session, "--lines", "0"]);
        let (before, _) = found.split_once(" reading memory nothing wrote").expect("suspects' line");
        let count = before.rsplit(", ").next().unwrap().split(' ').next().unwrap();
        count.parse::<usize>().unwrap_or_else(|_| panic!("{found}"))
    };
    let (play_reads, still_reads) = (unwritten_reads("play"), unwritten_reads("still"));
    assert!(still_reads <= play_reads, "still {still_reads}, play {play_reads}");
    // The port compares from a transplant at its first tick, not from the port's start.
    let (unstarted, ok) = rrc.run(&["compare", "later"]);
    assert!(!ok && unstarted.contains("starts at play's tick 100: compare it from a transplant"), "{unstarted}");
    let compared = rrc.ok(&["compare", "later", "--transplant", "1"]);
    assert!(compared.contains("match     every mapped field at all 110 ticks"), "{compared}");
    // An edit at its tick 10 is at its tick 10.
    rrc.ok(&["session", "edit", "later", "10", "score", "77"]);
    rrc.ok(&["record", "later"]);
    assert!(rrc.ok(&["peek", "later", "--tick", "10", "score"]).contains("value     score = 77"));
    assert!(!rrc.ok(&["peek", "later", "--tick", "9", "score"]).contains("score = 77"), "not before its tick");
    // A question about the whole run is about that run, the edit in it (D152): else it's refused, its end not the recording's.
    rrc.ok(&["suspects", "later", "--lines", "0"]);
    let listed = rrc.ok(&["session"]);
    assert!(listed.contains("later: 3 s, 110 ticks, from play's tick 100, 1 edit"), "{listed}");
    // A sync point added to the base numbers its ticks otherwise (D128): `later` starts where it did, its base tick
    // found again by its cycle and its number set.
    let kept = std::fs::read_to_string(rrc.repo.join("sessions/later.toml")).unwrap();
    assert!(kept.contains("cycle = "), "kept by its cycle too: {kept}");
    rrc.ok(&["sync", "TOY.EXE+00114", "update"]);
    rrc.ok(&["record", "play"]);
    let settled = rrc.ok(&["record", "later"]);
    assert!(settled.contains("from      play's tick 100 is its tick 198 now"), "{settled}");
    let at_start = values(rrc.ok(&["peek", "later", "--tick", "1", "player_x", "score", "rng_seed"]));
    assert_eq!(at_start, values(rrc.ok(&["peek", "play", "--tick", "198", "player_x", "score", "rng_seed"])));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_play_is_followed_in_parts_and_compared_in_lockstep() {
    // D124: play recorded from play's tick 100 (1.4 s), right held from 2.2 s to 2.6 s, followed every 0.4 s of
    // it, as far as its marks (every 0.25 s) allow: parts from 1.4, 1.9, 2.4 and 2.9 s, the key held across the
    // third's start.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let (dir, mut rrc) = setup_file("followed", "TOY.EXE", &std::fs::read(root.join("testbins/bin/TOY.EXE")).unwrap());
    let port = built("toy-port", "toy-port", true);
    example_repository(
        &rrc,
        "toy",
        &port,
        &[
            "mapping.toml",
            "quirks.toml",
            "knowledge/functions.tsv",
            "knowledge/data.tsv",
            "knowledge/types.toml",
            "knowledge/ticks.toml",
            "sessions/play.toml",
            "sessions/play.inputs",
            "src/main.rs",
        ],
    );
    rrc.env.push(("RRC_SNAPSHOT_MS", "1000"));
    rrc.ok(&["game", dir.join("game").to_str().unwrap()]);
    rrc.ok(&["session", "set", "play", "--seconds", "3"]);
    rrc.ok(&["record", "play"]);
    let c = |ms: u64| ms * 8000;
    let mut text = "# clockwork session v1
program TOY.EXE
args 
ips 8000000
"
    .to_owned();
    for (ms, lines) in [
        (1400, vec!["# at"]),
        (1650, vec!["# at"]),
        (1900, vec!["# at"]),
        (2150, vec!["# at"]),
        (2200, vec!["K E0", "K 4D"]),
        (2400, vec!["# at"]),
        (2600, vec!["K E0", "K CD"]),
        (2650, vec!["# at"]),
        (2900, vec!["# at"]),
        (3000, vec!["# end"]),
    ] {
        for line in lines {
            let line = match line.strip_prefix("# ") {
                Some(mark) => format!(
                    "# {mark} {}
",
                    c(ms)
                ),
                None => format!(
                    "{} {line}
",
                    c(ms)
                ),
            };
            text.push_str(&line);
        }
    }
    let recorded = dir.join("held.session");
    std::fs::write(&recorded, text).unwrap();
    let held = recorded.to_str().unwrap();
    let args = ["play", "hold", "--from", "play", "--tick", "100", "--every", "0.4", "--replay", held];
    let followed = rrc.ok(&args);
    assert!(
        followed.contains("play      hold: the original from play's tick 100, recorded in sessions/hold.session"),
        "{followed}"
    );
    assert!(followed.contains("start     the original's state at play's tick 100 isn't kept: made now"), "{followed}");
    assert!(followed.contains("play      hold: 0:02 played from play's tick 100 (1.4 s from power-on)"), "{followed}");
    assert_eq!(followed.matches("passes match").count(), 4, "four parts, each matching: {followed}");
    assert!(followed.contains("part      hold/4: "), "{followed}");
    assert!(followed.contains("4 parts (sessions/hold/1 to 4)"), "{followed}");
    assert!(followed.contains("match     hold: every mapped field, frame and sound in all "), "{followed}");
    let (taken, ok) = rrc.run(&args);
    assert!(!ok && taken.contains("there is a session hold already"), "{taken}");
    // The parts, one from the last tick of the one before, are the play recorded whole: no input lost or
    // given twice where a part begins.
    let ticks: u64 = followed
        .lines()
        .find(|l| l.starts_with("play") && l.contains(" parts "))
        .and_then(|l| l.rsplit(", ").next()?.strip_suffix(" ticks")?.parse().ok())
        .unwrap_or_else(|| panic!("{followed}"));
    let whole = rrc.ok(&["record", "hold"]);
    assert!(whole.contains(&format!("trace     {ticks} ticks")), "{ticks}: {whole}");
    let last = std::fs::read_to_string(rrc.repo.join("sessions/hold/4.toml")).unwrap();
    let last: u64 =
        last.lines().find_map(|l| l.strip_prefix("ticks = ")?.parse().ok()).unwrap_or_else(|| panic!("{last}"));
    let values = |text: String| text.lines().filter(|l| l.starts_with("value")).map(str::to_owned).collect::<Vec<_>>();
    let ends = |session: &str, tick: u64| {
        values(rrc.ok(&["peek", session, "--tick", &tick.to_string(), "player_x", "score", "rng_seed"]))
    };
    assert_eq!(ends("hold", ticks), ends("hold/4", last));
    // A port that doesn't count a bounce: said in the part it is first in, and where to look.
    rrc.env.push(("TOY_BUG", "1"));
    let args = ["play", "bugged", "--from", "play", "--tick", "100", "--every", "0.4", "--replay", held];
    let bugged = rrc.ok(&args);
    assert!(
        bugged.contains("new       differs enemies: in ") && bugged.contains("enemies[1].hits: original 1, port 0"),
        "{bugged}"
    );
    assert!(bugged.contains("next      rrc compare bugged/"), "{bugged}");
    rrc.env.pop();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn state_behind_pointers_is_mapped_through_them() {
    // D122: testbins/src/list.c's actors in a list on the far heap (`actors`, each's `next` at +4), an array on
    // the heap (`table`) and a pointer into it (`current`); a tick ends where `pass` returns (+00147).
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let (dir, rrc) = setup_file("list", "LIST.EXE", &std::fs::read(root.join("testbins/bin/LIST.EXE")).unwrap());
    rrc.ok(&["init", "--program", "LIST.EXE"]);
    rrc.ok(&["game", dir.join("game").to_str().unwrap()]);
    for (id, name, size, ty) in [
        ("LIST.EXE+02528", "table", "4", "far"),
        ("LIST.EXE+0252C", "current", "4", "far"),
        ("LIST.EXE+02530", "actors", "4", "far"),
        ("LIST.EXE+02534", "pass_count", "2", "u16"),
    ] {
        rrc.ok(&["data", id, name, "--size", size, "--type", ty, "--by", "human"]);
    }
    rrc.ok(&["sync", "LIST.EXE+00147", "pass", "--by", "human", "--evidence", "where pass returns to main's loop"]);
    std::fs::write(
        rrc.repo.join("knowledge/types.toml"),
        "[[struct]]\nname = \"actor\"\nsize = 8\nfields = [\n\
         { offset = 0, size = 2, name = \"x\", type = \"i16\" },\n\
         { offset = 2, size = 2, name = \"hits\", type = \"i16\" },\n\
         { offset = 4, size = 4, name = \"next\", type = \"far\" },\n]\n",
    )
    .unwrap();
    std::fs::write(
        rrc.repo.join("mapping.toml"),
        "[[map]]\nfield = \"pass_count\"\nat = \"pass_count\"\n\n\
         [[map]]\nfield = \"actors\"\nvia = \"actors\"\ntype = \"actor\"\nnext = 4\ncount = 8\n\n\
         [[map]]\nfield = \"table\"\nvia = \"table\"\ntype = \"i16[8]\"\n\n\
         [[map]]\nfield = \"current\"\nat = \"current\"\ntype = \"far\"\ninto = \"table\"\n\n\
         [[map]]\nfield = \"second_hits\"\nvia = \"actors\"\nthen = [{ offset = 4, pointer = \"far\" }]\noffset = 2\n\
         type = \"i16\"\n",
    )
    .unwrap();
    assert!(rrc.ok(&["check"]).contains("mapping   5 fields mapped"));
    rrc.ok(&["session", "add", "run", "--seconds", "2"]);
    let recorded = rrc.ok(&["record", "run"]);
    assert!(recorded.contains("trace     100 ticks"), "{recorded}");
    // The mapped fields' values at a tick, as a transplant would give them (rrc start, D120).
    let state = |tick: &str| -> serde_json::Value {
        let out = dir.join(format!("start-{tick}"));
        rrc.ok(&["start", "run", "--tick", tick, "--out", out.to_str().unwrap()]);
        let text = std::fs::read_to_string(out.join("state.json")).unwrap();
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["state"].clone()
    };
    let xs = |s: &serde_json::Value| -> Vec<i64> {
        s["actors"].as_array().unwrap().iter().map(|a| a["x"].as_i64().unwrap()).collect()
    };
    // After 10 passes: each actor moved by its place in the list + 1 each pass; the array written at
    // pass & 7; `current` at (pass × 3) & 7; the second actor's hits through the first's next.
    let ten = state("10");
    assert_eq!((ten["pass_count"].as_i64(), xs(&ten)), (Some(10), vec![10, 30, 50, 70]), "{ten}");
    assert_eq!(ten["table"], serde_json::json!([8, 9, 10, 3, 4, 5, 6, 7]), "{ten}");
    assert_eq!((ten["current"].as_i64(), ten["second_hits"].as_i64()), (Some(6), Some(1)), "{ten}");
    // At pass 50 the third left the list: three, the last now third.
    let sixty = state("60");
    assert_eq!(xs(&sixty), vec![60, 130, 260], "{sixty}");
    assert_eq!(sixty["current"].as_i64(), Some(4), "(60 × 3) & 7: {sixty}");
    std::fs::remove_dir_all(&dir).ok();
}
