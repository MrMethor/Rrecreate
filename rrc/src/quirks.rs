//! The quirk register (docs/design.md §6.5, D18, D74): `quirks.toml` in the port repository lists the
//! original's bugs the faithful port keeps. Each is an explicit branch in the port's code, which names
//! it (`rrc_runtime::keeps("enemy_overrun")`), kept in comparisons and fixed by a toggle elsewhere.
//! `rrc quirk` keeps the register and says where the port's code names each; `rrc suspects` finds
//! candidates in a session: the original's reads and writes past named data's ends (Clockwork's bounds
//! checks) and its reads of memory nothing wrote.

use std::collections::BTreeMap;
use std::process::ExitCode;

use ids::Id;
use serde_json::{Value, json};

use crate::analysis::Analysis;
use crate::args::Args;
use crate::knowledge::{Knowledge, is_name, parse_id};
use crate::places::Places;
use crate::repo::{Repo, read_toml, write_toml};
use crate::say;
use crate::session::Session;
use crate::trace::Trace;

pub const QUIRKS: &str = "quirks.toml";
pub const SUSPECTS: &str = "suspects.toml";
/// Suspects shown of each kind.
const SHOWN: usize = 20;
/// An instruction that reaches past this many named data is a sweep (a clear or a copy running over
/// them, `rep stosw`), not an index past one.
const SWEEP: usize = 3;

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Quirks {
    #[serde(default, rename = "quirk", skip_serializing_if = "Vec::is_empty")]
    pub quirks: Vec<Quirk>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Quirk {
    /// How the port's code names it.
    pub name: String,
    /// The original's code that does it: an ID.
    pub at: String,
    /// What happens.
    pub what: String,
    /// What its switch does when the bug is fixed: every bug has both (D160).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fix: String,
    /// Where its switch was shown to change a run (D160): the session and the first difference its fix makes there,
    /// as `rrc compare SESSION --fix NAME` found it while the port, the bug kept, matches the original to that tick.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub shown: String,
    pub provenance: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub evidence: String,
}

impl Quirks {
    pub fn load(repo: &Repo) -> Result<Quirks, String> {
        let path = repo.root.join(QUIRKS);
        if path.is_file() { read_toml(&path) } else { Ok(Quirks::default()) }
    }

    fn save(&self, repo: &Repo) -> Result<(), String> {
        write_toml(&repo.root.join(QUIRKS), self)
    }

    /// What's wrong with the register: names that aren't names or are taken twice, IDs that aren't
    /// IDs of `module`, quirks that don't say what happens.
    pub fn problems(&self, module: &str) -> Vec<String> {
        let mut problems = Vec::new();
        for (i, q) in self.quirks.iter().enumerate() {
            let wrong = |e: String| format!("{QUIRKS}: quirk {}: {e}", q.name);
            if !is_name(&q.name) {
                problems.push(wrong("isn't a name".into()));
            }
            if self.quirks[..i].iter().any(|other| other.name == q.name) {
                problems.push(wrong("named twice".into()));
            }
            match parse_id(&q.at) {
                Ok(id) if id.module() != module => problems.push(wrong(format!("{id} isn't in {module}"))),
                Ok(_) => {}
                Err(e) => problems.push(wrong(format!("at: {e}"))),
            }
            if q.what.trim().is_empty() {
                problems.push(wrong("say what happens (what)".into()));
            }
            if q.fix.trim().is_empty() {
                problems.push(wrong(
                    "no fix: say what its switch does when the bug is fixed (rrc quirk NAME --fix TEXT); every bug \
                     is kept and has a switch (D160)"
                        .into(),
                ));
            }
        }
        problems
    }
}

/// What `rrc suspects` found (D148): each instruction, until it's judged a quirk or not a bug, and which runs of which
/// sessions have been looked through.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Suspects {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ran: Vec<Ran>,
    #[serde(default, rename = "suspect", skip_serializing_if = "Vec::is_empty")]
    pub suspects: Vec<Suspect>,
}

/// A session's run that `rrc suspects` looked through.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Ran {
    pub session: String,
    pub run: String,
}

