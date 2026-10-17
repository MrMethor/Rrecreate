//! The game crates `rec-host` knows (`games/<game>/`, PROJECT.md §7.1): their Rust overrides
//! for a program, for `--game NAME` with `--rust` and `--dual`.

use dos::hybrid::{Finding, Hybrid, Mode, Override};

/// `wolf3d-planted` is Wolfenstein 3D with a planted bug, for calibration.
pub const GAMES: [&str; 3] = ["testbins", "wolf3d", "wolf3d-planted"];

/// The overrides game `game` has for `program` (a path; its file name is the module).
pub fn overrides(game: &str, program: &str) -> Result<Vec<Override>, String> {
    let module = program.rsplit(['\\', '/']).next().unwrap_or(program).to_ascii_uppercase();
    match game {
        "testbins" => Ok(testbins::overrides(&module)),
        "wolf3d" => Ok(wolf3d::overrides(&module)),
        "wolf3d-planted" => Ok(wolf3d::overrides_with_planted_bug(&module)),
        other => Err(format!("unknown game {other:?} (games: {})", GAMES.join(", "))),
    }
}

/// Applies `--rust`/`--dual` lists: override names or image offsets in hex, or `all`.
pub fn set_modes(hybrid: &mut Hybrid, list: &[String], mode: Mode) -> Result<(), String> {
    for which in list.iter().flat_map(|l| l.split(',')).map(str::trim).filter(|w| !w.is_empty()) {
        if which == "all" {
            hybrid.set_all(mode);
        } else if !hybrid.set_mode(which, mode) {
            let known: Vec<_> = hybrid.overrides().map(|(o, _)| o.name).collect();
            return Err(format!("no override {which:?} (overrides: {})", known.join(", ")));
        }
    }
    Ok(())
}

/// `rec-host replay --game NAME --program MODULE FILE [--json]`: captured calls against the
/// overrides, a result per call.
pub fn cmd_replay(args: &[String]) -> Result<std::process::ExitCode, String> {
    let (mut game, mut program, mut file, mut json) = (None, None, None, false);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--game" => game = Some(args.next().ok_or("--game needs a name")?.clone()),
            "--program" => program = Some(args.next().ok_or("--program needs a module")?.clone()),
            "--json" => json = true,
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            path => file = Some(std::path::PathBuf::from(path)),
        }
    }
    let game = game.ok_or("replay needs --game")?;
    let program = program.ok_or("replay needs --program")?;
    let file = file.ok_or("replay needs a file of captured calls")?;
    let overrides = overrides(&game, &program)?;
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let (mut passed, mut failed, mut skipped) = (0, 0, 0);
    let mut results = Vec::new();
    for (number, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let capture = dos::capture::Capture::from_line(line)?;
        let o = overrides.iter().find(|o| o.offset == capture.offset);
        let (status, differences) = match (o, &capture.unreplayable) {
            (None, _) => ("no-override", Vec::new()),
            (_, Some(reason)) => ("unreplayable", vec![reason.clone()]),
            (Some(o), None) => {
                let differences = dos::capture::replay(&capture, *o);
                (if differences.is_empty() { "pass" } else { "fail" }, differences)
            }
        };
        match status {
            "pass" => passed += 1,
            "fail" => failed += 1,
            _ => skipped += 1,
        }
        results.push((number + 1, capture.offset, capture.name.clone(), capture.input_hash, status, differences));
    }
    if json {
        let items: Vec<String> = results
            .iter()
            .map(|(line, offset, name, hash, status, differences)| {
                let differences: Vec<String> = differences.iter().map(|d| format!("{d:?}")).collect();
                format!(
                    r#"{{"line":{line},"offset":{offset},"name":"{name}","hash":"{hash:016X}","status":"{status}","differences":[{}]}}"#,
                    differences.join(",")
                )
            })
            .collect();
        println!(r#"{{"passed":{passed},"failed":{failed},"skipped":{skipped},"results":[{}]}}"#, items.join(","));
    } else {
        for (line, _, name, _, status, differences) in &results {
            if *status == "fail" {
                println!("fail      line {line} {name}: {}", differences.join("; "));
            }
        }
        println!("replayed  {passed} passed, {failed} failed, {skipped} skipped");
    }
    // 3, not 1: a failing call is a result, not an error of the command.
    Ok(if failed > 0 { std::process::ExitCode::from(3) } else { std::process::ExitCode::SUCCESS })
}

