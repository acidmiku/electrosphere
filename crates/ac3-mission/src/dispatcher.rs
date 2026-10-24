use crate::{Effect, Error, read_u16, read_u32, require_bytes};

/// Five halfwords at `[0x800F69D0]`; observed allocation base `0x800F9E18`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MissionState {
    pub state: i16,
    pub substate: i16,
    pub previous: i16,
    pub choice: i16,
    pub auxiliary_choice: i16,
}
impl MissionState {
    pub const BYTE_LEN: usize = 10;
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        Ok(Self {
            state: read_u16(bytes, 0) as i16,
            substate: read_u16(bytes, 2) as i16,
            previous: read_u16(bytes, 4) as i16,
            choice: read_u16(bytes, 6) as i16,
            auxiliary_choice: read_u16(bytes, 8) as i16,
        })
    }
    pub fn write_into(&self, bytes: &mut [u8]) -> Result<(), Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        for (index, value) in [
            self.state,
            self.substate,
            self.previous,
            self.choice,
            self.auxiliary_choice,
        ]
        .into_iter()
        .enumerate()
        {
            bytes[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
        }
        Ok(())
    }
}

/// Sparse resident context used by the dispatcher; offsets are original bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MissionContext {
    pub progress_flags: u32,    // +0x964
    pub options: u32,           // +0x970
    pub resume_byte: u8,        // +0x95B
    pub current_pair: [u16; 2], // +0x390, +0x392
    pub saved_pair: [u16; 2],   // +0x394, +0x396
    pub transition_code: u32,   // +0x3A4
}
impl MissionContext {
    pub const BYTE_LEN: usize = 0x974;
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        Ok(Self {
            progress_flags: read_u32(bytes, 0x964),
            options: read_u32(bytes, 0x970),
            resume_byte: bytes[0x95b],
            current_pair: [read_u16(bytes, 0x390), read_u16(bytes, 0x392)],
            saved_pair: [read_u16(bytes, 0x394), read_u16(bytes, 0x396)],
            transition_code: read_u32(bytes, 0x3a4),
        })
    }
    pub fn write_into(&self, bytes: &mut [u8]) -> Result<(), Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        for (offset, value) in [
            (0x964, self.progress_flags),
            (0x970, self.options),
            (0x3a4, self.transition_code),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (offset, value) in [
            (0x390, self.current_pair[0]),
            (0x392, self.current_pair[1]),
            (0x394, self.saved_pair[0]),
            (0x396, self.saved_pair[1]),
        ] {
            bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        bytes[0x95b] = self.resume_byte;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissionUpdate {
    pub state: MissionState,
    pub context: MissionContext,
    pub callback_result: u32,
    pub effects: Vec<Effect>,
}

/// Callback arrays reached through table `0x800F1DD4`, read from PAL overlay 2.
pub const CALLBACKS: [&[u32]; 16] = [
    &[0x800d483c, 0x800d4b44],
    &[0x800eee70, 0x800ef01c],
    &[0x800da5f4, 0x800da65c],
    &[0x800da7ac, 0x800da80c],
    &[0x800d483c, 0x800d4b44],
    &[0x800de354, 0x800de3ec],
    &[0x800dd754, 0x800dd918],
    &[0x800db454, 0x800db618, 0x800db6cc, 0x800db8c8],
    &[0x800db454, 0x800db618, 0x800db6cc, 0x800db8c8],
    &[0x800d75a4, 0x800d7640],
    &[0x800dd1a4, 0x800dd214],
    &[0x800d2c98, 0x800d2e18, 0x800d31fc],
    &[0x800dc7b0, 0x800dc868],
    &[0x800d3730, 0x800d3794],
    &[0x800d3254, 0x800d328c],
    &[0x800d3534, 0x800d3584],
];
const NEXT: [u8; 16] = [
    255, 2, 3, 127, 255, 6, 255, 0, 4, 127, 128, 12, 127, 127, 127, 10,
];
const BACK: [u8; 16] = [
    127, 126, 1, 127, 127, 4, 5, 126, 126, 126, 126, 127, 127, 127, 127, 127,
];

pub fn select_callback(state: MissionState) -> Result<u32, Error> {
    let callbacks = CALLBACKS
        .get(usize::try_from(state.state).unwrap_or(usize::MAX))
        .ok_or(Error::InvalidMissionState(state.state))?;
    callbacks
        .get(usize::try_from(state.substate).unwrap_or(usize::MAX))
        .copied()
        .ok_or(Error::InvalidMissionSubstate {
            state: state.state,
            substate: state.substate,
        })
}

/// `0x800CB3BC`: missions 32..=37 bypass briefing.
pub fn refresh_briefing_option(mut context: MissionContext) -> MissionContext {
    context.options = (context.options & !0x80)
        | if (32..=37).contains(&context.current_pair[0]) {
            0x80
        } else {
            0
        };
    context
}

/// Scalar initialization at `0x800CB8B4`. Allocation and the remaining original
/// initializers are effects; this does not claim to initialize their resources.
pub fn initialize_mission(mut context: MissionContext) -> MissionUpdate {
    let mut state = MissionState::default();
    context.options |= 0x42;
    if context.options & 8 != 0 {
        context.options = (context.options & !2) | 0x10;
    }
    if context.options & 0x10 != 0 {
        context.options &= !2;
    }
    if context.resume_byte != 0 {
        state.state = 13;
        context.resume_byte = 0;
    } else if context.progress_flags & 0x10 != 0 {
        state.state = 14;
        context.progress_flags &= !0x10;
    } else if context.progress_flags & 0x20 != 0 {
        state.state = 15;
        context.progress_flags &= !0x20;
        state.choice = 3;
        if context.options & 0x10 != 0 {
            state.previous = 4;
        }
        context.current_pair = context.saved_pair;
    } else if context.progress_flags & 8 != 0 {
        state.state = 11;
        context.progress_flags &= !8;
    }
    context = refresh_briefing_option(context);
    let mut effects = vec![
        Effect::InitializeFrameState,
        Effect::AllocateScene { bytes: 0x200e8 },
        Effect::SetSceneWord {
            offset: 0x200e4,
            value: 0,
        },
    ];
    effects.extend(
        [
            0x800f197c, 0x800f1a9c, 0x800d0c2c, 0x800d0e90, 0x800d169c, 0x800eaf0c,
        ]
        .map(|address| Effect::PendingOriginalCall { address }),
    );
    MissionUpdate {
        state,
        context,
        callback_result: 0x04000000,
        effects,
    }
}

/// Full result routing at `0x800CB408`, after the selected callback has run.
/// These are exact result values, not bit flags. All halfword increments wrap.
pub fn apply_callback_result(
    mut state: MissionState,
    mut context: MissionContext,
    result: u32,
) -> Result<MissionUpdate, Error> {
    let index = usize::try_from(state.state)
        .ok()
        .filter(|index| *index < 16)
        .ok_or(Error::InvalidMissionState(state.state))?;
    let mut effects = vec![];
    let mut callback_result = 0;
    match result {
        1 => state.substate = state.substate.wrapping_add(1),
        0x10 => {
            context.transition_code = 3;
            context.progress_flags |= 0x20;
            effects.push(Effect::PendingOriginalCall {
                address: 0x8002d9b8,
            });
            callback_result = 0x60000009;
        }
        0x20 => {
            context.progress_flags |= 4;
            effects.push(Effect::PendingOriginalCall {
                address: 0x8002d9b8,
            });
            callback_result = 0x60000004;
        }
        0x80 => {
            context.progress_flags |= 8;
            callback_result = 0x60000002;
        }
        0x100 => {
            let next = NEXT[index];
            if next & 0x80 != 0 {
                state.previous = state.state;
            }
            state.substate = 0;
            if state.state == 5 {
                context = refresh_briefing_option(context);
            }
            match state.state {
                0 => match state.choice {
                    0 => state.state = if context.options & 0x80 == 0 { 1 } else { 2 },
                    1 => state.state = 7,
                    2 => state.state = 9,
                    3 => state.state = 10,
                    _ => {}
                },
                4 => match state.choice {
                    0 => state.state = 5,
                    1 => state.state = 8,
                    2 => state.state = 9,
                    3 => state.state = 10,
                    _ => {}
                },
                12 => {
                    context = refresh_briefing_option(context);
                    if context.options & 8 != 0 {
                        state.state = 9;
                    } else if context.options & 0x10 != 0 {
                        state.state = 4;
                    } else if context.options & 0x80 != 0 {
                        context.progress_flags |= 8;
                        callback_result = 0x60000002;
                    } else {
                        state.state = 0;
                    }
                }
                9 => {
                    context.options &= !8;
                    state.state = if context.options & 0x10 != 0 { 4 } else { 0 };
                }
                13 | 14 => state.state = if context.options & 0x10 != 0 { 4 } else { 0 },
                6 => state.state = if context.options & 0x80 == 0 { 1 } else { 2 },
                _ if next & 0x7f != 0x7f => state.state = i16::from(next & 0x7f),
                _ => {}
            }
        }
        0x200 => {
            state.substate = 0;
            state.state = match BACK[index] {
                1 if context.options & 0x80 != 0 => state.previous,
                0x7e => state.previous,
                0x7f => state.state,
                next => i16::from(next),
            };
        }
        _ => {}
    }
    Ok(MissionUpdate {
        state,
        context,
        callback_result,
        effects,
    })
}
