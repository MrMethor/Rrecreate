//! `clockwork run`: a headless run, then what the options ask for (files, dumps, the summary).

use std::path::Path;
use std::process::ExitCode;

use dos::{Config, Machine, Stop, Vfs};

use crate::hotspots;
use crate::options::{Options, parse_address};
use crate::screenshot::{bmp, png_image};
use crate::session::Session;
use crate::summary::Summary;

/// The game folder as drive C:, and the disc image the CD shows (with its folder and label), if
/// one.
type GameDrive = (Vfs, String, String, Option<std::sync::Arc<dos::cdrom::CdImage>>);

fn game_drive(o: &Options) -> Result<GameDrive, String> {
    let game_dir = o.game_dir.as_path();
    let (mut drive, warnings) = Vfs::from_host_dir(game_dir).map_err(|e| format!("{}: {e}", game_dir.display()))?;
    for warning in warnings.iter().take(5) {
        eprintln!("clockwork: warning: {warning}");
    }
    if warnings.len() > 5 {
        eprintln!("clockwork: warning: ... {} more", warnings.len() - 5);
    }
    // A disc image: its files go on the drive for the CD to show, its audio to MSCDEX.
    let (mut cd_root, mut cd_label, mut cd_image) = (o.cd_root.clone(), o.cd_label.clone(), None);
    let lower = cd_root.to_ascii_lowercase();
    if lower.ends_with(".cue") || lower.ends_with(".iso") {
        let path = [Path::new(&o.cd_root).to_path_buf(), game_dir.join(&o.cd_root)]
            .into_iter()
            .find(|p| p.is_file())
            .ok_or_else(|| format!("no disc image {}", o.cd_root))?;
        let image = dos::cdrom::CdImage::open(&path)?;
        let (root, label) = dos::cdrom::mount(&mut drive, &image)?;
        cd_root = root;
        if cd_label.is_empty() {
            cd_label = label;
        }
        cd_image = Some(std::sync::Arc::new(image));
    }
    Ok((drive, cd_root, cd_label, cd_image))
}

pub(crate) fn boot(o: &Options, program: &str, args: &str, ips: u64) -> Result<Machine, String> {
    let (drive, cd_root, cd_label, cd_image) = game_drive(o)?;
    let config = Config {
        instructions_per_second: ips,
        cpu_model: o.cpu.unwrap_or(Config::default().cpu_model),
        cd_drive: o.cd_drive,
        cd_root,
        cd_label,
        start_drive: o.start_drive.unwrap_or('C'),
        start_dir: o.start_dir.clone(),
        environment: o.environment.clone(),
        ems_kb: o.ems_kb.unwrap_or(Config::default().ems_kb),
        sound_blaster: o.sound_blaster.unwrap_or(Config::default().sound_blaster),
        extended_kb: o.memory_mb.map_or(Config::default().extended_kb, |mb| (mb - 1) * 1024),
        xms: !o.no_xms,
        fpu: !o.no_fpu,
        joysticks: o.joysticks,
        dpmi: o.dpmi,
        ..Config::default()
    };
    let mut m = Machine::with_program(config, drive, program, args).map_err(|e| e.to_string())?;
    m.cd_image = cd_image;
    Ok(m)
}

