use crate::{Effect, Error};

/// Signed halfwords at `[0x800F6860] + 4/+6`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BriefingState {
    pub phase: i16,
    pub substate: i16,
}
/// Signed words at `[0x800F5FD4] + 0/+4`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AircraftState {
    pub phase: i32,
    pub substate: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrapperUpdate<T> {
    pub state: T,
    pub callback_result: u32,
    pub effects: Vec<Effect>,
}

pub const BRIEFING_CALLBACKS: [&[u32]; 4] = [
    &[0x800edf78, 0x800edfa4, 0x800ee128, 0x800ee1e4],
    &[0x800ee3b8, 0x800ee860, 0x800ee8d0, 0x800ee95c],
    &[0x800ee9a0, 0x800eea20, 0x800eec90],
    &[0x800eed50, 0x800eedbc],
];
pub const AIRCRAFT_CALLBACKS: [&[u32]; 2] = [
    &[0x800d9038, 0x800d92b8, 0x800d9438, 0x800d9770, 0x800d98a0],
    &[0x800d98c4, 0x800d9c78, 0x800d9ddc, 0x800da2cc, 0x800da440],
];

pub fn select_briefing_callback(state: BriefingState) -> Result<u32, Error> {
    BRIEFING_CALLBACKS
        .get(usize::try_from(state.phase).unwrap_or(usize::MAX))
        .and_then(|callbacks| callbacks.get(usize::try_from(state.substate).unwrap_or(usize::MAX)))
        .copied()
        .ok_or(Error::InvalidBriefingPhase {
            phase: state.phase,
            substate: state.substate,
        })
}
pub fn select_aircraft_callback(state: AircraftState) -> Result<u32, Error> {
    AIRCRAFT_CALLBACKS
        .get(usize::try_from(state.phase).unwrap_or(usize::MAX))
        .and_then(|callbacks| callbacks.get(usize::try_from(state.substate).unwrap_or(usize::MAX)))
        .copied()
        .ok_or(Error::InvalidAircraftPhase {
            phase: state.phase,
            substate: state.substate,
        })
}

/// Result routing inside `0x800EF01C`, after a briefing phase callback.
/// Priority is 2,1,4,8,16. Rendering/script processing remains explicit effects.
pub fn apply_briefing_result(
    mut state: BriefingState,
    result: u32,
    pressed: u32,
) -> WrapperUpdate<BriefingState> {
    let mut effects = vec![];
    let mut callback_result = 0;
    if result & 2 != 0 {
        state.substate = state.substate.wrapping_add(1);
    } else if result & 1 != 0 {
        state.phase = state.phase.wrapping_add(1);
        state.substate = 0;
    } else if result & 4 != 0 {
        effects.extend([
            Effect::PendingOriginalCall {
                address: 0x800e9824,
            },
            Effect::StopStream,
        ]);
        callback_result = 0x100;
    } else if result & 8 != 0 {
        effects.extend([
            Effect::StopStream,
            Effect::PendingOriginalCall {
                address: 0x800e7a18,
            },
        ]);
        callback_result = 0x200;
    } else if result & 0x10 != 0 {
        state.phase = 3;
        state.substate = 0;
    }
    if callback_result == 0 {
        effects.extend(
            [0x800eb654, 0x800eb9e8, 0x800ebc14]
                .map(|address| Effect::PendingOriginalCall { address }),
        );
        if pressed & 0x40 == 0 {
            // The original iterates eight records at +0x4A4 with stride 12.
            effects.extend((0..8).map(|_| Effect::PendingOriginalCall {
                address: 0x800eb754,
            }));
        }
        effects.extend(
            [0x800ed834, 0x800ed8b0].map(|address| Effect::PendingOriginalCall { address }),
        );
    }
    WrapperUpdate {
        state,
        callback_result,
        effects,
    }
}

/// Result routing inside `0x800DA65C`, after an aircraft phase callback.
/// Priority is 1,2,8,4,16; this differs from the briefing wrapper.
pub fn apply_aircraft_result(
    mut state: AircraftState,
    result: u32,
) -> WrapperUpdate<AircraftState> {
    let mut effects = vec![];
    let mut callback_result = 0;
    if result & 1 != 0 {
        state.substate = state.substate.wrapping_add(1);
    } else if result & 2 != 0 {
        state.phase = state.phase.wrapping_add(1);
        state.substate = 0;
    } else if result & 8 != 0 {
        state.phase = state.phase.wrapping_sub(1);
        state.substate = 0;
    } else if result & 4 != 0 {
        effects.push(Effect::StopStream);
        callback_result = 0x100;
    } else if result & 0x10 != 0 {
        effects.push(Effect::StopStream);
        callback_result = 0x200;
    }
    if callback_result == 0 {
        effects.extend(
            [0x800d7950, 0x800d8cb4, 0x800d8d34, 0x800d7c54, 0x800d7dc8]
                .map(|address| Effect::PendingOriginalCall { address }),
        );
    }
    WrapperUpdate {
        state,
        callback_result,
        effects,
    }
}
