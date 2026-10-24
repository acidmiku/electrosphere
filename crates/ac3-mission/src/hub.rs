use crate::{Effect, Error, MissionContext, MissionState, read_u32, require_bytes};

/// Scalar control fields in `[0x800F5E98]`, originally `0x800F9448`.
/// The map propagation arrays at +0x18/+0x98 remain a rendering effect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HubState {
    pub flags: u32,
    pub selection: i32,
    pub previous_selection: i32,
    pub phase: i32,
    pub timer: i32,
    pub reveal_timer: i32,
    pub exit_offset: i32,
}
impl HubState {
    pub const BYTE_LEN: usize = 0x120;
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        Ok(Self {
            flags: read_u32(bytes, 0),
            selection: read_u32(bytes, 4) as i32,
            previous_selection: read_u32(bytes, 8) as i32,
            phase: read_u32(bytes, 0xc) as i32,
            timer: read_u32(bytes, 0x10) as i32,
            reveal_timer: read_u32(bytes, 0x14) as i32,
            exit_offset: read_u32(bytes, 0x11c) as i32,
        })
    }
    pub fn write_into(&self, bytes: &mut [u8]) -> Result<(), Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        for (offset, value) in [
            (0, self.flags),
            (4, self.selection as u32),
            (8, self.previous_selection as u32),
            (0xc, self.phase as u32),
            (0x10, self.timer as u32),
            (0x14, self.reveal_timer as u32),
            (0x11c, self.exit_offset as u32),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubUpdate {
    pub state: HubState,
    pub mission: MissionState,
    pub context: MissionContext,
    pub callback_result: u32,
    pub effects: Vec<Effect>,
}

pub const HUB_PHASE_CALLBACKS: [u32; 4] = [0x800d4180, 0x800d4448, 0x800d45e4, 0x800d4760];

/// Scalar initialization at `0x800D483C`. The selected mission's map seed and
/// visual resources are explicit effects, not reconstructed by this function.
pub fn initialize_hub(mission: MissionState, context: MissionContext) -> HubUpdate {
    let selection = i32::from(mission.choice);
    let animate = context.options & 0x40 != 0 || should_slide(context.options, selection);
    let state = HubState {
        flags: u32::from(animate),
        selection,
        previous_selection: selection,
        phase: 0,
        timer: 25,
        reveal_timer: 0,
        exit_offset: 0,
    };
    let mut effects = vec![
        Effect::DrawHubBackdrop { selection },
        Effect::InitializeHubMap {
            mission: context.current_pair[0],
            placeholder: context.options & 0x10 != 0,
        },
    ];
    if selection == 2 || context.options & 0x40 != 0 {
        effects.push(Effect::SetVisualOffset {
            slot: 1,
            x: 0,
            y: 625,
        });
    }
    if animate {
        effects.extend([
            Effect::SetVisualOffset {
                slot: 2,
                x: -625,
                y: 0,
            },
            Effect::PlaySound { id: 0x5a },
        ]);
    }
    effects.extend([
        Effect::SetVisualOffset {
            slot: 3,
            x: 0,
            y: 0,
        },
        Effect::SetVisualStyle { slot: 5, style: 0 },
        Effect::ClearTextVisibility,
        Effect::SetTextVisibility {
            entry: selection.wrapping_add(0xb9),
            value: 1,
        },
        Effect::SetVisualParameter { slot: 1, value: 0 },
    ]);
    if context.options & 0x40 == 0 {
        effects.push(Effect::SetSceneColor {
            red: 143,
            green: 135,
            blue: 83,
        });
    } else {
        effects.extend([
            Effect::PlaySound { id: 0x62 },
            Effect::BeginHubBackgroundFade,
            Effect::PendingOriginalCall {
                address: 0x800e9824,
            },
            Effect::SetSceneColor {
                red: 0,
                green: 0,
                blue: 0,
            },
        ]);
    }
    if context.options & 0x50 == 0x50 {
        effects.push(Effect::PendingOriginalCall {
            address: 0x800cec18,
        });
    }
    effects.push(Effect::SelectVisualPage { page: 2 });
    highlight(selection, &mut effects);
    HubUpdate {
        state,
        mission,
        context,
        callback_result: 1,
        effects,
    }
}

/// `0x800D4B44` and all four hub phase callbacks. Input is context +0x12C:
/// confirm 0x10, next/right 8, previous/left 4, in that priority order.
pub fn tick_hub(
    mut state: HubState,
    mut mission: MissionState,
    mut context: MissionContext,
    input: u32,
) -> Result<HubUpdate, Error> {
    if !(0..4).contains(&state.phase) {
        return Err(Error::InvalidHubPhase(state.phase));
    }
    let mut effects = vec![Effect::DrawHubBackdrop {
        selection: state.selection,
    }];
    let result = match state.phase {
        0 => {
            if state.flags & 1 == 0 {
                state.timer = 0;
                1
            } else {
                navigate_or_mark_confirm(&mut state, input, &mut effects);
                highlight(state.selection, &mut effects);
                if state.flags & 4 != 0 {
                    state.timer = 0;
                    effects.extend([
                        Effect::SetVisualOffset {
                            slot: 1,
                            x: 0,
                            y: 0,
                        },
                        Effect::SetVisualOffset {
                            slot: 2,
                            x: 0,
                            y: 0,
                        },
                        Effect::SetSceneColor {
                            red: 143,
                            green: 135,
                            blue: 83,
                        },
                    ]);
                }
                if state.timer < 1 {
                    context.options &= !0x40;
                    state.flags &= !1;
                    state.timer = 0;
                    effects.push(Effect::PlaySound { id: 0x5c });
                    1
                } else {
                    let square = state.timer.wrapping_mul(state.timer);
                    if state.previous_selection == 2 || context.options & 0x40 != 0 {
                        effects.push(Effect::SetVisualOffset {
                            slot: 1,
                            x: 0,
                            y: square,
                        });
                        if context.options & 0x40 != 0 {
                            let amount = 25_i32.wrapping_sub(state.timer);
                            effects.push(Effect::SetSceneColor {
                                red: amount.wrapping_mul(143) / 25,
                                green: amount.wrapping_mul(135) / 25,
                                blue: amount.wrapping_mul(83) / 25,
                            });
                        }
                    }
                    effects.push(Effect::SetVisualOffset {
                        slot: 2,
                        x: square.wrapping_neg(),
                        y: 0,
                    });
                    state.timer = state.timer.wrapping_sub(1);
                    0
                }
            }
        }
        1 => {
            navigate_or_mark_confirm(&mut state, input, &mut effects);
            highlight(state.selection, &mut effects);
            if state.flags & 4 != 0 {
                effects.extend([
                    Effect::SetVisualParameter { slot: 1, value: 10 },
                    Effect::SetVisualStyle { slot: 5, style: 1 },
                ]);
                state.reveal_timer = 120;
                state.flags &= !4;
                1
            } else if state.reveal_timer < 60 {
                state.reveal_timer = state.reveal_timer.wrapping_add(1);
                if state.reveal_timer < 11 {
                    effects.push(Effect::SetVisualParameter {
                        slot: 1,
                        value: state.reveal_timer,
                    });
                }
                if state.reveal_timer == 30 {
                    effects.push(Effect::SetVisualStyle { slot: 5, style: 1 });
                }
                0
            } else {
                1
            }
        }
        2 => {
            if input & 0x10 != 0 {
                mission.choice = state.selection as i16;
                if should_slide(context.options, state.selection) {
                    state.flags |= 2;
                }
                effects.push(Effect::PlaySound { id: 0x3c });
                if state.flags & 2 != 0 {
                    effects.push(Effect::PlaySound { id: 0x5b });
                }
                1
            } else {
                navigate_or_mark_confirm(&mut state, input, &mut effects);
                highlight(state.selection, &mut effects);
                0
            }
        }
        3 => {
            if state.timer < 25 {
                let old = state.timer;
                state.timer = state.timer.wrapping_add(1);
                state.exit_offset = old.wrapping_neg().wrapping_mul(old);
                let square = state.timer.wrapping_mul(state.timer);
                if state.selection == 2 {
                    effects.push(Effect::SetVisualOffset {
                        slot: 1,
                        x: 0,
                        y: square,
                    });
                }
                if state.flags & 2 != 0 {
                    effects.push(Effect::SetVisualOffset {
                        slot: 2,
                        x: square.wrapping_neg(),
                        y: 0,
                    });
                }
                effects.push(Effect::SetVisualOffset {
                    slot: 3,
                    x: square.wrapping_neg(),
                    y: 0,
                });
                0
            } else {
                state.flags &= !2;
                2
            }
        }
        _ => unreachable!(),
    };
    if result == 1 {
        state.phase = state.phase.wrapping_add(1);
    }
    let callback_result = if result == 2 {
        0x100
    } else {
        effects.extend([
            Effect::ClearTextVisibility,
            Effect::SetTextVisibility {
                entry: state.selection.wrapping_add(0xb9),
                value: 1,
            },
            Effect::PendingOriginalCall {
                address: 0x800d3c50,
            },
            Effect::DrawHubMap {
                reveal_timer: state.reveal_timer,
                exit_offset: state.exit_offset,
            },
        ]);
        0
    };
    Ok(HubUpdate {
        state,
        mission,
        context,
        callback_result,
        effects,
    })
}

fn should_slide(options: u32, selection: i32) -> bool {
    if options & 0x10 == 0 {
        selection != 3
    } else {
        selection == 1 || selection == 2
    }
}
fn navigate_or_mark_confirm(state: &mut HubState, input: u32, effects: &mut Vec<Effect>) {
    if input & 0x10 != 0 {
        state.flags |= 4;
    } else if input & 8 != 0 {
        state.selection = if state.selection < 3 {
            state.selection.wrapping_add(1)
        } else {
            0
        };
        effects.push(Effect::PlaySound { id: 0x3d });
    } else if input & 4 != 0 {
        state.selection = if state.selection < 1 {
            3
        } else {
            state.selection.wrapping_sub(1)
        };
        effects.push(Effect::PlaySound { id: 0x3d });
    }
}
fn highlight(selection: i32, effects: &mut Vec<Effect>) {
    effects.extend((1..=4).map(|slot| Effect::SetVisualStyle { slot, style: 2 }));
    effects.push(Effect::SetVisualStyle {
        slot: selection.wrapping_add(1),
        style: 1,
    });
}
