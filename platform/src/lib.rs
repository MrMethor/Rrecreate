//! The platform layer (docs/design.md §6.1, D119): what a Rust port plays on, outside a comparison. It owns the main
//! loop: a window that shows the presentation's frames, the host's keyboard and mouse given to the port as the
//! PC's (the keyboard controller's bytes, the mouse in the screen's coordinates), and the sound the presentation
//! makes, played (the OPL3 by Nuked OPL3, the PC speaker, digitized sound). A port's [`Live`] turns those
//! devices into each tick's boundary values as the original's interrupt handlers and timer did, and says when
//! the original's loop would go on. The wall clock is read here, and only here: the simulation steps as it
//! does in a comparison, and the presentation draws and sounds as it does there. The host's keys reach the port
//! through its actions ([`Live::ACTIONS`], [`Controls`], D162): bound to other keys by a controls file, each still
//! comes as the PC key the original takes for it.
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
mod controls;
mod keys;

pub use controls::Controls;

use std::collections::BTreeMap;
use std::time::Instant;

use minifb::{MouseButton, MouseMode, Scale, ScaleMode, Window, WindowOptions};
use rrc_runtime::actions::Action;
use rrc_runtime::mouse::{MOUSE, MouseRanges};
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
    /// Whether the digitized sound last started still plays: what a sound driver's status answers (D121).
    pub pcm_playing: bool,
    /// The host's time of day when play began, in milliseconds since midnight (UTC): what a PC's clock would
    /// have read at power-on (D134).
    pub day_ms: u64,
    /// The port's actions begun (true) and ended since the last tick, in order, by name (D162): what the keys in
    /// `scancodes` and `buttons` are, for a port that reads actions instead (a modern mode, M7).
    pub actions: Vec<(&'static str, bool)>,
}

/// The window's mouse buttons pressed as its messages come (Windows: the window's procedure, subclassed), so a
/// press let go before the next frame is still one (D145); and the host's cursor put where the program's pointer is
/// (D145). Elsewhere neither: what the frames see, the host's cursor where it is.
#[cfg(windows)]
mod presses {
    use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
    use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
    use winapi::shared::windef::HWND;
    use winapi::um::winuser;

    static PRESSES: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];
    static ORIGINAL: AtomicIsize = AtomicIsize::new(0);

    unsafe extern "system" fn procedure(window: HWND, message: UINT, w: WPARAM, l: LPARAM) -> LRESULT {
        let button = match message {
            winuser::WM_LBUTTONDOWN | winuser::WM_LBUTTONDBLCLK => Some(0),
            winuser::WM_RBUTTONDOWN | winuser::WM_RBUTTONDBLCLK => Some(1),
            winuser::WM_MBUTTONDOWN | winuser::WM_MBUTTONDBLCLK => Some(2),
            _ => None,
        };
        if let Some(b) = button {
            PRESSES[b].fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: ORIGINAL is the window's own procedure, which `watch` replaced and this one hands each message to.
        unsafe {
            let original: winuser::WNDPROC = std::mem::transmute(ORIGINAL.load(Ordering::Relaxed));
            winuser::CallWindowProcW(original, window, message, w, l)
        }
    }

    /// The window's messages watched for presses.
    pub fn watch(window: &minifb::Window) {
        let handle = window.get_window_handle() as HWND;
        // SAFETY: the handle is minifb's live window; the replaced procedure is kept and called for every message.
        unsafe {
            let was = winuser::SetWindowLongPtrW(handle, winuser::GWLP_WNDPROC, procedure as *const () as isize);
            ORIGINAL.store(was, Ordering::Relaxed);
        }
    }

    /// The presses so far, left, right, middle.
    pub fn counts() -> [u32; 3] {
        [0, 1, 2].map(|b| PRESSES[b].load(Ordering::Relaxed))
    }

    /// The host's cursor put at (x, y) of the window's inside.
    pub fn put_cursor(window: &minifb::Window, (x, y): (i32, i32)) {
        let handle = window.get_window_handle() as HWND;
        let mut at = winapi::shared::windef::POINT { x, y };
        // SAFETY: the handle is minifb's live window; `at` is a local point.
        unsafe {
            if winuser::ClientToScreen(handle, &mut at) != 0 {
                winuser::SetCursorPos(at.x, at.y);
            }
        }
    }
}

