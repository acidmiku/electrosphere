//! The keyboard as port 1's buttons: `[controls.keys]` gives each
//! PlayStation button its keys (physical positions, by winit's key code
//! names: `KeyW`, `ArrowUp`, `Space`), rebound in the launcher.
//!
//! The default layout flies like a PC flight game, with the game's own
//! button functions (measured in mission 1, the default key config): the
//! arrows pitch and roll (the d-pad, or the left stick on a DualShock),
//! W / S speed up and slow down (R1 / L1), A / D yaw (L2 / R2), Space or J
//! square, K cross, L circle, I triangle, Enter START, Backspace SELECT.
//! Escape and the function keys are the window's (quit, F2 to F9) and
//! cannot be bound.
use crate::gamepad::PsButton;
use serde::{Deserialize, Serialize};
use winit::keyboard::KeyCode;

/// The keys that can be bound: their name in the settings file, the name
/// shown, and the key.
pub const KEYS: &[(&str, &str, KeyCode)] = &[
    ("ArrowUp", "Up", KeyCode::ArrowUp),
    ("ArrowDown", "Down", KeyCode::ArrowDown),
    ("ArrowLeft", "Left", KeyCode::ArrowLeft),
    ("ArrowRight", "Right", KeyCode::ArrowRight),
    ("KeyA", "A", KeyCode::KeyA),
    ("KeyB", "B", KeyCode::KeyB),
    ("KeyC", "C", KeyCode::KeyC),
    ("KeyD", "D", KeyCode::KeyD),
    ("KeyE", "E", KeyCode::KeyE),
    ("KeyF", "F", KeyCode::KeyF),
    ("KeyG", "G", KeyCode::KeyG),
    ("KeyH", "H", KeyCode::KeyH),
    ("KeyI", "I", KeyCode::KeyI),
    ("KeyJ", "J", KeyCode::KeyJ),
    ("KeyK", "K", KeyCode::KeyK),
    ("KeyL", "L", KeyCode::KeyL),
    ("KeyM", "M", KeyCode::KeyM),
    ("KeyN", "N", KeyCode::KeyN),
    ("KeyO", "O", KeyCode::KeyO),
    ("KeyP", "P", KeyCode::KeyP),
    ("KeyQ", "Q", KeyCode::KeyQ),
    ("KeyR", "R", KeyCode::KeyR),
    ("KeyS", "S", KeyCode::KeyS),
    ("KeyT", "T", KeyCode::KeyT),
    ("KeyU", "U", KeyCode::KeyU),
    ("KeyV", "V", KeyCode::KeyV),
    ("KeyW", "W", KeyCode::KeyW),
    ("KeyX", "X", KeyCode::KeyX),
    ("KeyY", "Y", KeyCode::KeyY),
    ("KeyZ", "Z", KeyCode::KeyZ),
    ("Digit0", "0", KeyCode::Digit0),
    ("Digit1", "1", KeyCode::Digit1),
    ("Digit2", "2", KeyCode::Digit2),
    ("Digit3", "3", KeyCode::Digit3),
    ("Digit4", "4", KeyCode::Digit4),
    ("Digit5", "5", KeyCode::Digit5),
    ("Digit6", "6", KeyCode::Digit6),
    ("Digit7", "7", KeyCode::Digit7),
    ("Digit8", "8", KeyCode::Digit8),
    ("Digit9", "9", KeyCode::Digit9),
    ("Space", "Space", KeyCode::Space),
    ("Enter", "Enter", KeyCode::Enter),
    ("Tab", "Tab", KeyCode::Tab),
    ("Backspace", "Backspace", KeyCode::Backspace),
    ("ShiftLeft", "Left Shift", KeyCode::ShiftLeft),
    ("ShiftRight", "Right Shift", KeyCode::ShiftRight),
    ("ControlLeft", "Left Ctrl", KeyCode::ControlLeft),
    ("ControlRight", "Right Ctrl", KeyCode::ControlRight),
    ("AltLeft", "Left Alt", KeyCode::AltLeft),
    ("AltRight", "Right Alt", KeyCode::AltRight),
    ("CapsLock", "Caps Lock", KeyCode::CapsLock),
    ("Minus", "-", KeyCode::Minus),
    ("Equal", "=", KeyCode::Equal),
    ("BracketLeft", "[", KeyCode::BracketLeft),
    ("BracketRight", "]", KeyCode::BracketRight),
    ("Backslash", "\\", KeyCode::Backslash),
    ("Semicolon", ";", KeyCode::Semicolon),
    ("Quote", "'", KeyCode::Quote),
    ("Backquote", "`", KeyCode::Backquote),
    ("Comma", ",", KeyCode::Comma),
    ("Period", ".", KeyCode::Period),
    ("Slash", "/", KeyCode::Slash),
    ("Insert", "Insert", KeyCode::Insert),
    ("Delete", "Delete", KeyCode::Delete),
    ("Home", "Home", KeyCode::Home),
    ("End", "End", KeyCode::End),
    ("PageUp", "Page Up", KeyCode::PageUp),
    ("PageDown", "Page Down", KeyCode::PageDown),
    ("Numpad0", "Num 0", KeyCode::Numpad0),
    ("Numpad1", "Num 1", KeyCode::Numpad1),
    ("Numpad2", "Num 2", KeyCode::Numpad2),
    ("Numpad3", "Num 3", KeyCode::Numpad3),
    ("Numpad4", "Num 4", KeyCode::Numpad4),
    ("Numpad5", "Num 5", KeyCode::Numpad5),
    ("Numpad6", "Num 6", KeyCode::Numpad6),
    ("Numpad7", "Num 7", KeyCode::Numpad7),
    ("Numpad8", "Num 8", KeyCode::Numpad8),
    ("Numpad9", "Num 9", KeyCode::Numpad9),
    ("NumpadAdd", "Num +", KeyCode::NumpadAdd),
    ("NumpadSubtract", "Num -", KeyCode::NumpadSubtract),
    ("NumpadMultiply", "Num *", KeyCode::NumpadMultiply),
    ("NumpadDivide", "Num /", KeyCode::NumpadDivide),
    ("NumpadDecimal", "Num .", KeyCode::NumpadDecimal),
    ("NumpadEnter", "Num Enter", KeyCode::NumpadEnter),
];

