//! Host gamepads for controller port 1: [`gilrs`] input mapped onto the
//! PlayStation controller the machine emulates, and the game's DualShock
//! vibration played as rumble.
//!
//! - **Backends.** Windows reads XInput (gilrs' `xinput` feature): Xbox
//!   360/One/Series pads and everything presenting itself as one (Steam
//!   Input, DS4Windows, most third-party pads), whether or not the window has
//!   focus, with rumble through `XInputSetState`. Linux reads evdev
//!   (`/dev/input/event*`, found through udev) and also covers DualShock 4/5
//!   and other SDL-mapped pads. With no backend or no pad, every poll reports
//!   nothing and the keyboard works as before.
//! - **Mapping.** [`Bindings`] (the `[controls]` table of `ac3.toml`) gives
//!   each gamepad button a PlayStation button by position (A ✕, B ○, X □,
//!   Y △). The analog triggers press L2/R2 past a threshold. The sticks
//!   become DualShock analog bytes after a radial dead zone ([`stick_bytes`]:
//!   0x00..0xFF, 0x80 centre, up and left 0x00).
//! - **The game's dead zone.** The game zeroes each stick axis on its own
//!   within a large dead zone of its DualShock calibration (KEY CONFIG's
//!   response; by default 60 % of each half axis), which drops the smaller
//!   axis of a diagonal and every small movement. [`compensate_sticks`]
//!   carries each moved axis past it, so the game reads the direction and
//!   amount the stick has (`[controls] compensate_deadzone`, on by default).
//! - **Hot-plug.** [`Gamepads::poll`] follows the pad used last and falls
//!   back to any connected one.
//! - **Port 1.** [`PadChoice`] picks the emulated controller at start (by
//!   default a DualShock when a gamepad is connected, else the digital pad
//!   the game has always had). [`port_input`] merges the keyboard and the
//!   gamepad for it; on a DualShock the keyboard's arrows move the left
//!   stick, because the game's analog layout flies with the sticks.
//! - **Rumble.** [`Motors::from_actuators`] decodes what the game sent to a
//!   DualShock's two motors; [`Gamepads::rumble`] plays it: the small on/off
//!   motor on the pad's high-frequency (weak) motor, the large 0..255 motor on
//!   its low-frequency (strong) motor.
use crate::machine_player::{PadType, buttons};
use gilrs::{
    Axis, Button, EventType, GamepadId, Gilrs, GilrsBuilder,
    ff::{BaseEffect, BaseEffectType, Effect, EffectBuilder, Repeat, Replay},
};
use serde::{Deserialize, Serialize};

/// L3 and R3, the stick clicks: DualShock button bits 1 and 2 (a digital pad
/// always reports them released).
pub const L3: u16 = 1 << 1;
pub const R3: u16 = 1 << 2;
/// Both stick clicks.
pub const STICK_CLICKS: u16 = L3 | R3;
/// Centred sticks.
pub const CENTRE: [u8; 4] = [0x80; 4];

/// The gamepad input backend of this build.
pub const BACKEND: &str = if cfg!(windows) {
    "XInput"
} else if cfg!(target_os = "linux") {
    "evdev"
} else if cfg!(target_os = "macos") {
    "IOKit"
} else {
    "none"
};

// ---- settings -------------------------------------------------------------------------

/// A PlayStation button a gamepad button can press.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PsButton {
    /// Unbound.
    #[default]
    None,
    Cross,
    Circle,
    Square,
    Triangle,
    L1,
    R1,
    L2,
    R2,
    L3,
    R3,
    Select,
    Start,
    Up,
    Down,
    Left,
    Right,
}

impl PsButton {
    /// The button's libpad bit (active-high, `machine_player::buttons`).
    pub fn bit(self) -> u16 {
        match self {
            Self::None => 0,
            Self::Cross => buttons::CROSS,
            Self::Circle => buttons::CIRCLE,
            Self::Square => buttons::SQUARE,
            Self::Triangle => buttons::TRIANGLE,
            Self::L1 => buttons::L1,
            Self::R1 => buttons::R1,
            Self::L2 => buttons::L2,
            Self::R2 => buttons::R2,
            Self::L3 => L3,
            Self::R3 => R3,
            Self::Select => buttons::SELECT,
            Self::Start => buttons::START,
            Self::Up => buttons::UP,
            Self::Down => buttons::DOWN,
            Self::Left => buttons::LEFT,
            Self::Right => buttons::RIGHT,
        }
    }
}

/// The PlayStation button of each gamepad button, named by the Xbox layout
/// (`[controls.buttons]`). On a PlayStation pad read through gilrs the
/// positions are the same: `a` is ✕, `b` ○, `x` □, `y` △, `back` Share or
/// Create, `start` Options, `guide` the PS button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ButtonMap {
    pub a: PsButton,
    pub b: PsButton,
    pub x: PsButton,
    pub y: PsButton,
    pub lb: PsButton,
    pub rb: PsButton,
    /// The analog triggers, pressed past [`Bindings::trigger_threshold`].
    pub lt: PsButton,
    pub rt: PsButton,
    /// View (Xbox One and later) or Back.
    #[serde(alias = "view")]
    pub back: PsButton,
    /// Menu (Xbox One and later) or Start.
    #[serde(alias = "menu")]
    pub start: PsButton,
    pub dpad_up: PsButton,
    pub dpad_down: PsButton,
    pub dpad_left: PsButton,
    pub dpad_right: PsButton,
    /// The stick clicks.
    #[serde(alias = "ls")]
    pub left_stick: PsButton,
    #[serde(alias = "rs")]
    pub right_stick: PsButton,
    /// The Xbox (guide) button; unbound by default.
    pub guide: PsButton,
}

impl Default for ButtonMap {
    fn default() -> Self {
        Self {
            a: PsButton::Cross,
            b: PsButton::Circle,
            x: PsButton::Square,
            y: PsButton::Triangle,
            lb: PsButton::L1,
            rb: PsButton::R1,
            lt: PsButton::L2,
            rt: PsButton::R2,
            back: PsButton::Select,
            start: PsButton::Start,
            dpad_up: PsButton::Up,
            dpad_down: PsButton::Down,
            dpad_left: PsButton::Left,
            dpad_right: PsButton::Right,
            left_stick: PsButton::L3,
            right_stick: PsButton::R3,
            guide: PsButton::None,
        }
    }
}

