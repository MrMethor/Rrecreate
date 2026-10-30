//! The chime of `testbins` (`testbins/src/chime.c`), ported with its sound (M6, D108): the game's pass, and
//! a presentation that makes what the original's speaker and OPL writes make, compared with them event by
//! event, tick by tick.
//!
//! A tick begins where the loop's `wait_tick` returns (`pass`, CHIME.EXE+002C8); a step is `pass`. The
//! inputs are the timer count (INT 08h's, 70 Hz) and the key states (INT 09h's). It draws nothing: the screen
//! stays DOS's.
//!
//! The simulation shows a scene: the notes played, the passes left of the one sounding, which it is. `Chime`,
//! the sound, makes from it what `instrument`, `speaker_on`, `speaker_off` and `opl` wrote: the instrument at
//! start-up, and for each pass the note that stopped and the note that started. `CHIME_BUG=block` leaves a
//! bug in: the OPL keys a note on an octave low.
//!
//! Each function names the original's it reimplements by its ID (DESIGN.md §6.6).

use rrc_runtime::present::{Presentation, Shows, Sounds, Speaker};
use rrc_runtime::{Simulation, Tick};
use serde::{Deserialize, Serialize};

const NOTES: u8 = 8;
const NOTE_TICKS: u8 = 20;
const BLOCK: u8 = 5;
/// C5 to C6: the speaker's divisors and the OPL's frequency numbers in block 5 (`divisors`, `fnums`).
const DIVISORS: [u16; 8] = [2281, 2032, 1810, 1709, 1522, 1356, 1208, 1140];
const FNUMS: [u16; 8] = [0x159, 0x183, 0x1B2, 0x1CC, 0x205, 0x244, 0x28B, 0x2B2];

const KEY_SPACE: usize = 0x39;
const KEY_UP: usize = 0x48;
const KEY_DOWN: usize = 0x50;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Chime {
    last_count: u16,
    pass_count: u16,
    /// The note Space plays.
    note: u8,
    /// Passes left of the note sounding.
    playing: u8,
    /// The note sounding.
    sounding: u8,
    /// Bit per key (Space, Up, Down) down at the last pass.
    keys_held: u8,
    /// Notes played.
    played: u16,
}

impl Simulation for Chime {
    /// `main` (CHIME.EXE+00274) before its first tick: the handlers, the timer at 70 Hz, the instrument.
    fn start() -> Chime {
        Chime::default()
    }

    /// `pass` (CHIME.EXE+00166): Up and Down choose the note; the note sounding counts down; Space plays the
    /// chosen one.
    fn step(&mut self, tick: &Tick) {
        let down = |key| tick.inputs.u8_at("key_down", key) != 0;
        self.last_count = tick.inputs.u16("timer_count");
        self.pass_count = self.pass_count.wrapping_add(1);
        let held = u8::from(down(KEY_SPACE)) | u8::from(down(KEY_UP)) << 1 | u8::from(down(KEY_DOWN)) << 2;
        let pressed = held & !self.keys_held;
        if pressed & 2 != 0 && self.note < NOTES - 1 {
            self.note += 1;
        }
        if pressed & 4 != 0 && self.note > 0 {
            self.note -= 1;
        }
        self.playing = self.playing.saturating_sub(1);
        if pressed & 1 != 0 {
            self.sounding = self.note;
            self.playing = NOTE_TICKS;
            self.played = self.played.wrapping_add(1);
        }
        self.keys_held = held;
    }
}

/// What there is to hear: the notes played, the passes left of the one sounding, which it is.
struct Scene {
    played: u16,
    playing: u8,
    sounding: u8,
}

impl Shows for Chime {
    type Scene<'a> = Scene;

    fn scene(&self) -> Scene {
        Scene { played: self.played, playing: self.playing, sounding: self.sounding }
    }
}

/// The sound: the speaker and the OPL's channel 0, and what it last heard of the scene.
#[derive(Default)]
struct Sound {
    played: u16,
    playing: u8,
    sounding: u8,
}

impl Presentation<Chime> for Sound {
    const DRAWS: bool = false;
    const SOUNDS: bool = true;

    fn start(&mut self, scene: &Scene, _: &mut rrc_runtime::present::Frame, _: bool) {
        (self.played, self.playing, self.sounding) = (scene.played, scene.playing, scene.sounding);
    }

    fn sound(&mut self, pass: Option<&str>, scene: &Scene, sounds: &mut Sounds) {
        let Some(_) = pass else {
            instrument(sounds);
            return;
        };
        // The note sounding ran out in this pass (`playing` reached 0): `speaker_off` and the key off.
        let stopped = self.playing == 1;
        if stopped {
            speaker_off(sounds);
            opl(sounds, 0xB0, BLOCK << 2 | (FNUMS[usize::from(self.sounding)] >> 8) as u8);
        }
        // A note played: `speaker_on` at its divisor, its frequency and the key on.
        if scene.played != self.played {
            let note = usize::from(scene.sounding);
            speaker_on(sounds, DIVISORS[note], self.playing > 1);
            let block = if std::env::var("CHIME_BUG").is_ok_and(|b| b == "block") { BLOCK - 1 } else { BLOCK };
            opl(sounds, 0xA0, (FNUMS[note] & 0xFF) as u8);
            opl(sounds, 0xB0, 0x20 | block << 2 | (FNUMS[note] >> 8) as u8);
        }
        (self.played, self.playing, self.sounding) = (scene.played, scene.playing, scene.sounding);
    }
}

/// `opl` (CHIME.EXE+0006A): a register written.
fn opl(sounds: &mut Sounds, register: u16, value: u8) {
    sounds.opl.push((register, value));
}

/// `instrument` (CHIME.EXE+000FE): channel 0's plain tone.
fn instrument(sounds: &mut Sounds) {
    for (register, value) in [
        (0x20, 0x01),
        (0x40, 0x3F),
        (0x60, 0xF0),
        (0x80, 0x77),
        (0x23, 0x01),
        (0x43, 0x00),
        (0x63, 0xF0),
        (0x83, 0x77),
        (0xC0, 0x00),
    ] {
        opl(sounds, register, value);
    }
}

/// `speaker_on`: the timer's channel 2 set to a square wave at `divisor`, then the speaker's gate opened. Set
/// while it sounds, the channel holds the cone out until the new count is loaded.
fn speaker_on(sounds: &mut Sounds, divisor: u16, sounding: bool) {
    if sounding {
        sounds.speaker.push(Speaker::Steady(true));
    }
    let period = u32::from(divisor);
    sounds.speaker.push(Speaker::Tone { period, high: period.div_ceil(2) });
}

/// `speaker_off` (CHIME.EXE+000E5): the gate closed.
fn speaker_off(sounds: &mut Sounds) {
    sounds.speaker.push(Speaker::Steady(false));
}

fn main() -> std::process::ExitCode {
    rrc_runtime::main_drawn::<Chime, Sound>()
}
