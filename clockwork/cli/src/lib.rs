//! `clockwork`: runs a DOS program on Clockwork.
//!
//! `run` is headless and prints a short summary (or JSON) meant for tools and agents.
//! `play` opens a window, takes keyboard input, and can record it as a session that `run
//! --replay` reproduces bit for bit.

mod gamepads;
mod hotspots;
mod input_script;
mod listing;
mod live_audio;
mod play;
pub mod serve;
mod session;
mod unpack;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dos::hash::fnv64;
use dos::{Config, Machine, Stop, Vfs};

use crate::session::Session;

const USAGE: &str = "\
usage:
  clockwork run  <game-dir> <program> [options]  run headless and print a summary
  clockwork play <game-dir> <program> [options]  run in a window with keyboard input
  clockwork unpack <program> --out FILE [--json] undo an executable packer (PKLITE, LZEXE, ...) by
                                                 running it to its entry point; writes an MZ file
  clockwork serve [--port N]                     be driven by a tool: JSON requests, one per line,
                                                 on stdin or a local TCP port (see serve.rs)
  clockwork listing <image> --entry OFFSET --segment SEG
                                                 a function's code from a load image (hex image
                                                 offset and link segment), following its control
                                                 flow and jump tables, in image offsets

options:
  --args TEXT        command line passed to the program
  --ips N            emulated instructions per second (default 8000000)
  --seconds N        emulated seconds to run (run: default 10; play: until the window closes)
  --type TEXT        run: type TEXT on the keyboard (\\r = Enter, \\e = Esc)
  --type-at MS       run: when typing starts, in emulated ms (default 500)
  --input MS:KEYS    run: keys and clicks from emulated time MS (repeatable): text, {enter} {esc}
                     {tab} {bksp} {space} {up} {down} {left} {right} {home} {end} {pgup} {pgdn}
                     {ins} {del} {f1}..{f12} {shift} {rshift} {ctrl} {rctrl} {alt} {ralt} {caps},
                     {hold KEY MS} (held MS ms), {keydown KEY} {keyup KEY} (KEY a name or a
                     character), {click X Y} {rclick X Y} {move X Y} (mouse driver coordinates),
                     {wait MS}; one step after the other, keys 120 ms apart
  --input-file FILE  run: --input lines from FILE (# starts a comment)
  --replay FILE      run: replay a session recorded by play (program may be omitted)
  --screenshot FILE  run: save the final screen as a PNG (FILE.png) or a BMP
  --wav FILE         run: save the sound (OPL and Sound Blaster) as a WAV file
  --coverage FILE    run: record which instructions of the program's image ran and the calls it
                     made (JSON)
  --contracts FILE   run: infer each called function's contract (registers read and returned,
                     registers preserved, stack arguments and pops, memory use; JSON)
  --dump-image DIR   run: afterwards, write the program's image for analysis to DIR: in real mode
                     image.bin (from the load segment) and image.json, in protected mode
                     segments.json (the selector map) and the memory segments cover
  --save-snapshot FILE  run: afterwards, save the machine to FILE (without the game's files)
  --load-snapshot FILE  run: go on from a snapshot instead of starting the program, for
                     --seconds more (the game folder and CD must be those it was taken with)
  --save-changes DIR run: afterwards, write the files the program created or changed to DIR
                     (same paths as on C:; the game folder itself is never touched), e.g. what
                     an installer unpacked
  --json             run: print the summary as JSON
  --hotspots N       run: sample CS:IP while running and show the N hottest addresses
  --trace FILE       run: write every BIOS/DOS/EMS service call to FILE
  --midi FILE        run: write the MIDI bytes sent to the MPU-401, one line per PIT tick
  --opl FILE         run: write the OPL register writes, one line each (ms, register, value)
  --watch-exec SEG:OFF[:LEN]  run: record each instruction run in that range (real-mode address,
                     LEN hex, default 1) and list the entry points with their callers
  --watch-log FILE   run: with --watch-exec, write every recorded instruction and its registers
  --dump SEG:OFF[:LEN]  run: afterwards, hex-dump LEN bytes (hex, default 40h) of memory there;
                     +OFF[:LEN] for an offset in the program's load image (as in function IDs)
  --disasm SEG:OFF   run: afterwards, show registers and disassemble 40 instructions there
                     (or: here, for the current CS:IP)
  --record FILE      play: record keyboard input as a session
  --no-audio         play: no sound and no MIDI output
  --midi-out NAME    play: send MIDI to the host MIDI output whose name contains NAME (default:
                     the first one, e.g. Microsoft GS Wavetable Synth; none: no MIDI)
  --cpu 286|386|486  processor the program sees (default 486)
  --no-fpu           no x87 coprocessor (escape opcodes do nothing)
  --joystick N       joysticks on the game port, 0-2 (default 0); play: the host's gamepads
                     drive them (left stick, A and B buttons)
  --cd LETTER[=DIR]  add a CD-ROM drive (with MSCDEX) showing the same files as C:, or only its
                     folder DIR (like DOSBox mounting a game's CD folder), or a disc image:
                     FILE.CUE (BINARY and 16-bit 44.1 kHz stereo WAVE tracks: its data track's
                     files and its CD audio) or FILE.ISO
  --cd-label NAME    the CD's volume label (default: the image's, its folder's name, or CDROM)
  --drive LETTER     the current drive when the program starts (default C)
  --cwd PATH         the current directory when the program starts, as if CD ran first;
                     a relative program path starts there
  --env NAME=VALUE   set an environment variable (repeatable)
  --ems KB           expanded memory size (default 4096; 0 disables EMS)
  --memory MB        total memory of the PC, 1-16 (default 16)
  --no-xms           no XMS driver (programs see the extended memory through INT 15h)
  --dpmi             a DPMI 0.9 host, for programs that look for one (DJGPP without CWSDPMI,
                     Borland's protected-mode programs); off by default, as extenders that
                     bring their own (DOS/4GW) would use it instead
  --sb SPEC          sound card: none, sb1, sb2, sbpro1, sbpro2 or sb16 (default), optionally
                     followed by BLASTER-style resources, e.g. \"sbpro2 A220 I5 D1\"
  --scale N          play: window scale (default 2)
  --help             this text";

#[derive(Default)]
struct Options {
    game_dir: PathBuf,
    program: Option<String>,
    args: String,
    ips: Option<u64>,
    seconds: Option<f64>,
    type_text: Option<String>,
    inputs: Vec<input_script::Script>,
    type_at_ms: u64,
    replay: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    wav: Option<PathBuf>,
    save_changes: Option<PathBuf>,
    save_snapshot: Option<PathBuf>,
    dump_image: Option<PathBuf>,
    load_snapshot: Option<PathBuf>,
    coverage: Option<PathBuf>,
    contracts: Option<PathBuf>,
    sound_blaster: Option<Option<dos::sblaster::SbConfig>>,
    json: bool,
    record: Option<PathBuf>,
    no_audio: bool,
    midi_out: Option<String>,
    scale: usize,
    cpu: Option<dos::CpuModel>,
    cd_drive: Option<char>,
    cd_root: String,
    cd_label: String,
    start_drive: Option<char>,
    start_dir: String,
    environment: Vec<(String, String)>,
    ems_kb: Option<u32>,
    memory_mb: Option<u32>,
    no_xms: bool,
    no_fpu: bool,
    dpmi: bool,
    joysticks: u8,
    hotspots: usize,
    disasm: Option<String>,
    dump: Option<String>,
    trace: Option<PathBuf>,
    midi: Option<PathBuf>,
    opl: Option<PathBuf>,
    watch_exec: Option<String>,
    watch_log: Option<PathBuf>,
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

/// The command line.
pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = match args.first().map(String::as_str) {
        Some("run") => parse_options(&args[1..]).and_then(|o| cmd_run(&o)),
        Some("play") => parse_options(&args[1..]).and_then(|o| play::run(&o)),
        Some("unpack") => cmd_unpack(&args[1..]),
        Some("listing") => listing::cmd_listing(&args[1..]),
        Some("serve") => serve::cmd_serve(&args[1..]),
        Some("--version") => {
            println!("clockwork {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(1);
        }
    };
    result.unwrap_or_else(|message| {
        eprintln!("clockwork: error: {message}");
        ExitCode::from(1)
    })
}

fn cmd_unpack(args: &[String]) -> Result<ExitCode, String> {
    let (mut program, mut out, mut json) = (None, None, false);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(args.next().ok_or("--out needs a file")?)),
            "--json" => json = true,
            flag if flag.starts_with("--") => {
                return Err(format!(
                    "unknown option {flag}

{USAGE}"
                ));
            }
            path if program.is_none() => program = Some(PathBuf::from(path)),
            extra => return Err(format!("unexpected argument {extra:?}")),
        }
    }
    let program = program.ok_or("unpack needs a program")?;
    let out = out.ok_or("unpack needs --out FILE")?;
    let unpacked = unpack::unpack(&program)?;
    std::fs::write(&out, unpack::mz_file(&unpacked)).map_err(|e| format!("{}: {e}", out.display()))?;
    let u = &unpacked;
    if json {
        println!(
            r#"{{"entry":"{:04X}:{:04X}","stack":"{:04X}:{:04X}","image_size":{},"relocations":{},"instructions":{},"stray_bytes":{}}}"#,
            u.cs,
            u.ip,
            u.ss,
            u.sp,
            u.image.len(),
            u.relocations.len(),
            u.instructions,
            u.stray_bytes
        );
    } else {
        println!(
            "unpacked  entry {:04X}:{:04X}, stack {:04X}:{:04X}, image {} bytes, {} relocations ({} instructions)",
            u.cs,
            u.ip,
            u.ss,
            u.sp,
            u.image.len(),
            u.relocations.len(),
            u.instructions
        );
        if u.stray_bytes > 0 {
            println!("warning   {} bytes differ between the two runs outside relocations", u.stray_bytes);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut options = Options { type_at_ms: 500, scale: 2, ..Options::default() };
    let mut positional = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |name: &str| iter.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--args" => options.args = value(arg)?,
            "--ips" => options.ips = Some(value(arg)?.parse().map_err(|_| "--ips needs a whole number")?),
            "--seconds" => options.seconds = Some(value(arg)?.parse().map_err(|_| "--seconds needs a number")?),
            "--type" => options.type_text = Some(value(arg)?.replace("\\r", "\r").replace("\\e", "\x1b")),
            "--type-at" => options.type_at_ms = value(arg)?.parse().map_err(|_| "--type-at needs milliseconds")?,
            "--input" => options.inputs.push(input_script::Script::parse(&value(arg)?)?),
            "--input-file" => {
                let path = value(arg)?;
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
                for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
                    options.inputs.push(input_script::Script::parse(line)?);
                }
            }
            "--replay" => options.replay = Some(value(arg)?.into()),
            "--screenshot" => options.screenshot = Some(value(arg)?.into()),
            "--wav" => options.wav = Some(value(arg)?.into()),
            "--save-changes" => options.save_changes = Some(value(arg)?.into()),
            "--save-snapshot" => options.save_snapshot = Some(value(arg)?.into()),
            "--dump-image" => options.dump_image = Some(value(arg)?.into()),
            "--load-snapshot" => options.load_snapshot = Some(value(arg)?.into()),
            "--coverage" => options.coverage = Some(value(arg)?.into()),
            "--contracts" => options.contracts = Some(value(arg)?.into()),
            "--sb" => options.sound_blaster = Some(sound_blaster(&value(arg)?)?),
            "--json" => options.json = true,
            "--trace" => options.trace = Some(value(arg)?.into()),
            "--midi" => options.midi = Some(value(arg)?.into()),
            "--opl" => options.opl = Some(value(arg)?.into()),
            "--watch-exec" => options.watch_exec = Some(value(arg)?),
            "--watch-log" => options.watch_log = Some(value(arg)?.into()),
            "--disasm" => options.disasm = Some(value(arg)?),
            "--dump" => options.dump = Some(value(arg)?),
            "--hotspots" => options.hotspots = value(arg)?.parse().map_err(|_| "--hotspots needs a count")?,
            "--record" => options.record = Some(value(arg)?.into()),
            "--no-audio" => options.no_audio = true,
            "--midi-out" => options.midi_out = Some(value(arg)?),
            "--scale" => options.scale = value(arg)?.parse().map_err(|_| "--scale needs a whole number")?,
            "--ems" => options.ems_kb = Some(value(arg)?.parse().map_err(|_| "--ems needs kilobytes")?),
            "--memory" => {
                let mb: u32 = value(arg)?.parse().map_err(|_| "--memory needs megabytes")?;
                if !(1..=16).contains(&mb) {
                    return Err("--memory must be 1 to 16 (MB)".into());
                }
                options.memory_mb = Some(mb);
            }
            "--no-xms" => options.no_xms = true,
            "--no-fpu" => options.no_fpu = true,
            "--dpmi" => options.dpmi = true,
            "--joystick" => {
                options.joysticks = value(arg)?.parse().ok().filter(|&n| n <= 2).ok_or("--joystick needs 0, 1 or 2")?;
            }
            "--cd" => {
                let spec = value(arg)?;
                let (letter, root) = spec.split_once('=').unwrap_or((&spec, ""));
                options.cd_drive = Some(drive_letter(letter)?);
                options.cd_root = root.to_string();
            }
            "--cd-label" => options.cd_label = value(arg)?,
            "--drive" => options.start_drive = Some(drive_letter(&value(arg)?)?),
            "--cwd" => options.start_dir = value(arg)?,
            "--env" => {
                let assignment = value(arg)?;
                let (name, text) = assignment.split_once('=').ok_or("--env needs NAME=VALUE")?;
                options.environment.push((name.to_ascii_uppercase(), text.to_owned()));
            }
            "--cpu" => {
                options.cpu = Some(match value(arg)?.as_str() {
                    "286" => dos::CpuModel::I286,
                    "386" => dos::CpuModel::I386,
                    "486" => dos::CpuModel::I486,
                    other => return Err(format!("--cpu must be 286, 386 or 486, not {other:?}")),
                })
            }
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}\n\n{USAGE}")),
            _ => positional.push(arg.clone()),
        }
    }
    match positional.as_slice() {
        [dir] => options.game_dir = dir.into(),
        [dir, program] => {
            options.game_dir = dir.into();
            options.program = Some(program.clone());
        }
        _ => return Err(format!("expected <game-dir> and <program>\n\n{USAGE}")),
    }
    if options.program.is_none() && options.replay.is_none() && options.load_snapshot.is_none() {
        return Err("missing <program>".into());
    }
    if options.replay.is_some() && options.load_snapshot.is_some() {
        return Err("--replay and --load-snapshot exclude each other".into());
    }
    if options.seconds.is_some_and(|s| !s.is_finite() || s <= 0.0) || options.scale == 0 {
        return Err("--seconds and --scale must be positive".into());
    }
    Ok(options)
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

