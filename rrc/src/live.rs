//! A play followed as it goes (M6.5's step 5, D192): the person plays the original in Clockwork's window; a second
//! Clockwork, the follower, runs the play as its file is written (D189) and makes its ticks as the window's run made
//! them; each goes to the port, run free and corrected where it differs (D191), and to the comparer, which says what
//! differs as it is found. Nothing waits on the window: the follower, the port and the comparer keep a few ticks
//! between them at most, a slow one slowing those before it, and the follower and the port run below normal priority.
//! A play recorded before (`rrc play --replay`) or cut short (`--resume`) goes through the same at full speed.

use std::io::IsTerminal as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::cache::Cache;
use crate::compare::{At, Begin, Census, Counting, Original};
use crate::knowledge::Knowledge;
use crate::mapping::Mapping;
use crate::places::Places;
use crate::repo::Repo;
use crate::say;
use crate::session::Session;
use crate::trace::{Recorder, Tick, TraceFile, Written};

/// The follower's ticks the comparer hasn't taken yet, at most (the plan's 3.6): past that the follower waits.
const AHEAD: usize = 4;

/// How far the play is written and the follower has run (cycles), for the status line.
#[derive(Default)]
pub(crate) struct Progress {
    pub written: AtomicU64,
    pub now: AtomicU64,
}

/// What the follower sends the comparer: a tick, or its run's end.
enum Made {
    Tick(Box<Tick>),
    /// The machine's state at the last tick made, for a port started again there (D193).
    State(Box<crate::compare::Given>),
    End(Box<Result<Ended, String>>),
}

/// The follower's run at its end: its trace as written, the data segment it kept (D92), the window's state hashes it
/// checked, the marks it read.
struct Ended {
    written: Written,
    memory: Option<String>,
    hashes: u64,
    /// The play's end: the cycle its file ends at.
    end: Option<u64>,
}

/// A play followed: what the comparison found, the follower's trace, the play's speed and end.
pub(crate) struct Followed {
    pub census: Census,
    pub written: Written,
    pub memory: Option<String>,
    pub hashes: u64,
    pub ips: u64,
    pub end: u64,
}

/// Follows the play of session `name` (`session`) to its end as its file is written by `window` (none: the file is
/// written, or as far as it will be), comparing the port with it as it goes.
pub(crate) fn follow(
    repo: &Repo,
    name: &str,
    session: &Session,
    mut window: Option<std::process::Child>,
) -> Result<Followed, String> {
    let k = Knowledge::load(&repo.knowledge())?;
    let exe = crate::clockwork::find(&repo.local)?;
    let header = crate::trace::header(repo, &k, name, &exe, Vec::new())?;
    // Once the window is closed (or there is none), the play ends at its last mark, written to its end or not.
    let done = Arc::new(AtomicBool::new(window.is_none()));
    let mut follow = crate::trace::follow_play(repo, session, &exe)?;
    follow.finished_when(done.clone());
    let ips = follow.ips;
    let mapping = Mapping::load(repo)?;
    let mapped = mapping.resolve(&k)?;
    if mapped.is_empty() {
        return Err(
            "nothing is mapped: mapping.toml names the port's fields and the original's items (docs/formats.md)".into(),
        );
    }
    let places = Places::new(repo, session);
    let linear = |linear: u32| places.linear_id(linear).ok();
    let reader = crate::values::of(&k).with_linear(&linear);
    let mut original = Original::new(&header, &mapped, &reader, name)?;
    let cache = Cache::open(&repo.local)?;
    let start = crate::compare::PortStart::new(repo, session, name, &k, &mapping, &mapped, header.clone())?;
    let sets = std::collections::BTreeMap::new();
    let census = Census { port: crate::sources::fingerprint(&repo.root), ..Census::default() };
    let mut counting = Counting::new(name, &mapped, &sets, &cache, start.frames(), 1, true, census);
    let progress = Arc::new(Progress::default());
    crate::clockwork::below_normal(true);
    let (sent, taken) = mpsc::sync_channel::<Made>(AHEAD);
    let (ask, asked) = mpsc::channel::<()>();
    let (k, mapping) = (&k, &mapping);
    let followed = std::thread::scope(|s| {
        let to_end = progress.clone();
        let follower = s.spawn(move || {
            let ended = (|| {
                let mut recorder = Recorder::start(repo, session, header, Some(&mut follow))?;
                recorder.until_play_ends(to_end);
                let mut file = TraceFile::create(repo, recorder.header.clone())?;
                let stopped = |_| "the comparison stopped".to_owned();
                while let Some(tick) = recorder.next()? {
                    file.tick(&tick)?;
                    sent.send(Made::Tick(Box::new(tick))).map_err(stopped)?;
                    // A port that stopped starts again from the state here (D193).
                    if asked.try_recv().is_ok() {
                        let given = recorder.at_made(|clockwork, places, tick| {
                            crate::compare::given_now(clockwork, places, k, mapping, tick)
                        })?;
                        sent.send(Made::State(Box::new(given))).map_err(stopped)?;
                    }
                }
                let (end, _, memory) = recorder.finish()?;
                let written = file.end(&end, &Cache::open(&repo.local)?)?;
                Ok(Ended { written, memory, hashes: follow.checked, end: follow.ended })
            })();
            let _ = sent.send(Made::End(Box::new(ended)));
        });
        let watched = Watched { window: &mut window, done: &done };
        let compared = compare(&taken, &ask, &mut original, &mut counting, &start, &progress, ips, watched);
        // The follower stops at its next tick once the comparison has stopped taking them.
        drop(taken);
        let _ = follower.join();
        compared
    });
    crate::clockwork::below_normal(false);
    let (ended, _) = followed?;
    let end = ended.end.ok_or("the play has no end")?;
    Ok(Followed {
        census: counting.census,
        written: ended.written,
        memory: ended.memory,
        hashes: ended.hashes,
        ips,
        end,
    })
}