/// The controller on port 1 (`pad_type`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PadChoice {
    /// A DualShock when a gamepad is connected at start, else a digital pad.
    #[default]
    Auto,
    Digital,
    DualShock,
}

impl PadChoice {
    /// The machine's port 1 controller, given whether a gamepad is connected
    /// when the machine starts.
    pub fn resolve(self, gamepad_connected: bool) -> PadType {
        match self {
            Self::Auto if gamepad_connected => PadType::DualShock,
            Self::Auto | Self::Digital => PadType::Digital,
            Self::DualShock => PadType::DualShock,
        }
    }
}

/// Gamepad settings: the `[controls]` table of `ac3.toml`. Missing keys
/// keep their defaults; [`Gamepads::new`] clamps values out of range
/// ([`Bindings::sanitized`]).
///
/// ```toml
/// [controls]
/// pad_type = "auto"         # auto, digital or dualshock
/// deadzone = 0.15           # radial stick dead zone, 0.0 .. 0.9
/// trigger_threshold = 0.3   # LT/RT pull that presses L2/R2, 0.05 .. 1.0
/// rumble = true
/// compensate_deadzone = true # carry the sticks past the game's dead zone
/// swap_triggers = false     # LT/RT press what LB/RB would, and back
///
/// [controls.buttons]        # gamepad button = PlayStation button or "none"
/// a = "cross"
/// lt = "l2"
/// guide = "none"
///
/// [controls.keys]           # PlayStation button = its keys (crate::keyboard)
/// r1 = ["KeyW"]
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bindings {
    pub pad_type: PadChoice,
    /// Radial stick dead zone as a fraction of full deflection; outside it
    /// the range is rescaled, so movement starts from the centre value.
    pub deadzone: f64,
    /// How far an analog trigger must be pulled (0..1) to press its button.
    pub trigger_threshold: f64,
    /// Play the game's DualShock vibration on the gamepad.
    pub rumble: bool,
    /// Carry each moved stick axis past the game's own per-axis dead zone
    /// ([`compensate_sticks`]); off, the game gets the bytes a DualShock
    /// would send.
    pub compensate_deadzone: bool,
    /// The triggers press what the bumpers are bound to, and the other way
    /// round: with the game's default key config, throttle on the triggers
    /// and yaw on the bumpers.
    pub swap_triggers: bool,
    pub buttons: ButtonMap,
    /// The keyboard (`[controls.keys]`).
    pub keys: crate::keyboard::KeyMap,
}

impl Default for Bindings {
    fn default() -> Self {
        Self {
            pad_type: PadChoice::Auto,
            deadzone: Self::DEADZONE,
            trigger_threshold: Self::TRIGGER_THRESHOLD,
            rumble: true,
            compensate_deadzone: true,
            swap_triggers: false,
            buttons: ButtonMap::default(),
            keys: crate::keyboard::KeyMap::default(),
        }
    }
}

impl Bindings {
    pub const DEADZONE: f64 = 0.15;
    pub const TRIGGER_THRESHOLD: f64 = 0.3;

    /// These settings with the dead zone clamped to 0.0..0.9 and the trigger
    /// threshold to 0.05..1.0 (a value that is not a number: the default).
    pub fn sanitized(mut self) -> Self {
        let clamp = |value: f64, low: f64, high: f64, default: f64| {
            if value.is_nan() {
                default
            } else {
                value.clamp(low, high)
            }
        };
        self.deadzone = clamp(self.deadzone, 0.0, 0.9, Self::DEADZONE);
        self.trigger_threshold = clamp(self.trigger_threshold, 0.05, 1.0, Self::TRIGGER_THRESHOLD);
        self
    }

    /// The port 1 input for the gamepad readings `pad`.
    pub fn state(&self, pad: &RawPad) -> PadState {
        let map = &self.buttons;
        let threshold = self.trigger_threshold as f32;
        let bumpers = (pad.lb, pad.rb);
        let triggers = (pad.lt >= threshold, pad.rt >= threshold);
        // Swapped, each trigger presses its bumper's button and back.
        let ((lb, rb), (lt, rt)) = if self.swap_triggers {
            (triggers, bumpers)
        } else {
            (bumpers, triggers)
        };
        let held = [
            (pad.a, map.a),
            (pad.b, map.b),
            (pad.x, map.x),
            (pad.y, map.y),
            (lb, map.lb),
            (rb, map.rb),
            (lt, map.lt),
            (rt, map.rt),
            (pad.back, map.back),
            (pad.start, map.start),
            (pad.dpad_up, map.dpad_up),
            (pad.dpad_down, map.dpad_down),
            (pad.dpad_left, map.dpad_left),
            (pad.dpad_right, map.dpad_right),
            (pad.left_stick, map.left_stick),
            (pad.right_stick, map.right_stick),
            (pad.guide, map.guide),
        ];
        let buttons = held
            .iter()
            .filter(|(down, _)| *down)
            .fold(0, |bits, (_, button)| bits | button.bit());
        let deadzone = self.deadzone as f32;
        let [lx, ly] = stick_bytes(pad.left, deadzone);
        let [rx, ry] = stick_bytes(pad.right, deadzone);
        PadState {
            buttons,
            sticks: Some([rx, ry, lx, ly]),
            connected: true,
        }
    }
}

// ---- pure mapping -----------------------------------------------------------------------

/// One gamepad's readings, in gilrs' terms: sticks `[x, y]` in -1.0..1.0
/// with up and right positive, triggers 0.0..1.0.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RawPad {
    pub a: bool,
    pub b: bool,
    pub x: bool,
    pub y: bool,
    pub lb: bool,
    pub rb: bool,
    pub back: bool,
    pub start: bool,
    pub guide: bool,
    pub left_stick: bool,
    pub right_stick: bool,
    pub dpad_up: bool,
    pub dpad_down: bool,
    pub dpad_left: bool,
    pub dpad_right: bool,
    pub lt: f32,
    pub rt: f32,
    pub left: [f32; 2],
    pub right: [f32; 2],
}

