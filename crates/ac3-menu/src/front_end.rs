//! Title, main menu and new-game difficulty logic from overlay 1.
use crate::{Effect, Error, MenuContext, MenuState, read_u32, require_bytes};

pub const INPUT_CANCEL: u32 = 0x20;
pub const INPUT_START: u32 = 0x40;

/// Original `[0x800D87A0]`, initially `0x800DAFC8`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TitleState {
    pub phase: i32,
    pub timer: i32,
    pub idle_timer: i32,
}

impl TitleState {
    pub const BYTE_LEN: usize = 12;
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        Ok(Self {
            phase: read_u32(bytes, 0) as i32,
            timer: read_u32(bytes, 4) as i32,
            idle_timer: read_u32(bytes, 8) as i32,
        })
    }
    pub fn write_into(&self, bytes: &mut [u8]) -> Result<(), Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        for (index, value) in [self.phase, self.timer, self.idle_timer]
            .into_iter()
            .enumerate()
        {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleUpdate {
    pub state: TitleState,
    pub context: MenuContext,
    pub callback_result: u32,
    pub effects: Vec<Effect>,
}

/// `0x800D18B0`. Progress icon styling is an explicit pending effect; hosts can
/// use `title_progress_effects` on context memory to implement `0x800D1800`.
pub fn initialize_title(context: MenuContext) -> TitleUpdate {
    TitleUpdate {
        state: TitleState {
            phase: 0,
            timer: 0,
            idle_timer: 1000,
        },
        context,
        callback_result: 1,
        effects: vec![
            Effect::SelectVisualPage { page: 0 },
            Effect::ClearTextVisibility,
            Effect::PendingOriginalCall {
                address: 0x800d1800,
                arguments: vec![],
            },
        ],
    }
}

/// `0x800D1900` with phase callbacks `0x800D1578`, `0x800D1794`.
/// Press-start bit 0x40 takes precedence over timeout; all other input is ignored.
pub fn tick_title(
    mut state: TitleState,
    context: MenuContext,
    input_bits: u32,
) -> Result<TitleUpdate, Error> {
    let mut effects = vec![];
    let phase_result = match state.phase {
        0 => {
            effects.extend([
                Effect::ClearTextVisibility,
                Effect::SetTextVisibility { entry: 0, value: 1 },
                Effect::SelectVisualPage { page: 3 },
                Effect::DrawScreenFade {
                    shade: (25_i32.wrapping_sub(state.timer).wrapping_mul(255) / 25) as u8,
                },
            ]);
            if state.timer < 25 {
                state.timer = state.timer.wrapping_add(1);
                0
            } else {
                1
            }
        }
        1 => {
            if input_bits & INPUT_START != 0 {
                effects.push(Effect::PlaySound { id: 0x58 });
                4
            } else if state.idle_timer < 0 {
                2
            } else {
                state.idle_timer = state.idle_timer.wrapping_sub(1);
                0
            }
        }
        phase => return Err(Error::InvalidTitlePhase(phase)),
    };
    let callback_result = if phase_result & 1 != 0 {
        state.phase = state.phase.wrapping_add(1);
        0
    } else if phase_result & 4 != 0 {
        0x100
    } else if phase_result & 2 != 0 {
        effects.push(Effect::PendingOriginalCall {
            address: 0x800cc6f4,
            arguments: vec![],
        });
        8
    } else {
        0
    };
    if callback_result == 0 {
        effects.push(Effect::PulseVisualColor {
            slot: 0x78,
            from: [0xf8, 0x98, 0x18],
            to: [0xf8, 0xf8, 0xf0],
        });
    }
    Ok(TitleUpdate {
        state,
        context,
        callback_result,
        effects,
    })
}

/// `0x800D1800`: three sets of 36 progress values, skipping element zero.
pub fn title_progress_effects(context_bytes: &[u8]) -> Result<Vec<Effect>, Error> {
    require_bytes(context_bytes, 0xcfd)?;
    let mut effects = Vec::with_capacity(108);
    for index in 0..36 {
        for (base, slot_base) in [(0xc61, 1), (0xc9d, 0x25), (0xcd9, 0x49)] {
            let value = context_bytes[base + index];
            effects.push(Effect::SetVisualStyle {
                slot: (slot_base + index) as i32,
                style: if value == 0 { 5 } else { value },
            });
        }
    }
    Ok(effects)
}

/// `0x800D532C`, including signed remainder and byte truncation for unusual
/// counters. The caller supplies the original context `+0x20` frame counter.
pub fn pulse_color(frame_counter: i32, from: [u8; 3], to: [u8; 3]) -> [u8; 3] {
    let weight = ((frame_counter % 50) - 25).abs();
    std::array::from_fn(|index| {
        (i32::from(from[index]) + ((i32::from(to[index]) - i32::from(from[index])) * weight) / 25)
            as u8
    })
}

/// Original `[0x800D87B4]`, initially `0x800DAFD8`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MainMenuState {
    pub selection: i32,
    pub choice_count: i32,
}
impl MainMenuState {
    pub const BYTE_LEN: usize = 8;
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        Ok(Self {
            selection: read_u32(bytes, 0) as i32,
            choice_count: read_u32(bytes, 4) as i32,
        })
    }
    pub fn write_into(&self, bytes: &mut [u8]) -> Result<(), Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        bytes[..4].copy_from_slice(&self.selection.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.choice_count.to_le_bytes());
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MainMenuUpdate {
    pub state: MainMenuState,
    pub menu: MenuState,
    pub context: MenuContext,
    pub callback_result: u32,
    pub effects: Vec<Effect>,
}

