//! Resident state-2 loading control and the original mode-1 loading screen.
//! I/O progress/completion is supplied by the host; no CD timing is simulated.
use crate::mission_map::Random;
use ac3_graphics::{Error, Result};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlightLoadingState {
    pub frame: i16,
    pub progress: i16,
    pub mode: u8,
    pub visual_status: u8,
    pub stage: u8,
    pub reserved: u8,
    pub pending: i32,
    pub animation_flags: u32,
    pub horizontal_offset: i32,
    pub vertical_offset: i32,
    pub amount: i32,
    pub line_offset: i32,
    pub line_count: i32,
    pub line_index: i32,
    pub color: [u8; 4],
    pub secondary_color: [u8; 4],
    pub angles: [i16; 2],
    pub scale: i32,
    pub ticks: i32,
}
impl FlightLoadingState {
    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        if b.len() != 60 {
            return Err(Error::Invalid("flight loading state needs 60 bytes"));
        }
        let i16_at = |o| i16::from_le_bytes(b[o..o + 2].try_into().unwrap());
        let i32_at = |o| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        Ok(Self {
            frame: i16_at(0),
            progress: i16_at(2),
            mode: b[4],
            visual_status: b[5],
            stage: b[6],
            reserved: b[7],
            pending: i32_at(8),
            animation_flags: i32_at(12) as u32,
            horizontal_offset: i32_at(16),
            vertical_offset: i32_at(20),
            amount: i32_at(24),
            line_offset: i32_at(28),
            line_count: i32_at(32),
            line_index: i32_at(36),
            color: b[40..44].try_into().unwrap(),
            secondary_color: b[44..48].try_into().unwrap(),
            angles: [i16_at(48), i16_at(50)],
            scale: i32_at(52),
            ticks: i32_at(56),
        })
    }
    pub fn to_bytes(&self) -> [u8; 60] {
        let mut b = [0; 60];
        b[..2].copy_from_slice(&self.frame.to_le_bytes());
        b[2..4].copy_from_slice(&self.progress.to_le_bytes());
        b[4..8].copy_from_slice(&[self.mode, self.visual_status, self.stage, self.reserved]);
        for (offset, value) in [
            (8, self.pending),
            (12, self.animation_flags as i32),
            (16, self.horizontal_offset),
            (20, self.vertical_offset),
            (24, self.amount),
            (28, self.line_offset),
            (32, self.line_count),
            (36, self.line_index),
            (52, self.scale),
            (56, self.ticks),
        ] {
            b[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        b[40..44].copy_from_slice(&self.color);
        b[44..48].copy_from_slice(&self.secondary_color);
        for i in 0..2 {
            b[48 + i * 2..50 + i * 2].copy_from_slice(&self.angles[i].to_le_bytes());
        }
        b
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlightLoadingContext {
    pub mission: i16,
    pub aircraft: i16,
    pub language: u32,
    pub difficulty: u32,
    pub frame_clock: u32,
    pub replay: u8,
    pub replay_index: u8,
    pub restart_count: u16,
    pub exit_reason: u8,
    pub status: u16,
}
impl FlightLoadingContext {
    pub const BYTE_LEN: usize = 0x95e;
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < Self::BYTE_LEN {
            return Err(Error::Truncated("flight loading context"));
        }
        let word = |o| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
        let half = |o| u16::from_le_bytes(bytes[o..o + 2].try_into().unwrap());
        Ok(Self {
            frame_clock: word(0x20),
            language: word(0x38c),
            mission: half(0x390) as i16,
            aircraft: half(0x392) as i16,
            difficulty: word(0x398),
            replay: bytes[0x3a9],
            replay_index: bytes[0x3aa],
            restart_count: half(0x3b0),
            exit_reason: bytes[0x95b],
            status: half(0x95c),
        })
    }
    /// Writes only modeled fields and preserves the remainder of the shared context.
    pub fn write_into(&self, bytes: &mut [u8]) -> Result<()> {
        if bytes.len() < Self::BYTE_LEN {
            return Err(Error::Truncated("flight loading context"));
        }
        for (offset, value) in [
            (0x20, self.frame_clock),
            (0x38c, self.language),
            (0x398, self.difficulty),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (offset, value) in [
            (0x390, self.mission as u16),
            (0x392, self.aircraft as u16),
            (0x3b0, self.restart_count),
            (0x95c, self.status),
        ] {
            bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        bytes[0x3a9] = self.replay;
        bytes[0x3aa] = self.replay_index;
        bytes[0x95b] = self.exit_reason;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoadingRenderStatus {
    pub flags: u32,
    pub phase: u32,
    pub background: [u8; 4],
    pub parameter: u32,
}
impl LoadingRenderStatus {
    /// State-2 mode1 sets this through 36668; packed RGB is converted by the GPU.
    pub fn clear_rgb(&self) -> [u8; 3] {
        [self.background[0], self.background[1], self.background[2]]
    }
}

/// The unused alternate backdrop is initialized after map objects by EAF0C.
/// Its three trails consume fifteen real RNG calls even when never drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InactiveTrail {
    pub positions: [[i32; 3]; 15],
    pub angles: [[i16; 3]; 15],
}
pub fn initialize_inactive_trails(random: &mut Random) -> [InactiveTrail; 3] {
    std::array::from_fn(|_| {
        let distance = random.next_value() & 0x3fff;
        let side = random.next_value() & 3;
        let spread = random.next_value();
        let first = random.next_value();
        let second = random.next_value();
        let width = (distance * 160 / 250) as i16 as i32;
        let height = (distance * 240 / 250) as i16 as i32;
        let (position, angles) = match side {
            0 => (
                [width, ((spread % 512 - 256) * distance) / 250, distance],
                [(first & 0x1ff) as i16, (second & 0xdff) as i16, 0],
            ),
            1 => (
                [-width, ((spread % 512 - 256) * distance) / 250, distance],
                [(first & 0x1ff) as i16, (second & 0x5ff) as i16, 0],
            ),
            2 => (
                [((spread % 320 - 160) * distance) / 250, height, distance],
                [(first & 0xdff) as i16, (second & 0xfff) as i16, 0],
            ),
            _ => (
                [((spread % 320 - 160) * distance) / 250, -height, distance],
                [(first & 0x5ff) as i16, (second & 0xfff) as i16, 0],
            ),
        };
        InactiveTrail {
            positions: [position; 15],
            angles: [angles; 15],
        }
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlightLoadObservation {
    /// Original 2C6A8 result, -1 when no resource is active.
    pub active_resource: i32,
    /// Original 2C604 fixed-point progress, normally 0..4096.
    pub progress: i16,
    /// Deliver the original 354AC completion before this poll.
    pub completed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlightLoadingEffect {
    ResetResourceQueue,
    ResetSound,
    QueueResource { resource: i16 },
}

/// Each group is one addPrim chain. Groups prepend; packets inside a chain do not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlightLoadingCommands {
    pub groups: Vec<Vec<Vec<u32>>>,
}
impl FlightLoadingCommands {
    fn packet(&mut self, words: Vec<u32>) {
        self.groups.push(vec![words]);
    }
    pub fn execution_order(&self) -> Vec<Vec<u32>> {
        self.groups.iter().rev().flatten().cloned().collect()
    }
    pub fn emission_order(&self) -> Vec<Vec<u32>> {
        self.groups.iter().flatten().cloned().collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlightLoadingUpdate {
    pub callback_result: u32,
    pub effects: Vec<FlightLoadingEffect>,
    pub commands: FlightLoadingCommands,
}

pub struct FlightLoader<'a> {
    resident: &'a [u8],
}
impl<'a> FlightLoader<'a> {
    pub fn new(resident: &'a [u8]) -> Result<Self> {
        let this = Self { resident };
        this.bytes(0x800bab40, 4)?;
        Ok(this)
    }
    fn bytes(&self, address: u32, n: usize) -> Result<&[u8]> {
        address
            .checked_sub(0x80010000)
            .and_then(|a| self.resident.get(a as usize..(a as usize).checked_add(n)?))
            .ok_or(Error::Invalid("resident loading table outside payload"))
    }
    fn word(&self, address: u32) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.bytes(address, 4)?.try_into().unwrap(),
        ))
    }
    fn half(&self, address: u32) -> Result<i16> {
        Ok(i16::from_le_bytes(
            self.bytes(address, 2)?.try_into().unwrap(),
        ))
    }
    fn string(&self, address: u32) -> Result<&[u8]> {
        let start = address
            .checked_sub(0x80010000)
            .ok_or(Error::Invalid("resident text pointer"))? as usize;
        let tail = self
            .resident
            .get(start..)
            .ok_or(Error::Invalid("resident text pointer"))?;
        let len = tail
            .iter()
            .position(|x| *x == 0)
            .ok_or(Error::Invalid("unterminated resident text"))?;
        Ok(&tail[..len])
    }
    fn resource(&self, table: u32, index: i16, count: i16) -> Result<i16> {
        if !(0..count).contains(&index) {
            return Err(Error::Invalid("flight resource index"));
        }
        self.half(table + index as u32 * 2)
    }
    pub fn mission_resource(&self, mission: i16) -> Result<i16> {
        self.resource(0x800af748, mission, 42)
    }
    pub fn aircraft_resource(&self, aircraft: i16) -> Result<i16> {
        self.resource(0x800af79c, aircraft, 23)
    }
    fn localized(&self, language: u32, index: u32) -> Result<&[u8]> {
        if language >= 5 || index >= 26 {
            return Err(Error::Invalid("resident localized text index"));
        }
        self.string(self.word(self.word(0x800af6dc + index * 4)? + language * 4)?)
    }
    pub fn initialize(
        &self,
        state: &mut FlightLoadingState,
        context: &mut FlightLoadingContext,
        render: &mut LoadingRenderStatus,
        observation: FlightLoadObservation,
        random: &mut Random,
    ) -> Result<FlightLoadingUpdate> {
        if context.replay != 0 {
            if context.replay_index > 15 {
                context.replay_index = 1;
            }
            let record = self.bytes(0x800af7ca + u32::from(context.replay_index) * 4, 2)?;
            context.mission = i16::from(record[0]);
            context.aircraft = i16::from(record[1]);
        }
        let resource = self.mission_resource(context.mission)?;
        let mode = if context.replay != 0 {
            1
        } else {
            self.bytes(0x800af808 + context.mission as u32, 1)?[0]
        };
        if mode != 1 {
            return Err(Error::Unsupported(
                "flight loading visual mode other than 1",
            ));
        }
        context.exit_reason = 0;
        context.status = 0;
        context.restart_count = 0;
        state.pending = 1;
        state.frame = 0;
        state.mode = mode;
        state.stage = 0;
        state.progress = 0;
        render.phase = 0;
        render.parameter = 0;
        render.flags |= 1;
        let mut out = self.poll(state, context, render, observation, random)?;
        out.callback_result = 0x04000000;
        out.effects = vec![
            FlightLoadingEffect::ResetResourceQueue,
            FlightLoadingEffect::ResetSound,
            FlightLoadingEffect::QueueResource { resource },
        ];
        Ok(out)
    }
    pub fn queue_aircraft(
        &self,
        state: &mut FlightLoadingState,
        context: &FlightLoadingContext,
        render: &mut LoadingRenderStatus,
        observation: FlightLoadObservation,
        random: &mut Random,
    ) -> Result<FlightLoadingUpdate> {
        let resource = self.aircraft_resource(context.aircraft)?;
        state.pending = 1;
        let mut out = self.poll(state, context, render, observation, random)?;
        out.callback_result = 0x04000000;
        out.effects
            .push(FlightLoadingEffect::QueueResource { resource });
        Ok(out)
    }
    pub fn poll(
        &self,
        state: &mut FlightLoadingState,
        context: &FlightLoadingContext,
        render: &mut LoadingRenderStatus,
        observation: FlightLoadObservation,
        random: &mut Random,
    ) -> Result<FlightLoadingUpdate> {
        if state.mode != 1 {
            return Err(Error::Unsupported(
                "flight loading visual mode other than 1",
            ));
        }
        if observation.completed {
            state.pending = 0;
        }
        let stage =
            if observation.active_resource == i32::from(self.mission_resource(context.mission)?) {
                1
            } else if observation.active_resource
                == i32::from(self.aircraft_resource(context.aircraft)?)
            {
                2
            } else {
                0
            };
        if stage != 0 {
            state.stage = stage;
            state.progress = observation.progress;
        }
        if state.frame < 1 {
            Self::visual_initialize(state, render)?;
        }
        let mut commands = FlightLoadingCommands::default();
        if state.frame == 1 || state.frame == 2 {
            commands.groups.push(Self::fade(0));
        }
        if state.frame > 2 {
            commands = self.mode1_draw(state, context, random)?;
        }
        state.frame = state.frame.wrapping_add(1);
        Ok(FlightLoadingUpdate {
            callback_result: if state.pending == 0 { 0x04000000 } else { 0 },
            commands,
            effects: vec![],
        })
    }
    pub fn visual_initialize(
        state: &mut FlightLoadingState,
        render: &mut LoadingRenderStatus,
    ) -> Result<()> {
        if state.mode != 1 {
            return Err(Error::Unsupported(
                "flight loading visual mode other than 1",
            ));
        }
        state.visual_status = 0;
        render.flags |= 8;
        render.background[..3].copy_from_slice(&[0, 12, 0]);
        state.horizontal_offset = 0xf8000000u32 as i32;
        state.vertical_offset = 0xa00000;
        state.line_offset = 199;
        state.line_count = 4;
        state.color[0] = 0;
        state.color[1] = 255;
        state.color[2] = 0;
        state.secondary_color[..3].copy_from_slice(&[0, 131, 0]);
        state.ticks = 0;
        state.animation_flags = 0;
        state.amount = 0;
        state.line_index = 0;
        state.scale = 0x2c3b2;
        state.angles = [0; 2];
        Ok(())
    }
    /// 35F5C: the four packets form a forward chain prepended as one group.
    pub fn fade(amount: i32) -> Vec<Vec<u32>> {
        let magnitude = amount.wrapping_abs() as u32;
        vec![
            vec![0xe6000001, 0],
            vec![if amount < 0 { 0xe1000240 } else { 0xe1000220 }],
            vec![
                0x62000000 | magnitude | (magnitude << 8) | (magnitude << 16),
                0,
                0x01000140,
            ],
            vec![0xe6000000, 0],
        ]
    }
    pub fn mode1_draw(
        &self,
        state: &mut FlightLoadingState,
        context: &FlightLoadingContext,
        random: &mut Random,
    ) -> Result<FlightLoadingCommands> {
        if state.mode != 1 {
            return Err(Error::Unsupported(
                "flight loading visual mode other than 1",
            ));
        }
        let progress = match state.stage {
            1 => i32::from(state.progress),
            2 => 4096,
            _ => 0,
        }
        .min(4096);
        let mut out = self.text(state, context)?;
        let a = 0x80023948 + state.ticks.clamp(0, 16) as u32 * 4;
        let width = (i32::from(self.half(a)?) * 320 / 4096) as i16;
        let height = (i32::from(self.half(a + 2)?) / 16) as i16;
        let rect = [
            ((320 - i32::from(width)) >> 1) as i16,
            ((256 - i32::from(height)) >> 1) as i16,
            width,
            height,
        ];
        if let Some(chain) = Self::aperture(rect, (4096 - progress) * 2, random) {
            out.groups.push(chain);
        }
        state.ticks = state.ticks.wrapping_add(1);
        Ok(out)
    }
    pub fn text(
        &self,
        state: &FlightLoadingState,
        context: &FlightLoadingContext,
    ) -> Result<FlightLoadingCommands> {
        let mut out = FlightLoadingCommands::default();
        if state.frame < 40 && context.replay == 0 {
            self.difficulty(context, 170, 18, &mut out)?;
        }
        if (state.stage == 1 && state.progress > 0) || state.stage == 2 {
            self.mission_resource(context.mission)?;
            let text = self.string(self.word(0x800af844 + context.mission as u32 * 4)?)?;
            let x = ((320 - text.len() as i32 * 8) >> 1) as i16;
            let progress = if state.stage == 1 {
                i32::from(state.progress)
            } else {
                4096 - i32::from(state.progress)
            };
            let color = ((progress / 32) as u32 & 255) << 8;
            Self::fixed_font(text, [x, 118], color, &mut out);
            if context.frame_clock & 0x10 != 0 {
                Self::fixed_font(
                    self.string(0x800bab7c)?,
                    [x.wrapping_sub(12), 118],
                    color,
                    &mut out,
                );
            }
        }
        Ok(out)
    }
    pub fn difficulty(
        &self,
        context: &FlightLoadingContext,
        x: i16,
        y: i16,
        out: &mut FlightLoadingCommands,
    ) -> Result<()> {
        out.packet(vec![0xe1000035]);
        let label = self.localized(context.language, 13)?;
        self.variable_font(label, [x, y], 0x8000, out)?;
        let width = self.text_width(label)?;
        let index = u32::from(self.bytes(0x800bab40 + (context.difficulty & 3), 1)?[0]);
        self.variable_font(
            self.localized(context.language, index)?,
            [x.wrapping_add(width as i16), y],
            0x8000,
            out,
        )?;
        out.packet(vec![0xe1000015]);
        Ok(())
    }
    pub fn text_width(&self, text: &[u8]) -> Result<i32> {
        let mut width = 0i32;
        for &byte in text {
            if byte < 32 {
                return Err(Error::Invalid("resident text control character"));
            }
            width = width.wrapping_add(if byte == 32 {
                6
            } else {
                i32::from(self.bytes(0x800aee8c + u32::from(byte - 32) * 8, 1)?[0])
            });
        }
        Ok(width)
    }
    pub fn variable_font(
        &self,
        text: &[u8],
        position: [i16; 2],
        color: u32,
        out: &mut FlightLoadingCommands,
    ) -> Result<()> {
        let [mut x, y] = position;
        for &byte in text {
            if byte < 32 {
                return Err(Error::Invalid("resident text control character"));
            }
            if byte == 32 {
                x = x.wrapping_add(6);
                continue;
            }
            let b = self.bytes(0x800aee88 + u32::from(byte - 32) * 8, 8)?;
            let clut = u16::from_le_bytes([b[0], b[1]]);
            let uv = u16::from_le_bytes([b[2], b[3]]);
            let offset = i16::from_le_bytes([b[6], b[7]]);
            out.packet(vec![
                0x66000000 | color,
                xy(x, y.wrapping_add(offset)),
                u32::from(uv) | (u32::from(clut) << 16),
                u32::from(b[4]) | (u32::from(b[5]) << 16),
            ]);
            x = x.wrapping_add(i16::from(b[4]));
        }
        Ok(())
    }
    pub fn fixed_font(
        text: &[u8],
        position: [i16; 2],
        color: u32,
        out: &mut FlightLoadingCommands,
    ) {
        let [mut x, y] = position;
        for &byte in text {
            let index = u32::from(byte).wrapping_sub(32);
            if index != 0 {
                let uv = ((index & 15) * 8 + 128)
                    | (((index as i32 >> 4) as u32) * 0x800 + 0x4700)
                    | 0x7f980000;
                out.packet(vec![0x76000000 | color, xy(x, y), uv]);
                if index == 0x2d || index == 0x37 {
                    x = x.wrapping_add(1);
                }
                if index == 0x3b || index == 0x3d {
                    x = x.wrapping_sub(4);
                }
                if byte == 0x3a || byte == 0x3b {
                    x = x.wrapping_sub(5);
                }
            }
            x = x.wrapping_add(8);
        }
        out.packet(vec![0xe1000036]);
    }
    pub fn aperture(rect: [i16; 4], amount: i32, random: &mut Random) -> Option<Vec<Vec<u32>>> {
        if rect[2] <= 0 || rect[3] <= 0 {
            return None;
        }
        let mut chain = vec![vec![0xe1000218, 0xe20c4318, 0]];
        chain.extend(Self::noise_tiles(
            rect,
            0x46d6,
            0x64838383,
            (4096i32.wrapping_sub(amount)) as i16,
            random,
        ));
        chain.push(vec![0xe1000220, 0xe2000000, 0]);
        Some(chain)
    }
    pub fn noise_tiles(
        rect: [i16; 4],
        clut: u16,
        rgb: u32,
        ir0: i16,
        random: &mut Random,
    ) -> Vec<Vec<u32>> {
        let mut color = rgb & 0xff000000;
        for channel in 0..3 {
            let base = ((rgb >> (channel * 8)) & 255) as i64 * 16;
            let delta = (48 - base).clamp(-32768, 32767);
            let mac = base + ((i64::from(ir0) * delta) >> 12);
            color |= ((mac >> 4).clamp(0, 255) as u32) << (channel * 8);
        }
        let mut out = vec![];
        let mut remaining_y = i32::from(rect[3]);
        let mut y = rect[1];
        while remaining_y > 0 {
            let mut remaining_x = i32::from(rect[2]);
            let mut x = rect[0];
            while remaining_x > 0 {
                let uv =
                    (random.next_value() as u16) & if clut == 0x4696 { 0x7c7e } else { 0x7e7f };
                out.push(vec![
                    color,
                    xy(x, y),
                    u32::from(uv) | (u32::from(clut) << 16),
                    remaining_x.min(32) as u32 | ((remaining_y.min(48) as u32) << 16),
                ]);
                remaining_x -= 32;
                x = x.wrapping_add(32);
            }
            remaining_y -= 48;
            y = y.wrapping_add(48);
        }
        out
    }
}
fn xy(x: i16, y: i16) -> u32 {
    u32::from(x as u16) | (u32::from(y as u16) << 16)
}
