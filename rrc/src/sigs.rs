//! `rrc sigs build MAP --name NAME --compiler TEXT --source TEXT [--out FILE]`: a library signature
//! pack (`sigs/README.md`) from the port's analysis and the linker map of the same build. The map says
//! which functions came from libraries (Watcom: modules `x.lib(y)`; Borland: modules without a file
//! extension, and the C0 startup); the analysis has their Ghidra Function ID hashes. The pack holds
//! their names and hashes only, no code.
//!
//! Ported from Recreate's `rec sigs build` and its map parser (`tools/rec/src/rec/sigs.py`, `maps.py`),
//! with 32-bit Watcom maps (`0001:00000024`: an LE object and an offset) added.

use std::collections::BTreeMap;
use std::process::ExitCode;

use ids::Id;
use serde_json::{Value, json};

use crate::analysis::{Analysis, Index};
use crate::args::Args;
use crate::cache::Cache;
use crate::knowledge::{Decision, Knowledge, is_name};
use crate::repo::Repo;
use crate::say;

/// The pack format's version.
const VERSION: u64 = 1;

/// Where a map puts a symbol: an LE object's number and an offset in it, or an offset in the load
/// image (`None`).
type Place = (Option<u16>, u32);

#[derive(Debug, Clone, PartialEq)]
struct Segment {
    class: String,
    start: Place,
    size: u32,
}

#[derive(Debug, Clone, PartialEq)]
struct Symbol {
    name: String,
    at: Place,
    /// It came from a library module.
    library: bool,
}

#[derive(Debug, Default)]
struct LinkerMap {
    segments: Vec<Segment>,
    symbols: Vec<Symbol>,
}

impl LinkerMap {
    fn parse(text: &str) -> LinkerMap {
        if text.contains("Open Watcom Linker")
            || text.chars().take(400).collect::<String>().to_uppercase().contains("WATCOM")
        {
            parse_watcom(text)
        } else {
            parse_borland(text)
        }
    }

    /// The symbols in code segments, by place (the first name at a place wins), without what isn't a
    /// function there (Borland's equates, Watcom's code model markers).
    fn functions(&self) -> BTreeMap<Place, &Symbol> {
        let code: Vec<&Segment> =
            self.segments.iter().filter(|s| s.class.eq_ignore_ascii_case("CODE") && s.size > 0).collect();
        let mut found = BTreeMap::new();
        for symbol in self.symbols.iter().filter(|s| !not_code(&s.name)) {
            let (object, offset) = symbol.at;
            let in_code = code.iter().any(|s| s.start.0 == object && (s.start.1..s.start.1 + s.size).contains(&offset));
            if in_code {
                found.entry(symbol.at).or_insert(symbol);
            }
        }
        found
    }
}

/// Symbols in code segments that aren't functions: Borland's equates (the 8087 emulator's fixup values
/// FIARQQ, FJSRQQ...; huge pointer shifts; the memory model; startup switches) and Watcom's code model
/// markers, which share an address with the next function.
fn not_code(name: &str) -> bool {
    let emulator = name.len() == 6
        && name.starts_with('F')
        && matches!(name.as_bytes()[1], b'I' | b'J')
        && name.as_bytes()[2].is_ascii_uppercase()
        && name.ends_with("RQQ");
    emulator
        || name.starts_with("__turbo")
        || name.starts_with("DGROUP@")
        || [
            "__AHSHIFT",
            "__AHINCR",
            "__setargv__",
            "__setenvp__",
            "__floatconvert",
            "__cvtfak",
            "__MMODEL",
            "_big_code_",
            "_small_code_",
            "CodeModelMismatch",
        ]
        .contains(&name)
}

/// `SSSS:OOOO` (16-bit: the load image's paragraph and offset) or `NNNN:OOOOOOOO` (32-bit: an object
/// and an offset), hex.
fn place(text: &str) -> Option<Place> {
    let (segment, offset) = text.split_once(':')?;
    let hex = |t: &str| (t.len() == 4 || t.len() == 8).then(|| u32::from_str_radix(t, 16).ok()).flatten();
    let (segment, offset_value) = (hex(segment)?, hex(offset)?);
    if segment > 0xFFFF {
        return None;
    }
    Some(match offset.len() {
        8 => (Some(segment as u16), offset_value),
        _ => (None, segment * 16 + offset_value),
    })
}