/// An instruction `rrc suspects` found.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Suspect {
    /// The instruction: an ID.
    pub at: String,
    /// `past` named data's end, or `uninit`: reads memory nothing wrote.
    pub kind: String,
    /// What it did, as first found.
    pub what: String,
    /// The sessions that find it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<String>,
    /// `quirk NAME`, or `not a bug: why`. Empty: not judged yet.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub verdict: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provenance: String,
}

impl Suspects {
    pub fn load(repo: &Repo) -> Result<Suspects, String> {
        let path = repo.root.join(SUSPECTS);
        if path.is_file() { read_toml(&path) } else { Ok(Suspects::default()) }
    }

    fn save(&self, repo: &Repo) -> Result<(), String> {
        write_toml(&repo.root.join(SUSPECTS), self)
    }

    /// `name`'s findings of this run in place of its last: an instruction found again keeps its verdict; one no
    /// session finds any more goes, unless it was judged.
    fn found(&mut self, name: &str, run: &str, found: Vec<Suspect>) {
        for suspect in &mut self.suspects {
            suspect.sessions.retain(|s| s != name);
        }
        for new in found {
            match self.suspects.iter_mut().find(|s| s.at == new.at && s.kind == new.kind) {
                Some(old) => old.sessions.push(name.to_owned()),
                None => self.suspects.push(Suspect { sessions: vec![name.to_owned()], ..new }),
            }
        }
        self.suspects.retain(|s| !s.sessions.is_empty() || !s.verdict.is_empty());
        self.suspects.sort_by(|a, b| (&a.at, &a.kind).cmp(&(&b.at, &b.kind)));
        self.ran.retain(|r| r.session != name);
        self.ran.push(Ran { session: name.to_owned(), run: run.to_owned() });
        self.ran.sort_by(|a, b| a.session.cmp(&b.session));
    }
}

/// What the rule that every bug of the original is registered and branched on (D18, D74, D148) still asks of the port:
/// `rrc check` refuses these and `rrc status` names the first.
pub(crate) fn open(repo: &Repo) -> Result<Vec<String>, String> {
    let register = Quirks::load(repo)?;
    let names: Vec<String> = register.quirks.iter().map(|q| q.name.clone()).collect();
    let mut open = Vec::new();
    let kept = crate::sources::kept(&repo.root);
    for (name, (file, line)) in &kept {
        if !names.contains(name) {
            open.push(format!("{file}:{line}: keeps({name:?}) isn't registered: rrc quirk {name} --at ID --what TEXT"));
        }
    }
    for name in &names {
        if !kept.contains_key(name) {
            open.push(format!("quirk {name}: no code branches on it: rrc_runtime::keeps({name:?})"));
        }
    }
    for (file, line, text) in crate::sources::unmarked(&repo.root, &names) {
        open.push(format!("{file}:{line}: says a bug or quirk without naming a registered one: {text}"));
    }
    // The notes and the knowledge too (D160): a bug written down there and nowhere else is a bug missed.
    for (file, line, text) in crate::sources::unmarked_text(&repo.root, &names) {
        let text: String = text.chars().take(90).collect();
        open.push(format!(
            "{file}:{line}: says a bug or quirk without naming a registered one (or saying `not a bug`): {text}"
        ));
    }
    let suspects = Suspects::load(repo)?;
    for name in crate::session::names(repo)? {
        let Some(recorded) = Session::load(repo, &name)?.trace else { continue };
        if !suspects.ran.iter().any(|r| r.session == name && r.run == recorded.run) {
            open.push(format!("session {name}: its run hasn't been looked through: rrc suspects {name}"));
        }
    }
    for s in &suspects.suspects {
        if s.verdict.is_empty() && !register.quirks.iter().any(|q| q.at == s.at) {
            open.push(format!(
                "suspect {} ({}): {}: judge it: rrc suspects --judge {} (--quirk NAME | --not-a-bug WHY)",
                s.at, s.kind, s.what, s.at
            ));
        }
    }
    Ok(open)
}

