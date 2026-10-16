//! `rec-host`: runs a DOS program on the Recreate host (PROJECT.md §5.2).
//!
//! `run` is headless and prints a short summary (or JSON) meant for tools and agents.
//! `play` opens a window, takes keyboard input, and can record it as a session that `run
//! --replay` reproduces bit for bit.

mod hotspots;
mod play;
mod session;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;

use dos::{Config, Machine, Stop, Vfs};
use machine::hash::fnv64;

use crate::session::Session;

const USAGE: &str = "\
usage:
  rec-host run  <game-dir> <program> [options]   run headless and print a summary
  rec-host play <game-dir> <program> [options]   run in a window with keyboard input

options:
  --args TEXT        command line passed to the program
  --ips N            emulated instructions per second (default 8000000)
  --seconds N        run: emulated seconds to run (default 10)
  --type TEXT        run: type TEXT on the keyboard (\\r = Enter, \\e = Esc)
  --type-at MS       run: when typing starts, in emulated ms (default 500)
  --replay FILE      run: replay a session recorded by play (program may be omitted)
  --screenshot FILE  run: save the final mode 13h screen as a BMP
  --json             run: print the summary as JSON
  --hotspots N       run: sample CS:IP while running and show the N hottest addresses
  --trace FILE       run: write every BIOS/DOS/EMS service call to FILE
  --midi FILE        run: write the MIDI bytes sent to the MPU-401, one line per PIT tick
  --disasm SEG:OFF   run: afterwards, show registers and disassemble 40 instructions there
                     (or: here, for the current CS:IP)
  --record FILE      play: record keyboard input as a session
  --cpu 286|386      processor the program sees (default 386)
  --cd LETTER        add a CD-ROM drive (with MSCDEX) showing the same files as C:
  --drive LETTER     the current drive when the program starts (default C)
  --env NAME=VALUE   set an environment variable (repeatable)
  --ems KB           expanded memory size (default 4096; 0 disables EMS)
  --scale N          play: window scale (default 2)";