fn parse_watcom(text: &str) -> LinkerMap {
    let mut map = LinkerMap::default();
    let (mut in_symbols, mut module) = (false, None::<String>);
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("Module: ") {
            module = Some(name.trim().to_owned());
            continue;
        }
        if line.starts_with("Address        Symbol") {
            in_symbols = true;
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        // Segments: "_TEXT  CODE  AUTO  0001:00000010  00003171".
        if let [_, class, _, address, size] = fields[..]
            && size.len() == 8
            && let (Some(start), Ok(size)) = (place(address), u32::from_str_radix(size, 16))
        {
            map.segments.push(Segment { class: (*class).to_owned(), start, size });
            continue;
        }
        // Symbols: "0001:00000024+ sum_" (`*` unreferenced, `+` referenced locally only).
        if in_symbols
            && let [address, name] = fields[..]
            && let Some(at) = place(address.trim_end_matches(['*', '+']))
        {
            let library = module.as_ref().is_some_and(|m| m.to_ascii_lowercase().contains(".lib("));
            map.symbols.push(Symbol { name: name.to_owned(), at, library });
        }
    }
    map
}

fn parse_borland(text: &str) -> LinkerMap {
    let mut map = LinkerMap::default();
    // The detailed map's pieces: (image offset, length, module).
    let mut pieces: Vec<(u32, u32, String)> = Vec::new();
    let mut in_publics = false;
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // " 0000:436E 0028 C=CODE   S=_TEXT          G=(none)  M=FMEMCMP    ACBP=28"
        if fields.len() >= 6
            && let (Some((None, start)), Ok(length)) = (place(fields[0]), u32::from_str_radix(fields[1], 16))
            && fields[2].starts_with("C=")
            && let Some(module) = fields.iter().find_map(|f| f.strip_prefix("M="))
        {
            pieces.push((start, length, module.to_owned()));
            continue;
        }
        if line.contains("Publics by Value") {
            in_publics = true;
            continue;
        }
        if line.contains("Publics by Name") || line.contains("Line numbers for") {
            in_publics = false;
            continue;
        }
        // Segments: " 00000H 044BBH 044BCH _TEXT              CODE".
        let hex_h =
            |t: &str| (t.len() == 6 && t.ends_with('H')).then(|| u32::from_str_radix(&t[..5], 16).ok()).flatten();
        if let [start, _, length, _, class] = fields[..]
            && let (Some(start), Some(length)) = (hex_h(start), hex_h(length))
        {
            map.segments.push(Segment { class: class.to_owned(), start: (None, start), size: length });
            continue;
        }
        // Publics: " 0000:1F9B       ___sbrk", " 0000:0000 idle  __turboCrt", " 0000:8087  Abs  __turboFloat".
        if in_publics && let Some(Some(at)) = fields.first().map(|f| place(f)) {
            match fields[1..] {
                [name] | ["idle", name] => map.symbols.push(Symbol { name: name.to_owned(), at, library: false }),
                _ => {}
            }
        }
    }
    for symbol in &mut map.symbols {
        let offset = symbol.at.1;
        if let Some((_, _, module)) =
            pieces.iter().find(|(start, length, _)| (*start..start + length).contains(&offset))
        {
            // Library members have bare names (FMEMCPY, EMU086); the program's objects keep their source
            // file's name (WL_MAIN.C). C0.ASM is the compiler's startup code.
            symbol.library = !module.contains('.') || module.to_ascii_uppercase().starts_with("C0");
        }
    }
    map
}

/// A pack of the library functions `map` names, with their hashes from `records` (Ghidra's export) and
/// `analysis` (which places its addresses); also how many of them the analysis had no hash for.
fn build(map: &LinkerMap, records: &str, analysis: &Analysis, module: &str) -> (Vec<Value>, usize) {
    let layout = analysis.layout();
    let mut hashes: BTreeMap<Id, Value> = BTreeMap::new();
    for line in records.lines() {
        let Ok(r) = serde_json::from_str::<Value>(line) else { continue };
        if r["record"] != "function" || !r["fid"].is_object() {
            continue;
        }
        if let Some(id) = r["address"]["linear"].as_u64().and_then(|l| layout.id_at(module, l)) {
            hashes.insert(id, r["fid"].clone());
        }
    }
    let (mut functions, mut missing) = (Vec::new(), 0);
    for (&(object, offset), symbol) in map.functions().iter().filter(|(_, s)| s.library) {
        let id = Id::new(module, object, offset).ok();
        let Some(fid) = id.and_then(|id| hashes.get(&id)) else {
            missing += 1;
            continue;
        };
        functions.push(json!({
            "name": symbol.name, "units": fid["units"], "full": fid["full"], "specific": fid["specific"],
        }));
    }
    functions.sort_by(|a, b| (a["name"].as_str(), a["full"].as_str()).cmp(&(b["name"].as_str(), b["full"].as_str())));
    (functions, missing)
}

