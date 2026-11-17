//! `clockwork play`: a window showing the emulated screen, with keyboard input and live sound.
//!
//! The emulator runs about one 70 Hz frame of virtual time per window refresh, a little more or
//! less to keep pace with the sound output ([`crate::live_audio`]). Keys are delivered at the
//! cycle where the emulator paused, which is what makes a recorded session replay exactly.
//!
//! From a snapshot (`--load-snapshot`, a session's tick), the input it still had to come is dropped but the
//! releases of keys held then, as for a session started there (D118): the person's play takes its place.
//! `--record` writes the play as it goes ([`Recording`]), so a tool can follow it.
//!
//! `--headless FILE` is the same loop without a window (D189), for tests and tools: a recorded play's input (or an
//! input script's, `MS:KEYS` lines as `--input-file` reads them, D192) given as the window gives what the person does,
//! at the first frame that reaches its cycle and at that frame's cycle, recorded as the window records it, as fast as
//! the machine runs.

use std::path::Path;
use std::process::ExitCode;

use dos::{Input, Machine, Stop};
use minifb::{Key, KeyRepeat, MouseButton, MouseMode, Scale, Window, WindowOptions};

use crate::gamepads::Gamepads;
use crate::live_audio::LiveAudio;
use crate::options::Options;
use crate::session::Recording;

const WIDTH: usize = 640;
const HEIGHT: usize = 400;

pub fn run(o: &Options) -> Result<ExitCode, String> {
    let (mut m, program, ips) = match &o.load_snapshot {
        Some(_) => {
            let (mut m, program, ips) = crate::run::prepare(o)?;
            m.replace_input();
            // A recording's snapshot brings its call tracker (for watches from it); the window needs none, and it
            // cost a third of each frame's emulation (M6.5's step 0, D186).
            m.track_calls(false);
            (m, program, ips)
        }
        None => {
            let program = o.program.clone().ok_or("play needs <program>")?;
            let ips = o.ips.unwrap_or(8_000_000);
            (crate::run::boot(o, &program, &o.args, ips)?, program, ips)
        }
    };
    m.stop_on_input_wait = false;
    let mut recording = match &o.record {
        Some(path) => {
            let source = env!("CLOCKWORK_SOURCE");
            let mut recording = Recording::create(path, &program, &o.args, ips, m.cpu.cycles, source)?;
            if let Some(seconds) = o.hash_every {
                recording.hash_every((seconds * ips as f64) as u64);
            }
            Some(recording)
        }
        None => None,
    };
    let limit = o.seconds.map(|s| (s * ips as f64) as u64);
    let stop = match &o.headless {
        Some(source) => headless(&mut m, source, &mut recording, ips, limit)?,
        None => windowed(o, &mut m, &program, ips, &mut recording, limit)?,
    };

    match &stop {
        Some(stop) => eprintln!("clockwork: program stopped: {stop:?}"),
        None => eprintln!("clockwork: stopped after {} ms of emulated time", m.elapsed_ms()),
    }
    for issue in &m.diagnostics {
        eprintln!("clockwork: issue: {issue}");
    }
    if let (Some(recording), Some(path)) = (recording, &o.record) {
        let inputs = recording.end(m.cpu.cycles)?;
        eprintln!("clockwork: recorded {inputs} input events to {}", path.display());
    }
    Ok(if matches!(stop, Some(Stop::Fault(_))) { ExitCode::from(2) } else { ExitCode::SUCCESS })
}