/// `rrc suspects --judge ID`: what an instruction `rrc suspects` found turned out to be.
fn judge(repo: &Repo, a: &Args, at: &str) -> Result<ExitCode, String> {
    let id = parse_id(at)?.to_string();
    let verdict = match (a.value("--quirk"), a.value("--not-a-bug")) {
        (Some(name), None) => {
            if !Quirks::load(repo)?.quirks.iter().any(|q| q.name == name) {
                return Err(format!("no quirk {name}: rrc quirk {name} --at ID --what TEXT"));
            }
            format!("quirk {name}")
        }
        (None, Some(why)) if !why.trim().is_empty() => format!("not a bug: {why}"),
        _ => return Err("--judge ID --quirk NAME, or --judge ID --not-a-bug WHY".into()),
    };
    let mut suspects = Suspects::load(repo)?;
    let provenance =
        a.value("--by").map(str::to_owned).or_else(|| std::env::var("RRC_BY").ok()).unwrap_or_else(|| "agent".into());
    let mut judged = 0;
    for s in suspects.suspects.iter_mut().filter(|s| s.at == id) {
        (s.verdict, s.provenance) = (verdict.clone(), provenance.clone());
        judged += 1;
    }
    if judged == 0 {
        return Err(format!("no suspect at {id}"));
    }
    suspects.save(repo)?;
    say("judged", format!("{id}: {verdict}"));
    Ok(ExitCode::SUCCESS)
}

/// `rrc quirk`: the register, or a quirk added, changed or removed.
pub(crate) fn quirk(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let options = ["--at", "--what", "--fix", "--evidence", "--by"];
    let a = Args::parse(args, &options, &["--remove"])?;
    let usage = "quirk [NAME (--at ID --what TEXT --fix TEXT [--evidence TEXT] [--by WHO] | --remove)]";
    let names = a.positionals(0, 1, usage)?;
    let mut register = Quirks::load(repo)?;
    let Some(name) = names.first() else {
        return list(repo, &register);
    };
    let found = register.quirks.iter().position(|q| q.name == **name);
    if a.has("--remove") {
        let i = found.ok_or_else(|| format!("no quirk {name}"))?;
        register.quirks.remove(i);
        register.save(repo)?;
        say("removed", name);
        return Ok(ExitCode::SUCCESS);
    }
    let mut q = found.map(|i| register.quirks[i].clone()).unwrap_or_default();
    q.name = name.to_string();
    let value = |flag: &str, old: &str| a.value(flag).map_or_else(|| old.to_owned(), str::to_owned);
    (q.at, q.what, q.fix, q.evidence) =
        (value("--at", &q.at), value("--what", &q.what), value("--fix", &q.fix), value("--evidence", &q.evidence));
    q.provenance = a
        .value("--by")
        .map(str::to_owned)
        .or_else(|| std::env::var("RRC_BY").ok())
        .unwrap_or_else(|| if q.provenance.is_empty() { "agent".into() } else { q.provenance.clone() });
    if q.at.is_empty() || q.what.is_empty() || q.fix.is_empty() {
        return Err(format!(
            "a quirk needs --at (the original's code), --what (what happens) and --fix (what its switch does when the \
             bug is fixed): {usage}"
        ));
    }
    // What the fix changed is to be shown again.
    if a.value("--fix").is_some() || a.value("--at").is_some() {
        q.shown.clear();
    }
    match found {
        Some(i) => register.quirks[i] = q.clone(),
        None => register.quirks.push(q.clone()),
    }
    register.quirks.sort_by(|a, b| a.name.cmp(&b.name));
    // This quirk's own problems: another's (a register kept before a switch was asked of each) don't stop this one.
    let own = format!("{QUIRKS}: quirk {}: ", q.name);
    if let Some(problem) = register.problems(&repo.module()).into_iter().find(|p| p.starts_with(&own)) {
        return Err(problem);
    }
    register.save(repo)?;
    say("quirk", format!("{} at {}{}", q.name, q.at, if found.is_some() { " (changed)" } else { "" }));
    say_switch(&q);
    say_sites(repo, &q.name);
    Ok(ExitCode::SUCCESS)
}

