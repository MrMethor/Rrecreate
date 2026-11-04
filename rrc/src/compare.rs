//! `rrc compare SESSION` (DESIGN.md §7, §8, D61-D63): the port against the original, tick by tick.
//!
//! rrc writes the session's inputs (`rrc-inputs`) from the original's trace, runs the port (`[port]
//! command` in rrc.toml) with them, from its start state, a transplant of the original's mapped values
//! at a tick, or its own state kept at a paired checkpoint, and reads the port's trace back. Every
//! mapped field (`mapping.toml`) is compared at every tick, as values of the knowledge's types. At the
//! first tick where one differs it says which, with both values, and who wrote the original's value in
//! the pass before (a watch from the session's snapshots). Comparing needs no emulator: the original's
//! trace is in the cache (D26); only the who-wrote and a paired checkpoint's snapshot run Clockwork.
//!
//! A scenario session's edits (D64) reach the port with the inputs: at an edit's tick, the mapped
//! fields it wrote, set to the original's values there.

use std::collections::BTreeMap;
use std::process::{Command, ExitCode};

use ids::Id;
use serde_json::{Value, json};

use crate::args::Args;
use crate::cache::Cache;
use crate::knowledge::{Knowledge, Types};
use crate::mapping::{MAPPING, Mapped, Mapping};
use crate::places::Places;
use crate::repo::Repo;
use crate::say;
use crate::session::{Pair, Session};
use crate::trace::Trace;
use crate::values::{Reader, Target, at_path, decode, differences};
use rrc_runtime::present::Frame;

/// Fields shown of a divergence.
const SHOWN: usize = 8;

pub(crate) fn compare(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--transplant", "--resume", "--to", "--pair", "--feed"], &["--notes", "--lockstep"])?;
    let usage = "compare SESSION [--transplant TICK | --resume TICK] [--to TICK] [--pair TICK] [--feed FIELD,...] \
                 [--notes] | compare SESSION --lockstep";
    let [name] = a.positionals(1, 1, usage)? else { unreachable!() };
    if a.has("--lockstep") {
        let census = lockstep(repo, name)?;
        let file = repo.state()?.join("compare").join(name.as_str()).join("census.json");
        census.save(&file)?;
        census.show(name, 20);
        say("census", crate::path_text(&file));
        return Ok(if census.differs() { ExitCode::from(1) } else { ExitCode::SUCCESS });
    }
    let tick = |flag: &str| -> Result<Option<u64>, String> {
        a.value(flag)
            .map(|t| t.parse::<u64>().ok().filter(|&t| t > 0).ok_or(format!("{flag} is a tick number")))
            .transpose()
    };
    let (transplant, resume, to, pair) = (tick("--transplant")?, tick("--resume")?, tick("--to")?, tick("--pair")?);
    if transplant.is_some() && resume.is_some() {
        return Err("--transplant or --resume, not both".into());
    }
    let mut session = Session::load(repo, name)?;
    // A session from another's tick starts where the port's start isn't (D118).
    if let Some(from) = session.from.as_ref().filter(|_| transplant.is_none() && resume.is_none()) {
        return Err(format!(
            "{name} starts at {}'s tick {}: compare it from a transplant (--transplant 1) or a paired checkpoint",
            from.session, from.tick
        ));
    }
    let recorded = session.trace.clone().ok_or_else(|| format!("{name} isn't recorded: rrc record {name}"))?;
    let k = Knowledge::load(&repo.knowledge())?;
    let mapping = Mapping::load(repo)?;
    let mapped = mapping.resolve(&k)?;
    let presented = mapping.resolve_present(&k)?;
    if mapped.is_empty() {
        return Err(format!(
            "nothing is mapped: {MAPPING} names the port's fields and the original's items (docs/formats.md)"
        ));
    }
    let cache = Cache::open(&repo.local)?;
    let trace = Trace::parse(&String::from_utf8_lossy(&cache.get(&recorded.hash)?))?;
    let original = {
        // A far pointer's target is named where the session put the program (D72).
        let places = Places::new(repo, &session);
        let linear = |linear: u32| places.linear_id(linear).ok();
        original_values(&trace, &mapped, &crate::values::of(&k).with_linear(&linear), name)?
    };
    let ticks = trace.ticks.len() as u64;
    for t in [transplant, resume, to, pair].into_iter().flatten() {
        if t > ticks {
            return Err(format!("{name} has {ticks} ticks, not {t}"));
        }
    }

    let work = repo.state()?.join("compare").join(name.as_str());
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let file = |f: &str| work.join(f).to_string_lossy().into_owned();
    let after = transplant.or(resume).unwrap_or(0);
    let sets = edit_sets(&trace, &mapped, &original, &k.types, after)?;
    let mut feeds = BTreeMap::new();
    let fed = match a.value("--feed") {
        Some(fields) => feed(&mut feeds, &mapped, &original, fields, after)?,
        None => Vec::new(),
    };
    // What each feed changed: ticks the port's own value differed, the first, and where (D114).
    let mut overrode: BTreeMap<String, (u64, u64, String)> = BTreeMap::new();
    // What the port noted (D115): in all, the first tick and how many ticks; and in the pass to the last tick.
    let mut noted: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut noted_last: Vec<(String, u64)> = Vec::new();
    write_inputs(&trace, &sets, &feeds, &file("inputs.jsonl"))?;
    let mut port_args =
        vec!["run".to_owned(), "--inputs".into(), file("inputs.jsonl"), "--out".into(), file("port.jsonl")];
    if let Some(t) = transplant {
        let carried = read_given(repo, &session, &k, &mapping.resolve_carried(&k)?, t)?;
        write_start(&trace.header.session, &mapped, &original, t, carried, &file("start.json"))?;
        port_args.extend(["--transplant".into(), file("start.json")]);
    }
    if let Some(t) = resume {
        let kept = session.pairs.iter().find(|p| p.tick == t).ok_or_else(|| {
            let ticks: Vec<String> = session.pairs.iter().map(|p| p.tick.to_string()).collect();
            format!(
                "{name} has no paired checkpoint at tick {t} (it has: {})",
                if ticks.is_empty() { "none".into() } else { ticks.join(", ") }
            )
        })?;
        std::fs::write(file("resume.json"), cache.get(&kept.port)?).map_err(|e| e.to_string())?;
        port_args.extend(["--resume".into(), file("resume.json")]);
    }
    // The presentation's own state there (D110), from the original's memory at that tick.
    if let Some(t) = transplant.or(resume)
        && !presented.is_empty()
    {
        let at = (&trace.header.session[..], t);
        write_presentation(repo, &session, &k, &presented, at, &file("presentation.json"))?;
        port_args.extend(["--presentation".into(), file("presentation.json")]);
        say("given", format!("the presentation's own state at tick {t}: {} fields (D110)", presented.len()));
    }
    if let Some(t) = pair {
        let _ = std::fs::remove_file(file("pair.json"));
        port_args.extend(["--save-at".into(), t.to_string(), "--save".into(), file("pair.json")]);
    }
    if let Some(t) = to {
        port_args.extend(["--to".into(), t.to_string()]);
    }
    // The original's screen at each tick (D107): the port is asked for its frames when the trace has it.
    let screens = screens(&trace);
    if screens.iter().any(Option::is_some) {
        let frames = work.join("frames");
        if frames.is_dir() {
            std::fs::remove_dir_all(&frames).map_err(|e| format!("{}: {e}", frames.display()))?;
        }
        port_args.extend(["--frames".into(), file("frames")]);
        if let Some(hash) = transplant.or(resume).and_then(|t| screens[(t - 1) as usize].as_ref()) {
            std::fs::write(file("screen.frm"), cache.get(hash)?).map_err(|e| e.to_string())?;
            port_args.extend(["--screen".into(), file("screen.frm")]);
        }
    }
    run_port(repo, &port_args)?;

    // Tick by tick, the port's state against the original's.
    let port_trace = std::fs::read_to_string(file("port.jsonl")).map_err(|e| format!("the port's trace: {e}"))?;
    let mut lines = port_trace.lines().filter(|l| !l.trim().is_empty());
    let header: Value =
        serde_json::from_str(lines.next().ok_or("the port wrote an empty trace")?).map_err(|e| e.to_string())?;
    if header["format"] != "rrc-port-trace" || header["version"] != 1 {
        return Err(format!(
            "the port's trace isn't rrc-port-trace version 1 ({} {})",
            header["format"], header["version"]
        ));
    }
    let mut state = json!({});
    let (mut first, mut last, mut diverged) = (None, 0, None);
    // The port's frame and the original's screen, decoded as they change.
    let mut seen = Screens::default();
    let mut drawn: Option<(u64, ScreenDiff)> = None;
    // The port's sound, if it says it makes it (D108): every device's, or those it names (D121).
    let made: Vec<&str> = match &header["sound"] {
        Value::Bool(true) => SOUND_DEVICES.iter().map(|(d, _)| *d).collect(),
        Value::Array(devices) => devices.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let sounding = !made.is_empty();
    let mut heard: Option<(u64, SoundDiff)> = None;
    // The fields the port left unset (null), by the ticks they were (D102).
    let mut unset: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for line in lines {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("the port's trace: {e}"))?;
        let Some(t) = v["tick"].as_u64() else { continue };
        for (key, value) in v["state"].as_object().into_iter().flatten() {
            state[key] = value.clone();
        }
        noted_last.clear();
        for (what, n) in v["notes"].as_object().into_iter().flatten() {
            noted.entry(what.clone()).or_insert((t, 0)).1 += 1;
            noted_last.push((what.clone(), n.as_u64().unwrap_or(0)));
        }
        for (path, own) in v["overrode"].as_object().into_iter().flatten() {
            let Some(i) = mapped.iter().position(|m| m.field == *path && fed.contains(path)) else { continue };
            let (m, theirs) = (&mapped[i], &original[i][(t.max(1) - 1) as usize]);
            let mut differ = Vec::new();
            differences(&m.field, theirs, &m.masked(own), 1, &mut differ);
            if let Some((at, _, _)) = differ.first() {
                overrode.entry(path.clone()).or_insert((0, t, at.clone())).0 += 1;
            }
        }
        if let Some(frame) = v["frame"].as_str() {
            seen.port(&work.join("frames").join(frame))?;
        }
        if t == 0 || t > ticks {
            return Err(format!("the port's trace has tick {t}; the session has 1 to {ticks}"));
        }
        let start = *first.get_or_insert(t);
        last = t;
        let mut found = Vec::new();
        for (m, values) in mapped.iter().zip(&original) {
            let theirs = &values[(t - 1) as usize];
            match at_path(&state, &m.field) {
                Some(Value::Null) => {
                    if unset_matches(&m.field, &values[(start - 1) as usize], theirs, &mut found) {
                        unset.entry(m.field.as_str()).or_insert((t, t)).1 = t;
                    }
                }
                Some(ours) => differences(&m.field, theirs, &m.masked(ours), SHOWN, &mut found),
                None => found.push((m.field.clone(), theirs.clone(), Value::Null)),
            }
        }
        if !found.is_empty() {
            diverged = Some((t, found));
            break;
        }
        if let Some(hash) = &screens[(t - 1) as usize]
            && let Some(diff) = seen.compare(hash, &cache)?
        {
            drawn = Some((t, diff));
            break;
        }
        // From a transplant or a resume the pass to the first tick was the original's: its sound isn't compared.
        let own_pass = after == 0 || t > after;
        if sounding
            && own_pass
            && let Some(diff) = sound_differences(&trace.ticks[(t - 1) as usize].sound, &v["sound"], &made)
        {
            heard = Some((t, diff));
            break;
        }
    }
    let first = first.ok_or("the port ran no tick")?;
    let bytes: u32 = mapped.iter().map(|m| m.size).sum();
    say("compare", format!("{name}, ticks {first}-{last} of {ticks}: {} fields mapped ({bytes} bytes)", mapped.len()));
    // What each feed did (D114): one the port's own never needed can go; one doing work hides a difference.
    for field in &fed {
        let ran = last.saturating_sub(first);
        match overrode.get(field) {
            None => say(
                "fed",
                format!("{field}: the port's own was the original's at all {ran} ticks fed: the feed can go"),
            ),
            Some((n, at, path)) => say(
                "fed",
                format!("{field}: the port's own differed at {n} of {ran} ticks fed, first at tick {at} ({path})"),
            ),
        }
    }
    for (field, (from, to)) in &unset {
        let Some(m) = mapped.iter().find(|m| m.field == *field) else { continue };
        let writer = last_writer(repo, name, &trace, &k, m, first)?;
        say("unset", format!("{field} at ticks {from}-{to}: the original's held its value of tick {first}, {writer}"));
    }
    // What this found, for `rrc progress` (D117): the passes that match, to the last tick that does.
    let (matched, differs) = match (&diverged, &drawn, &heard) {
        (Some((t, _)), _, _) => (t - 1, Some("state")),
        (None, Some((t, _)), _) => (t - 1, Some("screen")),
        (None, None, Some((t, _))) => (t - 1, Some("sound")),
        (None, None, None) => (last, None),
    };
    let outcome = crate::progress::Outcome {
        recorded: recorded.hash.clone(),
        from: after,
        first,
        last,
        matched: matched.max(after),
        ticks,
        fed: fed.clone(),
        differs: differs.map(str::to_owned),
    };
    outcome.save(&work)?;
    let code = match diverged {
        None if heard.is_some() => {
            let (t, diff) = heard.as_ref().expect("a sound that differs");
            if *t > first {
                say("match", format!("every mapped field at all {} ticks", t - first + 1));
            }
            report_sound(repo, name, &trace, *t, diff)?;
            ExitCode::from(1)
        }
        None if drawn.is_some() => {
            let (t, diff) = drawn.as_ref().expect("a screen that differs");
            if *t > first {
                say("match", format!("every mapped field at all {} ticks", t - first + 1));
            }
            report_screen(repo, name, &trace, &k, *t, diff, &file(&format!("screen-{t}.png")))?;
            ExitCode::from(1)
        }
        None => {
            say("match", format!("every mapped field at all {} ticks", last - first + 1));
            match seen.compared {
                0 if screens.iter().any(Option::is_some) => say("screen", "not compared: the port draws no frames"),
                0 => {}
                _ => say(
                    "screen",
                    format!("the port's frames are the original's screen at all {} ticks", last - first + 1),
                ),
            }
            if sounding {
                let unmade: Vec<&str> = SOUND_DEVICES.iter().map(|(d, _)| *d).filter(|d| !made.contains(d)).collect();
                let whose =
                    if unmade.is_empty() { "the port's".to_owned() } else { format!("the port's {}", made.join(", ")) };
                say("sound", format!("{whose} sound is the original's at all {} ticks", last - first + 1));
                if !unmade.is_empty() {
                    say("unsounded", format!("{}: the port makes none yet, not compared", unmade.join(", ")));
                }
            }
            ExitCode::SUCCESS
        }
        Some((t, found)) => {
            report(repo, name, &trace, &mapped, &k, t, first, transplant.is_some(), &found)?;
            ExitCode::from(1)
        }
    };
    // What the port noted in the pass to where it differs, and in all (D115).
    if code != ExitCode::SUCCESS {
        noted_last.sort_by_key(|(what, n)| (std::cmp::Reverse(*n), what.clone()));
        for (what, n) in noted_last.iter().take(NOTES) {
            say(
                "noted",
                format!(
                    "in the pass to tick {last}: {what}{}",
                    if *n > 1 { format!(" ({n} times)") } else { String::new() }
                ),
            );
        }
        if noted_last.len() > NOTES {
            say("noted", format!("{} more in that pass (--notes lists every note)", noted_last.len() - NOTES));
        }
    }
    if a.has("--notes") {
        let mut all: Vec<(&String, &(u64, u64))> = noted.iter().collect();
        all.sort_by_key(|(what, (first, _))| (*first, (*what).clone()));
        for (what, (from, ticks)) in all {
            say("note", format!("{what}: first in the pass to tick {from}, in {ticks} ticks"));
        }
    } else if !noted.is_empty() {
        say("notes", format!("{} things the port noted in these ticks (--notes lists them)", noted.len()));
    }
    if let Some(t) = pair {
        keep_pair(repo, &mut session, name, &k, t, &file("pair.json"), &cache)?;
    }
    unmapped(&k, &mapped, repo)?;
    Ok(code)
}

