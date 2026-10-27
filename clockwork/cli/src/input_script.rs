//! Scripted input for `clockwork run --input MS:KEYS`: keys and mouse clicks at set times of
//! emulated time, so a tool or an agent can drive a program through its menus, and the run
//! stays exactly reproducible.
//!
//! KEYS is text typed on a US keyboard, one key every 120 ms, each held 60 ms (a game that polls
//! slowly may need more time between steps), with named keys in braces:
//! `{enter}` `{esc}` `{tab}` `{bksp}` `{space}` `{up}` `{down}` `{left}` `{right}` `{home}`
//! `{end}` `{pgup}` `{pgdn}` `{ins}` `{del}` `{f1}`-`{f12}`, and the modifiers `{shift}` (left),
//! `{rshift}` `{ctrl}` `{rctrl}` `{alt}` `{ralt}` `{caps}`; `{click X Y}` (and `{rclick X Y}`)
//! moves the mouse to X, Y in the mouse driver's coordinates, presses the button 100 ms later and
//! releases it 250 ms after that (moving before pressing matters: some games, like Arena,
//! put the cursor back where they last read it every frame); `{move X Y}` only moves, to a place on
//! the screen (the driver keeps the cursor in its range, and the motion stops with it); `{motion DX DY}`
//! moves the mouse by DX, DY mickeys, as a hand does: the driver's motion counters take all of it, and
//! the cursor follows within its range (a game that turns by the motion turns as far as asked);
//! `{mouse X Y BUTTONS}` puts the mouse at X, Y with BUTTONS held (bit 0 left, bit 1 right, bit 2
//! middle) until the next mouse step: a drag is a press, moves with the button held, a release
//! (`{mouse 320 60 2}{mouse 320 140 2}{mouse 320 140 0}`: Arena's swing);
//! `{stick X Y BUTTONS}` puts the first joystick at X, Y (-32767 to 32767, left and up negative) with
//! BUTTONS held (bit 0 button 1, bit 1 button 2) until the next such step, `{stick2 ...}` the second
//! (`--joystick N` connects them);
//! `{hold KEY MS}` holds a key (a named one or a character) down for MS ms, to walk in a game;
//! `{keydown KEY}` and `{keyup KEY}` press and release one with other steps between (Ctrl held
//! while walking, Alt+X); `{wait MS}` waits.
//!
//! MS counts from power-on. A line whose time has passed when it is given (`clockwork serve`'s
//! `input`, between runs) is refused: its input would land later than the time it says.

use dos::Machine;
use dos::keyboard::{LEFT_SHIFT, ascii_to_scancode};

const KEY_SPACING_MS: u64 = 120;
const KEY_HOLD_MS: u64 = 60;
const CLICK_DELAY_MS: u64 = 100;
const CLICK_HOLD_MS: u64 = 250;

/// One step of a script.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// Scancodes to press (with the E0 prefix for extended keys) and release.
    Key(Vec<u8>),
    /// The same, held down for a number of milliseconds.
    Hold(Vec<u8>, u64),
    /// A key pressed (and left down).
    KeyDown(Vec<u8>),
    /// A key released.
    KeyUp(Vec<u8>),
    Click {
        x: i16,
        y: i16,
        button: u8,
    },
    Move {
        x: i16,
        y: i16,
    },
    /// The mouse at a place with buttons held.
    Mouse {
        x: i16,
        y: i16,
        buttons: u8,
    },
    /// Mouse motion in mickeys.
    Motion {
        dx: i16,
        dy: i16,
    },
    /// A joystick (0 or 1) at a position with buttons held.
    Stick {
        stick: u8,
        x: i16,
        y: i16,
        buttons: u8,
    },
    Wait(u64),
}

/// A script entry: steps starting at a time in emulated milliseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    start_ms: u64,
    steps: Vec<Step>,
}

fn named_key(name: &str) -> Option<Vec<u8>> {
    let extended = |code: u8| Some(vec![0xE0, code]);
    match name {
        "enter" => Some(vec![0x1C]),
        "esc" => Some(vec![0x01]),
        "tab" => Some(vec![0x0F]),
        "bksp" => Some(vec![0x0E]),
        "space" => Some(vec![0x39]),
        "up" => extended(0x48),
        "down" => extended(0x50),
        "left" => extended(0x4B),
        "right" => extended(0x4D),
        "home" => extended(0x47),
        "end" => extended(0x4F),
        "pgup" => extended(0x49),
        "pgdn" => extended(0x51),
        "ins" => extended(0x52),
        "del" => extended(0x53),
        "shift" | "lshift" => Some(vec![LEFT_SHIFT]),
        "rshift" => Some(vec![0x36]),
        "ctrl" | "lctrl" => Some(vec![0x1D]),
        "rctrl" => extended(0x1D),
        "alt" | "lalt" => Some(vec![0x38]),
        "ralt" => extended(0x38),
        "caps" => Some(vec![0x3A]),
        "f11" => Some(vec![0x57]),
        "f12" => Some(vec![0x58]),
        f => match f.strip_prefix('f')?.parse::<u8>() {
            Ok(n @ 1..=10) => Some(vec![0x3A + n]),
            _ => None,
        },
    }
}

