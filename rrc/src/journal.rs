//! The journal (D157): each command `rrc` ran in the port, in `.rrc/journal.jsonl` on this machine, with how a
//! comparison ended. From it the toolkit says what an agent can't see from inside: that a comparison has ended at the
//! same first difference for the third time running (`stuck`), with the kinds of evidence not looked at since it
//! first showed; how the commands spent on an item divide between comparing and looking (`mix`); and, for a test of
//! the playbook, what a stretch of work was made of (`rrc journal`). It is never state the port is judged by.

use std::cell::RefCell;
use std::io::Write;
use std::process::ExitCode;

use serde::{Deserialize, Serialize};

use crate::args::Args;
use crate::repo::Repo;
use crate::say;

const JOURNAL: &str = "journal.jsonl";
/// A comparison that ends at the same first difference this many times running is stuck.
pub(crate) const STUCK: usize = 3;
/// Commands `rrc journal` lists at most.
const SHOWN: usize = 12;
/// Commands that say where things are and aren't the work.
const UNKEPT: [&str; 9] = ["help", "--help", "-h", "--version", "init", "rules", "status", "doctor", "journal"];

/// A command that ran.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// When, in seconds since 1970 as the host has them.
    pub t: u64,
    pub cmd: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    pub ok: bool,
    /// The session a comparison was of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// How a comparison ended: `match`, or its first difference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    /// Whether this was the comparison that made its first difference a stuck one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stuck: bool,
}

/// A kind of command: the comparison, the kinds of evidence (the sources' order of docs/porting.md), and the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    Compare,
    Code,
    Sources,
    Files,
    Runs,
    Other,
}

impl Kind {
    /// The kinds an agent chooses between when a value is in question.
    const EVIDENCE: [Kind; 4] = [Kind::Code, Kind::Sources, Kind::Files, Kind::Runs];

    fn name(self) -> &'static str {
        match self {
            Kind::Compare => "compare",
            Kind::Code => "code",
            Kind::Sources => "sources",
            Kind::Files => "files",
            Kind::Runs => "runs",
            Kind::Other => "other",
        }
    }

    /// How to look at this kind of evidence.
    fn how(self) -> &'static str {
        match self {
            Kind::Code => "the code (rrc show ID --c, rrc pass SESSION --tick N)",
            Kind::Sources => "an outside source (rrc sources WORD)",
            Kind::Files => "the game's files (rrc files SESSION, rrc asset FILE)",
            Kind::Runs => "the value over the run (rrc watch SESSION ID --tick N, rrc trace SESSION --tick N)",
            Kind::Compare | Kind::Other => "",
        }
    }
}

impl Entry {
    pub(crate) fn kind(&self) -> Kind {
        let verb = self.args.first().map(String::as_str).unwrap_or_default();
        match self.cmd.as_str() {
            "compare" => Kind::Compare,
            "show" | "pass" | "calls" => Kind::Code,
            // Keeping the list isn't looking something up in it.
            "sources" if !matches!(verb, "" | "add" | "exclude" | "remove" | "sweep" | "none") => Kind::Sources,
            "files" | "asset" => Kind::Files,
            "watch" | "trace" | "peek" | "search" | "frame" | "inputs" | "ports" => Kind::Runs,
            _ => Kind::Other,
        }
    }

    fn line(&self) -> String {
        let end = self.end.as_ref().map_or(String::new(), |end| format!(" -> {end}"));
        let failed = if self.ok || self.end.is_some() { "" } else { " (failed)" };
        format!("{} {}{end}{failed}", self.cmd, self.args.join(" ")).trim().to_owned()
    }
}

