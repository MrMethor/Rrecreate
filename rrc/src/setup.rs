//! Setting a port up: `rrc init`, `rrc game`, `rrc doctor`, `rrc status`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::args::Args;
use crate::cache::Cache;
use crate::repo::{self, Game, Original, Port, Repo, Settings, Version};
use crate::{clockwork, game, knowledge, say, tools};

pub(crate) fn init(args: &[String]) -> Result<ExitCode, String> {
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
        port: Port::default(),
        original: Original { program, options: vec![], module: None },
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
    // The knowledge files are text with LF line ends on every platform (docs/formats.md).
    write_new(
        &dir.join(".gitattributes"),
        "* text=auto eol=lf
",
    )?;
    say("port", format!("{name} in {}", dir.display()));
    // A port is a repository of its own (DESIGN.md §3).
    if !dir.join(".git").exists() {
        match std::process::Command::new("git").arg("init").arg("-q").arg(&dir).status() {
            Ok(status) if status.success() => say("git", "a new repository: commit rrc.toml, knowledge/ and sessions/"),
            _ => say("git", "not found: make the port a git repository yourself (git init)"),
        }
    }
    say("next", "rrc game <the game's folder>");
    Ok(ExitCode::SUCCESS)
}

/// Whether `text` matches `pattern`, where `*` is any run of characters and `?` any one.
fn matches(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.first(), text.first()) {
        (None, None) => true,
        (Some(b'*'), _) => matches(&pattern[1..], text) || (!text.is_empty() && matches(pattern, &text[1..])),
        (Some(&p), Some(&c)) if p == b'?' || p == c => matches(&pattern[1..], &text[1..]),
        _ => false,
    }
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