impl RawPad {
    fn read(pad: &gilrs::Gamepad<'_>) -> Self {
        let pressed = |button| pad.is_pressed(button);
        // gilrs reports a trigger's travel as its button value (0..1; a
        // digital trigger jumps 0 → 1).
        let trigger = |button| pad.button_data(button).map_or(0.0, |data| data.value());
        // Pads whose d-pad is a hat report it as axes (up positive).
        let (hat_x, hat_y) = (pad.value(Axis::DPadX), pad.value(Axis::DPadY));
        Self {
            a: pressed(Button::South),
            b: pressed(Button::East),
            x: pressed(Button::West),
            y: pressed(Button::North),
            lb: pressed(Button::LeftTrigger),
            rb: pressed(Button::RightTrigger),
            back: pressed(Button::Select),
            start: pressed(Button::Start),
            guide: pressed(Button::Mode),
            left_stick: pressed(Button::LeftThumb),
            right_stick: pressed(Button::RightThumb),
            dpad_up: pressed(Button::DPadUp) || hat_y > 0.5,
            dpad_down: pressed(Button::DPadDown) || hat_y < -0.5,
            dpad_left: pressed(Button::DPadLeft) || hat_x < -0.5,
            dpad_right: pressed(Button::DPadRight) || hat_x > 0.5,
            lt: trigger(Button::LeftTrigger2),
            rt: trigger(Button::RightTrigger2),
            left: [pad.value(Axis::LeftStickX), pad.value(Axis::LeftStickY)],
            right: [pad.value(Axis::RightStickX), pad.value(Axis::RightStickY)],
        }
    }
}

/// A stick position after a radial dead zone: centred inside `deadzone`;
/// outside it the distance from the centre is rescaled so that the edge of
/// the dead zone is 0 and the edge of a round gate stays 1 (square-gate
/// corners still reach both extremes).
pub fn apply_deadzone([x, y]: [f32; 2], deadzone: f32) -> [f32; 2] {
    let finite = |v: f32| {
        if v.is_finite() {
            v.clamp(-1.0, 1.0)
        } else {
            0.0
        }
    };
    let (x, y) = (finite(x), finite(y));
    let deadzone = if deadzone.is_finite() {
        deadzone.clamp(0.0, 0.9)
    } else {
        0.0
    };
    let distance = x.hypot(y);
    if distance <= deadzone || distance == 0.0 {
        return [0.0, 0.0];
    }
    let scale = (distance - deadzone) / (1.0 - deadzone) / distance;
    [(x * scale).clamp(-1.0, 1.0), (y * scale).clamp(-1.0, 1.0)]
}

/// One axis (-1.0..1.0) as a DualShock byte: 0x00 at -1, 0x80 at 0, 0xFF at
/// +1.
pub fn axis_byte(value: f32) -> u8 {
    let value = if value.is_finite() {
        value.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    if value >= 0.0 {
        0x80 + (value * 127.0).round() as u8
    } else {
        0x80 - (-value * 128.0).round() as u8
    }
}

/// A stick (gilrs orientation, up positive) as DualShock X and Y bytes after
/// the dead zone: left and up are 0x00, right and down 0xFF.
pub fn stick_bytes(stick: [f32; 2], deadzone: f32) -> [u8; 2] {
    let [x, y] = apply_deadzone(stick, deadzone);
    [axis_byte(x), axis_byte(-y)]
}

// ---- the game's dead zone ----------------------------------------------------------------

/// One stick axis of the game's DualShock calibration (8002895C reads four
/// {low, centre, high, dead zone} byte records): the game clamps the byte
/// to `low..=high` and reads it as 0 within `dead` of `centre`, else as
/// `±(offset - dead) / (high or low side - dead)` of full deflection. Each
/// axis on its own: a cross-shaped dead zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxisCalibration {
    pub low: u8,
    pub centre: u8,
    pub high: u8,
    pub dead: u8,
}

impl AxisCalibration {
    /// The game's default DualShock axis (the exe's template, PAL and JP;
    /// KEY CONFIG's middle response): 0x4C of 0x7F, 60 % of each half axis.
    pub const DEFAULT: Self = Self {
        low: 0x00,
        centre: 0x7f,
        high: 0xff,
        dead: 0x4c,
    };

    /// The byte that the game reads as `byte`'s deflection: a centred stick
    /// (0x80) stays; any other byte goes at least one step past the dead
    /// zone, then on in proportion to its deflection (full stays full).
    /// A record with no live range on that side leaves the byte as it is.
    pub fn compensate(self, byte: u8) -> u8 {
        let (low, centre, high, dead) = (
            i32::from(self.low),
            i32::from(self.centre),
            i32::from(self.high),
            i32::from(self.dead),
        );
        let (amount, span, sign) = match byte {
            0x80 => return byte,
            0x81.. => (f32::from(byte - 0x80) / 127.0, high - centre - dead, 1),
            _ => (f32::from(0x80 - byte) / 128.0, centre - low - dead, -1),
        };
        if span < 1 {
            return byte;
        }
        let live = ((amount * span as f32).round() as i32).clamp(1, span);
        (centre + sign * (dead + live)).clamp(low, high) as u8
    }
}

/// `sticks` (`[right X, right Y, left X, left Y]`, as `Host::sticks`) with
/// every moved axis carried past the game's dead zone, `calibration` being
/// the game's four records in its own order (right Y, right X, left Y,
/// left X: the pad state's analog halfwords +0x12..+0x18).
pub fn compensate_sticks(sticks: [u8; 4], calibration: &[AxisCalibration; 4]) -> [u8; 4] {
    const RECORD: [usize; 4] = [1, 0, 3, 2];
    std::array::from_fn(|axis| calibration[RECORD[axis]].compensate(sticks[axis]))
}

/// Port 1 input from the gamepads, read once per main-loop iteration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PadState {
    /// Held PlayStation buttons (libpad active-high order, as
    /// `Host::buttons`; L3/R3 are bits 1/2).
    pub buttons: u16,
    /// DualShock stick bytes `[right X, right Y, left X, left Y]` while a pad
    /// is connected (`Host::sticks`).
    pub sticks: Option<[u8; 4]>,
    pub connected: bool,
}

/// The d-pad bits.
pub const ARROWS: u16 = buttons::UP | buttons::DOWN | buttons::LEFT | buttons::RIGHT;