thread_local! {
    /// How this command's comparison ended, and of which session: said by `rrc compare`, kept with the command.
    static ENDED: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

/// `rrc compare` says how its comparison of `session` ended: `match`, or its first difference.
pub(crate) fn ended(session: &str, end: impl Into<String>) {
    ENDED.set(Some((session.to_owned(), end.into())));
}

fn path(repo: &Repo) -> Result<std::path::PathBuf, String> {
    Ok(repo.state()?.join(JOURNAL))
}

/// The journal's entries, oldest first; a line that isn't one is left out.
pub(crate) fn load(repo: &Repo) -> Vec<Entry> {
    let Ok(text) = path(repo).and_then(|path| std::fs::read_to_string(path).map_err(|e| e.to_string())) else {
        return Vec::new();
    };
    text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

/// Keeps the command `args` (its name first) that just ran in `repo`, and says so if its comparison is stuck.
pub(crate) fn close(repo: &Repo, args: &[String], ok: bool) {
    let Some(cmd) = args.first().filter(|cmd| !UNKEPT.contains(&cmd.as_str())) else { return };
    let (session, end) = ENDED.take().unzip();
    let mut entry = Entry { t: now(), cmd: cmd.clone(), args: args[1..].to_vec(), ok, session, end, stuck: false };
    let before = load(repo);
    if let Some(run) = stuck_run(&before, &entry) {
        entry.stuck = run.len() + 1 == STUCK;
        let end = entry.end.as_deref().unwrap_or_default();
        say("stuck", format!("the same first difference {} times running: {end}", run.len() + 1));
        let since = &before[run[0]..];
        let untried: Vec<&str> =
            Kind::EVIDENCE.iter().filter(|kind| !since.iter().any(|e| e.kind() == **kind)).map(|k| k.how()).collect();
        match untried.is_empty() {
            false => say("untried", format!("since it first showed: {}", untried.join("; "))),
            true => say(
                "untried",
                "nothing: every kind of source was looked at. Write the question without its history and get fresh \
                 eyes (docs/porting.md, \"When stuck\")",
            ),
        }
    }
    if entry.cmd == "compare" {
        crate::work::compared(repo, &before);
    }
    // Kept whatever happens to the rest: a journal that can't be written doesn't fail the command.
    let Ok(line) = serde_json::to_string(&entry) else { return };
    let file = path(repo).and_then(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))
    });
    if let Ok(mut file) = file {
        let _ = writeln!(file, "{line}");
    }
}

/// If `entry` is a comparison ending at the first difference its session's comparisons before it ended at, at least
/// [`STUCK`] times running with it: where those before it are in `before`, oldest first. Other commands between them
/// don't break the run; a comparison of the session that ended otherwise does.
fn stuck_run(before: &[Entry], entry: &Entry) -> Option<Vec<usize>> {
    let end = entry.end.as_ref().filter(|end| *end != "match" && !end.starts_with("census"))?;
    if entry.cmd != "compare" {
        return None;
    }
    let mut run = Vec::new();
    for (i, e) in before.iter().enumerate().rev() {
        if e.cmd != "compare" || e.session != entry.session || e.end.is_none() {
            continue;
        }
        if e.end.as_ref() != Some(end) {
            break;
        }
        run.push(i);
    }
    run.reverse();
    (run.len() + 1 >= STUCK).then_some(run)
}

/// How `entries` divide between comparing and each kind of looking: `compare 14, code 0, sources 0, files 0, runs 2`.
pub(crate) fn mix(entries: &[Entry]) -> String {
    let count = |kind: Kind| entries.iter().filter(|e| e.kind() == kind).count();
    let kinds = [Kind::Compare, Kind::Code, Kind::Sources, Kind::Files, Kind::Runs];
    kinds.map(|kind| format!("{} {}", kind.name(), count(kind))).join(", ")
}

