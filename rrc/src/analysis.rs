//! `rrc analyze` (DESIGN.md §5, §9): the program as Clockwork unpacks it, analyzed by Ghidra headless,
//! exported (`ghidra/RecExport.java`) into the cache. Library functions that a signature pack
//! (`sigs/`) recognizes by their Function ID hashes are named in the knowledge (`library:<pack>`),
//! unless someone named them already. What analysis left is indexed in `.rrc/analysis.toml`.
//!
//! The scripts and packs are built into `rrc`, so a port repository needs no Rrecreate checkout.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, ExitCode};

use ids::Id;
use serde_json::Value;

use crate::args::Args;
use crate::cache::{Cache, sha256_file};
use crate::clockwork;
use crate::knowledge::{Decision, Knowledge};
use crate::repo::{Repo, read_toml, write_toml};
use crate::{say, tools};

const EXPORT_SCRIPT: &str = include_str!("../../ghidra/RecExport.java");
const PACKS: &[&str] = &[include_str!("../../sigs/borland-c++-1991-medium.json")];
const PROCESSOR: &str = "x86:LE:16:Real Mode";
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
    /// The load image's offset of the entry point, which places Ghidra's addresses in the image.
    pub entry: u32,
    pub ghidra: String,
}

impl Index {
    pub fn load(repo: &Repo) -> Result<Option<Index>, String> {
        let path = repo.root.join(crate::repo::STATE).join(INDEX);
        if path.is_file() { read_toml(&path).map(Some) } else { Ok(None) }
    }
}

pub(crate) fn analyze(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &[], &["--no-decompile"])?;
    a.positionals(0, 0, "analyze [--no-decompile]")?;
    let (ghidra, java) = (tools::ghidra(&repo.local)?, tools::java(&repo.local)?);
    let cache = Cache::open(&repo.local)?;
    let original = repo.game_dir()?.join(&repo.settings.original.program);
    let state = repo.state()?;
    let work = state.join("ghidra");
    std::fs::create_dir_all(work.join("scripts")).map_err(|e| format!("{}: {e}", work.display()))?;
    let module = repo.module();

    // The program as its packer leaves it in memory, if it was packed.
    let program = work.join(&module);
    let unpack = Command::new(clockwork::find(&repo.local)?)
        .arg("unpack")
        .arg(&original)
        .arg("--out")
        .arg(&program)
        .output()
        .map_err(|e| format!("clockwork unpack: {e}"))?;
    let unpacked = unpack.status.success();
    if !unpacked {
        std::fs::copy(&original, &program).map_err(|e| format!("{}: {e}", original.display()))?;
    }
    let bytes = std::fs::read(&program).map_err(|e| format!("{}: {e}", program.display()))?;
    let entry = mz_entry(&bytes).ok_or_else(|| format!("{module}: only MZ programs can be analyzed so far"))?;
    say("program", format!("{module}{}", if unpacked { ", unpacked by Clockwork" } else { "" }));

    let export = work.join("export.jsonl");
    let _ = std::fs::remove_file(&export);
    let script = work.join("scripts").join("RecExport.java");
    std::fs::write(&script, EXPORT_SCRIPT).map_err(|e| format!("{}: {e}", script.display()))?;
    // Ghidra's project is a cache of this run only, in a temporary folder: Ghidra refuses project
    // paths with a folder starting with a dot (`.rrc`).
    let project = std::env::temp_dir().join(format!("rrc-ghidra-{}", std::process::id()));
    std::fs::create_dir_all(&project).map_err(|e| format!("{}: {e}", project.display()))?;
    let mut command = headless(&ghidra);
    command
        .env("JAVA_HOME", &java)
        .arg(&project)
        .arg(&repo.settings.game.name)
        .args(["-import"])
        .arg(&program)
        .args(["-overwrite", "-processor", PROCESSOR, "-scriptPath"])
        .arg(work.join("scripts"))
        .args(["-postScript", "RecExport.java"])
        .arg(&export);
    if !a.has("--no-decompile") {
        command.arg("decompile");
    }
    let log = work.join("ghidra.log");
    let output = command.output().map_err(|e| format!("Ghidra: {e}"));
    let _ = std::fs::remove_dir_all(&project);
    let output = output?;
    std::fs::write(&log, [output.stdout.as_slice(), &output.stderr].concat()).map_err(|e| e.to_string())?;
    if !output.status.success() || !export.is_file() {
        return Err(format!("Ghidra's analysis failed (its log: {})", log.display()));
    }
    let records = std::fs::read(&export).map_err(|e| format!("{}: {e}", export.display()))?;
    let index = Index {
        original: sha256_file(&original)?,
        program: cache.put(&bytes)?,
        unpacked,
        export: cache.put(&records)?,
        entry,
        ghidra: String::new(),
    };
    let analysis = Analysis::parse(&String::from_utf8_lossy(&records), &module, entry)?;
    let index = Index { ghidra: analysis.ghidra.clone(), ..index };
    write_toml(&state.join(INDEX), &index)?;
    say("ghidra", format!("{} functions, {} calls ({})", analysis.functions.len(), analysis.calls.len(), index.ghidra));

    let mut k = Knowledge::load(&repo.knowledge())?;
    let named = name_library_functions(&mut k, &analysis);
    k.save(&repo.knowledge())?;
    let library = analysis.functions.values().filter(|f| f.library.is_some()).count();
    say("library", format!("{library} functions from signature packs, {named} newly named"));
    say("export", &index.export);
    Ok(ExitCode::SUCCESS)
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
    /// The library function a signature pack recognizes: (name, pack).
    pub library: Option<(String, String)>,
}

