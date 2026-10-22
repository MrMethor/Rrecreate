//! `rrc analyze` (DESIGN.md §5, §9): the program as Clockwork unpacks it, analyzed by Ghidra headless,
//! exported (`ghidra/RecExport.java`) into the cache. Library functions that a signature pack
//! (`sigs/`) recognizes by their Function ID hashes are named in the knowledge (`library:<pack>`),
//! unless someone named them already. What analysis left is indexed in `.rrc/analysis.toml`.
//!
//! Functions the knowledge names that Ghidra didn't find (an interrupt handler only a vector
//! reaches, code called through a pointer) are added to it, so they get calls, references and
//! decompiled code too: Ghidra's project of the analysis is kept in the cache, and opened again
//! (read only, without analysing it again) to create them and export just their records, which are
//! read after the analysis's own.
//!
//! The scripts and packs are built into `rrc`, so a port repository needs no Rrecreate checkout.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, ExitCode};

use ids::Id;
use serde_json::Value;

use crate::args::Args;
use crate::cache::{Cache, sha256, sha256_file};
use crate::clockwork;
use crate::knowledge::{Decision, Knowledge};
use crate::repo::{Repo, read_toml, write_toml};
use crate::{say, tools};

const EXPORT_SCRIPT: &str = include_str!("../../ghidra/RecExport.java");
const PACKS: &[&str] = &[include_str!("../../sigs/borland-c++-1991-medium.json")];
const PROCESSOR: &str = "x86:LE:16:Real Mode";
/// A flat program under a DOS extender (its LE objects as Clockwork placed them, an ELF file).
const FLAT_PROCESSOR: &str = "x86:LE:32:default";
/// Short functions hash alike too easily: only this many code units or more are matched.
const MIN_UNITS: u64 = 6;
const INDEX: &str = "analysis.toml";

/// What `rrc analyze` left.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Index {
    /// The game's program it analyzed (its hash): a new version needs a new analysis.
    pub original: String,
    /// What Ghidra imported, in the cache: the program as Clockwork unpacked it, or as it is.
    pub program: String,
    pub unpacked: bool,
    /// Ghidra's export (JSON lines), in the cache.
    pub export: String,
    /// The records of the functions added to it (as the export's), in the cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions: Option<String>,
    /// The load image's offset of the entry point, which places Ghidra's addresses in the image.
    pub entry: u32,
    /// An LE program's objects as Clockwork placed them, in the LE's order: (linear address, size).
    /// Ghidra's addresses are these linear addresses, and IDs name the objects (`GAME.EXE#2+0041C`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<(u32, u32)>,
    pub ghidra: String,
}

impl Index {
    pub fn load(repo: &Repo) -> Result<Option<Index>, String> {
        let path = repo.root.join(crate::repo::STATE).join(INDEX);
        if path.is_file() { read_toml(&path).map(Some) } else { Ok(None) }
    }

    /// The export's records, and the additions' after them.
    fn records(&self, cache: &Cache) -> Result<String, String> {
        let mut text = String::from_utf8_lossy(&cache.get(&self.export)?).into_owned();
        if let Some(additions) = &self.additions {
            text.push_str(&String::from_utf8_lossy(&cache.get(additions)?));
        }
        Ok(text)
    }
}