/// `rrc journal`: what the work in this port was made of, for a test of the playbook and for the person: how many
/// commands, how they divide, how often a comparison was stuck, and the last ones.
pub(crate) fn journal(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--lines"], &[])?;
    a.positionals(0, 0, "journal [--lines N]")?;
    let shown: usize = a.value("--lines").map_or(Ok(SHOWN), str::parse).map_err(|_| "--lines is a count")?;
    let entries = load(repo);
    let Some(first) = entries.first() else {
        say("journal", "empty: rrc keeps each command it runs here from now on");
        return Ok(ExitCode::SUCCESS);
    };
    let minutes = entries.last().map_or(0, |last| last.t.saturating_sub(first.t)) / 60;
    say("journal", format!("{} commands over {}", entries.len(), span(minutes)));
    say("mix", mix(&entries));
    let stuck = entries.iter().filter(|e| e.stuck).count();
    say("stuck", format!("{stuck} times a comparison ended at the same first difference {STUCK} times running"));
    // Each item taken and closed (`rrc work`), with what it took.
    for e in entries.iter().filter(|e| e.cmd == "work" && e.end.is_some()) {
        say("item", e.end.as_deref().unwrap_or_default());
    }
    for e in entries.iter().rev().take(shown).rev() {
        say("ran", e.line());
    }
    Ok(ExitCode::SUCCESS)
}

/// Minutes as hours and minutes.
pub(crate) fn span(minutes: u64) -> String {
    match minutes / 60 {
        0 => format!("{minutes} min"),
        hours => format!("{hours} h {} min", minutes % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ran(cmd: &str, args: &[&str]) -> Entry {
        Entry { cmd: cmd.into(), args: args.iter().map(|a| a.to_string()).collect(), ok: true, ..Entry::default() }
    }

    fn compared(session: &str, end: &str) -> Entry {
        Entry { session: Some(session.into()), end: Some(end.into()), ok: end == "match", ..ran("compare", &[session]) }
    }

    #[test]
    fn a_comparison_is_stuck_at_the_same_first_difference_three_times_running() {
        let at = "tick 194: enemies[1].hits";
        let mut before = vec![compared("play", "tick 90: score"), compared("play", at)];
        assert_eq!(stuck_run(&before, &compared("play", at)), None, "twice isn't stuck");
        // Other commands and other sessions' comparisons between them don't break the run.
        before.extend([ran("watch", &["play", "score"]), compared("menu", "match"), compared("play", at)]);
        assert_eq!(stuck_run(&before, &compared("play", at)), Some(vec![1, 4]));
        assert_eq!(stuck_run(&before, &compared("play", "tick 200: enemies[1].hits")), None, "it moved: progress");
        assert_eq!(stuck_run(&before, &compared("menu", at)), None, "another session's");
        before.push(compared("play", "match"));
        assert_eq!(stuck_run(&before, &compared("play", at)), None, "a match between breaks it");
        let matches = vec![compared("play", "match"), compared("play", "match")];
        assert_eq!(stuck_run(&matches, &compared("play", "match")), None, "a match is never stuck");
    }

    #[test]
    fn commands_are_kinds_of_evidence() {
        let kinds: Vec<Kind> = [
            ran("compare", &["play"]),
            ran("show", &["update", "--c"]),
            ran("sources", &["spells"]),
            ran("sources", &["add", "the-wiki"]),
            ran("sources", &[]),
            ran("asset", &["A.DAT"]),
            ran("watch", &["play", "score"]),
            ran("record", &["play"]),
        ]
        .iter()
        .map(Entry::kind)
        .collect();
        let expected =
            [Kind::Compare, Kind::Code, Kind::Sources, Kind::Other, Kind::Other, Kind::Files, Kind::Runs, Kind::Other];
        assert_eq!(kinds, expected);
        let entries = [compared("play", "match"), ran("show", &["x"]), ran("trace", &["play"]), ran("peek", &["play"])];
        assert_eq!(mix(&entries), "compare 1, code 1, sources 0, files 0, runs 2");
        assert_eq!(compared("play", "tick 2: x").line(), "compare play -> tick 2: x");
        assert_eq!((span(59), span(135)), ("59 min".into(), "2 h 15 min".into()));
    }

    #[test]
    fn an_entry_reads_back_as_written() {
        let entry = Entry { t: 5, stuck: true, ..compared("play", "tick 2: x") };
        let line = serde_json::to_string(&entry).unwrap();
        assert_eq!(serde_json::from_str::<Entry>(&line).unwrap(), entry);
        assert_eq!(serde_json::to_string(&ran("show", &[])).unwrap(), r#"{"t":0,"cmd":"show","ok":true}"#);
    }
}