/// Ghidra's export, with addresses as IDs.
#[derive(Debug, Default)]
pub struct Analysis {
    pub ghidra: String,
    pub functions: BTreeMap<Id, Function>,
    /// (caller, call site, callee).
    pub calls: Vec<(Id, Id, Id)>,
    /// Disassembly and decompiled code, by function.
    pub code: BTreeMap<Id, (String, Option<String>)>,
}

impl Analysis {
    /// The analysis `rrc analyze` left, if any.
    pub fn load(repo: &Repo) -> Result<Option<Analysis>, String> {
        let Some(index) = Index::load(repo)? else { return Ok(None) };
        let cache = Cache::open(&repo.local)?;
        let records = cache.get(&index.export)?;
        Analysis::parse(&String::from_utf8_lossy(&records), &repo.module(), index.entry).map(Some)
    }

    /// Reads an export of `module` whose entry point is at image offset `entry`.
    pub fn parse(records: &str, module: &str, entry: u32) -> Result<Analysis, String> {
        let records: Vec<Value> = records
            .lines()
            .filter(|l| !l.is_empty())
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let program = records.iter().find(|r| r["record"] == "program").ok_or("the export has no program record")?;
        let entry_linear = program["entry"]["linear"].as_u64().ok_or("the export has no entry point")?;
        let base = entry_linear.checked_sub(u64::from(entry)).ok_or("the export's entry point is below its image")?;
        let id = |address: &Value| -> Option<Id> {
            let offset = address["linear"].as_u64()?.checked_sub(base)?;
            Id::new(module, None, u32::try_from(offset).ok()?).ok()
        };
        let packs = Packs::load()?;
        let mut analysis =
            Analysis { ghidra: program["ghidra"].as_str().unwrap_or("?").to_owned(), ..Analysis::default() };
        for r in &records {
            match r["record"].as_str() {
                Some("function") => {
                    let Some(at) = id(&r["address"]) else { continue };
                    let size =
                        r["body"].as_array().into_iter().flatten().filter_map(|range| range[1].as_u64()).sum::<u64>();
                    let function = Function {
                        name: r["name"].as_str().unwrap_or_default().to_owned(),
                        size: size as u32,
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
{"record":"call","caller":{"linear":65552},"site":{"linear":65560},"callee":{"linear":65600}}
{"record":"code","address":{"linear":65600},"asm":"1000:0040  RETF\n","c":null}
"#;

    #[test]
    fn an_export_reads_as_ids_and_library_names() {
        // The entry point is at image offset 410h, so Ghidra's 1000:0000 is the image's start.
        let a = Analysis::parse(EXPORT, "GAME.EXE", 0x410).unwrap();
        let id = |t: &str| t.parse::<Id>().unwrap();
        assert_eq!(a.functions.len(), 2);
        assert_eq!(a.functions[&id("GAME.EXE+00010")].size, 20);
        let ftol = &a.functions[&id("GAME.EXE+00040")];
        assert_eq!(ftol.library, Some(("F_FTOL@".into(), "borland-c++-1991-medium".into())));
        assert_eq!(a.calls, [(id("GAME.EXE+00010"), id("GAME.EXE+00018"), id("GAME.EXE+00040"))]);
        assert_eq!(a.code[&id("GAME.EXE+00040")].0, "1000:0040  RETF\n");
        let mut k = Knowledge::default();
        assert_eq!(name_library_functions(&mut k, &a), 1);
        assert_eq!(k.functions[&id("GAME.EXE+00040")].provenance, "library:borland-c++-1991-medium");
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
