//! The quirk register (DESIGN.md §6.5, D18, D74): `quirks.toml` in the port repository lists the
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
use crate::clockwork::Clockwork;
use crate::knowledge::{Knowledge, is_name, parse_id};
use crate::places::Places;
use crate::repo::{Repo, read_toml, write_toml};
use crate::say;
use crate::session::Session;
use crate::trace::Trace;

pub const QUIRKS: &str = "quirks.toml";
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
    /// What fixing it does (its toggle).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fix: String,
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
        }
        problems
    }
}

/// `rrc quirk`: the register, or a quirk added, changed or removed.
pub(crate) fn quirk(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let options = ["--at", "--what", "--fix", "--evidence", "--by"];
    let a = Args::parse(args, &options, &["--remove"])?;
    let usage = "quirk [NAME (--at ID --what TEXT [--fix TEXT] [--evidence TEXT] [--by WHO] | --remove)]";
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
    if q.at.is_empty() || q.what.is_empty() {
        return Err(format!("a quirk needs --at (the original's code) and --what (what happens): {usage}"));
    }
    match found {
        Some(i) => register.quirks[i] = q.clone(),
        None => register.quirks.push(q.clone()),
    }
    register.quirks.sort_by(|a, b| a.name.cmp(&b.name));
    if let Some(problem) = register.problems(&repo.module()).into_iter().next() {
        return Err(problem);
    }
    register.save(repo)?;
    say("quirk", format!("{} at {}{}", q.name, q.at, if found.is_some() { " (changed)" } else { "" }));
    say_sites(repo, &q.name);
    Ok(ExitCode::SUCCESS)
}

fn list(repo: &Repo, register: &Quirks) -> Result<ExitCode, String> {
    if register.quirks.is_empty() {
        say("quirks", format!("none ({QUIRKS}; rrc suspects SESSION finds candidates)"));
    }
    for q in &register.quirks {
        say("quirk", format!("{} at {}: {}", q.name, q.at, q.what));
        say_sites(repo, &q.name);
    }
    Ok(ExitCode::SUCCESS)
}

/// Where the port's code names quirk `name`, or that it doesn't yet.
fn say_sites(repo: &Repo, name: &str) {
    let sites = crate::sources::naming(&repo.root, name, 3);
    if sites.is_empty() {
        say("  port", format!("no code names it yet: branch on rrc_runtime::keeps({name:?})"));
    } else {
        let at: Vec<String> = sites.iter().map(|(file, line)| format!("{file}:{line}")).collect();
        say("  port", at.join(", "));
    }
}

/// `rrc suspects SESSION`: quirk candidates in the session's run (DESIGN.md §6.5): the original's
/// instructions that reach past named data's ends, and its reads of memory nothing wrote.
pub(crate) fn suspects(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--lines"], &[])?;
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
    let (mut clockwork, _) = Clockwork::session(repo, &session)?;
    clockwork.ask("bounds", json!({ "objects": objects }))?;
    clockwork.ask("uninit", json!({}))?;
    clockwork.ask("run", json!({ "ms": (session.seconds * 1000.0) as u64 }))?;
    let bounds = clockwork.ask("bounds_end", Value::Null)?;
    let uninit = clockwork.ask("uninit_end", Value::Null)?;

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
    fn the_register_is_checked() {
        let quirk = |name: &str, at: &str, what: &str| Quirk {
            name: name.into(),
            at: at.into(),
            what: what.into(),
            provenance: "human".into(),
            ..Quirk::default()
        };
        let register = Quirks {
            quirks: vec![
                quirk("overrun", "GAME.EXE+00100", "reads past the table"),
                quirk("overrun", "GAME.EXE+00200", "again"),
                quirk("bad name", "OTHER.EXE+00100", ""),
            ],
        };
        assert_eq!(
            register.problems("GAME.EXE"),
            [
                "quirks.toml: quirk overrun: named twice",
                "quirks.toml: quirk bad name: isn't a name",
                "quirks.toml: quirk bad name: OTHER.EXE+00100 isn't in GAME.EXE",
                "quirks.toml: quirk bad name: say what happens (what)",
            ]
        );
    }
}