/// The window's loop, till it closes, the time limit or the program stops (that stop).
fn windowed(
    o: &Options,
    m: &mut Machine,
    program: &str,
    ips: u64,
    recording: &mut Option<Recording>,
    limit: Option<u64>,
) -> Result<Option<Stop>, String> {
    let scale = match o.scale {
        0..=2 => Scale::X1,
        3..=4 => Scale::X2,
        _ => Scale::X4,
    };
    let title = format!("clockwork: {program} (close the window to quit)");
    let mut window = Window::new(&title, WIDTH, HEIGHT, WindowOptions { scale, ..WindowOptions::default() })
        .map_err(|e| format!("can't open a window: {e}"))?;
    window.set_target_fps(70);
    window.set_cursor_visibility(false); // the program draws its own cursor

    let cycles_per_frame = (ips / 70).max(1);
    let mut audio = (!o.no_audio).then(|| LiveAudio::start(o.midi_out.clone()));
    let mut gamepads = (o.joysticks > 0).then(|| Gamepads::start(o.joysticks)).flatten();
    let mut buffer = vec![0u32; WIDTH * HEIGHT];
    let record = |recording: &mut Option<Recording>, cycle: u64, input: Input| -> Result<(), String> {
        recording.as_mut().map_or(Ok(()), |r| r.input(cycle, &input))
    };
    let mut last_mouse = None;
    while window.is_open() && limit.is_none_or(|limit| m.cpu.cycles < limit) {
        let now = m.cpu.cycles;
        // The machine's state now, before this frame's input, for a follower to prove its run this one (D189).
        if let Some(recording) = recording.as_mut().filter(|r| r.hash_due(now)) {
            recording.hash(now, m.state_hash())?;
        }
        let pressed = window.get_keys_pressed(KeyRepeat::No).into_iter().filter_map(|k| scancodes(k, true));
        let released = window.get_keys_released().into_iter().filter_map(|k| scancodes(k, false));
        for codes in pressed.chain(released) {
            m.queue_scancodes(now, &codes);
            for &code in &codes {
                record(recording, now, Input::Key(code))?;
            }
        }
        if let Some(mouse) = mouse_input(&window, m)
            && Some(mouse) != last_mouse
        {
            m.queue_input(now, mouse);
            record(recording, now, mouse)?;
            last_mouse = Some(mouse);
        }
        for input in gamepads.iter_mut().flat_map(Gamepads::changes) {
            m.queue_input(now, input);
            record(recording, now, input)?;
        }
        // Every input up to now is written.
        if let Some(recording) = recording.as_mut() {
            recording.at(now)?;
        }
        let cycles = audio.as_ref().map_or(cycles_per_frame, |a| a.frame_cycles(cycles_per_frame));
        match m.run(cycles) {
            Stop::CycleLimit | Stop::WaitingForInput => {}
            other => return Ok(Some(other)),
        }
        if let Some(audio) = &mut audio {
            audio.update(m);
        }
        render(m, &mut buffer);
        window.update_with_buffer(&buffer, WIDTH, HEIGHT).map_err(|e| e.to_string())?;
    }
    Ok(None)
}

/// `--headless FILE`: the window's loop without a window, the input of the play in `FILE` given as the window gives
/// the person's (module docs); till the time limit or the program stops (that stop).
fn headless(
    m: &mut Machine,
    source: &Path,
    recording: &mut Option<Recording>,
    ips: u64,
    limit: Option<u64>,
) -> Result<Option<Stop>, String> {
    let text = std::fs::read_to_string(source).map_err(|e| format!("{}: {e}", source.display()))?;
    let events = if text.starts_with("# clockwork session") {
        crate::session::Session::load(source)?.inputs
    } else {
        // An input script: its events at their cycles, in time order (the same time: the script's order).
        let mut events = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            events.extend(crate::input_script::Script::parse(line)?.events(&|ms| m.cycle_at_ms(ms)));
        }
        events.sort_by_key(|&(cycle, _)| cycle);
        events
    };
    let mut next = 0;
    let cycles_per_frame = (ips / 70).max(1);
    while limit.is_none_or(|limit| m.cpu.cycles < limit) {
        let now = m.cpu.cycles;
        if let Some(recording) = recording.as_mut().filter(|r| r.hash_due(now)) {
            recording.hash(now, m.state_hash())?;
        }
        while let Some(&(_, input)) = events.get(next).filter(|&&(cycle, _)| cycle <= now) {
            m.queue_input(now, input);
            if let Some(recording) = recording.as_mut() {
                recording.input(now, &input)?;
            }
            next += 1;
        }
        if let Some(recording) = recording.as_mut() {
            recording.at(now)?;
        }
        match m.run(cycles_per_frame) {
            Stop::CycleLimit | Stop::WaitingForInput => {}
            other => return Ok(Some(other)),
        }
    }
    Ok(None)
}

/// The window's mouse, mapped onto the driver's coordinate range.
fn mouse_input(window: &Window, m: &Machine) -> Option<Input> {
    let (px, py) = window.get_mouse_pos(MouseMode::Clamp)?;
    let mouse = &m.mouse;
    let scale = |pixel: f32, size: usize, min: i32, max: i32| {
        let span = (max - min + 1) as f32;
        (min + (pixel / size as f32 * span) as i32).clamp(min, max) as i16
    };
    let buttons = u8::from(window.get_mouse_down(MouseButton::Left))
        | (u8::from(window.get_mouse_down(MouseButton::Right)) << 1)
        | (u8::from(window.get_mouse_down(MouseButton::Middle)) << 2);
    Some(Input::Mouse {
        x: scale(px, WIDTH, mouse.min_x, mouse.max_x),
        y: scale(py, HEIGHT, mouse.min_y, mouse.max_y),
        buttons,
    })
}

/// Scales the emulated screen to the window.
fn render(m: &Machine, buffer: &mut [u32]) {
    let (w, h, pixels) = m.screen_rgb();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let i = ((y * h / HEIGHT) * w + x * w / WIDTH) * 3;
            buffer[y * WIDTH + x] =
                (u32::from(pixels[i]) << 16) | (u32::from(pixels[i + 1]) << 8) | u32::from(pixels[i + 2]);
        }
    }
}

