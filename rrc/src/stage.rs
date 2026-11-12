//! The workflow's stages and their gates (D158; docs/porting.md, "The workflow"): where a port stands, worked out
//! from the repository each time it is asked and never stored, so that it can't be marked done. `rrc status` names
//! the stage the port is at (the first whose gate lacks something), what its gate lacks, and the next step. A gate is
//! reported, not refused: stages overlap in honest work. What it measures is the port's scope (D156): each of its
//! sessions' recording, its census (`rrc compare --lockstep`) and its last comparisons, as they were kept.

use std::collections::BTreeMap;
use std::process::ExitCode;

use crate::args::Args;
use crate::cache::Cache;
use crate::compare::Census;
use crate::knowledge::Knowledge;
use crate::progress::Outcome;
use crate::repo::Repo;
use crate::say;
use crate::session::Session;

/// The stages, in order.
pub(crate) const STAGES: [&str; 8] =
    ["Set up", "Survey", "Sessions", "Skeleton", "Logic", "Data and start-up", "Presentation", "Play"];
/// Things a gate lacks, said at most without `--all`.
const SHOWN: usize = 4;
/// Sessions named in a line.
const NAMED: usize = 3;

/// A thing a stage's gate lacks, and the step that gives it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Lack {
    pub what: String,
    pub next: String,
}

fn lack(what: impl Into<String>, next: impl Into<String>) -> Lack {
    Lack { what: what.into(), next: next.into() }
}

/// A session of the scope, with what is kept of it: its census and its last comparisons, those made of the recording
/// it has now.
pub(crate) struct Held {
    pub name: String,
    pub session: Session,
    /// Why it has to be recorded (again), as the step that does it; none if its recording is the knowledge's.
    pub unrecorded: Option<String>,
    pub census: Option<Census>,
    /// By kind (`Outcome::kind`).
    pub outcomes: BTreeMap<String, Outcome>,
}

impl Held {
    /// Whether the session starts at power-on, not at another's tick (D118): one the port can run from its own start.
    fn starts_at_power_on(&self) -> bool {
        self.session.from.is_none()
    }

    fn ticks(&self) -> u64 {
        self.session.trace.as_ref().map_or(0, |t| t.ticks)
    }

    /// The comparison that shows the session matching to its end with nothing fed, among `kinds`; else why none does.
    fn matches(&self, kinds: &[&str]) -> Result<&Outcome, String> {
        let mut why = "not compared".to_owned();
        for kind in kinds {
            let Some(o) = self.outcomes.get(*kind) else { continue };
            match (&o.differs, o.fed.is_empty()) {
                (None, true) if o.last >= o.ticks => return Ok(o),
                (None, true) => why = format!("compared to tick {} of {}", o.last, o.ticks),
                (None, false) => why = format!("fed {}", o.fed.join(", ")),
                (Some(what), _) => why = format!("its {what} differs at tick {}", o.matched + 1),
            }
        }
        Err(why)
    }

    /// Whether anything kept of it was made by other code of the port than `port`'s (`sources::fingerprint`).
    pub(crate) fn stale(&self, port: &str) -> bool {
        self.census.as_ref().is_some_and(|c| c.port != port) || self.outcomes.values().any(|o| o.port != port)
    }
}

/// Where the port stands: what each stage's gate lacks, and the scope's sessions.
pub(crate) struct Standing {
    pub lacks: Vec<Vec<Lack>>,
    pub held: Vec<Held>,
    /// The sessions with something kept that the port's code as it is now didn't make.
    pub stale: Vec<String>,
}

impl Standing {
    /// The stage the port is at: the first whose gate lacks something.
    pub(crate) fn stage(&self) -> usize {
        self.lacks.iter().position(|lacks| !lacks.is_empty()).unwrap_or(STAGES.len() - 1)
    }
}

/// Up to [`NAMED`] of `names` (each with what `more` says of it), and how many more.
fn some<T>(items: &[T], text: impl Fn(&T) -> String) -> String {
    let shown: Vec<String> = items.iter().take(NAMED).map(text).collect();
    match items.len().saturating_sub(NAMED) {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
    }
}

