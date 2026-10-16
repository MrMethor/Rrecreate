//! `rec-host play`: a window showing the emulated screen, with keyboard input.
//!
//! The emulator runs one 70 Hz frame of virtual time per window refresh. Keys are delivered
//! at the cycle where the emulator paused, which is what makes a recorded session replay
//! exactly.

use std::process::ExitCode;

use dos::{Machine, Stop};
use minifb::{Key, KeyRepeat, Scale, Window, WindowOptions};

use crate::Options;
use crate::session::Session;

const WIDTH: usize = 640;
const HEIGHT: usize = 400;

pub fn run(o: &Options) -> Result<ExitCode, String> {
    let program = o.program.clone().ok_or("play needs <program>")?;
    let ips = o.ips.unwrap_or(8_000_000);
    let mut m = crate::boot(&o.game_dir, &program, &o.args, ips, o.cpu)?;
    m.stop_on_input_wait = false;

    let scale = match o.scale {
        0..=2 => Scale::X1,
        3..=4 => Scale::X2,
        _ => Scale::X4,
    };
    let title = format!("rec-host: {program} (close the window to quit)");
    let mut window = Window::new(&title, WIDTH, HEIGHT, WindowOptions { scale, ..WindowOptions::default() })
        .map_err(|e| format!("can't open a window: {e}"))?;
    window.set_target_fps(70);

    let cycles_per_frame = (ips / 70).max(1);
    let mut buffer = vec![0u32; WIDTH * HEIGHT];
    let mut recorded = Vec::new();
    let mut last_text: Option<Vec<String>> = None;
    let mut stop = None;

    while window.is_open() {
        let now = m.cpu.cycles;
        let pressed = window.get_keys_pressed(KeyRepeat::No).into_iter().filter_map(|k| scancodes(k, true));
        let released = window.get_keys_released().into_iter().filter_map(|k| scancodes(k, false));
        for codes in pressed.chain(released) {
            m.queue_scancodes(now, &codes);
            recorded.extend(codes.iter().map(|&c| (now, c)));
        }
        match m.run(cycles_per_frame) {
            Stop::CycleLimit | Stop::WaitingForInput => {}
            other => {
                stop = Some(other);
                break;
            }
        }
        render(&m, &mut buffer);
        window.update_with_buffer(&buffer, WIDTH, HEIGHT).map_err(|e| e.to_string())?;
        // Text modes are drawn without a font, so mirror the text to the terminal.
        let text = m.screen_text();
        if text.is_some() && text != last_text {
            print_text(text.as_deref().unwrap_or_default());
        }
        last_text = text;
    }

    match &stop {
        Some(stop) => eprintln!("rec-host: program stopped: {stop:?}"),
        None => eprintln!("rec-host: window closed after {} ms of emulated time", m.elapsed_ms()),
    }
    for issue in &m.diagnostics {
        eprintln!("rec-host: issue: {issue}");
    }
    if let Some(path) = &o.record {
        let session = Session { program, args: o.args.clone(), instructions_per_second: ips, inputs: recorded };
        session.save(path)?;
        eprintln!("rec-host: recorded {} scancodes to {}", session.inputs.len(), path.display());
    }
    Ok(if matches!(stop, Some(Stop::Fault(_))) { ExitCode::from(2) } else { ExitCode::SUCCESS })
}

fn print_text(lines: &[String]) {
    let last = lines.iter().rposition(|l| !l.is_empty()).map_or(0, |i| i + 1);
    println!("{}", "-".repeat(80));
    for line in &lines[..last] {
        println!("{line}");
    }
}

fn rgb(color: [u8; 3]) -> u32 {
    let expand = |c: u8| u32::from((c << 2) | (c >> 4));
    (expand(color[0]) << 16) | (expand(color[1]) << 8) | expand(color[2])
}

fn render(m: &Machine, buffer: &mut [u32]) {
    if let Some((w, h, pixels)) = m.screen_rgb() {
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let i = ((y * h / HEIGHT) * w + x * w / WIDTH) * 3;
                buffer[y * WIDTH + x] =
                    (u32::from(pixels[i]) << 16) | (u32::from(pixels[i + 1]) << 8) | u32::from(pixels[i + 2]);
            }
        }
        return;
    }
    let Some(lines) = m.screen_text() else {
        buffer.fill(0);
        return;
    };
    // Text mode without a font: each cell shows its background, and non-blank characters a
    // foreground-colored bar, which is enough to see layout and colors.
    let vga = &m.devices.vga;
    let columns = lines.first().map_or(80, |_| if vga.mode < 2 { 40 } else { 80 });
    let cell_w = WIDTH / columns;
    let cells = m.mem.slice(dos::vga::TEXT_BASE, columns * 25 * 2);
    for (index, cell) in cells.chunks(2).enumerate() {
        let (ch, attr) = (cell[0], cell[1]);
        let color = |i: u8| rgb(vga.dac[usize::from(vga.attr[usize::from(i & 0x0F)])]);
        let (fg, bg) = (color(attr & 0x0F), color((attr >> 4) & 0x07));
        let (cx, cy) = (index % columns * cell_w, index / columns * 16);
        for y in 0..16 {
            for x in 0..cell_w {
                let glyph = ch != b' ' && ch != 0 && (4..12).contains(&y) && (1..cell_w - 1).contains(&x);
                buffer[(cy + y) * WIDTH + cx + x] = if glyph { fg } else { bg };
            }
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
