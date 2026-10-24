//! Original 0x800EEE70/0x800EF01C control, with an explicit rendering boundary.
use crate::briefing_script::{tick_briefing_camera, tick_briefing_objects};
use crate::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FullBriefingState {
    pub control: BriefingState,
    pub script: BriefingScriptState,
    pub visual_address: u32,
    pub panel_timer: i32,
    pub panel_offsets: [i16; 2],
    pub zoom: i32,
    pub view_angles: [i16; 2],
    pub zoom_timer: i32,
    pub view_offsets: [i16; 2],
    pub text_timer: i32,
    pub text_count: i32,
    pub text_index: i32,
}
impl FullBriefingState {
    pub const BYTE_LEN: usize = 0x554;
    pub fn from_bytes(bytes: &[u8], script_base: u32) -> Result<Self, BriefingScriptError> {
        if bytes.len() < Self::BYTE_LEN {
            return Err(BriefingScriptError::BufferTooShort {
                required: Self::BYTE_LEN,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            control: BriefingState {
                phase: read_u16(bytes, 4) as i16,
                substate: read_u16(bytes, 6) as i16,
            },
            script: BriefingScriptState::from_bytes(bytes, script_base)?,
            visual_address: read_u32(bytes, 0x10),
            panel_timer: read_u32(bytes, 0x14) as i32,
            panel_offsets: [read_u16(bytes, 0x18) as i16, read_u16(bytes, 0x1a) as i16],
            zoom: read_u32(bytes, 0x538) as i32,
            view_angles: [read_u16(bytes, 0x53c) as i16, read_u16(bytes, 0x53e) as i16],
            zoom_timer: read_u32(bytes, 0x540) as i32,
            view_offsets: [read_u16(bytes, 0x544) as i16, read_u16(bytes, 0x546) as i16],
            text_timer: read_u32(bytes, 0x548) as i32,
            text_count: read_u32(bytes, 0x54c) as i32,
            text_index: read_u32(bytes, 0x550) as i32,
        })
    }
    /// Init overwrites the script pointer; its prior value need not point into this script.
    pub fn from_bytes_for_initialization(bytes: &[u8]) -> Result<Self, BriefingScriptError> {
        if bytes.len() < Self::BYTE_LEN {
            return Err(BriefingScriptError::BufferTooShort {
                required: Self::BYTE_LEN,
                actual: bytes.len(),
            });
        }
        Self::from_bytes(bytes, read_u32(bytes, 0xc))
    }
    pub fn write_into(
        &self,
        bytes: &mut [u8],
        script_base: u32,
    ) -> Result<(), BriefingScriptError> {
        if bytes.len() < Self::BYTE_LEN {
            return Err(BriefingScriptError::BufferTooShort {
                required: Self::BYTE_LEN,
                actual: bytes.len(),
            });
        }
        self.script.write_into(bytes, script_base)?;
        for (offset, value) in [
            (4, self.control.phase),
            (6, self.control.substate),
            (0x18, self.panel_offsets[0]),
            (0x1a, self.panel_offsets[1]),
            (0x53c, self.view_angles[0]),
            (0x53e, self.view_angles[1]),
            (0x544, self.view_offsets[0]),
            (0x546, self.view_offsets[1]),
        ] {
            bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        for (offset, value) in [
            (0x10, self.visual_address),
            (0x14, self.panel_timer as u32),
            (0x538, self.zoom as u32),
            (0x540, self.zoom_timer as u32),
            (0x548, self.text_timer as u32),
            (0x54c, self.text_count as u32),
            (0x550, self.text_index as u32),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BriefingEffect {
    Common(Effect),
    /// 0x800F1788, including the original color index (briefing uses 120).
    SetVisualColor {
        slot: i32,
        rgb: [u8; 3],
    },
    /// 0x800D0B74(language, index, scene+0x200B8), then 0x800D04C8 font 16.
    DrawTextLine {
        index: i32,
        position: [i32; 2],
    },
    /// 0x800EEA20: localized string 9, mission name, selected label name, grid.
    DrawMapText {
        selected_label: i32,
        grid: [i16; 2],
    },
    /// 0x800EB754 before this frame's object projection; uses existing screen fields.
    DrawLabelConnectors {
        visual_offset: [i16; 2],
    },
    /// 0x800ED834: camera/terrain/marker/object drawing; object state mutates.
    DrawScene,
    /// 0x800ED8B0, called even when scene drawing is disabled.
    DrawMinimap,
}
impl From<Effect> for BriefingEffect {
    fn from(effect: Effect) -> Self {
        Self::Common(effect)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BriefingFrameInput {
    pub pressed: u32,
    /// Return value of original 0x800F1BA0; retained only for the memory bridge.
    pub visual_address: u32,
    /// Signed visual slot 2 offsets read by 0x800F1908 before label updates.
    pub visual_offset2: [i16; 2],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BriefingUpdate {
    pub state: FullBriefingState,
    pub callback_result: u32,
    pub effects: Vec<BriefingEffect>,
    /// Must be resolved by finish_briefing_render, even when the outcome set is empty.
    pub render_pending: bool,
}
fn sound(effects: &mut Vec<BriefingEffect>, id: u32) {
    effects.push(Effect::PlaySound { id }.into());
}
fn parameter(effects: &mut Vec<BriefingEffect>, slot: i32, value: i32) {
    effects.push(Effect::SetVisualParameter { slot, value }.into());
}
fn style(effects: &mut Vec<BriefingEffect>, slot: i32, value: i32) {
    effects.push(
        Effect::SetVisualStyle {
            slot,
            style: value as u32,
        }
        .into(),
    );
}
fn offset(effects: &mut Vec<BriefingEffect>, slot: i32, x: i32, y: i32) {
    effects.push(Effect::SetVisualOffset { slot, x, y }.into());
}
fn append(
    update: BriefingScriptUpdate,
    state: &mut FullBriefingState,
    effects: &mut Vec<BriefingEffect>,
) {
    state.script = update.state;
    effects.extend(update.effects.into_iter().map(Into::into));
}

/// Full original initialization; line count is the checked localized table lookup.
pub fn initialize_briefing(
    mut state: FullBriefingState,
    script: &BriefingScript,
    text_count: i32,
    math: &impl BriefingMath,
) -> Result<BriefingUpdate, BriefingScriptError> {
    let mut effects = vec![];
    append(
        reset_briefing_script(state.script),
        &mut state,
        &mut effects,
    );
    state.control = BriefingState::default();
    state.text_timer = 0;
    state.text_index = 0;
    state.text_count = text_count;
    state.panel_timer = 0;
    state.panel_offsets = [625, -625];
    append(
        dispatch_briefing_prefix(state.script, script, math)?,
        &mut state,
        &mut effects,
    );
    effects.push(Effect::SelectVisualPage { page: 3 }.into());
    parameter(&mut effects, 9, 0);
    parameter(&mut effects, 10, 0);
    effects.push(BriefingEffect::SetVisualColor {
        slot: 120,
        rgb: [128; 3],
    });
    effects.push(Effect::ClearTextVisibility.into());
    style(&mut effects, 9, 1);
    style(&mut effects, 11, 1);
    effects.push(
        Effect::PendingOriginalCall {
            address: 0x800e983c,
        }
        .into(),
    );
    effects.push(Effect::StartStream { resource: 0x827 }.into());
    effects.push(
        Effect::PendingOriginalCallWithArgs {
            address: 0x800e7a18,
            arguments: vec![30, 10],
        }
        .into(),
    );
    Ok(BriefingUpdate {
        state,
        callback_result: 1,
        effects,
        render_pending: false,
    })
}

/// 0x800EB9E8. Uses signed 16-bit label timers and strict flag priority.
pub fn tick_briefing_labels(
    mut state: BriefingScriptState,
    visual_offset: [i16; 2],
) -> BriefingScriptUpdate {
    let mut effects = vec![];
    for (index, label) in state.labels.iter_mut().enumerate() {
        let flags = label.flags;
        if flags & 1 == 0 {
            continue;
        }
        let slot = index as i32 + 1;
        if flags & 0x20 != 0 {
            if label.timer < 1 {
                label.flags &= !0x22;
                label.timer = 0;
            } else {
                effects.push(Effect::SetVisualParameter {
                    slot,
                    value: i32::from(label.timer),
                });
                label.timer = label.timer.wrapping_sub(1);
            }
        } else if flags & 0x40 != 0 {
            if label.timer < 25 {
                label.timer = label.timer.wrapping_add(1);
            } else {
                label.flags = (flags & !0x40) | 4;
                label.timer = 0;
                effects.push(Effect::PlaySound { id: 100 });
            }
        } else if flags & 4 != 0 {
            if label.timer >= 25 {
                label.flags &= !4;
                label.timer = 0;
            } else {
                effects.push(Effect::SetVisualParameter {
                    slot,
                    value: i32::from(label.timer),
                });
                label.timer = label.timer.wrapping_add(1);
            }
        } else {
            let visible = flags & 0x1a != 0;
            if flags & 8 != 0 {
                if label.timer < 25 {
                    label.y = label.timer.wrapping_mul(5);
                    label.timer = label.timer.wrapping_add(1);
                } else {
                    label.flags &= !8;
                    label.timer = 0;
                }
            } else if flags & 0x10 != 0 {
                if label.timer < 25 {
                    label.y = label.timer.wrapping_mul(18).wrapping_add(125);
                    label.timer = label.timer.wrapping_add(1);
                } else {
                    label.flags &= !0x12;
                    label.timer = 0;
                }
            } else if !visible {
                label.y = 0;
            }
            effects.push(Effect::SetVisualParameter {
                slot,
                value: if visible { 25 } else { 0 },
            });
        }
        effects.push(Effect::SetVisualOffset {
            slot: index as i32 + 4,
            x: i32::from(label.x) + i32::from(visual_offset[0]),
            y: i32::from(label.y) + i32::from(visual_offset[1]),
        });
    }
    BriefingScriptUpdate {
        state,
        effects,
        records_executed: 0,
    }
}
fn shared_updates(
    state: &mut FullBriefingState,
    visual_offset: [i16; 2],
    effects: &mut Vec<BriefingEffect>,
    math: &impl BriefingMath,
) -> Result<(), BriefingScriptError> {
    state.script = tick_briefing_objects(state.script, math)?;
    append(
        tick_briefing_labels(state.script, visual_offset),
        state,
        effects,
    );
    state.script = tick_briefing_camera(state.script, math)?;
    Ok(())
}
fn label_index(state: &BriefingScriptState) -> Result<usize, BriefingScriptError> {
    usize::try_from(state.selected_label)
        .ok()
        .filter(|&i| i < 8)
        .ok_or(BriefingScriptError::InvalidLabel(state.selected_label))
}
fn object_position(
    state: &BriefingScriptState,
    index: u8,
) -> Result<[i32; 2], BriefingScriptError> {
    let object = state
        .objects
        .get(usize::from(index))
        .ok_or(BriefingScriptError::InvalidObject(index))?;
    Ok([
        i32::from(object.halfword(0xe).unwrap()),
        i32::from(object.halfword(0x12).unwrap()),
    ])
}
fn move_camera_to_label(
    state: &mut BriefingScriptState,
    math: &impl BriefingMath,
) -> Result<(), BriefingScriptError> {
    let p = object_position(state, state.labels[label_index(state)?].object)?;
    let y = p[0].wrapping_sub(state.camera[0]);
    let x = p[1].wrapping_sub(state.camera[2]);
    state.camera_duration = 50;
    state.camera_speed = math.evaluate(BriefingMathRequest::SquareRoot {
        value: y.wrapping_mul(y).wrapping_add(x.wrapping_mul(x)) as u32,
    })? / 50;
    state.camera_angles[1] = math.evaluate(BriefingMathRequest::Atan2 { y, x })? as i16;
    Ok(())
}
/// 0x800EDD28, including right-over-left priority and interrupted fade reversal.
pub fn navigate_briefing_label(
    mut state: BriefingScriptState,
    pressed: u32,
) -> Result<BriefingScriptUpdate, BriefingScriptError> {
    if !(1..=8).contains(&state.label_count) {
        return Err(BriefingScriptError::InvalidLabelCount(state.label_count));
    }
    let mut effects = vec![];
    let old = label_index(&state)?;
    let label = &mut state.labels[old];
    if label.flags & 4 != 0 {
        label.flags = (label.flags & !4) | 0x20;
        effects.push(Effect::PlaySound { id: 0x65 });
    } else if label.flags & 0x40 != 0 {
        label.flags &= !0x42;
    } else {
        label.timer = 25;
        label.flags |= 0x20;
        effects.push(Effect::PlaySound { id: 0x65 });
    }
    if pressed & 8 != 0 {
        state.selected_label = if state.selected_label >= state.label_count - 1 {
            0
        } else {
            state.selected_label.wrapping_add(1)
        };
    } else if pressed & 4 != 0 {
        state.selected_label = if state.selected_label < 1 {
            state.label_count - 1
        } else {
            state.selected_label.wrapping_sub(1)
        };
    }
    let new = label_index(&state)?;
    let label = &mut state.labels[new];
    if label.flags & 0x20 == 0 {
        label.flags |= 0x42;
        label.timer = 0;
        label.x = 0;
        label.y = 0;
    } else {
        label.flags = (label.flags & !0x20) | 4;
    }
    Ok(BriefingScriptUpdate {
        state,
        effects,
        records_executed: 0,
    })
}

fn phase_tick(
    state: &mut FullBriefingState,
    script: &BriefingScript,
    input: BriefingFrameInput,
    effects: &mut Vec<BriefingEffect>,
    math: &impl BriefingMath,
) -> Result<u32, BriefingScriptError> {
    let pressed = input.pressed;
    Ok(match (state.control.phase, state.control.substate) {
        (0, 0) => {
            state.zoom = 0x7fff;
            state.view_angles = [0; 2];
            state.zoom_timer = 0;
            state.view_offsets = [0; 2];
            2
        }
        (0, 1) => {
            if pressed & 0x10 != 0 {
                state.panel_timer = 50;
            }
            let timer = state.panel_timer;
            if timer == 0 {
                sound(effects, 0x5a);
            }
            if timer < 25 {
                state.panel_timer = timer.wrapping_add(1);
                let d = 25i32.wrapping_sub(state.panel_timer);
                let y = d.wrapping_neg().wrapping_mul(d);
                offset(effects, 3, 0, y);
                state.panel_offsets[1] = y as i16;
                0
            } else if timer > 49 {
                state.panel_timer = 0;
                offset(effects, 2, 0, 0);
                offset(effects, 3, 0, 0);
                state.panel_offsets = [0; 2];
                style(effects, 9, 4);
                sound(effects, 100);
                2
            } else {
                if timer == 25 {
                    sound(effects, 0x5c);
                }
                state.panel_timer = timer.wrapping_add(1);
                let d = 50i32.wrapping_sub(state.panel_timer);
                let y = d.wrapping_mul(d);
                offset(effects, 2, 0, y);
                offset(effects, 3, 0, 0);
                state.panel_offsets = [y as i16, 0];
                0
            }
        }
        (0, 2) => {
            let timer = state.zoom_timer;
            parameter(effects, 9, timer);
            parameter(effects, 10, timer);
            style(effects, 10, timer / 5);
            if timer < 25 {
                state.zoom_timer = timer.wrapping_add(1);
                0
            } else {
                state.zoom_timer = 0;
                style(effects, 9, 0);
                style(effects, 11, 0);
                sound(effects, 0x66);
                2
            }
        }
        (0, 3) => {
            let t = state.zoom_timer;
            let remaining = 25i32.wrapping_sub(t);
            state.script.flags |= 1;
            state.zoom = 0x7fffi32.wrapping_sub(t.wrapping_mul(0x6da3) / 25);
            state.view_angles = [
                (t.wrapping_mul(-512) / 25).wrapping_add(1024) as i16,
                (t.wrapping_shl(8) / 25).wrapping_add(2048) as i16,
            ];
            let color = (remaining.wrapping_mul(128) / 25) as u8;
            effects.push(BriefingEffect::SetVisualColor {
                slot: 120,
                rgb: [color; 3],
            });
            state.view_offsets = [
                ((state.script.camera[0] / 128).wrapping_sub(128) / 4)
                    .wrapping_mul(remaining)
                    .wrapping_div(25) as i16,
                (128i32.wrapping_sub(state.script.camera[2] / 128) / 2)
                    .wrapping_mul(remaining)
                    .wrapping_div(25) as i16,
            ];
            if t < 25 {
                state.zoom_timer = t.wrapping_add(1);
                0
            } else {
                1
            }
        }
        (1, 0) => {
            append(
                dispatch_briefing_due(state.script, script, math)?,
                state,
                effects,
            );
            if pressed & 0x20 != 0 {
                state.script.flags |= 4;
                return Ok(0x10);
            }
            if pressed & 0x40 != 0 || (pressed & 0x10 != 0 && state.script.flags & 0x10 != 0) {
                if state.script.flags & 0x20 != 0 {
                    return Ok(2);
                }
                sound(effects, 0x3c);
                state.script.flags |= 2;
                let mut clock = state.script.clock;
                // A valid signed16 timeline fits this bound. Malformed/infinite scripts fail explicitly.
                for _ in 0..=65536 {
                    while i32::from(script.command(state.script.cursor)?.time) == clock {
                        append(
                            apply_briefing_command(
                                state.script,
                                script.command(state.script.cursor)?,
                                math,
                            )?,
                            state,
                            effects,
                        );
                        state.script.cursor += 1;
                        if script.command(state.script.cursor)?.opcode == 0 {
                            state.script.finish_timer = 0;
                            return Ok(2);
                        }
                    }
                    clock = clock.wrapping_add(1);
                    shared_updates(state, input.visual_offset2, effects, math)?;
                }
                return Err(BriefingScriptError::FastForwardBudgetExceeded);
            }
            if state.script.flags & 0x30 == 0x30 {
                return Ok(2);
            }
            state.script.clock = state.script.clock.wrapping_add(1);
            if state.script.flags & 0x10 == 0 {
                if state.script.flags & 8 == 0 {
                    if pressed & 0x10 != 0 {
                        state.text_timer = 95;
                    }
                    if state.text_timer > 94 {
                        state.text_timer = 0;
                        if state.text_index < state.text_count.wrapping_sub(1) {
                            if (state.text_index / 3).wrapping_mul(3)
                                != state.text_index.wrapping_sub(2)
                            {
                                state.text_index = state.text_index.wrapping_add(1);
                            } else {
                                state.script.flags |= 8;
                            }
                        } else {
                            state.script.flags |= 0x10;
                        }
                    } else {
                        state.text_timer = state.text_timer.wrapping_add(1);
                    }
                } else {
                    if pressed & 0x10 != 0 {
                        state.text_timer = 15;
                    }
                    if state.text_timer > 14 {
                        state.text_index = state.text_index.wrapping_add(1);
                        state.text_timer = 0;
                        state.script.flags &= !8;
                    } else {
                        state.text_timer = state.text_timer.wrapping_add(1);
                    }
                }
            }
            if state.script.flags & 8 == 0 {
                let remainder = state.text_index % 3;
                for row in 0..remainder + 1 {
                    effects.push(BriefingEffect::DrawTextLine {
                        index: state.text_index.wrapping_sub(remainder).wrapping_add(row),
                        position: [25, 320 + row * 20],
                    });
                }
            }
            0
        }
        (1, 1) => {
            if pressed & 0x10 != 0 {
                sound(effects, 0x3c);
                2
            } else if state.script.finish_timer < 25 {
                state.script.finish_timer = state.script.finish_timer.wrapping_add(1);
                0
            } else {
                2
            }
        }
        (1, 2) => {
            for label in &mut state.script.labels {
                if label.flags & 2 != 0 {
                    label.timer = 25;
                    label.flags |= 0x20;
                    sound(effects, 0x65);
                }
            }
            2
        }
        (1, 3) => u32::from(
            state
                .script
                .labels
                .iter()
                .all(|label| label.flags & 0x20 == 0),
        ),
        (2, 0) => {
            state.script.flags &= !2;
            let i = label_index(&state.script)?;
            let label = &mut state.script.labels[i];
            label.timer = 0;
            label.x = 0;
            label.y = 0;
            label.flags |= 0x42;
            let p = object_position(&state.script, state.script.labels[i].object)?;
            state.script.camera = [p[0], 0, p[1]];
            2
        }
        (2, 1) => {
            effects.push(BriefingEffect::DrawMapText {
                selected_label: state.script.selected_label,
                grid: [
                    (state.script.camera[0] / 128).wrapping_add(128) as i16,
                    (state.script.camera[2] / 128).wrapping_add(128) as i16,
                ],
            });
            if pressed & 0x20 != 0 {
                state.script.flags |= 4;
                sound(effects, 0x3b);
                0x10
            } else if pressed & 0x10 != 0 {
                sound(effects, 0x3c);
                0x10
            } else if pressed & 0xc != 0 {
                append(
                    navigate_briefing_label(state.script, pressed)?,
                    state,
                    effects,
                );
                move_camera_to_label(&mut state.script, math)?;
                sound(effects, 0x3d);
                0
            } else {
                0
            }
        }
        (2, 2) | (3, 1) => {
            if state.panel_timer < 1 {
                if state.script.flags & 4 == 0 { 4 } else { 8 }
            } else {
                let t = 25i32.wrapping_sub(state.panel_timer);
                let y = t.wrapping_mul(t);
                state.panel_offsets = [y as i16, y.wrapping_neg() as i16];
                offset(effects, 3, 0, y.wrapping_neg());
                offset(effects, 2, 0, y);
                state.panel_timer = state.panel_timer.wrapping_sub(1);
                0
            }
        }
        (3, 0) => {
            state.panel_timer = 25;
            sound(effects, 0x5b);
            for label in &mut state.script.labels {
                if label.flags & 2 != 0 {
                    label.flags |= 0x20;
                }
            }
            2
        }
        (phase, substate) => {
            return Err(BriefingScriptError::InvalidControlPhase { phase, substate });
        }
    })
}

/// Runs original control up to 0x800ED834. Finish with explicit renderer outcomes.
pub fn prepare_briefing_tick(
    mut state: FullBriefingState,
    script: &BriefingScript,
    input: BriefingFrameInput,
    math: &impl BriefingMath,
) -> Result<BriefingUpdate, BriefingScriptError> {
    state.visual_address = input.visual_address;
    let mut effects = vec![];
    let result = phase_tick(&mut state, script, input, &mut effects, math)?;
    if result & 2 != 0 {
        state.control.substate = state.control.substate.wrapping_add(1);
    } else if result & 1 != 0 {
        state.control.phase = state.control.phase.wrapping_add(1);
        state.control.substate = 0;
    } else if result & 4 != 0 {
        effects.push(
            Effect::PendingOriginalCall {
                address: 0x800e9824,
            }
            .into(),
        );
        effects.push(Effect::StopStream.into());
        return Ok(BriefingUpdate {
            state,
            callback_result: 0x100,
            effects,
            render_pending: false,
        });
    } else if result & 8 != 0 {
        effects.push(Effect::StopStream.into());
        effects.push(
            Effect::PendingOriginalCallWithArgs {
                address: 0x800e7a18,
                arguments: vec![100, 10],
            }
            .into(),
        );
        return Ok(BriefingUpdate {
            state,
            callback_result: 0x200,
            effects,
            render_pending: false,
        });
    } else if result & 0x10 != 0 {
        state.control = BriefingState {
            phase: 3,
            substate: 0,
        };
    }
    let mut visual_offset = input.visual_offset2;
    for effect in &effects {
        if let BriefingEffect::Common(Effect::SetVisualOffset { slot: 2, x, y }) = effect {
            visual_offset = [*x as i16, *y as i16];
        }
    }
    shared_updates(&mut state, visual_offset, &mut effects, math)?;
    if input.pressed & 0x40 == 0 {
        effects.push(BriefingEffect::DrawLabelConnectors { visual_offset });
    }
    effects.push(BriefingEffect::DrawScene);
    effects.push(BriefingEffect::DrawMinimap);
    Ok(BriefingUpdate {
        state,
        callback_result: 0,
        effects,
        render_pending: true,
    })
}

/// Enforces a complete object-rendering boundary; no fabricated screen positions.
pub fn finish_briefing_render(
    mut update: BriefingUpdate,
    outcomes: &[BriefingObjectDrawOutcome],
) -> Result<BriefingUpdate, BriefingScriptError> {
    let expected: Vec<u8> = if update.render_pending && update.state.script.flags & 1 != 0 {
        briefing_draw_requests(&update.state.script)
            .iter()
            .map(|r| r.index)
            .collect()
    } else {
        vec![]
    };
    for outcome in outcomes {
        if !expected.contains(&outcome.index) {
            return Err(BriefingScriptError::UnexpectedDrawOutcome(outcome.index));
        }
    }
    for index in expected {
        if !outcomes.iter().any(|o| o.index == index) {
            return Err(BriefingScriptError::MissingDrawOutcome(index));
        }
    }
    update.state.script = apply_briefing_draw_outcomes(update.state.script, outcomes)?;
    update.render_pending = false;
    Ok(update)
}