pub(crate) fn analyze(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &[], &["--no-decompile", "--again"])?;
    a.positionals(0, 0, "analyze [--no-decompile] [--again]")?;
    let cache = Cache::open(&repo.local)?;
    let original = repo.game_dir()?.join(repo.module_file());
    let original_hash = sha256_file(&original)?;
    let decompile = !a.has("--no-decompile");
    let module = repo.module();
    // The same program through the same script gives the same export: this machine's cache keeps
    // it for every repository (and fresh clone) of the game.
    let key = sha256(format!("{original_hash}\n{decompile}\n{EXPORT_SCRIPT}").as_bytes());
    let again = a.has("--again");
    // Ghidra's project of the analysis under `key`, kept for adding functions to it.
    let project = cache.dir().join("ghidra").join(&key);
    let analyse = || run_ghidra(repo, &cache, &original, &original_hash, decompile, &project);
    let mut index = kept_or_run(repo, &cache, &key, again, analyse)?;
    let mut analysis = Analysis::parse(&index.records(&cache)?, &module, index.entry, &index.objects)?;
    // Then the functions the knowledge names that Ghidra didn't find, added to it.
    let mut k = Knowledge::load(&repo.knowledge())?;
    let missing: Vec<Id> = k
        .functions
        .keys()
        .filter(|id| id.module() == module && !analysis.functions.contains_key(*id))
        .cloned()
        .collect();
    if !missing.is_empty() {
        let entries: Vec<String> = missing.iter().map(ToString::to_string).collect();
        say("adding", format!("{} functions the knowledge names that Ghidra didn't find", missing.len()));
        let added = sha256(format!("{key}\n{}", entries.join("\n")).as_bytes());
        let additions = match cache.recall("additions", &added).filter(|hash| !again && cache.has(hash)) {
            Some(hash) => hash,
            None => {
                if !project.join(format!("{}.gpr", repo.settings.game.name)).is_file() {
                    say(
                        "program",
                        "Ghidra's project of the analysis isn't kept on this machine: analysing again, once",
                    );
                    index = analyse()?;
                    cache.remember("analysis", &key, &toml::to_string(&index).map_err(|e| e.to_string())?)?;
                    analysis = Analysis::parse(&index.records(&cache)?, &module, index.entry, &index.objects)?;
                }
                let layout = analysis.layout();
                let linear: Vec<u64> = missing.iter().filter_map(|id| layout.linear(id)).collect();
                let hash = add_functions(repo, &cache, &project, &linear, decompile)?;
                cache.remember("additions", &added, &hash)?;
                hash
            }
        };
        index.additions = Some(additions);
        analysis = Analysis::parse(&index.records(&cache)?, &module, index.entry, &index.objects)?;
        for id in missing.iter().filter(|id| !analysis.functions.contains_key(*id)) {
            say("missing", format!("{id}: Ghidra made no function there"));
        }
    }
    write_toml(&repo.state()?.join(INDEX), &index)?;
    say("ghidra", format!("{} functions, {} calls ({})", analysis.functions.len(), analysis.calls.len(), index.ghidra));
    let named = name_library_functions(&mut k, &analysis);
    k.save(&repo.knowledge())?;
    let library = analysis.functions.values().filter(|f| f.library.is_some()).count();
    say("library", format!("{library} functions from signature packs, {named} newly named"));
    say("export", &index.export);
    Ok(ExitCode::SUCCESS)
}

/// The analysis kept under `key` on this machine (unless `again`), else `run`'s, kept there.
fn kept_or_run(
    repo: &Repo,
    cache: &Cache,
    key: &str,
    again: bool,
    run: impl FnOnce() -> Result<Index, String>,
) -> Result<Index, String> {
    let earlier = cache.recall("analysis", key).and_then(|text| toml::from_str::<Index>(&text).ok());
    if let Some(index) = earlier.filter(|i| !again && cache.has(&i.export)) {
        say("program", format!("{}: analyzed before on this machine (--again redoes it)", repo.module()));
        return Ok(index);
    }
    let index = run()?;
    cache.remember("analysis", key, &toml::to_string(&index).map_err(|e| e.to_string())?)?;
    Ok(index)
}