/// The devices whose sound is compared (D108), and the I/O ports that make it.
const SOUND_DEVICES: [(&str, &str); 4] =
    [("opl", "388,389"), ("speaker", "42,43,61"), ("pcm", ""), ("midi", "330,331")];

/// Where a tick's sound differs: the device, the first event that differs (its index, the original's, the
/// port's), and how many each made.
#[derive(Debug, Clone, PartialEq)]
struct SoundDiff {
    device: &'static str,
    at: usize,
    original: Option<Value>,
    port: Option<Value>,
    counts: (usize, usize),
}

/// The original's sound events of a tick as a port makes them, without their times (D108): an OPL write
/// `[register, value]`, the speaker `{"steady": on}`, `{"tone": [period, high]}` or `{"one_shot": true}`, a
/// digitized sound `[rate, stereo]`, a MIDI byte.
fn untimed(device: &str, events: Option<&Value>) -> Vec<Value> {
    let events = events.and_then(Value::as_array).cloned().unwrap_or_default();
    events
        .iter()
        .map(|e| match device {
            "opl" | "pcm" => json!([e[1], e[2]]),
            "midi" => e[1].clone(),
            "speaker" => {
                let wave = &e[1];
                if let Some(on) = wave.get("Steady") {
                    json!({ "steady": on })
                } else if let Some(p) = wave.get("Periodic") {
                    json!({ "tone": [p["period"], p["high"]] })
                } else {
                    json!({ "one_shot": true })
                }
            }
            _ => e.clone(),
        })
        .collect()
}

/// An untimed sound event as `rrc trace` shows one: `B0=35`, `tone 2281/1141`, `steady off`, `pcm 11025`.
fn untimed_text(device: &str, event: &Value) -> String {
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    match device {
        "opl" => format!("{:X}={:02X}", n(&event[0]), n(&event[1])),
        "speaker" => match (event.get("steady"), event.get("tone")) {
            (Some(on), _) => format!("steady {}", if on.as_bool() == Some(true) { "on" } else { "off" }),
            (_, Some(t)) => format!("tone {}/{}", n(&t[0]), n(&t[1])),
            _ => "one-shot".to_owned(),
        },
        "pcm" => format!("pcm {}{}", n(&event[0]), if event[1].as_bool() == Some(true) { " stereo" } else { "" }),
        "midi" => format!("{:02X}", n(event)),
        _ => event.to_string(),
    }
}

/// How a tick's sound from the port differs from the original's, if it does.
fn sound_differences(original: &serde_json::Map<String, Value>, port: &Value, made: &[&str]) -> Option<SoundDiff> {
    for &(device, _) in SOUND_DEVICES.iter().filter(|(d, _)| made.contains(d)) {
        let theirs = untimed(device, original.get(device));
        let ours = port.get(device).and_then(Value::as_array).cloned().unwrap_or_default();
        if theirs != ours {
            let at = theirs.iter().zip(&ours).take_while(|(a, b)| a == b).count();
            return Some(SoundDiff {
                device,
                at,
                original: theirs.get(at).cloned(),
                port: ours.get(at).cloned(),
                counts: (theirs.len(), ours.len()),
            });
        }
    }
    None
}

