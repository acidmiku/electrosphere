//! Bounded script operations from the original briefing. This is not a complete
//! 0x800EF01C wrapper: text timing, label animation and drawing remain separate.
use crate::{Effect, read_u16, read_u32};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BriefingMathRequest {
    Atan2 { y: i32, x: i32 },
    SquareRoot { value: u32 },
    Sin { angle: i32 },
    Cos { angle: i32 },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BriefingScriptError {
    BufferTooShort {
        required: usize,
        actual: usize,
    },
    InvalidScriptPointer(u32),
    ScriptAddressOverflow,
    PartialRecord {
        bytes: usize,
    },
    InvalidOpcode(u8),
    CursorOutsideScript(usize),
    InvalidObject(u8),
    InvalidLabelCount(i32),
    DivisionByZero,
    DivisionOverflow,
    MissingMath(BriefingMathRequest),
    InvalidMath {
        request: BriefingMathRequest,
        reason: String,
    },
    DuplicateDrawOutcome(u8),
    InvalidControlPhase {
        phase: i16,
        substate: i16,
    },
    InvalidLabel(i32),
    FastForwardBudgetExceeded,
    MissingDrawOutcome(u8),
    UnexpectedDrawOutcome(u8),
}
impl std::fmt::Display for BriefingScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for BriefingScriptError {}

/// Implementations must use the original PsyQ integer behavior, including its
/// tables/truncation. No floating approximation or fallback is supplied here.
pub trait BriefingMath {
    fn evaluate(&self, request: BriefingMathRequest) -> Result<i32, BriefingScriptError>;
}
/// Commands without mathematical dependencies remain usable before math is ported.
pub struct MissingBriefingMath;
impl BriefingMath for MissingBriefingMath {
    fn evaluate(&self, request: BriefingMathRequest) -> Result<i32, BriefingScriptError> {
        Err(BriefingScriptError::MissingMath(request))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BriefingCommand {
    pub time: i16,
    pub opcode: u8,
    pub slot: u8,
    /// Original halfwords +4,+6,+8,+10,+12,+14.
    pub arguments: [i16; 6],
}
impl BriefingCommand {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, BriefingScriptError> {
        if bytes.len() != 16 {
            return Err(BriefingScriptError::PartialRecord { bytes: bytes.len() });
        }
        if bytes[2] > 7 {
            return Err(BriefingScriptError::InvalidOpcode(bytes[2]));
        }
        Ok(Self {
            time: read_u16(bytes, 0) as i16,
            opcode: bytes[2],
            slot: bytes[3],
            arguments: std::array::from_fn(|i| read_u16(bytes, 4 + i * 2) as i16),
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BriefingScript {
    commands: Vec<BriefingCommand>,
}
impl BriefingScript {
    pub fn parse(bytes: &[u8]) -> Result<Self, BriefingScriptError> {
        if !bytes.len().is_multiple_of(16) {
            return Err(BriefingScriptError::PartialRecord { bytes: bytes.len() });
        }
        Ok(Self {
            commands: bytes
                .chunks_exact(16)
                .map(BriefingCommand::from_bytes)
                .collect::<Result<_, _>>()?,
        })
    }
    pub fn commands(&self) -> &[BriefingCommand] {
        &self.commands
    }
    pub(crate) fn command(&self, cursor: usize) -> Result<BriefingCommand, BriefingScriptError> {
        self.commands
            .get(cursor)
            .copied()
            .ok_or(BriefingScriptError::CursorOutsideScript(cursor))
    }
}

/// Original 0x24-byte object. Unmodeled/unused bytes are preserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BriefingObject(pub [u8; 0x24]);
impl Default for BriefingObject {
    fn default() -> Self {
        Self([0; 0x24])
    }
}
impl BriefingObject {
    pub fn flags(self) -> u32 {
        read_u32(&self.0, 0)
    }
    pub fn halfword(self, offset: usize) -> Option<i16> {
        if offset.is_multiple_of(2) && offset.checked_add(2).is_some_and(|end| end <= self.0.len())
        {
            Some(read_u16(&self.0, offset) as i16)
        } else {
            None
        }
    }
    fn set_half(&mut self, offset: usize, value: i16) {
        self.0[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn set_flags(&mut self, value: u32) {
        self.0[..4].copy_from_slice(&value.to_le_bytes());
    }
    fn half(&self, offset: usize) -> i16 {
        read_u16(&self.0, offset) as i16
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BriefingLabel {
    pub flags: u32,
    pub style: u8,
    pub object: u8,
    pub timer: i16,
    pub x: i16,
    pub y: i16,
}
/// Scalar state written by the script and its movement helpers. Cursor is a
/// checked record index, replacing the original guest pointer at +0xC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BriefingScriptState {
    pub flags: u32,
    pub clock: i32,
    pub cursor: usize,
    pub objects: [BriefingObject; 32],
    pub label_count: i32,
    pub labels: [BriefingLabel; 8],
    pub selected_label: i32,
    pub finish_timer: i32,
    pub marker: [i16; 3],
    pub marker_id: i32,
    pub camera_duration: i32,
    pub camera: [i32; 3],
    pub camera_speed: i32,
    pub camera_angles: [i16; 3],
}
impl BriefingScriptState {
    pub const BYTE_LEN: usize = 0x536;
    /// Reads only the modeled fields; translates the script pointer to a record index.
    pub fn from_bytes(bytes: &[u8], script_base: u32) -> Result<Self, BriefingScriptError> {
        if bytes.len() < Self::BYTE_LEN {
            return Err(BriefingScriptError::BufferTooShort {
                required: Self::BYTE_LEN,
                actual: bytes.len(),
            });
        }
        let pointer = read_u32(bytes, 0xc);
        let offset = pointer
            .checked_sub(script_base)
            .filter(|offset| offset.is_multiple_of(16))
            .ok_or(BriefingScriptError::InvalidScriptPointer(pointer))?;
        Ok(Self {
            flags: read_u32(bytes, 0),
            clock: read_u32(bytes, 8) as i32,
            cursor: (offset / 16) as usize,
            objects: std::array::from_fn(|index| {
                BriefingObject(
                    bytes[0x1c + index * 36..0x40 + index * 36]
                        .try_into()
                        .unwrap(),
                )
            }),
            label_count: read_u32(bytes, 0x4a0) as i32,
            labels: std::array::from_fn(|index| {
                let start = 0x4a4 + index * 12;
                BriefingLabel {
                    flags: read_u32(bytes, start),
                    style: bytes[start + 4],
                    object: bytes[start + 5],
                    timer: read_u16(bytes, start + 6) as i16,
                    x: read_u16(bytes, start + 8) as i16,
                    y: read_u16(bytes, start + 10) as i16,
                }
            }),
            selected_label: read_u32(bytes, 0x504) as i32,
            finish_timer: read_u32(bytes, 0x508) as i32,
            marker: std::array::from_fn(|index| read_u16(bytes, 0x50c + index * 2) as i16),
            marker_id: read_u32(bytes, 0x514) as i32,
            camera_duration: read_u32(bytes, 0x518) as i32,
            camera: std::array::from_fn(|index| read_u32(bytes, 0x51c + index * 4) as i32),
            camera_speed: read_u32(bytes, 0x52c) as i32,
            camera_angles: std::array::from_fn(|index| read_u16(bytes, 0x530 + index * 2) as i16),
        })
    }
    /// Sparse stores preserve phase, text counters, projection state and padding.
    pub fn write_into(self, bytes: &mut [u8], script_base: u32) -> Result<(), BriefingScriptError> {
        if bytes.len() < Self::BYTE_LEN {
            return Err(BriefingScriptError::BufferTooShort {
                required: Self::BYTE_LEN,
                actual: bytes.len(),
            });
        }
        let offset = self
            .cursor
            .checked_mul(16)
            .and_then(|offset| u32::try_from(offset).ok())
            .ok_or(BriefingScriptError::ScriptAddressOverflow)?;
        let pointer = script_base
            .checked_add(offset)
            .ok_or(BriefingScriptError::ScriptAddressOverflow)?;
        for (offset, value) in [
            (0, self.flags),
            (8, self.clock as u32),
            (0xc, pointer),
            (0x4a0, self.label_count as u32),
            (0x504, self.selected_label as u32),
            (0x508, self.finish_timer as u32),
            (0x514, self.marker_id as u32),
            (0x518, self.camera_duration as u32),
            (0x52c, self.camera_speed as u32),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (index, object) in self.objects.iter().enumerate() {
            bytes[0x1c + index * 36..0x40 + index * 36].copy_from_slice(&object.0);
        }
        for (index, label) in self.labels.iter().enumerate() {
            let start = 0x4a4 + index * 12;
            bytes[start..start + 4].copy_from_slice(&label.flags.to_le_bytes());
            bytes[start + 4] = label.style;
            bytes[start + 5] = label.object;
            bytes[start + 6..start + 8].copy_from_slice(&label.timer.to_le_bytes());
            bytes[start + 8..start + 10].copy_from_slice(&label.x.to_le_bytes());
            bytes[start + 10..start + 12].copy_from_slice(&label.y.to_le_bytes());
        }
        for index in 0..3 {
            bytes[0x50c + index * 2..0x50e + index * 2]
                .copy_from_slice(&self.marker[index].to_le_bytes());
            bytes[0x51c + index * 4..0x520 + index * 4]
                .copy_from_slice(&self.camera[index].to_le_bytes());
            bytes[0x530 + index * 2..0x532 + index * 2]
                .copy_from_slice(&self.camera_angles[index].to_le_bytes());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BriefingScriptUpdate {
    pub state: BriefingScriptState,
    pub effects: Vec<Effect>,
    pub records_executed: usize,
}

/// Sparse object/label/camera resets in EB564/EB5AC/EB630, and the scalar
/// stores surrounding them in EEE70. Resource/text initialization is separate.
pub fn reset_briefing_script(mut state: BriefingScriptState) -> BriefingScriptUpdate {
    state.flags = 0;
    state.clock = 0;
    state.cursor = 0;
    state.label_count = 0;
    state.selected_label = 0;
    state.marker_id = -1;
    for object in &mut state.objects {
        object.set_flags(0);
        for offset in [8, 10] {
            object.set_half(offset, 0);
        }
        for offset in [6, 0x1e, 0x20] {
            object.set_half(offset, -1);
        }
    }
    state.labels = [BriefingLabel::default(); 8];
    state.camera_speed = 0;
    state.camera_duration = 0;
    state.camera_angles = [0; 3];
    BriefingScriptUpdate {
        state,
        effects: (1..=8)
            .map(|slot| Effect::SetVisualParameter { slot, value: 0 })
            .collect(),
        records_executed: 0,
    }
}

fn divide(value: i32, duration: i32) -> Result<i32, BriefingScriptError> {
    if duration == 0 {
        return Err(BriefingScriptError::DivisionByZero);
    }
    value
        .checked_div(duration)
        .ok_or(BriefingScriptError::DivisionOverflow)
}
fn object_index(slot: u8) -> Result<usize, BriefingScriptError> {
    if slot < 32 {
        Ok(usize::from(slot))
    } else {
        Err(BriefingScriptError::InvalidObject(slot))
    }
}
fn distance(math: &impl BriefingMath, y: i32, x: i32) -> Result<i32, BriefingScriptError> {
    math.evaluate(BriefingMathRequest::SquareRoot {
        value: y.wrapping_mul(y).wrapping_add(x.wrapping_mul(x)) as u32,
    })
}

/// Exactly one dispatch-table operation (table 0x800F6864, eight entries).
/// Inputs are owned copies, so all error paths leave the caller's state intact.
pub fn apply_briefing_command(
    mut state: BriefingScriptState,
    command: BriefingCommand,
    math: &impl BriefingMath,
) -> Result<BriefingScriptUpdate, BriefingScriptError> {
    let mut effects = vec![];
    let a = command.arguments;
    match command.opcode {
        0 | 4 => {} // 0x800EB0E8
        1 => {
            // 0x800EB0F0
            let object = &mut state.objects[object_index(command.slot)?];
            object.set_flags(object.flags() | 1);
            object.set_half(6, 1);
            object.set_half(8, command.time.wrapping_add(15));
            object.set_half(10, command.time.wrapping_add(15));
            object.set_half(4, a[0]);
            object.set_half(
                0xe,
                i32::from(a[1]).wrapping_sub(128).wrapping_mul(128) as i16,
            );
            object.set_half(0x10, (-i32::from(a[2]) / 10) as i16);
            object.set_half(
                0x12,
                i32::from(a[3]).wrapping_sub(128).wrapping_mul(128) as i16,
            );
            object.set_half(0x16, 0);
            object.set_half(0x1a, 0);
            object.set_half(0x18, (i32::from(a[4]) * 4096 / 360) as i16);
        }
        2 => {
            // 0x800EB1D8
            let object = &mut state.objects[object_index(command.slot)?];
            let duration = i32::from(a[0]);
            if duration == 0 {
                return Err(BriefingScriptError::DivisionByZero);
            }
            let target_y = (i32::from(a[1]).wrapping_sub(128).wrapping_mul(128) as i16) as i32;
            let target_x = (i32::from(a[3]).wrapping_sub(128).wrapping_mul(128) as i16) as i32;
            let y = target_y.wrapping_sub(i32::from(object.half(0xe)));
            let x = target_x.wrapping_sub(i32::from(object.half(0x12)));
            let heading = math.evaluate(BriefingMathRequest::Atan2 { y, x })?;
            // The distance deltas wrap through signed halfwords; atan2 above
            // receives the wider subtraction. See MIPS 0x800EB258 onward.
            let speed = divide(
                distance(math, i32::from(y as i16), i32::from(x as i16))?,
                duration,
            )?;
            object.set_half(6, 2);
            object.set_half(8, a[0]);
            object.set_half(10, a[0]);
            object.set_half(0x18, heading as i16);
            object.set_half(12, speed as i16);
        }
        3 => {
            // 0x800EB304
            let index = usize::try_from(state.label_count)
                .ok()
                .filter(|&i| i < 8)
                .ok_or(BriefingScriptError::InvalidLabelCount(state.label_count))?;
            object_index(command.slot)?;
            state.labels[index].flags |= 7;
            if state.flags & 2 == 0 {
                effects.push(Effect::PlaySound { id: 100 });
            }
            state.labels[index].style = a[0] as u8;
            state.labels[index].object = command.slot;
            effects.push(Effect::SetVisualStyle {
                slot: state.label_count.wrapping_add(1),
                style: u32::from(a[0] as u8),
            });
            state.label_count = state.label_count.wrapping_add(1);
            if index >= 1 {
                state.labels[index - 1].flags |= 8;
            }
            if index >= 2 {
                state.labels[index - 2].flags |= 16;
            }
        }
        5 => {
            // 0x800EB414
            let duration = i32::from(a[0]);
            if duration == 0 {
                return Err(BriefingScriptError::DivisionByZero);
            }
            let y = i32::from(a[1])
                .wrapping_sub(128)
                .wrapping_mul(128)
                .wrapping_sub(state.camera[0]);
            let x = i32::from(a[3])
                .wrapping_sub(128)
                .wrapping_mul(128)
                .wrapping_sub(state.camera[2]);
            let speed = divide(distance(math, y, x)?, duration)?;
            let heading = math.evaluate(BriefingMathRequest::Atan2 { y, x })?;
            state.camera_duration = duration;
            state.camera_speed = speed;
            state.camera_angles[1] = heading as i16;
        }
        6 => {
            // 0x800EB4F0
            state.camera = [
                i32::from(a[1]).wrapping_sub(128).wrapping_mul(128),
                0,
                i32::from(a[3]).wrapping_sub(128).wrapping_mul(128),
            ];
        }
        7 => {
            // 0x800EB524
            state.marker = [
                i32::from(a[1]).wrapping_sub(128).wrapping_mul(128) as i16,
                -300,
                i32::from(a[3]).wrapping_sub(128).wrapping_mul(128) as i16,
            ];
            state.marker_id = i32::from(a[0]);
        }
        opcode => return Err(BriefingScriptError::InvalidOpcode(opcode)),
    }
    Ok(BriefingScriptUpdate {
        state,
        effects,
        records_executed: 1,
    })
}

/// Initialization loop in 0x800EEE70: consume the contiguous time=-1 prefix.
pub fn dispatch_briefing_prefix(
    mut state: BriefingScriptState,
    script: &BriefingScript,
    math: &impl BriefingMath,
) -> Result<BriefingScriptUpdate, BriefingScriptError> {
    let mut effects = vec![];
    let mut records_executed = 0;
    while script.command(state.cursor)?.time == -1 {
        let update = apply_briefing_command(state, script.command(state.cursor)?, math)?;
        state = update.state;
        effects.extend(update.effects);
        records_executed += 1;
        state.cursor += 1;
    }
    Ok(BriefingScriptUpdate {
        state,
        effects,
        records_executed,
    })
}

/// Timed dispatch at the start of 0x800EE3B8. This does not advance the clock,
/// process controller input, or perform the repeated-update fast-forward loop.
pub fn dispatch_briefing_due(
    mut state: BriefingScriptState,
    script: &BriefingScript,
    math: &impl BriefingMath,
) -> Result<BriefingScriptUpdate, BriefingScriptError> {
    let mut effects = vec![];
    let mut records_executed = 0;
    if state.flags & 0x20 == 0 {
        while i32::from(script.command(state.cursor)?.time) == state.clock {
            let update = apply_briefing_command(state, script.command(state.cursor)?, math)?;
            state = update.state;
            effects.extend(update.effects);
            records_executed += 1;
            state.cursor += 1;
            if script.command(state.cursor)?.opcode == 0 {
                state.finish_timer = 0;
                state.flags |= 0x20;
            }
        }
    }
    Ok(BriefingScriptUpdate {
        state,
        effects,
        records_executed,
    })
}

/// Shared scalar movement in 0x800EB654 / 0x800EBC14. Appearance timers and
/// projected coordinates are deliberately not guessed here.
pub fn tick_briefing_movement(
    state: BriefingScriptState,
    math: &impl BriefingMath,
) -> Result<BriefingScriptState, BriefingScriptError> {
    tick_briefing_camera(tick_briefing_objects(state, math)?, math)
}
pub(crate) fn tick_briefing_objects(
    mut state: BriefingScriptState,
    math: &impl BriefingMath,
) -> Result<BriefingScriptState, BriefingScriptError> {
    for object in &mut state.objects {
        if object.half(6) == 2 && object.half(8) > 0 {
            let angle = i32::from(object.half(0x18));
            let speed = i32::from(object.half(12));
            let y = speed.wrapping_mul(math.evaluate(BriefingMathRequest::Sin { angle })?) / 4096;
            let x = speed.wrapping_mul(math.evaluate(BriefingMathRequest::Cos { angle })?) / 4096;
            object.set_half(0xe, object.half(0xe).wrapping_add(y as i16));
            object.set_half(0x12, object.half(0x12).wrapping_add(x as i16));
            object.set_half(8, object.half(8).wrapping_sub(1));
        }
    }
    Ok(state)
}
pub(crate) fn tick_briefing_camera(
    mut state: BriefingScriptState,
    math: &impl BriefingMath,
) -> Result<BriefingScriptState, BriefingScriptError> {
    if state.camera_duration > 0 {
        let angle = i32::from(state.camera_angles[1]);
        let y = state
            .camera_speed
            .wrapping_mul(math.evaluate(BriefingMathRequest::Sin { angle })?)
            / 4096;
        let x = state
            .camera_speed
            .wrapping_mul(math.evaluate(BriefingMathRequest::Cos { angle })?)
            / 4096;
        state.camera[0] = state.camera[0].wrapping_add(y);
        state.camera[2] = state.camera[2].wrapping_add(x);
        state.camera_duration = state.camera_duration.wrapping_sub(1);
    }
    Ok(state)
}

/// Explicit input/output boundary for 0x800ECE7C. A native renderer must supply
/// original-equivalent outcomes; this module never fabricates screen positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BriefingObjectDrawRequest {
    pub index: u8,
    pub object: BriefingObject,
    pub camera: [i32; 3],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BriefingObjectDrawOutcome {
    pub index: u8,
    pub flags: u32,
    pub appearance_remaining: i16,
    pub screen: [i16; 2],
}
pub fn briefing_draw_requests(state: &BriefingScriptState) -> Vec<BriefingObjectDrawRequest> {
    state
        .objects
        .iter()
        .enumerate()
        .filter(|(_, object)| object.flags() & 1 != 0)
        .map(|(index, &object)| BriefingObjectDrawRequest {
            index: index as u8,
            object,
            camera: state.camera,
        })
        .collect()
}
pub fn apply_briefing_draw_outcomes(
    mut state: BriefingScriptState,
    outcomes: &[BriefingObjectDrawOutcome],
) -> Result<BriefingScriptState, BriefingScriptError> {
    let mut seen = [false; 32];
    for outcome in outcomes {
        let index = object_index(outcome.index)?;
        if seen[index] {
            return Err(BriefingScriptError::DuplicateDrawOutcome(outcome.index));
        }
        seen[index] = true;
        let object = &mut state.objects[index];
        object.set_flags(outcome.flags);
        object.set_half(8, outcome.appearance_remaining);
        object.set_half(0x1e, outcome.screen[0]);
        object.set_half(0x20, outcome.screen[1]);
    }
    Ok(state)
}
