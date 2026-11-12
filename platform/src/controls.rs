//! The controls (docs/design.md §6.1, D162): the host's keys bound to a port's actions ([`crate::Live::ACTIONS`]), each
//! action given to the port as the PC input the original takes for it. By default an action is bound to the host's
//! key that is its own PC key, so every key sends what it would with no actions at all: the neutral binding, under
//! which a faithful port plays as its comparisons run. A controls file (`play --controls FILE`, docs/formats.md)
//! binds actions to other keys, an action a line:
//!
//! ```text
//! # an action = the host's keys that do it, by their names (W, Up, LeftCtrl, NumPad8, Space...)
//! forward = W, Up
//! use = E
//! ```
//!
//! An action named there has those keys only, and a key named there does that action only; a key bound to no action
//! sends its own PC key. A key bound to an action of a mouse button holds the button.

use minifb::Key;
use rrc_runtime::actions::{self, Action, Pc};

use crate::keys;

/// The host's keys bound to a port's actions, and which actions are held.
#[derive(Debug, Clone)]
pub struct Controls {
    actions: &'static [Action],
    /// Each bound host key and its action's place.
    bound: Vec<(Key, usize)>,
    /// How many of each action's keys are down.
    down: Vec<u8>,
}

/// What a frame's keys give the port.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Given {
    /// The keyboard controller's bytes, in order.
    pub scancodes: Vec<u8>,
    /// The mouse buttons the actions hold, and those an action pressed and let go within the frame.
    pub buttons: u8,
    /// The actions begun (true) and ended, in order.
    pub actions: Vec<(&'static str, bool)>,
}

impl Controls {
    /// The neutral binding: each action bound to the host's keys that are its PC key.
    pub fn new(actions: &'static [Action]) -> Controls {
        let bound = keys::KEYS
            .iter()
            .filter_map(|&key| {
                let (grey, code) = keys::scancode(key)?;
                let pc = if grey { Pc::GreyKey(code) } else { Pc::Key(code) };
                Some((key, actions.iter().position(|a| a.pc == pc)?))
            })
            .collect();
        Controls { actions, bound, down: vec![0; actions.len()] }
    }

    /// The neutral binding with a controls file's (`text`) over it.
    pub fn read(actions: &'static [Action], text: &str) -> Result<Controls, String> {
        let mut controls = Controls::new(actions);
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or_default().trim();
            if line.is_empty() {
                continue;
            }
            let at = |e: String| format!("line {}: {e}", n + 1);
            let (name, keys) = line.split_once('=').ok_or_else(|| at("an action = the keys that do it".into()))?;
            let name = name.trim();
            let Some((i, _)) = actions::named(actions, name) else {
                let known: Vec<&str> = actions.iter().map(|a| a.name).collect();
                return Err(at(format!("{name}: no such action (the port's: {})", known.join(", "))));
            };
            controls.bound.retain(|&(_, j)| j != i);
            for name in keys.split(',').map(str::trim).filter(|k| !k.is_empty()) {
                let key = keys::named(name).ok_or_else(|| at(format!("{name}: no such key")))?;
                controls.bound.retain(|&(k, _)| k != key);
                controls.bound.push((key, i));
            }
        }
        Ok(controls)
    }

    /// The host's keys bound to the action at `i`.
    pub fn keys_of(&self, i: usize) -> Vec<Key> {
        self.bound.iter().filter(|&&(_, j)| j == i).map(|&(k, _)| k).collect()
    }

    /// What the host's keys pressed and let go in a frame give the port, the presses first: an action's PC input
    /// when it begins and ends (two of its keys held, it ends with the last), a key bound to none its own.
    pub(crate) fn give(&mut self, pressed: &[Key], released: &[Key]) -> Given {
        let mut given = Given::default();
        for (keys, down) in [(pressed, true), (released, false)] {
            for &key in keys {
                match self.bound.iter().find(|&&(k, _)| k == key) {
                    Some(&(_, i)) => self.action(i, down, &mut given),
                    None => {
                        if let Some((grey, code)) = keys::scancode(key) {
                            given.scancodes.extend(grey.then_some(0xE0));
                            given.scancodes.push(if down { code } else { code | 0x80 });
                        }
                    }
                }
            }
        }
        for (action, &down) in self.actions.iter().zip(&self.down) {
            if let Pc::Button(b) = action.pc
                && down > 0
            {
                given.buttons |= 1 << b;
            }
        }
        given
    }

    fn action(&mut self, i: usize, down: bool, given: &mut Given) {
        let count = &mut self.down[i];
        let was = *count > 0;
        *count = if down { count.saturating_add(1) } else { count.saturating_sub(1) };
        let is = *count > 0;
        if was == is {
            return;
        }
        let action = &self.actions[i];
        given.actions.push((action.name, is));
        match action.pc.scancodes(is) {
            Some(bytes) => given.scancodes.extend(bytes),
            // A press, even let go within the frame (D145).
            None => {
                if let Pc::Button(b) = action.pc
                    && is
                {
                    given.buttons |= 1 << b;
                }
            }
        }
    }
}