/// `n of m sessions`, or `the session` when there is one.
fn of(n: usize, m: usize) -> String {
    if m == 1 { "the session".to_owned() } else { format!("{n} of {m} sessions") }
}

pub(crate) fn standing(repo: &Repo) -> Result<Standing, String> {
    let k = Knowledge::load(&repo.knowledge())?;
    let all = crate::session::names(repo)?;
    let (scope, unmatched) = crate::session::in_scope(repo)?;
    let cache = Cache::open(&repo.local)?;
    // What a recording reads, when the knowledge can say (a session can't be recorded before there is a sync point).
    let items = match k.ticks.syncs.is_empty() {
        true => None,
        false => Some(crate::trace::items(repo, &k, &repo.module())?),
    };
    let unrecorded = |name: &str, session: &Session| -> Result<Option<String>, String> {
        let Some(recorded) = &session.trace else { return Ok(Some(format!("rrc record {name}"))) };
        let Some((syncs, inputs, state)) = &items else { return Ok(None) };
        let edits = crate::edits::listed(&crate::edits::resolve(&k, &session.edits)?);
        Ok(match crate::trace::recorded_header(&cache, &recorded.hash) {
            None => Some(format!("rrc record {name}: its trace isn't in this machine's cache")),
            Some(h) if h.version != crate::trace::VERSION => {
                Some(format!("rrc record {name}: its trace is in an older format"))
            }
            Some(h) if (&h.syncs, &h.inputs, &h.state) != (syncs, inputs, state) => {
                Some(format!("rrc record {name}: the knowledge changed since it was recorded"))
            }
            Some(h) if h.edits != edits => Some(format!("rrc record {name}: its edits changed since it was recorded")),
            Some(_) => None,
        })
    };
    let mut held = Vec::new();
    for name in &scope {
        let session = Session::load(repo, name)?;
        let recorded = session.trace.as_ref().map(|t| t.hash.clone());
        let current = |made_of: &str| recorded.as_deref() == Some(made_of);
        let census = Census::load(repo, name)?.filter(|c| current(&c.recorded));
        let mut outcomes = Outcome::kept(repo, name)?;
        outcomes.retain(|_, o| current(&o.recorded));
        held.push(Held { unrecorded: unrecorded(name, &session)?, name: name.clone(), session, census, outcomes });
    }
    let port = crate::sources::fingerprint(&repo.root);
    let stale = held.iter().filter(|h| h.stale(&port)).map(|h| h.name.clone()).collect();

    let mut lacks = vec![set_up(repo)];

    // 1 Survey: the analysis, a first session into play, its loops and what they read, a recording, the sources.
    let mut survey = Vec::new();
    if crate::analysis::Index::load(repo)?.is_none() {
        survey.push(lack("the program isn't analysed", "rrc analyze"));
    }
    match all.first() {
        None => survey.push(lack(
            "no session yet",
            "rrc session add NAME --seconds N --inputs FILE: a session to reproduce, into play",
        )),
        Some(first) => {
            if k.ticks.syncs.is_empty() {
                survey.push(lack(
                    "no sync point: where a tick ends",
                    format!("find where a tick ends: rrc calls {first} lists the loops; then rrc sync ID NAME"),
                ));
            }
            if k.ticks.inputs.is_empty() {
                survey.push(lack(
                    "no boundary value: what a tick reads from outside",
                    format!("find what a tick reads from outside: rrc inputs {first}; then rrc input ID NAME"),
                ));
            }
            if !k.ticks.syncs.is_empty()
                && !k.ticks.inputs.is_empty()
                && let Some(next) = unrecorded(first, &Session::load(repo, first)?)?
            {
                survey.push(lack(format!("{first} isn't recorded as the knowledge is now"), next));
            }
        }
    }
    for line in crate::prior::open(repo)? {
        survey.push(lack(line, "rrc sources"));
    }
    lacks.push(survey);

    // 2 Sessions: the scope, each of its sessions recorded.
    let mut sessions = Vec::new();
    if repo.settings.port.scope.is_empty() {
        sessions.push(lack(
            "no scope: the sessions the port is to reproduce",
            "ask the person what the port is to reproduce; then rrc scope SESSION...",
        ));
    }
    for pattern in &unmatched {
        sessions.push(lack(format!("the scope names no session: {pattern}"), "rrc session add, or rrc play"));
    }
    let unrecorded: Vec<&Held> = held.iter().filter(|h| h.unrecorded.is_some()).collect();
    if let Some(first) = unrecorded.first() {
        let what = format!(
            "{} not recorded as the knowledge is now: {}",
            of(unrecorded.len(), held.len()),
            some(&unrecorded, |h| h.name.clone())
        );
        sessions.push(lack(what, first.unrecorded.clone().unwrap_or_default()));
    }
    lacks.push(sessions);

    // 3 Skeleton: a mapping, a port that runs, a census of every session, no named datum left out.
    let mut skeleton = Vec::new();
    let mapping = crate::mapping::Mapping::load(repo)?;
    let mapped = mapping.resolve(&k)?;
    if mapped.is_empty() {
        skeleton.push(lack(
            "nothing is mapped",
            format!(
                "map the state: {} names the port's fields and the original's items (docs/formats.md)",
                crate::mapping::MAPPING
            ),
        ));
    }
    if repo.settings.port.command.is_empty() {
        skeleton.push(lack(
            "the port's program isn't named",
            "rrc.toml: [port] command, the port's program (docs/formats.md)",
        ));
    }
    let uncounted: Vec<&Held> = held.iter().filter(|h| h.census.is_none()).collect();
    if let Some(first) = uncounted.first() {
        let what = format!(
            "{} without a census of the recording: {}",
            of(uncounted.len(), held.len()),
            some(&uncounted, |h| h.name.clone())
        );
        skeleton.push(lack(what, format!("rrc compare {} --lockstep (rrc work --run: every one)", first.name)));
    }
    if !mapped.is_empty() {
        let unmapped = crate::compare::unmapped_names(&k, &mapped, repo)?;
        if !unmapped.is_empty() {
            let what =
                format!("{} named data neither mapped nor ignored: {}", unmapped.len(), some(&unmapped, String::clone));
            skeleton.push(lack(what, format!("{}: map each, or [[ignore]] it", crate::mapping::MAPPING)));
        }
    }
    lacks.push(skeleton);

    // 4 Logic: the census without a field, each session matching in its fields to the end with nothing fed, the
    // quirk rule kept.
    let mut logic = Vec::new();
    let fields = |c: &Census| c.fields.len() + c.unset.len();
    let differing: Vec<&Held> = held.iter().filter(|h| h.census.as_ref().is_some_and(|c| fields(c) > 0)).collect();
    if !differing.is_empty() {
        let text = |h: &&Held| format!("{} ({})", h.name, h.census.as_ref().map_or(0, fields));
        let what = format!(
            "{} with fields in the census the port leaves otherwise or unset: {}",
            of(differing.len(), held.len()),
            some(&differing, text)
        );
        logic.push(lack(what, "rrc work: the list, the most passes first"));
    }
    let noting: Vec<&Held> = held.iter().filter(|h| h.census.as_ref().is_some_and(|c| !c.notes.is_empty())).collect();
    if !noting.is_empty() {
        let text = |h: &&Held| format!("{} ({})", h.name, h.census.as_ref().map_or(0, |c| c.notes.len()));
        let what = format!(
            "{} taking paths the port notes as not ported: {}",
            of(noting.len(), held.len()),
            some(&noting, text)
        );
        logic.push(lack(what, "rrc work: the list, the most passes first"));
    }
    const FIELDS: [&str; 4] = ["start-all", "start-fields", "transplant-all", "transplant-fields"];
    let unmatched: Vec<(&Held, String)> =
        held.iter().filter_map(|h| h.matches(&FIELDS).err().map(|why| (h, why))).collect();
    if let Some((first, _)) = unmatched.first() {
        let what = format!(
            "{} not matching in the mapped fields to the end with nothing fed: {}",
            of(unmatched.len(), held.len()),
            some(&unmatched, |(h, why)| format!("{} ({why})", h.name))
        );
        let from = if first.starts_at_power_on() { "" } else { " --transplant 1" };
        logic.push(lack(what, format!("rrc compare {} --fields{from}", first.name)));
    }
    let open = crate::quirks::open(repo)?;
    if let Some(first) = open.first() {
        logic.push(lack(format!("rrc check refuses {} things; the first: {first}", open.len()), "rrc check"));
    }
    lacks.push(logic);

    // 5 Data and start-up: each session that starts at power-on matches in its fields from the port's own start.
    let mut data = Vec::new();
    let starting: Vec<&Held> = held.iter().filter(|h| h.starts_at_power_on()).collect();
    let unmatched: Vec<(&Held, String)> =
        starting.iter().filter_map(|h| h.matches(&["start-all", "start-fields"]).err().map(|why| (*h, why))).collect();
    if let Some((first, _)) = unmatched.first() {
        let what = format!(
            "{} from power-on not matching in the fields from the port's own start: {}",
            of(unmatched.len(), starting.len()),
            some(&unmatched, |(h, why)| format!("{} ({why})", h.name))
        );
        data.push(lack(what, format!("rrc compare {} --fields", first.name)));
    }
    lacks.push(data);

    // 6 Presentation: each session matches in frames and in every device the original sounds, from the port's own
    // start (from a transplant, one that starts at another's tick); the census without a frame or a sound.
    let mut presentation = Vec::new();
    let mut unmatched: Vec<(&Held, String)> = Vec::new();
    for h in &held {
        let kind = if h.starts_at_power_on() { "start-all" } else { "transplant-all" };
        match h.matches(&[kind]) {
            Err(why) => unmatched.push((h, why)),
            Ok(o) if !o.drew => unmatched.push((h, "the port draws no frames".to_owned())),
            Ok(o) if !o.unsounded.is_empty() => {
                unmatched.push((h, format!("the port makes no {} sound", o.unsounded.join(", "))));
            }
            Ok(_) => {}
        }
    }
    if let Some((first, _)) = unmatched.first() {
        let what = format!(
            "{} not matching in frames and sound: {}",
            of(unmatched.len(), held.len()),
            some(&unmatched, |(h, why)| format!("{} ({why})", h.name))
        );
        let from = if first.starts_at_power_on() { "" } else { " --transplant 1" };
        // What the port doesn't make yet is written, not compared again.
        let next = match unmatched[0].1.as_str() {
            "the port draws no frames" => {
                "the port's presentation draws no frames yet: Presentation::draw (docs/porting.md, stage 6)".to_owned()
            }
            why if why.starts_with("the port makes no") => {
                format!("{why} yet: Presentation::sound and its SOUND_DEVICES (docs/porting.md, stage 6)")
            }
            _ => format!("rrc compare {}{from}", first.name),
        };
        presentation.push(lack(what, next));
    }
    let seen: Vec<&Held> =
        held.iter().filter(|h| h.census.as_ref().is_some_and(|c| c.screen.is_some() || c.sound.is_some())).collect();
    if !seen.is_empty() {
        let what = format!(
            "{} with frames or sound that differ in the census: {}",
            of(seen.len(), held.len()),
            some(&seen, |h| h.name.clone())
        );
        presentation.push(lack(what, "rrc work: the list, the most passes first"));
    }
    lacks.push(presentation);

    // 7 Play: not something rrc can test; the person closes it.
    lacks.push(vec![lack(
        "play isn't tested by rrc: the port's live tests, saves loaded both ways with the original, the person's \
         play-test",
        "PORT play (docs/porting.md, stage 7); what the person's play finds becomes sessions",
    )]);
    Ok(Standing { lacks, held, stale })
}

