//! A play followed as it goes (M6.5's step 5, D192): the person plays the original in Clockwork's window; a second
//! Clockwork, the follower, runs the play as its file is written (D189) and makes its ticks as the window's run made
//! them; each goes to the port, run free and corrected where it differs (D191), and to the comparer, which says what
//! differs as it is found. Nothing waits on the window: the follower, the port and the comparer keep a few ticks
//! between them at most, a slow one slowing those before it, and the follower and the port run below normal priority.
//! The port's own input handling is checked beside (D197): each tick's line gives it the play's input up to the tick
//! (`devices`), its `Live` says what it makes of it, and that is judged at the play's end (`rrc compare --live`'s).
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

/// More new things than this found at once are said in one line, each in the follow's log (D207).
const BURST: usize = 5;

/// How far the play is written and the follower has run (cycles), for the status line; and where the follower's time
/// went (µs): making ticks, and held while the comparer hadn't taken the ones before.
#[derive(Default)]
pub(crate) struct Progress {
    pub written: AtomicU64,
    pub now: AtomicU64,
    making: AtomicU64,
    held: AtomicU64,
}

/// Microseconds since `from`, added to `to`.
fn spent(to: &AtomicU64, from: Instant) {
    to.fetch_add(from.elapsed().as_micros() as u64, Ordering::Relaxed);
}

