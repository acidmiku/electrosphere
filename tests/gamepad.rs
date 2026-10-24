//! Gamepad settings in TOML and the gilrs backend without a pad.
use ac3_port::gamepad::{Bindings, ButtonMap, Gamepads, Motors, PadChoice, PsButton};
use ac3_port::machine_player::PadType;

/// The settings file's shape: `Bindings` is its `[controls]` table.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Default)]
#[serde(default)]
struct Settings {
    controls: Bindings,
}

#[test]
fn bindings_round_trip_through_toml() {
    let defaults = Settings::default();
    let text = toml::to_string(&defaults).unwrap();
    assert_eq!(toml::from_str::<Settings>(&text).unwrap(), defaults);
    for line in [
        "[controls]",
        "pad_type = \"auto\"",
        "deadzone = 0.15",
        "trigger_threshold = 0.3",
        "rumble = true",
        "[controls.buttons]",
        "a = \"cross\"",
        "b = \"circle\"",
        "x = \"square\"",
        "y = \"triangle\"",
        "lb = \"l1\"",
        "rt = \"r2\"",
        "back = \"select\"",
        "start = \"start\"",
        "dpad_up = \"up\"",
        "left_stick = \"l3\"",
        "guide = \"none\"",
    ] {
        assert!(text.lines().any(|l| l == line), "{line} missing in\n{text}");
    }

    let custom = Settings {
        controls: Bindings {
            pad_type: PadChoice::DualShock,
            deadzone: 0.25,
            trigger_threshold: 0.6,
            rumble: false,
            compensate_deadzone: false,
            swap_triggers: true,
            keys: ac3_port::keyboard::KeyMap::default(),
            buttons: ButtonMap {
                a: PsButton::Circle,
                guide: PsButton::Select,
                rt: PsButton::None,
                ..ButtonMap::default()
            },
        },
    };
    let text = toml::to_string(&custom).unwrap();
    assert_eq!(toml::from_str::<Settings>(&text).unwrap(), custom);
}

#[test]
fn partial_controls_keep_their_defaults() {
    let text = "[controls]\n\
                rumble = false\n\
                [controls.buttons]\n\
                view = \"start\"\n\
                menu = \"select\"\n\
                lt = \"none\"\n";
    let settings: Settings = toml::from_str(text).unwrap();
    let expected = Bindings {
        rumble: false,
        buttons: ButtonMap {
            back: PsButton::Start,
            start: PsButton::Select,
            lt: PsButton::None,
            ..ButtonMap::default()
        },
        ..Bindings::default()
    };
    assert_eq!(settings.controls, expected);
    assert_eq!(
        toml::from_str::<Settings>("").unwrap().controls,
        Bindings::default()
    );
    assert!(toml::from_str::<Settings>("[controls]\npad_type = \"joystick\"\n").is_err());
    assert!(toml::from_str::<Settings>("[controls.buttons]\na = \"jump\"\n").is_err());
}

#[test]
fn backend_starts_without_a_pad() {
    // Without a controller the backend starts, or reports why it cannot;
    // either way every poll is empty and rumble is a no-op.
    let mut pads = Gamepads::new(&Bindings::default());
    if pads.connected() {
        eprintln!(
            "a gamepad is connected ({:?}); no-pad checks skipped",
            pads.names()
        );
        return;
    }
    for _ in 0..3 {
        let state = pads.poll();
        assert!(!state.connected);
        assert_eq!(state.buttons, 0);
        assert_eq!(state.sticks, None);
    }
    pads.rumble(Motors {
        small: true,
        large: 0xff,
    });
    pads.stop_rumble();
    assert_eq!(
        pads.bindings().pad_type.resolve(pads.connected()),
        PadType::Digital
    );
}