/// `0x800D1B58`; bit 2 enables a third choice. Selection is copied from the
/// signed halfword at menu offset 6 without normalization.
pub fn initialize_main_menu(menu: MenuState, context: MenuContext) -> MainMenuUpdate {
    let state = MainMenuState {
        selection: i32::from(menu.choice3),
        choice_count: if context.progress_flags & 2 != 0 {
            3
        } else {
            2
        },
    };
    let mut effects = vec![
        Effect::SelectVisualPage { page: 4 },
        Effect::ClearTextVisibility,
    ];
    effects.extend([1, 12, 13, 14].map(|entry| Effect::SetTextVisibility { entry, value: 1 }));
    effects.extend(main_menu_styles(state.selection, context.progress_flags));
    effects.extend([
        Effect::SetVisualStyle { slot: 4, style: 1 },
        Effect::SetVisualStyle { slot: 5, style: 1 },
        Effect::StartStream { resource: 0x81c },
    ]);
    MainMenuUpdate {
        state,
        menu,
        context,
        callback_result: 1,
        effects,
    }
}

/// `0x800D1C1C`: cancel precedes confirm. Navigation uses context `+0x130`
/// repeat bits, not the `+0x12C` pressed bits used by confirm/cancel. Up wins
/// over down even when it is already clamped at the first item.
pub fn tick_main_menu(
    mut state: MainMenuState,
    mut menu: MenuState,
    context: MenuContext,
    pressed_bits: u32,
    repeat_bits: u32,
) -> MainMenuUpdate {
    let mut effects = vec![];
    let callback_result = if pressed_bits & INPUT_CANCEL != 0 {
        effects.extend([Effect::PlaySound { id: 0x3b }, Effect::StopStream]);
        0x200
    } else if pressed_bits & crate::INPUT_CONFIRM != 0 {
        menu.choice3 = state.selection as i16;
        effects.push(Effect::PlaySound { id: 0x3c });
        0x100
    } else {
        let previous = state.selection;
        if repeat_bits & crate::INPUT_PREVIOUS != 0 {
            if state.selection > 0 {
                state.selection = state.selection.wrapping_sub(1);
            }
        } else if repeat_bits & crate::INPUT_NEXT != 0
            && state.selection < state.choice_count.wrapping_sub(1)
        {
            state.selection = state.selection.wrapping_add(1);
        }
        if previous != state.selection {
            effects.push(Effect::PlaySound { id: 0x3d });
        }
        effects.extend(main_menu_styles(state.selection, context.progress_flags));
        0
    };
    MainMenuUpdate {
        state,
        menu,
        context,
        callback_result,
        effects,
    }
}

/// `0x800D19E4`; styles and text-font changes occur in original call order.
pub fn main_menu_styles(selection: i32, progress_flags: u32) -> Vec<Effect> {
    if !(0..=2).contains(&selection) {
        return vec![];
    }
    let mut effects = Vec::with_capacity(6);
    for index in 0..3 {
        let selected = selection == index;
        let disabled = index == 2 && !selected && progress_flags & 2 == 0;
        effects.push(Effect::SetVisualStyle {
            slot: index + 1,
            style: if selected {
                1
            } else if disabled {
                3
            } else {
                2
            },
        });
        effects.push(Effect::SetTextFont {
            entry: index + 12,
            font: if selected {
                2
            } else if disabled {
                4
            } else {
                3
            },
        });
    }
    effects
}

