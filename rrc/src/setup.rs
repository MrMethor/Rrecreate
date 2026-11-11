//! Setting a port up: `rrc init`, `rrc game`, `rrc doctor`, `rrc status`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::args::Args;
use crate::cache::Cache;
use crate::entry::{self, write_new};
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
    ignore(&dir, &["/rrc.local.toml", "/.rrc/", "/target/", "/.claude/settings.local.json"])?;
    // The knowledge files are text with LF line ends on every platform (docs/formats.md).
    write_new(
        &dir.join(".gitattributes"),
        "* text=auto eol=lf
",
    )?;
    say("port", format!("{name} in {}", dir.display()));
    // What an agent meets first (D155): the person's rules to ask for, the agent's page, the notes, the session hook.
    entry::write(&dir, &name)?;
    entry::install_session_hook(&dir)?;
    // A port is a repository of its own (docs/design.md §3).
    if !dir.join(".git").exists() {
        match std::process::Command::new("git").arg("init").arg("-q").arg(&dir).status() {
            Ok(status) if status.success() => {
                say("git", "a new repository: commit rrc.toml, knowledge/ and sessions/");
                install_hook(&dir)?;
            }
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
    let hook =
        hook_path(&repo.root).filter(|path| std::fs::read_to_string(path).is_ok_and(|text| text.contains(HOOK_MARK)));
    say("hook", hook.map_or("no commit hook (rrc hooks writes it)".to_owned(), |p| p.display().to_string()));
    say(
        "rules",
        match entry::unanswered(&repo.root) {
            None => format!("no {} (rrc hooks writes it): the person's rules for this port", entry::RULES),
            Some(left) if left.is_empty() => entry::RULES.to_owned(),
            Some(left) => format!("{}: {} questions for the person not answered yet", entry::RULES, left.len()),
        },
    );
    check("ghidra", tools::ghidra(&repo.local).map(|d| d.display().to_string()), false);
    check("java", tools::java(&repo.local).map(|d| d.display().to_string()), false);
    Ok(if ok { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

/// What marks a commit hook as this tool's.
const HOOK_MARK: &str = "Written by rrc (D148)";

/// Where the repository's pre-commit hook goes, as git says.
fn hook_path(root: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git").arg("-C").arg(root).args(["rev-parse", "--git-path", "hooks"]).output();
    let out = out.ok().filter(|o| o.status.success())?;
    let hooks = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    Some(if hooks.is_absolute() { hooks } else { root.join(hooks) }.join("pre-commit"))
}

/// Writes the commit hook that refuses a commit while `rrc check` fails (D148), unless someone else's is there.
fn install_hook(root: &Path) -> Result<(), String> {
    let Some(path) = hook_path(root) else {
        say("hook", "not a git repository: no commit hook");
        return Ok(());
    };
    if std::fs::read_to_string(&path).is_ok_and(|text| !text.contains(HOOK_MARK)) {
        say("hook", format!("{} is someone else's: add rrc check to it", path.display()));
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let text = format!(
        "#!/bin/sh\n# {HOOK_MARK}: a commit is refused while rrc check fails.\nexec \"{}\" check\n",
        exe.display().to_string().replace('\\', "/")
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    say("hook", path.display());
    Ok(())
}

/// `rrc hooks`: the commit hook and the session hook written (again, as this rrc), and the entry files a port made
/// before them lacks (D155).
pub(crate) fn hooks(repo: &Repo) -> Result<ExitCode, String> {
    install_hook(&repo.root)?;
    entry::write(&repo.root, &repo.settings.game.name)?;
    entry::install_session_hook(&repo.root)?;
    Ok(ExitCode::SUCCESS)
}

/// The playbook, as this rrc was built with it (`docs/porting.md`).
const PLAYBOOK: &str = include_str!("../../docs/porting.md");
/// A port's own rules, from the person who runs it.
const PORT_RULES: &str = entry::RULES;

/// From the line starting `from` up to the line starting `to`.
fn between<'a>(text: &'a str, from: &str, to: &str) -> &'a str {
    let Some(start) = text.find(from) else { return "" };
    let rest = &text[start..];
    rest[from.len()..].find(to).map_or(rest, |end| rest[..from.len() + end].trim_end())
}

/// `rrc rules`: the rules as they're written, for an agent to read at the start of a session and after its context is
/// summarised (D148), so a summary can't soften them: the playbook's and the port's own.
pub(crate) fn rules(repo: Option<&Repo>) -> ExitCode {
    println!("{}\n", between(PLAYBOOK, "**Rules:**", "**Where to start"));
    println!("{}", between(PLAYBOOK, "## Quirks", "\n## "));
    if let Some(text) = repo.and_then(|repo| std::fs::read_to_string(repo.root.join(PORT_RULES)).ok()) {
        println!("\n{}", text.trim_end());
    }
    ExitCode::SUCCESS
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
    // What the rule still asks (D148), beside whatever is next.
    let open = crate::quirks::open(repo)?;
    if let Some(first) = open.first() {
        say("open", format!("{} rrc check refuses; the first: {first}", open.len()));
    }
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
/// (docs/porting.md, stages 1 to 3).
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
        let session = crate::session::Session::load(repo, name)?;
        let edits = crate::edits::listed(&crate::edits::resolve(&k, &session.edits)?);
        let Some(recorded) = session.trace else {
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
            Some(h) if h.edits != edits => {
                return Ok(format!("rrc record {name}: its edits changed since it was recorded"));
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
    fn the_rules_are_the_playbook_s_own_words() {
        let rules = super::between(super::PLAYBOOK, "**Rules:**", "**Where to start");
        assert!(rules.starts_with("**Rules:**") && rules.contains("Plan before running"), "{rules}");
        assert!(rules.contains("registered as it's found") && !rules.contains("Where to start"), "{rules}");
        let quirks = super::between(super::PLAYBOOK, "## Quirks", "\n## ");
        assert!(quirks.contains("rrc_runtime::keeps") && !quirks.contains("## Naming"), "{quirks}");
    }

    #[test]
    fn wildcards_match_names() {
        let m = |p: &str, t: &str| super::matches(p.as_bytes(), t.as_bytes());
        assert!(m("SAVEGAM*.WL1", "SAVEGAM0.WL1") && m("*.SAV", "A.SAV") && m("CONFIG.WL?", "CONFIG.WL1"));
        assert!(!m("*.SAV", "A.SAVE") && !m("SAVEGAM?.WL1", "SAVEGAM10.WL1") && m("*", ""));
    }
}