fn list(repo: &Repo, register: &Quirks) -> Result<ExitCode, String> {
    if register.quirks.is_empty() {
        say("quirks", format!("none ({QUIRKS}; rrc suspects SESSION finds candidates)"));
    }
    for q in &register.quirks {
        say("quirk", format!("{} at {}: {}", q.name, q.at, q.what));
        say_switch(q);
        say_sites(repo, &q.name);
    }
    Ok(ExitCode::SUCCESS)
}

/// A quirk's switch: what its fix does, and where a session showed it.
fn say_switch(q: &Quirk) {
    say("  fix", if q.fix.is_empty() { "not said: rrc quirk NAME --fix TEXT" } else { &q.fix });
    match q.shown.is_empty() {
        false => say("  shown", &q.shown),
        true => say(
            "  shown",
            format!(
                "not yet: rrc compare SESSION --fix {}, on a session that reaches it and matches with it kept",
                q.name
            ),
        ),
    }
}

/// The registered quirks whose switch no session has shown to change a run (D160).
pub(crate) fn unshown(repo: &Repo) -> Result<Vec<String>, String> {
    Ok(Quirks::load(repo)?.quirks.into_iter().filter(|q| q.shown.is_empty()).map(|q| q.name).collect())
}

/// `rrc compare SESSION --fix NAME` found what the fix of quirk `name` changes, where the port matches with the bug
/// kept: kept with the quirk (D160).
pub(crate) fn shown(repo: &Repo, name: &str, what: &str) -> Result<(), String> {
    let mut register = Quirks::load(repo)?;
    let q = register.quirks.iter_mut().find(|q| q.name == name).ok_or_else(|| format!("no quirk {name}"))?;
    q.shown = what.to_owned();
    register.save(repo)
}

/// Everything of the first rule that `rrc check` refuses: what is wrong in the register itself (a quirk with no fix
/// said), then what the port still owes it ([`open`]).
pub(crate) fn refused(repo: &Repo) -> Result<Vec<String>, String> {
    let mut refused = Quirks::load(repo)?.problems(&repo.module());
    refused.extend(open(repo)?);
    Ok(refused)
}

/// The first rule and the register's state, in a line (D160): said by `rrc status` and `rrc check` every time, so
/// that it is never out of sight.
pub(crate) fn rule_line(repo: &Repo) -> Result<String, String> {
    let register = Quirks::load(repo)?;
    let unshown = register.quirks.iter().filter(|q| q.shown.is_empty()).count();
    let refused = refused(repo)?.len();
    Ok(format!(
        "every bug of the original is reproduced and has a switch, registered when found (rrc quirk): {} registered, \
         {unshown} with a switch not shown yet, {refused} things rrc check refuses",
        register.quirks.len()
    ))
}

/// Where the port's code names quirk `name`, or that it doesn't yet.
fn say_sites(repo: &Repo, name: &str) {
    let sites = crate::sources::naming(&repo.root, name, None, 3);
    if sites.is_empty() {
        say("  port", format!("no code names it yet: branch on rrc_runtime::keeps({name:?})"));
    } else {
        let at: Vec<String> = sites.iter().map(|(file, line)| format!("{file}:{line}")).collect();
        say("  port", at.join(", "));
    }
}