/// The window, and the flag set once it is closed.
struct Watched<'w> {
    window: &'w mut Option<std::process::Child>,
    done: &'w AtomicBool,
}

impl Watched<'_> {
    /// Whether the window was closed: then the play ends at its last mark.
    fn look(&mut self) {
        if let Some(window) = self.window.as_mut()
            && window.try_wait().ok().flatten().is_some()
        {
            self.done.store(true, Ordering::Relaxed);
            *self.window = None;
        }
    }
}

/// The port as the comparer keeps it (D193): waiting for its start, running, stopped with the follower's state
/// asked for, to start again when the original's tick it was read at comes, or stopped for good.
enum Port {
    Waiting(Vec<Value>),
    Running(Box<crate::compare::PortRun>),
    Asked,
    Again(Box<crate::compare::Given>),
    Off,
}

/// How many times the port is started again in a play at most; past that, it is compared no more.
const AGAIN: usize = 10;

/// The comparer: the follower's ticks as they come, each to the comparer and as an inputs line to the port, what
/// differs said when it is first seen, a port that stops started again from the follower's state at its next tick
/// (`ask`), and a status line on a terminal.
#[allow(clippy::too_many_arguments)]
fn compare(
    taken: &mpsc::Receiver<Made>,
    ask: &mpsc::Sender<()>,
    original: &mut Original,
    counting: &mut Counting,
    start: &crate::compare::PortStart,
    progress: &Progress,
    ips: u64,
    mut watched: Watched,
) -> Result<(Ended, u64), String> {
    let mut status = Status::new(ips);
    let mut port = Port::Waiting(Vec::new());
    let mut previous: Option<Tick> = None;
    let mut screen: Option<String> = None;
    let mut said = Census::default();
    let mut cycles = std::collections::BTreeMap::new();
    let mut start_cycle = 0;
    let mut ticks = 0;
    // Each input's value as the original's ticks so far left it; a port started again gets them all in its first line
    // (the ticks before brought its changes to a port that wasn't running): that tick, those values.
    let mut inputs: std::collections::BTreeMap<usize, String> = std::collections::BTreeMap::new();
    let mut whole: Option<(u64, Vec<Value>)> = None;
    let header = crate::compare::inputs_header(start.header());
    // A line to the port: given if it runs (a port that stops: counted, and its start again asked for), held until
    // its start, or dropped while it is stopped.
    let give = |line: Value, port: &mut Port, counting: &mut Counting| match port {
        Port::Waiting(held) => held.push(line),
        Port::Running(run) => {
            if let Err(e) = run.give(line, counting) {
                counting.stopped(e);
                *port = match counting.census.panics.len() < AGAIN && ask.send(()).is_ok() {
                    true => Port::Asked,
                    false => Port::Off,
                };
            }
        }
        Port::Asked | Port::Again(_) | Port::Off => {}
    };
    // The port started: what it was held for given.
    let started = |run: crate::compare::PortRun, held: Vec<Value>, port: &mut Port, counting: &mut Counting| {
        *port = Port::Running(Box::new(run));
        for line in held {
            give(line, port, counting);
        }
    };
    let ended = loop {
        let made = match taken.recv_timeout(Duration::from_millis(250)) {
            Ok(made) => made,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                watched.look();
                status.show(progress, counting, &cycles, start_cycle);
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("the follower stopped".into()),
        };
        let tick = match made {
            Made::Tick(tick) => *tick,
            Made::State(given) => {
                if matches!(port, Port::Asked) {
                    port = Port::Again(given);
                }
                continue;
            }
            Made::End(ended) => break (*ended)?,
        };
        for (i, value) in &tick.inputs {
            match value {
                Some(value) => inputs.insert(*i, value.clone()),
                None => inputs.remove(i),
            };
        }
        if tick.tick == 0 {
            start_cycle = tick.cycle;
            give(header.clone(), &mut port, counting);
            if start.at_power_on()
                && let Port::Waiting(held) = std::mem::replace(&mut port, Port::Off)
            {
                started(start.spawn(Begin::PowerOn)?, held, &mut port, counting);
            }
        } else {
            let values = original.values(&tick)?;
            if tick.screen.is_some() {
                screen.clone_from(&tick.screen);
            }
            cycles.insert(tick.tick, tick.cycle);
            counting.original(At { tick: tick.tick, values, screen: screen.clone(), sound: tick.sound.clone() });
            ticks = tick.tick;
        }
        // A tick's line has the next tick's skipped hits (D130): it goes once that tick is made.
        if let Some(before) = previous.take() {
            let mut line = crate::compare::inputs_line(&before, Some(&tick), None, None);
            if let Some((_, values)) = whole.take_if(|(at, _)| *at == before.tick) {
                line["inputs"] = json!(values);
            }
            give(line, &mut port, counting);
        }
        // Started again (the tick before's line dropped) at the tick the follower's state was read at: from there, as from a transplant (D193).
        if let Port::Again(given) = &port
            && given.tick == tick.tick
        {
            let given = given.clone();
            let at = counting.at(tick.tick).ok_or("the tick the port starts again at")?;
            let run = start.spawn(Begin::Again(at, &given))?;
            counting.restart(given.tick);
            whole = Some((given.tick, inputs.iter().map(|(i, v)| json!([i, v])).collect()));
            status.clear();
            say("port", format!("started again at tick {}, from the follower's state there", given.tick));
            started(run, vec![header.clone()], &mut port, counting);
        }
        // From a session's tick: the port starts from the original's state at the first tick (D118), read once the
        // tick after it is made, as `rrc compare` reads it.
        if tick.tick == 2
            && !start.at_power_on()
            && let Port::Waiting(held) = std::mem::replace(&mut port, Port::Off)
        {
            let first = counting.first().ok_or("the play's first tick")?;
            started(start.spawn(Begin::First(first))?, held, &mut port, counting);
        }
        previous = Some(tick);
        for (kind, key, seen) in said.new_in(&counting.census) {
            let (kind, text) = Census::line(kind, key, seen, counting.census.passes);
            let when = cycles.get(&seen.tick).map(|c| clock((c - start_cycle) as f64 / ips as f64)).unwrap_or_default();
            status.clear();
            say("new", format!("at {when}: {kind} {text}"));
        }
        said = counting.census.clone();
        let keep = counting.compared();
        cycles.retain(|t, _| *t + 64 >= keep);
        watched.look();
        status.show(progress, counting, &cycles, start_cycle);
    };
    status.clear();
    if let Some(before) = previous.take() {
        give(crate::compare::inputs_line(&before, None, None, None), &mut port, counting);
    }
    give(json!({"end": ended.written.end, "ticks": ticks}), &mut port, counting);
    if let Port::Running(run) = port
        && let Err(e) = run.finish(counting)
    {
        counting.stopped(e);
    }
    for (kind, key, seen) in said.new_in(&counting.census) {
        let (kind, text) = Census::line(kind, key, seen, counting.census.passes);
        say("new", format!("at the end: {kind} {text}"));
    }
    Ok((ended, start_cycle))
}