/// The key named `name` (`KeyW`).
pub fn code(name: &str) -> Option<KeyCode> {
    KEYS.iter()
        .find(|(n, _, _)| *n == name)
        .map(|&(_, _, code)| code)
}

/// The settings name of `code`, if it can be bound.
pub fn name(code: KeyCode) -> Option<&'static str> {
    KEYS.iter()
        .find(|(_, _, c)| *c == code)
        .map(|&(name, _, _)| name)
}

/// The name shown for the key named `name` (`W`, `Left Shift`).
pub fn label(name: &str) -> &str {
    KEYS.iter()
        .find(|(n, _, _)| *n == name)
        .map_or(name, |&(_, label, _)| label)
}

/// Keys the game window keeps for itself: Escape (quit) and the function
/// keys (F2 CRT, F3 frame rate, F4 curvature, F5 / F8 quick save and load,
/// F6 effects, F7 discs, F9 reload).
pub fn reserved(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Escape
            | KeyCode::F1
            | KeyCode::F2
            | KeyCode::F3
            | KeyCode::F4
            | KeyCode::F5
            | KeyCode::F6
            | KeyCode::F7
            | KeyCode::F8
            | KeyCode::F9
            | KeyCode::F10
            | KeyCode::F11
            | KeyCode::F12
    )
}

/// The keys of each PlayStation button (`[controls.keys]`; up to two each
/// in the launcher, any number in the file).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyMap {
    pub up: Vec<String>,
    pub down: Vec<String>,
    pub left: Vec<String>,
    pub right: Vec<String>,
    pub cross: Vec<String>,
    pub circle: Vec<String>,
    pub square: Vec<String>,
    pub triangle: Vec<String>,
    pub l1: Vec<String>,
    pub r1: Vec<String>,
    pub l2: Vec<String>,
    pub r2: Vec<String>,
    pub l3: Vec<String>,
    pub r3: Vec<String>,
    pub select: Vec<String>,
    pub start: Vec<String>,
}