/// Port 1's buttons and sticks (`Host::buttons`, `Host::sticks`) from the
/// keyboard's PlayStation buttons `keys` and the gamepad input `pad`, for the
/// controller `port` on port 1.
///
/// The game switches a DualShock to analog mode and locks it there. Its
/// analog layout flies with the sticks: in flight the left stick pitches
/// and rolls and the d-pad looks around (as the right stick does), while
/// menus take directions from the d-pad or the left stick. So:
/// - **DualShock:** the keyboard's arrows deflect the left stick fully
///   instead of pressing the d-pad (keyboard flight keeps working); an arrow
///   overrides the gamepad's left stick on its axis. The gamepad's own
///   d-pad, sticks and stick clicks pass through.
/// - **Digital pad:** both sets of buttons, without the stick clicks a
///   digital pad lacks; the gamepad's left stick past half deflection
///   presses the d-pad, so a pad connected after start still flies.
pub fn port_input(keys: u16, pad: PadState, port: PadType) -> (u16, [u8; 4]) {
    let sticks = pad.sticks.unwrap_or(CENTRE);
    match port {
        PadType::Digital => {
            let [_, _, x, y] = sticks;
            let stick = [
                (y < 0x40, buttons::UP),
                (y > 0xc0, buttons::DOWN),
                (x < 0x40, buttons::LEFT),
                (x > 0xc0, buttons::RIGHT),
            ]
            .iter()
            .filter(|(pushed, _)| *pushed)
            .fold(0, |bits, (_, bit)| bits | bit);
            ((keys | pad.buttons | stick) & !STICK_CLICKS, CENTRE)
        }
        PadType::DualShock => {
            let [rx, ry, mut x, mut y] = sticks;
            let held = |bit| keys & bit != 0;
            match (held(buttons::LEFT), held(buttons::RIGHT)) {
                (true, false) => x = 0x00,
                (false, true) => x = 0xff,
                _ => {}
            }
            match (held(buttons::UP), held(buttons::DOWN)) {
                (true, false) => y = 0x00,
                (false, true) => y = 0xff,
                _ => {}
            }
            ((keys & !ARROWS) | pad.buttons, [rx, ry, x, y])
        }
    }
}

// ---- vibration --------------------------------------------------------------------------

/// The two DualShock motors as the game drives them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Motors {
    /// The small (right, high-frequency) motor: on or off.
    pub small: bool,
    /// The large (left, low-frequency) motor: 0 off .. 0xFF full.
    pub large: u8,
}

impl Motors {
    /// The motors driven by the data bytes of a poll (`bytes`: the bytes the
    /// console sent after the 0x42 command and 0x00, the game's PadSetAct
    /// data as libpad's 8009C56C sends it) through the actuator map of
    /// command 0x4D (`map[i]`: the actuator byte `i` drives, as
    /// `libpad::Controller::motors` holds it; 0xFF none).
    ///
    /// The DualShock describes actuator 0 as the small motor, switched by bit
    /// 0 (its 0x46 table entry has no level byte, so libpad's 8009C628 tests
    /// it with mask 1, 10 power units), and actuator 1 as the large motor
    /// with a 0..255 level (mask 0xFF, 20 units). Ace Combat 3 aligns its two
    /// PadSetAct bytes as {0, 1}: byte 0 the small motor (0/1 from a
    /// 32-frame pattern), byte 1 the large motor's level (80028784).
    pub fn from_actuators(map: &[u8; 6], bytes: &[u8; 6]) -> Self {
        map.iter().zip(bytes).fold(
            Self::default(),
            |motors, (&actuator, &byte)| match actuator {
                0 => Self {
                    small: motors.small || byte & 1 != 0,
                    ..motors
                },
                1 => Self {
                    large: motors.large.max(byte),
                    ..motors
                },
                _ => motors,
            },
        )
    }

    pub fn is_off(self) -> bool {
        self == Self::default()
    }
}

/// The rumble effects playing on one pad.
struct Rumble {
    pad: GamepadId,
    /// Low-frequency motor (the DualShock's large one) and high-frequency
    /// motor (the small one); `None` when the pad has no force feedback.
    effects: Option<[Effect; 2]>,
    playing: Motors,
}

impl Rumble {
    fn new(gilrs: &mut Gilrs, pad: GamepadId) -> Self {
        let motor = |gilrs: &mut Gilrs, kind| {
            let effect = EffectBuilder::new()
                .add_effect(BaseEffect {
                    kind,
                    // One tick, repeated forever: constant until stopped.
                    scheduling: Replay::default(),
                    ..BaseEffect::default()
                })
                .gamepads(&[pad])
                .repeat(Repeat::Infinitely)
                .gain(0.0)
                .finish(gilrs)?;
            effect.play()?;
            Ok::<_, gilrs::ff::Error>(effect)
        };
        let effects = motor(
            gilrs,
            BaseEffectType::Strong {
                magnitude: u16::MAX,
            },
        )
        .and_then(|strong| {
            let weak = motor(
                gilrs,
                BaseEffectType::Weak {
                    magnitude: u16::MAX,
                },
            )?;
            Ok([strong, weak])
        });
        let effects = match effects {
            Ok(effects) => Some(effects),
            Err(error) => {
                eprintln!("gamepad: pad {pad}: no rumble ({error})");
                None
            }
        };
        Self {
            pad,
            effects,
            playing: Motors::default(),
        }
    }

    fn play(&mut self, motors: Motors) {
        if motors == self.playing {
            return;
        }
        self.playing = motors;
        if let Some([strong, weak]) = &self.effects {
            // gilrs mutes an effect whose gain is under 0.05 (large < 13).
            let results = [
                strong.set_gain(f32::from(motors.large) / 255.0),
                weak.set_gain(if motors.small { 1.0 } else { 0.0 }),
            ];
            if let Some(Err(error)) = results.into_iter().find(Result::is_err) {
                eprintln!("gamepad: pad {}: rumble: {error}", self.pad);
                self.effects = None;
            }
        }
    }
}

// ---- the host side -----------------------------------------------------------------------

/// The host's gamepads; see the module documentation.
pub struct Gamepads {
    bindings: Bindings,
    gilrs: Option<Gilrs>,
    /// The pad whose input port 1 receives.
    active: Option<GamepadId>,
    rumble: Option<Rumble>,
}