/// A machine set up as `run`'s options say: booted (or loaded from a snapshot), with its input
/// scheduled and its debugging records on; with the program's name and the speed.
pub(crate) fn prepare(o: &Options) -> Result<(Machine, String, u64), String> {
    let session = o.replay.as_deref().map(Session::load).transpose()?;
    let (program, args, ips) = match &session {
        Some(s) => (s.program.clone(), s.args.clone(), o.ips.unwrap_or(s.instructions_per_second)),
        None => (o.program.clone().unwrap_or_default(), o.args.clone(), o.ips.unwrap_or(8_000_000)),
    };
    let mut m = match &o.load_snapshot {
        Some(path) => {
            let (drive, _, _, cd_image) = game_drive(o)?;
            let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
            dos::snapshot::load(std::io::BufReader::new(file), drive, cd_image)?
        }
        None => boot(o, &program, &args, ips)?,
    };
    let (program, ips) = if o.load_snapshot.is_some() {
        (m.dos.program.clone(), o.ips.unwrap_or(m.config.instructions_per_second))
    } else {
        (program, ips)
    };
    if o.coverage.is_some() {
        m.start_coverage();
    }
    if o.contracts.is_some() {
        m.start_contracts();
    }
    if let Some(session) = &session {
        for &(cycle, input) in &session.inputs {
            m.queue_input(cycle, input);
        }
    }
    if let Some(text) = &o.type_text {
        let start = m.cycle_at_ms(o.type_at_ms);
        m.type_text(start, text, m.cycle_at_ms(60));
    }
    for script in &o.inputs {
        script.schedule(&mut m);
    }
    if o.trace.is_some() {
        m.probes.service_trace = Some(Vec::new());
    }
    if let Some(spec) = &o.watch_exec {
        let (address, length) = match spec.rsplit_once(':') {
            Some((address, length)) if address.contains(':') => {
                (address, u32::from_str_radix(length, 16).map_err(|_| format!("bad length {length:?}"))?)
            }
            _ => (spec.as_str(), 1),
        };
        let (segment, offset) = parse_address(address)?;
        let start = (u32::from(segment) << 4).wrapping_add(offset);
        m.probes.exec_watch = Some(start..start.wrapping_add(length));
    }
    Ok((m, program, ips))
}

