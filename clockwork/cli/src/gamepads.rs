//! Host gamepads as the emulated joysticks in `rec-host play`, through `gilrs`.
//!
//! The first connected gamepad drives joystick A, the second joystick B: the left stick is the
//! position, the South and East buttons (A and B on an Xbox pad) are buttons 1 and 2. Changes
//! become [`Input::Joystick`] events, so a recorded session replays them exactly.

use dos::Input;
use dos::joystick::AXIS_MAX;
use gilrs::{Axis, Button, Gilrs};

pub struct Gamepads {
    gilrs: Gilrs,
    sticks: u8,
    /// What each joystick was last set to.
    last: [Option<Input>; 2],
}

impl Gamepads {
    /// Starts reading the host's gamepads for `sticks` joysticks (with a message if that fails).
    pub fn start(sticks: u8) -> Option<Gamepads> {
        match Gilrs::new() {
            Ok(gilrs) => {
                let names: Vec<String> = gilrs.gamepads().map(|(_, pad)| pad.name().to_string()).collect();
                if names.is_empty() {
                    eprintln!("rec-host: no gamepad connected; the joysticks stay centered");
                } else {
                    eprintln!("rec-host: joysticks from {}", names.join(", "));
                }
                Some(Gamepads { gilrs, sticks, last: [None; 2] })
            }
            Err(e) => {
                eprintln!("rec-host: can't read gamepads: {e}");
                None
            }
        }
    }

    /// The joystick events for what changed since the last call.
    pub fn changes(&mut self) -> Vec<Input> {
        while self.gilrs.next_event().is_some() {}
        let to_axis = |v: f32| (v.clamp(-1.0, 1.0) * f32::from(AXIS_MAX)).round() as i16;
        let pads: Vec<Input> = self
            .gilrs
            .gamepads()
            .filter(|(_, pad)| pad.is_connected())
            .take(usize::from(self.sticks))
            .enumerate()
            .map(|(stick, (_, pad))| Input::Joystick {
                stick: stick as u8,
                x: to_axis(pad.value(Axis::LeftStickX)),
                // Up is positive for gilrs, negative (toward 0 on the game port) for DOS.
                y: to_axis(-pad.value(Axis::LeftStickY)),
                buttons: u8::from(pad.is_pressed(Button::South)) | (u8::from(pad.is_pressed(Button::East)) << 1),
            })
            .collect();
        let mut changes = Vec::new();
        for input in pads {
            let Input::Joystick { stick, .. } = input else { continue };
            let last = &mut self.last[usize::from(stick)];
            if *last != Some(input) {
                *last = Some(input);
                changes.push(input);
            }
        }
        changes
    }
}
