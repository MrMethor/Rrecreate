//! `rrc calls SESSION`: how often the session calls each function, grouped by count. Functions that
//! one loop calls once a pass are called equally often, so a large group is a loop, and a function
//! in it that waits for the timer is where a tick ends: a sync point (DESIGN.md §10, step 3).

use std::collections::BTreeMap;
use std::process::{Command, ExitCode};

use ids::Id;

use crate::args::Args;
use crate::clockwork;
use crate::knowledge::Knowledge;
use crate::repo::Repo;
use crate::say;
use crate::session::Session;

pub(crate) fn calls(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--min"], &[])?;
    let [name] = a.positionals(1, 1, "calls SESSION [--min N]")? else { unreachable!() };
    let min: u64 = a.value("--min").map_or(Ok(100), str::parse).map_err(|_| "--min is a count")?;
    let session = Session::load(repo, name)?;
    let counts = count_calls(repo, &session)?;
    let k = Knowledge::load(&repo.knowledge())?;
    let module = repo.module();
    let mut groups: BTreeMap<u64, Vec<u32>> = BTreeMap::new();
    for (&target, &n) in &counts {
        groups.entry(n).or_default().push(target);
    }
    let mut groups: Vec<(u64, Vec<u32>)> = groups.into_iter().filter(|(n, _)| *n >= min).collect();
    groups.sort_by_key(|(n, fs)| (std::cmp::Reverse(fs.len()), std::cmp::Reverse(*n)));
    say("called", format!("{} functions in {name}", counts.len()));
    for (n, functions) in groups.iter().take(6) {
        let names: Vec<String> = functions
            .iter()
            .map(|&offset| match Id::new(&module, None, offset) {
                Ok(id) => k.functions.get(&id).map_or_else(|| id.to_string(), |d| d.name.clone()),
                Err(_) => format!("+{offset:05X}"),
            })
            .collect();
        say(&format!("{n}x"), format!("{} functions: {}", functions.len(), names.join(", ")));
    }
    Ok(ExitCode::SUCCESS)
}

/// Each called function's image offset with how often the session called it.
fn count_calls(repo: &Repo, session: &Session) -> Result<BTreeMap<u32, u64>, String> {
    let coverage = repo.state()?.join("coverage.json");
    let original = &repo.settings.original;
    let output = Command::new(clockwork::find(&repo.local)?)
        .arg("run")
        .arg(repo.game_dir()?)
        .arg(&original.program)
        .args(&original.options)
        .args(session.options(repo))
        .args(["--seconds", &session.seconds.to_string(), "--coverage"])
        .arg(&coverage)
        .output()
        .map_err(|e| format!("clockwork run: {e}"))?;
    if !coverage.is_file() {
        return Err(format!("clockwork run gave no coverage: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    let text = std::fs::read_to_string(&coverage).map_err(|e| format!("{}: {e}", coverage.display()))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("the coverage: {e}"))?;
    let mut counts = BTreeMap::new();
    for call in json["calls"].as_array().into_iter().flatten() {
        // [call site, target (-1: outside the image), count]
        if let (Some(target), Some(n)) = (call[1].as_i64(), call[2].as_u64())
            && let Ok(target) = u32::try_from(target)
        {
            *counts.entry(target).or_default() += n;
        }
    }
    Ok(counts)
}