impl Default for KeyMap {
    fn default() -> Self {
        let keys = |names: &[&str]| names.iter().map(|&name| name.to_owned()).collect();
        Self {
            up: keys(&["ArrowUp"]),
            down: keys(&["ArrowDown"]),
            left: keys(&["ArrowLeft"]),
            right: keys(&["ArrowRight"]),
            cross: keys(&["KeyK"]),
            circle: keys(&["KeyL"]),
            square: keys(&["Space", "KeyJ"]),
            triangle: keys(&["KeyI"]),
            l1: keys(&["KeyS"]),
            r1: keys(&["KeyW"]),
            l2: keys(&["KeyA"]),
            r2: keys(&["KeyD"]),
            l3: Vec::new(),
            r3: Vec::new(),
            select: keys(&["Backspace"]),
            start: keys(&["Enter"]),
        }
    }
}

impl KeyMap {
    /// Each button and its keys, in the launcher's order.
    pub fn buttons(&self) -> [(PsButton, &Vec<String>); 16] {
        [
            (PsButton::Up, &self.up),
            (PsButton::Down, &self.down),
            (PsButton::Left, &self.left),
            (PsButton::Right, &self.right),
            (PsButton::R1, &self.r1),
            (PsButton::L1, &self.l1),
            (PsButton::L2, &self.l2),
            (PsButton::R2, &self.r2),
            (PsButton::Square, &self.square),
            (PsButton::Cross, &self.cross),
            (PsButton::Circle, &self.circle),
            (PsButton::Triangle, &self.triangle),
            (PsButton::Start, &self.start),
            (PsButton::Select, &self.select),
            (PsButton::L3, &self.l3),
            (PsButton::R3, &self.r3),
        ]
    }

    /// The keys of `button`, to edit.
    pub fn keys_mut(&mut self, button: PsButton) -> Option<&mut Vec<String>> {
        Some(match button {
            PsButton::Up => &mut self.up,
            PsButton::Down => &mut self.down,
            PsButton::Left => &mut self.left,
            PsButton::Right => &mut self.right,
            PsButton::Cross => &mut self.cross,
            PsButton::Circle => &mut self.circle,
            PsButton::Square => &mut self.square,
            PsButton::Triangle => &mut self.triangle,
            PsButton::L1 => &mut self.l1,
            PsButton::R1 => &mut self.r1,
            PsButton::L2 => &mut self.l2,
            PsButton::R2 => &mut self.r2,
            PsButton::L3 => &mut self.l3,
            PsButton::R3 => &mut self.r3,
            PsButton::Select => &mut self.select,
            PsButton::Start => &mut self.start,
            PsButton::None => return None,
        })
    }

    /// Bind `key` to `button` in slot `slot` (0 or 1), taking it from any
    /// other button it held.
    pub fn bind(&mut self, button: PsButton, slot: usize, key: &str) {
        for keys in self.all_mut() {
            keys.retain(|k| k != key);
        }
        if let Some(keys) = self.keys_mut(button) {
            if slot < keys.len() {
                keys[slot] = key.to_owned();
            } else {
                keys.push(key.to_owned());
            }
        }
    }

    /// Unbind slot `slot` of `button`.
    pub fn clear(&mut self, button: PsButton, slot: usize) {
        if let Some(keys) = self.keys_mut(button)
            && slot < keys.len()
        {
            keys.remove(slot);
        }
    }

    fn all_mut(&mut self) -> [&mut Vec<String>; 16] {
        [
            &mut self.up,
            &mut self.down,
            &mut self.left,
            &mut self.right,
            &mut self.cross,
            &mut self.circle,
            &mut self.square,
            &mut self.triangle,
            &mut self.l1,
            &mut self.r1,
            &mut self.l2,
            &mut self.r2,
            &mut self.l3,
            &mut self.r3,
            &mut self.select,
            &mut self.start,
        ]
    }

