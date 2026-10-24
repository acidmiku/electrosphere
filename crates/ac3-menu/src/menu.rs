use crate::{Effect, Error, read_u16, read_u32, require_bytes};

/// The six leading halfwords of the block referenced by `0x800D8948`.
/// The original sign-extends state and substate when indexing callback tables.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuState {
    pub state: i16,
    pub substate: i16,
    pub previous_state: i16,
    pub choice3: i16,
    pub choice4: i16,
    pub reserved: i16,
}

impl MenuState {
    pub const BYTE_LEN: usize = 12;
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        Ok(Self {
            state: read_u16(bytes, 0) as i16,
            substate: read_u16(bytes, 2) as i16,
            previous_state: read_u16(bytes, 4) as i16,
            choice3: read_u16(bytes, 6) as i16,
            choice4: read_u16(bytes, 8) as i16,
            reserved: read_u16(bytes, 10) as i16,
        })
    }
    pub fn write_into(&self, bytes: &mut [u8]) -> Result<(), Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        for (index, value) in [
            self.state,
            self.substate,
            self.previous_state,
            self.choice3,
            self.choice4,
            self.reserved,
        ]
        .into_iter()
        .enumerate()
        {
            bytes[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
        }
        Ok(())
    }
}

/// Sparse view of the active game context used by menu transitions.
/// Names other than language are provisional; offsets preserve the evidence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuContext {
    /// `+0x964`.
    pub return_flags: u32,
    /// `+0x970`.
    pub progress_flags: u32,
    /// `+0x38C`.
    pub language_id: i32,
    /// `+0x390`, `+0x392`.
    pub current_pair: [u16; 2],
    /// `+0x394`, `+0x396`.
    pub saved_pair: [u16; 2],
    /// `+0x3A4`.
    pub mode: u32,
    /// `+0x3AC`.
    pub status: u8,
}

impl MenuContext {
    pub const BYTE_LEN: usize = 0x974;
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        Ok(Self {
            return_flags: read_u32(bytes, 0x964),
            progress_flags: read_u32(bytes, 0x970),
            language_id: read_u32(bytes, 0x38c) as i32,
            current_pair: [read_u16(bytes, 0x390), read_u16(bytes, 0x392)],
            saved_pair: [read_u16(bytes, 0x394), read_u16(bytes, 0x396)],
            mode: read_u32(bytes, 0x3a4),
            status: bytes[0x3ac],
        })
    }
    /// Preserves all unknown bytes and rejects short buffers before writing.
    pub fn write_into(&self, bytes: &mut [u8]) -> Result<(), Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        for (offset, value) in [
            (0x964, self.return_flags),
            (0x970, self.progress_flags),
            (0x38c, self.language_id as u32),
            (0x3a4, self.mode),
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
        bytes[0x3ac] = self.status;
        Ok(())
    }
}

/// Recovered PAL tables. Callers can parse these from the overlay using
/// [`MenuTables::from_overlay`] instead of trusting an unchecked revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuTables {
    pub callbacks: [[u32; 2]; 9],
    pub forward: [u8; 9],
    pub backward: [u8; 9],
}

impl MenuTables {
    pub const PAL: Self = Self {
        callbacks: [
            [0x800d0b74, 0x800d0bf4],
            [0x800d13e4, 0x800d14e0],
            [0x800d18b0, 0x800d1900],
            [0x800d1b58, 0x800d1c1c],
            [0x800d1ef4, 0x800d1f68],
            [0x800d2ba4, 0x800d2c3c],
            [0x800cb5b4, 0x800cb5e4],
            [0x800d1ef4, 0x800d1f68],
            [0x800d0d04, 0x800d0d38],
        ],
        forward: [0xff, 2, 3, 0xff, 6, 0xff, 0xff, 6, 2],
        backward: [0xff, 0xff, 0xff, 2, 3, 3, 0xff, 5, 0xff],
    };

