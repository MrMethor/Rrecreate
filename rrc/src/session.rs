//! Sessions (DESIGN.md §7): what to reproduce. A session is a TOML file in `sessions/` naming its input
//! (an input script, or play recorded by `clockwork play --record`, kept beside it), its length and the
//! edits that set up its scenario (D59, D64); `rrc record` adds the hashes of its trace and its run
//! (D44, D54), and `rrc compare --pair` its paired checkpoints (D63).

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
    /// Paired checkpoints (D63): where the port matched, its state and the original's snapshot.
    #[serde(default, rename = "pair", skip_serializing_if = "Vec::is_empty")]
    pub pairs: Vec<Pair>,
    /// The scenario's edits (D64), by tick.
    #[serde(default, rename = "edit", skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<Edit>,
}

/// A write into the original's memory at a tick: at its sync point, before that pass reads.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Edit {
    pub tick: u64,
    /// A data name or an ID, with a path into it as its type reads (`enemies[2].alive`).
    pub at: String,
    /// How to read it; empty: data.tsv's type.
    #[serde(default, rename = "type", skip_serializing_if = "String::is_empty")]
    pub ty: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// The value as the type reads it: a number, an array, some of a structure's fields, an
    /// enumeration's value name.
    pub value: serde_json::Value,
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

/// A paired checkpoint: the port's state and the original's snapshot at the end of a tick both matched
/// at.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pair {
    pub tick: u64,
    /// The port's state (`rrc-port-state`), in the cache.
    pub port: String,
    /// Clockwork's snapshot there, in the cache.
    pub snapshot: String,
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
    let valued = ["--inputs", "--replay", "--seconds", "--description", "--clockwork", "--type", "--note"];
    let a = Args::parse(args, &valued, &["--remove"])?;
    let usage = "session [add NAME --seconds N (--inputs FILE | --replay FILE) [--description TEXT] \
                 [--clockwork OPTIONS] | set NAME [--seconds N] [--description TEXT] [--clockwork OPTIONS] | \
                 edit NAME TICK AT (VALUE [--type T] [--note TEXT] | --remove)]";
    match a.positionals(0, 5, usage)? {
        [] => {
            for name in names(repo)? {
                let s = Session::load(repo, &name)?;
                let trace = s.trace.map_or("not recorded".to_owned(), |t| format!("{} ticks", t.ticks));
                let edits = match s.edits.len() {
                    0 => String::new(),
                    n => format!(", {n} edit{}", if n == 1 { "" } else { "s" }),
                };
                say("session", format!("{name}: {} s, {trace}{edits}{}", s.seconds, note(&s.description)));
            }
            Ok(ExitCode::SUCCESS)
        }
        [edit, name, tick, at, rest @ ..] if edit == "edit" && rest.len() <= 1 => {
            edit_session(repo, &a, name, tick, at, rest.first())
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
    Session { description, inputs, replay, seconds, options, ..Session::default() }.save(repo, name)?;
    say("session", format!("{name}: {seconds} s"));
    say("next", format!("rrc record {name}"));
    Ok(ExitCode::SUCCESS)
}

/// `rrc session edit`: adds an edit, or replaces the one at the same tick and place; or removes it.
fn edit_session(
    repo: &Repo,
    a: &Args,
    name: &str,
    tick: &str,
    at: &str,
    value: Option<&String>,
) -> Result<ExitCode, String> {
    let mut s = Session::load(repo, name)?;
    let tick: u64 = tick.parse().ok().filter(|&t| t > 0).ok_or("TICK is a tick number, from 1")?;
    let before = s.edits.len();
    s.edits.retain(|e| !(e.tick == tick && e.at == at));
    match (value, a.has("--remove")) {
        (None, true) if s.edits.len() == before => return Err(format!("{name} has no edit of {at} at tick {tick}")),
        (None, true) => say("removed", format!("the edit of {at} at tick {tick}")),
        (Some(value), false) => {
            // JSON, or a word (an enumeration's value name).
            let value = serde_json::from_str(value).unwrap_or_else(|_| serde_json::Value::String(value.clone()));
            let ty = a.value("--type").unwrap_or_default().to_owned();
            let note = a.value("--note").unwrap_or_default().to_owned();
            let edit = Edit { tick, at: at.to_owned(), ty, note, value };
            let k = crate::knowledge::Knowledge::load(&repo.knowledge())?;
            let writes = crate::edits::resolve(&k, std::slice::from_ref(&edit))?;
            let at_ids: Vec<String> =
                writes.iter().map(|w| format!("{} {}", w.id, crate::edits::hex(&w.bytes))).collect();
            say("edit", format!("tick {tick}: {at} = {} ({})", edit.value, at_ids.join(", ")));
            s.edits.push(edit);
            s.edits.sort_by_key(|e| e.tick);
        }
        _ => return Err("a VALUE, or --remove".into()),
    }
    s.save(repo, name)?;
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