/// A tick whose sound differs: the device, its first event that differs, and who wrote the device's ports
/// in the pass to it.
fn report_sound(repo: &Repo, name: &str, trace: &Trace, t: u64, diff: &SoundDiff) -> Result<(), String> {
    let tick = &trace.ticks[(t - 1) as usize];
    let (theirs, ours) = diff.counts;
    say(
        "diverges",
        format!("at tick {t} ({}): the {} sound: {theirs} events, the port's {ours}", tick.sync, diff.device),
    );
    let text = |e: &Option<Value>| e.as_ref().map_or_else(|| "none".to_owned(), |e| untimed_text(diff.device, e));
    say("  event", format!("{}: original {}, port {}", diff.at + 1, text(&diff.original), text(&diff.port)));
    let ports = SOUND_DEVICES.iter().find(|(d, _)| *d == diff.device).map_or("", |(_, p)| p);
    if !ports.is_empty() {
        say("wrote", format!("its ports ({ports}), between the sync points of ticks {} and {t}:", t.saturating_sub(1)));
        let args: Vec<String> =
            [name, ports, "--tick", &t.to_string(), "--ticks", "1"].iter().map(|s| s.to_string()).collect();
        crate::watch::ports(repo, &args)?;
    }
    Ok(())
}

/// The original's screen at each tick (D107): the last one the trace kept by then, its picture's hash.
fn screens(trace: &Trace) -> Vec<Option<String>> {
    let mut last = None;
    trace
        .ticks
        .iter()
        .map(|t| {
            if t.screen.is_some() {
                last.clone_from(&t.screen);
            }
            last.clone()
        })
        .collect()
}

/// Where a port's frame and the original's screen differ: the pixels (how many, their box, the first, its
/// index in each), the colours (index, the original's, the port's), or the size.
#[derive(Debug, Clone, PartialEq)]
struct ScreenDiff {
    pixels: usize,
    area: (usize, usize, usize, usize),
    /// The differing pixels' areas (D113), the largest first.
    areas: Vec<Area>,
    first: Option<(usize, usize, u8, u8)>,
    colours: Vec<(usize, [u8; 3], [u8; 3])>,
    sizes: Option<((usize, usize), (usize, usize))>,
    original: Frame,
    port: Frame,
}

/// The two sides' pictures, decoded once each as they change.
#[derive(Default)]
struct Screens {
    port: Option<Frame>,
    original: Option<(String, Frame)>,
    /// How many ticks were compared.
    compared: u64,
}

impl Screens {
    fn port(&mut self, file: &std::path::Path) -> Result<(), String> {
        let bytes = std::fs::read(file).map_err(|e| format!("the port's frame {}: {e}", file.display()))?;
        self.port = Some(Frame::decode(&bytes).map_err(|e| format!("the port's frame {}: {e}", file.display()))?);
        Ok(())
    }

    /// The port's frame against the original's screen `hash`; none when the port draws nothing.
    fn compare(&mut self, hash: &str, cache: &Cache) -> Result<Option<ScreenDiff>, String> {
        let Some(port) = &self.port else { return Ok(None) };
        if self.original.as_ref().is_none_or(|(h, _)| h != hash) {
            let frame = Frame::decode(&cache.get(hash)?)
                .map_err(|e| format!("the original's screen: {e} (only indexed modes are compared)"))?;
            self.original = Some((hash.to_owned(), frame));
        }
        self.compared += 1;
        let (_, original) = self.original.as_ref().expect("decoded");
        Ok(screen_differences(original, port))
    }
}

/// An area of differing pixels, each touching another (diagonally too): its box (x0, y0, x1, y1), how many,
/// and its first (top row, then leftmost).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Area {
    bounds: (usize, usize, usize, usize),
    pixels: usize,
    first: (usize, usize),
}

/// The areas the differing pixels make (`differs`, row by row, `width` a row), the largest first.
fn areas(differs: &[bool], width: usize) -> Vec<Area> {
    let mut seen = vec![false; differs.len()];
    let mut found = Vec::new();
    for start in 0..differs.len() {
        if !differs[start] || seen[start] {
            continue;
        }
        let first = (start % width, start / width);
        let mut area = Area { bounds: (first.0, first.1, first.0, first.1), pixels: 0, first };
        seen[start] = true;
        let mut todo = vec![start];
        while let Some(i) = todo.pop() {
            let (x, y) = (i % width, i / width);
            area.pixels += 1;
            let (x0, y0, x1, y1) = area.bounds;
            area.bounds = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            for (dx, dy) in [(-1i64, -1i64), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx < 0 || ny < 0 || nx >= width as i64 {
                    continue;
                }
                let j = ny as usize * width + nx as usize;
                if j < differs.len() && differs[j] && !seen[j] {
                    seen[j] = true;
                    todo.push(j);
                }
            }
        }
        found.push(area);
    }
    found.sort_by_key(|a| (std::cmp::Reverse(a.pixels), a.first.1, a.first.0));
    found
}

/// Areas shown at most.
const AREAS: usize = 4;

/// Notes shown at most for the pass that differs.
const NOTES: usize = 6;