/// 0 Set up: the game's copy as a recorded version, Clockwork, the person's rules.
fn set_up(repo: &Repo) -> Vec<Lack> {
    let mut lacks = Vec::new();
    match crate::setup::which_version(repo) {
        Ok(_) => {}
        Err(_) if repo.game_dir().is_err() => {
            lacks.push(lack("the game's folder isn't set", "rrc game <the game's folder>"));
        }
        Err(_) if repo.settings.versions.is_empty() => lacks.push(lack(
            "this copy of the game isn't a recorded version",
            "rrc game --add-version <name>: record this copy's files as a version",
        )),
        Err(e) => lacks.push(lack(e, "rrc game: how this copy differs")),
    }
    if let Err(e) = crate::clockwork::find(&repo.local) {
        lacks.push(lack(format!("Clockwork isn't found: {e}"), "rrc doctor"));
    }
    match crate::entry::unanswered(&repo.root) {
        None => lacks.push(lack(
            format!("no {}: the person's rules for this port", crate::entry::RULES),
            "rrc hooks writes it; then ask the person its questions",
        )),
        Some(left) if !left.is_empty() => lacks.push(lack(
            format!(
                "{}: {} questions for the person not answered; the first: {}",
                crate::entry::RULES,
                left.len(),
                left[0]
            ),
            format!("ask the person, and write each answer in its place in {}", crate::entry::RULES),
        )),
        Some(_) => {}
    }
    lacks
}