impl Gamepads {
    /// Starts the gamepad backend. Without one (or when it fails) every
    /// poll reports no pad; the reason is logged.
    pub fn new(bindings: &Bindings) -> Self {
        let bindings = bindings.clone().sanitized();
        let gilrs = GilrsBuilder::new()
            .with_default_filters(false)
            .with_force_feedback(bindings.rumble)
            .build();
        let gilrs = match gilrs {
            Ok(gilrs) => Some(gilrs),
            Err(error) => {
                eprintln!("gamepad: {BACKEND}: no gamepad input ({error})");
                None
            }
        };
        let mut pads = Self {
            bindings,
            gilrs,
            active: None,
            rumble: None,
        };
        if let Some(gilrs) = &pads.gilrs {
            for (id, pad) in gilrs.gamepads() {
                log_connected(id, &pad);
            }
            pads.active = gilrs.gamepads().next().map(|(id, _)| id);
            if pads.active.is_none() {
                eprintln!("gamepad: {BACKEND}: no pad connected");
            }
        }
        pads
    }

    /// The settings in use (after [`Bindings::sanitized`]).
    pub fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    /// Whether a pad is connected (for [`PadChoice::resolve`] at start).
    pub fn connected(&self) -> bool {
        self.gilrs
            .as_ref()
            .is_some_and(|gilrs| gilrs.gamepads().next().is_some())
    }

    /// Names of the connected pads.
    pub fn names(&self) -> Vec<String> {
        self.gilrs.as_ref().map_or_else(Vec::new, |gilrs| {
            gilrs
                .gamepads()
                .map(|(_, pad)| pad.name().to_owned())
                .collect()
        })
    }

    /// Handles connections and reads the active pad: call once per
    /// main-loop iteration. A pad becomes active when one of its buttons is
    /// pressed or a stick or trigger moves well past centre.
    pub fn poll(&mut self) -> PadState {
        let Some(gilrs) = &mut self.gilrs else {
            return PadState::default();
        };
        let threshold = self.bindings.trigger_threshold as f32;
        while let Some(event) = gilrs.next_event() {
            let id = event.id;
            match event.event {
                EventType::Connected => {
                    log_connected(id, &gilrs.gamepad(id));
                    self.active.get_or_insert(id);
                }
                EventType::Disconnected => {
                    eprintln!("gamepad: pad {id} disconnected");
                    if self.active == Some(id) {
                        self.active = None;
                    }
                }
                EventType::ButtonPressed(..) => self.active = Some(id),
                EventType::ButtonChanged(_, value, _) if value >= threshold => {
                    self.active = Some(id)
                }
                EventType::AxisChanged(_, value, _) if value.abs() >= 0.5 => self.active = Some(id),
                _ => {}
            }
        }
        if self
            .active
            .is_none_or(|id| gilrs.connected_gamepad(id).is_none())
        {
            self.active = gilrs.gamepads().next().map(|(id, _)| id);
        }
        if self.rumble.as_ref().map(|rumble| rumble.pad) != self.active {
            // A rumble belongs to one pad; a new active pad starts still.
            self.rumble = None;
        }
        match self.active.and_then(|id| gilrs.connected_gamepad(id)) {
            Some(pad) => self.bindings.state(&RawPad::read(&pad)),
            None => PadState::default(),
        }
    }

    /// Drives the active pad's motors (no effect with rumble off, no pad, or
    /// a pad without force feedback). Call after each main-loop iteration
    /// with port 1's motors; changes reach the pad within gilrs' 50 ms
    /// force-feedback tick.
    pub fn rumble(&mut self, motors: Motors) {
        if !self.bindings.rumble {
            return;
        }
        let (Some(gilrs), Some(active)) = (&mut self.gilrs, self.active) else {
            return;
        };
        if self.rumble.is_none() {
            if motors.is_off() {
                return;
            }
            self.rumble = Some(Rumble::new(gilrs, active));
        }
        if let Some(rumble) = &mut self.rumble {
            rumble.play(motors);
        }
    }

    /// Stops the motors (pause, focus loss, exit).
    pub fn stop_rumble(&mut self) {
        if let Some(rumble) = &mut self.rumble {
            rumble.play(Motors::default());
        }
    }
}