/// How `port` differs from `original`, if it does.
fn screen_differences(original: &Frame, port: &Frame) -> Option<ScreenDiff> {
    let mut diff = ScreenDiff {
        pixels: 0,
        area: (usize::MAX, usize::MAX, 0, 0),
        areas: Vec::new(),
        first: None,
        colours: Vec::new(),
        sizes: None,
        original: original.clone(),
        port: port.clone(),
    };
    if (original.width, original.height) != (port.width, port.height) {
        diff.sizes = Some(((original.width, original.height), (port.width, port.height)));
        return Some(diff);
    }
    for (i, (a, b)) in original.pixels.iter().zip(&port.pixels).enumerate() {
        if a != b {
            let (x, y) = (i % original.width, i / original.width);
            diff.pixels += 1;
            diff.first.get_or_insert((x, y, *a, *b));
            let (x0, y0, x1, y1) = diff.area;
            diff.area = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
    }
    if diff.pixels > 0 {
        let differs: Vec<bool> = original.pixels.iter().zip(&port.pixels).map(|(a, b)| a != b).collect();
        diff.areas = areas(&differs, original.width);
    }
    for (i, (a, b)) in original.palette.chunks(3).zip(port.palette.chunks(3)).enumerate() {
        if a != b {
            diff.colours.push((i, [a[0], a[1], a[2]], [b[0], b[1], b[2]]));
        }
    }
    (diff.pixels > 0 || !diff.colours.is_empty()).then_some(diff)
}

/// A screen that differs at tick `t`: where, a picture of both and their differences, and who drew the
/// original's first differing pixel (mode 13h: its byte at MEM+A0000 + y*320 + x) in the pass to it, with the port's
/// code for that function (D73).
fn report_screen(
    repo: &Repo,
    name: &str,
    trace: &Trace,
    k: &Knowledge,
    t: u64,
    diff: &ScreenDiff,
    png: &str,
) -> Result<(), String> {
    let tick = &trace.ticks[(t - 1) as usize];
    if let Some(((w0, h0), (w1, h1))) = diff.sizes {
        say(
            "diverges",
            format!("at tick {t} ({}): the screen is {w0} × {h0}, the port's frame {w1} × {h1}", tick.sync),
        );
        return Ok(());
    }
    let (x0, y0, x1, y1) = diff.area;
    let pixels = if diff.pixels == 0 {
        "the same pixels".to_owned()
    } else {
        format!("{} pixels differ in ({x0}, {y0})-({x1}, {y1})", diff.pixels)
    };
    let colours = match diff.colours.len() {
        0 => String::new(),
        n => format!(", {n} colour{}", if n == 1 { "" } else { "s" }),
    };
    say("diverges", format!("at tick {t} ({}): the screen: {pixels}{colours}", tick.sync));
    if let Some((x, y, a, b)) = diff.first {
        say("  pixel", format!("({x}, {y}): original {a}, port {b}"));
    }
    // Its areas, when there is more than one (D113).
    if diff.areas.len() > 1 {
        for a in diff.areas.iter().take(AREAS) {
            let (x0, y0, x1, y1) = a.bounds;
            let pixels = if a.pixels == 1 { "1 pixel".to_owned() } else { format!("{} pixels", a.pixels) };
            say("  area", format!("({x0}, {y0})-({x1}, {y1}): {pixels}, first ({}, {})", a.first.0, a.first.1));
        }
        if diff.areas.len() > AREAS {
            say("  areas", format!("{} more, {} in all", diff.areas.len() - AREAS, diff.areas.len()));
        }
    }
    for (i, a, b) in diff.colours.iter().take(4) {
        say("  colour", format!("{i}: original {a:?}, port {b:?}"));
    }
    std::fs::write(png, side_by_side(diff).png()).map_err(|e| format!("{png}: {e}"))?;
    say(
        "png",
        format!("{} (the original's, the port's, where they differ)", crate::path_text(std::path::Path::new(png))),
    );
    let Some((x, y, _, _)) = diff.first else { return Ok(()) };
    // The video mode then: the last frame shown by the tick's end.
    let mode = trace.ticks[..t as usize].iter().flat_map(|t| &t.frames).filter_map(|f| f[2].as_u64()).next_back();
    if mode != Some(0x13) {
        say("wrote", "who drew it is found in mode 13h (one byte a pixel from MEM+A0000) only");
        return Ok(());
    }
    let at = format!("MEM+{:05X}", 0xA_0000 + y * 320 + x);
    say("wrote", format!("({x}, {y}) ({at}), between the sync points of ticks {} and {t}:", t.saturating_sub(1)));
    let args: Vec<String> = [name, &at, "--len", "1", "--tick", &t.to_string(), "--ticks", "1", "--lines", "4"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let writer = crate::watch::watched(repo, &args)?;
    // A pixel copied from a buffer (a view drawn off the screen) was decided where the buffer's was drawn.
    let writer = through_copies(repo, name, k, writer, 1, t)?;
    port_code(repo, k, &writer.chain);
    Ok(())
}

/// The original's screen, the port's frame, and the original's with the pixels that differ in red, side by
/// side.
fn side_by_side(diff: &ScreenDiff) -> crate::frames::Picture {
    let (w, h) = (diff.original.width, diff.original.height);
    let colour = |f: &Frame, p: u8| -> [u8; 3] {
        let c = &f.palette[3 * usize::from(p)..3 * usize::from(p) + 3];
        [(c[0] << 2) | (c[0] >> 4), (c[1] << 2) | (c[1] >> 4), (c[2] << 2) | (c[2] >> 4)]
    };
    let mut rgb = Vec::with_capacity(w * 3 * h * 3);
    for y in 0..h {
        for x in 0..w {
            rgb.extend(colour(&diff.original, diff.original.pixels[y * w + x]));
        }
        for x in 0..w {
            rgb.extend(colour(&diff.port, diff.port.pixels[y * w + x]));
        }
        for x in 0..w {
            let (a, b) = (diff.original.pixels[y * w + x], diff.port.pixels[y * w + x]);
            let [r, g, b2] = colour(&diff.original, a);
            rgb.extend(if a == b { [r / 3, g / 3, b2 / 3] } else { [255, 0, 0] });
        }
    }
    crate::frames::Picture { width: w * 3, height: h, rgb }
}

/// A field the port left unset (null) is what the original's memory held before the program set it: it
/// matches while the original's value is `leftovers`, its value at the first tick (D102). Where that
/// changed, the port has nothing.
fn unset_matches(path: &str, leftovers: &Value, theirs: &Value, found: &mut Vec<(String, Value, Value)>) -> bool {
    let mut changed = Vec::new();
    differences(path, leftovers, theirs, SHOWN.saturating_sub(found.len()).max(1), &mut changed);
    let matches = changed.is_empty();
    found.extend(changed.into_iter().map(|(path, _, theirs)| (path, theirs, Value::Null)));
    matches
}

/// Each mapped field's value at each tick of the original's trace.
fn original_values(trace: &Trace, mapped: &[Mapped], r: &Reader, name: &str) -> Result<Vec<Vec<Value>>, String> {
    let mut all = Vec::new();
    for m in mapped {
        let at = m.id.to_string();
        let column = trace
            .header
            .state
            .iter()
            .position(|item| match &m.via {
                Some(via) => {
                    item.via.as_ref().is_some_and(|v| v.at == at && v.offset == via.offset)
                        && item.name == via.item_name()
                        && item.size == m.size
                }
                None => item.via.is_none() && item.at == at && item.size == m.size,
            })
            .ok_or_else(|| format!("the trace of {name} lacks {} ({at}): rrc record {name}", m.field))?;
        let mut last: Option<(Option<String>, Value)> = None;
        let mut values = Vec::with_capacity(trace.ticks.len());
        for hex in trace.values(column) {
            let value = match &last {
                Some((h, v)) if *h == hex => v.clone(),
                _ => match &hex {
                    Some(h) => decoded(m, &unhex(h)?, r).map_err(|e| format!("{}: {e}", m.field))?,
                    None => Value::Null,
                },
            };
            last = Some((hex, value.clone()));
            values.push(value);
        }
        all.push(values);
    }
    // An index into an array on the heap (D122): where the field's pointer points, as an element of where the
    // array's pointer points then.
    for (i, m) in mapped.iter().enumerate() {
        let Some((array, width, count)) = &m.into else { continue };
        let a = mapped.iter().find(|a| a.field == *array).ok_or_else(|| format!("{}: into {array}", m.field))?;
        let via = a.via.as_ref().ok_or_else(|| format!("{}: into {array}, not through a pointer", m.field))?;
        let kind = crate::values::PointerKind::of_type(&m.ty, r.types)
            .ok_or_else(|| format!("{}: {} isn't a pointer", m.field, m.ty))??;
        let column = |at: &str, size: u32| {
            trace.header.state.iter().position(|item| item.via.is_none() && item.at == at && item.size == size)
        };
        let lacks = || format!("the trace of {name} lacks {}'s pointers: rrc record {name}", m.field);
        let own = column(&m.id.to_string(), m.size).ok_or_else(lacks)?;
        let base = column(&a.id.to_string(), via.pointer.size()).ok_or_else(lacks)?;
        let index = |p: &Option<String>, b: &Option<String>| -> Value {
            let target = |hex: &Option<String>, kind: &crate::values::PointerKind, past: u32| {
                hex.as_deref().and_then(|h| unhex(h).ok()).and_then(|bytes| kind.target(&bytes, past))
            };
            let apart = match (target(p, &kind, 0), target(b, &via.pointer, via.offset)) {
                (Some(Target::Linear(p)), Some(Target::Linear(b))) => p.checked_sub(b),
                (Some(Target::Id(p)), Some(Target::Id(b))) if p.module() == b.module() && p.part() == b.part() => {
                    p.offset().checked_sub(b.offset())
                }
                _ => None,
            };
            match apart.filter(|d| d % width == 0 && d / width < *count) {
                Some(d) => Value::from(d / width),
                None => Value::Null,
            }
        };
        all[i] = trace.values(own).iter().zip(&trace.values(base)).map(|(p, b)| index(p, b)).collect();
    }
    Ok(all)
}

/// A field's value from its item's bytes: as its type reads them (its mask's bits), or a list's nodes, one
/// after another, as an array (D122).
fn decoded(m: &Mapped, bytes: &[u8], r: &Reader) -> Result<Value, String> {
    if m.via.as_ref().is_some_and(|v| v.next.is_some()) {
        let nodes =
            bytes.chunks_exact(m.size as usize).map(|node| decode(node, &m.ty, r)).collect::<Result<Vec<_>, _>>()?;
        return Ok(Value::Array(nodes));
    }
    Ok(m.masked(&decode(bytes, &m.ty, r)?))
}

/// The fields the port gets set by the scenario's edits after tick `after` (D64), by tick: the mapped
/// leaves each edit wrote, with the original's values at its tick. An edit of memory nothing maps is
/// said: the port can't get it.
fn edit_sets(
    trace: &Trace,
    mapped: &[Mapped],
    original: &[Vec<Value>],
    types: &Types,
    after: u64,
) -> Result<BTreeMap<u64, serde_json::Map<String, Value>>, String> {
    let mut sets: BTreeMap<u64, serde_json::Map<String, Value>> = BTreeMap::new();
    for edit in &trace.header.edits {
        let (Some(tick), Some(at), Some(hex)) = (edit["tick"].as_u64(), edit["at"].as_str(), edit["hex"].as_str())
        else {
            continue;
        };
        if tick <= after {
            continue;
        }
        let id = crate::knowledge::parse_id(at)?;
        let (start, end) = (id.offset(), id.offset() + hex.len() as u32 / 2);
        let mut covered = 0;
        for (m, values) in mapped.iter().zip(original) {
            // An edit is at an ID: an item through a pointer is the pointer's target, elsewhere.
            if m.via.is_some() || (m.id.module(), m.id.part()) != (id.module(), id.part()) {
                continue;
            }
            let (mut offset, to) = (start.max(m.id.offset()), end.min(m.id.offset() + m.size));
            while offset < to {
                let Some((path, leaf, inner)) = crate::values::place(&m.ty, offset - m.id.offset(), types) else {
                    break;
                };
                let width = crate::values::size(&leaf, types)? - inner;
                let value = crate::values::at_leaf(&values[(tick - 1) as usize], &path).cloned().unwrap_or_default();
                sets.entry(tick).or_default().insert(format!("{}{path}", m.field), value);
                covered += width.min(to - offset);
                offset += width;
            }
        }
        if covered < end - start {
            say(
                "edit",
                format!(
                    "tick {tick}: {}: memory nothing maps, which the port can't get",
                    crate::trace::edit_text(edit)
                ),
            );
        }
    }
    Ok(sets)
}

/// Feeds the port the original's values of `fields` (mapped fields, or the groups they are in: `random`
/// for `random.seed`) at every tick after `after`, before it steps (D84): a subsystem not ported yet
/// gives the one being ported what it reads, and matches by construction. The mapped fields fed.
fn feed(
    feeds: &mut BTreeMap<u64, serde_json::Map<String, Value>>,
    mapped: &[Mapped],
    original: &[Vec<Value>],
    fields: &str,
    after: u64,
) -> Result<Vec<String>, String> {
    let wanted: Vec<&str> = fields.split(',').map(str::trim).filter(|f| !f.is_empty()).collect();
    let fed =
        |field: &str| wanted.iter().any(|w| field == *w || field.strip_prefix(*w).is_some_and(|r| r.starts_with('.')));
    for w in &wanted {
        if !mapped.iter().any(|m| m.field == *w || m.field.strip_prefix(*w).is_some_and(|r| r.starts_with('.'))) {
            return Err(format!("--feed {w}: no mapped field is it or in it"));
        }
    }
    let mut names = Vec::new();
    for (m, values) in mapped.iter().zip(original).filter(|(m, _)| fed(&m.field)) {
        names.push(m.field.clone());
        feed_field(feeds, m, values, after);
    }
    say("fed", format!("{}: the original's values at every tick", names.join(", ")));
    Ok(names)
}

/// The original's values of `m` from the tick after `after` on, as the inputs file feeds them (D124): where
/// they changed, each holding until it changes.
fn feed_field(feeds: &mut BTreeMap<u64, serde_json::Map<String, Value>>, m: &Mapped, values: &[Value], after: u64) {
    let mut last: Option<&Value> = None;
    for (i, value) in values.iter().enumerate().skip(after as usize) {
        if last != Some(value) {
            feeds.entry(i as u64 + 1).or_default().insert(m.field.clone(), value.clone());
            last = Some(value);
        }
    }
}

/// The session's inputs, tick by tick (`rrc-inputs`): from the trace, what the port gets; the fields the
/// scenario's edits set (`set`), and the fed fields' values where they changed (`fed`, D124).
fn write_inputs(
    trace: &Trace,
    sets: &BTreeMap<u64, serde_json::Map<String, Value>>,
    feeds: &BTreeMap<u64, serde_json::Map<String, Value>>,
    path: &str,
) -> Result<(), String> {
    let inputs: Vec<Value> = trace
        .header
        .inputs
        .iter()
        .map(|i| {
            let mut item = json!({"name": i.name, "size": i.size});
            if !i.device.is_empty() {
                item["device"] = json!(i.device);
            }
            // The sync points whose hits start its ticks, and its rounds (D130).
            if !i.sync.is_empty() {
                item["sync"] = json!(i.sync);
            }
            item
        })
        .collect();
    let mut header = json!({"format": "rrc-inputs", "version": 3, "session": trace.header.session, "inputs": inputs});
    if !trace.header.hooks.is_empty() {
        let hooks: Vec<Value> =
            trace.header.hooks.iter().map(|h| json!({"name": h.name, "registers": h.registers})).collect();
        header["hooks"] = json!(hooks);
    }
    let mut text = header.to_string();
    text.push('\n');
    for (n, t) in trace.ticks.iter().enumerate() {
        let read: Vec<Value> = t.inputs.iter().filter_map(|(i, v)| v.as_ref().map(|v| json!([i, v]))).collect();
        let mut line = json!({"tick": t.tick, "sync": t.sync, "inputs": read});
        // How many times the original went round the wait the pass ends at: the next tick's skipped hits (D130).
        if let Some(spun) = trace.ticks.get(n + 1).and_then(|next| next.skipped) {
            line["spun"] = json!(spun);
        }
        // The hooks' calls in the pass (D98).
        if !t.hooks.is_empty() {
            let calls: Vec<Value> = t.hooks.iter().map(|(i, values)| json!([i, values])).collect();
            line["hooks"] = json!(calls);
        }
        // What each late read saw, and its bytes that differ from what the program should have read (D97).
        let late: Vec<Value> = t
            .late
            .iter()
            .filter_map(|l| {
                l.value.as_ref().map(|v| match (&l.bytes, l.round) {
                    (bytes, round) if round != 0 => json!([l.input, l.at, v, l.read, bytes, round]),
                    (Some(bytes), _) => json!([l.input, l.at, v, l.read, bytes]),
                    (None, _) => json!([l.input, l.at, v, l.read]),
                })
            })
            .collect();
        if !late.is_empty() {
            line["late"] = json!(late);
        }
        if let Some(set) = sets.get(&t.tick) {
            line["set"] = json!(set);
        }
        if let Some(fed) = feeds.get(&t.tick) {
            line["fed"] = json!(fed);
        }
        text.push_str(&line.to_string());
        text.push('\n');
    }
    text.push_str(&json!({"end": trace.end.end, "ticks": trace.ticks.len()}).to_string());
    text.push('\n');
    std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))
}