/// One line of counts, and each finding on a line of its own.
pub fn summary_lines(hybrid: &Hybrid) -> Vec<String> {
    let (rust, dual, mismatches) =
        hybrid.stats.iter().fold((0, 0, 0), |(r, d, x), s| (r + s.rust, d + s.dual, x + s.mismatches));
    let captured = hybrid.captures.len();
    let active = hybrid.overrides().filter(|(_, mode)| *mode != Mode::Original).count();
    let mut lines = vec![format!(
        "overrides {active} of {} active: {rust} Rust calls, {dual} dual-runs, {mismatches} mismatches, {captured} calls captured",
        hybrid.overrides().count()
    )];
    lines.extend(hybrid.findings.iter().take(20).map(|f| format!("finding   {}", finding_text(hybrid, f))));
    if hybrid.findings.len() > 20 {
        lines.push(format!("finding   ... {} more", hybrid.findings.len() - 20));
    }
    lines
}

fn finding_text(hybrid: &Hybrid, f: &Finding) -> String {
    let mut parts = Vec::new();
    for (reg, a, b) in &f.registers {
        parts.push(format!("{reg} {a:04X} != {b:04X}"));
    }
    if let Some((a, b)) = f.flags {
        parts.push(format!("flags {a:04X} != {b:04X}"));
    }
    for (addr, a, b) in f.memory.iter().take(4) {
        let place = image_place(hybrid, *addr);
        parts.push(format!("{place} {} != {}", hex(a), hex(b)));
    }
    if f.memory.len() > 4 {
        parts.push(format!("... {} more memory runs", f.memory.len() - 4));
    }
    if !f.vram.is_empty() {
        parts.push(format!("{} video memory runs", f.vram.len()));
    }
    if !f.palette.is_empty() {
        parts.push(format!("{} palette entries", f.palette.len()));
    }
    if let Some(note) = &f.note {
        parts.push(note.clone());
    }
    format!("{} +{:05X} call {} at cycle {}: {}", f.name, f.offset, f.call, f.cycle, parts.join("; "))
}

/// A physical address as an image offset (`+1234`) when it lies in the image, else as is.
fn image_place(hybrid: &Hybrid, addr: u32) -> String {
    match addr.checked_sub(hybrid.base) {
        Some(offset) if offset < 0x10_0000 => format!("+{offset:05X}"),
        _ => format!("{addr:05X}h"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// Each override with its mode and counts, as a JSON array.
pub fn stats_json(hybrid: &Hybrid) -> String {
    let items: Vec<String> = hybrid
        .overrides()
        .zip(&hybrid.stats)
        .map(|((o, mode), s)| {
            format!(
                r#"{{"offset":{},"name":"{}","mode":"{}","rust":{},"dual":{},"mismatches":{},"captured":{}}}"#,
                o.offset,
                o.name,
                format!("{mode:?}").to_lowercase(),
                s.rust,
                s.dual,
                s.mismatches,
                s.captured
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

/// The findings as a JSON array (addresses as image offsets where they are in the image).
pub fn findings_json(hybrid: &Hybrid) -> String {
    let items: Vec<String> = hybrid
        .findings
        .iter()
        .map(|f| {
            let registers: Vec<String> =
                f.registers.iter().map(|(r, a, b)| format!(r#"["{r}",{a},{b}]"#)).collect();
            let runs = |runs: &[(u32, Vec<u8>, Vec<u8>)], relative: bool| -> String {
                let items: Vec<String> = runs
                    .iter()
                    .map(|(addr, a, b)| {
                        let offset = if relative { i64::from(*addr) - i64::from(hybrid.base) } else { i64::from(*addr) };
                        format!(r#"[{addr},{offset},"{}","{}"]"#, hex(a), hex(b))
                    })
                    .collect();
                format!("[{}]", items.join(","))
            };
            format!(
                r#"{{"offset":{},"name":"{}","call":{},"cycle":{},"registers":[{}],"flags":{},"memory":{},"vram":{},"palette":{:?},"note":{}}}"#,
                f.offset,
                f.name,
                f.call,
                f.cycle,
                registers.join(","),
                f.flags.map_or("null".into(), |(a, b)| format!("[{a},{b}]")),
                runs(&f.memory, true),
                runs(&f.vram, false),
                f.palette,
                f.note.as_ref().map_or("null".into(), |n| format!("{n:?}")),
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}