/// `rrc suspects SESSION`: quirk candidates in the session's run (docs/design.md §6.5): the original's
/// instructions that reach past named data's ends, and its reads of memory nothing wrote. They're kept
/// (`suspects.toml`, D148) until judged; `--judge ID` judges one.
pub(crate) fn suspects(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--lines", "--judge", "--quirk", "--not-a-bug", "--by"], &[])?;
    if let Some(at) = a.value("--judge") {
        return judge(repo, &a, at);
    }
    let [name] = a.positionals(1, 1, "suspects SESSION [--lines N]")? else { unreachable!() };
    let shown: usize = a.value("--lines").map_or(Ok(SHOWN), str::parse).map_err(|_| "--lines is a count")?;
    let session = Session::load(repo, name)?;
    let k = Knowledge::load(&repo.knowledge())?;
    let places = Places::new(repo, &session);
    places.objects()?;
    // Named data with a size, as the objects the bounds checks guard.
    let module = repo.module();
    let data: Vec<(&Id, u32, &str)> = k
        .data
        .iter()
        .filter(|(id, _)| id.module() == module || id.is_memory())
        .filter_map(|(id, d)| Some((id, d.size?, d.decision.name.as_str())))
        .collect();
    let mut objects = Vec::new();
    for (id, size, _) in &data {
        objects.push(json!({"at": places.address(&id.to_string())?, "size": size}));
    }
    // From another session's tick, what was written before it isn't a read of nothing (D150).
    let unwritten = crate::snapshots::unwritten(repo, &session)?;
    let (mut clockwork, _) = crate::trace::begin_run(repo, &session)?;
    clockwork.ask("bounds", json!({ "objects": objects }))?;
    clockwork.ask("uninit", crate::snapshots::track_from(unwritten.as_deref()))?;
    clockwork.ask("coverage", json!({}))?;
    crate::trace::run_to_end(repo, &session, &mut clockwork)?;
    let bounds = clockwork.ask("bounds_end", Value::Null)?;
    let uninit = clockwork.ask("uninit_end", Value::Null)?;
    let covered = clockwork.ask("coverage_end", Value::Null)?;

    let when = When::new(repo, &session);
    let analysis = Analysis::load(repo)?;
    let code = |address: &str| code_text(&places, &k, analysis.as_ref(), address);
    let register = Quirks::load(repo)?;
    let registered = |address: &str| {
        let id = places.id(address).map(|id| id.to_string()).unwrap_or_default();
        register.quirks.iter().find(|q| q.at == id).map(|q| format!(" (quirk {})", q.name)).unwrap_or_default()
    };
    let all = bounds["findings"].as_array().cloned().unwrap_or_default();
    let objects_by = |f: &Value| all.iter().filter(|g| g["instruction"] == f["instruction"]).count();
    let (sweeps, findings): (Vec<Value>, Vec<Value>) = all.iter().cloned().partition(|f| objects_by(f) >= SWEEP);
    // Reads of memory nothing wrote, by the instruction that made them.
    let mut reads: BTreeMap<String, Unread> = BTreeMap::new();
    for read in uninit["reads"].as_array().into_iter().flatten() {
        let by = read["instruction"].as_str().unwrap_or("?").to_owned();
        let cycle = read["cycle"].as_u64().unwrap_or(0);
        let at =
            places.id(read["at"].as_str().unwrap_or("?")).map_or_else(|| read["at"].to_string(), |id| id.to_string());
        let value = read["value"].as_u64().unwrap_or(0);
        reads.entry(by).or_insert(Unread { bytes: 0, at, value, cycle }).bytes += 1;
    }
    let dropped = uninit["dropped"].as_u64().unwrap_or(0);
    say(
        "suspects",
        format!(
            "{name}: {} reaching past named data, {} reading memory nothing wrote{}",
            count(findings.len(), "instruction"),
            count(reads.len(), "instruction"),
            if dropped > 0 { format!(" ({dropped} reads past the limit)") } else { String::new() }
        ),
    );
    let mut kept = Vec::new();
    for f in &findings {
        let Some(&(id, size, data_name)) = f["object"].as_u64().and_then(|i| data.get(i as usize)) else { continue };
        let Some(by) = f["instruction"].as_str().and_then(|by| places.id(by)) else { continue };
        let offsets: Vec<String> = f["offsets"].as_array().into_iter().flatten().map(Value::to_string).collect();
        kept.push(Suspect {
            at: by.to_string(),
            kind: "past".into(),
            what: format!("{data_name} ({id}, {}): offsets {}", count(size as usize, "byte"), offsets.join(" ")),
            ..Suspect::default()
        });
    }
    for (by, unread) in &reads {
        let Some(by) = places.id(by) else { continue };
        kept.push(Suspect {
            at: by.to_string(),
            kind: "uninit".into(),
            what: format!("{} from {}", count(unread.bytes as usize, "byte"), unread.at),
            ..Suspect::default()
        });
    }
    let mut register_of_suspects = Suspects::load(repo)?;
    let run = session.trace.as_ref().map(|t| t.run.clone()).unwrap_or_default();
    register_of_suspects.found(name, &run, kept);
    register_of_suspects.save(repo)?;
    crate::unrun::keep(repo, &session, &places, &covered)?;
    for f in findings.iter().take(shown) {
        let Some(&(id, size, data_name)) = f["object"].as_u64().and_then(|i| data.get(i as usize)) else { continue };
        let by = f["instruction"].as_str().unwrap_or("?");
        let offsets: Vec<String> = f["offsets"].as_array().into_iter().flatten().map(Value::to_string).collect();
        say(
            "past",
            format!(
                "{data_name} ({id}, {}) by {}{}: offsets {}, {}, first {}",
                count(size as usize, "byte"),
                code(by),
                registered(by),
                offsets.join(" "),
                count(f["outside"].as_u64().unwrap_or(0) as usize, "time"),
                when.text(f["cycle"].as_u64().unwrap_or(0))
            ),
        );
    }
    let mut by_first: Vec<(&String, &Unread)> = reads.iter().collect();
    by_first.sort_by_key(|(_, r)| r.cycle);
    for (by, Unread { bytes, at, value, cycle }) in by_first.iter().take(shown) {
        say(
            "uninit",
            format!(
                "{} by {}{}: {at} first (it held {value:02X}), {}",
                count(*bytes as usize, "byte"),
                code(by),
                registered(by),
                when.text(*cycle)
            ),
        );
    }
    let mut swept: Vec<(&str, usize, u64)> = Vec::new();
    for f in &sweeps {
        let by = f["instruction"].as_str().unwrap_or("?");
        let cycle = f["cycle"].as_u64().unwrap_or(0);
        match swept.iter_mut().find(|(at, _, _)| *at == by) {
            Some((_, n, first)) => (*n, *first) = (*n + 1, (*first).min(cycle)),
            None => swept.push((by, 1, cycle)),
        }
    }
    for (by, n, cycle) in &swept {
        say(
            "sweep",
            format!(
                "{} past {n} named data, first {}: a clear or a copy running over them",
                code(by),
                when.text(*cycle)
            ),
        );
    }
    let more = findings.len().saturating_sub(shown) + reads.len().saturating_sub(shown);
    if more > 0 {
        say("...", format!("{more} more (--lines N shows more)"));
    }
    if !findings.is_empty() || !reads.is_empty() {
        say("next", "read each one's code: a bug the port has to keep is a quirk (rrc quirk NAME --at ID --what TEXT)");
        say("kept", format!("in {SUSPECTS}, until judged: rrc suspects --judge ID (--quirk NAME | --not-a-bug WHY)"));
    }
    Ok(ExitCode::SUCCESS)
}

