//! Sessions (DESIGN.md §7): what to reproduce. A session is a TOML file in `sessions/` naming its input
//! (an input script, or play recorded by `clockwork play --record`, kept beside it) and its length;
//! `rrc record` adds the hashes of its trace and its run (D44, D54).

use std::path::PathBuf;
use std::process::ExitCode;

use crate::args::Args;
use crate::repo::{Repo, read_toml, write_toml};
use crate::say;

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Session {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// An input script (`clockwork run --input-file`), in `sessions/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<String>,
    /// Recorded play (`clockwork run --replay`), in `sessions/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<String>,
    /// Emulated seconds it runs.
    pub seconds: f64,
    /// More `clockwork run` options for this session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<Recorded>,
}

/// The session's recorded trace.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Recorded {
    /// The trace, in the cache.
    pub hash: String,
    /// The run's hash: what the machine did, whatever the knowledge read (D54).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub run: String,
    pub ticks: u64,
    /// The Clockwork that recorded it.
    pub clockwork: String,
}

impl Session {
    pub fn path(repo: &Repo, name: &str) -> PathBuf {
        repo.sessions().join(format!("{name}.toml"))
    }

    pub fn load(repo: &Repo, name: &str) -> Result<Session, String> {
        let path = Session::path(repo, name);
        if !path.is_file() {
            return Err(format!("no session {name:?} (rrc session lists them)"));
        }
        read_toml(&path)
    }

    pub fn save(&self, repo: &Repo, name: &str) -> Result<(), String> {
        write_toml(&Session::path(repo, name), self)
    }

    /// The `clockwork run` options that play it.
    pub fn options(&self, repo: &Repo) -> Vec<String> {
        let file = |name: &str| repo.sessions().join(name).to_string_lossy().into_owned();
        let mut options = self.options.clone();
        if let Some(inputs) = &self.inputs {
            options.extend(["--input-file".to_owned(), file(inputs)]);
        }
        if let Some(replay) = &self.replay {
            options.extend(["--replay".to_owned(), file(replay)]);
        }
        options
    }
}

/// The sessions' names, sorted.
pub fn names(repo: &Repo) -> Result<Vec<String>, String> {
    let dir = repo.sessions();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".toml").map(str::to_owned))
        .collect();
    names.sort();
    Ok(names)
}

pub(crate) fn session(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--inputs", "--replay", "--seconds", "--description", "--clockwork"], &[])?;
    let usage = "session [add NAME --seconds N (--inputs FILE | --replay FILE) [--description TEXT] \
                 [--clockwork OPTIONS] | set NAME [--seconds N] [--description TEXT] [--clockwork OPTIONS]]";
    match a.positionals(0, 2, usage)? {
        [] => {
            for name in names(repo)? {
                let s = Session::load(repo, &name)?;
                let trace = s.trace.map_or("not recorded".to_owned(), |t| format!("{} ticks", t.ticks));
                say("session", format!("{name}: {} s, {trace}{}", s.seconds, note(&s.description)));
            }
            Ok(ExitCode::SUCCESS)
        }
        [add, name] if add == "add" => add_session(repo, &a, name),
        [set, name] if set == "set" => {
            let mut s = Session::load(repo, name)?;
            if let Some(seconds) = a.value("--seconds") {
                s.seconds = seconds.parse().ok().filter(|s: &f64| *s > 0.0).ok_or("--seconds is a number")?;
            }
            if let Some(description) = a.value("--description") {
                s.description = description.to_owned();
            }
            if let Some(options) = a.value("--clockwork") {
                s.options = clockwork_options(options);
            }
            s.save(repo, name)?;
            say("session", format!("{name}: {} s (rrc record {name} to trace it so)", s.seconds));
            Ok(ExitCode::SUCCESS)
        }
        _ => Err(format!("usage: rrc {usage}")),
    }
}

fn add_session(repo: &Repo, a: &Args, name: &str) -> Result<ExitCode, String> {
    if !crate::knowledge::is_name(&name.replace('-', "_")) {
        return Err(format!("{name:?}: a session's name is letters, digits, - and _"));
    }
    if Session::path(repo, name).exists() {
        return Err(format!("there is a session {name} already"));
    }
    let seconds: f64 =
        a.required("--seconds")?.parse().ok().filter(|s: &f64| *s > 0.0).ok_or("--seconds is a number")?;
    std::fs::create_dir_all(repo.sessions()).map_err(|e| format!("{}: {e}", repo.sessions().display()))?;
    // The input goes beside the session, named after it.
    let keep = |source: &str, extension: &str| -> Result<String, String> {
        let file = format!("{name}.{extension}");
        let target = repo.sessions().join(&file);
        // The file may be there already (written in sessions/ first).
        let same = std::fs::canonicalize(source).ok().is_some_and(|s| std::fs::canonicalize(&target).ok() == Some(s));
        if !same {
            std::fs::copy(source, &target).map_err(|e| format!("{source}: {e}"))?;
        }
        Ok(file)
    };
    let (inputs, replay) = match (a.value("--inputs"), a.value("--replay")) {
        (Some(file), None) => (Some(keep(file, "inputs")?), None),
        (None, Some(file)) => (None, Some(keep(file, "session")?)),
        (None, None) => (None, None),
        _ => return Err("--inputs or --replay, not both".into()),
    };
    let description = a.value("--description").unwrap_or_default().to_owned();
    let options = a.value("--clockwork").map(clockwork_options).unwrap_or_default();
    Session { description, inputs, replay, seconds, options, trace: None }.save(repo, name)?;
    say("session", format!("{name}: {seconds} s"));
    say("next", format!("rrc record {name}"));
    Ok(ExitCode::SUCCESS)
}

/// More `clockwork run` options for a session (`--joystick 1`), split at spaces.
fn clockwork_options(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

fn note(text: &str) -> String {
    if text.is_empty() { String::new() } else { format!(": {text}") }
}