/// `rrc status [--all]`: the stage the port is at, what its gate lacks and the next step; with `--all`, every
/// stage's gate, the scope's sessions and where each stands.
pub(crate) fn status(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &[], &["--all"])?;
    a.positionals(0, 0, "status [--all]")?;
    let standing = standing(repo)?;
    let at = standing.stage();
    if a.has("--all") {
        for (stage, lacks) in standing.lacks.iter().enumerate() {
            match lacks.is_empty() {
                true => say("gate", format!("{stage} {}: holds", STAGES[stage])),
                false => {
                    for l in lacks {
                        say("gate", format!("{stage} {}: lacks: {}", STAGES[stage], l.what));
                    }
                }
            }
        }
        sessions(&standing);
        // The top of the work list, what parts not ported still give the port, the quirks, and what the work was
        // made of: with the gates, what a handoff would say (D158).
        let items = crate::work::list(&standing);
        say("work", format!("{} items in the censuses (rrc work)", items.len()));
        crate::work::show(&items, 5);
        let k = Knowledge::load(&repo.knowledge())?;
        let parts = k.ticks.inputs.iter().filter(|i| !i.part.is_empty()).count();
        if parts + k.ticks.hooks.len() > 0 {
            say(
                "given",
                format!(
                    "{} hooks and {parts} inputs give the port what parts not ported make (rrc hook, rrc input)",
                    k.ticks.hooks.len()
                ),
            );
        }
        say("quirks", format!("{} registered (rrc quirk)", crate::quirks::Quirks::load(repo)?.quirks.len()));
        let journal = crate::journal::load(repo);
        if !journal.is_empty() {
            say("mix", format!("{} over {} commands (rrc journal)", crate::journal::mix(&journal), journal.len()));
        }
    }
    let done = match at {
        0 => String::new(),
        1 => "; 0 holds".to_owned(),
        n => format!("; 0 to {} hold", n - 1),
    };
    say("stage", format!("{at} {}, of 0 {} to 7 {}{done}", STAGES[at], STAGES[0], STAGES[7]));
    let lacks = &standing.lacks[at];
    for l in lacks.iter().take(SHOWN) {
        say("lacks", &l.what);
    }
    if lacks.len() > SHOWN {
        say("lacks", format!("{} more (rrc status --all)", lacks.len() - SHOWN));
    }
    if let Some(first) = lacks.first() {
        say("next", &first.next);
    }
    if !standing.stale.is_empty() {
        say(
            "stale",
            format!(
                "{} last compared before the port's code changed: {} (rrc work --run counts again)",
                of(standing.stale.len(), standing.held.len()),
                some(&standing.stale, String::clone)
            ),
        );
    }
    // What a commit is refused for (D148), whatever the stage: named here unless the stage's own gate just did.
    if at != 4 {
        let open = crate::quirks::open(repo)?;
        if let Some(first) = open.first() {
            say("open", format!("{} rrc check refuses; the first: {first}", open.len()));
        }
    }
    crate::work::standing(repo)?;
    Ok(ExitCode::SUCCESS)
}

