//! `rrc`: Rrecreate's toolkit (DESIGN.md §9, §10). It runs inside a port repository, keeps its
//! knowledge files, and drives Clockwork through `clockwork serve`. Its output is read by agents:
//! a few lines, a label and a value each.

pub mod analysis;
mod args;
pub mod cache;
mod calls;
pub mod clockwork;
mod compare;
pub mod edits;
pub mod frames;
pub mod game;
pub mod knowledge;
pub mod mapping;
mod naming;
pub mod places;
pub mod quirks;
pub mod repo;
pub mod session;
mod setup;
mod sigs;
pub mod snapshots;
mod sources;
mod ticks;
pub mod tools;
pub mod trace;
pub mod values;
mod watch;

use std::process::ExitCode;

use crate::repo::Repo;

const USAGE: &str = "\
usage: rrc <command> [arguments]     (inside a port repository)

  init [DIR] --program PROGRAM [--name NAME] [--title TEXT]
                         make a port repository in DIR (default: here; a git repository too) for the
                         game whose program is PROGRAM (as `clockwork run` takes it, in the game's
                         folder)
  doctor                 what is there and what is missing: Clockwork, the game, the cache, Ghidra
  status                 the next step
  game [FOLDER] [--add-version NAME [--without FILE,...]]
                         the person's copy of the game (FOLDER is kept in rrc.local.toml) and which
                         version it is; --add-version records its files as a version the port supports
                         (--without: but these, with * and ? as wildcards: saved games, a setup's
                         configuration)
  analyze [--no-decompile] [--again]
                         the program, unpacked by Clockwork, analyzed by Ghidra into the cache (an
                         analysis of the same program on this machine is reused unless --again);
                         library functions the signature packs know get their names; functions the
                         knowledge names that Ghidra didn't find are added
  check                  the knowledge files: IDs, names, sizes, ticks, types (and against the image)
  name ID NAME           name a function (--evidence TEXT, --confidence 0-1, --by WHO: who decided,
                         else RRC_BY, else agent)
  data ID NAME [--size N] [--type T]
                         name data (the same options as name)
  sync [ID NAME [--when-changed A,B] [--note TEXT]]
                         the sync points, or one (where a tick ends; with --when-changed, a loop that
                         polls ticks when these inputs or data changed); --remove NAME takes one out
  input [ID|mouse|joystick NAME [--size N] [--sync A,B] [--note TEXT]]
                         the boundary values, or one (memory, or a device's state; --sync: read only
                         at those sync points); --remove NAME; both take name's options
  show ID|NAME|SEG:OFF [--asm] [--c] [--listing] [--all] [--session NAME [--tick N]]
                         what is known about a function or data (SEG:OFF: Ghidra's address, as in
                         its DAT_SEG_OFF names): its name and evidence, what Ghidra found (size,
                         calls, callers; the function an address is inside; who reads and writes
                         data), its disassembly (--asm, lines and targets as IDs, each instruction's
                         data named; Clockwork's for code Ghidra has no function for), Clockwork's
                         disassembly following jumps and jump tables (--listing), or decompiled
                         code (--c), with the knowledge's names for Ghidra's FUN_ and DAT_ names;
                         --all: every function that refers to data, not the first ten; --session:
                         code made at run time, from the session's memory (by tick N, or its last
                         snapshot)
  session [add NAME --seconds N (--inputs FILE | --replay FILE) [--description TEXT]
          [--clockwork OPTIONS]]
                         the sessions, or a new one: an input script (clockwork run --input-file) or
                         play recorded by clockwork play --record, and how many emulated seconds;
                         --clockwork: more `clockwork run` options for it (\"--joystick 1\")
  session set NAME [--seconds N] [--description TEXT] [--clockwork OPTIONS]
                         change a session
  session edit NAME TICK AT (VALUE [--type T] [--note TEXT] | --remove)
                         a scenario's edit: at tick TICK's sync point, write VALUE (JSON: a number, an
                         array, {\"field\": value, ...} for some of a structure's fields; or an
                         enumeration's value name) at AT, named data or an ID with a path into it as its
                         type reads (enemies[2].alive); every run of the session makes it
  record SESSION         run the session through Clockwork and trace it: at each tick (a sync point
                         hit) the boundary values, the named data that changed, frames and sound;
                         keeps snapshots for rrc watch and rrc inputs to start from
  calls SESSION [ID|NAME] [--min N] [--top N]
                         how often the session calls each function, grouped by count (at least N,
                         default 100), in phases (before the first tick, each run of ticks at one
                         sync point): a loop calls its functions equally often, and the one in a
                         group that waits for the timer is where a tick ends; with a function, how
                         often each phase calls it
  watch SESSION ID|NAME|SEG:OFF [--len N] [--tick N] [--ticks N] [--reads] [--cycles]
        [--limit N] [--lines N]
                         who writes (and with --reads, reads) it during the session, or from tick N
                         for N ticks: the instruction, the value, and the call chain (start: before
                         the first sync point hit); at most --limit accesses (10000), --lines lines
  inputs SESSION [--tick N] [--ticks N | --from-ms MS [--to-ms MS]] [--at ID|SEG:OFF --len N]
                         boundary value candidates: memory the program reads that only interrupt
                         handlers or host services write (the key states, the timer counts), in the
                         data segment from tick N (default 2) on, or from MS ms (a loop with no sync
                         point yet), or in the range given, by the instruction that wrote it; and
                         the mouse and joystick calls it can't see
  ports SESSION [PORT,...] [--tick N] [--ticks N]
                         the instructions that read and write each port (hex), or those given: the
                         display's retrace (3DA), the sound card, the joystick (201)
  frame SESSION --tick N [--out FILE]
                         the picture the original showed by the end of tick N, as a PNG (in
                         .rrc/frames/ unless --out says where)
  trace SESSION [--tick N] [--item NAME [--lines N]] [--path]
                         a recorded trace: a summary, one tick, or one item over the ticks, values
                         read as data.tsv's types (an array as its elements that aren't zero);
                         --path: where the trace file is (the path alone), to read it whole
  compare SESSION [--transplant TICK | --resume TICK] [--to TICK] [--pair TICK]
                         run the port ([port] command in rrc.toml) with the session's inputs and compare
                         its state with the original's trace at every tick, field by field as
                         mapping.toml maps them: the first tick that differs, the fields and values, and
                         who wrote the original's value; from the port's start state, or the original's
                         mapped values at a tick (--transplant), or a paired checkpoint (--resume);
                         --pair keeps one at the end of tick N (the port's state and Clockwork's
                         snapshot) if the port matched to there. Exits 1 if it diverges
  quirk [NAME (--at ID --what TEXT [--fix TEXT] [--evidence TEXT] [--by WHO] | --remove)]
                         the quirk register (quirks.toml): the original's bugs the port keeps, each
                         with the port's code that names it; or one added, changed or removed
  suspects SESSION [--lines N]
                         quirk candidates in the session's run: the original's instructions that
                         reach past named data's ends, and that read memory nothing wrote
  sigs build MAP --name NAME --compiler TEXT --source TEXT [--out FILE]
                         a library signature pack (sigs/README.md) from this analysis and the linker
                         map of the same build: the library functions the map names, by their
                         Function ID hashes (NAME.json unless --out says where)
  sigs name MAP          name the map's functions in the knowledge (provenance map), so that rrc
                         analyze adds those Ghidra didn't find: for a port made to build a pack

Files: rrc.toml (the port), rrc.local.toml (this machine), knowledge/, sessions/, mapping.toml,
quirks.toml (docs/formats.md).
Environment: RRC_CLOCKWORK (the clockwork program), RRC_CACHE (the cache folder), GHIDRA_INSTALL_DIR and
JAVA_HOME (for analysis).";

/// The command line.
pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or_default();
    let result = match args.first().map(String::as_str) {
        None | Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Some("--version") => {
            println!("rrc {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        Some("init") => setup::init(rest),
        Some("doctor") => here().and_then(|repo| setup::doctor(&repo)),
        Some("status") => here().and_then(|repo| setup::status(&repo)),
        Some("game") => here().and_then(|mut repo| setup::game(&mut repo, rest)),
        Some("analyze") => here().and_then(|repo| analysis::analyze(&repo, rest)),
        Some("check") => here().and_then(|repo| naming::check(&repo)),
        Some("name") => here().and_then(|repo| naming::name(&repo, rest)),
        Some("data") => here().and_then(|repo| naming::data(&repo, rest)),
        Some("sync") => here().and_then(|repo| ticks::sync(&repo, rest)),
        Some("input") => here().and_then(|repo| ticks::input(&repo, rest)),
        Some("show") => here().and_then(|repo| naming::show(&repo, rest)),
        Some("session") => here().and_then(|repo| session::session(&repo, rest)),
        Some("calls") => here().and_then(|repo| calls::calls(&repo, rest)),
        Some("record") => here().and_then(|repo| trace::record(&repo, rest)),
        Some("watch") => here().and_then(|repo| watch::watch(&repo, rest)),
        Some("inputs") => here().and_then(|repo| watch::inputs(&repo, rest)),
        Some("ports") => here().and_then(|repo| watch::ports(&repo, rest)),
        Some("trace") => here().and_then(|repo| trace::show(&repo, rest)),
        Some("frame") => here().and_then(|repo| frames::frame(&repo, rest)),
        Some("compare") => here().and_then(|repo| compare::compare(&repo, rest)),
        Some("quirk") => here().and_then(|repo| quirks::quirk(&repo, rest)),
        Some("suspects") => here().and_then(|repo| quirks::suspects(&repo, rest)),
        Some("sigs") => here().and_then(|repo| sigs::sigs(&repo, rest)),
        Some(other) => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    };
    result.unwrap_or_else(|message| {
        eprintln!("rrc: error: {message}");
        ExitCode::from(1)
    })
}

/// The port repository the current folder is in.
fn here() -> Result<Repo, String> {
    let dir = std::env::current_dir().map_err(|e| e.to_string())?;
    Repo::find(&dir)
}

/// A path as people and other programs read it: without Windows' `\\?\` prefix for long paths.
pub(crate) fn path_text(path: &std::path::Path) -> String {
    let text = path.display().to_string();
    match text.strip_prefix(r"\\?\") {
        Some(rest) => rest.strip_prefix("UNC\\").map_or_else(|| rest.to_owned(), |unc| format!(r"\\{unc}")),
        None => text,
    }
}

/// One line of output: a label and what it says.
fn say(label: &str, text: impl std::fmt::Display) {
    let line = format!("{label:<10}{text}");
    snapshots::captured(&line);
    println!("{line}");
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_long_path_shows_without_its_prefix() {
        let text = |s: &str| super::path_text(std::path::Path::new(s));
        assert_eq!(text(r"\\?\C:\port\.rrc\frames\a-1000.png"), r"C:\port\.rrc\frames\a-1000.png");
        assert_eq!(text(r"\\?\UNC\server\share\a.png"), r"\\server\share\a.png");
        assert_eq!(text("port/a.png"), "port/a.png");
    }
}