/// `SEG:OFF` in hex; the offset may be 32-bit (protected mode).
fn parse_address(text: &str) -> Result<(u16, u32), String> {
    let (segment, offset) = text.split_once(':').ok_or_else(|| format!("expected SEG:OFF in hex, not {text:?}"))?;
    let segment = u16::from_str_radix(segment, 16).map_err(|_| format!("bad hex number {segment:?}"))?;
    let offset = u32::from_str_radix(offset, 16).map_err(|_| format!("bad hex number {offset:?}"))?;
    Ok((segment, offset))
}

fn drive_letter(text: &str) -> Result<char, String> {
    match text.trim_end_matches(':').chars().collect::<Vec<_>>()[..] {
        [letter] if letter.is_ascii_alphabetic() => Ok(letter.to_ascii_uppercase()),
        _ => Err(format!("expected a drive letter, not {text:?}")),
    }
}

/// Loads the game folder as drive C: and the program into a fresh machine.
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

fn boot(o: &Options, program: &str, args: &str, ips: u64) -> Result<Machine, String> {
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
/// scheduled and its debugging records on; with the program's name, the speed, and where its
/// load image starts.
fn prepare(o: &Options) -> Result<(Machine, String, u64, u32), String> {
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
    // The program's load image starts at its PSP's paragraph 10h (where image offsets count from).
    let image_base = (u32::from(m.dos.psp) + 0x10) << 4;
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
        m.service_trace = Some(Vec::new());
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
        m.exec_watch = Some(start..start.wrapping_add(length));
    }
    Ok((m, program, ips, image_base))
}

