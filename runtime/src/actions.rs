//! The actions seam (docs/design.md §6.1, D162): what the player does, by name, not by device. A port declares its
//! game's actions, each with the PC input the original takes for it (a key by its scan code, a mouse button). The
//! platform binds the host's devices to the actions and gives each action to the port as that PC input
//! (`rrc_platform`'s controls), so a faithful port's input handling, and every comparison, are the same whatever the
//! bindings: a binding is a presentation toggle (§6.4). From the boundary values an action is read back as the
//! original's input code reads its key ([`Action::held`]); a modern mode (M7) reads the actions themselves.
//!
//! ```
//! use rrc_runtime::actions::{Action, Pc};
//! const ACTIONS: &[Action] = &[
//!     Action { name: "forward", pc: Pc::GreyKey(0x48), what: "walk forward" },
//!     Action { name: "use", pc: Pc::Key(0x16), what: "use an item" },
//!     Action { name: "attack", pc: Pc::Button(1), what: "ready and swing the weapon" },
//! ];
//! assert!(rrc_runtime::actions::check(ACTIONS).is_ok());
//! let mut key_down = [0u8; 128];
//! key_down[0x48] = 1;
//! assert!(ACTIONS[0].held(&key_down, 0) && !ACTIONS[1].held(&key_down, 0) && ACTIONS[2].held(&key_down, 2));
//! ```

/// The PC input the original takes for an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pc {
    /// A key, by its scan code (set 1, below 80h).
    Key(u8),
    /// A grey key (an arrow, Insert, the right Ctrl...), its scan code sent after E0h.
    GreyKey(u8),
    /// A mouse button: 0 the left, 1 the right, 2 the middle.
    Button(u8),
}

impl Pc {
    /// The keyboard controller's bytes for this key pressed (`down`) or let go: its code, with 80h when let go, after
    /// E0h for a grey key. None for a button.
    pub fn scancodes(self, down: bool) -> Option<Vec<u8>> {
        let up = if down { 0 } else { 0x80 };
        match self {
            Pc::Key(code) => Some(vec![code | up]),
            Pc::GreyKey(code) => Some(vec![0xE0, code | up]),
            Pc::Button(_) => None,
        }
    }
}

/// One of a game's actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    /// Its name, as a controls file binds it: lower case letters, digits and `_`.
    pub name: &'static str,
    /// The input the original takes for it.
    pub pc: Pc,
    /// What it does, for a person choosing keys.
    pub what: &'static str,
}

impl Action {
    /// Its key's place in a key-state array kept by scan code (a keyboard handler's: a grey key at its code's place,
    /// as such a handler keeps it); none for a button.
    pub fn key(&self) -> Option<usize> {
        match self.pc {
            Pc::Key(code) | Pc::GreyKey(code) => Some(usize::from(code & 0x7F)),
            Pc::Button(_) => None,
        }
    }

    /// Whether it is held in the boundary values, as the original's input code reads it: its key in a key-state array
    /// kept by scan code ([`Action::key`]), or its button among the mouse's.
    pub fn held(&self, key_states: &[u8], buttons: u8) -> bool {
        match self.pc {
            Pc::Button(b) => b < 8 && buttons & (1 << b) != 0,
            _ => self.key().and_then(|k| key_states.get(k)).is_some_and(|&s| s != 0),
        }
    }
}

/// The action named `name` among `actions`, and its place.
pub fn named<'a>(actions: &'a [Action], name: &str) -> Option<(usize, &'a Action)> {
    actions.iter().enumerate().find(|(_, a)| a.name == name)
}

/// Whether a game's actions are well made: each name given once, of lower case letters, digits and `_`; each key's
/// scan code below 80h and each button 0 to 2. The first that isn't, said.
pub fn check(actions: &[Action]) -> Result<(), String> {
    for (i, action) in actions.iter().enumerate() {
        let name = action.name;
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
            return Err(format!("{name:?}: an action's name is lower case letters, digits and _"));
        }
        if actions[..i].iter().any(|a| a.name == name) {
            return Err(format!("{name}: two actions of that name"));
        }
        match action.pc {
            Pc::Key(code) | Pc::GreyKey(code) if code >= 0x80 => {
                return Err(format!("{name}: scan code {code:02X}h is a release's; a key's is below 80h"));
            }
            Pc::Button(b) if b > 2 => return Err(format!("{name}: button {b}: a mouse's are 0 to 2")),
            _ => {}
        }
    }
    Ok(())
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
    fn an_action_is_its_pc_input() {
        assert_eq!(Pc::GreyKey(0x48).scancodes(true), Some(vec![0xE0, 0x48]));
        assert_eq!(Pc::GreyKey(0x48).scancodes(false), Some(vec![0xE0, 0xC8]));
        assert_eq!(Pc::Key(0x32).scancodes(false), Some(vec![0xB2]));
        assert_eq!(Pc::Button(1).scancodes(true), None);
        assert_eq!(named(ACTIONS, "map").map(|(i, a)| (i, a.pc)), Some((1, Pc::Key(0x32))));
        assert!(named(ACTIONS, "jump").is_none());
    }

    #[test]
    fn an_action_is_read_back_from_the_boundary_values() {
        let mut keys = [0u8; 128];
        keys[0x32] = 1;
        assert!(!ACTIONS[0].held(&keys, 0) && ACTIONS[1].held(&keys, 0));
        assert!(ACTIONS[2].held(&keys, 0b10) && !ACTIONS[2].held(&keys, 0b01));
        assert!(!ACTIONS[1].held(&[], 0), "a short array holds nothing");
        assert_eq!((ACTIONS[0].key(), ACTIONS[1].key(), ACTIONS[2].key()), (Some(0x48), Some(0x32), None));
    }

    #[test]
    fn a_game_s_actions_are_checked() {
        assert!(check(ACTIONS).is_ok());
        let twice = [ACTIONS[0], ACTIONS[0]];
        assert!(check(&twice).unwrap_err().contains("two actions"));
        let name = [Action { name: "Walk", ..ACTIONS[0] }];
        assert!(check(&name).is_err());
        let release = [Action { pc: Pc::Key(0xC8), ..ACTIONS[0] }];
        assert!(check(&release).unwrap_err().contains("release"));
        let button = [Action { pc: Pc::Button(3), ..ACTIONS[2] }];
        assert!(check(&button).is_err());
    }
}