/// Where the port's own passes differed from the original's in a lockstep comparison (D124): for a field, the
/// screen, the sound or a note, in how many ticks, the first (its session and tick), and what it was there.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Seen {
    pub ticks: u64,
    pub session: String,
    pub tick: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub what: String,
}

/// What a lockstep comparison found (D124): the port given the original's mapped state before every pass,
/// every field its own pass left otherwise, the frames and the sound that differ, and what it noted.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Census {
    /// The port's own passes compared.
    pub passes: u64,
    /// Fields its pass left otherwise than the original's.
    pub fields: BTreeMap<String, Seen>,
    /// Fields it leaves unset (null): not ported.
    pub unset: BTreeMap<String, Seen>,
    pub screen: Option<Seen>,
    pub sound: Option<Seen>,
    /// What it noted (`rrc_runtime::note`, D115): paths not ported.
    pub notes: BTreeMap<String, Seen>,
}

impl Census {
    /// Whether the port's state, frames or sound differ anywhere.
    pub fn differs(&self) -> bool {
        !self.fields.is_empty() || self.screen.is_some() || self.sound.is_some()
    }

    /// Adds `other`'s (a later part of the same play): counts summed, the first kept where it was.
    pub fn add(&mut self, other: &Census) {
        fn into(seen: &mut Option<Seen>, other: &Seen) {
            match seen {
                Some(seen) => seen.ticks += other.ticks,
                None => *seen = Some(other.clone()),
            }
        }
        self.passes += other.passes;
        for (mine, theirs) in
            [(&mut self.fields, &other.fields), (&mut self.unset, &other.unset), (&mut self.notes, &other.notes)]
        {
            for (key, seen) in theirs {
                let mut slot = mine.remove(key);
                into(&mut slot, seen);
                mine.insert(key.clone(), slot.expect("added"));
            }
        }
        if let Some(seen) = &other.screen {
            into(&mut self.screen, seen);
        }
        if let Some(seen) = &other.sound {
            into(&mut self.sound, seen);
        }
    }