/// Unpacks the program with Clockwork (if it was packed), analyzes it with Ghidra in a new project
/// at `project` (kept, for adding functions later) and stores what that gives in the cache.
fn run_ghidra(
    repo: &Repo,
    cache: &Cache,
    original: &Path,
    original_hash: &str,
    decompile: bool,
    project: &Path,
) -> Result<Index, String> {
    let (ghidra, java) = (tools::ghidra(&repo.local)?, tools::java(&repo.local)?);
    let work = repo.state()?.join("ghidra");
    std::fs::create_dir_all(work.join("scripts")).map_err(|e| format!("{}: {e}", work.display()))?;
    let module = repo.module();

    // The program as its packer leaves it in memory, if it was packed.
    let program = work.join(&module);
    let unpack = Command::new(clockwork::find(&repo.local)?)
        .arg("unpack")
        .arg(original)
        .arg("--out")
        .arg(&program)
        .output()
        .map_err(|e| format!("clockwork unpack: {e}"))?;
    let unpacked = unpack.status.success();
    if !unpacked {
        std::fs::copy(original, &program).map_err(|e| format!("{}: {e}", original.display()))?;
    }
    let bytes = std::fs::read(&program).map_err(|e| format!("{}: {e}", program.display()))?;
    // An LE program comes out of Clockwork as its objects placed, in an ELF file.
    let (entry, objects, processor) = match elf_objects(&bytes) {
        Some(objects) => (0, objects, FLAT_PROCESSOR),
        None => {
            let entry = mz_entry(&bytes).ok_or_else(|| format!("{module}: only MZ and LE programs can be analyzed"))?;
            (entry, Vec::new(), PROCESSOR)
        }
    };
    say("program", format!("{module}{}; Ghidra takes minutes", if unpacked { ", unpacked by Clockwork" } else { "" }));

    let export = work.join("export.jsonl");
    let _ = std::fs::remove_file(&export);
    let script = work.join("scripts").join("RecExport.java");
    std::fs::write(&script, EXPORT_SCRIPT).map_err(|e| format!("{}: {e}", script.display()))?;
    // Ghidra's project is in the cache, not in `.rrc`: Ghidra refuses project paths with a folder
    // starting with a dot.
    let _ = std::fs::remove_dir_all(project);
    std::fs::create_dir_all(project).map_err(|e| format!("{}: {e}", project.display()))?;
    let mut command = headless(&ghidra);
    command
        .env("JAVA_HOME", &java)
        .arg(project)
        .arg(&repo.settings.game.name)
        .args(["-import"])
        .arg(&program)
        .args(["-overwrite", "-processor", processor, "-scriptPath"])
        .arg(work.join("scripts"))
        .args(["-postScript", "RecExport.java"])
        .arg(&export);
    if decompile {
        command.arg("decompile");
    }
    let log = work.join("ghidra.log");
    let output = command.output().map_err(|e| format!("Ghidra: {e}"))?;
    std::fs::write(&log, [output.stdout.as_slice(), &output.stderr].concat()).map_err(|e| e.to_string())?;
    if !output.status.success() || !export.is_file() {
        return Err(format!("Ghidra's analysis failed (its log: {})", log.display()));
    }
    let records = std::fs::read(&export).map_err(|e| format!("{}: {e}", export.display()))?;
    let ghidra_version = Analysis::parse(&String::from_utf8_lossy(&records), &module, entry, &objects)?.ghidra;
    Ok(Index {
        original: original_hash.to_owned(),
        program: cache.put(&bytes)?,
        unpacked,
        export: cache.put(&records)?,
        additions: None,
        entry,
        objects,
        ghidra: ghidra_version,
    })
}

/// The loadable segments of an ELF executable (an LE program's objects as `clockwork unpack` writes
/// them): (address, size), in order; `None` if it isn't one.
fn elf_objects(bytes: &[u8]) -> Option<Vec<(u32, u32)>> {
    if !bytes.starts_with(b"\x7FELF") {
        return None;
    }
    let half = |at: usize| Some(usize::from(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?)));
    let word = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let (table, size, count) = (word(28)? as usize, half(42)?, half(44)?);
    (0..count).map(|i| Some((word(table + i * size + 8)?, word(table + i * size + 20)?))).collect()
}

/// Functions created at `functions` (Ghidra's linear addresses) in the analysis kept at `project`,
/// opened read only and not analysed again: their records, in the cache.
fn add_functions(
    repo: &Repo,
    cache: &Cache,
    project: &Path,
    functions: &[u64],
    decompile: bool,
) -> Result<String, String> {
    let (ghidra, java) = (tools::ghidra(&repo.local)?, tools::java(&repo.local)?);
    let work = repo.state()?.join("ghidra");
    std::fs::create_dir_all(work.join("scripts")).map_err(|e| format!("{}: {e}", work.display()))?;
    let script = work.join("scripts").join("RecExport.java");
    std::fs::write(&script, EXPORT_SCRIPT).map_err(|e| format!("{}: {e}", script.display()))?;
    let list = work.join("functions.txt");
    let text: String = functions.iter().map(|f| format!("{f}\n")).collect();
    std::fs::write(&list, text).map_err(|e| format!("{}: {e}", list.display()))?;
    let out = work.join("additions.jsonl");
    let _ = std::fs::remove_file(&out);
    say("program", "Ghidra adds them to the analysis (a minute or so)");
    let mut command = headless(&ghidra);
    command
        .env("JAVA_HOME", &java)
        .arg(project)
        .arg(&repo.settings.game.name)
        .arg("-process")
        .arg(repo.module())
        .args(["-readOnly", "-noanalysis", "-scriptPath"])
        .arg(work.join("scripts"))
        .args(["-postScript", "RecExport.java"])
        .arg(&out);
    if decompile {
        command.arg("decompile");
    }
    // Not `functions=FILE`: Windows' cmd splits arguments at `=`.
    command.arg("functions").arg(&list).arg("only");
    let log = work.join("ghidra-additions.log");
    let output = command.output().map_err(|e| format!("Ghidra: {e}"))?;
    std::fs::write(&log, [output.stdout.as_slice(), &output.stderr].concat()).map_err(|e| e.to_string())?;
    if !output.status.success() || !out.is_file() {
        return Err(format!("Ghidra couldn't add the functions (its log: {})", log.display()));
    }
    cache.put(&std::fs::read(&out).map_err(|e| format!("{}: {e}", out.display()))?)
}

