//! `rrc import recreate DIR`: Recreate's knowledge files (`games/<game>/knowledge/*.tsv` there) as
//! this port's. Each item keeps its provenance, prefixed `recreate:`; nothing replaces a decision
//! the port has, and a name the port uses for another item is left out.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::ExitCode;

use ids::Id;

use crate::args::Args;
use crate::knowledge::{Datum, Decision, Field, Knowledge, Struct, is_name, parse_id};
use crate::repo::Repo;
use crate::say;

pub(crate) fn import(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--without"], &[])?;
    let usage = "import recreate DIR [--without PROVENANCE,...]";
    let [source, dir] = a.positionals(2, 2, usage)? else { unreachable!() };
    if source != "recreate" {
        return Err(format!("usage: rrc {usage}"));
    }
    if !repo.settings.port.imports {
        return Err("this port takes no imports ([port] imports = false in rrc.toml): an honest pilot's knowledge \
                    comes from its own analysis (D46)"
            .into());
    }
    let without: BTreeSet<&str> = a.value("--without").map(|w| w.split(',').collect()).unwrap_or_default();
    let mut k = Knowledge::load(&repo.knowledge())?;
    let report = from_recreate(&mut k, Path::new(dir), &repo.module(), &without)?;
    k.save(&repo.knowledge())?;
    say("imported", format!("{} functions, {} data, {} types", report.functions, report.data, report.types));
    if report.left_out > 0 {
        say("left out", format!("{} by provenance (--without)", report.left_out));
    }
    for conflict in report.conflicts.iter().take(10) {
        say("kept", conflict);
    }
    if report.conflicts.len() > 10 {
        say("kept", format!("... {} more", report.conflicts.len() - 10));
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(Debug, Default)]
pub struct Report {
    pub functions: usize,
    pub data: usize,
    pub types: usize,
    /// Items whose provenance was excluded.
    pub left_out: usize,
    /// Items not taken because the port decided otherwise.
    pub conflicts: Vec<String>,
}

/// Adds the knowledge in Recreate's `functions.tsv`, `globals.tsv` and `types.tsv` in `dir` to `k`.
pub fn from_recreate(k: &mut Knowledge, dir: &Path, module: &str, without: &BTreeSet<&str>) -> Result<Report, String> {
    let mut report = Report::default();
    let table = |file: &str| -> Result<Vec<Vec<String>>, String> {
        let path = dir.join(file);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .map(|l| l.split('\t').map(str::to_owned).collect())
            .collect())
    };
    let column = |row: &[String], i: usize| row.get(i).cloned().unwrap_or_default();
    // id, name, provenance, confidence, evidence
    for row in table("functions.tsv")? {
        let id = parse_id(&row[0])?;
        let Some(decision) =
            decision(&column(&row, 1), &column(&row, 2), &column(&row, 3), &column(&row, 4), without, &mut report)?
        else {
            continue;
        };
        if let Some(conflict) = conflict(k, &id, &decision.name) {
            report.conflicts.push(conflict);
            continue;
        }
        k.functions.insert(id, decision);
        report.functions += 1;
    }
    // offset (hex), name, size, type, provenance, confidence
    for row in table("globals.tsv")? {
        let offset = u32::from_str_radix(&row[0], 16).map_err(|_| format!("globals.tsv: bad offset {:?}", row[0]))?;
        let id = Id::new(module, None, offset).map_err(|e| e.to_string())?;
        let Some(decision) = decision(&column(&row, 1), &column(&row, 4), &column(&row, 5), "", without, &mut report)?
        else {
            continue;
        };
        if let Some(conflict) = conflict(k, &id, &decision.name) {
            report.conflicts.push(conflict);
            continue;
        }
        let size = column(&row, 2).parse().ok().filter(|&n: &u32| n > 0);
        k.data.insert(id, Datum { decision, size, ty: column(&row, 3) });
        report.data += 1;
    }
    // type, kind, size, field offset, field size, field name, field type, provenance: a line per field
    let types = dir.join("types.tsv");
    if types.is_file() {
        for row in table("types.tsv")? {
            let (name, kind) = (column(&row, 0), column(&row, 1));
            if kind != "struct" || without.contains(column(&row, 7).as_str()) {
                continue;
            }
            let number =
                |i: usize| column(&row, i).parse::<u32>().map_err(|_| format!("types.tsv: {name}: bad number"));
            let field = Field { offset: number(3)?, size: number(4)?, name: column(&row, 5), ty: column(&row, 6) };
            match k.types.structs.iter_mut().find(|s| s.name == name) {
                Some(s) => s.fields.push(field),
                None => {
                    let (size, provenance) = (number(2)?, format!("recreate:{}", column(&row, 7)));
                    k.types.structs.push(Struct { name, size, fields: vec![field], provenance });
                    report.types += 1;
                }
            }
        }
    }
    Ok(report)
}

/// The decision a Recreate row makes, unless its provenance is left out.
fn decision(
    name: &str,
    provenance: &str,
    confidence: &str,
    evidence: &str,
    without: &BTreeSet<&str>,
    report: &mut Report,
) -> Result<Option<Decision>, String> {
    if without.contains(provenance) {
        report.left_out += 1;
        return Ok(None);
    }
    let confidence = match confidence {
        "" => None,
        c => Some(c.parse().map_err(|_| format!("{name}: bad confidence {c:?}"))?),
    };
    Ok(Some(Decision {
        name: name.to_owned(),
        provenance: format!("recreate:{provenance}"),
        confidence,
        evidence: evidence.to_owned(),
    }))
}

/// Why `name` for `id` can't be taken, if it can't.
fn conflict(k: &Knowledge, id: &Id, name: &str) -> Option<String> {
    if !is_name(name) {
        return Some(format!("{id}: {name:?} isn't a name"));
    }
    if let Some(d) = k.functions.get(id).or_else(|| k.data.get(id).map(|d| &d.decision)) {
        return (d.name != name).then(|| format!("{id}: the port's {} over {name}", d.name));
    }
    k.named(name).map(|other| format!("{id}: {name} names {other} in the port"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recreate_s_knowledge_comes_in_with_its_provenance() {
        let dir = std::env::temp_dir().join(format!("rrc-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("functions.tsv"),
            "# id\tname\tprovenance\tconfidence\tevidence\n\
             GAME.EXE+00100\tPlay\ttransfer:src\t0.8\t[\"GAME.EXE+00200\"]\n\
             GAME.EXE+00300\tThink\tagent:x\t\t\n\
             GAME.EXE+00400\tTaken\tagent:x\t\t\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("globals.tsv"),
            "# offset\tname\tsize\ttype\tprovenance\tconfidence\n209F0\telapsed\t2\t\thuman\t\n",
        )
        .unwrap();
        let mut k = Knowledge::default();
        let mine =
            Decision { name: "Mine".into(), provenance: "human".into(), confidence: None, evidence: String::new() };
        k.functions.insert("GAME.EXE+00400".parse().unwrap(), mine);
        let report = from_recreate(&mut k, &dir, "GAME.EXE", &BTreeSet::from(["transfer:src"])).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!((report.functions, report.data, report.left_out), (1, 1, 1));
        assert_eq!(report.conflicts, ["GAME.EXE+00400: the port's Mine over Taken"]);
        let think = &k.functions[&"GAME.EXE+00300".parse().unwrap()];
        assert_eq!(think.provenance, "recreate:agent:x");
        let elapsed = &k.data[&"GAME.EXE+209F0".parse().unwrap()];
        assert_eq!((elapsed.decision.name.as_str(), elapsed.size), ("elapsed", Some(2)));
    }
}
