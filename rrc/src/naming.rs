//! The knowledge, item by item: `rrc check`, `rrc name`, `rrc data`, `rrc show`.

use std::collections::BTreeMap;
use std::process::ExitCode;

use ids::Id;

use crate::analysis::Analysis;
use crate::args::Args;
use crate::cache::Cache;
use crate::clockwork::Clockwork;
use crate::knowledge::{Datum, Decision, Knowledge, is_name, parse_id};
use crate::repo::Repo;
use crate::say;

pub(crate) fn check(repo: &Repo) -> Result<ExitCode, String> {
    let k = Knowledge::load(&repo.knowledge())?;
    let image_size = image_size(repo);
    let ticks = &k.ticks;
    say(
        "knowledge",
        format!(
            "{} functions, {} data, {} types, {} sync points, {} inputs",
            k.functions.len(),
            k.data.len(),
            k.types.structs.len() + k.types.enums.len() + k.types.pointers.len() + k.types.bits.len(),
            ticks.syncs.len(),
            ticks.inputs.len()
        ),
    );
    match &image_size {
        Ok(size) => say("image", format!("{size} bytes")),
        Err(e) => say("image", format!("unknown, so not checked against it: {e}")),
    }
    let objects = crate::places::le_object_sizes(repo);
    if let Some(objects) = &objects {
        let sizes: Vec<String> = objects.iter().enumerate().map(|(i, size)| format!("#{} {size}", i + 1)).collect();
        say("objects", format!("{} bytes", sizes.join(", ")));
    }
    let mut problems = k.problems(&repo.module(), image_size.ok(), objects.as_deref());
    let mapping = crate::mapping::Mapping::load(repo)?;
    match mapping.resolve(&k).and_then(|mapped| Ok((mapped, mapping.ignored(&k)?))) {
        Ok((mapped, _)) if !mapped.is_empty() => say("mapping", format!("{} fields mapped", mapped.len())),
        Ok(_) => {}
        Err(e) => problems.push(e),
    }
    for problem in &problems {
        say("problem", problem);
    }
    Ok(if problems.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

/// The size of the program's image, from Clockwork loading it (a 16-bit game a launcher starts: where
/// the first session's run loads it).
fn image_size(repo: &Repo) -> Result<u32, String> {
    if crate::places::launched_16bit(repo) {
        let first = crate::session::names(repo)?.into_iter().next();
        let first = first.ok_or("the launcher's game is loaded by a session's run: no session yet")?;
        let session = crate::session::Session::load(repo, &first)?;
        return Ok(crate::places::placement(repo, &session)?.image.1);
    }
    let (_, loaded) = Clockwork::original(repo, &[])?;
    loaded
        .get("image_size")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as u32)
        .ok_or("Clockwork gave no image size".into())
}

/// Who decides: `--by`, else `RRC_BY`, else `agent`.
fn provenance(a: &Args) -> String {
    a.value("--by").map(str::to_owned).or_else(|| std::env::var("RRC_BY").ok()).unwrap_or_else(|| "agent".into())
}

pub(crate) fn decision(a: &Args, name: &str) -> Result<Decision, String> {
    if !is_name(name) {
        return Err(format!("{name:?} isn't a name (letters, digits, _, @ and $, not starting with a digit)"));
    }
    let confidence = match a.value("--confidence") {
        Some(c) => Some(c.parse::<f64>().ok().filter(|c| (0.0..=1.0).contains(c)).ok_or("--confidence is 0 to 1")?),
        None => None,
    };
    Ok(Decision {
        name: name.to_owned(),
        provenance: provenance(a),
        confidence,
        evidence: a.value("--evidence").unwrap_or_default().to_owned(),
    })
}

/// Fails if `name` names an item other than `id`.
fn unique(k: &Knowledge, id: &Id, name: &str) -> Result<(), String> {
    match k.named(name) {
        Some(other) if other != id => Err(format!("{name} names {other} already")),
        _ => Ok(()),
    }
}

pub(crate) const DECISION_OPTIONS: [&str; 3] = ["--by", "--confidence", "--evidence"];

pub(crate) fn name(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &DECISION_OPTIONS, &[])?;
    let [id, name] = a.positionals(2, 2, "name ID NAME [--evidence TEXT] [--confidence X] [--by WHO]")? else {
        unreachable!()
    };
    let id = parse_id(id)?;
    let _lock = crate::knowledge::EditLock::take(&repo.state()?)?;
    let mut k = Knowledge::load(&repo.knowledge())?;
    if k.data.contains_key(&id) {
        return Err(format!("{id} is data: rrc data"));
    }
    unique(&k, &id, name)?;
    let old = k.functions.insert(id.clone(), decision(&a, name)?);
    k.save(&repo.knowledge())?;
    say("function", format!("{id} {name}{}", old.map(|d| format!(" (was {})", d.name)).unwrap_or_default()));
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn data(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let options = [DECISION_OPTIONS.as_slice(), &["--size", "--type"]].concat();
    let a = Args::parse(args, &options, &[])?;
    let usage = "data ID NAME [--size N] [--type T] [--evidence TEXT] [--confidence X] [--by WHO]";
    let [id, name] = a.positionals(2, 2, usage)? else { unreachable!() };
    let id = parse_id(id)?;
    let _lock = crate::knowledge::EditLock::take(&repo.state()?)?;
    let mut k = Knowledge::load(&repo.knowledge())?;
    if k.functions.contains_key(&id) {
        return Err(format!("{id} is a function: rrc name"));
    }
    unique(&k, &id, name)?;
    let size = match a.value("--size") {
        Some(n) => Some(n.parse::<u32>().ok().filter(|&n| n > 0).ok_or("--size is a number of bytes")?),
        None => k.data.get(&id).and_then(|d| d.size),
    };
    let ty = a.value("--type").map(str::to_owned).or_else(|| k.data.get(&id).map(|d| d.ty.clone())).unwrap_or_default();
    let old = k.data.insert(id.clone(), Datum { decision: decision(&a, name)?, size, ty });
    k.save(&repo.knowledge())?;
    say("data", format!("{id} {name}{}", old.map(|d| format!(" (was {})", d.decision.name)).unwrap_or_default()));
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn show(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--session", "--tick"], &["--asm", "--c", "--listing", "--all"])?;
    let usage = "show ID|NAME|SEG:OFF [--asm] [--c] [--listing] [--all] [--session NAME [--tick N]]";
    let [what] = a.positionals(1, 1, usage)? else { unreachable!() };
    let k = Knowledge::load(&repo.knowledge())?;
    let analysis = Analysis::load(repo)?;
    let id = resolve(repo, &k, analysis.as_ref(), what)?;
    let about = |d: &Decision| {
        let confidence = d.confidence.map(|c| format!(", {c}")).unwrap_or_default();
        format!("{id} {} ({}{confidence})", d.name, d.provenance)
    };
    if let Some(d) = k.functions.get(&id) {
        say("function", about(d));
        if !d.evidence.is_empty() {
            say("evidence", &d.evidence);
        }
    } else if let Some(datum) = k.data.get(&id) {
        say("data", about(&datum.decision));
        let size = datum.size.map(|s| format!("{s} bytes")).unwrap_or_else(|| "size unknown".into());
        say("layout", if datum.ty.is_empty() { size } else { format!("{size}, {}", datum.ty) });
        if !datum.decision.evidence.is_empty() {
            say("evidence", &datum.decision.evidence);
        }
    } else {
        say("unnamed", &id);
        if let Some((function, offset)) = analysis.as_ref().and_then(|a| a.containing(&id)).filter(|f| *f.0 != id) {
            let name =
                k.functions.get(function).map_or_else(|| function.to_string(), |d| format!("{} ({function})", d.name));
            say("inside", format!("{name}, {offset:#X} bytes in"));
        }
    }
    let text = id.to_string();
    for s in k.ticks.syncs.iter().filter(|s| s.at == text) {
        say("sync", format!("{}{}", s.name, note(&s.note)));
    }
    for i in k.ticks.inputs.iter().filter(|i| i.at == text) {
        say("input", format!("{}, {} bytes{}", i.name, i.size, note(&i.note)));
    }
    if let Some(analysis) = &analysis {
        show_analysis(&k, analysis, &id, a.has("--asm"), a.has("--c"));
        let size = k.data.get(&id).and_then(|d| d.size).unwrap_or(1);
        let shown = if a.has("--all") { usize::MAX } else { REFS_SHOWN };
        let referred = show_refs(&k, analysis, &id, size, shown);
        let in_code = analysis.functions.contains_key(&id) || analysis.containing(&id).is_some();
        // Code a 16-bit program makes at run time is past its file image, in the session's memory only.
        let file_size = file_image_size(repo, analysis)?;
        let made = analysis.objects.is_empty()
            && id.module() == repo.module()
            && !id.is_memory()
            && u64::from(id.offset()) >= file_size;
        let image = match a.value("--session") {
            Some(session) if made => Some(session_image(repo, session, a.value("--tick"))?),
            Some(_) => return Err(format!("{id} is in the program's file: --session is for code made at run time")),
            None => None,
        };
        if made && image.is_none() && (a.has("--asm") || a.has("--listing")) {
            return Err(format!(
                "{id} is past the program's file image ({file_size} bytes): code made at run time; --session NAME \
                 [--tick N] disassembles it from the session's memory"
            ));
        }
        if a.has("--listing") {
            print!("{}", clockwork_listing(repo, &k, analysis, &id, image.as_deref())?);
        } else if !in_code && !referred && !k.data.contains_key(&id) && !id.is_memory() {
            if a.has("--asm") {
                print!("{}", clockwork_listing(repo, &k, analysis, &id, image.as_deref())?);
            } else if made {
                say(
                    "code",
                    format!(
                        "past the program's file image ({file_size} bytes): made at run time (code a game generates, or \
                         data); --asm --session NAME [--tick N] disassembles it from the session's memory"
                    ),
                );
            } else {
                say(
                    "code",
                    "Ghidra has no function here; if it is code (an interrupt handler: rrc inputs names them), --asm \
                     disassembles it with Clockwork, and once named (rrc name) rrc analyze makes it a function",
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The instructions that refer to the `size` bytes at `id`, by kind and function (Ghidra's data
/// references), `shown` functions a kind; whether there are any.
fn show_refs(k: &Knowledge, analysis: &Analysis, id: &Id, size: u32, shown: usize) -> bool {
    let range = id.offset()..id.offset() + size;
    let mut by_kind: BTreeMap<&str, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    for (from, to, kind) in &analysis.refs {
        if to.module() != id.module() || to.part() != id.part() || !range.contains(&to.offset()) {
            continue;
        }
        let function = match analysis.containing(from) {
            Some((entry, _)) => k.functions.get(entry).map_or_else(|| entry.to_string(), |d| d.name.clone()),
            None => "no function".to_owned(),
        };
        let at = by_kind.entry(kind.as_str()).or_default().entry(function).or_default();
        let instruction = format!("+{:05X}", from.offset());
        if !at.contains(&instruction) {
            at.push(instruction);
        }
    }
    for (kind, label) in [("write", "written"), ("read", "read"), ("data", "address")] {
        let Some(functions) = by_kind.get(kind) else { continue };
        let mut list: Vec<String> =
            functions.iter().take(shown).map(|(f, at)| format!("{f} ({})", at.join(", "))).collect();
        if functions.len() > shown {
            list.push(format!("and {} more functions (--all)", functions.len() - shown));
        }
        say(label, list.join("; "));
    }
    !by_kind.is_empty()
}

/// Functions listed per kind of reference.
const REFS_SHOWN: usize = 10;

/// The analyzed program: an LE program's ELF file as `clockwork unpack` wrote it; a 16-bit one's load
/// image (without its MZ header).
fn file_image(repo: &Repo, analysis: &Analysis) -> Result<Vec<u8>, String> {
    let index = crate::analysis::Index::load(repo)?.ok_or("no analysis: rrc analyze")?;
    let program = Cache::open(&repo.local)?.get(&index.program)?;
    if !analysis.objects.is_empty() {
        return Ok(program);
    }
    let header = program.get(8..10).map_or(0, |w| usize::from(u16::from_le_bytes([w[0], w[1]])) * 16);
    Ok(program.get(header..).ok_or("the analyzed program is shorter than its header")?.to_vec())
}

fn file_image_size(repo: &Repo, analysis: &Analysis) -> Result<u64, String> {
    Ok(file_image(repo, analysis)?.len() as u64)
}

/// The 16-bit program's image in a session's memory, by tick `tick` or at its last snapshot (with what
/// it made at run time), written for `clockwork listing`.
fn session_image(repo: &Repo, name: &str, tick: Option<&str>) -> Result<std::path::PathBuf, String> {
    let session = crate::session::Session::load(repo, name)?;
    let syncs = crate::trace::sync_points(&Knowledge::load(&repo.knowledge())?)?;
    let length_ms = (session.seconds * 1000.0) as u64;
    let (mut clockwork, where_) = match tick {
        Some(n) => {
            let n: u64 = n.parse().map_err(|_| "--tick is a number")?;
            if syncs.is_empty() {
                return Err("--tick needs sync points (knowledge/ticks.toml)".into());
            }
            let (mut clockwork, _, from) = crate::watch::open(repo, &session, &syncs, n)?;
            crate::watch::to_tick(&mut clockwork, n + 1, from.as_ref(), length_ms)?;
            (clockwork, format!("by tick {n}"))
        }
        None => match crate::snapshots::before(repo, &session, &syncs, u64::MAX)? {
            Some(taken) => {
                let (clockwork, _) = crate::snapshots::load(repo, &session, &taken)?;
                (clockwork, format!("at tick {} ({:.1} s)", taken.tick, taken.ms as f64 / 1000.0))
            }
            None => {
                let (mut clockwork, _, _) = crate::trace::begin(repo, &session, &syncs)?;
                clockwork.ask("run", serde_json::json!({ "until_ms": length_ms }))?;
                (clockwork, "at the session's end".to_owned())
            }
        },
    };
    let dir = repo.state()?.join("images").join(name);
    clockwork.ask("image", serde_json::json!({ "dir": crate::path_text(&dir) }))?;
    say("memory", format!("{name}'s, {where_}"));
    Ok(dir.join("image.bin"))
}

/// Clockwork's disassembly from `id`, following its control flow and the jump tables it finds
/// (`clockwork listing`), from the analyzed program or `image` (a session's memory); lines and
/// targets as IDs, named ones by name.
fn clockwork_listing(
    repo: &Repo,
    k: &Knowledge,
    analysis: &Analysis,
    id: &Id,
    image: Option<&std::path::Path>,
) -> Result<String, String> {
    let flat = !analysis.objects.is_empty();
    let path = match image {
        Some(image) => image.to_path_buf(),
        None => {
            let image = file_image(repo, analysis)?;
            let path = repo.state()?.join("ghidra").join(if flat { "image.elf" } else { "image.bin" });
            if std::fs::read(&path).ok().as_deref() != Some(&image[..]) {
                std::fs::create_dir_all(path.parent().expect("a folder")).map_err(|e| e.to_string())?;
                std::fs::write(&path, &image).map_err(|e| format!("{}: {e}", path.display()))?;
            }
            path
        }
    };
    let layout = analysis.layout();
    let entry = if flat {
        let linear = layout.linear(id).ok_or_else(|| format!("{id} isn't in the program's objects"))?;
        vec!["--entry".to_owned(), format!("{linear:X}")]
    } else {
        let segment = analysis.segment_of(id.offset()).unwrap_or(id.offset() >> 4);
        vec!["--entry".to_owned(), format!("{:X}", id.offset()), "--segment".to_owned(), format!("{segment:X}")]
    };
    let output = std::process::Command::new(crate::clockwork::find(&repo.local)?)
        .arg("listing")
        .arg(&path)
        .args(&entry)
        .output()
        .map_err(|e| format!("clockwork listing: {e}"))?;
    if !output.status.success() {
        return Err(format!("clockwork listing: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let module = id.module();
    // An address as Clockwork lists it (`+OFFSET` in the image, `@LINEAR` in a flat program) as an ID.
    let id_of = |sign: char, hex: &str| -> Option<Id> {
        match sign {
            '+' => parse_id(&format!("{module}+{hex}")).ok(),
            _ => layout.id_at(module, u64::from_str_radix(hex, 16).ok()?),
        }
    };
    let named = |id: Id| -> String {
        let name =
            k.functions.get(&id).map(|d| d.name.clone()).or_else(|| k.data.get(&id).map(|d| d.decision.name.clone()));
        name.unwrap_or_else(|| id.to_string())
    };
    let mut out = format!("code      Clockwork's disassembly from {id}\n");
    for line in text.lines().skip(1) {
        // `+OFFSET  instruction` or `@LINEAR  instruction`, with targets the same way.
        let (at, rest) = line.split_once("  ").unwrap_or(("", line));
        let at = match at.chars().next() {
            Some(sign @ ('+' | '@')) => id_of(sign, &at[1..]).map_or_else(|| at.to_owned(), |id| id.to_string()),
            _ => at.to_owned(),
        };
        let mut rest = rest.to_owned();
        let mut from = 0;
        let sign = if flat { '@' } else { '+' };
        while let Some(sign_at) = rest[from..].find(sign).map(|i| from + i) {
            let digits = rest[sign_at + 1..].chars().take_while(char::is_ascii_hexdigit).count();
            let id = (digits >= 5).then(|| id_of(sign, &rest[sign_at + 1..sign_at + 1 + digits])).flatten();
            match id {
                Some(id) => {
                    let name = named(id);
                    rest.replace_range(sign_at..sign_at + 1 + digits, &name);
                    from = sign_at + name.len();
                }
                None => from = sign_at + 1,
            }
        }
        if flat {
            rest = with_object_ids(&rest, |linear| {
                layout.id_at(module, linear).filter(|id| id.part().is_some()).map(named)
            });
        }
        out.push_str(&format!("{at}  {rest}\n"));
    }
    Ok(out)
}

/// `text` (an instruction as Clockwork formats it) with each number (`54BC04h`) that `name` names (an
/// address in the program's objects) replaced by the name.
fn with_object_ids(text: &str, name: impl Fn(u64) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(|c: char| c.is_ascii_hexdigit()) {
        let digits = rest[start..].chars().take_while(char::is_ascii_hexdigit).count();
        let after = &rest[start + digits..];
        let word = start == 0 || !rest[..start].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
        let named = (word && after.starts_with('h'))
            .then(|| u64::from_str_radix(&rest[start..start + digits], 16).ok().and_then(&name))
            .flatten();
        match named {
            Some(named) => {
                out.push_str(&rest[..start]);
                out.push_str(&named);
                rest = &after[1..];
            }
            None => {
                out.push_str(&rest[..start + digits]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The ID `what` stands for: an ID, a name, or Ghidra's `SEG:OFF`.
pub(crate) fn resolve(repo: &Repo, k: &Knowledge, analysis: Option<&Analysis>, what: &str) -> Result<Id, String> {
    if let Ok(id) = parse_id(what) {
        return Ok(id);
    }
    let ghidra = || analysis.and_then(|a| a.id_of(&repo.module(), what));
    k.named(what).cloned().or_else(ghidra).ok_or_else(|| format!("nothing is named {what:?}"))
}

/// What Ghidra found about `id`: the function there, its calls and callers, its code.
fn show_analysis(k: &Knowledge, analysis: &Analysis, id: &Id, asm: bool, c: bool) {
    let Some(f) = analysis.functions.get(id) else { return };
    let library = f.library.as_ref().map(|(name, pack)| format!(", {name} in {pack}")).unwrap_or_default();
    say("ghidra", format!("{}, {} bytes{library}", f.name, f.size));
    let named = |id: &Id| k.functions.get(id).map_or_else(|| id.to_string(), |d| d.name.clone());
    let mut calls: Vec<String> = analysis.calls.iter().filter(|c| &c.0 == id).map(|c| named(&c.2)).collect();
    let mut callers: Vec<String> = analysis.calls.iter().filter(|c| &c.2 == id).map(|c| named(&c.0)).collect();
    for (label, list) in [("calls", &mut calls), ("callers", &mut callers)] {
        list.sort();
        list.dedup();
        if !list.is_empty() {
            say(label, list.join(", "));
        }
    }
    if let Some((disassembly, decompiled)) = analysis.code.get(id) {
        if asm {
            let lines = analysis.with_ids(&analysis.with_names(disassembly, k, id.module()), id.module());
            print!("{}", annotated(&lines, k, analysis, id.module()));
        }
        if c {
            let decompiled = decompiled.as_deref().unwrap_or("(Ghidra didn't decompile it)");
            println!("{}", analysis.with_names(decompiled, k, id.module()));
        }
    }
}

/// Ghidra's disassembly (lines as IDs) with its branch targets (`0x1000:cce9`) as IDs or names, each
/// instruction's data references named after it, and a jump through a table pointed to `--listing`.
fn annotated(lines: &str, k: &Knowledge, analysis: &Analysis, module: &str) -> String {
    // A function's or data item's name, `name[+N]` for a byte inside an item, else the ID.
    let name = |id: &Id| {
        if let Some(d) = k.functions.get(id) {
            return d.name.clone();
        }
        let inside = k.data.range(..=id.clone()).next_back().filter(|(start, _)| start.module() == id.module());
        match inside {
            Some((start, d)) if start == id => d.decision.name.clone(),
            Some((start, d)) if d.size.is_some_and(|size| id.offset() - start.offset() < size) => {
                format!("{}[+{}]", d.decision.name, id.offset() - start.offset())
            }
            _ => id.to_string(),
        }
    };
    let mut out = String::with_capacity(lines.len());
    for line in lines.lines() {
        let (at, instruction) = line.split_once("  ").unwrap_or((line, ""));
        // `0xSSSS:OOOO` targets, as Ghidra prints near and far branches.
        let mut text = String::with_capacity(instruction.len());
        let mut rest = instruction;
        while let Some(start) = rest.find("0x") {
            let token: String = rest[start + 2..].chars().take_while(|c| c.is_ascii_hexdigit() || *c == ':').collect();
            text.push_str(&rest[..start]);
            // Real mode's `SSSS:OOOO`, or a flat program's address in one of its objects.
            let id = analysis.id_of(module, &token).filter(|id| token.contains(':') || id.part().is_some());
            match id {
                Some(target) => text.push_str(&name(&target)),
                None => text.push_str(&rest[start..start + 2 + token.len()]),
            }
            rest = &rest[start + 2 + token.len()..];
        }
        text.push_str(rest);
        let mut notes: Vec<String> = Vec::new();
        if let Ok(from) = parse_id(at) {
            for (_, to, _) in analysis.refs.iter().filter(|(f, _, _)| *f == from) {
                let note = name(to);
                if !notes.contains(&note) {
                    notes.push(note);
                }
            }
        }
        if instruction.starts_with("JMP") && instruction.contains("CS:[") {
            notes.push("a table: --listing lists its cases".into());
        }
        let notes = if notes.is_empty() { String::new() } else { format!("  ; {}", notes.join(", ")) };
        out.push_str(&format!("{at}  {text}{notes}\n"));
    }
    out
}

fn note(text: &str) -> String {
    if text.is_empty() { String::new() } else { format!(": {text}") }
}