fn headless(ghidra: &Path) -> Command {
    let script = tools::headless(ghidra);
    if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.arg("/c").arg(script);
        command
    } else {
        Command::new(script)
    }
}

/// The entry point's offset in an MZ program's load image (CS is relative to the load segment).
fn mz_entry(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 0x1C || !(bytes.starts_with(b"MZ") || bytes.starts_with(b"ZM")) {
        return None;
    }
    let word = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let (ip, cs) = (word(0x14), word(0x16) as i16);
    u32::try_from(i32::from(cs) * 16 + i32::from(ip)).ok()
}

/// Names the library functions the signature packs recognize that nobody named; returns how many.
fn name_library_functions(k: &mut Knowledge, analysis: &Analysis) -> usize {
    let mut named = 0;
    for (id, f) in &analysis.functions {
        let Some((name, pack)) = &f.library else { continue };
        if k.functions.contains_key(id) || k.named(name).is_some() || !crate::knowledge::is_name(name) {
            continue;
        }
        let provenance = format!("library:{pack}");
        k.functions.insert(
            id.clone(),
            Decision { name: name.clone(), provenance, confidence: Some(0.9), evidence: String::new() },
        );
        named += 1;
    }
    named
}

/// A function as Ghidra found it.
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    /// Ghidra's name (`FUN_…` when it has none).
    pub name: String,
    pub size: u32,
    /// Its code: (image offset, length) ranges.
    pub body: Vec<(u32, u32)>,
    /// The library function a signature pack recognizes: (name, pack).
    pub library: Option<(String, String)>,
}

/// Ghidra's export, with addresses as IDs.
#[derive(Debug, Default)]
pub struct Analysis {
    pub ghidra: String,
    /// The linear address (in Ghidra's address space) of the load image's start.
    pub base: u64,
    /// An LE program's objects (linear address, size): `Index::objects`.
    pub objects: Vec<(u32, u32)>,
    pub functions: BTreeMap<Id, Function>,
    /// (caller, call site, callee).
    pub calls: Vec<(Id, Id, Id)>,
    /// Data references: (instruction, the address it refers to, `read`, `write` or `data`).
    pub refs: Vec<(Id, Id, String)>,
    /// Ghidra's memory blocks (the program's segments): image offset of the start, and size.
    pub blocks: Vec<(u32, u32)>,
    /// Disassembly and decompiled code, by function.
    pub code: BTreeMap<Id, (String, Option<String>)>,
}

impl Analysis {
    /// The analysis `rrc analyze` left, if any.
    pub fn load(repo: &Repo) -> Result<Option<Analysis>, String> {
        let Some(index) = Index::load(repo)? else { return Ok(None) };
        let cache = Cache::open(&repo.local)?;
        Analysis::parse(&index.records(&cache)?, &repo.module(), index.entry, &index.objects).map(Some)
    }

    /// Reads an export of `module` whose entry point is at image offset `entry`, or, for an LE
    /// program, whose `objects` are at these linear addresses.
    pub fn parse(records: &str, module: &str, entry: u32, objects: &[(u32, u32)]) -> Result<Analysis, String> {
        let records: Vec<Value> = records
            .lines()
            .filter(|l| !l.is_empty())
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let program = records.iter().find(|r| r["record"] == "program").ok_or("the export has no program record")?;
        let entry_linear = program["entry"]["linear"].as_u64().ok_or("the export has no entry point")?;
        let base = if objects.is_empty() {
            entry_linear.checked_sub(u64::from(entry)).ok_or("the export's entry point is below its image")?
        } else {
            0
        };
        let ghidra = program["ghidra"].as_str().unwrap_or("?").to_owned();
        let mut analysis = Analysis { ghidra, base, objects: objects.to_vec(), ..Analysis::default() };
        let layout = analysis.layout();
        let id = |address: &Value| layout.id_at(module, address["linear"].as_u64()?);
        let packs = Packs::load()?;
        for r in &records {
            match r["record"].as_str() {
                Some("function") => {
                    let Some(at) = id(&r["address"]) else { continue };
                    let body: Vec<(u32, u32)> = r["body"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|range| Some((id(&range[0])?.offset(), range[1].as_u64()? as u32)))
                        .collect();
                    let function = Function {
                        name: r["name"].as_str().unwrap_or_default().to_owned(),
                        size: body.iter().map(|r| r.1).sum(),
                        body,
                        library: packs.matching(&r["fid"]),
                    };
                    analysis.functions.insert(at, function);
                }
                Some("call") => {
                    if let (Some(caller), Some(site), Some(callee)) =
                        (id(&r["caller"]), id(&r["site"]), id(&r["callee"]))
                    {
                        analysis.calls.push((caller, site, callee));
                    }
                }
                Some("ref") => {
                    if let (Some(from), Some(to)) = (id(&r["from"]), id(&r["to"])) {
                        analysis.refs.push((from, to, r["kind"].as_str().unwrap_or("data").to_owned()));
                    }
                }
                Some("block") => {
                    if let (Some(start), Some(size)) = (id(&r["start"]), r["size"].as_u64()) {
                        analysis.blocks.push((start.offset(), size as u32));
                    }
                }
                Some("code") => {
                    let Some(at) = id(&r["address"]) else { continue };
                    let asm = r["asm"].as_str().unwrap_or_default().to_owned();
                    analysis.code.insert(at, (asm, r["c"].as_str().map(str::to_owned)));
                }
                _ => {}
            }
        }
        Ok(analysis)
    }
}