pub(crate) fn sigs(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--name", "--compiler", "--source", "--out"], &[])?;
    let usage = "sigs build MAP --name NAME --compiler TEXT --source TEXT [--out FILE] | sigs name MAP";
    let [command, map_path] = a.positionals(2, 2, usage)? else { unreachable!() };
    let bytes = std::fs::read(map_path).map_err(|e| format!("{map_path}: {e}"))?;
    let map = LinkerMap::parse(&String::from_utf8_lossy(&bytes));
    match command.as_str() {
        "build" => build_pack(repo, &a, &map),
        "name" => name_functions(repo, &map),
        _ => Err(format!("usage: rrc {usage}")),
    }
}

/// Names the map's functions in the knowledge (provenance `map`), so that `rrc analyze` adds those Ghidra
/// didn't find and the pack gets their hashes too: for a port made to build a pack, of a test program.
fn name_functions(repo: &Repo, map: &LinkerMap) -> Result<ExitCode, String> {
    if !repo.settings.port.imports {
        return Err("this port takes no imports ([port] imports = false): a map is a build's truth".into());
    }
    let module = repo.module();
    let _lock = crate::knowledge::EditLock::take(&repo.state()?)?;
    let mut k = Knowledge::load(&repo.knowledge())?;
    let (mut named, mut kept) = (0, 0);
    for (&(object, offset), symbol) in &map.functions() {
        let Ok(id) = Id::new(&module, object, offset) else { continue };
        if k.functions.contains_key(&id) || k.named(&symbol.name).is_some() || !is_name(&symbol.name) {
            kept += 1;
            continue;
        }
        let decision = Decision {
            name: symbol.name.clone(),
            provenance: "map".into(),
            confidence: Some(1.0),
            evidence: "the linker map".into(),
        };
        k.functions.insert(id, decision);
        named += 1;
    }
    k.save(&repo.knowledge())?;
    say("named", format!("{named} functions from the map ({kept} named already, or not names)"));
    say("next", "rrc analyze: adds the ones Ghidra didn't find, then rrc sigs build");
    Ok(ExitCode::SUCCESS)
}