    /// Key names that are not keys this build knows, or are the window's.
    pub fn unknown(&self) -> Vec<String> {
        self.buttons()
            .iter()
            .flat_map(|(_, keys)| keys.iter())
            .filter(|name| code(name).is_none_or(reserved))
            .cloned()
            .collect()
    }

    /// Each bound key and the button bit it holds, for the game window
    /// (names it does not know are skipped).
    pub fn bindings(&self) -> Vec<(KeyCode, u16)> {
        self.buttons()
            .iter()
            .flat_map(|(button, keys)| {
                keys.iter()
                    .filter_map(|name| code(name).filter(|&c| !reserved(c)))
                    .map(move |code| (code, button.bit()))
            })
            .collect()
    }
}

/// What a button does in flight with the game's default key config, where
/// it was measured (mission 1, DualShock); empty for the rest.
pub fn function(button: PsButton) -> &'static str {
    match button {
        PsButton::Up | PsButton::Down => "Pitch",
        PsButton::Left | PsButton::Right => "Roll",
        PsButton::R1 => "Speed up",
        PsButton::L1 => "Slow down",
        PsButton::L2 => "Yaw left",
        PsButton::R2 => "Yaw right",
        PsButton::Start => "Pause",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_player::buttons;

    #[test]
    fn names_codes_and_labels() {
        assert_eq!(code("KeyW"), Some(KeyCode::KeyW));
        assert_eq!(name(KeyCode::ShiftLeft), Some("ShiftLeft"));
        assert_eq!(label("ShiftLeft"), "Left Shift");
        assert_eq!(label("Unknown"), "Unknown");
        assert_eq!(code("F5"), None);
        assert!(reserved(KeyCode::Escape) && reserved(KeyCode::F2));
        assert!(!reserved(KeyCode::KeyW));
        for (name, _, code) in KEYS {
            assert_eq!(self::name(*code), Some(*name), "unique");
            assert!(!reserved(*code));
        }
    }

    #[test]
    fn the_default_layout() {
        let map = KeyMap::default();
        assert!(map.unknown().is_empty());
        let held = |key| {
            map.bindings()
                .iter()
                .filter(|(code, _)| *code == key)
                .fold(0, |bits, (_, bit)| bits | bit)
        };
        assert_eq!(held(KeyCode::ArrowUp), buttons::UP);
        assert_eq!(held(KeyCode::KeyW), buttons::R1);
        assert_eq!(held(KeyCode::KeyS), buttons::L1);
        assert_eq!(held(KeyCode::KeyA), buttons::L2);
        assert_eq!(held(KeyCode::KeyD), buttons::R2);
        assert_eq!(held(KeyCode::Space), buttons::SQUARE);
        assert_eq!(held(KeyCode::KeyJ), buttons::SQUARE);
        assert_eq!(held(KeyCode::Enter), buttons::START);
        assert_eq!(held(KeyCode::KeyX), 0);
    }

    #[test]
    fn binding_moves_a_key_and_clearing_frees_a_slot() {
        let mut map = KeyMap::default();
        map.bind(PsButton::Cross, 1, "Space");
        assert_eq!(map.cross, ["KeyK", "Space"]);
        assert_eq!(map.square, ["KeyJ"], "taken from square");
        map.bind(PsButton::Cross, 0, "KeyX");
        assert_eq!(map.cross, ["KeyX", "Space"]);
        map.clear(PsButton::Cross, 0);
        assert_eq!(map.cross, ["Space"]);
        map.clear(PsButton::Cross, 5);
        assert_eq!(map.cross, ["Space"]);
        map.l3.push("F5".into());
        map.r3.push("Nope".into());
        assert_eq!(map.unknown(), ["F5", "Nope"]);
        assert!(map.bindings().iter().all(|(code, _)| !reserved(*code)));
    }
}