pub(crate) fn cmd_run(o: &Options) -> Result<ExitCode, String> {
    let (mut m, program, ips) = prepare(o)?;
    let limit = (o.seconds.unwrap_or(10.0) * ips as f64) as u64;
    let (stop, profile) = if o.hotspots > 0 {
        let (stop, profile) = hotspots::run_sampled(&mut m, limit);
        (stop, Some(profile))
    } else {
        (m.run(limit), None)
    };

    if let Some(path) = &o.screenshot {
        let (w, h, rgb) = m.screen_rgb();
        let png = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
        let bytes = if png { png_image(w, h, &rgb) } else { bmp(w, h, &rgb) };
        std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let Some(path) = &o.wav {
        let samples = m.audio_log().render(m.elapsed_ns());
        std::fs::write(path, dos::audio::wav(&samples)).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let Some(dir) = &o.save_changes {
        save_changes(&m, dir)?;
    }
    if let Some(dir) = &o.dump_image {
        dump_image(&m, dir)?;
    }
    if let Some(path) = &o.save_snapshot {
        let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        dos::snapshot::save(&m, std::io::BufWriter::new(file))?;
    }
    if let (Some(path), Some(coverage)) = (&o.coverage, &m.probes.coverage) {
        std::fs::write(path, coverage.to_json(&program)).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let (Some(path), Some(contracts)) = (&o.contracts, &m.probes.contracts) {
        std::fs::write(path, contracts.to_json(&program)).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let summary = Summary::new(&m, &stop);
    if let Some(profile) = &profile {
        profile.print(&m, o.hotspots);
    }
    if let Some(path) = &o.midi {
        let mut text = String::new();
        for chunk in m.devices.mpu.midi_log.chunk_by(|a, b| a.pit_tick == b.pit_tick) {
            let bytes: Vec<String> = chunk.iter().map(|b| format!("{:02X}", b.value)).collect();
            text.push_str(&format!("{:>12} {}\n", chunk[0].pit_tick, bytes.join(" ")));
        }
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if o.watch_exec.is_some() {
        print_exec_hits(&m);
    }
    if let Some(path) = &o.watch_log {
        let text: String = m
            .probes
            .exec_hits
            .iter()
            .map(|h| {
                let r = &h.regs;
                format!(
                    "{:>11} {:04X}:{:04X} AX={:04X} BX={:04X} CX={:04X} DX={:04X} SI={:04X} DI={:04X} BP={:04X} SP={:04X} DS={:04X} ES={:04X}\n",
                    h.cycle, h.cs, h.ip, r.ax(), r.bx(), r.cx(), r.dx(), r.si(), r.di(), r.bp(), r.sp(), r.ds(), r.es()
                )
            })
            .collect();
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let Some(path) = &o.opl {
        let text: String = m
            .audio_log()
            .opl
            .iter()
            .map(|w| format!("{:>10.3} {:03X} {:02X}\n", w.nanos as f64 / 1e6, w.register, w.value))
            .collect();
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let (Some(path), Some(calls)) = (&o.trace, &m.probes.service_trace) {
        let text: String = calls.iter().map(|call| format!("{call}\n")).collect();
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("trace     {} service calls written to {}", calls.len(), path.display());
    }
    if let Some(at) = &o.dump {
        if let Some(image) = at.strip_prefix('+') {
            let (offset, length) = image.split_once(':').unwrap_or((image, "40"));
            let offset = u32::from_str_radix(offset, 16).map_err(|_| format!("bad hex image offset {offset:?}"))?;
            let length = usize::from_str_radix(length, 16).map_err(|_| format!("bad hex length {length:?}"))?;
            print_image_dump(&m, m.image_range().start, offset, length);
        } else {
            let (address, length) = match at.rsplit_once(':') {
                Some((address, length)) if address.contains(':') => {
                    (address, usize::from_str_radix(length, 16).map_err(|_| format!("bad hex length {length:?}"))?)
                }
                _ => (at.as_str(), 64),
            };
            let (segment, offset) = parse_address(address)?;
            print_dump(&m, segment, offset, length)?;
        }
    }
    if let Some(at) = &o.disasm {
        let (cs, ip, code) = if at == "here" {
            (m.cpu.regs.cs(), m.cpu.regs.ip, hotspots::current_code(&m))
        } else {
            let (cs, ip) = parse_address(at)?;
            let (linear, big) = m.peek_address(cs, ip).ok_or_else(|| format!("no segment {cs:04X} now"))?;
            (cs, ip, hotspots::Code { linear, bits: if big { 32 } else { 16 } })
        };
        hotspots::print_listing(&m, cs, ip, code, 40);
    }
    if o.json {
        println!("{}", summary.json());
    } else {
        summary.print(&m);
    }
    // A fault stops the run: fail loud.
    Ok(if matches!(stop, Stop::Fault(_)) { ExitCode::from(2) } else { ExitCode::SUCCESS })
}

/// Writes the files the run created or changed (and that still exist) under `dir`, keeping their
/// paths on the virtual drive.
fn save_changes(m: &Machine, dir: &Path) -> Result<(), String> {
    for path in m.drive.changed() {
        let Some(data) = m.drive.file(path) else { continue };
        let target = path.split('\\').fold(dir.to_path_buf(), |target, part| target.join(part));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&target, data).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    Ok(())
}

/// Hex-dumps memory at `segment:offset` as the CPU sees it now: a selector's descriptor in
/// protected mode, and the page tables when paging is on (`--` for bytes on missing pages).
fn print_dump(m: &Machine, segment: u16, offset: u32, length: usize) -> Result<(), String> {
    let (linear, _) = m.peek_address(segment, offset).ok_or_else(|| format!("no segment {segment:04X} now"))?;
    println!("dump      {segment:04X}:{offset:04X} (linear {linear:08X}), {length:#x} bytes");
    for (row, chunk) in m.peek_linear(linear, length).chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| b.map_or("--".into(), |b| format!("{b:02X}"))).collect();
        let text: String = chunk.iter().map(|b| b.map_or(' ', dos::cp437_to_char)).collect();
        println!("  {segment:04X}:{:04X}  {:<48} {text}", offset.wrapping_add(row as u32 * 16), hex.join(" "));
    }
    Ok(())
}

/// Hex-dumps the program's load image (at linear `base`) from image offset `offset`.
fn print_image_dump(m: &Machine, base: u32, offset: u32, length: usize) {
    let linear = base + offset;
    println!("dump      +{offset:05X} (linear {linear:08X}), {length:#x} bytes");
    for (row, chunk) in (0..length as u32).map(|i| m.mem.read_u8(linear + i)).collect::<Vec<_>>().chunks(16).enumerate()
    {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02X}")).collect();
        let text: String = chunk.iter().map(|&b| dos::cp437_to_char(b)).collect();
        println!("  +{:05X}  {:<48} {text}", offset + row as u32 * 16, hex.join(" "));
    }
}

/// Writes [`Machine::image`] to `dir`.
pub(crate) fn dump_image(m: &Machine, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let write = |name: &str, data: &[u8]| {
        let path = dir.join(name);
        std::fs::write(&path, data).map_err(|e| format!("{}: {e}", path.display()))
    };
    match m.image() {
        dos::image::Image::Real { psp, load_segment, bytes, cs, ip, ss, sp } => {
            write("image.bin", &bytes)?;
            let map = serde_json::json!({
                "mode": "real", "psp": psp, "load_segment": load_segment, "size": bytes.len(),
                "cs": cs, "ip": ip, "ss": ss, "sp": sp,
            });
            write("image.json", map.to_string().as_bytes())?;
            println!("image     {} bytes from {load_segment:04X}:0000 in {}", bytes.len(), dir.display());
        }
        dos::image::Image::Protected { segments, regions } => {
            for (base, bytes) in &regions {
                write(&format!("region-{base:08X}.bin"), bytes)?;
            }
            let map = serde_json::json!({
                "mode": "protected",
                "segments": segments,
                "regions": regions.iter().map(|(base, bytes)| serde_json::json!({"base": base, "size": bytes.len()})).collect::<Vec<_>>(),
            });
            write("segments.json", map.to_string().as_bytes())?;
            println!("image     {} segments, {} regions in {}", segments.len(), regions.len(), dir.display());
        }
    }
    Ok(())
}

/// A watched entry point: CS, IP and the return address on the stack (segment, offset).
type EntryKey = (u16, u32, u16, u16);
/// How often it was entered, the first cycle, and AX and BX that first time.
type EntryStats = (usize, u64, u16, u16);

/// The watched range's entry points (instructions reached from outside the range, or first),
/// grouped with the far or near return address on the stack, most frequent first.
fn print_exec_hits(m: &Machine) {
    let hits = &m.probes.exec_hits;
    println!(
        "watch     {} instructions recorded{}",
        hits.len(),
        if hits.len() >= dos::MAX_EXEC_HITS { " (limit reached)" } else { "" }
    );
    let mut entries: std::collections::BTreeMap<EntryKey, EntryStats> = Default::default();
    let mut previous_cycle = None;
    for hit in hits {
        // An entry: the first hit, or one that isn't the next instruction after the last hit.
        let entered = previous_cycle.is_none_or(|c: u64| hit.cycle > c + 1);
        previous_cycle = Some(hit.cycle);
        if entered {
            let key = (hit.cs, hit.ip, hit.stack[1], hit.stack[0]);
            let entry = entries.entry(key).or_insert((0, hit.cycle, hit.regs.ax(), hit.regs.bx()));
            entry.0 += 1;
        }
    }
    let mut sorted: Vec<_> = entries.into_iter().collect();
    sorted.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.1.1.cmp(&b.1.1)));
    for ((cs, ip, ret_seg, ret_off), (count, first, ax, bx)) in sorted.into_iter().take(30) {
        println!(
            "  {cs:04X}:{ip:04X}  {count:>6}x  first at cycle {first:>11}  stack {ret_seg:04X}:{ret_off:04X}  AX={ax:04X} BX={bx:04X}"
        );
    }
}