    /// Each thing `other` has that this hasn't: (its kind, its name, where first).
    pub fn new_in<'a>(&self, other: &'a Census) -> Vec<(&'static str, &'a str, &'a Seen)> {
        let mut new = Vec::new();
        for (kind, mine, theirs) in [
            ("differs", &self.fields, &other.fields),
            ("unset", &self.unset, &other.unset),
            ("note", &self.notes, &other.notes),
        ] {
            new.extend(
                theirs.iter().filter(|(key, _)| !mine.contains_key(*key)).map(|(key, seen)| (kind, key.as_str(), seen)),
            );
        }
        for (kind, mine, theirs) in [("screen", &self.screen, &other.screen), ("sound", &self.sound, &other.sound)] {
            if let (None, Some(seen)) = (mine, theirs) {
                new.push((kind, "", seen));
            }
        }
        new
    }

    /// One line of a thing seen: kind, then the line's text.
    pub fn line(kind: &str, key: &str, seen: &Seen, passes: u64) -> (String, String) {
        let at = format!("first at {}'s tick {}", seen.session, seen.tick);
        let what = if seen.what.is_empty() { String::new() } else { format!(" ({})", seen.what) };
        let text = match kind {
            "differs" => format!("{key}: in {} of {passes} passes, {at}{what}", seen.ticks),
            "unset" => format!("{key}: the port leaves it unset, in {} passes, {at}", seen.ticks),
            "screen" => format!("the port's frame differs at {} of {passes} ticks, {at}{what}", seen.ticks),
            "sound" => format!("the port's sound differs at {} of {passes} ticks, {at}{what}", seen.ticks),
            _ => format!("{key}: in {} passes, first in the pass to {}'s tick {}", seen.ticks, seen.session, seen.tick),
        };
        (kind.to_owned(), text)
    }

    /// Says what it found, `limit` lines of each kind at most, the most passes first.
    pub fn show(&self, name: &str, limit: usize) {
        fn ranked(map: &BTreeMap<String, Seen>) -> Vec<(&String, &Seen)> {
            let mut all: Vec<(&String, &Seen)> = map.iter().collect();
            all.sort_by(|a, b| b.1.ticks.cmp(&a.1.ticks).then(a.0.cmp(b.0)));
            all
        }
        for (kind, map) in [("differs", &self.fields), ("unset", &self.unset)] {
            let all = ranked(map);
            for (key, seen) in all.iter().take(limit) {
                let (kind, text) = Census::line(kind, key, seen, self.passes);
                say(&kind, text);
            }
            if all.len() > limit {
                say("more", format!("{} more fields {kind} (in census.json)", all.len() - limit));
            }
        }
        for (kind, seen) in [("screen", &self.screen), ("sound", &self.sound)] {
            if let Some(seen) = seen {
                let (kind, text) = Census::line(kind, "", seen, self.passes);
                say(&kind, text);
            }
        }
        let notes = ranked(&self.notes);
        for (key, seen) in notes.iter().take(limit) {
            let (kind, text) = Census::line("note", key, seen, self.passes);
            say(&kind, text);
        }
        if notes.len() > limit {
            say("more", format!("{} more notes (in census.json)", notes.len() - limit));
        }
        if !self.differs() {
            say("match", format!("{name}: every mapped field, frame and sound in all {} passes", self.passes));
        }
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// `rrc compare SESSION --lockstep` (D124): the port from a transplant at tick 1, given the original's mapped
/// state before every pass (every field fed); every difference counted, none stopping it: the fields its own
/// pass left otherwise, the frames and sound that differ, what it noted.
pub(crate) fn lockstep(repo: &Repo, name: &str) -> Result<Census, String> {
    let session = Session::load(repo, name)?;
    let recorded = session.trace.clone().ok_or_else(|| format!("{name} isn't recorded: rrc record {name}"))?;
    let k = Knowledge::load(&repo.knowledge())?;
    let mapping = Mapping::load(repo)?;
    let mapped = mapping.resolve(&k)?;
    let presented = mapping.resolve_present(&k)?;
    if mapped.is_empty() {
        return Err(format!(
            "nothing is mapped: {MAPPING} names the port's fields and the original's items (docs/formats.md)"
        ));
    }
    let cache = Cache::open(&repo.local)?;
    let trace = Trace::parse(&String::from_utf8_lossy(&cache.get(&recorded.hash)?))?;
    let original = {
        let places = Places::new(repo, &session);
        let linear = |linear: u32| places.linear_id(linear).ok();
        original_values(&trace, &mapped, &crate::values::of(&k).with_linear(&linear), name)?
    };
    let mut census = Census::default();
    let ticks = trace.ticks.len() as u64;
    if ticks < 2 {
        return Ok(census);
    }
    let work = repo.state()?.join("compare").join(name);
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let file = |f: &str| work.join(f).to_string_lossy().into_owned();
    let after = 1;
    let sets = edit_sets(&trace, &mapped, &original, &k.types, after)?;
    let mut feeds = BTreeMap::new();
    for (m, values) in mapped.iter().zip(&original) {
        feed_field(&mut feeds, m, values, after);
    }
    write_inputs(&trace, &sets, &feeds, &file("inputs.jsonl"))?;
    let carried = read_given(repo, &session, &k, &mapping.resolve_carried(&k)?, after)?;
    write_start(&trace.header.session, &mapped, &original, after, carried, &file("start.json"))?;
    let mut port_args = vec![
        "run".to_owned(),
        "--inputs".into(),
        file("inputs.jsonl"),
        "--out".into(),
        file("port.jsonl"),
        "--transplant".into(),
        file("start.json"),
    ];
    if !presented.is_empty() {
        write_presentation(
            repo,
            &session,
            &k,
            &presented,
            (&trace.header.session[..], after),
            &file("presentation.json"),
        )?;
        port_args.extend(["--presentation".into(), file("presentation.json")]);
    }
    let screens = screens(&trace);
    if screens.iter().any(Option::is_some) {
        let frames = work.join("frames");
        if frames.is_dir() {
            std::fs::remove_dir_all(&frames).map_err(|e| format!("{}: {e}", frames.display()))?;
        }
        port_args.extend(["--frames".into(), file("frames")]);
        if let Some(hash) = &screens[(after - 1) as usize] {
            std::fs::write(file("screen.frm"), cache.get(hash)?).map_err(|e| e.to_string())?;
            port_args.extend(["--screen".into(), file("screen.frm")]);
        }
    }
    run_port(repo, &port_args)?;

    let port_trace = std::fs::read_to_string(file("port.jsonl")).map_err(|e| format!("the port's trace: {e}"))?;
    let mut lines = port_trace.lines().filter(|l| !l.trim().is_empty());
    let header: Value =
        serde_json::from_str(lines.next().ok_or("the port wrote an empty trace")?).map_err(|e| e.to_string())?;
    if header["format"] != "rrc-port-trace" || header["version"] != 1 {
        return Err(format!(
            "the port's trace isn't rrc-port-trace version 1 ({} {})",
            header["format"], header["version"]
        ));
    }
    let made: Vec<&str> = match &header["sound"] {
        Value::Bool(true) => SOUND_DEVICES.iter().map(|(d, _)| *d).collect(),
        Value::Array(devices) => devices.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let fields: BTreeMap<&str, usize> = mapped.iter().enumerate().map(|(i, m)| (m.field.as_str(), i)).collect();
    let seen_at = |t: u64, what: String| Seen { ticks: 1, session: name.to_owned(), tick: t, what };
    let count = |map: &mut BTreeMap<String, Seen>, key: &str, t: u64, what: String| match map.get_mut(key) {
        Some(seen) => seen.ticks += 1,
        None => {
            map.insert(key.to_owned(), seen_at(t, what));
        }
    };
    let mut screens_seen = Screens::default();
    for line in lines {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("the port's trace: {e}"))?;
        let Some(t) = v["tick"].as_u64() else { continue };
        if t == 0 || t > ticks {
            return Err(format!("the port's trace has tick {t}; the session has 1 to {ticks}"));
        }
        for (what, _) in v["notes"].as_object().into_iter().flatten() {
            count(&mut census.notes, what, t, String::new());
        }
        if let Some(frame) = v["frame"].as_str() {
            screens_seen.port(&work.join("frames").join(frame))?;
        }
        if t <= after {
            continue;
        }
        census.passes += 1;
        for (path, own) in v["overrode"].as_object().into_iter().flatten() {
            let Some(&i) = fields.get(path.as_str()) else { continue };
            let (m, theirs) = (&mapped[i], &original[i][(t - 1) as usize]);
            if own.is_null() {
                count(&mut census.unset, path, t, String::new());
                continue;
            }
            let mut differ = Vec::new();
            differences(&m.field, theirs, &m.masked(own), 1, &mut differ);
            if let Some((at, o, p)) = differ.first() {
                count(&mut census.fields, path, t, format!("{at}: original {o}, port {p}"));
            }
        }
        if let Some(hash) = &screens[(t - 1) as usize]
            && let Some(diff) = screens_seen.compare(hash, &cache)?
        {
            let what = match diff.sizes {
                Some(((w, h), (pw, ph))) => format!("the original's is {w}x{h}, the port's {pw}x{ph}"),
                None if diff.pixels > 0 => {
                    let (x0, y0, x1, y1) = diff.area;
                    format!("{} pixels in {x0},{y0}-{x1},{y1}", diff.pixels)
                }
                None => format!("{} palette colours", diff.colours.len()),
            };
            match &mut census.screen {
                Some(seen) => seen.ticks += 1,
                None => census.screen = Some(seen_at(t, what)),
            }
        }
        if !made.is_empty()
            && let Some(diff) = sound_differences(&trace.ticks[(t - 1) as usize].sound, &v["sound"], &made)
        {
            let text =
                |e: &Option<Value>| e.as_ref().map_or_else(|| "none".to_owned(), |e| untimed_text(diff.device, e));
            let what = format!(
                "{} event {}: original {}, port {}",
                diff.device,
                diff.at + 1,
                text(&diff.original),
                text(&diff.port)
            );
            match &mut census.sound {
                Some(seen) => seen.ticks += 1,
                None => census.sound = Some(seen_at(t, what)),
            }
        }
    }
    Ok(census)
}

/// A transplant (`rrc-state`): the mapped fields' values at `tick`.
/// `rrc start SESSION --tick N [--out DIR]` (D120): where the session's original is at tick N, for the port to
/// start from without the original running: the mapped fields' values (`state.json`, a transplant's), the
/// presentation's own state (`presentation.json`, D110) and the screen (`screen.frm`), in a folder.
pub(crate) fn start(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--tick", "--out"], &[])?;
    let [name] = a.positionals(1, 1, "start SESSION --tick N [--out DIR]")? else { unreachable!() };
    let tick: u64 = a.required("--tick")?.parse().ok().filter(|&t| t > 0).ok_or("--tick is a tick number")?;
    let session = Session::load(repo, name)?;
    let recorded = session.trace.clone().ok_or_else(|| format!("{name} isn't recorded: rrc record {name}"))?;
    let k = Knowledge::load(&repo.knowledge())?;
    let mapping = Mapping::load(repo)?;
    let (mapped, presented) = (mapping.resolve(&k)?, mapping.resolve_present(&k)?);
    let cache = Cache::open(&repo.local)?;
    let trace = Trace::parse(&String::from_utf8_lossy(&cache.get(&recorded.hash)?))?;
    if tick > trace.ticks.len() as u64 {
        return Err(format!("{name} has {} ticks, not {tick}", trace.ticks.len()));
    }
    let original = {
        let places = Places::new(repo, &session);
        let linear = |linear: u32| places.linear_id(linear).ok();
        original_values(&trace, &mapped, &crate::values::of(&k).with_linear(&linear), name)?
    };
    let dir = match a.value("--out") {
        Some(dir) => std::path::PathBuf::from(dir),
        None => repo.state()?.join("start").join(format!("{name}-{tick}")),
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = |f: &str| dir.join(f).to_string_lossy().into_owned();
    let carried = mapping.resolve_carried(&k)?;
    let values = read_given(repo, &session, &k, &carried, tick)?;
    write_start(&trace.header.session, &mapped, &original, tick, values, &file("state.json"))?;
    let mut has = vec![format!("{} fields mapped", mapped.len())];
    if !carried.is_empty() {
        has.push(format!("{} carried as they were", carried.len()));
    }
    if !presented.is_empty() {
        write_presentation(repo, &session, &k, &presented, (&trace.header.session, tick), &file("presentation.json"))?;
        has.push(format!("the presentation's own {}", presented.len()));
    }
    if let Some(hash) = &screens(&trace)[(tick - 1) as usize] {
        std::fs::write(file("screen.frm"), cache.get(hash)?).map_err(|e| e.to_string())?;
        has.push("the screen".into());
    }
    say("start", format!("{name}'s tick {tick}: {}", has.join(", ")));
    say("folder", crate::path_text(&dir));
    say("next", format!("the port plays from it: PORT play --start {}", crate::path_text(&dir)));
    Ok(ExitCode::SUCCESS)
}

/// A transplant (`rrc-state`): the mapped fields' values at `tick`, and the memory the port carries there
/// (`carried`, D126).
fn write_start(
    session: &str,
    mapped: &[Mapped],
    original: &[Vec<Value>],
    tick: u64,
    carried: serde_json::Map<String, Value>,
    path: &str,
) -> Result<(), String> {
    let mut state: serde_json::Map<String, Value> =
        mapped.iter().zip(original).map(|(m, values)| (m.field.clone(), values[(tick - 1) as usize].clone())).collect();
    state.extend(carried);
    let file = json!({"format": "rrc-state", "version": 1, "session": session, "tick": tick, "state": state});
    std::fs::write(path, format!("{file}\n")).map_err(|e| format!("{path}: {e}"))
}

/// Items given at a start (`[[present]]`, `[[carry]]`): read at `tick`'s sync point from the session's snapshots
/// (as `rrc peek` reads), as their types read them; none read when there are none.
fn read_given(
    repo: &Repo,
    session: &Session,
    k: &Knowledge,
    items: &[Mapped],
    tick: u64,
) -> Result<serde_json::Map<String, Value>, String> {
    let mut state = serde_json::Map::new();
    if items.is_empty() {
        return Ok(state);
    }
    let places = Places::new(repo, session);
    let ranges =
        items.iter().map(|m| Ok((places.address(&m.id.to_string())?, m.size))).collect::<Result<Vec<_>, String>>()?;
    let read = crate::peek::read_at(repo, session, &crate::trace::sync_points(k)?, tick, &ranges)?;
    let r = crate::values::of(k);
    for (m, hex) in items.iter().zip(read) {
        let hex = hex.ok_or_else(|| format!("{} ({}) isn't in memory at tick {tick}", m.field, m.id))?;
        let value = decode(&unhex(&hex)?, &m.ty, &r).map_err(|e| format!("{}: {e}", m.field))?;
        state.insert(m.field.clone(), value);
    }
    Ok(state)
}

/// The presentation's own state (`rrc-presentation`, D110): its fields' items at `tick` ([`read_given`]).
fn write_presentation(
    repo: &Repo,
    session: &Session,
    k: &Knowledge,
    presented: &[Mapped],
    (name, tick): (&str, u64),
    path: &str,
) -> Result<(), String> {
    let state = read_given(repo, session, k, presented, tick)?;
    let file = json!({"format": "rrc-presentation", "version": 1, "session": name, "tick": tick, "state": state});
    std::fs::write(
        path,
        format!(
            "{file}
"
        ),
    )
    .map_err(|e| format!("{path}: {e}"))
}

/// Builds the port ahead of its runs (`rrc play`, D124: so no build competes with the person's window):
/// `[port] build`, or for a `cargo run` command the same `cargo build`; nothing for another command.
pub(crate) fn build_port(repo: &Repo) -> Result<(), String> {
    let port = &repo.settings.port;
    let command: Vec<String> = match port.command.split_first() {
        _ if !port.build.is_empty() => port.build.clone(),
        Some((cargo, rest)) if cargo == "cargo" && rest.first().is_some_and(|r| r == "run") => {
            let mut command = vec![cargo.clone(), "build".to_owned()];
            command.extend(rest[1..].iter().take_while(|a| *a != "--").cloned());
            command
        }
        _ => return Ok(()),
    };
    let (program, rest) = command.split_first().ok_or("[port] build is empty")?;
    let output = Command::new(program)
        .args(rest)
        .current_dir(&repo.root)
        .output()
        .map_err(|e| format!("building the port ({program}): {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail: Vec<&str> = stderr.lines().rev().take(8).collect();
    Err(format!(
        "building the port failed ({}):
{}",
        output.status,
        tail.into_iter().rev().collect::<Vec<_>>().join(
            "
"
        )
    ))
}

/// Builds the port if rrc.toml says how (`[port] build`), then runs it (`[port] command`) in the port
/// repository.
fn run_port(repo: &Repo, args: &[String]) -> Result<(), String> {
    let port = &repo.settings.port;
    // The person's copy of the game, for what the port loads from it (D82, `rrc_runtime::original`):
    // its folder, and the program as the analysis unpacked it (its static data).
    let mut original: Vec<(&str, std::path::PathBuf)> = Vec::new();
    if let Ok(dir) = repo.game_dir() {
        original.push(("RRC_GAME", dir.to_path_buf()));
    }
    if let Some(index) = crate::analysis::Index::load(repo)? {
        original.push(("RRC_PROGRAM", crate::cache::Cache::open(&repo.local)?.path(&index.program)));
    }
    let run = |what: &str, command: &[String], args: &[String]| -> Result<(), String> {
        let (program, rest) = command.split_first().ok_or(format!("[port] {what} is empty"))?;
        let output = Command::new(program)
            .args(rest)
            .args(args)
            .envs(original.iter().cloned())
            .current_dir(&repo.root)
            .output()
            .map_err(|e| format!("[port] {what} ({program}): {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(8).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        Err(format!("[port] {what} failed ({}):\n{}", output.status, tail.join("\n")))
    };
    if !port.build.is_empty() {
        run("build", &port.build, &[])?;
    }
    if port.command.is_empty() {
        return Err("rrc.toml has no [port] command: the port's program, which rrc runs (docs/formats.md)".into());
    }
    run("command", &port.command, args)
}

/// The first divergence: the fields, the tick's late inputs, who wrote the original's value, and the
/// port's code for the function that did.
#[allow(clippy::too_many_arguments)]
fn report(
    repo: &Repo,
    name: &str,
    trace: &Trace,
    mapped: &[Mapped],
    k: &Knowledge,
    t: u64,
    first: u64,
    transplanted: bool,
    found: &[(String, Value, Value)],
) -> Result<(), String> {
    let tick = &trace.ticks[(t - 1) as usize];
    say(
        "diverges",
        format!("at tick {t} ({}): {} field{} differ", tick.sync, found.len(), if found.len() == 1 { "" } else { "s" }),
    );
    for (path, theirs, ours) in found {
        let ours = if ours.is_null() { "missing".to_owned() } else { ours.to_string() };
        say("  field", format!("{path}: original {theirs}, port {ours}"));
    }
    // A divergence after a scenario's edit is suspected of the edit first (D59): the state it made may
    // be one play can't reach.
    let mut edits = trace.header.edits.iter().filter(|e| e["tick"].as_u64().is_some_and(|e| e <= t));
    if let Some(edit) = edits.next_back() {
        let at = edit["tick"].as_u64().unwrap_or(0);
        let when = if at == t { "at this tick".to_owned() } else { format!("the last before it, at tick {at}") };
        let text = crate::trace::edit_text(edit);
        say("edit", format!("{when}: {text}; an edited state may be one play can't reach: suspect the edit first"));
    }
    if t == first {
        say(
            "start",
            if transplanted {
                "at the transplant's tick: a field the mapping misses, or the port's transplant() builds it otherwise"
            } else {
                "at the port's first tick: its start() isn't the original's state at its first sync point hit"
            },
        );
        return Ok(());
    }
    let before = &trace.ticks[(t - 2) as usize];
    for l in &before.late {
        let name = &trace.header.inputs[l.input].name;
        let which = if l.read > 1 { format!(", its read {} there", l.read) } else { String::new() };
        say(
            "late",
            format!("tick {}'s pass read {name} late at {}{which}: the port reads it there with at_site", t - 1, l.at),
        );
    }
    // Who wrote the first differing value: in the pass from tick t-1's sync point to tick t's, which a
    // watch numbers t.
    let (path, _, _) = &found[0];
    let Some((m, offset, size)) = mapped.iter().find_map(|m| locate(m, path, &k.types).map(|(o, s)| (m, o, s))) else {
        return Ok(());
    };
    let Some(id) = located(repo, name, trace, m, offset, t)? else {
        say("wrote", format!("{path}: its pointer ({}) is null at tick {t}", m.id));
        return Ok(());
    };
    say("wrote", format!("{path} ({id}), between the sync points of ticks {} and {t}:", t - 1));
    let args: Vec<String> =
        [name, &id.to_string(), "--len", &size.to_string(), "--tick", &t.to_string(), "--ticks", "1", "--lines", "6"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    let writer = crate::watch::watched(repo, &args)?;
    let writer = through_copies(repo, name, k, writer, size, t)?;
    // A registered quirk at that code (D74): the port keeps it, or should.
    let quirks = crate::quirks::Quirks::load(repo)?;
    let at = |id: &Id| quirks.quirks.iter().find(|q| q.at == id.to_string());
    if let Some(q) = writer.instruction.iter().chain(&writer.chain).find_map(at) {
        say("quirk", format!("{} is at {}: does the port keep it (rrc_runtime::keeps({:?}))?", q.name, q.at, q.name));
    }
    port_code(repo, k, &writer.chain);
    Ok(())
}

/// Where `m`'s byte `offset` is at tick `t`: through its pointer then, if it has one (D72); none if that
/// is null.
fn located(repo: &Repo, name: &str, trace: &Trace, m: &Mapped, offset: u32, t: u64) -> Result<Option<Id>, String> {
    let Some(via) = &m.via else {
        return Id::new(m.id.module(), m.id.part(), m.id.offset() + offset).map(Some).map_err(|e| e.to_string());
    };
    let size = via.pointer.size();
    let column = trace
        .header
        .state
        .iter()
        .position(|item| item.via.is_none() && item.at == m.id.to_string() && item.size == size);
    let bytes = column
        .and_then(|c| trace.values(c)[(t - 1) as usize].clone())
        .map(|hex| crate::trace::unhex(&hex))
        .unwrap_or_default();
    Ok(match via.pointer.target(&bytes, via.offset + offset) {
        None => None,
        Some(Target::Id(id)) => Some(id),
        Some(Target::Linear(linear)) => {
            let session = Session::load(repo, name)?;
            Some(Places::new(repo, &session).linear_id(linear)?)
        }
    })
}

/// Who last wrote `m` in the pass to tick `t` (from power-on, for tick 1), which tells the C library's
/// leftovers (a heap's links) from what the program set there and the port should have (D102).
fn last_writer(repo: &Repo, name: &str, trace: &Trace, k: &Knowledge, m: &Mapped, t: u64) -> Result<String, String> {
    let Some(id) = located(repo, name, trace, m, 0, t)? else {
        return Ok(format!("its pointer ({}) is null", m.id));
    };
    let args: Vec<String> =
        [name, &id.to_string(), "--len", &m.size.to_string(), "--tick", &t.to_string(), "--ticks", "1", "--quiet"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    let writer = crate::watch::watched(repo, &args)?;
    let Some(at) = writer.instruction else {
        return Ok(format!("nothing wrote it in the pass to tick {t}"));
    };
    let named = |id: &Id| k.functions.get(id).map_or_else(|| id.to_string(), |d| d.name.clone());
    let chain: Vec<String> = writer.chain.iter().take(3).map(named).collect();
    Ok(format!("last written in the pass to tick {t} by {at} ({})", chain.join(" < ")))
}

/// A copy's value was decided where its source was written (D85): who wrote that, before the copy, a few
/// copies back; the last writer found.
fn through_copies(
    repo: &Repo,
    name: &str,
    k: &Knowledge,
    mut writer: crate::watch::Writer,
    size: u32,
    t: u64,
) -> Result<crate::watch::Writer, String> {
    for _ in 0..4 {
        let Some(source) = writer.copied.clone() else { break };
        let session = Session::load(repo, name)?;
        let names = crate::watch::Names::new(k, repo, &session)?;
        let Some(source_id) = names.id(&source) else { break };
        let copier = writer.instruction.as_ref().map_or_else(|| "it".to_owned(), ToString::to_string);
        say("copied", format!("{copier} copied it from {}: who wrote that before:", names.data(&source, size)));
        let args: Vec<String> = [
            name,
            &source_id.to_string(),
            "--len",
            &size.to_string(),
            "--tick",
            &t.to_string(),
            "--ticks",
            "1",
            "--lines",
            "6",
            "--before",
            &writer.cycle.to_string(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let earlier = crate::watch::watched(repo, &args)?;
        if earlier.instruction.is_none() {
            say("copied", format!("nothing wrote it during tick {t} before the copy: it held its value from before"));
            break;
        }
        writer = earlier;
    }
    Ok(writer)
}

/// Where the port does what the original's writing function did (D73): its code that names that
/// function's ID (DESIGN.md §6.6), or, if none does, a caller's in the chain.
fn port_code(repo: &Repo, k: &Knowledge, chain: &[Id]) {
    let Some(writer) = chain.first() else { return };
    let named = |id: &Id| {
        let name = k.functions.get(id).map_or_else(|| id.to_string(), |d| d.name.clone());
        if name == id.to_string() { name } else { format!("{name} ({id})") }
    };
    let module = repo.module();
    for (i, id) in chain.iter().enumerate() {
        // The program's own may be named by its offset alone.
        let text = id.to_string();
        let short = text.split_once('+').map(|(_, offset)| format!("+{offset}"));
        let short = short.filter(|_| id.module() == module && id.part().is_none());
        let found = crate::sources::naming(&repo.root, &text, short.as_deref(), 3);
        if found.is_empty() {
            continue;
        }
        let at: Vec<String> = found.iter().map(|(file, line)| format!("{file}:{line}")).collect();
        let text = match i {
            0 => format!("{} is {}", named(id), at.join(", ")),
            _ => format!("{} isn't named; its caller {} is {}", named(writer), named(id), at.join(", ")),
        };
        say("port", text);
        return;
    }
    say(
        "port",
        format!(
            "no code names {} or its callers: name the original each function reimplements, by its ID, in a \
             comment (DESIGN.md §6.6)",
            named(writer)
        ),
    );
}

/// Where a differing path (`enemies[3].x`) is in its mapped item: its offset and size.
fn locate(m: &Mapped, path: &str, types: &Types) -> Option<(u32, u32)> {
    let rest = path.strip_prefix(m.field.as_str())?;
    if !rest.is_empty() && !rest.starts_with(['[', '.']) {
        return None;
    }
    let (offset, ty) = crate::values::locate(&m.ty, rest, types)?;
    Some((offset, crate::values::size(&ty, types).ok()?))
}

/// Keeps a paired checkpoint (D63): the port's own state at tick `t` and the original's snapshot there.
fn keep_pair(
    repo: &Repo,
    session: &mut Session,
    name: &str,
    k: &Knowledge,
    t: u64,
    saved: &str,
    cache: &Cache,
) -> Result<(), String> {
    let port = match std::fs::read(saved) {
        Ok(bytes) => cache.put(&bytes)?,
        Err(_) => {
            say("pair", format!("not kept: the port didn't reach tick {t}"));
            return Ok(());
        }
    };
    let snapshot = crate::snapshots::at_tick(repo, session, k, t)?;
    session.pairs.retain(|p| p.tick != t);
    session.pairs.push(Pair { tick: t, port, snapshot });
    session.pairs.sort_by_key(|p| p.tick);
    session.save(repo, name)?;
    say("pair", format!("kept at tick {t}: rrc compare {name} --resume {t} starts the port there"));
    Ok(())
}

/// Says which named data the mapping neither maps nor ignores: the state not compared yet.
fn unmapped(k: &Knowledge, mapped: &[Mapped], repo: &Repo) -> Result<(), String> {
    let mapping = Mapping::load(repo)?;
    let ignored = mapping.ignored(k)?;
    let presented = mapping.resolve_present(k)?;
    let covered = |id: &Id, size: u32| {
        let inside = |at: &Id, n: u32| {
            at.module() == id.module()
                && at.part() == id.part()
                && at.offset() <= id.offset()
                && id.offset() + size <= at.offset() + n
        };
        mapped.iter().chain(&presented).any(|m| inside(m.covers().0, m.covers().1))
            || ignored.iter().any(|(at, n)| inside(at, *n))
    };
    let inputs: Vec<String> = k.ticks.inputs.iter().map(|i| i.at.clone()).collect();
    let left: Vec<&str> = k
        .data
        .iter()
        .filter(|(id, d)| d.size.is_some_and(|size| !covered(id, size)) && !inputs.contains(&id.to_string()))
        .map(|(_, d)| d.decision.name.as_str())
        .collect();
    if !left.is_empty() {
        let shown: Vec<&str> = left.iter().take(10).copied().collect();
        let more = if left.len() > 10 { format!(" and {} more", left.len() - 10) } else { String::new() };
        say("unmapped", format!("{} named data not mapped or ignored: {}{more}", left.len(), shown.join(", ")));
    }
    Ok(())
}

fn unhex(text: &str) -> Result<Vec<u8>, String> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).map_err(|_| format!("bad hex {text:?}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::{Field, Struct};

    #[test]
    fn a_tick_s_sound_is_compared_without_its_times() {
        let original: serde_json::Map<String, Value> = serde_json::from_value(json!({
            "opl": [[100, 0xA0, 0x59], [200, 0xB0, 0x35]],
            "speaker": [[7, {"Periodic": {"origin": 7, "period": 2281, "high": 1141}}], [9, {"Steady": false}]],
        }))
        .unwrap();
        let port = json!({"opl": [[0xA0, 0x59], [0xB0, 0x35]], "speaker": [{"tone": [2281, 1141]}, {"steady": false}]});
        let all = ["opl", "speaker", "pcm", "midi"];
        assert_eq!(sound_differences(&original, &port, &all), None);
        let off = json!({"opl": [[0xA0, 0x59], [0xB0, 0x15]], "speaker": [{"tone": [2281, 1141]}, {"steady": false}]});
        let diff = sound_differences(&original, &off, &all).unwrap();
        assert_eq!((diff.device, diff.at, diff.port), ("opl", 1, Some(json!([0xB0, 0x15]))));
        let silent = sound_differences(&original, &json!({}), &all).unwrap();
        assert_eq!((silent.device, silent.counts), ("opl", (2, 0)));
        // A port that makes the speaker's sound only isn't compared on the OPL (D121).
        let speaker = json!({"speaker": [{"tone": [2281, 1141]}, {"steady": false}]});
        assert_eq!(sound_differences(&original, &speaker, &["speaker"]), None);
        assert_eq!(untimed_text("opl", &json!([0xB0, 0x35])), "B0=35");
        assert_eq!(untimed_text("speaker", &json!({"tone": [2281, 1141]})), "tone 2281/1141");
    }

    #[test]
    fn a_frame_differs_from_the_screen_by_its_pixels_and_colours() {
        let original = Frame::new(4, 3);
        assert_eq!(screen_differences(&original, &original), None);
        let mut port = original.clone();
        port.set(1, 2, 5);
        port.set(3, 0, 5);
        port.palette[3..6].copy_from_slice(&[1, 2, 3]);
        let diff = screen_differences(&original, &port).unwrap();
        assert_eq!((diff.pixels, diff.area, diff.first), (2, (1, 0, 3, 2), Some((3, 0, 0, 5))));
        assert_eq!(diff.areas.len(), 2, "two pixels apart: two areas");
        assert_eq!(diff.colours, [(1, [0, 0, 0], [1, 2, 3])]);
        assert_eq!(side_by_side(&diff).width, 12);
        assert!(screen_differences(&original, &Frame::new(4, 4)).unwrap().sizes.is_some());
    }

    #[test]
    fn differing_pixels_make_areas_largest_first() {
        // A diagonal run of three (touching at their corners) and a lone pixel, on a 5 x 4 screen.
        let mut differs = vec![false; 20];
        for i in [4, 6, 12, 18] {
            differs[i] = true;
        }
        let found = areas(&differs, 5);
        assert_eq!(
            found,
            [
                Area { bounds: (1, 1, 3, 3), pixels: 3, first: (1, 1) },
                Area { bounds: (4, 0, 4, 0), pixels: 1, first: (4, 0) },
            ]
        );
    }

    #[test]
    fn an_unset_field_matches_while_the_original_s_is_unchanged() {
        let mut found = Vec::new();
        assert!(unset_matches("map", &json!([16, 0, 16]), &json!([16, 0, 16]), &mut found));
        assert!(found.is_empty());
        assert!(!unset_matches("map", &json!([16, 0, 16]), &json!([16, 1, 16]), &mut found));
        assert_eq!(found, [("map[1]".to_owned(), json!(1), Value::Null)], "where it changed, the port has none");
    }

    #[test]
    fn a_differing_path_is_located_in_its_item() {
        let field = |offset, size, name: &str, ty: &str| Field { offset, size, name: name.into(), ty: ty.into() };
        let types = Types {
            structs: vec![Struct {
                name: "enemy".into(),
                size: 10,
                fields: vec![field(0, 2, "x", "i16"), field(2, 2, "y", "i16"), field(8, 1, "alive", "u8")],
                provenance: String::new(),
            }],
            ..Types::default()
        };
        let mapped = |field: &str, id: &str, ty: &str, size| Mapped {
            field: field.into(),
            id: id.parse().unwrap(),
            ty: ty.into(),
            size,
            via: None,
            mask: None,
            into: None,
        };
        let m = mapped("enemies", "GAME.EXE+00100", "enemy[6]", 60);
        assert_eq!(locate(&m, "enemies[3].y", &types), Some((32, 2)));
        assert_eq!(locate(&m, "enemies[1]", &types), Some((10, 10)));
        assert_eq!(locate(&m, "enemies", &types), Some((0, 60)));
        assert_eq!(locate(&m, "enemiesx", &types), None);
        let score = mapped("player.score", "GAME.EXE+00200", "u16", 2);
        assert_eq!(locate(&score, "player.score", &types), Some((0, 2)));
    }
}