/// Original `[0x800D87C0]`, initially `0x800DAFE0`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DifficultyState {
    pub phase: i32,
    pub selection: i32,
}
impl DifficultyState {
    pub const BYTE_LEN: usize = 8;
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        Ok(Self {
            phase: read_u32(bytes, 0) as i32,
            selection: read_u32(bytes, 4) as i32,
        })
    }
    pub fn write_into(&self, bytes: &mut [u8]) -> Result<(), Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        bytes[..4].copy_from_slice(&self.phase.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.selection.to_le_bytes());
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DifficultyUpdate {
    pub state: DifficultyState,
    pub context: MenuContext,
    pub callback_result: u32,
    pub effects: Vec<Effect>,
}

/// `0x800D1EF4` preserves the phase/selection words, including on re-entry.
pub fn initialize_difficulty(state: DifficultyState, context: MenuContext) -> DifficultyUpdate {
    let mut effects = vec![
        Effect::SelectVisualPage { page: 4 },
        Effect::SetVisualStyle { slot: 5, style: 3 },
        Effect::ClearTextVisibility,
    ];
    effects.extend([10, 24, 25, 26].map(|entry| Effect::SetTextVisibility { entry, value: 1 }));
    effects.push(Effect::SetContextByte {
        offset: 0x961,
        value: 6,
    });
    DifficultyUpdate {
        state,
        context,
        callback_result: 1,
        effects,
    }
}

/// `0x800D1F68` and its three phase callbacks. Confirm precedes cancel and
/// next precedes previous, the opposite of the main menu's direction priority.
pub fn tick_difficulty(
    mut state: DifficultyState,
    mut context: MenuContext,
    pressed_bits: u32,
) -> Result<DifficultyUpdate, Error> {
    let mut effects = vec![];
    let phase_result = match state.phase {
        0 => 1, // `0x800D1D78`: jr ra; li v0,1 in its delay slot.
        1 => {
            if pressed_bits & crate::INPUT_CONFIRM != 0 {
                effects.push(Effect::PlaySound { id: 0x3c });
                1
            } else if pressed_bits & INPUT_CANCEL != 0 {
                effects.push(Effect::PlaySound { id: 0x3b });
                4
            } else {
                let previous = state.selection;
                if pressed_bits & crate::INPUT_NEXT != 0 {
                    if state.selection < 2 {
                        state.selection = state.selection.wrapping_add(1);
                    }
                } else if pressed_bits & crate::INPUT_PREVIOUS != 0 && state.selection > 0 {
                    state.selection = state.selection.wrapping_sub(1);
                }
                if state.selection != previous {
                    effects.push(Effect::PlaySound { id: 0x3d });
                }
                effects.extend(difficulty_styles(state.selection));
                0
            }
        }
        2 => {
            effects.extend([
                Effect::SetContextWord {
                    offset: 0x398,
                    value: state.selection as u32,
                },
                Effect::ResetNewGameProgress,
            ]);
            context.current_pair[0] = 1;
            2
        }
        phase => return Err(Error::InvalidDifficultyPhase(phase)),
    };
    if phase_result & 1 != 0 {
        state.phase = state.phase.wrapping_add(1);
    }
    let callback_result = if phase_result & 2 != 0 {
        0x100
    } else {
        (phase_result & 4) << 7
    };
    Ok(DifficultyUpdate {
        state,
        context,
        callback_result,
        effects,
    })
}

pub fn difficulty_styles(selection: i32) -> Vec<Effect> {
    let mut effects = Vec::with_capacity(8);
    for index in 0..3 {
        effects.extend([
            Effect::SetVisualStyle {
                slot: index + 1,
                style: 2,
            },
            Effect::SetTextFont {
                entry: index + 24,
                font: 3,
            },
        ]);
    }
    effects.extend([
        Effect::SetVisualStyle {
            slot: selection.wrapping_add(1),
            style: 1,
        },
        Effect::SetTextFont {
            entry: selection.wrapping_add(24),
            font: 2,
        },
    ]);
    effects
}

/// `0x800D1D0C`: exact new-game progress-memory writes, leaving other bytes
/// untouched. This does not select the difficulty or change context +0x390;
/// those writes belong to the caller `0x800D1EAC`.
pub fn reset_new_game_progress(context_bytes: &mut [u8]) -> Result<(), Error> {
    require_bytes(context_bytes, 0xf70)?;
    for index in 0..60 {
        context_bytes[0xd50 + index] = 0;
        context_bytes[0xbe8 + index * 2..0xbea + index * 2]
            .copy_from_slice(&45000_u16.to_le_bytes());
        context_bytes[0xe7c + index * 4..0xe80 + index * 4].fill(0);
        context_bytes[0xd8c + index * 4..0xd90 + index * 4].fill(0);
    }
    context_bytes[0x9b8..0x9d0].fill(0);
    context_bytes[0x9b9] = 1;
    context_bytes[0xf6c..0xf70].fill(0);
    Ok(())
}