pub(crate) fn doctor(repo: &Repo) -> Result<ExitCode, String> {
    let mut ok = true;
    let mut check = |label: &str, result: Result<String, String>, required: bool| match result {
        Ok(text) => say(label, text),
        Err(text) => {
            ok &= !required;
            say(label, if required { text } else { format!("{text} (needed by rrc analyze)") });
        }
    };
    check("repo", Ok(format!("{} ({})", repo.root.display(), repo.settings.game.name)), true);
    // Started, it says what it is; rrc refuses one it can't drive.
    let clockwork = clockwork::find(&repo.local).and_then(|exe| {
        let hello = clockwork::Clockwork::start(&exe)?.hello()?;
        let text = |key: &str| hello[key].as_str().unwrap_or("?").to_owned();
        let protocol = hello["protocol"].as_u64().unwrap_or(0);
        let (emulator, version, platform) = (text("emulator"), text("version"), text("platform"));
        Ok(format!("{emulator} {version}, protocol {protocol}, {platform} ({})", exe.display()))
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

pub(crate) fn status(repo: &Repo) -> Result<ExitCode, String> {
    let next = match which_version(repo) {
        Err(_) if repo.game_dir().is_err() => "rrc game <the game's folder>".to_owned(),
        Err(_) if repo.settings.versions.is_empty() => {
            "rrc game --add-version <name>: record this copy's files as a version".to_owned()
        }
        Err(e) => e,
        Ok(_) => next_after_setup(repo)?,
    };
    say("next", next);
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn game(repo: &mut Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--add-version", "--without"], &[])?;
    if let Some(folder) = a.positionals(0, 1, "game [FOLDER] [--add-version NAME [--without FILE,...]]")?.first() {
        let dir = std::path::absolute(folder).map_err(|e| format!("{folder}: {e}"))?;
        if !dir.is_dir() {
            return Err(format!("{} isn't a folder", dir.display()));
        }
        repo.local.game = Some(dir);
        repo.save_local()?;
    }
    let dir = repo.game_dir()?.to_path_buf();
    let files = game::hash_folder(&dir, &repo.state()?)?;
    for program in [repo.program_file(), repo.module_file()] {
        if !files.keys().any(|path| path.eq_ignore_ascii_case(&program)) {
            return Err(format!("{}: no {program} there", dir.display()));
        }
    }
    if let Some(name) = a.value("--add-version") {
        if repo.settings.versions.iter().any(|v| v.name == name) {
            return Err(format!("there is a version {name:?} already"));
        }
        // What isn't the game: saved games, a setup's configuration (names or paths, with * and ?; any
        // case).
        let without: Vec<String> = a
            .value("--without")
            .into_iter()
            .flat_map(|w| w.split(','))
            .map(|w| w.trim().to_ascii_uppercase())
            .filter(|w| !w.is_empty())
            .collect();
        let left_out = |path: &str| {
            let path = path.to_ascii_uppercase();
            let file = path.rsplit('/').next().unwrap_or(&path);
            without.iter().any(|w| matches(w.as_bytes(), path.as_bytes()) || matches(w.as_bytes(), file.as_bytes()))
        };
        let kept: std::collections::BTreeMap<String, String> =
            files.iter().filter(|(path, _)| !left_out(path)).map(|(p, h)| (p.clone(), h.clone())).collect();
        if !without.is_empty() {
            say("without", format!("{} files left out", files.len() - kept.len()));
        }
        repo.settings.versions.push(Version { name: name.to_owned(), files: kept });
        repo.save_settings()?;
    } else if a.value("--without").is_some() {
        return Err("--without goes with --add-version".into());
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

/// The next step once the game is there: analysis, sessions, ticks and their inputs, traces
/// (docs/porting.md, steps 2 to 4).
fn next_after_setup(repo: &Repo) -> Result<String, String> {
    if crate::analysis::Index::load(repo)?.is_none() {
        return Ok("rrc analyze".into());
    }
    let k = knowledge::Knowledge::load(&repo.knowledge())?;
    let sessions = crate::session::names(repo)?;
    let Some(first) = sessions.first() else {
        return Ok("rrc session add NAME --seconds N --inputs FILE: a session to reproduce, into play".into());
    };
    if k.ticks.syncs.is_empty() {
        return Ok(format!("find where a tick ends: rrc calls {first} lists the loops; then rrc sync ID NAME"));
    }
    if k.ticks.inputs.is_empty() {
        return Ok(format!("find what a tick reads from outside: rrc inputs {first}; then rrc input ID NAME"));
    }
    let cache = crate::cache::Cache::open(&repo.local)?;
    let (syncs, inputs, state) = crate::trace::items(repo, &k, &repo.module())?;
    for name in &sessions {
        let Some(recorded) = crate::session::Session::load(repo, name)?.trace else {
            return Ok(format!("rrc record {name}"));
        };
        match crate::trace::recorded_header(&cache, &recorded.hash) {
            None => return Ok(format!("rrc record {name}: its trace isn't in this machine's cache")),
            Some(h) if h.version != crate::trace::VERSION => {
                return Ok(format!("rrc record {name}: its trace is in an older format"));
            }
            Some(h) if (&h.syncs, &h.inputs, &h.state) != (&syncs, &inputs, &state) => {
                return Ok(format!("rrc record {name}: the knowledge changed since it was recorded"));
            }
            Some(_) => {}
        }
    }
    let mapped = crate::mapping::Mapping::load(repo)?.resolve(&k)?;
    if mapped.is_empty() {
        return Ok(format!(
            "every session is recorded; map the state: {} names the port's fields and the original's items",
            crate::mapping::MAPPING
        ));
    }
    if repo.settings.port.command.is_empty() {
        return Ok("rrc.toml: [port] command, the port's program (docs/formats.md)".into());
    }
    Ok(format!("rrc compare {first}: the port against the original, tick by tick"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn wildcards_match_names() {
        let m = |p: &str, t: &str| super::matches(p.as_bytes(), t.as_bytes());
        assert!(m("SAVEGAM*.WL1", "SAVEGAM0.WL1") && m("*.SAV", "A.SAV") && m("CONFIG.WL?", "CONFIG.WL1"));
        assert!(!m("*.SAV", "A.SAVE") && !m("SAVEGAM?.WL1", "SAVEGAM10.WL1") && m("*", ""));
    }
}