/// One line saying where the play and the comparison are, rewritten each second on a terminal (the plan's section 5);
/// nothing elsewhere.
struct Status {
    terminal: bool,
    shown: Option<Instant>,
    ips: u64,
}

impl Status {
    fn new(ips: u64) -> Status {
        Status { terminal: std::io::stdout().is_terminal(), shown: None, ips }
    }

    fn show(
        &mut self,
        progress: &Progress,
        counting: &Counting,
        cycles: &std::collections::BTreeMap<u64, u64>,
        start: u64,
    ) {
        if !self.terminal || self.shown.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.shown = Some(Instant::now());
        let seconds = |cycles: u64| cycles as f64 / self.ips.max(1) as f64;
        let written = progress.written.load(Ordering::Relaxed);
        let now = progress.now.load(Ordering::Relaxed);
        let compared = counting.compared();
        let at = cycles.get(&compared).copied().unwrap_or(start);
        let what = if counting.census.differs() { "differences found" } else { "all matching" };
        // A long pass: no tick for a while (the plan's pitfall 11).
        let last = cycles.values().next_back().copied().unwrap_or(start);
        let pass = if seconds(now.saturating_sub(last)) > 5.0 {
            format!(" · in a pass: {} so far", clock(seconds(now - last)))
        } else {
            String::new()
        };
        let line = format!(
            "live      {} played · tick {compared} · {:.1} s behind · {what}{pass}",
            clock(seconds(written.saturating_sub(start))),
            seconds(written.saturating_sub(at))
        );
        print!("\r{line:<100}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }

    /// The status line wiped, before a line of its own.
    fn clear(&mut self) {
        if self.terminal && self.shown.take().is_some() {
            print!("\r{:<100}\r", "");
        }
    }
}

/// Seconds as m:ss.
pub(crate) fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
