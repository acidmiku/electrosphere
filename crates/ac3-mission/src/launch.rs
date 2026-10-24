use crate::{Effect, MissionContext};

/// The signed timer word at `[0x800F6028]`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LaunchState {
    pub timer: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchUpdate {
    pub state: LaunchState,
    pub context: MissionContext,
    pub callback_result: u32,
    pub effects: Vec<Effect>,
}

/// Original 0x800DA7AC, including the context +0x970 bit-4 store.
pub fn initialize_launch(mut context: MissionContext) -> LaunchUpdate {
    context.options |= 4;
    LaunchUpdate {
        state: LaunchState { timer: 0 },
        context,
        callback_result: 1,
        effects: vec![
            Effect::SelectVisualPage { page: 6 },
            Effect::SetVisualStyle { slot: 1, style: 1 },
            Effect::PlaySound { id: 0x63 },
            Effect::BeginLaunchBackgroundFade,
            Effect::PendingOriginalCall {
                address: 0x800e983c,
            },
        ],
    }
}

/// Original 0x800DA80C. Confirm changes the local comparison value to 75,
/// not the stored timer (MIPS 0x800DA878 and 0x800DA98C). Exit leaves the
/// timer unchanged. Signed comparisons and low-word arithmetic are deliberate.
pub fn tick_launch(mut state: LaunchState, context: MissionContext, pressed: u32) -> LaunchUpdate {
    let mut effects = vec![];
    let mut comparison = state.timer;
    if pressed & 0x10 != 0 {
        effects.extend([
            Effect::SetVisualOffset {
                slot: 1,
                x: 0,
                y: 625,
            },
            Effect::SetVisualOffset {
                slot: 2,
                x: 0,
                y: -625,
            },
            Effect::SetVisualStyle { slot: 1, style: 0 },
            Effect::SetSceneColor {
                red: 0,
                green: 0,
                blue: 0,
            },
        ]);
        comparison = 75;
    }
    if comparison < 25 {
        let square = state.timer.wrapping_mul(state.timer);
        let remaining = 25_i32.wrapping_sub(state.timer);
        effects.extend([
            Effect::SetVisualOffset {
                slot: 1,
                x: 0,
                y: square,
            },
            Effect::SetVisualOffset {
                slot: 2,
                x: 0,
                y: square.wrapping_neg(),
            },
            Effect::SetSceneColor {
                red: remaining.wrapping_mul(143) / 25,
                green: remaining.wrapping_mul(135) / 25,
                blue: remaining.wrapping_mul(83) / 25,
            },
        ]);
    } else if comparison < 50 {
        effects.extend([
            Effect::SetSceneColor {
                red: 0,
                green: 0,
                blue: 0,
            },
            Effect::SetVisualStyle { slot: 1, style: 0 },
        ]);
    } else if comparison < 75 {
        return LaunchUpdate {
            state,
            context,
            callback_result: 0x80,
            effects,
        };
    }
    state.timer = state.timer.wrapping_add(1);
    LaunchUpdate {
        state,
        context,
        callback_result: 0,
        effects,
    }
}