impl Drop for Gamepads {
    fn drop(&mut self) {
        let running = self
            .rumble
            .as_ref()
            .is_some_and(|rumble| !rumble.playing.is_off() && rumble.effects.is_some());
        self.stop_rumble();
        if running {
            // Let gilrs' force-feedback thread send the stop (one 50 ms tick)
            // before the process may exit.
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
    }
}

/// Names of the PlayStation buttons in `bits` (`cross+l2`), or `-`.
pub fn button_names(bits: u16) -> String {
    let names: Vec<_> = [
        PsButton::Select,
        PsButton::L3,
        PsButton::R3,
        PsButton::Start,
        PsButton::Up,
        PsButton::Right,
        PsButton::Down,
        PsButton::Left,
        PsButton::L2,
        PsButton::R2,
        PsButton::L1,
        PsButton::R1,
        PsButton::Triangle,
        PsButton::Circle,
        PsButton::Cross,
        PsButton::Square,
    ]
    .into_iter()
    .filter(|button| bits & button.bit() != 0)
    .map(|button| format!("{button:?}").to_ascii_lowercase())
    .collect();
    if names.is_empty() {
        "-".to_owned()
    } else {
        names.join("+")
    }
}

/// A gamepad check without the game (`ac3 gamepad`): prints the backend and
/// the connected pads, plays a short rumble test (small motor, then the
/// large one at three levels), then prints every change of the port 1
/// input until `seconds` have passed.
pub fn monitor(bindings: &Bindings, seconds: f64) {
    use std::time::{Duration, Instant};
    let mut pads = Gamepads::new(bindings);
    println!(
        "gamepad backend {BACKEND}; pads: {:?}; rumble {}",
        pads.names(),
        if pads.bindings().rumble { "on" } else { "off" }
    );
    let started = Instant::now();
    let motors = |small, large| Motors { small, large };
    let steps = [
        (0.0, motors(true, 0), "small motor"),
        (1.0, motors(false, 0x40), "large motor 0x40"),
        (2.0, motors(false, 0x80), "large motor 0x80"),
        (3.0, motors(false, 0xff), "large motor 0xFF"),
        (4.0, Motors::default(), "motors off"),
    ];
    let mut step = 0;
    let mut last = None;
    while started.elapsed().as_secs_f64() < seconds {
        let time = started.elapsed().as_secs_f64();
        let state = pads.poll();
        while step < steps.len() && time >= steps[step].0 {
            let (_, motors, name) = steps[step];
            if pads.connected() && pads.bindings().rumble {
                println!("{time:6.2}s rumble test: {name}");
            }
            pads.rumble(motors);
            step += 1;
        }
        if last != Some(state) {
            last = Some(state);
            let sticks = state.sticks.map_or_else(
                || "-".to_owned(),
                |[rx, ry, lx, ly]| format!("left {lx:02x},{ly:02x} right {rx:02x},{ry:02x}"),
            );
            println!(
                "{time:6.2}s {} buttons {} sticks {sticks}",
                if state.connected { "pad" } else { "no pad" },
                button_names(state.buttons),
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    pads.stop_rumble();
}

fn log_connected(id: GamepadId, pad: &gilrs::Gamepad<'_>) {
    eprintln!(
        "gamepad: {BACKEND}: pad {id} connected: {} (rumble {})",
        pad.name(),
        if pad.is_ff_supported() { "yes" } else { "no" }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(edit: impl FnOnce(&mut RawPad)) -> u16 {
        let mut pad = RawPad::default();
        edit(&mut pad);
        Bindings::default().state(&pad).buttons
    }

    #[test]
    fn default_map_follows_the_playstation_layout() {
        assert_eq!(press(|p| p.a = true), buttons::CROSS);
        assert_eq!(press(|p| p.b = true), buttons::CIRCLE);
        assert_eq!(press(|p| p.x = true), buttons::SQUARE);
        assert_eq!(press(|p| p.y = true), buttons::TRIANGLE);
        assert_eq!(press(|p| p.lb = true), buttons::L1);
        assert_eq!(press(|p| p.rb = true), buttons::R1);
        assert_eq!(press(|p| p.lt = 1.0), buttons::L2);
        assert_eq!(press(|p| p.rt = 1.0), buttons::R2);
        assert_eq!(press(|p| p.back = true), buttons::SELECT);
        assert_eq!(press(|p| p.start = true), buttons::START);
        assert_eq!(press(|p| p.dpad_up = true), buttons::UP);
        assert_eq!(press(|p| p.dpad_down = true), buttons::DOWN);
        assert_eq!(press(|p| p.dpad_left = true), buttons::LEFT);
        assert_eq!(press(|p| p.dpad_right = true), buttons::RIGHT);
        assert_eq!(press(|p| p.left_stick = true), 1 << 1);
        assert_eq!(press(|p| p.right_stick = true), 1 << 2);
        assert_eq!(press(|p| p.guide = true), 0, "guide unbound");
        assert_eq!(press(|_| {}), 0);
        assert_eq!(
            press(|p| {
                p.a = true;
                p.rb = true;
                p.dpad_left = true;
            }),
            buttons::CROSS | buttons::R1 | buttons::LEFT
        );
    }

    #[test]
    fn triggers_press_at_the_threshold() {
        assert_eq!(press(|p| p.lt = 0.29), 0);
        assert_eq!(press(|p| p.lt = 0.3), buttons::L2);
        assert_eq!(press(|p| p.rt = 0.31), buttons::R2);
        let bindings = Bindings {
            trigger_threshold: 0.8,
            ..Bindings::default()
        };
        let pad = RawPad {
            lt: 0.7,
            rt: 0.85,
            ..RawPad::default()
        };
        assert_eq!(bindings.state(&pad).buttons, buttons::R2);
    }

    #[test]
    fn remapped_buttons() {
        let bindings = Bindings {
            buttons: ButtonMap {
                a: PsButton::Circle,
                b: PsButton::Cross,
                lt: PsButton::None,
                guide: PsButton::Start,
                ..ButtonMap::default()
            },
            ..Bindings::default()
        };
        let pad = RawPad {
            a: true,
            lt: 1.0,
            guide: true,
            ..RawPad::default()
        };
        assert_eq!(
            bindings.state(&pad).buttons,
            buttons::CIRCLE | buttons::START
        );
    }

    #[test]
    fn axes_become_playstation_bytes() {
        assert_eq!(axis_byte(0.0), 0x80);
        assert_eq!(axis_byte(1.0), 0xff);
        assert_eq!(axis_byte(-1.0), 0x00);
        assert_eq!(axis_byte(0.5), 0x80 + 64);
        assert_eq!(axis_byte(-0.5), 0x40);
        assert_eq!(axis_byte(7.0), 0xff);
        assert_eq!(axis_byte(-7.0), 0x00);
        assert_eq!(axis_byte(f32::NAN), 0x80);
        // Up (gilrs +Y) is 0x00, down 0xFF; left 0x00, right 0xFF.
        assert_eq!(stick_bytes([0.0, 1.0], 0.0), [0x80, 0x00]);
        assert_eq!(stick_bytes([0.0, -1.0], 0.0), [0x80, 0xff]);
        assert_eq!(stick_bytes([-1.0, 0.0], 0.0), [0x00, 0x80]);
        assert_eq!(stick_bytes([1.0, 0.0], 0.0), [0xff, 0x80]);
    }

    #[test]
    fn sticks_are_right_then_left() {
        let pad = RawPad {
            left: [-1.0, 1.0],
            right: [1.0, -1.0],
            ..RawPad::default()
        };
        let state = Bindings::default().state(&pad);
        assert_eq!(state.sticks, Some([0xff, 0xff, 0x00, 0x00]));
        assert!(state.connected);
        let idle = Bindings::default().state(&RawPad::default());
        assert_eq!(idle.sticks, Some(CENTRE));
    }

    #[test]
    fn radial_deadzone_rescales_outside_it() {
        assert_eq!(apply_deadzone([0.1, 0.1], 0.15), [0.0, 0.0]);
        assert_eq!(apply_deadzone([0.0, -0.15], 0.15), [0.0, 0.0]);
        assert_eq!(stick_bytes([0.1, -0.1], 0.15), [0x80, 0x80]);
        // Just outside the zone, movement starts at the centre.
        let [x, y] = apply_deadzone([0.16, 0.0], 0.15);
        assert!(x > 0.0 && x < 0.02 && y == 0.0);
        assert_eq!(stick_bytes([0.16, 0.0], 0.15), [0x81, 0x80]);
        // Full deflection stays full; square-gate corners reach both ends.
        assert_eq!(stick_bytes([1.0, 0.0], 0.15), [0xff, 0x80]);
        assert_eq!(stick_bytes([0.0, -1.0], 0.15), [0x80, 0xff]);
        assert_eq!(stick_bytes([1.0, 1.0], 0.15), [0xff, 0x00]);
        // Half way between the zone and the edge.
        let [x, _] = apply_deadzone([0.575, 0.0], 0.15);
        assert!((x - 0.5).abs() < 1e-6);
        // Drift on the other axis does not bend a straight push.
        assert_eq!(stick_bytes([0.0, 1.0], 0.15), [0x80, 0x00]);
        assert_eq!(apply_deadzone([f32::NAN, 2.0], 0.15), [0.0, 1.0]);
    }

    /// What the game's 8002895C makes of `byte` on `axis`: full deflection
    /// 4096 (the release's own arithmetic).
    fn game_reads(byte: u8, axis: AxisCalibration) -> i32 {
        let (low, centre, high, dead) = (
            i32::from(axis.low),
            i32::from(axis.centre),
            i32::from(axis.high),
            i32::from(axis.dead),
        );
        let offset = i32::from(byte).clamp(low, high) - centre;
        if dead < offset {
            ((offset - dead) << 13) / ((high - centre - dead) << 1) - 1
        } else if offset < -dead {
            ((offset + dead) << 13) / -((low - centre + dead) << 1)
        } else {
            0
        }
    }

    #[test]
    fn the_games_dead_zone_drops_small_and_diagonal_axes() {
        let axis = AxisCalibration::DEFAULT;
        // Without compensation: 60 % of a half axis reads 0, and a full
        // diagonal on a round gate (0.707 per axis) reads 29 % per axis.
        assert_eq!(game_reads(axis_byte(0.5), axis), 0);
        assert_eq!(game_reads(axis_byte(0.59), axis), 0);
        let diagonal = game_reads(axis_byte(std::f32::consts::FRAC_1_SQRT_2), axis);
        assert!((1000..1200).contains(&diagonal), "{diagonal}");
        // A 22.5-degree push loses its small axis altogether.
        let small = 22.5f32.to_radians().sin();
        assert_eq!(game_reads(axis_byte(small), axis), 0);
    }

    #[test]
    fn compensated_axes_read_as_their_deflection() {
        for axis in [
            AxisCalibration::DEFAULT,
            AxisCalibration {
                dead: 0x26,
                ..AxisCalibration::DEFAULT
            },
            AxisCalibration {
                dead: 0x72,
                ..AxisCalibration::DEFAULT
            },
            AxisCalibration {
                low: 0x10,
                centre: 0x80,
                high: 0xf0,
                dead: 0x20,
            },
        ] {
            // Centred stays centred; full stays full.
            assert_eq!(axis.compensate(0x80), 0x80);
            assert_eq!(axis.compensate(0xff), axis.high);
            assert_eq!(axis.compensate(0x00), axis.low);
            let live = f32::from(axis.high - axis.centre - axis.dead);
            for step in 1..=127u8 {
                for byte in [0x80 + step, 0x80 - step] {
                    let wanted = if byte > 0x80 {
                        f32::from(step) / 127.0
                    } else {
                        -f32::from(step) / 128.0
                    };
                    let read = game_reads(axis.compensate(byte), axis) as f32 / 4096.0;
                    // Never zero; within one of the game's steps.
                    assert!(read != 0.0 && read.signum() == wanted.signum(), "{byte:#x}");
                    assert!(
                        (read - wanted).abs() <= 1.0 / live + 0.002,
                        "{byte:#x}: {read}"
                    );
                }
            }
        }
        // No live range on a side: the byte is left alone.
        let flat = AxisCalibration {
            high: 0x90,
            ..AxisCalibration::DEFAULT
        };
        assert_eq!(flat.compensate(0xc0), 0xc0);
    }

    #[test]
    fn compensated_diagonals_keep_their_direction() {
        let records = [AxisCalibration::DEFAULT; 4];
        for degrees in (0..=90).step_by(5) {
            for magnitude in [0.2f32, 0.5, 1.0] {
                let angle = (degrees as f32).to_radians();
                // Right and up on a round gate (gilrs orientation).
                let stick = [magnitude * angle.cos(), magnitude * angle.sin()];
                let [x, y] = stick_bytes(stick, 0.0);
                let [_, _, x, y] = compensate_sticks([0x80, 0x80, x, y], &records);
                let roll = game_reads(x, AxisCalibration::DEFAULT) as f32;
                let pitch = -game_reads(y, AxisCalibration::DEFAULT) as f32;
                // Each axis within one of the game's steps (1/51), so the
                // direction within its resolution at that deflection.
                for (read, wanted) in [(roll, stick[0]), (pitch, stick[1])] {
                    assert!(
                        (read / 4096.0 - wanted).abs() <= 1.0 / 51.0,
                        "{degrees} deg at {magnitude}: {read} for {wanted}"
                    );
                }
                let read = pitch.atan2(roll).to_degrees();
                let tolerance = if magnitude < 0.5 { 5.0 } else { 1.5 };
                assert!(
                    (read - degrees as f32).abs() <= tolerance,
                    "{degrees} deg at {magnitude}: the game reads {read}"
                );
            }
        }
    }

    #[test]
    fn compensation_follows_each_records_axis() {
        let mut records = [AxisCalibration::DEFAULT; 4];
        records[3].dead = 0x10; // left X
        records[2].dead = 0x70; // left Y
        let [rx, ry, lx, ly] = compensate_sticks([0x90, 0x70, 0x90, 0x90], &records);
        assert_eq!(rx, AxisCalibration::DEFAULT.compensate(0x90));
        assert_eq!(ry, AxisCalibration::DEFAULT.compensate(0x70));
        assert_eq!(lx, records[3].compensate(0x90));
        assert_eq!(ly, records[2].compensate(0x90));
        assert!(lx < ly, "a smaller dead zone needs a smaller byte");
        assert_eq!(compensate_sticks(CENTRE, &records), CENTRE);
    }

    #[test]
    fn swapped_triggers_press_the_bumpers_buttons() {
        let swapped = Bindings {
            swap_triggers: true,
            ..Bindings::default()
        };
        let pad = |edit: fn(&mut RawPad)| {
            let mut raw = RawPad::default();
            edit(&mut raw);
            raw
        };
        let rt = pad(|p| p.rt = 1.0);
        let lb = pad(|p| p.lb = true);
        assert_eq!(Bindings::default().state(&rt).buttons, buttons::R2);
        assert_eq!(swapped.state(&rt).buttons, buttons::R1);
        assert_eq!(swapped.state(&lb).buttons, buttons::L2);
        // The threshold still applies to the triggers.
        assert_eq!(swapped.state(&pad(|p| p.lt = 0.1)).buttons, 0);
    }

    #[test]
    fn bindings_clamp_out_of_range_values() {
        let bindings = Bindings {
            deadzone: 2.0,
            trigger_threshold: f64::NAN,
            ..Bindings::default()
        }
        .sanitized();
        assert_eq!(bindings.deadzone, 0.9);
        assert_eq!(bindings.trigger_threshold, Bindings::TRIGGER_THRESHOLD);
        let bindings = Bindings {
            deadzone: -1.0,
            trigger_threshold: 0.0,
            ..Bindings::default()
        }
        .sanitized();
        assert_eq!(bindings.deadzone, 0.0);
        assert_eq!(bindings.trigger_threshold, 0.05);
        assert_eq!(Bindings::default().sanitized(), Bindings::default());
    }

    #[test]
    fn pad_choice_resolves_at_start() {
        assert_eq!(PadChoice::Auto.resolve(false), PadType::Digital);
        assert_eq!(PadChoice::Auto.resolve(true), PadType::DualShock);
        assert_eq!(PadChoice::Digital.resolve(true), PadType::Digital);
        assert_eq!(PadChoice::DualShock.resolve(false), PadType::DualShock);
    }

    #[test]
    fn keyboard_alone_is_unchanged_on_a_digital_pad() {
        for keys in [0, buttons::UP | buttons::CROSS, 0xfff9] {
            assert_eq!(
                port_input(keys, PadState::default(), PadType::Digital),
                (keys, CENTRE)
            );
        }
    }

    #[test]
    fn digital_pad_merges_buttons_and_reads_the_left_stick_as_d_pad() {
        let pad = PadState {
            buttons: buttons::CROSS | STICK_CLICKS,
            sticks: Some([0x00, 0xff, 0x10, 0x80]),
            connected: true,
        };
        assert_eq!(
            port_input(buttons::START, pad, PadType::Digital),
            (buttons::START | buttons::CROSS | buttons::LEFT, CENTRE)
        );
        let pad = PadState {
            sticks: Some([0x80, 0x80, 0xc1, 0x3f]),
            ..PadState::default()
        };
        assert_eq!(
            port_input(0, pad, PadType::Digital).0,
            buttons::RIGHT | buttons::UP
        );
        let pad = PadState {
            sticks: Some([0x80, 0x80, 0xc0, 0x40]),
            ..PadState::default()
        };
        assert_eq!(port_input(0, pad, PadType::Digital).0, 0, "under half");
    }

    #[test]
    fn dualshock_keyboard_arrows_deflect_the_left_stick() {
        let keys = buttons::UP | buttons::RIGHT | buttons::SQUARE;
        assert_eq!(
            port_input(keys, PadState::default(), PadType::DualShock),
            (buttons::SQUARE, [0x80, 0x80, 0xff, 0x00])
        );
        // Opposite arrows cancel; the gamepad's stick and d-pad pass through.
        let pad = PadState {
            buttons: buttons::DOWN | (1 << 1),
            sticks: Some([0x12, 0x34, 0x56, 0x78]),
            connected: true,
        };
        assert_eq!(
            port_input(buttons::LEFT | buttons::RIGHT, pad, PadType::DualShock),
            (buttons::DOWN | (1 << 1), [0x12, 0x34, 0x56, 0x78])
        );
        assert_eq!(
            port_input(buttons::DOWN, pad, PadType::DualShock),
            (buttons::DOWN | (1 << 1), [0x12, 0x34, 0x56, 0xff])
        );
        assert_eq!(
            port_input(0, PadState::default(), PadType::DualShock),
            (0, CENTRE)
        );
    }

    #[test]
    fn motors_follow_the_actuator_map() {
        let aligned = [0, 1, 0xff, 0xff, 0xff, 0xff];
        let unmapped = [0xff; 6];
        assert_eq!(
            Motors::from_actuators(&aligned, &[1, 0x80, 0, 0, 0, 0]),
            Motors {
                small: true,
                large: 0x80
            }
        );
        // The small motor switches on bit 0 only.
        assert_eq!(
            Motors::from_actuators(&aligned, &[0xfe, 0, 0, 0, 0, 0]),
            Motors::default()
        );
        assert_eq!(
            Motors::from_actuators(&[1, 0, 0xff, 0xff, 0xff, 0xff], &[0x40, 1, 0, 0, 0, 0]),
            Motors {
                small: true,
                large: 0x40
            }
        );
        assert!(Motors::from_actuators(&unmapped, &[0xff; 6]).is_off());
        assert!(Motors::from_actuators(&aligned, &[0; 6]).is_off());
    }

    #[test]
    fn button_names_cover_every_bit() {
        assert_eq!(button_names(0), "-");
        assert_eq!(button_names(buttons::CROSS | buttons::L2), "l2+cross");
        assert_eq!(button_names(0xffff).split('+').count(), 16);
    }

    #[test]
    fn without_a_backend_nothing_is_reported() {
        let mut pads = Gamepads {
            bindings: Bindings::default(),
            gilrs: None,
            active: None,
            rumble: None,
        };
        assert_eq!(pads.poll(), PadState::default());
        assert!(!pads.connected());
        assert!(pads.names().is_empty());
        pads.rumble(Motors {
            small: true,
            large: 0xff,
        });
        pads.stop_rumble();
    }
}