#[derive(Default)]
struct Options {
    game_dir: PathBuf,
    program: Option<String>,
    args: String,
    ips: Option<u64>,
    seconds: f64,
    type_text: Option<String>,
    type_at_ms: u64,
    replay: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    json: bool,
    record: Option<PathBuf>,
    scale: usize,
    cpu: dos::CpuModel,
    cd_drive: Option<char>,
    start_drive: Option<char>,
    environment: Vec<(String, String)>,
    ems_kb: Option<u32>,
    hotspots: usize,
    disasm: Option<String>,
    trace: Option<PathBuf>,
    midi: Option<PathBuf>,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("run") => parse_options(&args[1..]).and_then(|o| cmd_run(&o)),
        Some("play") => parse_options(&args[1..]).and_then(|o| play::run(&o)),
        Some("--version") => {
            println!("rec-host {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(1);
        }
    };
    result.unwrap_or_else(|message| {
        eprintln!("rec-host: error: {message}");
        ExitCode::from(1)
    })
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut options = Options { seconds: 10.0, type_at_ms: 500, scale: 2, ..Options::default() };
    let mut positional = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |name: &str| iter.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--args" => options.args = value(arg)?,
            "--ips" => options.ips = Some(value(arg)?.parse().map_err(|_| "--ips needs a whole number")?),
            "--seconds" => options.seconds = value(arg)?.parse().map_err(|_| "--seconds needs a number")?,
            "--type" => options.type_text = Some(value(arg)?.replace("\\r", "\r").replace("\\e", "\x1b")),
            "--type-at" => options.type_at_ms = value(arg)?.parse().map_err(|_| "--type-at needs milliseconds")?,
            "--replay" => options.replay = Some(value(arg)?.into()),
            "--screenshot" => options.screenshot = Some(value(arg)?.into()),
            "--json" => options.json = true,
            "--trace" => options.trace = Some(value(arg)?.into()),
            "--midi" => options.midi = Some(value(arg)?.into()),
            "--disasm" => options.disasm = Some(value(arg)?),
            "--hotspots" => options.hotspots = value(arg)?.parse().map_err(|_| "--hotspots needs a count")?,
            "--record" => options.record = Some(value(arg)?.into()),
            "--scale" => options.scale = value(arg)?.parse().map_err(|_| "--scale needs a whole number")?,
            "--ems" => options.ems_kb = Some(value(arg)?.parse().map_err(|_| "--ems needs kilobytes")?),
            "--cd" => options.cd_drive = Some(drive_letter(&value(arg)?)?),
            "--drive" => options.start_drive = Some(drive_letter(&value(arg)?)?),
            "--env" => {
                let assignment = value(arg)?;
                let (name, text) = assignment.split_once('=').ok_or("--env needs NAME=VALUE")?;
                options.environment.push((name.to_ascii_uppercase(), text.to_owned()));
            }
            "--cpu" => {
                options.cpu = match value(arg)?.as_str() {
                    "286" => dos::CpuModel::I286,
                    "386" => dos::CpuModel::I386,
                    other => return Err(format!("--cpu must be 286 or 386, not {other:?}")),
                }
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
    if options.program.is_none() && options.replay.is_none() {
        return Err("missing <program>".into());
    }
    if !options.seconds.is_finite() || options.seconds <= 0.0 || options.scale == 0 {
        return Err("--seconds and --scale must be positive".into());
    }
    Ok(options)
}

fn parse_address(text: &str) -> Result<(u16, u16), String> {
    let (segment, offset) = text.split_once(':').ok_or_else(|| format!("expected SEG:OFF in hex, not {text:?}"))?;
    let hex = |part: &str| u16::from_str_radix(part, 16).map_err(|_| format!("bad hex number {part:?}"));
    Ok((hex(segment)?, hex(offset)?))
}

fn drive_letter(text: &str) -> Result<char, String> {
    match text.trim_end_matches(':').chars().collect::<Vec<_>>()[..] {
        [letter] if letter.is_ascii_alphabetic() => Ok(letter.to_ascii_uppercase()),
        _ => Err(format!("expected a drive letter, not {text:?}")),
    }
}

/// Loads the game folder as drive C: and the program into a fresh machine.
fn boot(o: &Options, program: &str, args: &str, ips: u64) -> Result<Machine, String> {
    let game_dir = o.game_dir.as_path();
    let (drive, warnings) = Vfs::from_host_dir(game_dir).map_err(|e| format!("{}: {e}", game_dir.display()))?;
    for warning in warnings.iter().take(5) {
        eprintln!("rec-host: warning: {warning}");
    }
    if warnings.len() > 5 {
        eprintln!("rec-host: warning: ... {} more", warnings.len() - 5);
    }
    let config = Config {
        instructions_per_second: ips,
        cpu_model: o.cpu,
        cd_drive: o.cd_drive,
        start_drive: o.start_drive.unwrap_or('C'),
        environment: o.environment.clone(),
        ems_kb: o.ems_kb.unwrap_or(Config::default().ems_kb),
        ..Config::default()
    };
    Machine::with_program(config, drive, program, args).map_err(|e| e.to_string())
}

fn cmd_run(o: &Options) -> Result<ExitCode, String> {
    let session = o.replay.as_deref().map(Session::load).transpose()?;
    let (program, args, ips) = match &session {
        Some(s) => (s.program.clone(), s.args.clone(), o.ips.unwrap_or(s.instructions_per_second)),
        None => (o.program.clone().unwrap_or_default(), o.args.clone(), o.ips.unwrap_or(8_000_000)),
    };
    let mut m = boot(o, &program, &args, ips)?;
    if let Some(session) = &session {
        for &(cycle, input) in &session.inputs {
            m.queue_input(cycle, input);
        }
    }
    if let Some(text) = &o.type_text {
        let start = m.cycle_at_ms(o.type_at_ms);
        m.type_text(start, text, m.cycle_at_ms(60));
    }
    if o.trace.is_some() {
        m.service_trace = Some(Vec::new());
    }
    let limit = (o.seconds * ips as f64) as u64;
    let (stop, profile) = if o.hotspots > 0 {
        let (stop, profile) = hotspots::run_sampled(&mut m, limit);
        (stop, Some(profile))
    } else {
        (m.run(limit), None)
    };

    if let Some(path) = &o.screenshot {
        match m.screen_rgb() {
            Some((w, h, rgb)) => {
                std::fs::write(path, bmp(w, h, &rgb)).map_err(|e| format!("{}: {e}", path.display()))?
            }
            None => eprintln!("rec-host: warning: no screenshot: not in mode 13h"),
        }
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
    if let (Some(path), Some(calls)) = (&o.trace, &m.service_trace) {
        let text: String = calls.iter().map(|call| format!("{call}\n")).collect();
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("trace     {} service calls written to {}", calls.len(), path.display());
    }
    if let Some(at) = &o.disasm {
        let (cs, ip) = if at == "here" { (m.cpu.regs.cs(), m.cpu.regs.ip) } else { parse_address(at)? };
        hotspots::print_listing(&m, cs, ip, 40);
    }
    if o.json {
        println!("{}", summary.json());
    } else {
        summary.print(&m);
    }
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
    last_frame: Option<dos::Frame>,
    state_hash: u64,
    console_hash: u64,
    issues: Vec<String>,
    files_changed: Vec<String>,
}

impl Summary {
    fn new(m: &Machine, stop: &Stop) -> Self {
        let stop_text = match stop {
            Stop::Exited(code) => format!("exited with code {code}"),
            Stop::CycleLimit => "time limit reached".into(),
            Stop::WaitingForInput => "waiting for keyboard input".into(),
            Stop::Halted => "halted with interrupts disabled".into(),
            Stop::Fault(exit) => format!("fault: {exit}"),
        };
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
            last_frame: m.frames.last().copied(),
            state_hash: m.state_hash(),
            console_hash: fnv64(&m.console),
            issues,
            files_changed: m.drive.changed().map(String::from).collect(),
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
        if self.midi_bytes > 0 || self.speaker_events > 0 {
            println!("audio     {} MIDI bytes, {} PC speaker changes", self.midi_bytes, self.speaker_events);
        }
        println!("state     {:016x}", self.state_hash);
        if !self.files_changed.is_empty() {
            println!("files     changed: {}", self.files_changed.join(", "));
        }
        for issue in &self.issues {
            println!("issue     {issue}");
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
            r#"{{"stop":{},"exit_code":{},"cycles":{},"elapsed_ms":{},"frames":{},"distinct_frames":{},"midi_bytes":{},"speaker_events":{},"last_frame":{},"state_hash":"{:016x}","console_hash":"{:016x}","issues":{},"files_changed":{}}}"#,
            json_string(&self.stop),
            self.exit_code.map_or("null".into(), |c| c.to_string()),
            self.cycles,
            self.elapsed_ms,
            self.frames,
            self.distinct_frames,
            self.midi_bytes,
            self.speaker_events,
            last_frame,
            self.state_hash,
            self.console_hash,
            strings(&self.issues),
            strings(&self.files_changed),
        )
    }
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