fn cmd_run(o: &Options) -> Result<ExitCode, String> {
    let (mut m, program, ips, image_base) = prepare(o)?;
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
    if let (Some(path), Some(coverage)) = (&o.coverage, &m.coverage) {
        std::fs::write(path, coverage.to_json(&program)).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let (Some(path), Some(contracts)) = (&o.contracts, &m.contracts) {
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
            .exec_hits
            .iter()
            .map(|h| {
                let r = &h.regs;
                format!(
                    "{:>11} {:04X}:{:04X} AX={:04X} BX={:04X} CX={:04X} DX={:04X} SI={:04X} DI={:04X} BP={:04X} SP={:04X} DS={:04X} ES={:04X}
",
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
            .map(|w| {
                format!(
                    "{:>10.3} {:03X} {:02X}
",
                    w.nanos as f64 / 1e6,
                    w.register,
                    w.value
                )
            })
            .collect();
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let (Some(path), Some(calls)) = (&o.trace, &m.service_trace) {
        let text: String = calls.iter().map(|call| format!("{call}\n")).collect();
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("trace     {} service calls written to {}", calls.len(), path.display());
    }
    if let Some(at) = &o.dump {
        if let Some(image) = at.strip_prefix('+') {
            let (offset, length) = image.split_once(':').unwrap_or((image, "40"));
            let offset = u32::from_str_radix(offset, 16).map_err(|_| format!("bad hex image offset {offset:?}"))?;
            let length = usize::from_str_radix(length, 16).map_err(|_| format!("bad hex length {length:?}"))?;
            print_image_dump(&m, image_base, offset, length);
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

struct Summary {
    stop: String,
    exit_code: Option<u8>,
    cycles: u64,
    elapsed_ms: u64,
    frames: usize,
    distinct_frames: usize,
    midi_bytes: usize,
    speaker_events: usize,
    opl_writes: usize,
    pcm_frames: usize,
    audio_hash: u64,
    /// Every frame's hash, in order.
    frames_hash: u64,
    timer: String,
    irqs: String,
    last_frame: Option<dos::Frame>,
    state_hash: u64,
    console_hash: u64,
    issues: Vec<String>,
    files_changed: Vec<String>,
    contract_lines: Vec<String>,
}

impl Summary {
    fn new(m: &Machine, stop: &Stop) -> Self {
        let stop_text = stop_text(stop);
        let mut issues: Vec<String> = m.diagnostics.iter().cloned().collect();
        if !m.devices.unknown_ports.is_empty() {
            let ports: Vec<String> = m.devices.unknown_ports.iter().map(|p| format!("{p:X}h")).collect();
            issues.push(format!("unemulated I/O ports: {}", ports.join(" ")));
        }
        Self {
            stop: stop_text,
            exit_code: m.exit_code(),
            cycles: m.cpu.cycles,
            elapsed_ms: m.elapsed_ms(),
            frames: m.frames.len(),
            distinct_frames: m.frames.iter().map(|f| f.hash).collect::<BTreeSet<_>>().len(),
            midi_bytes: m.devices.mpu.midi_log.len(),
            speaker_events: m.devices.pit.speaker_log.len(),
            opl_writes: m.audio_log().opl.len(),
            pcm_frames: m.audio_log().pcm.iter().map(|s| s.samples.len() / if s.stereo { 2 } else { 1 }).sum(),
            audio_hash: m.audio_log().hash(),
            frames_hash: {
                let mut h = dos::hash::Fnv64::default();
                for f in &m.frames {
                    std::hash::Hash::hash(&f.hash, &mut h);
                }
                std::hash::Hasher::finish(&h)
            },
            timer: timer_summary(m),
            irqs: irq_summary(m),
            last_frame: m.frames.last().copied(),
            state_hash: m.state_hash(),
            console_hash: fnv64(&m.console),
            issues,
            files_changed: m.drive.changed().map(String::from).collect(),
            contract_lines: m.contracts.as_deref().map(contract_lines).unwrap_or_default(),
        }
    }

    fn print(&self, m: &Machine) {
        println!("stop      {} at {} ms ({} cycles)", self.stop, self.elapsed_ms, self.cycles);
        println!("console   {} bytes: {}", m.console.len(), escape_tail(&m.console, 200));
        match self.last_frame {
            Some(f) => println!(
                "frames    {} ({} distinct), last: mode {:02X}h hash {:016x}",
                self.frames, self.distinct_frames, f.mode, f.hash
            ),
            None => println!("frames    0"),
        }
        if self.midi_bytes + self.speaker_events + self.opl_writes + self.pcm_frames > 0 {
            println!(
                "audio     {} OPL writes, {} Sound Blaster frames, {} MIDI bytes, {} PC speaker changes, hash {:016x}",
                self.opl_writes, self.pcm_frames, self.midi_bytes, self.speaker_events, self.audio_hash
            );
        }
        println!("timer     {}", self.timer);
        println!("irqs      {}", self.irqs);
        println!("state     {:016x}", self.state_hash);
        if !self.files_changed.is_empty() {
            println!("files     changed: {}", self.files_changed.join(", "));
        }
        for issue in &self.issues {
            println!("issue     {issue}");
        }
        for line in &self.contract_lines {
            println!("{line}");
        }
        if let Some(lines) = m.screen_text() {
            let last = lines.iter().rposition(|l| !l.is_empty()).map_or(0, |i| i + 1);
            if last > 0 {
                println!("screen");
                for line in &lines[..last] {
                    println!("  |{line}");
                }
            }
        }
    }

    fn json(&self) -> String {
        let strings =
            |items: &[String]| format!("[{}]", items.iter().map(|s| json_string(s)).collect::<Vec<_>>().join(","));
        let last_frame = self.last_frame.map_or("null".into(), |f| {
            format!(r#"{{"number":{},"cycle":{},"mode":{},"hash":"{:016x}"}}"#, f.number, f.cycle, f.mode, f.hash)
        });
        format!(
            r#"{{"stop":{},"exit_code":{},"cycles":{},"elapsed_ms":{},"frames":{},"distinct_frames":{},"midi_bytes":{},"speaker_events":{},"opl_writes":{},"pcm_frames":{},"audio_hash":"{:016x}","frames_hash":"{:016x}","last_frame":{},"state_hash":"{:016x}","console_hash":"{:016x}","issues":{},"files_changed":{}}}"#,
            json_string(&self.stop),
            self.exit_code.map_or("null".into(), |c| c.to_string()),
            self.cycles,
            self.elapsed_ms,
            self.frames,
            self.distinct_frames,
            self.midi_bytes,
            self.speaker_events,
            self.opl_writes,
            self.pcm_frames,
            self.audio_hash,
            self.frames_hash,
            last_frame,
            self.state_hash,
            self.console_hash,
            strings(&self.issues),
            strings(&self.files_changed),
        )
    }
}

/// Writes [`Machine::image`] to `dir`.
fn dump_image(m: &Machine, dir: &Path) -> Result<(), String> {
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

/// Why a run stopped, in words.
fn stop_text(stop: &Stop) -> String {
    match stop {
        Stop::Exited(code) => format!("exited with code {code}"),
        Stop::CycleLimit => "time limit reached".into(),
        Stop::WaitingForInput => "waiting for keyboard input".into(),
        Stop::Halted => "halted with interrupts disabled".into(),
        Stop::Fault(exit) => format!("fault: {exit}"),
        Stop::Sync { address, hit } => format!("sync point @{address:X}, hit {hit}"),
    }
}

/// What contract inference saw.
fn contract_lines(contracts: &dos::contracts::Contracts) -> Vec<String> {
    let calls: u64 = contracts.functions.values().map(|c| c.calls + c.abnormal).sum();
    vec![format!("contracts {} functions called, {calls} calls", contracts.functions.len())]
}

/// PIT channel 0 rate and the owners of the timer vectors (INT 08h, INT 1Ch).
fn timer_summary(m: &Machine) -> String {
    let channel = &m.devices.pit.channels[0];
    let hz = dos::clock::PIT_HZ as f64 / channel.period() as f64;
    let vector = |n: u32| (m.mem.read_u16(n * 4 + 2), m.mem.read_u16(n * 4));
    let ((s8, o8), (s1c, o1c)) = (vector(8), vector(0x1C));
    format!(
        "channel 0 mode {} at {hz:.1} Hz; INT 08h -> {s8:04X}:{o8:04X}, INT 1Ch -> {s1c:04X}:{o1c:04X}",
        channel.mode
    )
}

/// A watched entry point: CS, IP and the return address on the stack (segment, offset).
type EntryKey = (u16, u32, u16, u16);
/// How often it was entered, the first cycle, and AX and BX that first time.
type EntryStats = (usize, u64, u16, u16);

/// The watched range's entry points (instructions reached from outside the range, or first),
/// grouped with the far or near return address on the stack, most frequent first.
fn print_exec_hits(m: &Machine) {
    let hits = &m.exec_hits;
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

/// How often each IRQ line was delivered, and which lines the PICs mask now.
fn irq_summary(m: &Machine) -> String {
    let taken: Vec<String> =
        m.irq_counts.iter().enumerate().filter(|(_, n)| **n > 0).map(|(line, n)| format!("{line}: {n}")).collect();
    let imr = u16::from(m.devices.pic.imr()) | (u16::from(m.devices.pic2.imr()) << 8);
    let masked: Vec<String> = (0..16).filter(|line| imr & (1 << line) != 0).map(|line| line.to_string()).collect();
    format!(
        "taken {}; masked {}; vectors {:02X}h/{:02X}h",
        if taken.is_empty() { "none".into() } else { taken.join(", ") },
        if masked.is_empty() { "none".into() } else { masked.join(" ") },
        m.devices.pic.vector_base(),
        m.devices.pic2.vector_base()
    )
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The last `max` bytes, quoted, with control characters escaped.
fn escape_tail(bytes: &[u8], max: usize) -> String {
    let start = bytes.len().saturating_sub(max);
    let mut out = String::from(if start > 0 { "...\"" } else { "\"" });
    for &b in &bytes[start..] {
        match b {
            b'\r' => out.push_str("\\r"),
            b'\n' => out.push_str("\\n"),
            b'"' => out.push_str("\\\""),
            0x20..=0x7E => out.push(char::from(b)),
            _ => out.push_str(&format!("\\x{b:02X}")),
        }
    }
    out.push('"');
    out
}

/// An 8-bit RGB PNG, what image viewers and agents read.
fn png_image(width: usize, height: usize, rgb: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut raw = Vec::with_capacity((width * 3 + 1) * height);
    for line in rgb.chunks(width * 3).take(height) {
        raw.push(0); // filter: none
        raw.extend_from_slice(line);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&raw).expect("writing to memory");
    let data = z.finish().expect("writing to memory");
    let mut header = Vec::with_capacity(13);
    header.extend((width as u32).to_be_bytes());
    header.extend((height as u32).to_be_bytes());
    header.extend([8, 2, 0, 0, 0]); // 8 bits, RGB, deflate, no filtering choice, no interlace
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, body) in [(b"IHDR", &header[..]), (b"IDAT", &data[..]), (b"IEND", &[][..])] {
        out.extend((body.len() as u32).to_be_bytes());
        out.extend(kind);
        out.extend(body);
        let mut crc = crc32fast::Hasher::new();
        crc.update(kind);
        crc.update(body);
        out.extend(crc.finalize().to_be_bytes());
    }
    out
}

/// A 24-bit bottom-up BMP.
fn bmp(width: usize, height: usize, rgb: &[u8]) -> Vec<u8> {
    let row = (width * 3).div_ceil(4) * 4;
    let size = 54 + row * height;
    let mut out = Vec::with_capacity(size);
    out.extend(b"BM");
    out.extend((size as u32).to_le_bytes());
    out.extend([0; 4]);
    out.extend(54u32.to_le_bytes());
    out.extend(40u32.to_le_bytes());
    out.extend((width as i32).to_le_bytes());
    out.extend((height as i32).to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(24u16.to_le_bytes());
    out.extend([0; 24]);
    for y in (0..height).rev() {
        let line = &rgb[y * width * 3..(y + 1) * width * 3];
        for px in line.chunks(3) {
            out.extend([px[2], px[1], px[0]]);
        }
        out.extend(std::iter::repeat_n(0, row - width * 3));
    }
    out
}

/// `--sb`: a card type, optionally followed by BLASTER-style resources (A220 I7 D1 H5).
fn sound_blaster(spec: &str) -> Result<Option<dos::sblaster::SbConfig>, String> {
    use dos::sblaster::{SbConfig, SbType};
    let mut words = spec.split_whitespace();
    let kind = match words.next().unwrap_or("").to_ascii_lowercase().as_str() {
        "none" => return Ok(None),
        "sb1" => SbType::Sb1,
        "sb2" => SbType::Sb2,
        "sbpro1" => SbType::SbPro1,
        "sbpro2" => SbType::SbPro2,
        "sb16" => SbType::Sb16,
        other => return Err(format!("--sb: unknown card {other:?} (none, sb1, sb2, sbpro1, sbpro2, sb16)")),
    };
    let mut config = SbConfig { kind, ..SbConfig::default() };
    for word in words {
        let (key, number) = word.split_at(1);
        let bad = || format!("--sb: bad resource {word:?}");
        match key.to_ascii_uppercase().as_str() {
            "A" => config.base = u16::from_str_radix(number, 16).map_err(|_| bad())?,
            "I" => config.irq = number.parse().map_err(|_| bad())?,
            "D" => config.dma8 = number.parse().map_err(|_| bad())?,
            "H" => config.dma16 = number.parse().map_err(|_| bad())?,
            _ => return Err(bad()),
        }
    }
    Ok(Some(config))
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    #[test]
    fn a_png_screenshot_holds_the_pixels() {
        let rgb = [255, 0, 0, 0, 255, 0, 0, 0, 255, 1, 2, 3];
        let png = super::png_image(2, 2, &rgb);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..24], &[0, 0, 0, 2, 0, 0, 0, 2]);
        let idat = png.windows(4).position(|w| w == b"IDAT").expect("an IDAT chunk");
        let len = u32::from_be_bytes(png[idat - 4..idat].try_into().unwrap()) as usize;
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(&png[idat + 4..idat + 4 + len]).read_to_end(&mut raw).unwrap();
        assert_eq!(raw, [0, 255, 0, 0, 0, 255, 0, 0, 0, 0, 255, 1, 2, 3]);
        assert!(png.ends_with(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]));
    }
}