#[cfg(not(windows))]
mod presses {
    pub fn watch(_: &minifb::Window) {}

    pub fn counts() -> [u32; 3] {
        [0; 3]
    }

    pub fn put_cursor(_: &minifb::Window, _: (i32, i32)) {}
}

/// What the original's loop does next.
// Made once a frame and taken at once: boxing its inputs would buy nothing.
#[allow(clippy::large_enum_variant)]
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

    /// Whether the program draws its own mouse pointer (D142): the host's is then hidden over the window, as a PC's
    /// screen showed only the game's. False by default (a program that shows the mouse driver's, or none).
    const OWN_POINTER: bool = false;

    /// The game's actions (D162): what the player does, each with the PC input the original takes for it, which a
    /// controls file binds to the host's keys (`play --controls FILE`). None by default: every key is its own.
    const ACTIONS: &'static [Action] = &[];

    /// The port's `quirks.toml` (`include_str!`), for `play --fix`'s names (D149): a misspelt one would do nothing and
    /// say nothing. Empty by default: a port with no quirks registered, which takes none.
    const QUIRKS: &'static str = "";

    /// What the original's loop does next, given the host's devices as its handlers would have taken them.
    fn next(&self, devices: &mut Self::Devices, host: &Host) -> Next;

    /// Where the program's mouse pointer is, in the screen's pixels (its driver's position, its ranges kept): the
    /// platform keeps the host's cursor there (D145), so a range or a position the program set holds as on a PC.
    /// None by default (the host's cursor left alone).
    fn pointer(&self, devices: &Self::Devices) -> Option<(i32, i32)> {
        let _ = devices;
        None
    }

    /// What the pass wrote into its inputs itself (a keyboard queue it emptied: `Inputs::written`), taken into
    /// the handlers' state as the original's handlers would find it. Nothing by default.
    fn wrote(&self, devices: &mut Self::Devices, inputs: &Inputs) {
        let _ = (devices, inputs);
    }

    /// Played from power-on: the inputs the start-up reads before the first sync point (D134), for
    /// `Simulation::power_on`: the session's (`recorded`, from a start folder of tick 0, D141: a command line),
    /// with what the host's devices give instead (a clock it seeds a generator from: `Host::day_ms`). The
    /// session's by default.
    fn power_on(&self, devices: &mut Self::Devices, host: &Host, recorded: &Inputs) -> Inputs {
        let _ = (devices, host);
        recorded.clone()
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

/// The mouse as an INT 33h driver reports it, the boundary value Clockwork's `mouse` device is (D49: 46 bytes of
/// words): the buttons, the position in the driver's coordinates, the motion in mickeys, and each button's
/// presses and releases (left, right, middle: how many, and where the last was); and what it keeps besides, the
/// ranges a position is kept in and its video mode's granularity, which the program's own calls set (D133).
#[derive(Debug, Clone, Default)]
pub struct MouseDriver {
    pub buttons: u16,
    pub x: i32,
    pub y: i32,
    pub mickeys: (i32, i32),
    pub presses: [(u16, i32, i32); 3],
    pub releases: [(u16, i32, i32); 3],
    pub ranges: MouseRanges,
    /// The host's mouse at its last move, in the driver's coordinates.
    host: Option<(i32, i32)>,
}

impl MouseDriver {
    /// The host's mouse taken in: where it moved, its position in the driver's coordinates (a screen pixel is
    /// `scale` of them: (2, 1) in mode 13h's 640 × 200) kept in the ranges, its motion as mickeys (the driver's
    /// default ratio: 8 a pixel's 8 across, 16 down); its buttons' presses and releases. A position the program set
    /// holds until the host's mouse moves; where the platform can, it puts the host's there (D145).
    pub fn take(&mut self, host: &Host, (sx, sy): (i32, i32)) {
        if let Some((x, y)) = host.mouse {
            let (x, y) = (x * sx, y * sy);
            if self.host != Some((x, y)) {
                let (was_x, was_y) = self.host.unwrap_or((self.x, self.y));
                self.mickeys.0 += x - was_x;
                self.mickeys.1 += 2 * (y - was_y);
                self.host = Some((x, y));
                let r = &self.ranges;
                (self.x, self.y) = (x.clamp(r.x.0, r.x.1), y.clamp(r.y.0, r.y.1));
            }
        }
        let buttons = u16::from(host.buttons & 7);
        for b in 0..3 {
            let (now, was) = (buttons >> b & 1, self.buttons >> b & 1);
            let changed = match (now, was) {
                (1, 0) => Some(&mut self.presses[b]),
                (0, 1) => Some(&mut self.releases[b]),
                _ => None,
            };
            if let Some((count, x, y)) = changed {
                (*count, *x, *y) = (count.wrapping_add(1), self.x, self.y);
            }
        }
        self.buttons = buttons;
    }

    /// What it reports, as Clockwork's `mouse` device has it: the buttons, x, y (function 03h), the motion
    /// (0Bh), then the presses (05h) and the releases (06h), each a count and the last one's x and y; positions at
    /// the granularity.
    pub fn state(&self) -> [u8; 46] {
        let (gx, gy) = self.ranges.granularity;
        let mut words = vec![self.buttons, self.x as u16 & gx, self.y as u16 & gy];
        words.extend([self.mickeys.0 as u16, self.mickeys.1 as u16]);
        for &(count, x, y) in self.presses.iter().chain(&self.releases) {
            words.extend([count, x as u16 & gx, y as u16 & gy]);
        }
        let mut bytes = [0; 46];
        for (chunk, word) in bytes.chunks_exact_mut(2).zip(words) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    /// What the pass did to the driver itself (D133, `rrc_runtime::mouse`): the ranges the port keeps (`ranges`), and
    /// its writes to the `mouse` input (a position set, a motion or a count read and cleared) taken into the
    /// driver's state; a [`Live::wrote`] calls it.
    pub fn wrote(&mut self, ranges: &MouseRanges, inputs: &Inputs) {
        self.ranges = ranges.clone();
        let written = inputs.written(MOUSE);
        if written.is_empty() {
            return;
        }
        let mut bytes = self.state();
        for (at, byte) in written {
            if let Some(b) = bytes.get_mut(at) {
                *b = byte;
            }
        }
        let word = |i: usize| u16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]);
        let signed = |i: usize| i32::from(word(i) as i16);
        (self.x, self.y, self.mickeys) = (signed(1), signed(2), (signed(3), signed(4)));
        for b in 0..3 {
            self.presses[b] = (word(5 + 3 * b), signed(6 + 3 * b), signed(7 + 3 * b));
            self.releases[b] = (word(14 + 3 * b), signed(15 + 3 * b), signed(16 + 3 * b));
        }
    }

    /// Read by a pass: its motion and its counts start again, as functions 0Bh, 05h and 06h clear them.
    pub fn read(&mut self) {
        self.mickeys = (0, 0);
        for (count, _, _) in self.presses.iter_mut().chain(&mut self.releases) {
            *count = 0;
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

/// A port's `main` played (`PORT play [--game DIR] [--start DIR] [--no-sound | --sound-buffer FRAMES] [--fix QUIRK]...`):
/// the game's folder chosen,
/// the port's start (`start`, from power-on, or the folder `rrc start` wrote, D120, of tick 0 from power-on
/// with the program's initial data, D141: none for a port that starts from a folder alone), then [`play`]; none
/// when the
/// command line asks for something else (a comparison's run, which the runtime's `main` takes):
///
/// ```ignore
/// fn main() -> ExitCode {
///     rrc_platform::main::<Game, Screen>("Game", Some(Game::start))
///         .unwrap_or_else(rrc_runtime::main_drawn::<Game, Screen>)
/// }
/// ```
pub fn main<S: Live, P: Presentation<S>>(title: &str, start: Option<fn() -> S>) -> Option<std::process::ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("play") {
        return None;
    }
    let mut sound = Some(audio::BUFFER);
    let mut from = None;
    if let Err(e) = rrc_runtime::actions::check(S::ACTIONS) {
        return Some(fail(&format!("the port's actions: {e}")));
    }
    let mut controls = Controls::new(S::ACTIONS);
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        match (arg.as_str(), rest.clone().next()) {
            ("--controls", Some(file)) => {
                // The actions bound to the host's keys (D162, docs/formats.md).
                let text = match std::fs::read_to_string(file) {
                    Ok(text) => text,
                    Err(e) => return Some(fail(&format!("{file}: {e}"))),
                };
                controls = match Controls::read(S::ACTIONS, &text) {
                    Ok(controls) => controls,
                    Err(e) => return Some(fail(&format!("{file}: {e}"))),
                };
            }
            ("--actions", _) => {
                // What the actions are and the keys that do them, for a controls file.
                print!("{}", controls::list(&controls));
                return Some(std::process::ExitCode::SUCCESS);
            }
            ("--game", Some(dir)) => rrc_runtime::original::use_game_dir(dir.into()),
            ("--start", Some(dir)) => {
                // The game goes on from there: its files the original had changed, and the games saved since (D131).
                let dir = std::path::PathBuf::from(dir);
                rrc_runtime::original::use_saves_dir(dir.join("saves"));
                from = Some(dir);
            }
            ("--no-sound", _) => {
                sound = None;
                continue;
            }
            ("--sound-buffer", Some(frames)) => match sound_buffer(frames) {
                // The output's buffer, if the sound breaks up (D168).
                Ok(frames) => sound = sound.map(|_| frames),
                Err(e) => return Some(fail(&e)),
            },
            ("--fix", Some(quirk)) => {
                // A quirk fixed for this game (D149, docs/design.md §6.4).
                if !is_registered(S::QUIRKS, quirk) {
                    return Some(fail(&format!("{quirk}: no such quirk (rrc quirk lists them)")));
                }
                rrc_runtime::fix(quirk);
            }
            (other, _) => {
                return Some(fail(&format!(
                    "{other}: play [--game DIR] [--start DIR] [--no-sound | --sound-buffer FRAMES] [--fix QUIRK]... \
                     [--controls FILE] [--actions]"
                )));
            }
        }
        rest.next();
    }
    let started = match from {
        Some(dir) => match rrc_runtime::started::<S>(&dir) {
            Ok(started) => started,
            Err(e) => return Some(fail(&format!("{}: {e}", dir.display()))),
        },
        None => match start {
            Some(start) => Started { state: start(), presentation: None, screen: None, power_on: None },
            None => {
                return Some(fail(
                    "this port plays from where a session is: rrc start SESSION --tick N (0: power-on), then \
                     --start DIR",
                ));
            }
        },
    };
    Some(match play::<S, P>(title, started, sound, controls) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => fail(&e),
    })
}

