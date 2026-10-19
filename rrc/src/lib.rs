//! `rrc`: Rrecreate's toolkit (DESIGN.md §9, §10). It runs inside a port repository, keeps its
//! knowledge files, and drives Clockwork through `clockwork serve`. Its output is read by agents:
//! a few lines, a label and a value each.

mod args;
pub mod cache;
pub mod clockwork;
pub mod game;
pub mod knowledge;
pub mod repo;
pub mod tools;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::args::Args;
use crate::cache::Cache;
use crate::repo::{Game, Original, Repo, Settings, Version};

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
        Some("init") => init(rest),
        Some("doctor") => here().and_then(|repo| doctor(&repo)),
        Some("status") => here().and_then(|repo| status(&repo)),
        Some("game") => here().and_then(|mut repo| game(&mut repo, rest)),
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

fn init(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--program", "--name", "--title"], &[])?;
    let usage = "init [DIR] --program PROGRAM [--name NAME] [--title TEXT]";
    let dir = PathBuf::from(a.positionals(0, 1, usage)?.first().map_or(".", String::as_str));
    if dir.join(repo::SETTINGS).exists() {
        return Err(format!("{} is a port repository already", dir.display()));
    }
    let program = a.required("--program")?.to_owned();
    for sub in ["knowledge", "sessions"] {
        std::fs::create_dir_all(dir.join(sub)).map_err(|e| format!("{}: {e}", dir.join(sub).display()))?;
    }
    let dir = std::path::absolute(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = match a.value("--name") {
        Some(name) => name.to_owned(),
        None => dir.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or("name the port: --name NAME")?,
    };
    let title = a.value("--title").unwrap_or_default().to_owned();
    let settings = Settings {
        game: Game { name: name.clone(), title },
        original: Original { program, options: vec![] },
        versions: vec![],
    };
    repo::write_toml(&dir.join(repo::SETTINGS), &settings)?;
    let knowledge = dir.join("knowledge");
    let files = [
        (knowledge::FUNCTIONS, format!("{}\n", knowledge::FUNCTIONS_HEADER)),
        (knowledge::DATA, format!("{}\n", knowledge::DATA_HEADER)),
        (knowledge::TYPES, "# Structures and enumerations (docs/formats.md).\n".to_owned()),
        (knowledge::TICKS, "# Sync points and boundary values (docs/formats.md).\n".to_owned()),
    ];
    for (file, text) in files {
        write_new(&knowledge.join(file), &text)?;
    }
    ignore(&dir, &["/rrc.local.toml", "/.rrc/", "/target/"])?;
    say("port", format!("{name} in {}", dir.display()));
    say("next", "rrc game <the game's folder>");
    Ok(ExitCode::SUCCESS)
}

/// Writes `text` to `path` unless the file is there already.
fn write_new(path: &Path, text: &str) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Adds the lines `.gitignore` lacks.
fn ignore(dir: &Path, lines: &[&str]) -> Result<(), String> {
    let path = dir.join(".gitignore");
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    for line in lines {
        if !text.lines().any(|l| l.trim() == *line) {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(line);
            text.push('\n');
        }
    }
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn doctor(repo: &Repo) -> Result<ExitCode, String> {
    let mut ok = true;
    let mut check = |label: &str, result: Result<String, String>, required: bool| match result {
        Ok(text) => say(label, text),
        Err(text) => {
            ok &= !required;
            say(label, if required { text } else { format!("{text} (needed by rrc analyze)") });
        }
    };
    check("repo", Ok(format!("{} ({})", repo.root.display(), repo.settings.game.name)), true);
    let clockwork = clockwork::find(&repo.local).map(|exe| {
        let version = clockwork::version(&exe).unwrap_or_else(|| "?".into());
        format!("{version} ({})", exe.display())
    });
    check("clockwork", clockwork, true);
    check("game", which_version(repo).map(|(dir, version)| format!("{}: {version}", dir.display())), true);
    check("cache", Cache::open(&repo.local).map(|c| c.dir().display().to_string()), true);
    check("ghidra", tools::ghidra(&repo.local).map(|d| d.display().to_string()), false);
    check("java", tools::java(&repo.local).map(|d| d.display().to_string()), false);
    Ok(if ok { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

/// The game's folder and which version it is (an error if none the port knows).
fn which_version(repo: &Repo) -> Result<(PathBuf, String), String> {
    let dir = repo.game_dir()?.to_path_buf();
    let files = game::hash_folder(&dir, &repo.state()?)?;
    if repo.settings.versions.is_empty() {
        return Err(format!("{}: no versions in {} yet (rrc game --add-version NAME)", dir.display(), repo::SETTINGS));
    }
    match game::identify(&repo.settings.versions, &files) {
        Some(version) => Ok((dir, version.name.clone())),
        None => Err(format!("{}: not a version the port knows (rrc game says how it differs)", dir.display())),
    }
}

fn status(repo: &Repo) -> Result<ExitCode, String> {
    let next = match which_version(repo) {
        Err(_) if repo.game_dir().is_err() => "rrc game <the game's folder>".to_owned(),
        Err(_) if repo.settings.versions.is_empty() => {
            "rrc game --add-version <name>: record this copy's files as a version".to_owned()
        }
        Err(e) => e,
        Ok(_) => "the toolkit's next commands are still being built (M2)".to_owned(),
    };
    say("next", next);
    Ok(ExitCode::SUCCESS)
}

fn game(repo: &mut Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--add-version"], &[])?;
    if let Some(folder) = a.positionals(0, 1, "game [FOLDER] [--add-version NAME]")?.first() {
        let dir = std::path::absolute(folder).map_err(|e| format!("{folder}: {e}"))?;
        if !dir.is_dir() {
            return Err(format!("{} isn't a folder", dir.display()));
        }
        repo.local.game = Some(dir);
        repo.save_local()?;
    }
    let dir = repo.game_dir()?.to_path_buf();
    let files = game::hash_folder(&dir, &repo.state()?)?;
    let program = repo.settings.original.program.replace('\\', "/");
    if !files.keys().any(|path| path.eq_ignore_ascii_case(&program)) {
        return Err(format!("{}: no {program} there", dir.display()));
    }
    if let Some(name) = a.value("--add-version") {
        if repo.settings.versions.iter().any(|v| v.name == name) {
            return Err(format!("there is a version {name:?} already"));
        }
        repo.settings.versions.push(Version { name: name.to_owned(), files: files.clone() });
        repo.save_settings()?;
    }
    say("game", format!("{} ({} files)", dir.display(), files.len()));
    match game::identify(&repo.settings.versions, &files) {
        Some(version) => say("version", &version.name),
        None if repo.settings.versions.is_empty() => say("version", "none yet: rrc game --add-version NAME"),
        None => {
            for version in &repo.settings.versions {
                let differ = game::differences(version, &files);
                say("version", format!("not {}: {} of its files differ or are missing", version.name, differ.len()));
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}