/// The scope's sessions, each with where it stands; those that match in everything as a count.
fn sessions(standing: &Standing) {
    let mut whole = 0;
    for h in &standing.held {
        let kind = if h.starts_at_power_on() { "start-all" } else { "transplant-all" };
        let all = h.matches(&[kind]).is_ok_and(|o| o.drew && o.unsounded.is_empty());
        let census = h.census.as_ref().is_some_and(|c| !c.differs() && c.unset.is_empty());
        if all && census {
            whole += 1;
            continue;
        }
        let fields = match h.matches(&["start-all", "start-fields", "transplant-all", "transplant-fields"]) {
            Ok(o) if o.from == 0 => "fields match from the port's start".to_owned(),
            Ok(o) => format!("fields match from a transplant at tick {}", o.from),
            Err(why) => format!("fields: {why}"),
        };
        let census = match &h.census {
            None => "no census".to_owned(),
            Some(c) => format!(
                "census: {} fields, {} unset, {} notes{}{}",
                c.fields.len(),
                c.unset.len(),
                c.notes.len(),
                if c.screen.is_some() { ", frames differ" } else { "" },
                if c.sound.is_some() { ", sound differs" } else { "" }
            ),
        };
        say("session", format!("{} ({} ticks): {fields}; {census}", h.name, h.ticks()));
    }
    if whole > 0 {
        say("sessions", format!("{whole} of {} match in state, frames and sound", standing.held.len()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(from: u64, differs: Option<&str>, matched: u64) -> Outcome {
        Outcome {
            from,
            first: from.max(1),
            last: 100,
            matched,
            ticks: 100,
            differs: differs.map(str::to_owned),
            ..Outcome::default()
        }
    }

    fn held(outcomes: &[(&str, Outcome)], from: Option<&str>) -> Held {
        let base = |name: &str| crate::session::Base { session: name.to_owned(), tick: 5, cycle: None };
        Held {
            name: "play".into(),
            session: Session { from: from.map(base), ..Session::default() },
            unrecorded: None,
            census: None,
            outcomes: outcomes.iter().map(|(kind, o)| (kind.to_string(), o.clone())).collect(),
        }
    }

    #[test]
    fn a_session_matches_by_a_comparison_to_its_end_with_nothing_fed() {
        let all = ["start-all", "start-fields", "transplant-all", "transplant-fields"];
        assert_eq!(held(&[], None).matches(&all).unwrap_err(), "not compared");
        let screen = held(&[("start-all", outcome(0, Some("screen"), 40))], None);
        assert_eq!(screen.matches(&all).unwrap_err(), "its screen differs at tick 41");
        // The fields alone match where the screen stopped the whole comparison.
        let fields =
            held(&[("start-all", outcome(0, Some("screen"), 40)), ("start-fields", outcome(0, None, 100))], None);
        assert!(fields.matches(&all).is_ok() && fields.matches(&["start-all"]).is_err());
        let fed = Outcome { fed: vec!["enemies".into()], ..outcome(1, None, 100) };
        assert_eq!(held(&[("transplant-fields", fed)], None).matches(&all).unwrap_err(), "fed enemies");
        let short = Outcome { last: 60, ..outcome(1, None, 60) };
        assert_eq!(
            held(&[("transplant-fields", short)], None).matches(&all).unwrap_err(),
            "compared to tick 60 of 100"
        );
        assert!(held(&[], Some("walk")).session.from.is_some() && !held(&[], Some("walk")).starts_at_power_on());
    }

    #[test]
    fn what_other_code_of_the_port_made_is_stale() {
        let made = |port: &str| Outcome { port: port.into(), ..outcome(0, None, 100) };
        let h = held(&[("start-all", made("abc"))], None);
        assert!(!h.stale("abc") && h.stale("abd"));
        assert!(!held(&[], None).stale("abc"), "nothing kept, nothing stale");
    }

    #[test]
    fn the_stage_is_the_first_whose_gate_lacks_something() {
        let l = || vec![lack("x", "y")];
        let standing = Standing { lacks: vec![vec![], vec![], l(), vec![], l()], held: Vec::new(), stale: Vec::new() };
        assert_eq!(standing.stage(), 2);
        let done = Standing { lacks: vec![Vec::new(); 8], held: Vec::new(), stale: Vec::new() };
        assert_eq!(done.stage(), 7);
        assert_eq!(some(&["a", "b", "c", "d", "e"], |s| s.to_string()), "a, b, c and 2 more");
        assert_eq!((of(1, 1), of(2, 5)), ("the session".into(), "2 of 5 sessions".into()));
    }
}
