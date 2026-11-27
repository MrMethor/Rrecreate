//! `rrc compare SESSION` (docs/design.md §7, §8, D61-D63): the port against the original, tick by tick.
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

/// Fields shown of a divergence (D140).
const SHOWN: usize = 16;

/// A mapped field that differs at a tick (D140): its first leaf that does (the path, the original's value, the
/// port's) and how many of its leaves differ.
struct Differs {
    field: String,
    path: String,
    theirs: Value,
    ours: Value,
    leaves: usize,
}

impl Differs {
    /// The field `field`'s first difference of `original` and `port`, if they differ.
    fn of(field: &str, original: &Value, port: &Value) -> Option<Differs> {
        let mut first = Vec::new();
        differences(field, original, port, 1, &mut first);
        let (path, theirs, ours) = first.pop()?;
        let leaves = crate::values::count_differences(original, port);
        Some(Differs { field: field.to_owned(), path, theirs, ours, leaves })
    }
}

pub(crate) fn compare(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(
        args,
        &["--transplant", "--resume", "--to", "--pair", "--feed", "--fix"],
        &["--notes", "--lockstep", "--fields", "--streamed", "--corrected", "--live"],
    )?;
    let usage = "compare SESSION [--transplant TICK | --resume TICK] [--to TICK] [--pair TICK] [--feed FIELD,...] \
                 [--fields] [--notes] [--fix QUIRK] [--streamed] | compare SESSION --lockstep [--streamed] | compare \
                 SESSION --corrected | compare SESSION --live";
    // The port fed a tick at a time, each once it wrote the one before, as the live comparison feeds it (D190).
    STREAMED.store(a.has("--streamed"), std::sync::atomic::Ordering::Relaxed);
    // The mapped fields alone (D136): a stretch whose screens or sound the port doesn't make yet.
    let fields_only = a.has("--fields");
    let [name] = a.positionals(1, 1, usage)? else { unreachable!() };
    // A quirk's switch tried (D149): the run with its fix, so a difference is what the fix changes.
    let fixed = a.value("--fix");
    if let Some(quirk) = fixed {
        if a.has("--lockstep") {
            return Err("--fix tries a quirk's switch on one run, not --lockstep".into());
        }
        if !crate::quirks::Quirks::load(repo)?.quirks.iter().any(|q| q.name == quirk) {
            return Err(format!("no quirk {quirk}: rrc quirk lists them"));
        }
    }
    // The port's own input handling checked beside lockstep (D194): what its `Live` makes of the session's devices.
    if a.has("--live") {
        if a.has("--corrected") || fixed.is_some() {
            return Err("--live checks the port's input handling beside lockstep: not with --corrected or --fix".into());
        }
        let lived = live(repo, name)?;
        let file = repo.state()?.join("compare").join(name.as_str()).join("live.json");
        lived.save(&file)?;
        lived.show(name);
        say("live", crate::path_text(&file));
        return Ok(if lived.differs() { ExitCode::from(1) } else { ExitCode::SUCCESS });
    }
    // The port's free run, corrected where it differs (D191): its census, kept beside lockstep's.
    if a.has("--corrected") {
        if a.has("--lockstep") || fixed.is_some() {
            return Err("--corrected is a census of its own: not with --lockstep or --fix".into());
        }
        let census = corrected(repo, name)?;
        let file = repo.state()?.join("compare").join(name.as_str()).join("corrected.json");
        census.save(&file)?;
        census.show(name, 20);
        say("census", crate::path_text(&file));
        return Ok(if census.differs() { ExitCode::from(1) } else { ExitCode::SUCCESS });
    }
    if a.has("--lockstep") {
        let census = lockstep(repo, name)?;
        let file = repo.state()?.join("compare").join(name.as_str()).join("census.json");
        census.save(&file)?;
        census.show(name, 20);
        say("census", crate::path_text(&file));
        if census.differs() || !census.unset.is_empty() || !census.notes.is_empty() {
            say("next", "rrc work: this with every other session's census, ranked; rrc work take ITEM for the top one");
        }
        let differing = census.fields.len()
            + usize::from(census.screen.is_some())
            + usize::from(census.pictures.is_some())
            + usize::from(census.sound.is_some());
        crate::journal::ended(name, format!("census: {differing} things differ"));
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
    let ticks = trace.ticks.len() as u64;
    for t in [transplant, resume, to, pair].into_iter().flatten() {
        if t > ticks {
            return Err(format!("{name} has {ticks} ticks, not {t}"));
        }
    }
    let original = {
        // A far pointer's target is named where the session put the program (D72). The ticks compared alone, from
        // where the port starts to `--to` (D208): a long play's every tick is gigabytes.
        let places = Places::new(repo, &session);
        let linear = |linear: u32| places.linear_id(linear).ok();
        let from = transplant.or(resume).unwrap_or(1).max(1) - 1;
        let range = from as usize..to.unwrap_or(ticks) as usize;
        original_values(&trace, &mapped, &crate::values::of(&k).with_linear(&linear), name, range)?
    };

    let work = repo.state()?.join("compare").join(name.as_str());
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let file = |f: &str| work.join(f).to_string_lossy().into_owned();
    let after = transplant.or(resume).unwrap_or(0);
    let sets = edit_sets(repo, name.as_str(), &trace, &mapped, &original, &k.types, after)?;
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
    write_inputs(&trace, &sets, &feeds, None, &file("inputs.jsonl"))?;
    let mut port_args =
        vec!["run".to_owned(), "--inputs".into(), file("inputs.jsonl"), "--out".into(), file("port.jsonl")];
    if let Some(t) = transplant {
        let carried = read_given(repo, &session, &k, &mapping.resolve_carried(&k)?, t)?;
        write_start(&trace.header.session, &mapped, &original, t, carried, &file("start.json"))?;
        port_args.extend(["--transplant".into(), file("start.json")]);
    }
    // From power-on, the program's own initial data (D135): the mapped fields', and the carried memory's (D137).
    let mut initial = mapped.clone();
    initial.extend(mapping.resolve_carried(&k)?);
    if transplant.is_none()
        && resume.is_none()
        && write_initial(repo, &trace.header.session, &initial, &k, &file("initial.json"))?
    {
        port_args.extend(["--initial".into(), file("initial.json")]);
    }
    // The games the port saves and reads (D131): the original's files as they were where the port starts.
    let saves = work.join("saves");
    write_saves(repo, &session, &k, transplant.or(resume), &saves)?;
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
    let screens = if fields_only { vec![None; trace.ticks.len()] } else { screens(&trace) };
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
    if let Some(quirk) = fixed {
        say("fixed", format!("{quirk}: a difference here is what its fix changes, not kept for rrc progress"));
        port_args.extend(["--fix".into(), quirk.to_owned()]);
    }
    run_port(repo, &session, &port_args, Some(&saves), None)?;

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
    // The pictures the port shows between ticks (D180), and the first not among the original's.
    let mut between = Between::default();
    let mut missed: Option<Missed> = None;
    // The port's sound, if it says it makes it (D108): every device's, or those it names (D121).
    let made: Vec<&str> = match &header["sound"] {
        Value::Bool(true) => SOUND_DEVICES.iter().map(|(d, _)| *d).collect(),
        Value::Array(devices) => devices.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let sounding = !fields_only && !made.is_empty();
    let mut heard: Option<(u64, SoundDiff)> = None;
    // The fields the port left unset (null), by the ticks they were (D102).
    let mut unset: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for line in lines {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("the port's trace: {e}"))?;
        let Some(t) = v["tick"].as_u64() else { continue };
        if let Some(changed) = v["state"].as_object() {
            rrc_runtime::apply_changes(&mut state, changed)?;
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
                // None in both: memory the original freed (a pointer null), the port's field none.
                Some(Value::Null) if theirs.is_null() => {}
                Some(Value::Null) => {
                    if unset_matches(&m.field, &values[(start - 1) as usize], theirs, &mut found) {
                        unset.entry(m.field.as_str()).or_insert((t, t)).1 = t;
                    }
                }
                Some(ours) => found.extend(Differs::of(&m.field, theirs, &m.masked(ours))),
                None => found.push(Differs {
                    field: m.field.clone(),
                    path: m.field.clone(),
                    theirs: theirs.clone(),
                    ours: Value::Null,
                    leaves: 1,
                }),
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
        if let Some(names) = v["between"].as_array().filter(|n| !n.is_empty()) {
            let names: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
            let fleeting: Vec<&str> =
                v["fleeting"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let tick = &trace.ticks[(t - 1) as usize];
            if let Some(miss) = between.check(t, &tick.frames, &names, &fleeting, &work.join("frames"), &cache)? {
                drawn = Some((t, miss.diff.clone()));
                missed = Some(miss);
                break;
            }
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
    // The devices the original sounded in these ticks that nothing was compared for (D158): a session matches in
    // sound when there is none.
    let compared = |device: &str| sounding && made.contains(&device);
    let sounded =
        |device: &str| trace.ticks[(first - 1) as usize..last as usize].iter().any(|t| t.sound.contains_key(device));
    let unsounded: Vec<String> =
        SOUND_DEVICES.iter().map(|(d, _)| *d).filter(|d| sounded(d) && !compared(d)).map(str::to_owned).collect();
    let outcome = crate::progress::Outcome {
        recorded: recorded.hash.clone(),
        from: after,
        first,
        last,
        matched: matched.max(after),
        ticks,
        fed: fed.clone(),
        differs: differs.map(str::to_owned),
        fields_only,
        drew: seen.compared > 0,
        unsounded,
        port: crate::impact::fingerprint(repo)?,
    };
    if fixed.is_none() {
        outcome.save(&work)?;
        // How it ended, for the journal (D157): the same first difference again and again is a route that stopped paying.
        let end = match (&diverged, &drawn, &heard) {
            (Some((t, found)), _, _) => format!("tick {t}: {}", found[0].path),
            (None, Some((t, _)), _) => format!("tick {t}: the screen"),
            (None, None, Some((t, _))) => format!("tick {t}: the sound"),
            (None, None, None) => "match".to_owned(),
        };
        crate::journal::ended(name, end);
    }
    if let Some(quirk) = fixed {
        // What its fix changes here (D160): a switch is shown where the port matches the original with the bug kept,
        // and differs with it fixed.
        let first = match (&diverged, &drawn, &heard) {
            (Some((t, found)), _, _) => Some((*t, found[0].path.clone())),
            (None, Some((t, _)), _) => Some((*t, "the screen".to_owned())),
            (None, None, Some((t, _))) => Some((*t, "the sound".to_owned())),
            (None, None, None) => None,
        };
        match first {
            None => say(
                "shown",
                format!("not here: {quirk}'s fix changes nothing in these ticks, so no pass of them reaches it"),
            ),
            Some((t, what)) => {
                let kept = crate::progress::Outcome::kept(repo, name)?;
                if kept.values().any(|o| o.recorded == recorded.hash && o.matched >= t) {
                    let text = format!("{name} tick {t}: {what}");
                    crate::quirks::shown(repo, quirk, &text)?;
                    say("shown", format!("{quirk}: its switch changes {text}; kept in quirks.toml"));
                } else {
                    say(
                        "shown",
                        format!(
                            "not yet: with the bug kept the port has to match the original to tick {t} first (rrc \
                             compare {name}, without --fix)"
                        ),
                    );
                }
            }
        }
    }
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
            if let Some(miss) = &missed {
                say(
                    "diverges",
                    format!(
                        "at tick {t} ({}): the pictures its pass showed: the port's {} of {} isn't among the original's \
                         from its {} of {} on; the nearest, its {}:",
                        trace.ticks[(t - 1) as usize].sync,
                        ordinal(miss.picture + 1),
                        miss.pictures,
                        ordinal(miss.from + 1),
                        miss.shown,
                        ordinal(miss.nearest + 1)
                    ),
                );
            }
            report_screen(repo, name, &trace, &k, *t, diff, &file(&format!("screen-{t}.png")))?;
            ExitCode::from(1)
        }
        None => {
            say("match", format!("every mapped field at all {} ticks", last - first + 1));
            match seen.compared {
                _ if fields_only => say("screen", "not compared, nor the sound (--fields)"),
                0 if screens.iter().any(Option::is_some) => say("screen", "not compared: the port draws no frames"),
                0 => {}
                _ => say(
                    "screen",
                    format!("the port's frames are the original's screen at all {} ticks", last - first + 1),
                ),
            }
            if between.passes > 0 {
                say(
                    "between",
                    format!(
                        "the port's {} pictures between ticks, in {} pass{}, are among the original's there in order \
                         (of its {})",
                        between.pictures,
                        between.passes,
                        if between.passes == 1 { "" } else { "es" },
                        between.shown
                    ),
                );
                if between.unseen > 0 {
                    say(
                        "between",
                        format!(
                            "{} of them shown for less than a refresh, which the display didn't show",
                            between.unseen
                        ),
                    );
                }
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
            // The files the port saved, against those the original changed by the last tick (D231).
            let original = crate::peek::changed_files_at(repo, &session, &crate::trace::sync_points(&k)?, last)?;
            let differing = files_differing(&original, &work.join("saves"))?;
            if differing.is_empty() {
                if !original.is_empty() {
                    say(
                        "files",
                        format!("the port's files are the original's at tick {last}: {}", files_named(&original)),
                    );
                }
                ExitCode::SUCCESS
            } else {
                for difference in &differing {
                    say("files", difference);
                }
                crate::journal::ended(name, format!("tick {last}: the files"));
                ExitCode::from(1)
            }
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
        // Its exit code is the watch's own; the comparison's is said by the caller.
        let _ = crate::watch::ports(repo, &args)?;
    }
    say(
        "next",
        format!(
            "the events in order: rrc trace {name} --tick {t}; the code that made them: rrc show ID --c of who wrote \
             the device; a driver's answer the port needs: rrc hook (docs/porting.md, stage 6)"
        ),
    );
    crate::digests::say_about(repo, &[diff.device, "sound"], 3, true);
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

/// The pictures shown between ticks (D180): the port's, each one of the original's in the pass, in order. The
/// original's are its display's refreshes (a torn copy, a palette's steps among them), the port's those it draws.
#[derive(Default)]
struct Between {
    /// The original's pictures decoded, by their hash.
    decoded: BTreeMap<String, Frame>,
    passes: usize,
    pictures: usize,
    shown: usize,
    /// The port's pictures shown for less than a refresh that the display didn't show (D181).
    unseen: usize,
}

/// A picture the port showed between ticks that isn't among the original's from where the last was: which (of how
/// many), from which of the original's on (of how many it showed in the pass), the nearest one, and how they differ.
struct Missed {
    picture: usize,
    pictures: usize,
    from: usize,
    shown: usize,
    nearest: usize,
    diff: ScreenDiff,
}

impl Between {
    /// The port's pictures between the tick before and `t` (`names`, in `frames`) against the original's of the
    /// tick's pass (`shown`, the recording's refreshes), each that changed from the last.
    fn check(
        &mut self,
        t: u64,
        shown: &[Value],
        names: &[&str],
        fleeting: &[&str],
        frames: &std::path::Path,
        cache: &Cache,
    ) -> Result<Option<Missed>, String> {
        let mut originals: Vec<&str> = Vec::new();
        for f in shown {
            if let Some(hash) = f.get(4).and_then(Value::as_str)
                && originals.last() != Some(&hash)
            {
                originals.push(hash);
            }
        }
        let mut from = 0;
        for (n, name) in names.iter().enumerate() {
            let path = frames.join(name);
            let bytes = std::fs::read(&path).map_err(|e| format!("the port's picture {}: {e}", path.display()))?;
            let port = Frame::decode(&bytes).map_err(|e| format!("the port's picture {}: {e}", path.display()))?;
            let mut found = None;
            let mut nearest: Option<(usize, usize)> = None;
            for (i, hash) in originals.iter().enumerate().skip(from) {
                if !self.decoded.contains_key(*hash) {
                    let bytes = cache.get(hash).map_err(|e| {
                        format!("{e}: a picture between ticks of a play followed live isn't kept (D196), `rrc record NAME --run` makes them again")
                    })?;
                    let frame = Frame::decode(&bytes).map_err(|e| {
                        format!("the original's picture at tick {t}: {e} (only indexed modes are compared)")
                    })?;
                    self.decoded.insert((*hash).to_owned(), frame);
                }
                let original = &self.decoded[*hash];
                if *original == port {
                    found = Some(i);
                    break;
                }
                let differ = original.pixels.iter().zip(&port.pixels).filter(|(a, b)| a != b).count()
                    + usize::from(original.palette != port.palette);
                if nearest.is_none_or(|(_, d)| differ < d) {
                    nearest = Some((i, differ));
                }
            }
            match found {
                Some(i) => from = i + 1,
                // Shown for less than a refresh (D181): the display may have shown none of it.
                None if fleeting.contains(name) => self.unseen += 1,
                None => {
                    let nearest = nearest.map_or(from.min(originals.len().saturating_sub(1)), |(i, _)| i);
                    let original = originals.get(nearest).map(|h| self.decoded[*h].clone());
                    let diff = original
                        .and_then(|o| screen_differences(&o, &port))
                        .ok_or_else(|| format!("tick {t}: the original showed no picture in its pass"))?;
                    let (pictures, shown) = (names.len(), originals.len());
                    return Ok(Some(Missed { picture: n, pictures, from, shown, nearest, diff }));
                }
            }
        }
        self.passes += 1;
        self.pictures += names.len();
        self.shown += originals.len();
        Ok(None)
    }
}

/// `n` as an ordinal: 1st, 2nd, 3rd, 4th...
fn ordinal(n: usize) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
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
    let code = writer.chain.first().map_or_else(|| "ID".to_owned(), ToString::to_string);
    say(
        "next",
        format!(
            "its code: rrc show {code} --c; the drawing code's own state at the transplant ([[present]] in \
             mapping.toml); the original's screen: rrc frame {name} --tick {t} --screen"
        ),
    );
    let drew: Vec<String> =
        writer.chain.iter().take(2).filter_map(|id| k.functions.get(id)).map(|d| d.name.clone()).collect();
    crate::digests::say_about(repo, &drew, 3, true);
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
fn unset_matches(path: &str, leftovers: &Value, theirs: &Value, found: &mut Vec<Differs>) -> bool {
    let Some(changed) = Differs::of(path, leftovers, theirs) else {
        return true;
    };
    found.push(Differs { theirs: changed.ours, ours: Value::Null, ..changed });
    false
}

/// Each mapped field's value at each tick of the original's trace.
fn original_values(
    trace: &Trace,
    mapped: &[Mapped],
    r: &Reader,
    name: &str,
    range: std::ops::Range<usize>,
) -> Result<Vec<Column>, String> {
    let mut decoding = Original::new(&trace.header, mapped, r, name)?;
    let mut all: Vec<Column> = mapped.iter().map(|_| Column::starting(range.start)).collect();
    for (i, tick) in trace.ticks.iter().enumerate().take(range.end) {
        // Every tick decoded, as the state accumulates; those in `range` kept.
        let values = decoding.values(tick)?;
        if i >= range.start {
            for (column, value) in all.iter_mut().zip(values) {
                column.push(value);
            }
        }
    }
    Ok(all)
}

/// A mapped field's values at every tick of a session, kept as its changes (D208): from the first tick's, each value
/// from the tick it changed to on. A long play's thousands of ticks hold what changed, not a copy of every field a tick
/// (a level's map and floor alone are 16384 words each): indexed by a tick's place (from 0) as a list of values is. It
/// holds the ticks a comparison needs, from `start` to `end` (D208).
#[derive(Debug, Default)]
pub(crate) struct Column {
    changes: Vec<(usize, Value)>,
    start: usize,
    end: usize,
}

impl Column {
    /// A column whose first tick is the one at `start`.
    fn starting(start: usize) -> Column {
        Column { changes: Vec::new(), start, end: start }
    }

    /// The next tick's value.
    fn push(&mut self, value: Value) {
        if self.changes.last().is_none_or(|(_, last)| *last != value) {
            self.changes.push((self.end, value));
        }
        self.end += 1;
    }

    /// Whether it holds the tick at `at`.
    fn holds(&self, at: usize) -> bool {
        (self.start..self.end).contains(&at)
    }

    /// From the tick at `from` on (or its first): its value there, then each it changed to, with the tick's place.
    fn from(&self, from: usize) -> impl Iterator<Item = (usize, &Value)> {
        let from = from.max(self.start);
        let first = (from < self.end).then(|| (from, &self[from]));
        first.into_iter().chain(self.changes.iter().filter(move |(at, _)| *at > from).map(|(at, v)| (*at, v)))
    }
}

impl std::ops::Index<usize> for Column {
    type Output = Value;

    fn index(&self, at: usize) -> &Value {
        assert!(self.holds(at), "tick {} outside the ticks kept, {} to {}", at + 1, self.start + 1, self.end);
        &self.changes[self.changes.partition_point(|(from, _)| *from <= at) - 1].1
    }
}

/// The original's mapped fields decoded a tick at a time, as a trace's ticks come (D192): each field's value from the
/// items the ticks changed, keeping only the values as the last tick left them.
pub(crate) struct Original<'a> {
    mapped: &'a [Mapped],
    reader: &'a Reader<'a>,
    /// Each field's item (its column in the trace's state), and an index's (D122) array's pointer's with how it points.
    columns: Vec<(usize, Option<(usize, crate::values::PointerKind)>)>,
    /// The state's items as the ticks so far left them, by column.
    hex: Vec<Option<String>>,
    /// Each field's bytes and value at the last tick: a value whose bytes didn't change isn't decoded again.
    last: Vec<Option<(Option<String>, Value)>>,
}

impl<'a> Original<'a> {
    /// The fields `mapped` in a trace with `header` (of session `name`), as `reader` reads them.
    pub(crate) fn new(
        header: &crate::trace::Header,
        mapped: &'a [Mapped],
        reader: &'a Reader<'a>,
        name: &str,
    ) -> Result<Original<'a>, String> {
        let mut columns = Vec::new();
        for m in mapped {
            let at = m.id.to_string();
            let column = header
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
            // An index into an array on the heap (D122): where the field's pointer points, as an element of where the
            // array's pointer points then.
            let into = match &m.into {
                Some(into) => {
                    let kind = crate::values::PointerKind::of_type(&m.ty, reader.types)
                        .ok_or_else(|| format!("{}: {} isn't a pointer", m.field, m.ty))??;
                    let base = into.base.to_string();
                    let base = header
                        .state
                        .iter()
                        .position(|item| item.via.is_none() && item.at == base && item.size == into.pointer.size())
                        .ok_or_else(|| {
                            format!("the trace of {name} lacks {}'s pointers: rrc record {name}", m.field)
                        })?;
                    Some((base, kind))
                }
                None => None,
            };
            columns.push((column, into));
        }
        Ok(Original { mapped, reader, columns, hex: vec![None; header.state.len()], last: vec![None; mapped.len()] })
    }

    /// Each field's value at `tick`, the next tick of the trace.
    pub(crate) fn values(&mut self, tick: &crate::trace::Tick) -> Result<Vec<Value>, String> {
        for (column, value) in &tick.state {
            if let Some(hex) = self.hex.get_mut(*column) {
                hex.clone_from(value);
            }
        }
        let mut values = Vec::with_capacity(self.mapped.len());
        for (i, (m, (column, into))) in self.mapped.iter().zip(&self.columns).enumerate() {
            let hex = &self.hex[*column];
            if let (Some(into), Some((base, kind))) = (&m.into, into) {
                let bytes = |hex: &Option<String>| hex.as_deref().and_then(|h| unhex(h).ok()).unwrap_or_default();
                values.push(pointer_index(&bytes(hex), kind, &bytes(&self.hex[*base]), into));
                continue;
            }
            let value = match &self.last[i] {
                Some((h, v)) if h == hex => v.clone(),
                _ => match hex {
                    Some(h) => decoded(m, &unhex(h)?, self.reader).map_err(|e| format!("{}: {e}", m.field))?,
                    None => Value::Null,
                },
            };
            self.last[i] = Some((hex.clone(), value.clone()));
            values.push(value);
        }
        Ok(values)
    }
}

/// The original at a tick, as the comparer takes it (D192): the mapped fields' values, the screen there (the last
/// that changed), the sound in the pass to it.
pub(crate) struct At {
    pub tick: u64,
    pub values: Vec<Value>,
    pub screen: Option<String>,
    pub sound: serde_json::Map<String, Value>,
    /// The tick's sync point, where the original's loop took the pass that ended at the next (a play followed: D207).
    pub sync: Option<String>,
    /// The pictures its display showed in the pass, the recording's (D180): none in a play followed live, which keeps
    /// them by their hashes alone (D196).
    pub frames: Vec<Value>,
}

/// A pointer's bytes (`own`, of `kind`) read as the index of the element it points to in the array `into` names,
/// whose pointer's bytes are `base`: null where either is null or it points elsewhere (D122, D178).
fn pointer_index(own: &[u8], kind: &crate::values::PointerKind, base: &[u8], into: &crate::mapping::Into) -> Value {
    let apart = match (kind.target(own, 0), into.pointer.target(base, into.offset)) {
        (Some(Target::Linear(p)), Some(Target::Linear(b))) => p.checked_sub(b),
        (Some(Target::Id(p)), Some(Target::Id(b))) if p.module() == b.module() && p.part() == b.part() => {
            p.offset().checked_sub(b.offset())
        }
        _ => None,
    };
    match apart.filter(|d| d % into.width == 0 && d / into.width < into.count) {
        Some(d) => Value::from(d / into.width),
        None => Value::Null,
    }
}

/// A field's value from its item's bytes: as its type reads them (its mask's bits), or a list's nodes, one
/// after another, as an array (D122).
fn decoded(m: &Mapped, bytes: &[u8], r: &Reader) -> Result<Value, String> {
    if m.via.as_ref().is_some_and(|v| v.next.is_some()) {
        let nodes =
            bytes.chunks_exact(m.size as usize).map(|node| decode(node, &m.ty, r)).collect::<Result<Vec<_>, _>>()?;
        return Ok(Value::Array(nodes));
    }
    // A pointer as whether it is set (D178).
    if m.set {
        return Ok(Value::Bool(bytes.iter().any(|&b| b != 0)));
    }
    Ok(m.masked(&decode(bytes, &m.ty, r)?))
}

/// The fields the port gets set by the scenario's edits after tick `after` (D64), by tick: the mapped
/// leaves each edit wrote, with the original's values at its tick; an item through a pointer is where the pointer
/// points at that tick (D147). An edit of memory nothing maps is said: the port can't get it.
fn edit_sets(
    repo: &Repo,
    name: &str,
    trace: &Trace,
    mapped: &[Mapped],
    original: &[Column],
    types: &Types,
    after: u64,
) -> Result<BTreeMap<u64, serde_json::Map<String, Value>>, String> {
    let mut sets: BTreeMap<u64, serde_json::Map<String, Value>> = BTreeMap::new();
    for edit in &trace.header.edits {
        let (Some(tick), Some(at), Some(hex)) = (edit["tick"].as_u64(), edit["at"].as_str(), edit["hex"].as_str())
        else {
            continue;
        };
        // Before the port starts, or past the ticks compared (`--to`).
        if tick <= after || original.first().is_some_and(|c| !c.holds((tick - 1) as usize)) {
            continue;
        }
        let id = crate::knowledge::parse_id(at)?;
        let (start, end) = (id.offset(), id.offset() + hex.len() as u32 / 2);
        let mut covered = 0;
        for (m, values) in mapped.iter().zip(original) {
            // A list's nodes and a chain's last pointer aren't followed here: a pointer's own target is.
            if m.via.as_ref().is_some_and(|via| !via.then.is_empty() || via.next.is_some()) {
                continue;
            }
            let Some(base) = located(repo, name, trace, m, 0, tick)? else {
                continue;
            };
            if (base.module(), base.part()) != (id.module(), id.part()) {
                continue;
            }
            let value = &values[(tick - 1) as usize];
            let set = sets.entry(tick).or_default();
            covered += edited_leaves(m, base.offset(), (start, end), value, types, set)?;
        }
        if sets.get(&tick).is_some_and(serde_json::Map::is_empty) {
            sets.remove(&tick);
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

/// Whether an edit's `set` wrote the leaf at `at`: it, or an item holding it (an edit over bytes no field names sets the
/// whole item).
fn edit_wrote(set: &serde_json::Map<String, Value>, at: &str) -> bool {
    set.keys()
        .any(|key| at.strip_prefix(key.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '['])))
}

/// The leaves of `m`, at `base` in memory now, that an edit of the bytes `start..end` wrote, each with its value in
/// `value` (the field's at the edit's tick), into `set`. How many of the edit's bytes they cover.
fn edited_leaves(
    m: &Mapped,
    base: u32,
    (start, end): (u32, u32),
    value: &Value,
    types: &Types,
    set: &mut serde_json::Map<String, Value>,
) -> Result<u32, String> {
    let (mut offset, to) = (start.max(base), end.min(base + m.size));
    let mut covered = 0;
    while offset < to {
        let Some((path, leaf, inner)) = crate::values::place(&m.ty, offset - base, types) else {
            break;
        };
        let width = crate::values::size(&leaf, types)? - inner;
        set.insert(format!("{}{path}", m.field), crate::values::at_leaf(value, &path).cloned().unwrap_or_default());
        covered += width.min(to - offset);
        offset += width;
    }
    Ok(covered)
}

/// Feeds the port the original's values of `fields` (mapped fields, or the groups they are in: `random`
/// for `random.seed`) at every tick after `after`, before it steps (D84): a subsystem not ported yet
/// gives the one being ported what it reads, and matches by construction. The mapped fields fed.
fn feed(
    feeds: &mut BTreeMap<u64, serde_json::Map<String, Value>>,
    mapped: &[Mapped],
    original: &[Column],
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
fn feed_field(feeds: &mut BTreeMap<u64, serde_json::Map<String, Value>>, m: &Mapped, values: &Column, after: u64) {
    for (i, value) in values.from(after as usize) {
        feeds.entry(i as u64 + 1).or_default().insert(m.field.clone(), value.clone());
    }
}

/// The session's inputs, tick by tick (`rrc-inputs`): from the trace, what the port gets; the fields the
/// scenario's edits set (`set`), and the fed fields' values where they changed (`fed`, D124).
fn write_inputs(
    trace: &Trace,
    sets: &BTreeMap<u64, serde_json::Map<String, Value>>,
    feeds: &BTreeMap<u64, serde_json::Map<String, Value>>,
    played: Option<&BTreeMap<u64, Value>>,
    path: &str,
) -> Result<(), String> {
    let mut header = inputs_header(&trace.header);
    if played.is_some() {
        header["version"] = json!(7);
    }
    let mut text = header.to_string();
    text.push('\n');
    // The start-up's pass, from power-on (D134): tick 0, before the first.
    let all: Vec<&crate::trace::Tick> =
        trace.header.power_on.as_deref().into_iter().chain(trace.ticks.iter()).collect();
    for (n, t) in all.iter().enumerate() {
        let mut line = inputs_line(t, all.get(n + 1).copied(), sets.get(&t.tick), feeds.get(&t.tick));
        if let Some(devices) = played.and_then(|p| p.get(&t.tick)) {
            line["devices"] = devices.clone();
        }
        text.push_str(&line.to_string());
        text.push('\n');
    }
    text.push_str(&json!({"end": trace.end.end, "ticks": trace.ticks.len()}).to_string());
    text.push('\n');
    std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))
}

/// The inputs' header (`rrc-inputs`) for a trace with `header`.
pub(crate) fn inputs_header(header: &crate::trace::Header) -> Value {
    let inputs: Vec<Value> = header
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
    let mut line = json!({"format": "rrc-inputs", "version": 4, "session": header.session, "inputs": inputs});
    if !header.hooks.is_empty() {
        let hooks: Vec<Value> =
            header.hooks.iter().map(|h| json!({"name": h.name, "registers": h.registers})).collect();
        line["hooks"] = json!(hooks);
    }
    line
}

/// A tick's inputs line: what its pass reads (`next`, the tick after it, has how many times the original went round
/// the wait the pass ends at), the fields a scenario's edit sets there, the fields fed there (D124).
pub(crate) fn inputs_line(
    t: &crate::trace::Tick,
    next: Option<&crate::trace::Tick>,
    set: Option<&serde_json::Map<String, Value>>,
    fed: Option<&serde_json::Map<String, Value>>,
) -> Value {
    let read: Vec<Value> = t.inputs.iter().filter_map(|(i, v)| v.as_ref().map(|v| json!([i, v]))).collect();
    let mut line = json!({"tick": t.tick, "sync": t.sync, "inputs": read});
    // How many times the original went round the wait the pass ends at: the next tick's skipped hits (D130).
    if let Some(spun) = next.and_then(|next| next.skipped) {
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
    if let Some(set) = set {
        line["set"] = json!(set);
    }
    if let Some(fed) = fed {
        line["fed"] = json!(fed);
    }
    line
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
    /// What it counts (`Census::COUNTS`): one made before it counted all it does now holds for nothing (D229).
    #[serde(default)]
    pub counts: u32,
    /// The recording it is of, and the port's code that made it (`sources::fingerprint`): a census holds for those
    /// (D158).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub recorded: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub port: String,
    /// The port's own passes compared.
    pub passes: u64,
    /// Fields its pass left otherwise than the original's.
    pub fields: BTreeMap<String, Seen>,
    /// Fields it leaves unset (null): not ported.
    pub unset: BTreeMap<String, Seen>,
    pub screen: Option<Seen>,
    /// The pictures it showed between ticks (D180) where one isn't among the original's in the pass, in order (D229).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pictures: Option<Seen>,
    pub sound: Option<Seen>,
    /// What it noted (`rrc_runtime::note`, D115): paths not ported.
    pub notes: BTreeMap<String, Seen>,
    /// Where the port stopped in a play followed as it goes (D193): it panicked, or wrote no line for far longer than
    /// its slowest tick took; each time started again from the original's state.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panics: Vec<Seen>,
    /// Where a play followed went past what the port knows (D207): its loop took its passes at another sync point than
    /// the original's, `LOST` ticks running; until it took them at the original's again as many.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lost: Vec<Lost>,
}

/// A stretch of a play followed where the port's loop was at another screen than the original's (D207): from which
/// tick, to which (none: to the end), and the sync points it was found lost at, the port's and the original's.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Lost {
    pub from: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<u64>,
    pub port: String,
    pub original: String,
}

impl Lost {
    /// Its line: from where, why, to where.
    pub(crate) fn text(&self, name: &str) -> String {
        let to = match self.to {
            Some(to) => format!("to tick {to}, where it took them at the original's again"),
            None => "to the end".to_owned(),
        };
        format!(
            "the port past what it knows from {name}'s tick {}: its loop took its passes from {} where the original's \
             took them from {}, {to}; what differs there is counted, not said",
            self.from, self.port, self.original
        )
    }
}

/// The ticks running a port's loop takes its passes at another sync point than the original's, or at the original's
/// again, before it is taken to be lost, or back (D207).
pub(crate) const LOST: u32 = 3;

/// Tick `t`'s pass in `lost`'s stretches (D207): `runs` the ticks running at another sync point and at the original's.
fn lost_at(lost: &mut Vec<Lost>, runs: &mut [u32; 2], t: u64, port: Option<&str>, original: Option<&str>) {
    let (Some(port), Some(original)) = (port, original) else { return };
    let open = lost.last().is_some_and(|l| l.to.is_none());
    if port == original {
        *runs = [0, runs[1] + 1];
        if open
            && runs[1] == LOST
            && let Some(last) = lost.last_mut()
        {
            last.to = Some(t + 1 - u64::from(LOST));
        }
    } else {
        *runs = [runs[0] + 1, 0];
        if !open && runs[0] == LOST {
            lost.push(Lost {
                from: t + 1 - u64::from(LOST),
                to: None,
                port: port.to_owned(),
                original: original.to_owned(),
            });
        }
    }
}

impl Census {
    /// What a census counts now: 1, the pictures between ticks too (D229).
    pub const COUNTS: u32 = 1;

    /// Whether the port's state, frames or sound differ anywhere, or it stopped.
    pub fn differs(&self) -> bool {
        !self.fields.is_empty()
            || self.screen.is_some()
            || self.pictures.is_some()
            || self.sound.is_some()
            || !self.panics.is_empty()
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
        for (kind, mine, theirs) in [
            ("screen", &self.screen, &other.screen),
            ("pictures", &self.pictures, &other.pictures),
            ("sound", &self.sound, &other.sound),
        ] {
            if let (None, Some(seen)) = (mine, theirs) {
                new.push((kind, "", seen));
            }
        }
        for seen in other.panics.iter().skip(self.panics.len()) {
            new.push(("panic", "", seen));
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
            "pictures" => {
                format!("the port's pictures between ticks differ at {} of {passes} ticks, {at}{what}", seen.ticks)
            }
            "sound" => format!("the port's sound differs at {} of {passes} ticks, {at}{what}", seen.ticks),
            "panic" => format!("the port stopped in the pass to {}'s tick {}: {}", seen.session, seen.tick, seen.what),
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
        for (kind, seen) in [("screen", &self.screen), ("pictures", &self.pictures), ("sound", &self.sound)] {
            if let Some(seen) = seen {
                let (kind, text) = Census::line(kind, "", seen, self.passes);
                say(&kind, text);
            }
        }
        for seen in &self.panics {
            let (kind, text) = Census::line("panic", "", seen, self.passes);
            say(&kind, text);
        }
        for lost in &self.lost {
            say("lost", lost.text(name));
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

    /// The session's census, as `rrc compare --lockstep` or `rrc work --run` last left it.
    pub fn load(repo: &Repo, name: &str) -> Result<Option<Census>, String> {
        let path = repo.state()?.join("compare").join(name).join("census.json");
        let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
        serde_json::from_str(&text).map(Some).map_err(|e| format!("{}: {e}", path.display()))
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
    Ok(counted(repo, name, false, false)?.0)
}

/// `rrc compare SESSION --live` (D194): the port in lockstep, its own input handling (the platform's `Live`) given at
/// each of the original's sync points the devices as the original's run had them there (the session's input, at the
/// play's time); what it made judged against what the original's pass read.
pub(crate) fn live(repo: &Repo, name: &str) -> Result<Lived, String> {
    counted(repo, name, false, true)?.1.ok_or_else(|| "the port wrote no check of its input handling".into())
}

/// `rrc compare SESSION --corrected` (D191): the port runs free, on its own state, and where a tick's fields differ
/// from the original's, the original's values are set over them before that tick's step, a tick at a time as the
/// port writes it. Every difference is counted where it arises, as lockstep counts it, without the cascade a free
/// run would have after it; where the mapping is exact, the census is lockstep's.
pub(crate) fn corrected(repo: &Repo, name: &str) -> Result<Census, String> {
    Ok(counted(repo, name, true, false)?.0)
}

/// Lockstep's or the corrected run's census (D124, D191): the comparer takes the port's lines a tick at a time.
fn counted(repo: &Repo, name: &str, correcting: bool, live: bool) -> Result<(Census, Option<Lived>), String> {
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
        original_values(&trace, &mapped, &crate::values::of(&k).with_linear(&linear), name, 0..trace.ticks.len())?
    };
    let census = Census {
        counts: Census::COUNTS,
        recorded: recorded.hash.clone(),
        port: crate::impact::fingerprint(repo)?,
        ..Census::default()
    };
    let ticks = trace.ticks.len() as u64;
    if ticks < 2 {
        return Ok((census, None));
    }
    let work = repo.state()?.join("compare").join(name);
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let file = |f: &str| work.join(f).to_string_lossy().into_owned();
    // A session that starts at power-on is counted from there, as the port starts it, with the program's own initial
    // data (D135, D137): a transplant at its first tick would leave out what the port's power-on sets up that no
    // mapping carries (a menu's or a creation's own state, D164). Its first tick is fed too, as every pass is.
    // (A session from another's tick has the start-up's pass in its trace too, but starts where its base was: D118.)
    let from_power_on = session.from.is_none() && trace.header.power_on.is_some();
    let after = 1;
    let fed_from = if from_power_on { 0 } else { after };
    let sets = edit_sets(repo, name, &trace, &mapped, &original, &k.types, after)?;
    let mut feeds = BTreeMap::new();
    if !correcting {
        for (m, values) in mapped.iter().zip(&original) {
            feed_field(&mut feeds, m, values, fed_from);
        }
    }
    // The devices at each sync point, for the port's own input handling checked beside (D194).
    let played = if live { Some(played(repo, &session, &k, &trace)?) } else { None };
    write_inputs(&trace, &sets, &feeds, played.as_ref(), &file("inputs.jsonl"))?;
    let mut port_args =
        vec!["run".to_owned(), "--inputs".into(), file("inputs.jsonl"), "--out".into(), file("port.jsonl")];
    if live {
        port_args.push("--live".into());
    }
    if from_power_on {
        let mut initial = mapped.clone();
        initial.extend(mapping.resolve_carried(&k)?);
        if write_initial(repo, &trace.header.session, &initial, &k, &file("initial.json"))? {
            port_args.extend(["--initial".into(), file("initial.json")]);
        }
    } else {
        let carried = read_given(repo, &session, &k, &mapping.resolve_carried(&k)?, after)?;
        write_start(&trace.header.session, &mapped, &original, after, carried, &file("start.json"))?;
        port_args.extend(["--transplant".into(), file("start.json")]);
    }
    if !presented.is_empty() && !from_power_on {
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
        if let Some(hash) = screens[(after - 1) as usize].as_ref().filter(|_| !from_power_on) {
            std::fs::write(file("screen.frm"), cache.get(hash)?).map_err(|e| e.to_string())?;
            port_args.extend(["--screen".into(), file("screen.frm")]);
        }
    }
    let saves = work.join("saves");
    write_saves(repo, &session, &k, (!from_power_on).then_some(after), &saves)?;
    let mut counting = Counting::new(name, &mapped, &sets, &cache, work.join("frames"), after, correcting, census);
    // The original's ticks given as the port's lines reach them, not all at once: a long play's mapped values, each
    // tick's whole (a level's map in it), are gigabytes (D213; D208 kept them as their changes).
    let mut given = 0;
    let mut on_line = |v: &Value| {
        let upto = v["tick"].as_u64().unwrap_or(0);
        while let Some(tick) = trace.ticks.get(given).filter(|t| t.tick <= upto) {
            counting.original(At {
                tick: tick.tick,
                values: original.iter().map(|column| column[given].clone()).collect(),
                screen: screens[given].clone(),
                sound: tick.sound.clone(),
                sync: None,
                frames: tick.frames.clone(),
            });
            given += 1;
        }
        counting.line(v)
    };
    if correcting {
        run_port(repo, &session, &port_args, Some(&saves), Some(&mut on_line))?;
    } else {
        run_port(repo, &session, &port_args, Some(&saves), None)?;
        let port_trace = std::fs::File::open(file("port.jsonl")).map_err(|e| format!("the port's trace: {e}"))?;
        for line in std::io::BufRead::lines(std::io::BufReader::new(port_trace)) {
            let line = line.map_err(|e| format!("the port's trace: {e}"))?;
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(&line).map_err(|e| format!("the port's trace: {e}"))?;
            on_line(&v)?;
        }
    }
    if correcting {
        let c = &counting.corrected;
        let most: Vec<String> = c.fields.iter().map(|(f, n)| format!("{f} ({n})")).take(5).collect();
        match (c.ticks, c.noted) {
            (0, 0) => say("corrected", "nothing: the port's free run is the original's at every tick"),
            (0, _) => say("corrected", "nothing it differed in: the port's free run is the original's"),
            (n, _) => say("corrected", format!("at {n} ticks: {}", most.join(", "))),
        }
        if c.noted > 0 {
            say(
                "noted",
                format!("at {} ticks: every field set to the original's after a pass the port noted", c.noted),
            );
        }
    }
    let lived = match live {
        true => Some(judge(&trace, &[file("port.jsonl").into()], played.as_ref().unwrap_or(&BTreeMap::new()))?),
        false => None,
    };
    Ok((counting.census, lived))
}

/// What the player did up to each tick's sync point, as the session's run had it (D194): its input from where it
/// starts (Clockwork's `queued`, once the session is loaded), as [`devices`] gives a tick's. By tick, as an inputs
/// line's `devices`.
fn played(repo: &Repo, session: &Session, k: &Knowledge, trace: &Trace) -> Result<BTreeMap<u64, Value>, String> {
    let (mut clockwork, _, _) = crate::trace::begin(repo, session, &crate::trace::sync_points(k)?)?;
    let queued = clockwork.ask("queued", Value::Null)?;
    let ips = queued["ips"].as_u64().filter(|&i| i > 0).ok_or("the machine's speed")?;
    let mut events = std::collections::VecDeque::new();
    for line in queued["events"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        let cycle: u64 =
            line.split(' ').next().and_then(|c| c.parse().ok()).ok_or_else(|| format!("an input: {line:?}"))?;
        events.push_back((cycle, line.to_owned()));
    }
    let mut played = BTreeMap::new();
    for (i, t) in trace.ticks.iter().enumerate() {
        let n = events.iter().take_while(|(cycle, _)| *cycle <= t.cycle).count();
        let came: Vec<(u64, String)> = events.drain(..n).collect();
        played.insert(t.tick, devices(&came, t.cycle, trace.ticks.get(i + 1).map(|after| after.cycle), ips));
    }
    Ok(played)
}

/// A tick's devices (an inputs line's `devices`, D194): its time and the next tick's (µs from power-on, at `ips`
/// cycles a second), and the input that `came` since the tick before (`<cycle> K 1E` lines, as a play file and
/// Clockwork's `queued` have them): the keyboard controller's bytes and the mouse's moves (in the screen's pixels, to a
/// fraction where the driver counts finer, with its buttons), each at its time.
pub(crate) fn devices(came: &[(u64, String)], cycle: u64, next: Option<u64>, ips: u64) -> Value {
    let us = |cycle: u64| (u128::from(cycle) * 1_000_000 / u128::from(ips.max(1))) as u64;
    let (mut keys, mut mouse) = (Vec::new(), Vec::new());
    for (at, line) in came {
        let words: Vec<&str> = line.split(' ').skip(1).collect();
        let number = |i: usize| words.get(i).and_then(|w| w.parse::<i64>().ok()).unwrap_or(0);
        match words.first().copied() {
            Some("K") => {
                keys.extend(words.get(1).and_then(|w| u8::from_str_radix(w, 16).ok()).map(|k| json!([us(*at), k])));
            }
            // The driver's coordinates in the screen's pixels: mode 13h's counts two across one (321 is 160.5).
            Some("M") => mouse.push(json!([us(*at), number(1) as f64 / 2.0, number(2), number(3)])),
            _ => {}
        }
    }
    let mut devices = json!({"us": us(cycle), "keys": keys, "mouse": mouse});
    if let Some(next) = next {
        devices["next"] = json!(us(next));
    }
    devices
}

/// What a port's own input handling made at the original's sync points, judged (D194): the ticks it made as the
/// original's handlers did; by input, those one pass early or late (timing, as a read in the middle of a pass sees an
/// event one read early or late) and those otherwise (a difference); the ticks its loop would wait where the
/// original's took a pass (timing too), or take one from another sync point.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Lived {
    pub ticks: u64,
    pub same: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub differs: BTreeMap<String, Seen>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub timing: BTreeMap<String, Seen>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waits: Option<Seen>,
    /// Its loop took no pass by the original's next sync point, more than two frames on: a pass skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skips: Option<Seen>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub syncs: Option<Seen>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends: Option<Seen>,
}

impl Lived {
    /// Whether the port's input handling made anything otherwise than the original's handlers, timing aside.
    pub fn differs(&self) -> bool {
        !self.differs.is_empty() || self.syncs.is_some() || self.ends.is_some() || self.skips.is_some()
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// What it found, a line each.
    pub fn show(&self, name: &str) {
        let at = |seen: &Seen| {
            let what = if seen.what.is_empty() { String::new() } else { format!(" ({})", seen.what) };
            format!("in {} ticks, first at {name}'s tick {}{what}", seen.ticks, seen.tick)
        };
        if self.ticks == 0 {
            say("live", format!("{name}: the port's input handling isn't checked: a port without the platform's Live"));
            return;
        }
        say("live", format!("{}: {} of {} ticks as the original's handlers made them", name, self.same, self.ticks));
        for (input, seen) in &self.differs {
            say("differs", format!("{input}: {}", at(seen)));
        }
        if let Some(seen) = &self.syncs {
            say("differs", format!("the pass from another sync point: {}", at(seen)));
        }
        if let Some(seen) = &self.ends {
            say("differs", format!("the port's loop ends where the original's took a pass: {}", at(seen)));
        }
        if let Some(seen) = &self.skips {
            say("differs", format!("the port's loop takes no pass by the original's next: {}", at(seen)));
        }
        for (input, seen) in &self.timing {
            say("timing", format!("{input}: a moment apart, {}", at(seen)));
        }
        if let Some(seen) = &self.waits {
            say("timing", format!("the port's loop waits where the original's took a pass: {}", at(seen)));
        }
    }
}

/// The port's checks of its input handling (`live` on its traces' lines, `ports`: a run's, and a run started again's
/// after it) against the original's inputs at each tick of `trace` (`played`: each tick's devices, its time among
/// them).
pub(crate) fn judge(
    trace: &Trace,
    ports: &[std::path::PathBuf],
    played: &BTreeMap<u64, Value>,
) -> Result<Lived, String> {
    let due = |t: u64| played.get(&t).and_then(|d| d["us"].as_u64());
    // The platform's frame (µs): 70 a second.
    const FRAME: u64 = 1_000_000 / 70;
    // The inputs each pass read in its middle (late reads, D60), and what it read: an event there the port's live play
    // sees a pass on, and the values the original's handlers held in the pass, a moment from its sync points.
    let read_late: BTreeMap<u64, std::collections::BTreeSet<usize>> =
        trace.ticks.iter().map(|t| (t.tick, t.late.iter().map(|l| l.input).collect())).collect();
    let late_values = |t: u64, input: usize| -> Vec<String> {
        trace.ticks.get((t as usize).wrapping_sub(1)).map_or_else(Vec::new, |tick| {
            tick.late.iter().filter(|l| l.input == input).filter_map(|l| l.value.clone()).collect()
        })
    };
    let names: BTreeMap<&str, usize> =
        trace.header.inputs.iter().enumerate().map(|(i, item)| (item.name.as_str(), i)).collect();
    let values: Vec<Vec<Option<String>>> = (0..trace.header.inputs.len()).map(|i| trace.input_values(i)).collect();
    // The port's traces a line at a time (D208): a long play's are a gigabyte.
    let readers = ports
        .iter()
        .map(|port| {
            std::fs::File::open(port).map(std::io::BufReader::new).map_err(|e| format!("{}: {e}", port.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut lived = Lived::default();
    let seen = |slot: &mut Option<Seen>, t: u64, what: String| match slot {
        Some(seen) => seen.ticks += 1,
        None => *slot = Some(Seen { ticks: 1, session: trace.header.session.clone(), tick: t, what }),
    };
    for line in readers.into_iter().flat_map(std::io::BufRead::lines) {
        let line = line.map_err(|e| format!("the port's trace: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).map_err(|e| format!("the port's trace: {e}"))?;
        let (Some(t), Some(live)) = (v["tick"].as_u64(), v.get("live")) else { continue };
        let Some(tick) = trace.ticks.get((t as usize).wrapping_sub(1)) else { continue };
        lived.ticks += 1;
        // Its loop still waiting at the original's next sync point: a pass skipped, but where the original's next came
        // within two of the platform's frames (passes closer than its frames: a moment).
        if live["wait"] == true {
            // (The last tick has no next to wait for: a moment.)
            let close = due(t).zip(due(t + 1)).is_none_or(|(at, next)| next.saturating_sub(at) <= 2 * FRAME);
            seen(if close { &mut lived.waits } else { &mut lived.skips }, t, String::new());
            continue;
        }
        if live["end"] == true {
            seen(&mut lived.ends, t, String::new());
            continue;
        }
        let mut same = true;
        if live["sync"].as_str() != Some(tick.sync.as_str()) {
            seen(&mut lived.syncs, t, format!("the port's from {}, the original's from {}", live["sync"], tick.sync));
            same = false;
        }
        // The port's pass more than a frame from the original's (a pass of the original's that took long, which the
        // port's doesn't): what the time gives it is a moment apart, timing.
        let apart = match (live["us"].as_u64(), live["due"].as_u64()) {
            (Some(us), Some(due)) if us.abs_diff(due) > FRAME => Some(if us < due {
                format!("its pass {} ms early", (due - us) / 1000)
            } else {
                format!("its pass {} ms late", (us - due) / 1000)
            }),
            _ => None,
        };
        for (input, made) in live["inputs"].as_object().into_iter().flatten() {
            let (Some(&i), Some(made)) = (names.get(input.as_str()), made.as_str()) else { continue };
            let at = |t: u64| values[i].get((t as usize).wrapping_sub(1)).cloned().flatten();
            let Some(theirs) = at(t) else { continue };
            if theirs.eq_ignore_ascii_case(made) {
                continue;
            }
            same = false;
            // Byte by byte: one the original's a pass before or after is timing (a key's moment, a count's), any other
            // a difference; the first of each said.
            let byte = |hex: &str, k: usize| hex.get(2 * k..2 * k + 2).unwrap_or("--").to_ascii_uppercase();
            // The original's values a pass before and after, and at the ticks within a frame of when the port's pass
            // came: what the time there gave the original's handlers.
            let when = live["us"].as_u64();
            let mut others: Vec<String> = (t.saturating_sub(8)..=t + 8)
                .filter(|&u| u != t)
                .filter(|&u| u.abs_diff(t) == 1 || when.zip(due(u)).is_some_and(|(us, at)| us.abs_diff(at) <= FRAME))
                .filter_map(at)
                .collect();
            others.extend(late_values(t.wrapping_sub(1), i));
            let near = |k: usize, mine: &str| others.iter().any(|v| byte(v, k) == mine);
            let bytes = theirs.len().max(made.len()) / 2;
            let apart_bytes: Vec<usize> = (0..bytes).filter(|&k| byte(&theirs, k) != byte(made, k)).collect();
            // An event the original's pass before read in its middle: the port's live play sees it a pass on.
            let inside = read_late.get(&t.wrapping_sub(1)).is_some_and(|inputs| inputs.contains(&i));
            // The port's pass came before the original's moment, and its handlers asked there make the original's
            // (`then`, D197): the moment alone differs.
            let then = live["then"]["inputs"][input].as_str().is_some_and(|v| v.eq_ignore_ascii_case(&theirs));
            // A byte the original's tick has as the port's pass before wrote it itself, with the player's input come
            // in that pass (D197): there the write came after the input, here, the pass at once, before it.
            let came = played.get(&t).is_some_and(|d| {
                d["keys"].as_array().is_some_and(|k| !k.is_empty())
                    || d["mouse"].as_array().is_some_and(|m| !m.is_empty())
            });
            let wrote_over = |k: usize| {
                came && live["wrote"][input].as_array().into_iter().flatten().any(|w| {
                    w[0].as_u64() == Some(k as u64)
                        && w[1].as_u64().is_some_and(|b| format!("{b:02X}") == byte(&theirs, k))
                })
            };
            let wrong = apart_bytes
                .iter()
                .copied()
                .find(|&k| apart.is_none() && !inside && !then && !wrote_over(k) && !near(k, &byte(made, k)));
            let k = wrong.or(apart_bytes.first().copied()).unwrap_or(0);
            let mut what = format!("byte {k}: original {}, port {}", byte(&theirs, k), byte(made, k));
            if let Some(apart) = &apart {
                what = format!("{what}; {apart}");
            } else if inside && wrong.is_none() {
                what = format!("{what}; read in the middle of the pass before");
            } else if then && wrong.is_none() {
                let early = live["due"].as_u64().zip(live["us"].as_u64()).map_or(0, |(d, u)| d.saturating_sub(u));
                what = format!("{what}; its pass {} ms early, the original's at its moment", early / 1000);
            } else if wrong.is_none() && wrote_over(k) {
                what = format!("{what}; the pass before wrote it, the player's input came in it");
            }
            let map = if wrong.is_some() { &mut lived.differs } else { &mut lived.timing };
            match map.get_mut(input) {
                Some(seen) => seen.ticks += 1,
                None => {
                    map.insert(input.clone(), Seen { ticks: 1, session: trace.header.session.clone(), tick: t, what });
                }
            }
        }
        if same {
            lived.same += 1;
        }
    }
    Ok(lived)
}

/// The comparison a tick at a time (D191): the port's trace's lines as they come, against the original's ticks, the
/// census kept as it goes: the fields the port's own pass left otherwise, the frames and sound that differ, what it
/// noted. Lockstep's own values are what the feed overrode (the rest matched it); a free run's, its state's.
pub(crate) struct Counting<'a> {
    name: &'a str,
    mapped: &'a [Mapped],
    /// The original's ticks given and not compared yet, and the last given (D192).
    ticks: std::collections::VecDeque<At>,
    newest: u64,
    sets: &'a BTreeMap<u64, serde_json::Map<String, Value>>,
    cache: &'a Cache,
    frames: std::path::PathBuf,
    /// The tick the port starts at: its pass to it was the original's.
    after: u64,
    /// A free run (corrected): every field compared with the port's state.
    free: bool,
    made: Vec<String>,
    /// A free run's state as its lines leave it (each gives the top-level fields that changed).
    own: Value,
    screens_seen: Screens,
    /// The pictures between ticks compared (D229).
    between: Between,
    pub(crate) census: Census,
    corrected: Corrections,
    /// The last tick of the port's trace.
    compared: u64,
    /// The ticks running the port's loop took its pass at another sync point than the original's, and at the
    /// original's again (D207).
    off: u32,
    on: u32,
}

/// What a corrected run set (D191).
#[derive(Default)]
struct Corrections {
    /// The ticks some field differed or was unset at, and how many times each was.
    ticks: u64,
    fields: BTreeMap<String, u64>,
    /// The ticks after a pass the port noted (D115): its state there isn't its own, so every field is set.
    noted: u64,
}

impl<'a> Counting<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        name: &'a str,
        mapped: &'a [Mapped],
        sets: &'a BTreeMap<u64, serde_json::Map<String, Value>>,
        cache: &'a Cache,
        frames: std::path::PathBuf,
        after: u64,
        free: bool,
        census: Census,
    ) -> Counting<'a> {
        Counting {
            name,
            mapped,
            ticks: std::collections::VecDeque::new(),
            newest: 0,
            sets,
            cache,
            frames,
            after,
            free,
            made: Vec::new(),
            own: Value::Object(serde_json::Map::new()),
            screens_seen: Screens::default(),
            between: Between::default(),
            census,
            corrected: Corrections::default(),
            compared: 0,
            off: 0,
            on: 0,
        }
    }

    /// Tick `t`'s pass as the port's loop took it (its `Live`'s sync point, `port`) against the original's: `LOST` ticks
    /// running at another, the port is past what it knows from the first (D207); as many at the original's again, it
    /// is back. A tick the port's loop took no pass at, or without either, counts neither way.
    fn lost(&mut self, t: u64, port: Option<&str>, original: Option<&str>) {
        let mut runs = [self.off, self.on];
        lost_at(&mut self.census.lost, &mut runs, t, port, original);
        [self.off, self.on] = runs;
    }

    /// The session compared.
    pub(crate) fn name(&self) -> &str {
        self.name
    }

    /// Whether the port is past what it knows now (D207).
    pub(crate) fn is_lost(&self) -> bool {
        self.census.lost.last().is_some_and(|l| l.to.is_none())
    }

    /// The port started again at tick `from` (D193): its passes to there aren't its own, and its state is what it
    /// writes from there.
    pub(crate) fn restart(&mut self, from: u64) {
        self.after = from;
        self.own = Value::Object(serde_json::Map::new());
        self.made = Vec::new();
    }

    /// The port stopped (`what`) in the pass after the last tick it wrote.
    pub(crate) fn stopped(&mut self, what: String) {
        let tick = self.compared + 1;
        self.census.panics.push(Seen { ticks: 1, session: self.name.to_owned(), tick, what });
    }

    /// The original at tick `tick`, if it is given and not compared yet.
    pub(crate) fn at(&self, tick: u64) -> Option<&At> {
        self.ticks.iter().find(|at| at.tick == tick)
    }

    /// The original's first tick not compared yet.
    pub(crate) fn first(&self) -> Option<&At> {
        self.ticks.front()
    }

    /// The last tick the port's trace gave.
    pub(crate) fn compared(&self) -> u64 {
        self.compared
    }

    /// The original's next tick.
    pub(crate) fn original(&mut self, at: At) {
        self.newest = at.tick;
        self.ticks.push_back(at);
    }

    /// A line of the port's trace: counted; for a free run's tick, the original's values of the fields it differs in
    /// there (all of them after a pass the port noted, D115), to set before the tick's step.
    pub(crate) fn line(&mut self, v: &Value) -> Result<serde_json::Map<String, Value>, String> {
        let mut correct = serde_json::Map::new();
        if v.get("format").is_some() {
            if v["format"] != "rrc-port-trace" || v["version"] != 1 {
                return Err(format!(
                    "the port's trace isn't rrc-port-trace version 1 ({} {})",
                    v["format"], v["version"]
                ));
            }
            self.made = match &v["sound"] {
                Value::Bool(true) => SOUND_DEVICES.iter().map(|(d, _)| (*d).to_owned()).collect(),
                Value::Array(devices) => devices.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
                _ => Vec::new(),
            };
            return Ok(correct);
        }
        let Some(t) = v["tick"].as_u64() else { return Ok(correct) };
        self.compared = t;
        let ticks = self.newest;
        if t == 0 || t > ticks {
            return Err(format!("the port's trace has tick {t}; the session has 1 to {ticks}"));
        }
        // The original's ticks before it are done with.
        while self.ticks.front().is_some_and(|at| at.tick < t) {
            self.ticks.pop_front();
        }
        let original = self.ticks.front().filter(|at| at.tick == t).and_then(|at| at.sync.clone());
        self.lost(t, v["live"]["sync"].as_str(), original.as_deref());
        if let (Some(changed), Value::Object(_)) = (v["state"].as_object(), &self.own) {
            rrc_runtime::apply_changes(&mut self.own, changed)?;
        }
        let (name, census) = (self.name, &mut self.census);
        let seen_at = |t: u64, what: String| Seen { ticks: 1, session: name.to_owned(), tick: t, what };
        let count = |map: &mut BTreeMap<String, Seen>, key: &str, t: u64, what: String| match map.get_mut(key) {
            Some(seen) => seen.ticks += 1,
            None => {
                map.insert(key.to_owned(), seen_at(t, what));
            }
        };
        let noted = v["notes"].as_object().is_some_and(|n| !n.is_empty());
        for (what, _) in v["notes"].as_object().into_iter().flatten() {
            count(&mut census.notes, what, t, String::new());
        }
        if let Some(frame) = v["frame"].as_str() {
            self.screens_seen.port(&self.frames.join(frame))?;
        }
        if t <= self.after {
            return Ok(correct);
        }
        census.passes += 1;
        let at = self
            .ticks
            .front()
            .filter(|at| at.tick == t)
            .ok_or(format!("the port's trace has tick {t} out of order"))?;
        // Each field's own value: what the feed overrode (lockstep: a field not there matched), or the state's.
        static NONE: Value = Value::Null;
        let own: Vec<(usize, &Value)> = if self.free {
            (0..self.mapped.len()).map(|i| (i, at_path(&self.own, &self.mapped[i].field).unwrap_or(&NONE))).collect()
        } else {
            let fields: BTreeMap<&str, usize> =
                self.mapped.iter().enumerate().map(|(i, m)| (m.field.as_str(), i)).collect();
            v["overrode"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(path, own)| fields.get(path.as_str()).map(|&i| (i, own)))
                .collect()
        };
        let mut fixed: Vec<String> = Vec::new();
        for (i, own) in own {
            let (m, theirs) = (&self.mapped[i], &at.values[i]);
            if own.is_null() {
                // None in both: memory the original freed (a pointer null), the port's field none.
                if self.free && theirs.is_null() {
                    continue;
                }
                count(&mut census.unset, &m.field, t, String::new());
                correct.insert(m.field.clone(), theirs.clone());
                fixed.push(m.field.clone());
                continue;
            }
            // What a scenario's edit wrote at this tick isn't the pass's doing (D146).
            let edited = self.sets.get(&t);
            let mut differ = Vec::new();
            let masked = m.mask.map(|_| m.masked(own));
            differences(
                &m.field,
                theirs,
                masked.as_ref().unwrap_or(own),
                edited.map_or(1, |_| usize::MAX),
                &mut differ,
            );
            if let Some((at, o, p)) = differ.iter().find(|(at, ..)| !edited.is_some_and(|s| edit_wrote(s, at))) {
                count(&mut census.fields, &m.field, t, format!("{at}: original {o}, port {p}"));
            }
            if !differ.is_empty() {
                fixed.push(m.field.clone());
            }
            // After a noted pass every field is the original's: those that aren't already, unmasked.
            if !differ.is_empty() || (noted && own != theirs) {
                correct.insert(m.field.clone(), theirs.clone());
            }
        }
        if let Some(hash) = &at.screen
            && let Some(diff) = self.screens_seen.compare(hash, self.cache)?
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
        // The pictures shown between ticks (D229): where the recording has the original's in the pass.
        if let Some(names) = v["between"].as_array().filter(|n| !n.is_empty())
            && !at.frames.is_empty()
        {
            let names: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
            let fleeting: Vec<&str> =
                v["fleeting"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            if let Some(miss) = self.between.check(t, &at.frames, &names, &fleeting, &self.frames, self.cache)? {
                let (picture, pictures, shown) = (ordinal(miss.picture + 1), miss.pictures, miss.shown);
                let what = if miss.from < miss.shown {
                    format!(
                        "its {picture} of {pictures} isn't among the original's from its {} of {shown} on",
                        ordinal(miss.from + 1)
                    )
                } else {
                    format!("its {picture} of {pictures} comes after the original's last of {shown}")
                };
                match &mut census.pictures {
                    Some(seen) => seen.ticks += 1,
                    None => census.pictures = Some(seen_at(t, what)),
                }
            }
        }
        let made: Vec<&str> = self.made.iter().map(String::as_str).collect();
        if !made.is_empty()
            && let Some(diff) = sound_differences(&at.sound, &v["sound"], &made)
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
        if !self.free {
            return Ok(serde_json::Map::new());
        }
        // A free run's own values carry on from the corrected ones.
        if !fixed.is_empty() {
            self.corrected.ticks += 1;
            for field in fixed {
                *self.corrected.fields.entry(field).or_default() += 1;
            }
        }
        if noted {
            self.corrected.noted += 1;
        }
        if !correct.is_empty() {
            for (path, value) in &correct {
                rrc_runtime::set_path(&mut self.own, path, value.clone())?;
            }
        }
        Ok(correct)
    }
}

/// A transplant (`rrc-state`): the mapped fields' values at `tick`.
/// `rrc start SESSION --tick N [--out DIR]` (D120): where the session's original is at tick N, for the port to
/// start from without the original running: the mapped fields' values (`state.json`, a transplant's), the
/// presentation's own state (`presentation.json`, D110) and the screen (`screen.frm`), in a folder. Tick 0 is
/// power-on (D141): the program's own initial data and the start-up's inputs (`start_power_on`).
pub(crate) fn start(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--tick", "--out"], &[])?;
    let [name] = a.positionals(1, 1, "start SESSION --tick N [--out DIR]")? else { unreachable!() };
    let tick: u64 = a.required("--tick")?.parse().ok().ok_or("--tick is a tick number")?;
    let session = Session::load(repo, name)?;
    let k = Knowledge::load(&repo.knowledge())?;
    let mapping = Mapping::load(repo)?;
    let (mapped, presented) = (mapping.resolve(&k)?, mapping.resolve_present(&k)?);
    let carried = mapping.resolve_carried(&k)?;
    let dir = match a.value("--out") {
        Some(dir) => std::path::PathBuf::from(dir),
        None => repo.state()?.join("start").join(format!("{name}-{tick}")),
    };
    let cache = Cache::open(&repo.local)?;
    // Its recording, when the cache has it (power-on needs none, D225).
    let recording = match &session.trace {
        Some(recorded) if tick == 0 && !cache.has(&recorded.hash) => None,
        Some(recorded) => Some(Trace::parse(&String::from_utf8_lossy(&cache.get(&recorded.hash)?))?),
        None if tick == 0 => None,
        None => return Err(format!("{name} isn't recorded: rrc record {name}")),
    };
    if tick == 0 {
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut initial = mapped;
        initial.extend(carried);
        return start_power_on(repo, (name, &session, recording.as_ref()), &k, &initial, &dir);
    }
    let trace = recording.ok_or_else(|| format!("{name} isn't recorded: rrc record {name}"))?;
    if tick > trace.ticks.len() as u64 {
        return Err(format!("{name} has {} ticks, not {tick}", trace.ticks.len()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = |f: &str| dir.join(f).to_string_lossy().into_owned();
    let original = {
        let places = Places::new(repo, &session);
        let linear = |linear: u32| places.linear_id(linear).ok();
        original_values(
            &trace,
            &mapped,
            &crate::values::of(&k).with_linear(&linear),
            name,
            (tick - 1) as usize..tick as usize,
        )?
    };
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
    let changed = write_saves(repo, &session, &k, Some(tick), &dir.join("saves"))?;
    if changed > 0 {
        has.push(format!("{changed} files the original changed"));
    }
    say("start", format!("{name}'s tick {tick}: {}", has.join(", ")));
    say("folder", crate::path_text(&dir));
    say("next", format!("the port plays from it: PORT play --start {}", crate::path_text(&dir)));
    Ok(ExitCode::SUCCESS)
}

/// `rrc start SESSION --tick 0` (D141): power-on, for the port to play from as a comparison runs it from there:
/// the program's own initial data (`state.json`, D135: the mapped and carried items at fixed places in its load
/// image, from the game's program as it is unpacked without an analysis, D224), the inputs the session's start-up
/// had at power-on (`power-on.json`, `rrc-power-on`: its command line, its clock, which a live platform may give
/// from the host instead), and an empty saves folder. The inputs are its recording's, or without one in the cache
/// those a recording would read where its run begins, the session loaded and nothing run (D225): no recording is
/// needed to play from power-on.
fn start_power_on(
    repo: &Repo,
    (name, session, trace): (&str, &Session, Option<&Trace>),
    k: &Knowledge,
    initial: &[Mapped],
    dir: &std::path::Path,
) -> Result<ExitCode, String> {
    let file = |f: &str| dir.join(f).to_string_lossy().into_owned();
    let mut has = Vec::new();
    if write_initial(repo, name, initial, k, &file("state.json"))? {
        has.push("the program's initial data".to_owned());
    } else {
        let empty = json!({"format": "rrc-state", "version": 1, "session": name, "tick": 0, "state": {}});
        std::fs::write(file("state.json"), format!("{empty}\n")).map_err(|e| e.to_string())?;
    }
    let inputs: serde_json::Map<String, Value> = match trace {
        Some(trace) => {
            let start_up = trace.header.power_on.as_deref().ok_or_else(|| {
                format!("{name} was recorded without its start-up's inputs (D134): rrc record {name} again")
            })?;
            start_up
                .inputs
                .iter()
                .filter_map(|(i, v)| Some((trace.header.inputs.get(*i)?.name.clone(), json!(v.as_ref()?))))
                .collect()
        }
        None => {
            if session.from.is_some() {
                return Err(format!("{name} starts from another session's tick, not power-on: rrc record {name}"));
            }
            let (inputs, as_it_is) = crate::trace::power_on_inputs(repo, k, session)?;
            if as_it_is {
                say("folder", "the game's as it is: the one the session was recorded on can't be built here");
            }
            inputs
        }
    };
    let names: Vec<&str> = inputs.keys().map(String::as_str).collect();
    let from = if trace.is_some() { "its recording's" } else { "read at its load, no recording" };
    has.push(format!("the start-up's inputs ({}; {from})", names.join(", ")));
    let power_on = json!({"format": "rrc-power-on", "version": 1, "session": name, "inputs": inputs});
    std::fs::write(file("power-on.json"), format!("{power_on}\n")).map_err(|e| e.to_string())?;
    write_saves(repo, session, k, None, &dir.join("saves"))?;
    say("start", format!("{name}'s power-on: {}", has.join(", ")));
    say("folder", crate::path_text(dir));
    say("next", format!("the port plays from it: PORT play --start {}", crate::path_text(dir)));
    Ok(ExitCode::SUCCESS)
}

/// The port's saves folder for a run from `tick` (D131): made afresh, with the files the original changed by
/// then (its drive's, as the game's folder holds the rest), and `deleted`, the names of those it deleted, a line
/// each. None from the session's start. The port saves into it and reads saved games from it first
/// (`rrc_runtime::original::saved`). How many files it holds.
/// The files the original changed (`original`, as `changed_files_at` gives them) against the port's saves folder
/// (`saves`: the files it saved, and those it deleted listed in `deleted`), D231: each that differs said, where it
/// first does; a file only one of them changed is said too.
fn files_differing(original: &[crate::peek::ChangedFile], saves: &std::path::Path) -> Result<Vec<String>, String> {
    let deleted_list = std::fs::read_to_string(saves.join(rrc_runtime::original::DELETED)).unwrap_or_default();
    let deleted: Vec<String> = deleted_list.lines().map(|l| l.replace('\\', "/").to_uppercase()).collect();
    let mut port: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut stack = vec![saves.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path.strip_prefix(saves).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if relative == rrc_runtime::original::DELETED {
                continue;
            }
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            port.insert(relative.to_uppercase(), bytes);
        }
    }
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (path, bytes) in original {
        let key = path.replace('\\', "/").to_uppercase();
        seen.insert(key.clone());
        match (bytes, port.get(&key)) {
            (Some(o), Some(p)) if o == p => {}
            (Some(o), Some(p)) => {
                let at = o.iter().zip(p).position(|(a, b)| a != b).unwrap_or(o.len().min(p.len()));
                let byte = |b: &[u8]| b.get(at).map_or("its end".to_owned(), |v| format!("{v:02X}h"));
                out.push(format!(
                    "{path}: differs from byte {at} (original {}, port {}); {} bytes, the port's {}",
                    byte(o),
                    byte(p),
                    o.len(),
                    p.len()
                ));
            }
            (Some(_), None) if deleted.contains(&key) => {
                out.push(format!("{path}: the original wrote it, the port deleted it"))
            }
            (Some(_), None) => out.push(format!("{path}: the original wrote it, the port didn't")),
            (None, Some(_)) => out.push(format!("{path}: the original deleted it, the port has it")),
            (None, None) if deleted.contains(&key) => {}
            (None, None) => out.push(format!("{path}: the original deleted it, the port didn't")),
        }
    }
    for path in port.keys().filter(|p| !seen.contains(*p)) {
        out.push(format!("{path}: the port wrote it, the original didn't"));
    }
    for path in deleted.iter().filter(|p| !seen.contains(*p)) {
        out.push(format!("{path}: the port deleted it, the original didn't"));
    }
    Ok(out)
}

/// The files' names, the first few.
fn files_named(files: &[crate::peek::ChangedFile]) -> String {
    let names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    let more = if names.len() > 5 { format!(" and {} more", names.len() - 5) } else { String::new() };
    format!("{}{more}", names.iter().take(5).cloned().collect::<Vec<_>>().join(", "))
}

fn write_saves(
    repo: &Repo,
    session: &Session,
    k: &Knowledge,
    tick: Option<u64>,
    dir: &std::path::Path,
) -> Result<usize, String> {
    if dir.is_dir() {
        std::fs::remove_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let Some(tick) = tick else { return Ok(0) };
    write_files(crate::peek::changed_files_at(repo, session, &crate::trace::sync_points(k)?, tick)?, dir)
}

/// The files the program changed (`changed_files`), written into `dir`, and those it deleted listed (D131).
fn write_files(files: Vec<crate::peek::ChangedFile>, dir: &std::path::Path) -> Result<usize, String> {
    let mut deleted = String::new();
    let mut written = 0;
    for (path, bytes) in files {
        match bytes {
            Some(bytes) => {
                let to = dir.join(path.replace('\\', "/"));
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
                }
                std::fs::write(&to, bytes).map_err(|e| format!("{}: {e}", to.display()))?;
                written += 1;
            }
            None => {
                deleted.push_str(&path);
                deleted.push('\n');
            }
        }
    }
    if !deleted.is_empty() {
        let list = dir.join(rrc_runtime::original::DELETED);
        std::fs::write(&list, deleted).map_err(|e| format!("{}: {e}", list.display()))?;
    }
    Ok(written)
}

/// A transplant (`rrc-state`): the mapped fields' values at `tick`, and the memory the port carries there
/// (`carried`, D126).
fn write_start(
    session: &str,
    mapped: &[Mapped],
    original: &[Column],
    tick: u64,
    carried: serde_json::Map<String, Value>,
    path: &str,
) -> Result<(), String> {
    let values: Vec<Value> = original.iter().map(|values| values[(tick - 1) as usize].clone()).collect();
    write_state(session, mapped, &values, tick, carried, path)
}

/// A transplant (`rrc-state`) of the mapped fields' `values` at `tick`, and the memory the port carries there.
fn write_state(
    session: &str,
    mapped: &[Mapped],
    values: &[Value],
    tick: u64,
    carried: serde_json::Map<String, Value>,
    path: &str,
) -> Result<(), String> {
    let mut state: serde_json::Map<String, Value> =
        mapped.iter().zip(values).map(|(m, value)| (m.field.clone(), value.clone())).collect();
    state.extend(carried);
    let file = json!({"format": "rrc-state", "version": 1, "session": session, "tick": tick, "state": state});
    std::fs::write(path, format!("{file}\n")).map_err(|e| format!("{path}: {e}"))
}

/// The program's own initial data (`rrc-state`, tick 0, D135): each mapped field, and each carried item (D137), at
/// a fixed place in the load image of the program (the analysis's, else Clockwork's unpacking of the game's, D224),
/// as the image holds it (`initial_state`). False where there is no such program (no game's program, or not MZ).
fn write_initial(repo: &Repo, session: &str, items: &[Mapped], k: &Knowledge, path: &str) -> Result<bool, String> {
    let Some(image) = crate::analysis::program_image(repo)? else { return Ok(false) };
    let program = Cache::open(&repo.local)?.get(&image)?;
    let Some(image) = mz_image(&program) else { return Ok(false) };
    let state = initial_state(image, &repo.module(), items, k)?;
    let file = json!({"format": "rrc-state", "version": 1, "session": session, "tick": 0, "state": state});
    std::fs::write(path, format!("{file}\n")).map_err(|e| format!("{path}: {e}"))?;
    Ok(true)
}

/// The items at fixed places in `image` (`module`'s IDs, not in an object, not through a pointer, not an index,
/// wholly in the image) as it holds them, by their fields.
fn initial_state(
    image: &[u8],
    module: &str,
    items: &[Mapped],
    k: &Knowledge,
) -> Result<serde_json::Map<String, Value>, String> {
    // Read by the book a port played on its own reads it by (D226), so the two are the same.
    let book = rrc_runtime::standalone::Book {
        items: crate::standalone::initial_items(module, items),
        types: k.types.clone(),
        arrays: crate::standalone::arrays(k),
        ..Default::default()
    };
    Ok(book.initial(image)?.into_iter().collect())
}

/// An MZ program's load image: the file past its header, as long as the header says; none if it isn't one.
fn mz_image(bytes: &[u8]) -> Option<&[u8]> {
    let word = |at: usize| bytes.get(at..at + 2).map(|w| usize::from(u16::from_le_bytes([w[0], w[1]])));
    if !(bytes.starts_with(b"MZ") || bytes.starts_with(b"ZM")) {
        return None;
    }
    let (last, pages, header) = (word(2)?, word(4)?, word(8)? * 16);
    let size = if last == 0 { pages * 512 } else { pages.saturating_sub(1) * 512 + last };
    bytes.get(header..size.min(bytes.len()))
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
    if items.is_empty() {
        return Ok(serde_json::Map::new());
    }
    let places = Places::new(repo, session);
    let length_ms = (session.seconds * 1000.0) as u64;
    let (mut clockwork, _, from) = crate::watch::open(repo, session, &crate::trace::sync_points(k)?, tick + 1)?;
    crate::watch::to_tick(&mut clockwork, tick + 1, from.as_ref(), length_ms)?;
    read_given_from(&mut clockwork, &places, k, items, tick)
}

/// What the port is given of the original at a start, from a machine at `tick`'s sync point ([`read_given`]).
fn read_given_from(
    clockwork: &mut crate::clockwork::Clockwork,
    places: &Places,
    k: &Knowledge,
    items: &[Mapped],
    tick: u64,
) -> Result<serde_json::Map<String, Value>, String> {
    let mut state = serde_json::Map::new();
    if items.is_empty() {
        return Ok(state);
    }
    // Each item's bytes, or the pointer's an item is found through (a carried one, D177); then the pointers of the
    // arrays carried pointers are indexes into (D179).
    let mut ranges = items
        .iter()
        .map(|m| {
            let len = m.via.as_ref().map_or(m.size, |v| v.pointer.size());
            Ok(json!({"at": places.address(&m.id.to_string())?, "len": len}))
        })
        .collect::<Result<Vec<_>, String>>()?;
    for into in items.iter().filter_map(|m| m.into.as_ref()) {
        ranges.push(json!({"at": places.address(&into.base.to_string())?, "len": into.pointer.size()}));
    }
    let read = clockwork.ask("read", json!({ "ranges": ranges }))?;
    let read: Vec<Option<String>> =
        read["data"].as_array().into_iter().flatten().map(|hex| hex.as_str().map(str::to_owned)).collect();
    let (read, mut bases) = (read[..items.len().min(read.len())].to_vec(), read.into_iter().skip(items.len()));
    let r = crate::values::of(k);
    for (m, hex) in items.iter().zip(read) {
        if let Some(into) = &m.into {
            let kind = crate::values::PointerKind::of_type(&m.ty, r.types)
                .ok_or_else(|| format!("{}: {} isn't a pointer", m.field, m.ty))??;
            let own = hex.as_deref().map(crate::trace::unhex).unwrap_or_default();
            let base = bases.next().flatten().as_deref().map(crate::trace::unhex).unwrap_or_default();
            state.insert(m.field.clone(), pointer_index(&own, &kind, &base, into));
            continue;
        }
        let bytes = match (&m.via, hex) {
            (None, Some(hex)) => unhex(&hex)?,
            (None, None) => return Err(format!("{} ({}) isn't in memory at tick {tick}", m.field, m.id)),
            (Some(v), hex) => {
                let pointer = hex.as_deref().map(crate::trace::unhex).unwrap_or_default();
                let first = (&v.pointer, v.pointer.target(&pointer, 0));
                match crate::trace::followed(clockwork, places, first, &v.then, v.offset, m.size, v.next)? {
                    Some(bytes) => bytes,
                    // A null pointer: nothing there.
                    None => {
                        state.insert(m.field.clone(), Value::Null);
                        continue;
                    }
                }
            }
        };
        let value = decode(&bytes, &m.ty, &r).map_err(|e| format!("{}: {e}", m.field))?;
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

/// What a corrected run does with each line of the port's trace (D191): counts it, and answers the original's values
/// of the fields the tick differs in, for the next tick's inputs.
type Correct<'a> = dyn FnMut(&Value) -> Result<serde_json::Map<String, Value>, String> + 'a;

/// A file read as it grows, a whole line at a time: the port's trace while it runs (D190).
struct Tail {
    path: std::path::PathBuf,
    file: Option<std::fs::File>,
    partial: Vec<u8>,
}

impl Tail {
    fn new(path: &std::path::Path) -> Tail {
        Tail { path: path.to_owned(), file: None, partial: Vec::new() }
    }

    /// The whole lines written since the last look.
    fn lines(&mut self) -> Vec<String> {
        if self.file.is_none() {
            self.file = std::fs::File::open(&self.path).ok();
        }
        let Some(file) = self.file.as_mut() else { return Vec::new() };
        let mut bytes = Vec::new();
        if std::io::Read::read_to_end(file, &mut bytes).is_err() {
            return Vec::new();
        }
        self.partial.extend(bytes);
        let Some(end) = self.partial.iter().rposition(|&b| b == b'\n') else { return Vec::new() };
        let whole: Vec<u8> = self.partial.drain(..=end).collect();
        String::from_utf8_lossy(&whole).lines().filter(|l| !l.trim().is_empty()).map(str::to_owned).collect()
    }
}

/// Whether the port is fed a tick at a time (`rrc compare --streamed`, D190).
static STREAMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How long a streamed port may take to write a tick's line before rrc gives up on it.
const STREAM_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Writes the inputs file at `path` to the port's standard input (D190). With `trace`, the port's trace, a tick's
/// line goes only once the port wrote the tick before (from the trace's `from` on), as the live comparison feeds it:
/// a port that doesn't write and flush a tick before reading on is stopped here, not left waiting.
fn feed_inputs(mut stdin: std::process::ChildStdin, path: &str, trace: Option<&std::path::Path>) -> Result<(), String> {
    use std::io::Write as _;
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let mut written: Option<u64> = None;
    let (mut seen, mut from, mut ended) = (None::<u64>, None::<u64>, false);
    let mut tail = trace.map(Tail::new);
    for line in text.lines() {
        let tick = serde_json::from_str::<serde_json::Value>(line).ok().and_then(|v| v["tick"].as_u64());
        if let (Some(tail), Some(_), Some(before)) = (tail.as_mut(), tick, written) {
            let started = std::time::Instant::now();
            // The port's trace as it grows: its `from`, the last tick it wrote, whether it ended.
            loop {
                for l in tail.lines() {
                    let v: serde_json::Value = serde_json::from_str(&l).unwrap_or_default();
                    from = from.or(v["from"].as_u64());
                    seen = v["tick"].as_u64().or(seen);
                    ended |= v.get("end").is_some();
                }
                if ended {
                    return Ok(());
                }
                if from.is_some_and(|from| before < from) || seen.is_some_and(|seen| seen >= before) {
                    break;
                }
                if started.elapsed() > STREAM_WAIT {
                    return Err(format!(
                        "the port didn't write tick {before}'s line before asking for more: it writes and flushes \
                         each tick before reading past the next tick's inputs (docs/formats.md)"
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        // A port that stopped reading (it ran to --to) has what it wanted.
        if writeln!(stdin, "{line}").and_then(|()| stdin.flush()).is_err() {
            return Ok(());
        }
        written = tick.or(written);
    }
    Ok(())
}

/// Builds the port if rrc.toml says how (`[port] build`), then runs it (`[port] command`) in the port
/// repository, for `session`.
/// With `correct`, the port is run a tick at a time (D191): each line of its trace is given to `correct` as the port
/// writes it, and what it answers for a tick goes with the next tick's inputs (`correct`, set before that tick's step).
fn run_port(
    repo: &Repo,
    session: &Session,
    args: &[String],
    saves: Option<&std::path::Path>,
    mut correct: Option<&mut Correct>,
) -> Result<(), String> {
    let port = &repo.settings.port;
    let original = port_env(repo, session, saves)?;
    let run = |what: &str, command: &[String], args: &[String], correct: Option<&mut Correct>| -> Result<(), String> {
        let (program, rest) = command.split_first().ok_or(format!("[port] {what} is empty"))?;
        // The inputs through a pipe (D190): the port reads them from its standard input as it needs them; from the
        // file itself with RRC_PORT_INPUTS=file (a port that reads only files, or to compare the two).
        let value = |flag: &str| args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone());
        let piped = std::env::var("RRC_PORT_INPUTS").map_or(true, |v| v != "file");
        let (inputs, out) = (value("--inputs").filter(|_| piped), value("--out"));
        let args: Vec<String> =
            args.iter().map(|a| if Some(a) == inputs.as_ref() { "-".to_owned() } else { a.clone() }).collect();
        let paced = STREAMED.load(std::sync::atomic::Ordering::Relaxed) || correct.is_some();
        if let Some(out) = out.as_ref().filter(|_| paced) {
            let _ = std::fs::remove_file(out);
        }
        if correct.is_some() && inputs.is_none() {
            return Err(
                "a corrected run gives the port its inputs through a pipe: not with RRC_PORT_INPUTS=file".into()
            );
        }
        let mut child = Command::new(program)
            .args(rest)
            .args(&args)
            .envs(original.iter().cloned())
            .current_dir(&repo.root)
            .stdin(if inputs.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("[port] {what} ({program}): {e}"))?;
        // What the port says on its standard error, read beside it, so it never waits on a full pipe.
        let errors = child.stderr.take().map(|mut pipe| {
            std::thread::spawn(move || {
                let mut stderr = Vec::new();
                let _ = std::io::Read::read_to_end(&mut pipe, &mut stderr);
                stderr
            })
        });
        let fed = match (child.stdin.take(), inputs, correct) {
            (Some(stdin), Some(inputs), Some(correct)) => {
                let trace = std::path::PathBuf::from(out.as_deref().ok_or("a corrected run needs the port's --out")?);
                feed_corrected(stdin, &inputs, &trace, correct, &mut child)
            }
            (Some(stdin), Some(inputs), None) => {
                let trace = out.filter(|_| paced).map(std::path::PathBuf::from);
                let writer = std::thread::spawn(move || feed_inputs(stdin, &inputs, trace.as_deref()));
                writer.join().unwrap_or_else(|_| Err("feeding the port's inputs panicked".into()))
            }
            _ => Ok(()),
        };
        let status = child.wait().map_err(|e| format!("[port] {what} ({program}): {e}"))?;
        let stderr = errors.map(|e| e.join().unwrap_or_default()).unwrap_or_default();
        if status.success() {
            return fed;
        }
        let stderr = String::from_utf8_lossy(&stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(8).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        Err(format!("[port] {what} failed ({status}):\n{}", tail.join("\n")))
    };
    if port.command.is_empty() {
        return Err("rrc.toml has no [port] command: the port's program, which rrc runs (docs/formats.md)".into());
    }
    crate::sources::held_still(&repo.root, || {
        if !port.build.is_empty() {
            run("build", &port.build, &[], None)?;
        }
        run("command", &port.command, args, correct.take())
    })
}

/// What a port's run is given of the original: the person's copy of the game, for what the port loads from it (D82,
/// `rrc_runtime::original`): the folder the session runs on (D170), and the program as the analysis unpacked it, or
/// without one as Clockwork unpacks the game's (its static data, D224); where it keeps the games it saves, and reads saved ones first (D131).
fn port_env(
    repo: &Repo,
    session: &Session,
    saves: Option<&std::path::Path>,
) -> Result<Vec<(&'static str, std::path::PathBuf)>, String> {
    let mut original: Vec<(&str, std::path::PathBuf)> = Vec::new();
    match crate::folder::of(repo, session) {
        Ok(folder) => original.push(("RRC_GAME", folder)),
        // No game's folder, and none recorded: a port that reads none of the game's files still runs.
        Err(_) if repo.game_dir().is_err() && session.root(repo)?.files.is_empty() => {}
        Err(e) => return Err(e),
    }
    if let Some(image) = crate::analysis::program_image(repo)? {
        original.push(("RRC_PROGRAM", crate::cache::Cache::open(&repo.local)?.path(&image)));
    }
    if let Some(saves) = saves {
        original.push(("RRC_SAVES", saves.to_path_buf()));
    }
    Ok(original)
}

/// The original's state the port starts again from (D193): read from the follower at its tick `tick`, what the port
/// carries, the presentation's own, the games saved.
#[derive(Clone)]
pub(crate) struct Given {
    pub tick: u64,
    carried: serde_json::Map<String, Value>,
    presented: serde_json::Map<String, Value>,
    files: Vec<crate::peek::ChangedFile>,
}

/// What a port starting again is given, read from the follower's machine at its tick `tick`'s sync point.
pub(crate) fn given_now(
    clockwork: &mut crate::clockwork::Clockwork,
    places: &Places,
    k: &Knowledge,
    mapping: &Mapping,
    tick: u64,
) -> Result<Given, String> {
    let carried = read_given_from(clockwork, places, k, &mapping.resolve_carried(k)?, tick)?;
    let presented = read_given_from(clockwork, places, k, &mapping.resolve_present(k)?, tick)?;
    let answer = clockwork.ask("changed_files", json!({}))?;
    let mut files = Vec::new();
    for f in answer["files"].as_array().into_iter().flatten() {
        let path = f["path"].as_str().ok_or("a changed file without a path")?.to_owned();
        files.push((path, f["hex"].as_str().map(crate::trace::unhex)));
    }
    Ok(Given { tick, carried, presented, files })
}

/// Where the port starts on a play followed: at power-on, at the play's first tick, or again at a later one.
pub(crate) enum Begin<'b> {
    PowerOn,
    First(&'b At),
    Again(&'b At, &'b Given),
}

/// How the port starts on a play followed (D192): from power-on with the program's initial data (D135, D137), or from
/// a session's tick with the original's state at the play's first tick (D118), its carried memory and presentation,
/// the screen and the games saved there, read as `rrc compare` reads them.
pub(crate) struct PortStart<'a> {
    repo: &'a Repo,
    session: &'a Session,
    name: &'a str,
    k: &'a Knowledge,
    mapping: &'a Mapping,
    mapped: &'a [Mapped],
    header: crate::trace::Header,
    work: std::path::PathBuf,
    /// The port's runs so far: each writes a trace of its own.
    runs: std::cell::Cell<u32>,
    /// Its own input handling checked beside (`--live`, D197).
    live: bool,
}

impl<'a> PortStart<'a> {
    pub(crate) fn new(
        repo: &'a Repo,
        session: &'a Session,
        name: &'a str,
        k: &'a Knowledge,
        mapping: &'a Mapping,
        mapped: &'a [Mapped],
        header: crate::trace::Header,
    ) -> Result<PortStart<'a>, String> {
        let work = repo.state()?.join("compare").join(name);
        let frames = work.join("frames");
        if frames.is_dir() {
            std::fs::remove_dir_all(&frames).map_err(|e| format!("{}: {e}", frames.display()))?;
        }
        std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
        let runs = std::cell::Cell::new(0);
        Ok(PortStart { repo, session, name, k, mapping, mapped, header, work, runs, live: false })
    }

    /// Its input handling checked beside its runs (D197): each runs with `--live`.
    pub(crate) fn checking_live(mut self) -> Self {
        self.live = true;
        self
    }

    /// The traces of its runs so far, in order.
    pub(crate) fn traces(&self) -> Vec<std::path::PathBuf> {
        (1..=self.runs.get())
            .map(|n| self.work.join(if n == 1 { "port.jsonl".to_owned() } else { format!("port-{n}.jsonl") }))
            .collect()
    }

    pub(crate) fn header(&self) -> &crate::trace::Header {
        &self.header
    }

    pub(crate) fn at_power_on(&self) -> bool {
        self.session.from.is_none()
    }

    /// Where the port's frames go.
    pub(crate) fn frames(&self) -> std::path::PathBuf {
        self.work.join("frames")
    }

    /// The port started where `begin` says; its trace in `port.jsonl` (a run started again: `port-2.jsonl`, ...), its
    /// inputs given through [`PortRun::give`].
    pub(crate) fn spawn(&self, begin: Begin) -> Result<PortRun, String> {
        let file = |f: &str| self.work.join(f).to_string_lossy().into_owned();
        self.runs.set(self.runs.get() + 1);
        let trace = match self.runs.get() {
            1 => "port.jsonl".to_owned(),
            n => format!("port-{n}.jsonl"),
        };
        let mut args = vec!["run".to_owned(), "--inputs".into(), "-".into(), "--out".into(), file(&trace)];
        args.extend(["--frames".to_owned(), file("frames")]);
        if self.live {
            args.push("--live".into());
        }
        let saves = self.work.join("saves");
        match begin {
            Begin::Again(at, given) => {
                write_state(
                    self.name,
                    self.mapped,
                    &at.values,
                    given.tick,
                    given.carried.clone(),
                    &file("start.json"),
                )?;
                args.extend(["--transplant".into(), file("start.json")]);
                if !given.presented.is_empty() {
                    let state = &given.presented;
                    let presentation = json!({"format": "rrc-presentation", "version": 1, "session": self.name,
                                              "tick": given.tick, "state": state});
                    std::fs::write(file("presentation.json"), format!("{presentation}\n"))
                        .map_err(|e| e.to_string())?;
                    args.extend(["--presentation".into(), file("presentation.json")]);
                }
                if let Some(hash) = &at.screen {
                    let picture = Cache::open(&self.repo.local)?.get(hash)?;
                    std::fs::write(file("screen.frm"), picture).map_err(|e| e.to_string())?;
                    args.extend(["--screen".into(), file("screen.frm")]);
                }
                if saves.is_dir() {
                    std::fs::remove_dir_all(&saves).map_err(|e| format!("{}: {e}", saves.display()))?;
                }
                std::fs::create_dir_all(&saves).map_err(|e| format!("{}: {e}", saves.display()))?;
                write_files(given.files.clone(), &saves)?;
            }
            Begin::PowerOn => {
                let mut initial = self.mapped.to_vec();
                initial.extend(self.mapping.resolve_carried(self.k)?);
                if write_initial(self.repo, self.name, &initial, self.k, &file("initial.json"))? {
                    args.extend(["--initial".into(), file("initial.json")]);
                }
                write_saves(self.repo, self.session, self.k, None, &saves)?;
            }
            Begin::First(at) => {
                let carried = read_given(self.repo, self.session, self.k, &self.mapping.resolve_carried(self.k)?, 1)?;
                write_state(self.name, self.mapped, &at.values, 1, carried, &file("start.json"))?;
                args.extend(["--transplant".into(), file("start.json")]);
                let presented = self.mapping.resolve_present(self.k)?;
                if !presented.is_empty() {
                    let at = (self.name, 1);
                    write_presentation(self.repo, self.session, self.k, &presented, at, &file("presentation.json"))?;
                    args.extend(["--presentation".into(), file("presentation.json")]);
                }
                if let Some(hash) = &at.screen {
                    let picture = Cache::open(&self.repo.local)?.get(hash)?;
                    std::fs::write(file("screen.frm"), picture).map_err(|e| e.to_string())?;
                    args.extend(["--screen".into(), file("screen.frm")]);
                }
                write_saves(self.repo, self.session, self.k, Some(1), &saves)?;
            }
        }
        let port = &self.repo.settings.port;
        let (program, rest) = port.command.split_first().ok_or("[port] command is empty")?;
        let trace = self.work.join(&trace);
        let _ = std::fs::remove_file(&trace);
        let mut command = Command::new(program);
        command
            .args(rest)
            .args(&args)
            .envs(port_env(self.repo, self.session, Some(&saves))?)
            .current_dir(&self.repo.root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        crate::clockwork::lowered(&mut command);
        let mut child = command.spawn().map_err(|e| format!("[port] command ({program}): {e}"))?;
        let errors = child.stderr.take().map(|mut pipe| {
            std::thread::spawn(move || {
                let mut stderr = Vec::new();
                let _ = std::io::Read::read_to_end(&mut pipe, &mut stderr);
                stderr
            })
        });
        let stdin = child.stdin.take().ok_or("the port's input")?;
        Ok(PortRun { child, feeder: Some(Feeder::new(stdin, &trace).timing_hangs()), errors })
    }
}

/// The port's run as a play goes (D192): its inputs given a line at a time, each line of its trace to the comparer.
pub(crate) struct PortRun {
    child: std::process::Child,
    feeder: Option<Feeder>,
    errors: Option<std::thread::JoinHandle<Vec<u8>>>,
}

impl PortRun {
    /// Gives line `v`, once the port wrote the tick before (its lines to `counting`, corrected where it differs).
    pub(crate) fn give(&mut self, v: Value, counting: &mut Counting) -> Result<(), String> {
        let feeder = self.feeder.as_mut().ok_or("the port's run is over")?;
        match feeder.give(v, &mut |line: &Value| counting.line(line), &mut self.child) {
            Ok(true) => Ok(()),
            Ok(false) => Err(self.ended()),
            // It hangs, or what it wrote can't be compared: stopped.
            Err(e) => {
                self.feeder = None;
                let _ = self.child.kill();
                let _ = self.child.wait();
                Err(e)
            }
        }
    }

    /// The inputs' end: the port's last lines to `counting`, and how it ended.
    pub(crate) fn finish(mut self, counting: &mut Counting) -> Result<(), String> {
        if let Some(feeder) = self.feeder.take() {
            feeder.finish(&mut |line: &Value| counting.line(line), &mut self.child)?;
        }
        let status = self.child.wait().map_err(|e| format!("[port]: {e}"))?;
        if status.success() { Ok(()) } else { Err(self.ended()) }
    }

    /// How the port ended before its inputs did: its last words.
    fn ended(&mut self) -> String {
        self.feeder = None;
        let status = self.child.wait().map(|s| s.to_string()).unwrap_or_default();
        let stderr = self.errors.take().map(|e| e.join().unwrap_or_default()).unwrap_or_default();
        let stderr = String::from_utf8_lossy(&stderr);
        // Its last words: a panic's message, past the note on backtraces.
        let said: Vec<&str> =
            stderr.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("note: ")).collect();
        let last = said.iter().rev().take(2).rev().copied().collect::<Vec<_>>().join(" ");
        format!("it ended ({status}): {last}")
    }
}

/// Feeds the port the inputs file at `path` a tick at a time, corrected where it differs (D191): [`Feeder`].
fn feed_corrected(
    stdin: std::process::ChildStdin,
    path: &str,
    trace: &std::path::Path,
    correct: &mut Correct,
    child: &mut std::process::Child,
) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let mut feeder = Feeder::new(stdin, trace);
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("{path}: {e}"))?;
        if !feeder.give(v, correct, child)? {
            break;
        }
    }
    feeder.finish(correct, child)
}

/// The port fed a line at a time, corrected where it differs (D191, D192): a tick's inputs line goes once the port
/// wrote the tick before, with `correct`'s answer for that tick, the original's values of the fields it differed in;
/// the header says version 5, which has `correct`. Each line of the port's trace (`trace`, read as it grows) goes to
/// `correct`, the last ones too.
pub(crate) struct Feeder {
    stdin: Option<std::process::ChildStdin>,
    tail: Tail,
    port: PortSeen,
    /// The tick of the last line given.
    written: Option<u64>,
    /// A port that writes no line for ten times its slowest tick so far hangs (D193); the slowest so far.
    hangs: bool,
    slowest: std::time::Duration,
    /// The original's passes from the last lines given, by tick (their devices' `us` to `next`): the port's line for a
    /// tick comes from the pass of the line before, and a port that follows its reads one by one may take as long as
    /// it: it gets three times it (D208).
    passes: BTreeMap<u64, std::time::Duration>,
}

/// Where a fed port's time goes (µs): its lines read and compared (`take`), and waited for.
pub(crate) static PORT_TAKING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static PORT_WAITING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The least a port may take over a tick before it is taken to hang (D193).
const HANG_FLOOR: std::time::Duration = std::time::Duration::from_secs(10);

/// The port's trace so far: its `from`, the last tick it wrote, whether it ended, the corrections for its ticks.
#[derive(Default)]
struct PortSeen {
    from: Option<u64>,
    tick: Option<u64>,
    ended: bool,
    corrections: BTreeMap<u64, serde_json::Map<String, Value>>,
}

impl Feeder {
    pub(crate) fn new(stdin: std::process::ChildStdin, trace: &std::path::Path) -> Feeder {
        Feeder {
            stdin: Some(stdin),
            tail: Tail::new(trace),
            port: PortSeen::default(),
            written: None,
            hangs: false,
            slowest: std::time::Duration::ZERO,
            passes: BTreeMap::new(),
        }
    }

    /// A port that writes no line for ten times its slowest tick so far (ten seconds at least) is taken to hang.
    pub(crate) fn timing_hangs(mut self) -> Feeder {
        self.hangs = true;
        self
    }

    /// The lines the port wrote since the last look.
    fn take(&mut self, correct: &mut Correct) -> Result<(), String> {
        for l in self.tail.lines() {
            let v: Value = serde_json::from_str(&l).map_err(|e| format!("the port's trace: {e}"))?;
            self.port.from = self.port.from.or(v["from"].as_u64());
            self.port.ended |= v.get("end").is_some();
            let fix = correct(&v)?;
            if let Some(t) = v["tick"].as_u64() {
                self.port.tick = Some(t);
                if !fix.is_empty() {
                    self.port.corrections.insert(t, fix);
                }
            }
        }
        Ok(())
    }

    /// Gives line `v` (the header, a tick's, the end); false once the port has ended or stopped reading.
    pub(crate) fn give(
        &mut self,
        mut v: Value,
        correct: &mut Correct,
        child: &mut std::process::Child,
    ) -> Result<bool, String> {
        use std::io::Write as _;
        if v["format"] == "rrc-inputs" {
            v["version"] = json!(5);
        }
        let tick = v["tick"].as_u64();
        if let (Some(_), Some(before)) = (tick, self.written) {
            let started = std::time::Instant::now();
            // The pass its line comes from: the line before's (or this one's, should the numbering be otherwise).
            let pass = [before.saturating_sub(1), before].iter().filter_map(|t| self.passes.get(t)).max().copied();
            let limit = match self.hangs && !self.slowest.is_zero() {
                true => (self.slowest * 10).max(pass.unwrap_or_default() * 3).clamp(HANG_FLOOR, STREAM_WAIT),
                false => STREAM_WAIT,
            };
            loop {
                let taking = std::time::Instant::now();
                self.take(correct)?;
                PORT_TAKING.fetch_add(taking.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
                if self.port.ended {
                    return Ok(false);
                }
                let from = self.port.from;
                if from.is_some_and(|from| before < from) || self.port.tick.is_some_and(|seen| seen >= before) {
                    break;
                }
                if child.try_wait().ok().flatten().is_some() {
                    self.take(correct)?;
                    return Ok(false);
                }
                if started.elapsed() > limit {
                    return Err(match self.hangs {
                        true => format!(
                            "it hangs: no line for tick {before} in {} s, ten times its slowest tick or three times the \
                             original's pass",
                            limit.as_secs()
                        ),
                        false => format!(
                            "the port didn't write tick {before}'s line before asking for more (docs/formats.md)"
                        ),
                    });
                }
                let waiting = std::time::Instant::now();
                std::thread::sleep(std::time::Duration::from_millis(1));
                PORT_WAITING.fetch_add(waiting.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
            }
            self.slowest = self.slowest.max(started.elapsed());
            if let Some(fix) = self.port.corrections.remove(&before) {
                v["correct"] = Value::Object(fix);
            }
        }
        let Some(stdin) = self.stdin.as_mut() else { return Ok(false) };
        if writeln!(stdin, "{v}").and_then(|()| stdin.flush()).is_err() {
            self.stdin = None;
            return Ok(false);
        }
        self.written = tick.or(self.written);
        let us = |key: &str| v["devices"][key].as_u64();
        if let (Some(t), Some(from), Some(to)) = (tick, us("us"), us("next")) {
            self.passes.insert(t, std::time::Duration::from_micros(to.saturating_sub(from)));
            self.passes.retain(|&at, _| at + 4 > t);
        }
        Ok(true)
    }

    /// The inputs' end: the port's last lines read as it ends.
    pub(crate) fn finish(mut self, correct: &mut Correct, child: &mut std::process::Child) -> Result<(), String> {
        drop(self.stdin.take());
        let started = std::time::Instant::now();
        while !self.port.ended {
            self.take(correct)?;
            if self.port.ended {
                break;
            }
            if child.try_wait().ok().flatten().is_some() {
                self.take(correct)?;
                break;
            }
            if started.elapsed() > STREAM_WAIT {
                return Err("the port didn't end its trace (docs/formats.md)".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Ok(())
    }
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
    found: &[Differs],
) -> Result<(), String> {
    let tick = &trace.ticks[(t - 1) as usize];
    say(
        "diverges",
        format!("at tick {t} ({}): {} field{} differ", tick.sync, found.len(), if found.len() == 1 { "" } else { "s" }),
    );
    // Each field that differs, its first value that does, and how many do (D140).
    for d in found.iter().take(SHOWN) {
        let ours = if d.ours.is_null() { "missing".to_owned() } else { d.ours.to_string() };
        let of = if d.leaves > 1 { format!(" (the first of {} in {})", d.leaves, d.field) } else { String::new() };
        say("  field", format!("{}: original {}, port {ours}{of}", d.path, d.theirs));
    }
    if found.len() > SHOWN {
        say("  more", format!("{} more fields differ", found.len() - SHOWN));
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
        say(
            "next",
            "the mapping first: rrc show ITEM for the item's type and size, against mapping.toml; then how the port \
             builds the field from it",
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
    let path = &found[0].path;
    let Some((m, offset, size)) = mapped.iter().find_map(|m| locate(m, path, &k.types).map(|(o, s)| (m, o, s))) else {
        return Ok(());
    };
    let Some(id) = located(repo, name, trace, m, offset, t)? else {
        say("wrote", format!("{path}: its pointer ({}) is null at tick {t}", m.id));
        return Ok(());
    };
    say("wrote", format!("{path} ({id}), between the sync points of ticks {} and {t}:", t - 1));
    if id.is_expanded() {
        say("wrote", "in expanded memory, which a watch can't follow (D172): its pages show in the frame while mapped");
        return Ok(());
    }
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
    // The next moves, of more than one kind (D159): the code, the value over the run, what others wrote of it.
    let word = path.split(['.', '[']).next().unwrap_or(path);
    match writer.chain.first().filter(|_| writer.instruction.is_some()) {
        Some(function) => say(
            "next",
            format!(
                "its code: rrc show {function} --c; the value over the pass: rrc watch {name} {id} --tick {t} --ticks 1 \
                 --reads; what is written of it: rrc sources {word}"
            ),
        ),
        None => say(
            "next",
            format!(
                "nothing in the pass wrote it: an input not declared (rrc inputs {name} --tick {t}), or a handler's \
                 work, which is a subsystem (rrc watch {name} {id} --tick {} --ticks 2); what is written of it: rrc \
                 sources {word}",
                t.saturating_sub(1)
            ),
        ),
    }
    // What the outside sources say of it, here where the next move is chosen (D185).
    let mut about = vec![path.clone()];
    about.extend(writer.chain.iter().take(2).filter_map(|id| k.functions.get(id)).map(|d| d.name.clone()));
    crate::digests::say_about(repo, &about, 3, true);
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
/// function's ID (docs/design.md §6.6), or, if none does, a caller's in the chain.
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
             comment (docs/design.md §6.6)",
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

/// Says which named data the mapping neither maps, carries nor ignores: the state not accounted for yet.
fn unmapped(k: &Knowledge, mapped: &[Mapped], repo: &Repo) -> Result<(), String> {
    let left = unmapped_names(k, mapped, repo)?;
    if !left.is_empty() {
        let shown: Vec<&str> = left.iter().take(10).map(String::as_str).collect();
        let more = if left.len() > 10 { format!(" and {} more", left.len() - 10) } else { String::new() };
        say(
            "unmapped",
            format!("{} named data not mapped, carried or ignored: {}{more}", left.len(), shown.join(", ")),
        );
    }
    Ok(())
}

/// The named data the mapping neither maps, presents, carries (D126: kept as it was on purpose, through its pointer
/// too) nor ignores, and no input is: the state nothing accounts for yet.
pub(crate) fn unmapped_names(k: &Knowledge, mapped: &[Mapped], repo: &Repo) -> Result<Vec<String>, String> {
    let mapping = Mapping::load(repo)?;
    let ignored = mapping.ignored(k)?;
    let mut presented = mapping.resolve_present(k)?;
    presented.extend(mapping.resolve_carried(k)?);
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
    Ok(k.data
        .iter()
        .filter(|(id, d)| d.size.is_some_and(|size| !covered(id, size)) && !inputs.contains(&id.to_string()))
        .map(|(_, d)| d.decision.name.clone())
        .collect())
}

fn unhex(text: &str) -> Result<Vec<u8>, String> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).map_err(|_| format!("bad hex {text:?}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_files_the_port_saved_are_compared_with_the_original_s() {
        let saves = std::env::temp_dir().join(format!("rrc-files-{}", std::process::id()));
        std::fs::create_dir_all(saves.join("SUB")).unwrap();
        std::fs::write(saves.join("LOG.01"), b"same").unwrap();
        std::fs::write(saves.join("SAVE.01"), b"abcX").unwrap();
        std::fs::write(saves.join("SUB/OWN.01"), b"port").unwrap();
        std::fs::write(saves.join(rrc_runtime::original::DELETED), "GONE.01\n").unwrap();
        let original: Vec<crate::peek::ChangedFile> = vec![
            ("LOG.01".into(), Some(b"same".to_vec())),
            ("SAVE.01".into(), Some(b"abcd".to_vec())),
            ("NEW.01".into(), Some(b"x".to_vec())),
            ("GONE.01".into(), None),
        ];
        let differing = files_differing(&original, &saves).unwrap();
        assert_eq!(
            differing,
            [
                "SAVE.01: differs from byte 3 (original 64h, port 58h); 4 bytes, the port's 4",
                "NEW.01: the original wrote it, the port didn't",
                "SUB/OWN.01: the port wrote it, the original didn't",
            ],
            "{differing:?}"
        );
        assert!(files_differing(&original[..1], &saves).unwrap().len() == 3, "the port's own and its deletion");
        std::fs::remove_dir_all(&saves).ok();
    }

    #[test]
    fn a_column_keeps_a_field_s_changes_and_reads_as_every_tick_s() {
        let ticks = [1, 1, 2, 2, 2, 1, 3];
        let mut column = Column::starting(0);
        for v in ticks {
            column.push(json!(v));
        }
        assert_eq!(column.changes.len(), 4, "what changed, not a value a tick");
        let every: Vec<Value> = (0..ticks.len()).map(|i| column[i].clone()).collect();
        assert_eq!(every, ticks.map(|v| json!(v)));
        // Fed from the fourth tick on: its value there, then each change.
        let fed: Vec<(usize, Value)> = column.from(3).map(|(i, v)| (i, v.clone())).collect();
        assert_eq!(fed, vec![(3, json!(2)), (5, json!(1)), (6, json!(3))]);
        assert_eq!(column.from(7).count(), 0);
    }

    #[test]
    fn a_port_past_what_it_knows_is_lost_from_three_ticks_and_back_after_three() {
        let (mut lost, mut runs) = (Vec::new(), [0, 0]);
        let ticks: [(u64, &str); 9] = [
            (1, "play"),
            // One tick at another sync point (timing) isn't lost; three running are, from the first.
            (2, "button_wait"),
            (3, "play"),
            (4, "button_wait"),
            (5, "button_wait"),
            (6, "button_wait"),
            (7, "button_wait"),
            (8, "play"),
            (9, "play"),
        ];
        for (t, original) in ticks {
            lost_at(&mut lost, &mut runs, t, Some("play"), Some(original));
        }
        assert_eq!(lost.len(), 1);
        assert_eq!((lost[0].from, lost[0].to, lost[0].port.as_str()), (4, None, "play"));
        lost_at(&mut lost, &mut runs, 10, Some("play"), Some("play"));
        assert_eq!(lost[0].to, Some(8), "back from the first of three at the original's");
        lost_at(&mut lost, &mut runs, 11, None, Some("play"));
        assert_eq!(lost.len(), 1);
    }

    #[test]
    fn the_port_s_pictures_between_ticks_are_the_original_s_in_order() {
        let dir = std::env::temp_dir().join(format!("rrc-between-{}", std::process::id()));
        let cache = Cache::at(&dir.join("cache")).unwrap();
        let frames = dir.join("frames");
        std::fs::create_dir_all(&frames).unwrap();
        let picture = |v: u8| {
            let mut f = Frame::new(2, 1);
            f.set(0, 0, v);
            f
        };
        // The original's refreshes: 1, 1, a torn 9, 2, 3; the port's pictures 1 and 3, then 2 after 3.
        let hashes: Vec<String> = [1, 1, 9, 2, 3].iter().map(|&v| cache.put(&picture(v).encode()).unwrap()).collect();
        let frames_json: Vec<Value> = hashes.iter().enumerate().map(|(i, h)| json!([i, i * 10, 19, "", h])).collect();
        let tick: crate::trace::Tick =
            serde_json::from_value(json!({"tick": 1, "sync": "play", "cycle": 0, "frames": frames_json})).unwrap();
        for (name, v) in [("a.frm", 1), ("b.frm", 3), ("c.frm", 2)] {
            std::fs::write(frames.join(name), picture(v).encode()).unwrap();
        }
        let mut between = Between::default();
        assert!(between.check(1, &tick.frames, &["a.frm", "b.frm"], &[], &frames, &cache).unwrap().is_none());
        assert_eq!((between.passes, between.pictures, between.shown), (1, 2, 4), "the repeat counted once");
        // One shown for less than a refresh may be missing (D181).
        assert!(
            between
                .check(1, &tick.frames, &["a.frm", "c.frm", "b.frm"], &["c.frm"], &frames, &cache)
                .unwrap()
                .is_none()
        );
        assert_eq!(between.unseen, 0, "2 is there, after 1");
        assert!(between.check(1, &tick.frames, &["b.frm", "c.frm"], &["c.frm"], &frames, &cache).unwrap().is_none());
        assert_eq!(between.unseen, 1, "2 after 3: fleeting, passed over");
        let missed =
            between.check(1, &tick.frames, &["b.frm", "c.frm"], &[], &frames, &cache).unwrap().expect("2 after 3");
        assert_eq!((missed.picture, missed.from, missed.shown), (1, 4, 4), "none left after the 3 it matched");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_census_counts_the_ticks_whose_pictures_between_aren_t_the_original_s() {
        let dir = std::env::temp_dir().join(format!("rrc-census-pictures-{}", std::process::id()));
        let cache = Cache::at(&dir.join("cache")).unwrap();
        let frames = dir.join("frames");
        std::fs::create_dir_all(&frames).unwrap();
        let picture = |v: u8| {
            let mut f = Frame::new(2, 1);
            f.set(0, 0, v);
            f
        };
        // The original showed 1 then 2 in each pass; the port 1 then 2 in the first, 2 then 1 in the second.
        let shown: Vec<Value> = [1, 2]
            .iter()
            .enumerate()
            .map(|(i, &v)| json!([i, i, 19, "", cache.put(&picture(v).encode()).unwrap()]))
            .collect();
        for (name, v) in [("a.frm", 1), ("b.frm", 2)] {
            std::fs::write(frames.join(name), picture(v).encode()).unwrap();
        }
        let sets = BTreeMap::new();
        let mut counting = Counting::new("s", &[], &sets, &cache, frames.clone(), 0, false, Census::default());
        for tick in 1..=3 {
            let frames = if tick == 3 { Vec::new() } else { shown.clone() };
            counting.original(At {
                tick,
                values: Vec::new(),
                screen: None,
                sound: Default::default(),
                sync: None,
                frames,
            });
        }
        counting.line(&json!({"tick": 1, "between": ["a.frm", "b.frm"]})).unwrap();
        assert_eq!(counting.census.pictures, None, "1 then 2: the original's");
        counting.line(&json!({"tick": 2, "between": ["b.frm", "a.frm"]})).unwrap();
        let seen = counting.census.pictures.clone().expect("1 after 2 isn't");
        assert_eq!((seen.tick, seen.ticks), (2, 1));
        assert_eq!(seen.what, "its 2nd of 2 comes after the original's last of 2");
        counting.line(&json!({"tick": 3, "between": ["b.frm", "a.frm"]})).unwrap();
        assert_eq!(counting.census.pictures.as_ref().map(|s| s.ticks), Some(1), "none recorded there: not compared");
        assert!(counting.census.differs());
        std::fs::remove_dir_all(&dir).ok();
    }
    use crate::knowledge::{Field, Struct};

    #[test]
    fn a_field_that_differs_is_its_first_value_and_how_many() {
        let original = json!([1, 2, 3, 4]);
        let d = Differs::of("map", &original, &json!([1, 0, 3, 0])).expect("they differ");
        assert_eq!((d.path.as_str(), &d.theirs, &d.ours, d.leaves), ("map[1]", &json!(2), &json!(0), 2));
        assert!(Differs::of("map", &original, &original).is_none());
    }

    #[test]
    fn an_mz_program_s_load_image_is_past_its_header() {
        // A header of 2 paragraphs (32 bytes), 600 bytes in all: the last page's 88 of 2.
        let mut program = vec![0u8; 700];
        program[..2].copy_from_slice(b"MZ");
        program[2..4].copy_from_slice(&88u16.to_le_bytes());
        program[4..6].copy_from_slice(&2u16.to_le_bytes());
        program[8..10].copy_from_slice(&2u16.to_le_bytes());
        program[32] = 0xAB;
        let image = mz_image(&program).unwrap();
        assert_eq!((image.len(), image[0]), (600 - 32, 0xAB), "the overlay after the image left out");
        assert_eq!(mz_image(b"not a program"), None);
    }

    #[test]
    fn the_initial_data_is_the_image_s_mapped_and_carried_items() {
        let item = |field: &str, id: &str, ty: &str, size| Mapped {
            field: field.into(),
            id: id.parse().unwrap(),
            ty: ty.into(),
            size,
            via: None,
            mask: None,
            set: false,
            into: None,
        };
        let image: Vec<u8> = (0..16).collect();
        let items = [
            item("score", "GAME.EXE+00004", "u16", 2),
            // Carried memory (D137): as the image holds it too.
            item("saved.block", "GAME.EXE+00008", "u8[3]", 3),
            // Past the image's end, and another module's: not the image's.
            item("heap", "GAME.EXE+0000F", "u8[4]", 4),
            item("other", "OTHER.EXE+00000", "u8", 1),
        ];
        let k = Knowledge::default();
        let state = initial_state(&image, "GAME.EXE", &items, &k).unwrap();
        assert_eq!(state["score"], json!(0x0504));
        assert_eq!(state["saved.block"], json!([8, 9, 10]));
        assert!(!state.contains_key("heap") && !state.contains_key("other"));
    }

    #[test]
    fn what_an_edit_wrote_is_a_leaf_or_an_item_holding_it() {
        let mut set = serde_json::Map::new();
        set.insert("items[2].kind".to_owned(), json!(1));
        set.insert("items[3]".to_owned(), json!({}));
        assert!(
            edit_wrote(&set, "items[2].kind") && edit_wrote(&set, "items[3].flags") && edit_wrote(&set, "items[3]")
        );
        assert!(!edit_wrote(&set, "items[2].flags") && !edit_wrote(&set, "items[31].flags"));
    }

    #[test]
    fn an_edit_sets_the_leaves_it_wrote_wherever_the_item_is_now() {
        let m = Mapped {
            field: "table".into(),
            id: "GAME.EXE+00010".parse().unwrap(),
            ty: "u16[4]".into(),
            size: 8,
            via: None,
            mask: None,
            set: false,
            into: None,
        };
        let types = Types::default();
        let value = json!([10, 11, 12, 13]);
        let mut set = serde_json::Map::new();
        // The item is at 1000h now, whatever its own ID: the edit's bytes 1002h to 1006h are its second and third.
        let covered = edited_leaves(&m, 0x1000, (0x1002, 0x1006), &value, &types, &mut set).unwrap();
        assert_eq!(covered, 4);
        assert_eq!(Value::Object(set), json!({"table[1]": 11, "table[2]": 12}));
        // An edit past it writes none of it.
        let mut set = serde_json::Map::new();
        assert_eq!(edited_leaves(&m, 0x1000, (0x1008, 0x100A), &value, &types, &mut set).unwrap(), 0);
        assert!(set.is_empty());
    }

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
        let d = &found[0];
        assert_eq!((d.path.as_str(), &d.theirs, &d.ours), ("map[1]", &json!(1), &Value::Null), "the port has none");
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
            set: false,
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