fn build_pack(repo: &Repo, a: &Args, map: &LinkerMap) -> Result<ExitCode, String> {
    let name = a.required("--name")?;
    let index = Index::load(repo)?.ok_or("no analysis: rrc analyze first (of the build the map is of)")?;
    let analysis = Analysis::load(repo)?.ok_or("no analysis: rrc analyze first")?;
    let records = index.records(&Cache::open(&repo.local)?)?;
    let (functions, missing) = build(map, &records, &analysis, &repo.module());
    if functions.is_empty() {
        return Err("the map names no library functions that the analysis has hashes for".into());
    }
    let pack = json!({
        "version": VERSION, "name": name, "compiler": a.required("--compiler")?, "source": a.required("--source")?,
        "functions": functions,
    });
    let out = a.value("--out").map_or_else(|| format!("{name}.json"), str::to_owned);
    let text = serde_json::to_string_pretty(&pack).map_err(|e| e.to_string())? + "\n";
    std::fs::write(&out, text).map_err(|e| format!("{out}: {e}"))?;
    say("pack", format!("{name}: {} library functions, in {out}", functions.len()));
    if missing > 0 {
        let how = "rrc sigs name MAP and rrc analyze add those Ghidra didn't find; the shortest have none";
        say("missing", format!("{missing} of the map's library functions have no hash ({how})"));
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WATCOM32: &str = "Open Watcom Linker Version 2.0 beta
Segment                Class          Group          Address         Size
=======                =====          =====          =======         ====

BEGTEXT                CODE           AUTO           0001:00000000   00000010
_TEXT                  CODE           AUTO           0001:00000010   00003171
_NULL                  BEGDATA        DGROUP         0002:00000000   00000004

Address        Symbol
=======        ======

Module: obj\\flat32.obj(obj\\flat32.obj)
0002:0000041c+ _squares
0001:00000024+ sum_
Module: C:\\WATCOM\\lib386\\dos\\clib3r.lib(stk)
0001:000000a7* __STK
0001:000000ab  __CHK
Module: C:\\WATCOM\\lib386\\dos\\clib3r.lib(printf.c)
0001:000000e0  printf_
";

    #[test]
    fn a_32_bit_watcom_map_places_symbols_in_objects() {
        let map = LinkerMap::parse(WATCOM32);
        assert_eq!(map.segments.len(), 3);
        let functions = map.functions();
        let named = |at: Place| functions.get(&at).map(|s| (s.name.as_str(), s.library));
        assert_eq!(named((Some(1), 0x24)), Some(("sum_", false)));
        assert_eq!(named((Some(1), 0xAB)), Some(("__CHK", true)));
        assert_eq!(named((Some(1), 0xE0)), Some(("printf_", true)));
        assert_eq!(named((Some(2), 0x41C)), None, "data, not code");
    }

    #[test]
    fn a_16_bit_watcom_map_places_symbols_in_the_image() {
        let text = "Open Watcom Linker Version 2.0 beta
medmain_TEXT           CODE           AUTO           0000:0000       00000043
_TEXT                  CODE           AUTO           0005:0000       00000100

Address        Symbol
=======        ======

Module: obj\\medmain.obj(obj\\medmain.obj)
0000:0000      main_
Module: C:\\WATCOM\\lib286\\dos\\clibm.lib(strlen)
0005:0010*     strlen_
";
        let map = LinkerMap::parse(text);
        let functions = map.functions();
        assert_eq!(functions.get(&(None, 0)).map(|s| s.library), Some(false));
        assert_eq!(functions.get(&(None, 0x60)).map(|s| (s.name.as_str(), s.library)), Some(("strlen_", true)));
    }

    #[test]
    fn a_borland_map_s_library_modules_have_bare_names() {
        let text = " Start  Stop   Length Name               Class

 00000H 044BBH 044BCH _TEXT              CODE
 044C0H 04500H 00041H _DATA              DATA

Detailed map of segments

 0000:0000 0100 C=CODE   S=_TEXT          G=(none)  M=C0.ASM     ACBP=28
 0000:0100 0050 C=CODE   S=_TEXT          G=(none)  M=WL_MAIN.C  ACBP=28
 0000:0150 0028 C=CODE   S=_TEXT          G=(none)  M=FMEMCMP    ACBP=28

  Address         Publics by Value

 0000:0000       __startup
 0000:0100       _main
 0000:0150       _fmemcmp
 0000:0160  Abs  __turboFloat
 0000:0170       FIARQQ
";
        let map = LinkerMap::parse(text);
        let functions = map.functions();
        let named = |at: u32| functions.get(&(None, at)).map(|s| (s.name.as_str(), s.library));
        assert_eq!(named(0), Some(("__startup", true)), "C0 is the startup");
        assert_eq!(named(0x100), Some(("_main", false)));
        assert_eq!(named(0x150), Some(("_fmemcmp", true)));
        assert_eq!(named(0x160), None, "an absolute value");
        assert_eq!(named(0x170), None, "the emulator's fixup value");
    }

    #[test]
    fn a_pack_has_the_library_functions_hashes() {
        let records = r#"{"record":"program","name":"FLAT.EXE","entry":{"linear":4411640,"text":"-"},"ghidra":"-"}
{"record":"function","address":{"linear":4411428},"name":"FUN_00435024","fid":{"units":20,"full":"aa","specific":"bb","extra":0},"body":[]}
{"record":"function","address":{"linear":4411563},"name":"FUN_004350ab","fid":{"units":12,"full":"cc","specific":"dd","extra":0},"body":[]}
{"record":"function","address":{"linear":4411616},"name":"FUN_004350e0","fid":null,"body":[]}
"#;
        let analysis = Analysis::parse(records, "FLAT.EXE", 0xF8, &[(0x435000, 0x3181), (0x439000, 0x10590)]).unwrap();
        let (functions, missing) = build(&LinkerMap::parse(WATCOM32), records, &analysis, "FLAT.EXE");
        let names: Vec<(&str, &str)> =
            functions.iter().map(|f| (f["name"].as_str().unwrap(), f["full"].as_str().unwrap())).collect();
        assert_eq!(names, [("__CHK", "cc")], "the library's only: sum_ is the program's");
        assert_eq!(missing, 2, "printf_ has no hash; __STK isn't a function Ghidra found");
    }
}