/// Set 1 make (or break) codes for a key, with the E0 prefix for extended keys.
fn scancodes(key: Key, pressed: bool) -> Option<Vec<u8>> {
    let (code, extended) = match key {
        Key::Escape => (0x01, false),
        Key::Key1 => (0x02, false),
        Key::Key2 => (0x03, false),
        Key::Key3 => (0x04, false),
        Key::Key4 => (0x05, false),
        Key::Key5 => (0x06, false),
        Key::Key6 => (0x07, false),
        Key::Key7 => (0x08, false),
        Key::Key8 => (0x09, false),
        Key::Key9 => (0x0A, false),
        Key::Key0 => (0x0B, false),
        Key::Minus => (0x0C, false),
        Key::Equal => (0x0D, false),
        Key::Backspace => (0x0E, false),
        Key::Tab => (0x0F, false),
        Key::Q => (0x10, false),
        Key::W => (0x11, false),
        Key::E => (0x12, false),
        Key::R => (0x13, false),
        Key::T => (0x14, false),
        Key::Y => (0x15, false),
        Key::U => (0x16, false),
        Key::I => (0x17, false),
        Key::O => (0x18, false),
        Key::P => (0x19, false),
        Key::LeftBracket => (0x1A, false),
        Key::RightBracket => (0x1B, false),
        Key::Enter => (0x1C, false),
        Key::LeftCtrl => (0x1D, false),
        Key::A => (0x1E, false),
        Key::S => (0x1F, false),
        Key::D => (0x20, false),
        Key::F => (0x21, false),
        Key::G => (0x22, false),
        Key::H => (0x23, false),
        Key::J => (0x24, false),
        Key::K => (0x25, false),
        Key::L => (0x26, false),
        Key::Semicolon => (0x27, false),
        Key::Apostrophe => (0x28, false),
        Key::Backquote => (0x29, false),
        Key::LeftShift => (0x2A, false),
        Key::Backslash => (0x2B, false),
        Key::Z => (0x2C, false),
        Key::X => (0x2D, false),
        Key::C => (0x2E, false),
        Key::V => (0x2F, false),
        Key::B => (0x30, false),
        Key::N => (0x31, false),
        Key::M => (0x32, false),
        Key::Comma => (0x33, false),
        Key::Period => (0x34, false),
        Key::Slash => (0x35, false),
        Key::RightShift => (0x36, false),
        Key::NumPadAsterisk => (0x37, false),
        Key::LeftAlt => (0x38, false),
        Key::Space => (0x39, false),
        Key::CapsLock => (0x3A, false),
        Key::F1 => (0x3B, false),
        Key::F2 => (0x3C, false),
        Key::F3 => (0x3D, false),
        Key::F4 => (0x3E, false),
        Key::F5 => (0x3F, false),
        Key::F6 => (0x40, false),
        Key::F7 => (0x41, false),
        Key::F8 => (0x42, false),
        Key::F9 => (0x43, false),
        Key::F10 => (0x44, false),
        Key::NumLock => (0x45, false),
        Key::ScrollLock => (0x46, false),
        Key::NumPad7 => (0x47, false),
        Key::NumPad8 => (0x48, false),
        Key::NumPad9 => (0x49, false),
        Key::NumPadMinus => (0x4A, false),
        Key::NumPad4 => (0x4B, false),
        Key::NumPad5 => (0x4C, false),
        Key::NumPad6 => (0x4D, false),
        Key::NumPadPlus => (0x4E, false),
        Key::NumPad1 => (0x4F, false),
        Key::NumPad2 => (0x50, false),
        Key::NumPad3 => (0x51, false),
        Key::NumPad0 => (0x52, false),
        Key::NumPadDot => (0x53, false),
        Key::F11 => (0x57, false),
        Key::F12 => (0x58, false),
        Key::NumPadEnter => (0x1C, true),
        Key::RightCtrl => (0x1D, true),
        Key::NumPadSlash => (0x35, true),
        Key::RightAlt => (0x38, true),
        Key::Home => (0x47, true),
        Key::Up => (0x48, true),
        Key::PageUp => (0x49, true),
        Key::Left => (0x4B, true),
        Key::Right => (0x4D, true),
        Key::End => (0x4F, true),
        Key::Down => (0x50, true),
        Key::PageDown => (0x51, true),
        Key::Insert => (0x52, true),
        Key::Delete => (0x53, true),
        _ => return None,
    };
    let code = if pressed { code } else { code | 0x80 };
    Some(if extended { vec![0xE0, code] } else { vec![code] })
}