/// Where Ghidra's addresses are: an image from `base`, or LE objects.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    base: u64,
    objects: Vec<(u32, u32)>,
}

impl Layout {
    /// The ID of a linear address in Ghidra's address space: in the image (or an object), else
    /// memory of the PC's (`MEM`: below a real-mode image, the vectors and the BIOS's data).
    pub fn id_at(&self, module: &str, linear: u64) -> Option<Id> {
        let linear32 = u32::try_from(linear).ok()?;
        if !self.objects.is_empty() {
            let object = self.objects.iter().position(|&(base, size)| linear32.wrapping_sub(base) < size);
            return match object {
                Some(i) => Id::new(module, Some(i as u16 + 1), linear32 - self.objects[i].0).ok(),
                None => Some(Id::memory(linear32)),
            };
        }
        match linear.checked_sub(self.base) {
            Some(offset) => Id::new(module, None, u32::try_from(offset).ok()?).ok(),
            None => Some(Id::memory(linear32)),
        }
    }

    /// The linear address (Ghidra's) of an ID in the image or an object.
    pub fn linear(&self, id: &Id) -> Option<u64> {
        match id.part() {
            Some(n) => self
                .objects
                .get(usize::from(n).checked_sub(1)?)
                .map(|&(base, _)| u64::from(base) + u64::from(id.offset())),
            None if self.objects.is_empty() => Some(self.base + u64::from(id.offset())),
            None => None,
        }
    }
}

impl Analysis {
    /// Where Ghidra's addresses are.
    pub fn layout(&self) -> Layout {
        Layout { base: self.base, objects: self.objects.clone() }
    }

    /// `text` (Ghidra's code) with the knowledge's names for Ghidra's `FUN_` and `DAT_` names
    /// (`FUN_SSSS_OOOO` in real mode, `FUN_LLLLLLLL` in a flat program): a function's name, a data
    /// item's, or `name[+N]` for the byte N into one.
    pub fn with_names(&self, text: &str, k: &Knowledge, module: &str) -> String {
        let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        while let Some(found) = text[at..].find(['F', 'D']).map(|i| at + i) {
            out.push_str(&text[at..found]);
            at = found;
            let before = text[..found].chars().next_back();
            let named = [13, 12].into_iter().find_map(|len| {
                let token = text.get(found..found + len)?;
                let after = text[found + len..].chars().next();
                (before.is_none_or(|c| !is_word(c)) && after.is_none_or(|c| !is_word(c)))
                    .then(|| self.name_for(token, k, module).map(|name| (name, len)))
                    .flatten()
            });
            match named {
                Some((name, len)) => {
                    out.push_str(&name);
                    at += len;
                }
                None => {
                    out.push(text[at..].chars().next().expect("a character"));
                    at += 1;
                }
            }
        }
        out.push_str(&text[at..]);
        out
    }

