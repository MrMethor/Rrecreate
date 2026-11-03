//! The platform layer (DESIGN.md §6.1, D119): what a Rust port plays on, outside a comparison. It owns the main
//! loop: a window that shows the presentation's frames, the host's keyboard and mouse given to the port as the
//! PC's (the keyboard controller's bytes, the mouse in the screen's coordinates), and the sound the presentation
//! makes, played (the OPL3 by Nuked OPL3, the PC speaker, digitized sound). A port's [`Live`] turns those
//! devices into each tick's boundary values as the original's interrupt handlers and timer did, and says when
//! the original's loop would go on. The wall clock is read here, and only here: the simulation steps as it
//! does in a comparison, and the presentation draws and sounds as it does there.
//!
//! ```no_run
//! # use rrc_platform::{Host, KeyStates, Live, Next};
//! # #[derive(serde::Serialize, serde::Deserialize)] struct Game;
//! # impl rrc_runtime::Simulation for Game { fn start() -> Game { Game } fn step(&mut self, _: &rrc_runtime::Tick) {} }
//! # impl rrc_runtime::present::Shows for Game { type Scene<'a> = (); fn scene(&self) {} }
//! #[derive(Default)]
//! struct Devices { keys: KeyStates, last: u16 }
//!
//! impl Live for Game {
//!     type Devices = Devices;
//!     // INT 09h keeps the keys by scan code; INT 08h counts at 70 Hz, and the loop waits for its count to move.
//!     fn next(&self, d: &mut Devices, host: &Host) -> Next {
//!         d.keys.take(&host.scancodes);
//!         let count = rrc_platform::timer_count(host.ms, 70) as u16;
//!         if count == d.last {
//!             return Next::Wait;
//!         }
//!         d.last = count;
//!         Next::Tick("play".into(), rrc_platform::inputs(&[("timer_count", &count.to_le_bytes()), ("key_down", &d.keys.0)]))
//!     }
//! }
//! ```

mod audio;
mod keys;

use std::collections::BTreeMap;
use std::time::Instant;

use minifb::{MouseButton, MouseMode, Scale, ScaleMode, Window, WindowOptions};
use rrc_runtime::present::{Frame, Presentation, Shows, Sounds};
use rrc_runtime::{Inputs, Simulation, Started, Tick};

/// The screen the presentation draws: VGA's 320 × 200.
const WIDTH: usize = 320;
const HEIGHT: usize = 200;
/// Ticks run at most in one of the window's frames, catching up after a stall.
const CATCH_UP: usize = 8;

/// The host's devices as the PC's, when the platform asks what the original's loop does next.
#[derive(Debug, Clone, Default)]
pub struct Host {
    /// Milliseconds since play began.
    pub ms: u64,
    /// The keyboard controller's bytes since the last tick, in order (scan code set 1: a key's code when it is
    /// pressed, with 80h when it is let go; a grey key's after E0h).
    pub scancodes: Vec<u8>,
    /// The mouse in the screen's coordinates (none off the window), and the buttons held (bit 0 left, 1 right,
    /// 2 middle).
    pub mouse: Option<(i32, i32)>,
    pub buttons: u8,
}

/// What the original's loop does next.
pub enum Next {
    /// A pass from the sync point named so, with these boundary values.
    Tick(String, Inputs),
    /// It still waits (its timer hasn't come).
    Wait,
    /// It ended: the program exits.
    End,
}

/// A port played live: its simulation, and the original's input handling, which a comparison gives it from
/// the original's memory instead (the boundary values at each sync point).
pub trait Live: Simulation + Shows {
    /// What the original's input handling keeps: its keyboard handler's key states, its timer's count.
    type Devices: Default;

    /// What the original's loop does next, given the host's devices as its handlers would have taken them.
    fn next(&self, devices: &mut Self::Devices, host: &Host) -> Next;

    /// What the pass wrote into its inputs itself (a keyboard queue it emptied: `Inputs::written`), taken into
    /// the handlers' state as the original's handlers would find it. Nothing by default.
    fn wrote(&self, devices: &mut Self::Devices, inputs: &Inputs) {
        let _ = (devices, inputs);
    }
}

/// A key-state array as a keyboard interrupt handler keeps one by scan code: 1 while the key is down. A byte with
/// 80h clears its place, E0h's (60h) too, as such a handler does.
#[derive(Debug, Clone)]
pub struct KeyStates(pub [u8; 128]);

impl Default for KeyStates {
    fn default() -> KeyStates {
        KeyStates([0; 128])
    }
}

impl KeyStates {
    /// The handler's work on each byte the controller sent.
    pub fn take(&mut self, scancodes: &[u8]) {
        for &code in scancodes {
            self.0[usize::from(code & 0x7F)] = u8::from(code & 0x80 == 0);
        }
    }
}

/// How many times a timer interrupt at `hz` has come since play began.
pub fn timer_count(ms: u64, hz: u64) -> u64 {
    ms * hz / 1000
}

/// A tick's boundary values from their bytes, by name.
pub fn inputs(values: &[(&str, &[u8])]) -> Inputs {
    Inputs::new(values.iter().map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec())).collect::<BTreeMap<_, _>>())
}