    /// Read the exact known table extents from a whole overlay image loaded at
    /// `0x800C9DC0`. Invalid callback-table pointers produce an error.
    pub fn from_overlay(bytes: &[u8]) -> Result<Self, Error> {
        const BASE: u32 = 0x800c9dc0;
        const TABLE: usize = (0x800d5940 - BASE) as usize;
        require_bytes(bytes, (0x800d5979 - BASE) as usize)?;
        let mut callbacks = [[0; 2]; 9];
        for (index, pair) in callbacks.iter_mut().enumerate() {
            let address = read_u32(bytes, TABLE + index * 4);
            let offset = address
                .checked_sub(BASE)
                .ok_or(Error::InvalidMenuState(index as i16))? as usize;
            let end = offset
                .checked_add(8)
                .ok_or(Error::InvalidMenuState(index as i16))?;
            require_bytes(bytes, end)?;
            *pair = [read_u32(bytes, offset), read_u32(bytes, offset + 4)];
        }
        let mut forward = [0; 9];
        let mut backward = [0; 9];
        forward.copy_from_slice(&bytes[(0x800d5964 - BASE) as usize..(0x800d596d - BASE) as usize]);
        backward
            .copy_from_slice(&bytes[(0x800d5970 - BASE) as usize..(0x800d5979 - BASE) as usize]);
        Ok(Self {
            callbacks,
            forward,
            backward,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuUpdate {
    pub state: MenuState,
    pub context: MenuContext,
    /// Return to the resident dispatcher, not a nested phase result.
    pub callback_result: u32,
    pub effects: Vec<Effect>,
}

/// Logic in `0x800CB310..0x800CB3B0`, with remaining initialization represented
/// as ordered pending effects. External display-state mutations preceding the
/// allocation are documented in MENU_LOGIC.md and remain the host's work.
pub fn initialize_menu(mut context: MenuContext) -> MenuUpdate {
    let mut state = MenuState::default();
    context.progress_flags &= !0x10;
    if context.progress_flags == 0 {
        context.return_flags = 1;
    } else if context.return_flags & 2 != 0 {
        context.return_flags &= !2;
        context.current_pair = context.saved_pair;
    } else if context.return_flags & 4 != 0 {
        state.state = 8;
        context.return_flags &= !4;
    }
    MenuUpdate {
        state,
        context,
        callback_result: 0x04000000,
        effects: vec![
            Effect::AllocateMenuBlock { bytes: 0x809c },
            Effect::PendingOriginalCall {
                address: 0x800cccb8,
                arguments: vec![0, 0],
            },
            Effect::PendingOriginalCall {
                address: 0x800d54e8,
                arguments: vec![],
            },
            Effect::PendingOriginalCall {
                address: 0x800d5608,
                arguments: vec![0],
            },
            Effect::PendingOriginalCall {
                address: 0x800d0254,
                arguments: vec![],
            },
            Effect::PendingOriginalCall {
                address: 0x800d04b8,
                arguments: vec![],
            },
        ],
    }
}

pub fn select_menu_callback(state: MenuState, tables: &MenuTables) -> Result<u32, Error> {
    let pair = usize::try_from(state.state)
        .ok()
        .and_then(|i| tables.callbacks.get(i))
        .ok_or(Error::InvalidMenuState(state.state))?;
    usize::try_from(state.substate)
        .ok()
        .and_then(|i| pair.get(i))
        .copied()
        .ok_or(Error::InvalidMenuSubstate(state.substate))
}

/// Post-callback logic at `0x800CB008..0x800CB2AC`. Inputs must reflect changes
/// made by the nested callback. Results are exact integers, not bit flags.
pub fn apply_menu_callback_result(
    mut state: MenuState,
    mut context: MenuContext,
    result: i32,
    tables: &MenuTables,
) -> Result<MenuUpdate, Error> {
    let mut callback_result = 0;
    match result {
        1 => state.substate = state.substate.wrapping_add(1),
        4 => callback_result = 0x60000003,
        8 => {
            context.mode = 4;
            context.status = 0;
            context.return_flags |= 2;
            context.saved_pair = context.current_pair;
            callback_result = 0x60000005;
        }
        0x10 | 0x40 => {
            state.state = 0;
            state.substate = 0;
        }
        0x100 => {
            state.substate = 0;
            state.previous_state = state.state;
            match state.state {
                0 => {
                    state.state = if context.progress_flags & 1 != 0 {
                        2
                    } else {
                        1
                    }
                }
                3 => match state.choice3 {
                    0 => state.state = 4,
                    1 => state.state = 5,
                    2 => {
                        context.return_flags |= 0x10;
                        state.state = 6;
                    }
                    _ => (),
                },
                5 => match state.choice4 {
                    6 => state.state = 7,
                    7 => {
                        context.progress_flags |= 0x10;
                        state.state = 6;
                    }
                    _ => state.state = 6,
                },
                value => {
                    let next = table_value(&tables.forward, value)?;
                    if next != 0xff {
                        state.state = i16::from(next);
                    }
                }
            }
        }
        0x200 => {
            state.substate = 0;
            match table_value(&tables.backward, state.state)? {
                0xfe => state.state = state.previous_state,
                0xff => (),
                next => state.state = i16::from(next),
            }
        }
        _ => (),
    }
    Ok(MenuUpdate {
        state,
        context,
        callback_result,
        effects: vec![],
    })
}

fn table_value(table: &[u8; 9], state: i16) -> Result<u8, Error> {
    usize::try_from(state)
        .ok()
        .and_then(|i| table.get(i))
        .copied()
        .ok_or(Error::InvalidMenuState(state))
}

/// Logical counter from `0x800D8950`; cleanup implementations are host effects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExitState {
    pub counter: i32,
}

pub fn initialize_exit() -> (ExitState, u32, Vec<Effect>) {
    (
        ExitState { counter: 0 },
        1,
        vec![
            Effect::SelectVisualPage { page: 0 },
            Effect::PendingOriginalCall {
                address: 0x800d0548,
                arguments: vec![],
            },
        ],
    )
}

pub fn tick_exit(
    mut state: ExitState,
    mut context: MenuContext,
) -> (ExitState, MenuContext, u32, Vec<Effect>) {
    if state.counter < 3 {
        state.counter = state.counter.wrapping_add(1);
        (state, context, 0, vec![])
    } else {
        context.return_flags |= 0x10;
        (
            state,
            context,
            4,
            vec![
                Effect::PendingOriginalCall {
                    address: 0x800d02b0,
                    arguments: vec![],
                },
                Effect::PendingOriginalCall {
                    address: 0x800cc6f4,
                    arguments: vec![],
                },
            ],
        )
    }
}