/// What the follower sends the comparer: a tick with the play's input given since the tick before, or its run's end.
enum Made {
    Tick(Box<Tick>, Vec<(u64, String)>),
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

/// A play followed: what the comparison found, the port's input handling judged, the follower's trace, the play's
/// speed and end.
pub(crate) struct Followed {
    pub census: Census,
    pub lived: crate::compare::Lived,
    pub written: Written,
    pub memory: Option<String>,
    pub hashes: u64,
    pub ips: u64,
    pub end: u64,
}

/// Follows the play of session `name` (`session`) to its end as its file is written by `window` (none: the file is
/// written, or as far as it will be), comparing the port with it as it goes; by another build of Clockwork than the
/// one that played it if `other_build` (D212).
pub(crate) fn follow(
    repo: &Repo,
    name: &str,
    session: &Session,
    mut window: Option<std::process::Child>,
    other_build: bool,
) -> Result<Followed, String> {
    let k = Knowledge::load(&repo.knowledge())?;
    let exe = crate::clockwork::find(&repo.local)?;
    let header = crate::trace::header(repo, &k, name, &exe, Vec::new())?;
    // Once the window is closed (or there is none), the play ends at its last mark, written to its end or not.
    let done = Arc::new(AtomicBool::new(window.is_none()));
    let mut follow = crate::trace::follow_play(repo, session, &exe, other_build)?;
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
    let start =
        crate::compare::PortStart::new(repo, session, name, &k, &mapping, &mapped, header.clone())?.checking_live();
    let sets = std::collections::BTreeMap::new();
    let census = Census { port: crate::impact::fingerprint(repo)?, ..Census::default() };
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
                recorder.until_play_ends(to_end.clone());
                let mut file = TraceFile::create(repo, recorder.header.clone())?;
                let stopped = |_| "the comparison stopped".to_owned();
                loop {
                    let making = Instant::now();
                    let Some(tick) = recorder.next()? else { break };
                    file.tick(&tick)?;
                    let came = recorder.played_until(tick.cycle);
                    spent(&to_end.making, making);
                    let held = Instant::now();
                    sent.send(Made::Tick(Box::new(tick), came)).map_err(stopped)?;
                    spent(&to_end.held, held);
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
        let log = Log::open(repo, name);
        let compared = compare(&taken, &ask, &mut original, &mut counting, &start, &progress, ips, watched, log);
        // The follower stops at its next tick once the comparison has stopped taking them.
        drop(taken);
        let _ = follower.join();
        compared
    });
    crate::clockwork::below_normal(false);
    let (ended, _, played) = followed?;
    let end = ended.end.ok_or("the play has no end")?;
    // The port's input handling, judged against the play's ticks (D197).
    let trace = crate::trace::Trace::parse(&String::from_utf8_lossy(&cache.get(&ended.written.hash)?))?;
    let lived = crate::compare::judge(&trace, &start.traces(), &played)?;
    Ok(Followed {
        census: counting.census,
        lived,
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
    /// Whether the window was closed: then the play ends at its last mark. True the once it closes.
    fn look(&mut self) -> bool {
        if let Some(window) = self.window.as_mut()
            && window.try_wait().ok().flatten().is_some()
        {
            self.done.store(true, Ordering::Relaxed);
            *self.window = None;
            return true;
        }
        false
    }
}

/// What a follow said and how it kept pace, kept beside the census (`.rrc/play/NAME/follow.log`, D207): each line said
/// with how far behind the play it was said, and every ten seconds where the time went. A follow cut short keeps it.
struct Log {
    file: Option<std::fs::File>,
    began: Instant,
    /// The last pace line: when, and the times then (the comparer's waits, its reading, the port; the follower's
    /// making and held).
    last: Instant,
    times: [u64; 7],
    waited: u64,
    reading: u64,
    port: u64,
}

impl Log {
    fn open(repo: &Repo, name: &str) -> Log {
        let file = repo.state().ok().map(|s| s.join("play").join(name)).and_then(|dir| {
            std::fs::create_dir_all(&dir).ok()?;
            std::fs::File::create(dir.join("follow.log")).ok()
        });
        let now = Instant::now();
        Log { file, began: now, last: now, times: [0; 7], waited: 0, reading: 0, port: 0 }
    }

    /// A line: its label, the seconds since the follow began, its text.
    fn line(&mut self, label: &str, text: &str) {
        use std::io::Write as _;
        if let Some(f) = self.file.as_mut() {
            let _ = writeln!(f, "{label:<9} {:>8.1} s  {text}", self.began.elapsed().as_secs_f64());
        }
    }

    /// Every ten seconds: how far the play is and the comparison, and each part's share of the time since the last.
    fn pace(&mut self, progress: &Progress, played: f64, compared: f64, tick: u64) {
        if self.last.elapsed() < Duration::from_secs(10) {
            return;
        }
        let now = [
            self.waited,
            self.reading,
            self.port,
            progress.making.load(Ordering::Relaxed),
            progress.held.load(Ordering::Relaxed),
            crate::compare::PORT_TAKING.load(Ordering::Relaxed),
            crate::compare::PORT_WAITING.load(Ordering::Relaxed),
        ];
        let wall = self.last.elapsed().as_micros().max(1) as f64;
        let share = |i: usize| 100.0 * (now[i] - self.times[i]) as f64 / wall;
        let text = format!(
            "played {} · compared {} (tick {tick}) · {:.1} s behind · the comparer: waiting for the follower {:.0}%, \
             reading {:.0}%, the port {:.0}% (its lines read and compared {:.0}%, waited for {:.0}%) · the follower:              making ticks {:.0}%, held {:.0}%",
            clock(played),
            clock(compared),
            (played - compared).max(0.0),
            share(0),
            share(1),
            share(2),
            share(5),
            share(6),
            share(3),
            share(4)
        );
        self.line("pace", &text);
        (self.times, self.last) = (now, Instant::now());
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

/// The comparer: the follower's ticks as they come, each to the comparer and as an inputs line to the port with the
/// play's input up to it (its devices, kept to judge the port's input handling by), what differs said when it is first
/// seen, a port that stops started again from the follower's state at its next tick (`ask`), and a status line on a
/// terminal.
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
    mut log: Log,
) -> Result<(Ended, u64, std::collections::BTreeMap<u64, Value>), String> {
    let mut status = Status::new(ips);
    let mut port = Port::Waiting(Vec::new());
    let mut previous: Option<(Tick, Vec<(u64, String)>)> = None;
    // The play's input not on a tick's line yet (the start-up's goes to the first tick's), and each tick's devices.
    let mut came: Vec<(u64, String)> = Vec::new();
    let mut played = std::collections::BTreeMap::new();
    let mut screen: Option<String> = None;
    let mut said = Census::default();
    let mut cycles = std::collections::BTreeMap::new();
    let mut start_cycle = 0;
    let mut ticks = 0;
    // Each input's value as the original's ticks so far left it; a port started again gets them all in its first line
    // (the ticks before brought its changes to a port that wasn't running): that tick, those values.
    let mut inputs: std::collections::BTreeMap<usize, String> = std::collections::BTreeMap::new();
    let mut whole: Option<(u64, Vec<Value>)> = None;
    let mut header = crate::compare::inputs_header(start.header());
    // Its lines have the devices (inputs version 7).
    header["version"] = json!(7);
    let devices = |line: &mut Value, tick: &Tick, came: &[(u64, String)], next: Option<u64>| {
        line["devices"] = crate::compare::devices(came, tick.cycle, next, ips);
        line["devices"].clone()
    };
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
    let seconds = |cycles: u64| cycles as f64 / ips.max(1) as f64;
    // The window closed: what is left to follow said, and that this terminal must stay open for it.
    let closed = |log: &mut Log,
                  status: &mut Status,
                  compared: u64,
                  cycles: &std::collections::BTreeMap<u64, u64>,
                  start: u64| {
        let written = progress.written.load(Ordering::Relaxed);
        let at = cycles.get(&compared).copied().unwrap_or(start);
        let text = format!(
            "the window is closed; the rest of the play followed now, {} behind: leave this window open until the \
             census (closed sooner, rrc play NAME --resume goes on)",
            clock(seconds(written.saturating_sub(at)))
        );
        status.clear();
        say("follow", &text);
        log.line("follow", &text);
    };
    let ended = loop {
        let waiting = Instant::now();
        let made = taken.recv_timeout(Duration::from_millis(250));
        log.waited += waiting.elapsed().as_micros() as u64;
        let made = match made {
            Ok(made) => made,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if watched.look() {
                    closed(&mut log, &mut status, counting.compared(), &cycles, start_cycle);
                }
                status.show(progress, counting, &cycles, start_cycle);
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("the follower stopped".into()),
        };
        let tick = match made {
            Made::Tick(tick, given) => {
                came.extend(given);
                *tick
            }
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
            let reading = Instant::now();
            let values = original.values(&tick)?;
            if tick.screen.is_some() {
                screen.clone_from(&tick.screen);
            }
            cycles.insert(tick.tick, tick.cycle);
            counting.original(At {
                tick: tick.tick,
                values,
                screen: screen.clone(),
                sound: tick.sound.clone(),
                sync: Some(tick.sync.clone()),
            });
            ticks = tick.tick;
            log.reading += reading.elapsed().as_micros() as u64;
        }
        // A tick's line has the next tick's skipped hits (D130) and time: it goes once that tick is made.
        if let Some((before, given)) = previous.take() {
            let mut line = crate::compare::inputs_line(&before, Some(&tick), None, None);
            if let Some((_, values)) = whole.take_if(|(at, _)| *at == before.tick) {
                line["inputs"] = json!(values);
            }
            if before.tick > 0 {
                played.insert(before.tick, devices(&mut line, &before, &given, Some(tick.cycle)));
            }
            let giving = Instant::now();
            give(line, &mut port, counting);
            log.port += giving.elapsed().as_micros() as u64;
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
        let given = if tick.tick > 0 { std::mem::take(&mut came) } else { Vec::new() };
        previous = Some((tick, given));
        let written = progress.written.load(Ordering::Relaxed);
        let when = |t: u64| cycles.get(&t).map(|c| clock(seconds(c - start_cycle))).unwrap_or_default();
        // The port past what it knows, or back (D207): said once; what differs meanwhile is counted, not said.
        let (before, now) = (said.lost.last(), counting.census.lost.last());
        if now != before
            && let Some(lost) = now
        {
            let (label, at) = match lost.to {
                None => ("lost", lost.from),
                Some(to) => ("back", to),
            };
            let text = format!("at {}: {}", when(at), lost.text(counting.name()));
            status.clear();
            say(label, &text);
            log.line(label, &text);
        }
        let lost = counting.is_lost();
        let new = said.new_in(&counting.census);
        // Many at once (the port off what the original does: a screen or a place it hasn't): one line for them, each in
        // the log (D207).
        let burst = new.len() > BURST;
        for (i, &(kind, key, seen)) in new.iter().enumerate() {
            let (kind, text) = Census::line(kind, key, seen, counting.census.passes);
            let at = cycles.get(&seen.tick).copied();
            // How late it is said: the play written since its tick.
            let behind = at.map_or(0.0, |c| seconds(written.saturating_sub(c)));
            let when = when(seen.tick);
            if lost {
                log.line("counted", &format!("at {when}, past what the port knows: {kind} {text}"));
                continue;
            }
            log.line("new", &format!("at {when}, said {behind:.1} s behind: {kind} {text}"));
            if burst && i > 0 {
                continue;
            }
            status.clear();
            say("new", format!("at {when}: {kind} {text}"));
            if burst {
                say("many", format!("at {when}: {} more things new at once, in follow.log", new.len() - 1));
            }
        }
        said = counting.census.clone();
        let keep = counting.compared();
        if let Some(&at) = cycles.get(&keep) {
            log.pace(progress, seconds(written.saturating_sub(start_cycle)), seconds(at - start_cycle), keep);
        }
        cycles.retain(|t, _| *t + 64 >= keep);
        if watched.look() {
            closed(&mut log, &mut status, keep, &cycles, start_cycle);
        }
        status.show(progress, counting, &cycles, start_cycle);
    };
    status.clear();
    if let Some((before, given)) = previous.take() {
        let mut line = crate::compare::inputs_line(&before, None, None, None);
        if before.tick > 0 {
            played.insert(before.tick, devices(&mut line, &before, &given, None));
        }
        give(line, &mut port, counting);
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
        log.line("new", &format!("at the end: {kind} {text}"));
    }
    log.line("end", &format!("{ticks} ticks followed"));
    Ok((ended, start_cycle, played))
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