/// A named key, or a character's key (unshifted).
fn key(name: &str) -> Option<Vec<u8>> {
    named_key(name).or_else(|| match name.as_bytes() {
        &[c] => ascii_to_scancode(c).map(|(make, _)| vec![make]),
        _ => None,
    })
}

impl Script {
    /// Parses `MS:KEYS` (a line from a file may end in CR LF: the line break isn't a key).
    pub fn parse(spec: &str) -> Result<Script, String> {
        let spec = spec.trim_end_matches(['\r', '\n']);
        let (time, keys) = spec.split_once(':').ok_or_else(|| format!("--input {spec:?}: expected MS:KEYS"))?;
        let start_ms = time.trim().parse().map_err(|_| format!("--input {spec:?}: bad time {time:?}"))?;
        let mut steps = Vec::new();
        let mut rest = keys;
        while let Some(c) = rest.chars().next() {
            if c == '{' {
                let end = rest.find('}').ok_or_else(|| format!("--input {spec:?}: unclosed {{"))?;
                let words: Vec<&str> = rest[1..end].split_whitespace().collect();
                let number = |s: &str| s.parse::<i16>().map_err(|_| format!("--input {spec:?}: bad number {s:?}"));
                steps.push(match words.as_slice() {
                    ["click", x, y] => Step::Click { x: number(x)?, y: number(y)?, button: 1 },
                    ["rclick", x, y] => Step::Click { x: number(x)?, y: number(y)?, button: 2 },
                    ["move", x, y] => Step::Move { x: number(x)?, y: number(y)? },
                    ["mouse", x, y, buttons] => {
                        let buttons = buttons.parse::<u8>().ok().filter(|&b| b <= 7);
                        Step::Mouse {
                            x: number(x)?,
                            y: number(y)?,
                            buttons: buttons.ok_or_else(|| format!("--input {spec:?}: buttons are 0 to 7"))?,
                        }
                    }
                    ["motion", dx, dy] => Step::Motion { dx: number(dx)?, dy: number(dy)? },
                    [which @ ("stick" | "stick2"), x, y, buttons] => {
                        let buttons = buttons.parse::<u8>().ok().filter(|&b| b <= 3);
                        Step::Stick {
                            stick: u8::from(*which == "stick2"),
                            x: number(x)?,
                            y: number(y)?,
                            buttons: buttons.ok_or_else(|| format!("--input {spec:?}: buttons are 0 to 3"))?,
                        }
                    }
                    ["wait", ms] => Step::Wait(ms.parse().map_err(|_| format!("--input {spec:?}: bad wait {ms:?}"))?),
                    ["hold", name, ms] => {
                        let codes = key(name).ok_or_else(|| format!("--input {spec:?}: unknown key {name:?}"))?;
                        Step::Hold(codes, ms.parse().map_err(|_| format!("--input {spec:?}: bad time {ms:?}"))?)
                    }
                    [action @ ("keydown" | "keyup"), name] => {
                        let codes = key(name).ok_or_else(|| format!("--input {spec:?}: unknown key {name:?}"))?;
                        if *action == "keydown" { Step::KeyDown(codes) } else { Step::KeyUp(codes) }
                    }
                    [name] => {
                        Step::Key(named_key(name).ok_or_else(|| format!("--input {spec:?}: unknown key {{{name}}}"))?)
                    }
                    _ => return Err(format!("--input {spec:?}: can't read {{{}}}", &rest[1..end])),
                });
                rest = &rest[end + 1..];
            } else {
                let (make, shift) =
                    ascii_to_scancode(c as u8).ok_or_else(|| format!("--input {spec:?}: no key for {c:?}"))?;
                steps.push(Step::Key(if shift { vec![LEFT_SHIFT, make] } else { vec![make] }));
                rest = &rest[c.len_utf8()..];
            }
        }
        Ok(Script { start_ms, steps })
    }