/// An instruction's reads of memory nothing wrote: how many bytes, and the first's address, value and
/// cycle.
struct Unread {
    bytes: u64,
    at: String,
    value: u64,
    cycle: u64,
}

fn count(n: usize, what: &str) -> String {
    if n == 1 { format!("1 {what}") } else { format!("{n} {what}s") }
}

/// An instruction's ID with the function Ghidra's analysis found it in (`TOY.EXE+002FF in update`), as
/// the knowledge names that.
pub(crate) fn code_text(places: &Places, k: &Knowledge, analysis: Option<&Analysis>, address: &str) -> String {
    let Some(id) = places.id(address) else { return address.to_owned() };
    match analysis.and_then(|a| a.containing(&id)) {
        Some((entry, _)) => {
            format!("{id} in {}", k.functions.get(entry).map_or_else(|| entry.to_string(), |d| d.name.clone()))
        }
        None => id.to_string(),
    }
}

/// When a cycle of the session's run was, by its recorded trace's ticks: the pass to tick N.
pub(crate) struct When {
    cycles: Vec<u64>,
}

impl When {
    pub(crate) fn new(repo: &Repo, session: &Session) -> When {
        let trace = session.trace.as_ref().and_then(|recorded| {
            let cache = crate::cache::Cache::open(&repo.local).ok()?;
            Trace::parse(&String::from_utf8_lossy(&cache.get(&recorded.hash).ok()?)).ok()
        });
        When { cycles: trace.map(|t| t.ticks.iter().map(|tick| tick.cycle).collect()).unwrap_or_default() }
    }