/// The port's actions, each with its PC input and the host's keys bound to it, a line each: for `play --actions`.
pub(crate) fn list(controls: &Controls) -> String {
    let mut out = String::new();
    for (i, action) in controls.actions.iter().enumerate() {
        let pc = match action.pc {
            Pc::Key(code) => format!("key {code:02X}h"),
            Pc::GreyKey(code) => format!("key E0h {code:02X}h"),
            Pc::Button(b) => format!("button {b}"),
        };
        let keys: Vec<String> = controls.keys_of(i).iter().map(|k| format!("{k:?}")).collect();
        let keys = if keys.is_empty() { "none".to_owned() } else { keys.join(", ") };
        out.push_str(&format!("action    {}: {} ({pc}); keys {keys}\n", action.name, action.what));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIONS: &[Action] = &[
        Action { name: "forward", pc: Pc::GreyKey(0x48), what: "walk forward" },
        Action { name: "map", pc: Pc::Key(0x32), what: "the automap" },
        Action { name: "swing", pc: Pc::Button(1), what: "the weapon" },
    ];

    #[test]
    fn the_neutral_binding_sends_every_key_as_its_own() {
        let mut c = Controls::new(ACTIONS);
        assert_eq!((c.keys_of(0), c.keys_of(1), c.keys_of(2)), (vec![Key::Up], vec![Key::M], vec![]));
        let g = c.give(&[Key::Up, Key::M, Key::A], &[Key::Up]);
        assert_eq!(g.scancodes, [0xE0, 0x48, 0x32, 0x1E, 0xE0, 0xC8]);
        assert_eq!(g.actions, [("forward", true), ("map", true), ("forward", false)]);
        assert_eq!(g.buttons, 0);
    }

    #[test]
    fn a_controls_file_binds_actions_to_other_keys() {
        let mut c = Controls::read(ACTIONS, "# keys\nforward = W, Up\n\nswing = Space # the right button\n").unwrap();
        assert_eq!((c.keys_of(0), c.keys_of(2)), (vec![Key::W, Key::Up], vec![Key::Space]));
        // W sends the original's Up; with both held, the action ends with the last let go.
        let g = c.give(&[Key::W], &[]);
        assert_eq!((g.scancodes, g.actions), (vec![0xE0, 0x48], vec![("forward", true)]));
        let g = c.give(&[Key::Up], &[Key::W]);
        assert_eq!((g.scancodes, g.actions), (vec![], vec![]));
        let g = c.give(&[], &[Key::Up]);
        assert_eq!(g.scancodes, [0xE0, 0xC8]);
        // Space holds the right button, and a press let go within a frame is still one.
        assert_eq!(c.give(&[Key::Space], &[]).buttons, 2);
        assert_eq!(c.give(&[], &[]).buttons, 2);
        assert_eq!(c.give(&[], &[Key::Space]).buttons, 0);
        assert_eq!(c.give(&[Key::Space], &[Key::Space]).buttons, 2);
        // A key bound elsewhere does that action only: M bound to forward no longer opens the map.
        let mut c = Controls::read(ACTIONS, "forward = M").unwrap();
        assert_eq!((c.keys_of(0), c.keys_of(1)), (vec![Key::M], vec![]));
        assert_eq!(c.give(&[Key::Up, Key::M], &[]).scancodes, [0xE0, 0x48, 0xE0, 0x48]);
    }

    #[test]
    fn a_wrong_controls_file_is_refused_by_its_line() {
        assert!(Controls::read(ACTIONS, "jump = Space").unwrap_err().starts_with("line 1: jump: no such action"));
        assert!(Controls::read(ACTIONS, "\nforward = Up, Hyper").unwrap_err().starts_with("line 2: Hyper"));
        assert!(Controls::read(ACTIONS, "forward Up").is_err());
        assert!(list(&Controls::new(ACTIONS)).contains("action    forward: walk forward (key E0h 48h); keys Up\n"));
    }
}