/// `--sound-buffer`'s samples a channel the output takes at a time (twice buffered): 256 to 16384.
fn sound_buffer(frames: &str) -> Result<usize, String> {
    frames
        .parse::<usize>()
        .ok()
        .filter(|f| (256..=16384).contains(f))
        .ok_or_else(|| format!("--sound-buffer {frames}: samples a channel, 256 to 16384"))
}

/// Whether `quirks.toml`'s text registers `quirk`.
fn is_registered(quirks: &str, quirk: &str) -> bool {
    quirks.lines().any(|line| line.trim() == format!("name = \"{quirk}\""))
}

fn fail(message: &str) -> std::process::ExitCode {
    eprintln!("play: {message}");
    std::process::ExitCode::from(1)
}

/// Plays from `started` (the port's start, or where `rrc start` left the original) with its presentation `P` in
/// a window titled `title`, with its sound if `sound` (the output's buffer: samples a channel), the host's keys
/// given by `controls`, until the window is closed or the original's loop ends.
pub fn play<S: Live, P: Presentation<S>>(
    title: &str,
    started: Started<S>,
    sound: Option<usize>,
    mut controls: Controls,
) -> Result<(), String> {
    let Started { mut state, presentation: own, screen, power_on } = started;
    let mut presentation = P::default();
    if let Some(own) = own {
        presentation.given(own)?;
    }
    let given = screen.is_some();
    let mut devices = S::Devices::default();
    let day_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| (d.as_millis() % 86_400_000) as u64);
    // From power-on, the start-up's pass (D134): what it reads from the host's devices.
    if !given {
        let host = Host { day_ms, ..Host::default() };
        let inputs = state.power_on(&mut devices, &host, &power_on.unwrap_or_default());
        Simulation::power_on(
            &mut state,
            &Tick { number: 0, sync: "power-on", inputs: &inputs, spun: None, next: None },
        );
    }
    let mut frame = screen.unwrap_or_else(|| Frame::new(WIDTH, HEIGHT));
    presentation.start(&state.scene(), &mut frame, given);
    let audio = if let Some(frames) = sound.filter(|_| P::SOUNDS) {
        match audio::Audio::start(frames) {
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
    let options =
        WindowOptions { resize: true, scale: Scale::X2, scale_mode: ScaleMode::Stretch, ..WindowOptions::default() };
    let mut window = Window::new(title, WIDTH, HEIGHT, options).map_err(|e| format!("a window: {e}"))?;
    window.set_target_fps(70);
    // The program's own pointer is the only one (D142).
    if S::OWN_POINTER {
        window.set_cursor_visibility(false);
    }
    presses::watch(&window);
    let mut pressed = presses::counts();
    let started = Instant::now();
    let mut number = 0;
    let mut pixels = vec![0u32; WIDTH * HEIGHT];
    'play: while window.is_open() {
        let (down, up) = keys::changed(&window);
        let given = controls.give(&down, &up);
        let mut host = Host {
            ms: started.elapsed().as_millis() as u64,
            scancodes: given.scancodes,
            mouse: mouse(&window, &frame),
            buttons: buttons(&window, &mut pressed) | given.buttons,
            pcm_playing: audio.as_ref().is_some_and(audio::Audio::pcm_playing),
            day_ms,
            actions: given.actions,
        };
        for _ in 0..CATCH_UP {
            let (sync, inputs) = match state.next(&mut devices, &host) {
                Next::Wait => break,
                Next::End => break 'play,
                Next::Tick(sync, inputs) => (sync, inputs),
            };
            number += 1;
            state.step(&Tick { number, sync: &sync, inputs: &inputs, spun: None, next: None });
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
            // The bytes and the actions went to this tick's handlers.
            host.scancodes.clear();
            host.actions.clear();
        }
        // The host's cursor where the program's pointer is, inside the window while it has the keys (D145).
        if let (Some(at), Some(pointer)) = (host.mouse, state.pointer(&devices))
            && at != pointer
            && window.is_active()
        {
            presses::put_cursor(&window, in_window(&window, &frame, pointer));
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
/// A screen pixel's middle in the window, stretched over the frame: where `mouse` finds that pixel again.
fn in_window(window: &Window, frame: &Frame, (x, y): (i32, i32)) -> (i32, i32) {
    let (width, height) = window.get_size();
    let at = |v: i32, of: usize, to: usize| (2 * v + 1) * to as i32 / (2 * of.max(1) as i32);
    (at(x, frame.width, width), at(y, frame.height, height))
}

fn mouse(window: &Window, frame: &Frame) -> Option<(i32, i32)> {
    let (x, y) = window.get_unscaled_mouse_pos(MouseMode::Discard)?;
    let (width, height) = window.get_size();
    let scale = |at: f32, of: usize, to: usize| (at * to as f32 / of.max(1) as f32) as i32;
    Some((scale(x, width, frame.width), scale(y, height, frame.height)))
}

/// The buttons down, and those pressed since the last frame though let go (`pressed`: the presses counted so far).
fn buttons(window: &Window, pressed: &mut [u32; 3]) -> u8 {
    let down = |button| u8::from(window.get_mouse_down(button));
    let mut held = down(MouseButton::Left) | down(MouseButton::Right) << 1 | down(MouseButton::Middle) << 2;
    let now = presses::counts();
    for b in 0..3 {
        if now[b] != pressed[b] {
            held |= 1 << b;
        }
    }
    *pressed = now;
    held
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sound_buffer_is_a_number_of_samples_in_range() {
        assert_eq!(sound_buffer("4096"), Ok(4096));
        assert!(sound_buffer("100").is_err() && sound_buffer("lots").is_err() && sound_buffer("20000").is_err());
        // The default lasts longer than Windows' DirectSound reads ahead (D168).
        assert!(audio::BUFFER as f64 / 49_716.0 > 0.04);
    }

    #[test]
    fn a_fix_names_a_registered_quirk() {
        let quirks = "[[quirk]]\nname = \"an_overrun\"\nat = \"X.EXE+00000\"\nwhat = \"name = other\"\n";
        assert!(is_registered(quirks, "an_overrun"));
        assert!(
            !is_registered(quirks, "an_over") && !is_registered(quirks, "other") && !is_registered("", "an_overrun")
        );
    }

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

    #[test]
    fn the_mouse_is_reported_as_the_driver_would() {
        let mut mouse = MouseDriver::default();
        mouse.take(&Host { mouse: Some((10, 20)), ..Host::default() }, (2, 1));
        mouse.read();
        // Moved 5 pixels right and 3 down, the left button pressed there.
        mouse.take(&Host { mouse: Some((15, 23)), buttons: 1, ..Host::default() }, (2, 1));
        let state = mouse.state();
        let word = |i: usize| u16::from_le_bytes([state[2 * i], state[2 * i + 1]]);
        assert_eq!((word(0), word(1), word(2)), (1, 30, 23), "buttons, x in 640 across, y");
        assert_eq!((word(3), word(4)), (10, 6), "mickeys: 8 a pixel's 8 across, 16 down");
        assert_eq!((word(5), word(6), word(7)), (1, 30, 23), "the left button's press");
        mouse.read();
        assert_eq!((mouse.mickeys, mouse.presses[0].0), ((0, 0), 0));
    }

    #[test]
    fn the_program_s_calls_change_the_live_driver() {
        let mut mouse = MouseDriver::default();
        mouse.take(&Host { mouse: Some((165, 30)), ..Host::default() }, (2, 1));
        // The pass kept the mouse to a box (07h, 08h) and set it at the box's middle (04h).
        let state = mouse.state();
        let inputs = inputs(&[(MOUSE, &state)]);
        let mut ranges = MouseRanges::MODE_13H;
        ranges.set_x_range(&inputs, 162, 471);
        ranges.set_y_range(&inputs, 91, 137);
        ranges.set_position(&inputs, 158, 114);
        mouse.wrote(&ranges, &inputs);
        assert_eq!((mouse.x, mouse.y), (162, 114), "the driver's position as the pass left it");
        // The host's mouse still: the position holds; moved, it is the host's, kept in the ranges.
        mouse.take(&Host { mouse: Some((165, 30)), ..Host::default() }, (2, 1));
        assert_eq!((mouse.x, mouse.y), (162, 114));
        mouse.take(&Host { mouse: Some((300, 150)), ..Host::default() }, (2, 1));
        assert_eq!((mouse.x, mouse.y), (471, 137));
    }
}
