//! `rrc`: Rrecreate's toolkit (DESIGN.md §9, §10). It runs inside a port repository, keeps its
//! knowledge files, and drives Clockwork through `clockwork serve`. Its output is read by agents:
//! a few lines, a label and a value each.

pub mod analysis;
mod args;
mod asset;
pub mod cache;
mod calls;
pub mod clockwork;
mod compare;
mod contracts;
pub mod edits;
mod files;
pub mod formats;
pub mod frames;
pub mod game;
pub mod knowledge;
pub mod mapping;
mod memory;
mod naming;
mod pass;
mod peek;
pub mod places;
pub mod quirks;
pub mod repo;
mod search;
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
  analyze [--session NAME] [--no-decompile] [--again]
                         the program, unpacked by Clockwork, analyzed by Ghidra into the cache (an
                         analysis of the same program on this machine is reused unless --again);
                         library functions the signature packs know get their names; functions the
                         knowledge names that Ghidra didn't find are added, and with --session,
                         those the session's run called (reached through vectors and pointers)
  check                  the knowledge files: IDs, names, sizes, ticks, types (and against the image)
  name ID NAME           name a function (--evidence TEXT, --confidence 0-1, --by WHO: who decided,
                         else RRC_BY, else agent)
  data ID NAME [--size N] [--type T]
                         name data (the same options as name)
  forget ID|NAME         take a function's or data's name back out of the knowledge
  sync [ID NAME [--when-changed A,B] [--note TEXT]]
                         the sync points, or one (where a tick ends; with --when-changed, a loop that
                         polls ticks when these inputs or data changed); --remove NAME takes one out
  input [ID|mouse|joystick NAME [--size N] [--sync A,B] [--note TEXT]]
                         the boundary values, or one (memory, or a device's state; --sync: read only
                         at those sync points); --remove NAME; both take name's options
  hook [ID NAME --part TEXT --registers A,B]
                         the calls a part not ported makes into ported code, or one: each run of the
                         entry ID in a pass with those registers, given the port (D98); --remove NAME
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
  peek SESSION --tick N ID|NAME|SEG:OFF... [--len N]
                         memory at the end of tick N (its sync point), named or not, without
                         recording: as its type reads it, else as a number or bytes (--len: 2, or
                         the item's size)
  pass SESSION --tick N [--depth N] [--in ID|NAME] [--interrupts] [--lines N]
                         what tick N's pass runs, in order: the calls from tick N-1's sync point to
                         tick N's as a tree of named functions, --depth levels (2), a call repeated
                         in a row once with a count; --in: under each call of that function
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
  compare SESSION [--transplant TICK | --resume TICK] [--to TICK] [--pair TICK] [--feed FIELD,...]
                         run the port ([port] command in rrc.toml) with the session's inputs and compare
                         its state with the original's trace at every tick, field by field as
                         mapping.toml maps them: the first tick that differs, the fields and values, and
                         who wrote the original's value; from the port's start state, or the original's
                         mapped values at a tick (--transplant), or a paired checkpoint (--resume);
                         --pair keeps one at the end of tick N (the port's state and Clockwork's
                         snapshot) if the port matched to there; --feed gives the port the original's
                         values of those fields (or groups) before every step, for a subsystem not
                         ported yet. Exits 1 if it diverges
  files SESSION [--file NAME] [--lines N]
                         the files the original reads and writes in the session: by file, how often,
                         how much, by which code; with --file, each read or write: where in the file,
                         how many bytes, where in memory, by which code, when
  asset FILE[/MEMBER] [--session NAME] [--out FILE.png] [--lines N]
                         a file of the game or an archive's member, decoded by its format
                         (knowledge/formats.toml): an archive's members, a picture as PNG with its
                         palette, the bytes kept in the cache; with --session, where the original read
                         them and whether its memory holds them decoded after that
  search SESSION CONDITION... [--type T] [--step N] [--at ID --len N] [--lines N]
                         the places in the data segment (LE: the data objects; or the range given)
                         whose values, read as T (u16 unless --type says), meet every condition over
                         the session's ticks: changed:A-B, same:A-B (at every tick between), more:A-B,
                         less:A-B, is:T=V; --step N: every N bytes (1)
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
        Some("forget") => here().and_then(|repo| naming::forget(&repo, rest)),
        Some("sync") => here().and_then(|repo| ticks::sync(&repo, rest)),
        Some("input") => here().and_then(|repo| ticks::input(&repo, rest)),
        Some("hook") => here().and_then(|repo| ticks::hook(&repo, rest)),
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
        Some("search") => here().and_then(|repo| search::search(&repo, rest)),
        Some("files") => here().and_then(|repo| files::files(&repo, rest)),
        Some("asset") => here().and_then(|repo| asset::asset(&repo, rest)),
        Some("pass") => here().and_then(|repo| pass::pass(&repo, rest)),
        Some("peek") => here().and_then(|repo| peek::peek(&repo, rest)),
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