/// A port's `main` played (`PORT play [--game DIR] [--start DIR] [--no-sound]`): the game's folder chosen,
/// the port's start (`start`, from power-on, or the folder `rrc start` wrote, D120), then [`play`]; none when the
/// command line asks for something else (a comparison's run, which the runtime's `main` takes):
///
/// ```ignore
/// fn main() -> ExitCode {
///     rrc_platform::main::<Game, Screen>("Game", Game::start).unwrap_or_else(rrc_runtime::main_drawn::<Game, Screen>)
/// }
/// ```
pub fn main<S: Live, P: Presentation<S>>(title: &str, start: fn() -> S) -> Option<std::process::ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("play") {
        return None;
    }
    let mut sound = true;
    let mut from = None;
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        match (arg.as_str(), rest.clone().next()) {
            ("--game", Some(dir)) => rrc_runtime::original::use_game_dir(dir.into()),
            ("--start", Some(dir)) => from = Some(std::path::PathBuf::from(dir)),
            ("--no-sound", _) => {
                sound = false;
                continue;
            }
            (other, _) => return Some(fail(&format!("{other}: play [--game DIR] [--start DIR] [--no-sound]"))),
        }
        rest.next();
    }
    let started = match from {
        Some(dir) => match rrc_runtime::started::<S>(&dir) {
            Ok(started) => started,
            Err(e) => return Some(fail(&format!("{}: {e}", dir.display()))),
        },
        None => Started { state: start(), presentation: None, screen: None },
    };
    Some(match play::<S, P>(title, started, sound) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => fail(&e),
    })
}

fn fail(message: &str) -> std::process::ExitCode {
    eprintln!("play: {message}");
    std::process::ExitCode::from(1)
}

/// Plays from `started` (the port's start, or where `rrc start` left the original) with its presentation `P` in
/// a window titled `title`, with its sound if `sound`, until the window is closed or the original's loop ends.
pub fn play<S: Live, P: Presentation<S>>(title: &str, started: Started<S>, sound: bool) -> Result<(), String> {
    let Started { mut state, presentation: own, screen } = started;
    let mut presentation = P::default();
    if let Some(own) = own {
        presentation.given(own)?;
    }
    let given = screen.is_some();
    let mut frame = screen.unwrap_or_else(|| Frame::new(WIDTH, HEIGHT));
    presentation.start(&state.scene(), &mut frame, given);
    let audio = if P::SOUNDS && sound {
        match audio::Audio::start() {
            Ok(audio) => Some(audio),
            Err(e) => {
                eprintln!("no sound: {e}");
                None
            }
        }
    } else {
        None
    };
    // The start-up's sound, from power-on to the first tick.
    if let Some(audio) = audio.as_ref().filter(|_| !given) {
        let mut sounds = Sounds::default();
        presentation.sound(None, &state.scene(), &mut sounds);
        audio.play(&sounds);
    }
    let options = WindowOptions {
        resize: true,
        scale: Scale::FitScreen,
        scale_mode: ScaleMode::Stretch,
        ..WindowOptions::default()
    };
    let mut window = Window::new(title, WIDTH, HEIGHT, options).map_err(|e| format!("a window: {e}"))?;
    window.set_target_fps(70);
    let started = Instant::now();
    let mut devices = S::Devices::default();
    let mut number = 0;
    let mut pixels = vec![0u32; WIDTH * HEIGHT];
    'play: while window.is_open() {
        let mut host = Host {
            ms: started.elapsed().as_millis() as u64,
            scancodes: keys::scancodes(&window),
            mouse: mouse(&window, &frame),
            buttons: buttons(&window),
        };
        for _ in 0..CATCH_UP {
            let (sync, inputs) = match state.next(&mut devices, &host) {
                Next::Wait => break,
                Next::End => break 'play,
                Next::Tick(sync, inputs) => (sync, inputs),
            };
            number += 1;
            state.step(&Tick { number, sync: &sync, inputs: &inputs });
            state.wrote(&mut devices, &inputs);
            let scene = state.scene();
            if P::DRAWS {
                presentation.draw(&sync, &scene, &mut frame);
            }
            if let Some(audio) = &audio {
                let mut sounds = Sounds::default();
                presentation.sound(Some(&sync), &scene, &mut sounds);
                audio.play(&sounds);
            }
            // The bytes went to this tick's handlers.
            host.scancodes.clear();
        }
        rgb(&frame, &mut pixels);
        window.update_with_buffer(&pixels, frame.width, frame.height).map_err(|e| format!("the window: {e}"))?;
    }
    Ok(())
}

/// The frame in its palette's colours (6 bits to 8), as the window takes them.
fn rgb(frame: &Frame, out: &mut Vec<u32>) {
    let expand = |c: u8| u32::from((c << 2) | (c >> 4));
    out.clear();
    out.extend(frame.pixels.iter().map(|&p| {
        let c = &frame.palette[3 * usize::from(p)..3 * usize::from(p) + 3];
        expand(c[0]) << 16 | expand(c[1]) << 8 | expand(c[2])
    }));
}

/// The mouse in the frame's coordinates, the window stretched over it.
fn mouse(window: &Window, frame: &Frame) -> Option<(i32, i32)> {
    let (x, y) = window.get_unscaled_mouse_pos(MouseMode::Discard)?;
    let (width, height) = window.get_size();
    let scale = |at: f32, of: usize, to: usize| (at * to as f32 / of.max(1) as f32) as i32;
    Some((scale(x, width, frame.width), scale(y, height, frame.height)))
}

fn buttons(window: &Window) -> u8 {
    let down = |button| u8::from(window.get_mouse_down(button));
    down(MouseButton::Left) | down(MouseButton::Right) << 1 | down(MouseButton::Middle) << 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_state_array_keeps_what_the_handler_would() {
        let mut keys = KeyStates::default();
        // Left pressed, then the grey Up (E0h 48h), then Left let go.
        keys.take(&[0x4B, 0xE0, 0x48, 0xCB]);
        assert_eq!((keys.0[0x4B], keys.0[0x48], keys.0[0x60]), (0, 1, 0));
        assert_eq!(timer_count(1000, 70), 70);
        let i = inputs(&[("timer_count", &[3, 0]), ("key_down", &keys.0)]);
        assert_eq!((i.u16("timer_count"), i.u8_at("key_down", 0x48)), (3, 1));
    }
}