    /// Queues the script's input in the machine; refused if its time has passed.
    pub fn schedule(&self, m: &mut Machine) -> Result<(), String> {
        let mut at = m.cycle_at_ms(self.start_ms);
        if at < m.cpu.cycles {
            return Err(format!(
                "input at {} ms: the machine is at {} ms already (times count from power-on)",
                self.start_ms,
                m.elapsed_ms()
            ));
        }
        let ms = |m: &Machine, ms: u64| m.cycle_at_ms(ms);
        for step in &self.steps {
            match step {
                Step::Key(codes) | Step::Hold(codes, _) => {
                    let held_ms = if let Step::Hold(_, held) = step { *held } else { KEY_HOLD_MS };
                    m.queue_scancodes(at, codes);
                    // Release in reverse: the key, then its prefix or the shift.
                    let release = at + ms(m, held_ms);
                    let (last, held) = codes.split_last().expect("a key has a scancode");
                    match held {
                        [0xE0] => m.queue_scancodes(release, &[0xE0, last | 0x80]),
                        [shift] => m.queue_scancodes(release, &[last | 0x80, shift | 0x80]),
                        _ => m.queue_scancodes(release, &[last | 0x80]),
                    }
                    at += ms(m, held_ms + KEY_SPACING_MS - KEY_HOLD_MS);
                }
                &Step::Click { x, y, button } => {
                    m.queue_mouse(at, x, y, 0);
                    let press = at + ms(m, CLICK_DELAY_MS);
                    m.queue_mouse(press, x, y, button);
                    m.queue_mouse(press + ms(m, CLICK_HOLD_MS), x, y, 0);
                    at = press + ms(m, CLICK_HOLD_MS) + ms(m, KEY_SPACING_MS);
                }
                &Step::Move { x, y } => {
                    m.queue_mouse(at, x, y, 0);
                    at += ms(m, KEY_SPACING_MS);
                }
                &Step::Mouse { x, y, buttons } => {
                    m.queue_mouse(at, x, y, buttons);
                    at += ms(m, KEY_SPACING_MS);
                }
                &Step::Motion { dx, dy } => {
                    m.queue_input(at, dos::Input::MouseMotion { dx, dy });
                    at += ms(m, KEY_SPACING_MS);
                }
                &Step::Stick { stick, x, y, buttons } => {
                    m.queue_input(at, dos::Input::Joystick { stick, x, y, buttons });
                    at += ms(m, KEY_SPACING_MS);
                }
                Step::KeyDown(codes) => {
                    m.queue_scancodes(at, codes);
                    at += ms(m, KEY_SPACING_MS);
                }
                Step::KeyUp(codes) => {
                    let (last, prefix) = codes.split_last().expect("a key has a scancode");
                    m.queue_scancodes(at, &[prefix, &[last | 0x80]].concat());
                    at += ms(m, KEY_SPACING_MS);
                }
                &Step::Wait(wait) => at += ms(m, wait),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_named_keys_and_mouse() {
        let script = Script::parse("1500:aB{enter}{f12}{up}{click 320 -5}{wait 50}{hold up 900}{hold x 20}").unwrap();
        assert_eq!(script.start_ms, 1500);
        assert_eq!(
            script.steps,
            [
                Step::Key(vec![0x1E]),
                Step::Key(vec![LEFT_SHIFT, 0x30]),
                Step::Key(vec![0x1C]),
                Step::Key(vec![0x58]),
                Step::Key(vec![0xE0, 0x48]),
                Step::Click { x: 320, y: -5, button: 1 },
                Step::Wait(50),
                Step::Hold(vec![0xE0, 0x48], 900),
                Step::Hold(vec![0x2D], 20),
            ]
        );
        let script = Script::parse("0:{keydown ctrl}{hold up 500}{keyup ctrl}{keydown ralt}{keyup w}").unwrap();
        assert_eq!(
            script.steps,
            [
                Step::KeyDown(vec![0x1D]),
                Step::Hold(vec![0xE0, 0x48], 500),
                Step::KeyUp(vec![0x1D]),
                Step::KeyDown(vec![0xE0, 0x38]),
                Step::KeyUp(vec![0x11]),
            ]
        );
        assert_eq!(Script::parse("0:{down}{up}").unwrap().steps[1], Step::Key(vec![0xE0, 0x48]), "arrows still");
        assert_eq!(
            Script::parse("0:{stick -32767 0 1}{stick2 5 6 0}").unwrap().steps,
            [Step::Stick { stick: 0, x: -32767, y: 0, buttons: 1 }, Step::Stick { stick: 1, x: 5, y: 6, buttons: 0 }]
        );
        assert!(Script::parse("0:{stick 0 0 4}").is_err());
        assert_eq!(Script::parse("5:{motion -900 12}").unwrap().steps, [Step::Motion { dx: -900, dy: 12 }]);
        let drag = Script::parse("5:{mouse 320 60 2}{mouse 320 140 2}{mouse 320 140 0}").unwrap();
        assert_eq!(drag.steps[1], Step::Mouse { x: 320, y: 140, buttons: 2 });
        assert!(Script::parse("5:{mouse 1 2 8}").unwrap_err().contains("buttons are 0 to 7"));
        assert_eq!(Script::parse("5:a\r\n").unwrap().steps, [Step::Key(vec![0x1E])], "a line break isn't a key");
        assert!(Script::parse("12:{nope}").is_err());
        assert!(Script::parse("12:{keydown}").is_err());
        assert!(Script::parse("x:a").is_err());
    }
}