    /// The tick whose state is the first after the cycle: the pass it is in leads to it (tick 1 for what
    /// runs before the first sync point hit); none after the last tick, or without a recording.
    pub(crate) fn tick(&self, cycle: u64) -> Option<u64> {
        self.cycles.iter().position(|&end| cycle <= end).map(|i| i as u64 + 1)
    }

    pub(crate) fn text(&self, cycle: u64) -> String {
        if self.cycles.is_empty() {
            return format!("at cycle {cycle} (rrc record gives ticks)");
        }
        match self.cycles.iter().position(|&end| cycle <= end) {
            Some(0) => "before tick 1".into(),
            Some(i) => format!("in the pass to tick {}", i + 1),
            None => "after the last tick".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_s_findings_replace_its_last_and_a_verdict_stays() {
        let found = |at: &str| Suspect { at: at.into(), kind: "past".into(), what: "x".into(), ..Suspect::default() };
        let mut s = Suspects::default();
        s.found("one", "r1", vec![found("GAME.EXE+00100"), found("GAME.EXE+00200")]);
        s.found("two", "r2", vec![found("GAME.EXE+00100")]);
        assert_eq!(s.suspects[0].sessions, ["one", "two"]);
        s.suspects[1].verdict = "not a bug: its table is longer".into();
        s.found("one", "r3", vec![]);
        assert_eq!(s.suspects[0].sessions, ["two"], "another session still finds it");
        assert_eq!((s.suspects.len(), s.suspects[1].sessions.len()), (2, 0), "judged, it stays");
        s.found("two", "r2", vec![]);
        assert_eq!(s.suspects.len(), 1, "not judged and found by none, it goes");
        assert_eq!(
            s.ran,
            [Ran { session: "one".into(), run: "r3".into() }, Ran { session: "two".into(), run: "r2".into() }]
        );
    }

    #[test]
    fn the_register_is_checked() {
        let quirk = |name: &str, at: &str, what: &str, fix: &str| Quirk {
            name: name.into(),
            at: at.into(),
            what: what.into(),
            fix: fix.into(),
            provenance: "human".into(),
            ..Quirk::default()
        };
        let register = Quirks {
            quirks: vec![
                quirk("overrun", "GAME.EXE+00100", "reads past the table", "reads 0"),
                quirk("overrun", "GAME.EXE+00200", "again", "doesn't"),
                quirk("bad name", "OTHER.EXE+00100", "", "none"),
                // A bug kept with no switch said: every one has both (D160).
                quirk("kept_only", "GAME.EXE+00300", "wraps at 256", " "),
            ],
        };
        let problems = register.problems("GAME.EXE");
        assert_eq!(
            problems[..4],
            [
                "quirks.toml: quirk overrun: named twice",
                "quirks.toml: quirk bad name: isn't a name",
                "quirks.toml: quirk bad name: OTHER.EXE+00100 isn't in GAME.EXE",
                "quirks.toml: quirk bad name: say what happens (what)",
            ]
        );
        assert!(
            problems[4].starts_with("quirks.toml: quirk kept_only: no fix: say what its switch does"),
            "{problems:?}"
        );
        assert_eq!(problems.len(), 5);
        // Where a session showed the switch is kept with the quirk, and read back.
        let shown = Quirks {
            quirks: vec![Quirk { shown: "play tick 194: enemies[1].x".into(), ..register.quirks[0].clone() }],
        };
        let text = toml::to_string_pretty(&shown).unwrap();
        assert!(text.contains("shown = \"play tick 194: enemies[1].x\""), "{text}");
        assert_eq!(toml::from_str::<Quirks>(&text).unwrap(), shown);
    }
}
