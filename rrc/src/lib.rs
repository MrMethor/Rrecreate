//! `rrc`: Rrecreate's toolkit (DESIGN.md §9, §10). It runs inside a port repository, keeps its
//! knowledge files, and drives Clockwork through `clockwork serve`. Its output is read by agents:
//! a few lines, a label and a value each.

pub mod analysis;
mod args;
pub mod cache;
mod calls;
pub mod clockwork;
pub mod game;
pub mod import;
pub mod knowledge;
mod naming;
pub mod repo;
pub mod session;
mod setup;
pub mod tools;
pub mod trace;
mod watch;

use std::process::ExitCode;

use crate::repo::Repo;

const USAGE: &str = "\
usage: rrc <command> [arguments]     (inside a port repository)

  init [DIR] --program PROGRAM [--name NAME] [--title TEXT]
                         make a port repository in DIR (default: here) for the game whose program
                         is PROGRAM (as `clockwork run` takes it, in the game's folder)
  doctor                 what is there and what is missing: Clockwork, the game, the cache, Ghidra
  status                 the next step
  game [FOLDER] [--add-version NAME]
                         the person's copy of the game (FOLDER is kept in rrc.local.toml) and which
                         version it is; --add-version records its files as a version the port supports
  analyze [--no-decompile] [--again]
                         the program, unpacked by Clockwork, analyzed by Ghidra into the cache (an
                         analysis of the same program on this machine is reused unless --again);
                         library functions the signature packs know get their names
  check                  the knowledge files: IDs, names, sizes, ticks, types (and against the image)
  name ID NAME           name a function (--evidence TEXT, --confidence 0-1, --by WHO: who decided,
                         else RRC_BY, else agent)
  data ID NAME [--size N] [--type T]
                         name data (the same options as name)
  show ID|NAME|SEG:OFF [--asm] [--c]
                         what is known about a function or data (SEG:OFF: Ghidra's address, as in
                         its DAT_SEG_OFF names): its name and evidence, what Ghidra found (size,
                         calls, callers), its disassembly (--asm) or decompiled code (--c), with
                         the knowledge's names for Ghidra's FUN_ and DAT_ names
  session [add NAME --seconds N (--inputs FILE | --replay FILE) [--description TEXT]]
                         the sessions, or a new one: an input script (clockwork run --input-file) or
                         play recorded by clockwork play --record, and how many emulated seconds
  record SESSION         run the session through Clockwork and trace it: at each tick (a sync point
                         hit) the boundary values, the named data that changed, frames and sound
  calls SESSION [--min N]
                         how often the session calls each function, grouped by count (at least N,
                         default 100): a loop calls its functions equally often, and the one in a
                         group that waits for the timer is where a tick ends
  watch SESSION ID|NAME|SEG:OFF [--len N] [--tick N] [--ticks N] [--reads]
                         who writes (and with --reads, reads) it during the session, or from tick N
                         for N ticks: the instruction, the value, and the call chain
  inputs SESSION [--tick N] [--ticks N] [--at ID|SEG:OFF --len N]
                         boundary value candidates: memory the program reads that only interrupt
                         handlers or host services write (the key states, the timer counts), in the
                         data segment from tick N (default 2) on, or in the range given
  trace SESSION [--tick N] [--item NAME]
                         a recorded trace: a summary, one tick, or one item over the ticks
  import recreate DIR [--without PROVENANCE,...]
                         Recreate's knowledge files (its games/<game>/knowledge), leaving out the
                         items of the provenances named

Files: rrc.toml (the port), rrc.local.toml (this machine), knowledge/, sessions/ (docs/formats.md).
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
        Some("show") => here().and_then(|repo| naming::show(&repo, rest)),
        Some("session") => here().and_then(|repo| session::session(&repo, rest)),
        Some("calls") => here().and_then(|repo| calls::calls(&repo, rest)),
        Some("record") => here().and_then(|repo| trace::record(&repo, rest)),
        Some("watch") => here().and_then(|repo| watch::watch(&repo, rest)),
        Some("inputs") => here().and_then(|repo| watch::inputs(&repo, rest)),
        Some("trace") => here().and_then(|repo| trace::show(&repo, rest)),
        Some("import") => here().and_then(|repo| import::import(&repo, rest)),
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

/// One line of output: a label and what it says.
fn say(label: &str, text: impl std::fmt::Display) {
    println!("{label:<10}{text}");
}
