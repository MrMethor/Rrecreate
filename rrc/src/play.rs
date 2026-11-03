//! `rrc play` (D124): the person plays the original in Clockwork's window, and rrc follows the play as it is
//! recorded: every so often (`--every`, 60 s of play) the part played since is a session of its own, from the
//! last tick of the part before (`NAME/1`, `NAME/2`, ... in `sessions/NAME/`), recorded and compared with
//! the port in lockstep (`rrc compare --lockstep`): what differs and what the port noted, said as it is
//! first seen, and counted for the whole play in `.rrc/play/NAME/`. The play is the session `NAME` too.
//!
//! Recorded play (`clockwork play --record`, a session's `replay`) as rrc reads it: lines `<cycle>
//! K|M|J|R ...` after a header (`program`, `args`, `ips`); `# at <cycle>` says every input up to that cycle
//! is in the file, `# end <cycle>` that the play stopped.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, ExitCode};

use crate::args::Args;
use crate::cache::sha256;
use crate::compare::Census;
use crate::repo::Repo;
use crate::say;
use crate::session::{Base, Session};

/// How far a recorded play is: its speed, the cycle every input up to which is written, whether it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub ips: u64,
    pub at: u64,
    pub ended: bool,
}

/// Reads how far the play in `path` is.
pub fn progress(path: &Path) -> Result<Progress, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut progress =
        Progress { ips: ips(&text).ok_or_else(|| format!("{}: no ips line", path.display()))?, at: 0, ended: false };
    for line in text.lines() {
        if let Some(cycle) = line.strip_prefix("# at ").and_then(|c| c.trim().parse().ok()) {
            progress.at = cycle;
        } else if let Some(cycle) = line.strip_prefix("# end ").and_then(|c| c.trim().parse().ok()) {
            (progress.at, progress.ended) = (cycle, true);
        }
    }
    Ok(progress)
}

/// How far the play in `path` was at each of its marks, in order.
fn marks(path: &Path) -> Result<Vec<Progress>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ips = ips(&text).ok_or_else(|| format!("{}: no ips line", path.display()))?;
    let at = |line: &str, mark: &str| line.strip_prefix(mark).and_then(|c| c.trim().parse().ok());
    Ok(text
        .lines()
        .filter_map(|l| match (at(l, "# at "), at(l, "# end ")) {
            (Some(cycle), _) => Some(Progress { ips, at: cycle, ended: false }),
            (_, Some(cycle)) => Some(Progress { ips, at: cycle, ended: true }),
            _ => None,
        })
        .collect())
}

/// The play's speed, from its `ips` line.
fn ips(text: &str) -> Option<u64> {
    text.lines().find_map(|l| l.trim().strip_prefix("ips ").and_then(|n| n.trim().parse().ok())).filter(|&n| n > 0)
}

/// The cycle a session of `seconds` from power-on ends at, at `ips`.
pub fn end_cycle(seconds: f64, ips: u64) -> u64 {
    (seconds * ips as f64).round() as u64
}

/// The seconds from power-on of cycle `cycle` at `ips`: a session that long ends there ([`end_cycle`]).
pub fn seconds(cycle: u64, ips: u64) -> f64 {
    cycle as f64 / ips as f64
}

/// The cycle the play in `path` is at when a session `seconds` long ends.
pub fn end_of(path: &Path, seconds: f64) -> Result<u64, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ips = ips(&text).ok_or_else(|| format!("{}: no ips line", path.display()))?;
    Ok(end_cycle(seconds, ips))
}

/// What of the play in `path` a session `seconds` long runs with (its run's identity, D124): its header and
/// its input up to the session's end. Input after it can't change the run, so a play that goes on being
/// recorded leaves the identity of a session of a part of it as it was.
pub fn identity(path: &Path, seconds: f64) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let end = end_cycle(seconds, ips(&text).ok_or_else(|| format!("{}: no ips line", path.display()))?);
    let kept: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| l.split(' ').next().and_then(|c| c.parse::<u64>().ok()).is_none_or(|cycle| cycle <= end))
        .collect();
    Ok(sha256(format!("play 1\n{}", kept.join("\n")).as_bytes()))
}

/// A part of the play, recorded: its session, its ticks, the cycles it begins and ends at.
struct Part {
    name: String,
    ticks: u64,
    begin: u64,
    end: u64,
}

