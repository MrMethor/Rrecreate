//! The options of `run` and `play` (and of `serve`'s `load`), and the usage text.

use std::path::PathBuf;

use crate::input_script;

pub(crate) const USAGE: &str = "\
usage:
  clockwork run  <game-dir> <program> [options]  run headless and print a summary
  clockwork play <game-dir> <program> [options]  run in a window with keyboard input
  clockwork unpack <program> --out FILE [--json] undo an executable packer (PKLITE, LZEXE, ...) by
                                                 running it to its entry point; writes an MZ file
  clockwork serve [--port N]                     be driven by a tool: JSON requests, one per line,
                                                 on stdin or a local TCP port (see serve.rs)
  clockwork ask --port N REQUEST...              send requests (JSON, or OP for {\"op\": OP}) to a
                                                 serve on that port and print the answers: its
                                                 machine stays between asks
  clockwork gdb [--port N] <game-dir> <program> [options]
                                                 the program stopped at its entry, for gdb or
                                                 Ghidra's debugger on 127.0.0.1:N (1234; gdb.rs)
  clockwork listing <image> --entry OFFSET --segment SEG
  clockwork listing <file.elf> --entry LINEAR
                                                 a function's code from a load image (hex image
                                                 offset and link segment), following its control
                                                 flow and jump tables, in image offsets; or a flat
                                                 program's 32-bit code from the ELF file unpack
                                                 wrote, in linear addresses

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
                     {motion DX DY} (mickeys), {stick X Y BUTTONS} {stick2 ...}, {wait MS}; one step
                     after the other, keys 120 ms apart; MS counts from power-on
  --input-file FILE  run: --input lines from FILE (# starts a comment)
  --replay FILE      run: replay a session recorded by play (program may be omitted)
  --screenshot FILE  run: save the final screen as a PNG (FILE.png) or a BMP
  --wav FILE         run: save the sound (OPL, Sound Blaster, PC speaker, CD audio) as a WAV
                     file; after --load-snapshot, from the snapshot on
  --coverage FILE    run: record which instructions ran and the calls made, in any mode and
                     anywhere (JSON, linear addresses, with the program's image)
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
pub(crate) struct Options {
    pub(crate) game_dir: PathBuf,
    pub(crate) program: Option<String>,
    pub(crate) args: String,
    pub(crate) ips: Option<u64>,
    pub(crate) seconds: Option<f64>,
    pub(crate) type_text: Option<String>,
    pub(crate) inputs: Vec<input_script::Script>,
    pub(crate) type_at_ms: u64,
    pub(crate) replay: Option<PathBuf>,
    pub(crate) screenshot: Option<PathBuf>,
    pub(crate) wav: Option<PathBuf>,
    pub(crate) save_changes: Option<PathBuf>,
    pub(crate) save_snapshot: Option<PathBuf>,
    pub(crate) dump_image: Option<PathBuf>,
    pub(crate) load_snapshot: Option<PathBuf>,
    pub(crate) coverage: Option<PathBuf>,
    pub(crate) contracts: Option<PathBuf>,
    pub(crate) sound_blaster: Option<Option<dos::sblaster::SbConfig>>,
    pub(crate) json: bool,
    pub(crate) record: Option<PathBuf>,
    pub(crate) no_audio: bool,
    pub(crate) midi_out: Option<String>,
    pub(crate) scale: usize,
    pub(crate) cpu: Option<dos::CpuModel>,
    pub(crate) cd_drive: Option<char>,
    pub(crate) cd_root: String,
    pub(crate) cd_label: String,
    pub(crate) start_drive: Option<char>,
    pub(crate) start_dir: String,
    pub(crate) environment: Vec<(String, String)>,
    pub(crate) ems_kb: Option<u32>,
    pub(crate) memory_mb: Option<u32>,
    pub(crate) no_xms: bool,
    pub(crate) no_fpu: bool,
    pub(crate) dpmi: bool,
    pub(crate) joysticks: u8,
    pub(crate) hotspots: usize,
    pub(crate) disasm: Option<String>,
    pub(crate) dump: Option<String>,
    pub(crate) trace: Option<PathBuf>,
    pub(crate) midi: Option<PathBuf>,
    pub(crate) opl: Option<PathBuf>,
    pub(crate) watch_exec: Option<String>,
    pub(crate) watch_log: Option<PathBuf>,
}

pub(crate) fn parse_options(args: &[String]) -> Result<Options, String> {
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

/// `SEG:OFF` in hex; the offset may be 32-bit (protected mode).
pub(crate) fn parse_address(text: &str) -> Result<(u16, u32), String> {
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
