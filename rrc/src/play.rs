//! `rrc play` (D124, D192): the person plays the original in Clockwork's window, and rrc follows the play as it is
//! recorded: a second Clockwork runs it as its file is written, the port runs beside it, free and corrected where it
//! differs, and what differs is said as it is found (`live.rs`). The play is the session `NAME`, its trace the
//! follower's; what was found is in `.rrc/play/NAME/` and, as `rrc compare NAME --corrected` keeps it, in
//! `.rrc/compare/NAME/corrected.json`.
//!
//! Recorded play (`clockwork play --record`, a session's `replay`) as rrc reads it: lines `<cycle>
//! K|M|J|R ...` after a header (`program`, `args`, `ips`); `# at <cycle>` says every input up to that cycle
//! is in the file, `# end <cycle>` that the play stopped.

use std::path::Path;
use std::process::{Command, ExitCode};

use crate::args::Args;
use crate::cache::sha256;
use crate::compare::Census;
use crate::repo::Repo;
use crate::say;
use crate::session::{Base, Recorded, Session};

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

/// A play's length while it is played: a year.
const PLAYING: f64 = 365.0 * 24.0 * 3600.0;

/// `rrc play NAME [--from SESSION --tick N] [--replay FILE]`, or `rrc play NAME --resume`.
pub(crate) fn play(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--from", "--tick", "--replay"], &["--resume"])?;
    let usage = "play NAME [--from SESSION --tick N] [--replay FILE] | play NAME --resume";
    let [name] = a.positionals(1, 1, usage)? else { unreachable!() };
    if name.is_empty() || name.contains(['/', '\\', '.']) {
        return Err(format!("{name:?}: a play's name is a session's, without / \\ or ."));
    }
    let file = format!("{name}.session");
    let path = repo.sessions().join(&file);
    let resume = a.has("--resume");
    let mut session = if resume {
        // A play cut short (rrc or the machine stopped): followed again from its file, to its last mark.
        if a.value("--from").is_some() || a.value("--replay").is_some() {
            return Err("--resume goes on with a play as it was begun: alone".into());
        }
        let session = Session::load(repo, name)?;
        if session.replay.as_deref() != Some(file.as_str()) || !path.is_file() {
            return Err(format!("{name} isn't a play (rrc play {name} begins one)"));
        }
        session
    } else {
        if Session::path(repo, name).exists() || repo.sessions().join(name).exists() {
            return Err(format!("there is a session {name} already: play under another name"));
        }
        let from = match (a.value("--from"), a.value("--tick")) {
            (Some(session), Some(tick)) => {
                let tick = tick.parse().ok().filter(|&t: &u64| t > 0).ok_or("--tick is a tick number")?;
                Some(Base { session: session.to_owned(), tick, cycle: None })
            }
            (None, None) => None,
            _ => return Err("--from SESSION and --tick N go together".into()),
        };
        let mut session = Session {
            description: "played by the person (rrc play)".into(),
            replay: Some(file.clone()),
            from,
            ..Session::default()
        };
        // From power-on: the game's folder as it is now, which the play runs on (D170).
        if session.from.is_none() {
            session.files = crate::folder::take(repo)?;
        }
        session
    };
    std::fs::create_dir_all(repo.sessions()).map_err(|e| format!("{}: {e}", repo.sessions().display()))?;
    // Where the play starts: the session's tick (its snapshot there), or power-on. Said first, and what takes
    // time before the window opens, so the terminal never sits silent.
    let start = match &session.from {
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
    crate::compare::build_port(repo)?;
    let start_ms = based.as_ref().map_or(0, |(_, ms)| *ms);
    // As long as the play, which is known once it ends; until then, longer than any play (what reads the session's run
    // as it goes, as the port's start from a session's tick does, isn't cut short).
    session.seconds = PLAYING;
    // Kept before the window opens, so that a play cut short goes on (--resume).
    session.save(repo, name)?;
    let window = match (a.value("--replay"), resume) {
        // A play recorded before: followed as if played now.
        (Some(recorded), _) => {
            std::fs::copy(recorded, &path).map_err(|e| format!("{recorded}: {e}"))?;
            None
        }
        (None, true) => None,
        (None, false) => {
            let mut command = Command::new(crate::clockwork::find(&repo.local)?);
            let folder = crate::folder::of(repo, &session)?;
            command.arg("play").args(crate::clockwork::argv_on(repo, &folder, &session.options));
            if let Some((hash, _)) = &based {
                command.arg("--load-snapshot").arg(crate::cache::Cache::open(&repo.local)?.path(hash));
            }
            command.arg("--record").arg(&path);
            Some(command.spawn().map_err(|e| format!("clockwork play: {e}"))?)
        }
    };
    if window.is_some() {
        say("window", "play in it; close it to stop. What the port does otherwise shows here as it is found");
    }
    let followed = crate::sources::held_still(&repo.root, || crate::live::follow(repo, name, &session, window))?;
    // The play is the session: as long as the play, its trace the follower's.
    session.seconds = seconds(followed.end, followed.ips);
    let identity = crate::trace::identity(repo, &session)?;
    let written = &followed.written;
    session.trace = Some(Recorded {
        hash: written.hash.clone(),
        run: written.run.clone(),
        ticks: written.ticks,
        clockwork: written.header.clockwork.clone(),
        memory: followed.memory.clone(),
        identity,
    });
    session.save(repo, name)?;
    let census = Census { recorded: written.hash.clone(), ..followed.census };
    let dir = repo.state()?.join("play").join(name.as_str());
    census.save(&dir.join("census.json"))?;
    census.save(&repo.state()?.join("compare").join(name.as_str()).join("corrected.json"))?;
    let length = crate::live::clock(session.seconds - start_ms as f64 / 1000.0);
    let start = match &session.from {
        Some(f) => format!("{}'s tick {} ({:.1} s from power-on)", f.session, f.tick, start_ms as f64 / 1000.0),
        None => "power-on".to_owned(),
    };
    let hashes = match followed.hashes {
        1 => "1 state hash".to_owned(),
        n => format!("{n} state hashes"),
    };
    let panics = match census.panics.len() {
        1 => "1 panic".to_owned(),
        n => format!("{n} panics"),
    };
    say(
        "play",
        format!(
            "{name}: {length} played from {start}, {} ticks, {panics}; the runs the same ({hashes} of the window's)",
            written.ticks
        ),
    );
    census.show(name, 15);
    say("census", crate::path_text(&dir.join("census.json")));
    // The port's own input handling, beside (D197): what its `Live` made of the play's input at each tick.
    followed.lived.save(&dir.join("live.json"))?;
    followed.lived.show(name);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_play_is_read_while_it_grows() {
        let path = std::env::temp_dir().join(format!("rrc-play-{}.session", std::process::id()));
        let start = "# clockwork session v1\nprogram G.EXE\nargs \nips 1000\n";
        std::fs::write(&path, format!("{start}# at 2000\n2100 K 1E\n# at 3000\n3500 K 9E\n")).unwrap();
        let before = identity(&path, 3.0).unwrap();
        // Played on: a session of the first 3 s runs as it did.
        std::fs::write(&path, format!("{start}# at 2000\n2100 K 1E\n# at 3000\n3500 K 9E\n# at 4000\n# end 4100\n"))
            .unwrap();
        assert_eq!(identity(&path, 3.0).unwrap(), before);
        assert_ne!(identity(&path, 4.0).unwrap(), before, "the release at 3500 is in a longer one");
        assert_eq!((end_of(&path, 4.1).unwrap(), seconds(4100, 1000)), (4100, 4.1));
        std::fs::remove_file(&path).ok();
    }
}