/// `rrc play NAME [--from SESSION --tick N] [--every SECONDS] [--replay FILE]`.
pub(crate) fn play(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--from", "--tick", "--every", "--replay"], &[])?;
    let usage = "play NAME [--from SESSION --tick N] [--every SECONDS] [--replay FILE]";
    let [name] = a.positionals(1, 1, usage)? else { unreachable!() };
    if name.is_empty() || name.contains(['/', '\\', '.']) {
        return Err(format!("{name:?}: a play's name is a session's, without / \\ or ."));
    }
    if Session::path(repo, name).exists() || repo.sessions().join(name).exists() {
        return Err(format!("there is a session {name} already: play under another name"));
    }
    let from = match (a.value("--from"), a.value("--tick")) {
        (Some(session), Some(tick)) => {
            let tick = tick.parse().ok().filter(|&t: &u64| t > 0).ok_or("--tick is a tick number")?;
            Some(Base { session: session.to_owned(), tick })
        }
        (None, None) => None,
        _ => return Err("--from SESSION and --tick N go together".into()),
    };
    let every: f64 = match a.value("--every") {
        Some(s) => s.parse().ok().filter(|s: &f64| *s > 0.0).ok_or("--every is a number of seconds")?,
        None => 60.0,
    };
    std::fs::create_dir_all(repo.sessions()).map_err(|e| format!("{}: {e}", repo.sessions().display()))?;
    let file = format!("{name}.session");
    let path = repo.sessions().join(&file);
    let mut session = Session {
        description: "played by the person (rrc play)".into(),
        replay: Some(file.clone()),
        from: from.clone(),
        ..Session::default()
    };
    // Where the play starts: the session's tick (its snapshot there), or power-on. Said first, and what takes
    // time before the window opens, so the terminal never sits silent.
    let start = match &from {
        Some(f) => format!("{}'s tick {}", f.session, f.tick),
        None => "power-on".to_owned(),
    };
    say("play", format!("{name}: the original from {start}, recorded in sessions/{file}"));
    if !crate::snapshots::based_kept(repo, &session)? {
        say(
            "start",
            format!("the original's state at {start} isn't kept: made now from that session's run (minutes, once)"),
        );
    }
    let based = crate::quietly(|| crate::snapshots::based(repo, &session))?;
    // The port built now: a build while the person plays would slow the window.
    say("port", "built first, so that no build slows the window");
    let built = crate::compare::build_port(repo);
    let start_ms = based.as_ref().map_or(0, |(_, ms)| *ms);
    let mut window = match a.value("--replay") {
        // A play recorded before: followed as if played now.
        Some(recorded) => {
            std::fs::copy(recorded, &path).map_err(|e| format!("{recorded}: {e}"))?;
            None
        }
        None => {
            let mut command = Command::new(crate::clockwork::find(&repo.local)?);
            command.arg("play").args(crate::clockwork::argv(repo, &session.options)?);
            if let Some((hash, _)) = &based {
                command.arg("--load-snapshot").arg(crate::cache::Cache::open(&repo.local)?.path(hash));
            }
            command.arg("--record").arg(&path);
            Some(command.spawn().map_err(|e| format!("clockwork play: {e}"))?)
        }
    };
    let start = match &from {
        Some(f) => format!("{}'s tick {} ({:.1} s from power-on)", f.session, f.tick, start_ms as f64 / 1000.0),
        None => "power-on".to_owned(),
    };
    if window.is_some() {
        say("window", "play in it; close it to stop. What the port does otherwise shows here as it is found");
    }
    let dir = repo.state()?.join("play").join(name.as_str());
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut log = std::fs::File::create(dir.join("log.txt")).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut parts: Vec<Part> = Vec::new();
    let mut census = Census::default();
    let mut failed = false;
    if let Err(e) = built {
        say("error", format!("{e}; the play is recorded, compare its parts once the port builds"));
        failed = true;
    }
    // A play recorded before goes mark by mark, as it was played.
    let mut marks = match &window {
        Some(_) => Vec::new().into_iter(),
        None => marks(&path)?.into_iter(),
    };
    loop {
        let (progress, done) = match &mut window {
            Some(child) => {
                let done = child.try_wait().map_err(|e| format!("clockwork play: {e}"))?.is_some();
                (progress(&path).ok(), done)
            }
            None => (marks.next(), marks.len() == 0),
        };
        if let Some(progress) = progress {
            let ips = progress.ips;
            let last = parts.last().map_or(end_cycle(start_ms as f64 / 1000.0, ips), |p| p.end);
            let due = progress.at > last
                && (done || progress.ended || progress.at - last >= end_cycle(every, ips))
                && !failed;
            if due {
                match part(repo, name, &file, &from, &mut parts, (last, progress.at), ips) {
                    Ok(Some(found)) => {
                        let p = parts.last().expect("a part");
                        let played = |cycle: u64| clock(seconds(cycle, ips) - start_ms as f64 / 1000.0);
                        let line = format!(
                            "{}: {}-{} of the play, {} ticks: {}",
                            p.name,
                            played(p.begin),
                            played(p.end),
                            p.ticks,
                            summary(&found)
                        );
                        say("part", &line);
                        writeln!(log, "part      {line}").ok();
                        for (kind, key, seen) in census.new_in(&found) {
                            let (kind, text) = Census::line(kind, key, seen, found.passes);
                            say("new", format!("{kind} {text}"));
                            writeln!(log, "new       {kind} {text}").ok();
                        }
                        census.add(&found);
                        census.save(&dir.join("census.json"))?;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        say("error", format!("following the play: {e}; it goes on being recorded"));
                        failed = true;
                    }
                }
            }
        }
        if done {
            break;
        }
        if window.is_some() {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
    let progress = progress(&path)?;
    session.seconds = seconds(progress.at, progress.ips);
    session.save(repo, name)?;
    let length = clock(session.seconds - start_ms as f64 / 1000.0);
    let ticks = parts.first().map_or(0, |_| parts.iter().map(|p| p.ticks).sum::<u64>() + 1 - parts.len() as u64);
    say(
        "play",
        format!(
            "{name}: {length} played from {start}, {} parts (sessions/{name}/1 to {}), {ticks} ticks",
            parts.len(),
            parts.len()
        ),
    );
    census.show(name, 15);
    say("census", crate::path_text(&dir.join("census.json")));
    // Where to look first: the field that differs in the most passes.
    if let Some((key, seen)) = census.fields.iter().max_by_key(|(key, seen)| (seen.ticks, std::cmp::Reverse(*key))) {
        say(
            "next",
            format!(
                "rrc compare {} --transplant {}: {key} at its tick {}, with who wrote the original's value",
                seen.session,
                seen.tick.saturating_sub(1).max(1),
                seen.tick
            ),
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// The next part of the play, from cycle `begin` to `end`: a session from the last tick of the part before (or
/// where the play starts), recorded and compared in lockstep. None when no tick ended in it yet: a later
/// round takes it on with what follows.
fn part(
    repo: &Repo,
    name: &str,
    file: &str,
    from: &Option<Base>,
    parts: &mut Vec<Part>,
    (begin, end): (u64, u64),
    ips: u64,
) -> Result<Option<Census>, String> {
    let n = parts.len() + 1;
    let part = format!("{name}/{n}");
    let base = match parts.last() {
        Some(p) => Some(Base { session: p.name.clone(), tick: p.ticks }),
        None => from.clone(),
    };
    let session = Session {
        description: format!("part {n} of the play {name} (rrc play)"),
        replay: Some(file.to_owned()),
        seconds: seconds(end, ips),
        from: base,
        ..Session::default()
    };
    std::fs::create_dir_all(repo.sessions().join(name)).map_err(|e| e.to_string())?;
    session.save(repo, &part)?;
    crate::quietly(|| crate::trace::record(repo, std::slice::from_ref(&part)))?;
    let ticks = Session::load(repo, &part)?.trace.map_or(0, |t| t.ticks);
    if ticks == 0 {
        return Ok(None);
    }
    let census = crate::quietly(|| crate::compare::lockstep(repo, &part))?;
    parts.push(Part { name: part, ticks, begin, end });
    Ok(Some(census))
}

/// A part's census in a few words.
fn summary(c: &Census) -> String {
    let mut parts = Vec::new();
    if !c.fields.is_empty() {
        parts.push(format!("{} fields differ", c.fields.len()));
    }
    if let Some(s) = &c.screen {
        parts.push(format!("the screen at {} ticks", s.ticks));
    }
    if let Some(s) = &c.sound {
        parts.push(format!("the sound at {} ticks", s.ticks));
    }
    if !c.unset.is_empty() {
        parts.push(format!("{} unset", c.unset.len()));
    }
    if !c.notes.is_empty() {
        parts.push(format!("{} notes", c.notes.len()));
    }
    if parts.is_empty() { format!("all {} passes match", c.passes) } else { parts.join(", ") }
}

/// Seconds as m:ss.
fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_play_is_read_while_it_grows() {
        let path = std::env::temp_dir().join(format!("rrc-play-{}.session", std::process::id()));
        let start = "# clockwork session v1\nprogram G.EXE\nargs \nips 1000\n";
        std::fs::write(&path, format!("{start}# at 2000\n2100 K 1E\n# at 3000\n3500 K 9E\n")).unwrap();
        assert_eq!(progress(&path).unwrap(), Progress { ips: 1000, at: 3000, ended: false });
        let before = identity(&path, 3.0).unwrap();
        // Played on: a session of the first 3 s runs as it did.
        std::fs::write(&path, format!("{start}# at 2000\n2100 K 1E\n# at 3000\n3500 K 9E\n# at 4000\n# end 4100\n"))
            .unwrap();
        assert_eq!(progress(&path).unwrap(), Progress { ips: 1000, at: 4100, ended: true });
        assert_eq!(identity(&path, 3.0).unwrap(), before);
        assert_ne!(identity(&path, 4.0).unwrap(), before, "the release at 3500 is in a longer one");
        assert_eq!((end_of(&path, 4.1).unwrap(), seconds(4100, 1000)), (4100, 4.1));
        std::fs::remove_file(&path).ok();
    }
}