    /// The knowledge's name for Ghidra's `FUN_SSSS_OOOO`, `DAT_SSSS_OOOO` (real mode) or
    /// `FUN_LLLLLLLL`, `DAT_LLLLLLLL` (flat), if it has one.
    fn name_for(&self, token: &str, k: &Knowledge, module: &str) -> Option<String> {
        let (kind, rest) = token.split_at_checked(4)?;
        let address = match rest.split_once('_') {
            Some((segment, offset)) if segment.len() == 4 && offset.len() == 4 => format!("{segment}:{offset}"),
            None if rest.len() == 8 && !self.objects.is_empty() => rest.to_owned(),
            _ => return None,
        };
        let id = self.id_of(module, &address)?;
        match kind {
            "FUN_" => k.functions.get(&id).map(|d| d.name.clone()),
            "DAT_" => {
                let (start, datum) = k.data.range(..=id.clone()).next_back()?;
                let inside = id.offset() - start.offset();
                match (inside, datum.size) {
                    _ if start.module() != id.module() || start.part() != id.part() => None,
                    (0, _) => Some(datum.decision.name.clone()),
                    (n, Some(size)) if n < size => Some(format!("{}[+{n}]", datum.decision.name)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The function whose code holds `id`, and how far into it (from its entry).
    pub fn containing(&self, id: &Id) -> Option<(&Id, u32)> {
        let at = id.offset();
        self.functions
            .iter()
            .filter(|(entry, _)| entry.part() == id.part() && entry.module() == id.module())
            .find(|(_, f)| f.body.iter().any(|&(start, len)| (start..start + len).contains(&at)))
            .map(|(entry, _)| (entry, at.wrapping_sub(entry.offset())))
    }

    /// The link segment of the block (Ghidra's segment) holding image offset `at`, if any.
    pub fn segment_of(&self, at: u32) -> Option<u32> {
        self.blocks.iter().find(|&&(start, size)| (start..start + size).contains(&at)).map(|&(start, _)| start >> 4)
    }

    /// Ghidra's disassembly with each line's `SSSS:OOOO` as an ID.
    pub fn with_ids(&self, asm: &str, module: &str) -> String {
        let mut out = String::with_capacity(asm.len());
        for line in asm.lines() {
            let (address, rest) = line.split_at(line.find(' ').unwrap_or(line.len()));
            match self.id_of(module, address) {
                Some(id) => out.push_str(&format!("{id}{rest}")),
                None => out.push_str(line),
            }
            out.push('\n');
        }
        out
    }

    /// The ID of one of Ghidra's addresses: `SSSS:OOOO` in real mode, a linear `LLLLLLLL` in a flat
    /// program.
    pub fn id_of(&self, module: &str, address: &str) -> Option<Id> {
        // Ghidra's names for addresses too, as its code has them: `FUN_0043525b`, `_DAT_0000046c` (its
        // leading underscores mark overlapping globals), `DAT_2a40_13c2`.
        let address = address.trim_start_matches('_');
        let address = ["FUN_", "DAT_", "LAB_", "PTR_"].iter().find_map(|p| address.strip_prefix(p)).unwrap_or(address);
        let linear = match address.split_once([':', '_']) {
            Some((segment, offset)) => {
                u64::from_str_radix(segment, 16).ok()? * 16 + u64::from_str_radix(offset, 16).ok()?
            }
            None if !self.objects.is_empty() => u64::from_str_radix(address, 16).ok()?,
            None => return None,
        };
        self.layout().id_at(module, linear)
    }
}

/// Library functions by their Function ID hashes, from the signature packs built into rrc.
struct Packs {
    /// By full hash and code units.
    by_full: BTreeMap<(String, u64), Vec<Signature>>,
}

struct Signature {
    name: String,
    specific: String,
    pack: String,
}

impl Packs {
    fn load() -> Result<Packs, String> {
        let mut by_full: BTreeMap<(String, u64), Vec<Signature>> = BTreeMap::new();
        for text in PACKS {
            let pack: Value = serde_json::from_str(text).map_err(|e| format!("a signature pack: {e}"))?;
            let pack_name = pack["name"].as_str().unwrap_or("?");
            for f in pack["functions"].as_array().into_iter().flatten() {
                let units = f["units"].as_u64().unwrap_or(0);
                if units < MIN_UNITS {
                    continue;
                }
                let text = |key: &str| f[key].as_str().unwrap_or_default().to_owned();
                let signature =
                    Signature { name: text("name"), specific: text("specific"), pack: pack_name.to_owned() };
                by_full.entry((text("full"), units)).or_default().push(signature);
            }
        }
        Ok(Packs { by_full })
    }

    /// The one name a function's hashes match (the specific hash decides between names), and its pack.
    fn matching(&self, fid: &Value) -> Option<(String, String)> {
        let (full, units) = (fid["full"].as_str()?, fid["units"].as_u64()?);
        let mut candidates: Vec<&Signature> = self.by_full.get(&(full.to_owned(), units))?.iter().collect();
        let names = |c: &[&Signature]| c.iter().map(|s| &s.name).collect::<std::collections::BTreeSet<_>>().len();
        if names(&candidates) > 1 {
            let specific = fid["specific"].as_str()?;
            candidates.retain(|s| s.specific == specific);
        }
        (names(&candidates) == 1).then(|| (candidates[0].name.clone(), candidates[0].pack.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPORT: &str = r#"{"record":"program","name":"GAME.EXE","entry":{"linear":66576,"text":"1000:0410"},"ghidra":"12.1.4"}
{"record":"function","address":{"linear":65552,"text":"1000:0010"},"name":"entry","source":"IMPORTED","fid":null,"body":[[{"linear":65552},16],[{"linear":65600},4]]}
{"record":"function","address":{"linear":65600,"text":"1000:0040"},"name":"FUN_1000_0040","source":"DEFAULT","fid":{"units":16,"full":"777f06a5fc57d39c","specific":"40de0469e7504e7c","extra":0},"body":[[{"linear":65600},30]]}
{"record":"block","name":"CODE_0","start":{"linear":65536,"text":"1000:0000"},"size":256,"execute":true,"initialized":true}
{"record":"block","name":"DATA_1","start":{"linear":131072,"text":"2000:0000"},"size":64,"execute":true,"initialized":true}
{"record":"call","caller":{"linear":65552},"site":{"linear":65560},"callee":{"linear":65600}}
{"record":"ref","from":{"linear":65556},"to":{"linear":131088,"text":"2000:0010"},"kind":"write"}
{"record":"code","address":{"linear":65600},"asm":"1000:0040  RETF\n","c":null}
"#;

    #[test]
    fn an_export_reads_as_ids_and_library_names() {
        // The entry point is at image offset 410h, so Ghidra's 1000:0000 is the image's start.
        let a = Analysis::parse(EXPORT, "GAME.EXE", 0x410, &[]).unwrap();
        let id = |t: &str| t.parse::<Id>().unwrap();
        assert_eq!(a.functions.len(), 2);
        assert_eq!(a.functions[&id("GAME.EXE+00010")].size, 20);
        assert_eq!(a.containing(&id("GAME.EXE+00015")), Some((&id("GAME.EXE+00010"), 5)));
        assert_eq!(a.containing(&id("GAME.EXE+00042")).map(|f| f.0), Some(&id("GAME.EXE+00010")), "its second range");
        assert_eq!(a.containing(&id("GAME.EXE+00030")), None);
        assert_eq!(a.with_ids("1000:0040  RETF\n", "GAME.EXE"), "GAME.EXE+00040  RETF\n");
        let ftol = &a.functions[&id("GAME.EXE+00040")];
        assert_eq!(ftol.library, Some(("F_FTOL@".into(), "borland-c++-1991-medium".into())));
        assert_eq!(a.calls, [(id("GAME.EXE+00010"), id("GAME.EXE+00018"), id("GAME.EXE+00040"))]);
        assert_eq!(a.refs, [(id("GAME.EXE+00014"), id("GAME.EXE+10010"), "write".into())]);
        assert_eq!((a.segment_of(0x10020), a.segment_of(0x00F0), a.segment_of(0x10040)), (Some(0x1000), Some(0), None));
        assert_eq!(a.code[&id("GAME.EXE+00040")].0, "1000:0040  RETF\n");
        assert_eq!(a.id_of("GAME.EXE", "2000:0010"), Some(id("GAME.EXE+10010")));
        assert_eq!(a.id_of("GAME.EXE", "0FFF:0000"), Some(id("MEM+0FFF0")), "before the image: the PC's memory");
        assert_eq!(a.id_of("GAME.EXE", "0040:006C"), Some(id("MEM+0046C")));
        assert_eq!(a.id_of("GAME.EXE", "DAT_0040_006c"), Some(id("MEM+0046C")), "Ghidra's name for it");
        let mut k = Knowledge::default();
        let d = |name: &str| Decision {
            name: name.into(),
            provenance: "human".into(),
            confidence: None,
            evidence: String::new(),
        };
        k.functions.insert(id("GAME.EXE+00040"), d("ftol"));
        k.data.insert(
            id("GAME.EXE+10010"),
            crate::knowledge::Datum { decision: d("count"), size: Some(4), ty: String::new() },
        );
        let code = "x = FUN_1000_0040(DAT_2000_0010, DAT_2000_0012, DAT_2000_0014, FUN_1000_0010); yDAT_2000_0010;";
        assert_eq!(
            a.with_names(code, &k, "GAME.EXE"),
            "x = ftol(count, count[+2], DAT_2000_0014, FUN_1000_0010); yDAT_2000_0010;",
            "named functions and data, a byte inside an item; not past it, not part of another word"
        );
        let mut k = Knowledge::default();
        assert_eq!(name_library_functions(&mut k, &a), 1);
        assert_eq!(k.functions[&id("GAME.EXE+00040")].provenance, "library:borland-c++-1991-medium");
    }

    /// A flat program's export: Ghidra's addresses are the objects' linear addresses.
    const FLAT_EXPORT: &str = r#"{"record":"program","name":"FLAT.ELF","entry":{"linear":4411640,"text":"004350f8"},"ghidra":"12.1.4"}
{"record":"function","address":{"linear":4411408,"text":"00435010"},"name":"FUN_00435010","source":"DEFAULT","fid":null,"body":[[{"linear":4411408},20]]}
{"record":"ref","from":{"linear":4411412,"text":"00435014"},"to":{"linear":4428828,"text":"0043941c"},"kind":"write"}
"#;

    #[test]
    fn a_flat_program_s_addresses_are_ids_of_its_objects() {
        let objects = [(0x435000, 0x3181), (0x439000, 0x10590)];
        let a = Analysis::parse(FLAT_EXPORT, "FLAT.EXE", 0, &objects).unwrap();
        let id = |t: &str| t.parse::<Id>().unwrap();
        assert!(a.functions.contains_key(&id("FLAT.EXE#1+00010")));
        assert_eq!(a.refs, [(id("FLAT.EXE#1+00014"), id("FLAT.EXE#2+0041C"), "write".into())]);
        assert_eq!(a.containing(&id("FLAT.EXE#1+00012")).map(|f| f.0), Some(&id("FLAT.EXE#1+00010")));
        assert_eq!(a.containing(&id("FLAT.EXE#2+00012")), None, "another object");
        assert_eq!(a.id_of("FLAT.EXE", "0043941c"), Some(id("FLAT.EXE#2+0041C")));
        assert_eq!(a.id_of("FLAT.EXE", "0000046c"), Some(id("MEM+0046C")));
        assert_eq!(a.id_of("FLAT.EXE", "_DAT_0000046c"), Some(id("MEM+0046C")), "Ghidra's name for it");
        assert_eq!(a.id_of("FLAT.EXE", "FUN_0043941c"), Some(id("FLAT.EXE#2+0041C")));
        assert_eq!(a.layout().linear(&id("FLAT.EXE#2+0041C")), Some(0x43941C));
        let mut k = Knowledge::default();
        let d =
            Decision { name: "squares".into(), provenance: "human".into(), confidence: None, evidence: String::new() };
        k.data
            .insert(id("FLAT.EXE#2+0041C"), crate::knowledge::Datum { decision: d, size: Some(64), ty: String::new() });
        assert_eq!(
            a.with_names("x = DAT_00439420; FUN_00435010();", &k, "FLAT.EXE"),
            "x = squares[+4]; FUN_00435010();"
        );
        let mut elf = vec![0x7F, b'E', b'L', b'F'];
        elf.resize(52 + 2 * 32, 0);
        elf[28..32].copy_from_slice(&52u32.to_le_bytes());
        elf[42..44].copy_from_slice(&32u16.to_le_bytes());
        elf[44..46].copy_from_slice(&2u16.to_le_bytes());
        for (i, (base, size)) in objects.iter().enumerate() {
            elf[52 + 32 * i + 8..][..4].copy_from_slice(&base.to_le_bytes());
            elf[52 + 32 * i + 20..][..4].copy_from_slice(&size.to_le_bytes());
        }
        assert_eq!(elf_objects(&elf), Some(objects.to_vec()));
        assert_eq!(elf_objects(b"MZ"), None);
    }

    #[test]
    fn the_entry_point_of_an_mz_program() {
        let mut header = vec![0u8; 0x20];
        header[..2].copy_from_slice(b"MZ");
        header[0x14..0x18].copy_from_slice(&[0x0E, 0x00, 0x27, 0x1A]); // 1A27:000E
        assert_eq!(mz_entry(&header), Some(0x1A27 * 16 + 0x0E));
        header[0x14..0x18].copy_from_slice(&[0x00, 0x01, 0xF0, 0xFF]); // FFF0:0100: the PSP's, at 0
        assert_eq!(mz_entry(&header), Some(0));
        header[0x14..0x18].copy_from_slice(&[0x00, 0x00, 0xF0, 0xFF]); // before the image
        assert_eq!(mz_entry(&header), None);
        assert_eq!(mz_entry(b"\xE9\x00\x01"), None);
    }
}
