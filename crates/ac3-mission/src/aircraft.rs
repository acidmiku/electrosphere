//! Overlay-2 aircraft and weapon selection. Original record bytes are retained;
//! no rendering, texture decompression or streaming is performed by this module.
use crate::{
    AircraftState, Effect, Error, read_u16, read_u32, require_bytes, select_aircraft_callback,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AircraftRecord(pub [u8; 48]);
impl Default for AircraftRecord {
    fn default() -> Self {
        Self([0; 48])
    }
}
impl AircraftRecord {
    pub fn id(self) -> i16 {
        read_u16(&self.0, 0) as i16
    }
    /// 0x800D804C: five signed halfword statistics, low 16 bits retained.
    pub fn scaled(mut self) -> Self {
        for offset in (2..12).step_by(2) {
            let value = read_u16(&self.0, offset).wrapping_shl(4);
            self.0[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WeaponRecord(pub [u8; 8]);
impl WeaponRecord {
    pub fn id(self) -> i32 {
        read_u32(&self.0, 0) as i32
    }
    /// 0x800D808C: byte truncation is observable for values >= 16.
    pub fn scaled(mut self) -> Self {
        for byte in &mut self.0[4..7] {
            *byte = byte.wrapping_shl(4);
        }
        self
    }
}

/// The complete 0x138-byte block at `[0x800F5FD4]`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AircraftSelectionState {
    pub phase: i32,
    pub substate: i32,
    pub flags: u32,
    pub timer: i32,
    pub panel_offsets: [i16; 4],
    pub flash_timer: i32,
    pub selection: i32,
    pub previous: AircraftRecord,
    pub current: AircraftRecord,
    pub next: AircraftRecord,
    pub interpolation_timer: i32,
    pub load_rectangle: [i16; 4],
    pub availability: [u8; 24],
    pub load_timer: i32,
    pub weapon_row: i32,
    pub weapon_availability: [[u8; 8]; 2],
    pub weapon_choice: [i16; 2],
    pub weapon_previous: [WeaponRecord; 2],
    pub weapon_current: [WeaponRecord; 2],
    pub weapon_next: [WeaponRecord; 2],
    pub weapon_interpolation_timer: [i32; 2],
    pub weapon_flags: [u32; 2],
    pub weapon_slide_timer: [i32; 2],
}
impl AircraftSelectionState {
    pub const BYTE_LEN: usize = 0x138;
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        let mut state = Self {
            phase: read_u32(bytes, 0) as i32,
            substate: read_u32(bytes, 4) as i32,
            flags: read_u32(bytes, 8),
            timer: read_u32(bytes, 12) as i32,
            flash_timer: read_u32(bytes, 0x18) as i32,
            selection: read_u32(bytes, 0x1c) as i32,
            previous: AircraftRecord(bytes[0x20..0x50].try_into().unwrap()),
            current: AircraftRecord(bytes[0x50..0x80].try_into().unwrap()),
            next: AircraftRecord(bytes[0x80..0xb0].try_into().unwrap()),
            interpolation_timer: read_u32(bytes, 0xb0) as i32,
            availability: bytes[0xbc..0xd4].try_into().unwrap(),
            load_timer: read_u32(bytes, 0xd4) as i32,
            weapon_row: read_u32(bytes, 0xd8) as i32,
            ..Self::default()
        };
        for i in 0..4 {
            state.panel_offsets[i] = read_u16(bytes, 0x10 + i * 2) as i16;
            state.load_rectangle[i] = read_u16(bytes, 0xb4 + i * 2) as i16;
        }
        for i in 0..2 {
            state.weapon_availability[i].copy_from_slice(&bytes[0xdc + i * 8..0xe4 + i * 8]);
            state.weapon_choice[i] = read_u16(bytes, 0xec + i * 2) as i16;
            state.weapon_previous[i]
                .0
                .copy_from_slice(&bytes[0xf0 + i * 8..0xf8 + i * 8]);
            state.weapon_current[i]
                .0
                .copy_from_slice(&bytes[0x100 + i * 8..0x108 + i * 8]);
            state.weapon_next[i]
                .0
                .copy_from_slice(&bytes[0x110 + i * 8..0x118 + i * 8]);
            state.weapon_interpolation_timer[i] = read_u32(bytes, 0x120 + i * 4) as i32;
            state.weapon_flags[i] = read_u32(bytes, 0x128 + i * 4);
            state.weapon_slide_timer[i] = read_u32(bytes, 0x130 + i * 4) as i32;
        }
        Ok(state)
    }
    pub fn to_bytes(self) -> [u8; Self::BYTE_LEN] {
        let mut bytes = [0; Self::BYTE_LEN];
        for (offset, value) in [
            (0, self.phase as u32),
            (4, self.substate as u32),
            (8, self.flags),
            (12, self.timer as u32),
            (0x18, self.flash_timer as u32),
            (0x1c, self.selection as u32),
            (0xb0, self.interpolation_timer as u32),
            (0xd4, self.load_timer as u32),
            (0xd8, self.weapon_row as u32),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        for i in 0..4 {
            bytes[0x10 + i * 2..0x12 + i * 2].copy_from_slice(&self.panel_offsets[i].to_le_bytes());
            bytes[0xb4 + i * 2..0xb6 + i * 2]
                .copy_from_slice(&self.load_rectangle[i].to_le_bytes());
        }
        bytes[0x20..0x50].copy_from_slice(&self.previous.0);
        bytes[0x50..0x80].copy_from_slice(&self.current.0);
        bytes[0x80..0xb0].copy_from_slice(&self.next.0);
        bytes[0xbc..0xd4].copy_from_slice(&self.availability);
        for i in 0..2 {
            bytes[0xdc + i * 8..0xe4 + i * 8].copy_from_slice(&self.weapon_availability[i]);
            bytes[0xec + i * 2..0xee + i * 2].copy_from_slice(&self.weapon_choice[i].to_le_bytes());
            bytes[0xf0 + i * 8..0xf8 + i * 8].copy_from_slice(&self.weapon_previous[i].0);
            bytes[0x100 + i * 8..0x108 + i * 8].copy_from_slice(&self.weapon_current[i].0);
            bytes[0x110 + i * 8..0x118 + i * 8].copy_from_slice(&self.weapon_next[i].0);
            bytes[0x120 + i * 4..0x124 + i * 4]
                .copy_from_slice(&self.weapon_interpolation_timer[i].to_le_bytes());
            bytes[0x128 + i * 4..0x12c + i * 4]
                .copy_from_slice(&self.weapon_flags[i].to_le_bytes());
            bytes[0x130 + i * 4..0x134 + i * 4]
                .copy_from_slice(&self.weapon_slide_timer[i].to_le_bytes());
        }
        bytes
    }
}

/// Sparse resident fields used by the aircraft callbacks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AircraftContext {
    pub mission_id: i16,
    pub aircraft_id: i16,
    pub weapon_codes: [u8; 3],
    pub options: u32,
    pub saved_availability: [u8; 24],
    pub availability: [u8; 24],
    pub special_unlocks: [u8; 2],
}
impl AircraftContext {
    pub const BYTE_LEN: usize = 0xa05;
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        Ok(Self {
            mission_id: read_u16(bytes, 0x390) as i16,
            aircraft_id: read_u16(bytes, 0x392) as i16,
            weapon_codes: bytes[0x3ad..0x3b0].try_into().unwrap(),
            options: read_u32(bytes, 0x970),
            saved_availability: bytes[0x978..0x990].try_into().unwrap(),
            availability: bytes[0x9b8..0x9d0].try_into().unwrap(),
            special_unlocks: bytes[0xa03..0xa05].try_into().unwrap(),
        })
    }
    pub fn write_into(self, bytes: &mut [u8]) -> Result<(), Error> {
        require_bytes(bytes, Self::BYTE_LEN)?;
        bytes[0x390..0x392].copy_from_slice(&self.mission_id.to_le_bytes());
        bytes[0x392..0x394].copy_from_slice(&self.aircraft_id.to_le_bytes());
        bytes[0x3ad..0x3b0].copy_from_slice(&self.weapon_codes);
        bytes[0x970..0x974].copy_from_slice(&self.options.to_le_bytes());
        bytes[0x978..0x990].copy_from_slice(&self.saved_availability);
        bytes[0x9b8..0x9d0].copy_from_slice(&self.availability);
        bytes[0xa03..0xa05].copy_from_slice(&self.special_unlocks);
        Ok(())
    }
}

/// Data remains sourced from the user's overlay, rather than re-created constants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AircraftTables {
    pub aircraft: [AircraftRecord; 21],
    pub loadouts: [[u8; 16]; 21],
    pub primary_weapons: [WeaponRecord; 7],
    pub secondary_weapons: [WeaponRecord; 8],
    pub missions: [[u8; 12]; 42],
}
impl AircraftTables {
    pub fn from_overlay(bytes: &[u8]) -> Result<Self, Error> {
        const BASE: usize = 0x800c9dc0;
        require_bytes(bytes, 0x800f5958 + 42 * 12 - BASE)?;
        Ok(Self {
            aircraft: std::array::from_fn(|i| {
                AircraftRecord(
                    bytes[0x800f53a0 - BASE + i * 48..0x800f53a0 - BASE + (i + 1) * 48]
                        .try_into()
                        .unwrap(),
                )
            }),
            loadouts: std::array::from_fn(|i| {
                bytes[0x800f5790 - BASE + i * 16..0x800f5790 - BASE + (i + 1) * 16]
                    .try_into()
                    .unwrap()
            }),
            primary_weapons: std::array::from_fn(|i| {
                WeaponRecord(
                    bytes[0x800f58e0 - BASE + i * 8..0x800f58e0 - BASE + (i + 1) * 8]
                        .try_into()
                        .unwrap(),
                )
            }),
            secondary_weapons: std::array::from_fn(|i| {
                WeaponRecord(
                    bytes[0x800f5918 - BASE + i * 8..0x800f5918 - BASE + (i + 1) * 8]
                        .try_into()
                        .unwrap(),
                )
            }),
            missions: std::array::from_fn(|i| {
                bytes[0x800f5958 - BASE + i * 12..0x800f5958 - BASE + (i + 1) * 12]
                    .try_into()
                    .unwrap()
            }),
        })
    }
    /// 0x800D17D0 scans 41 records and reads record 41 on no match.
    pub fn mission_category(&self, id: i16) -> u8 {
        self.missions[..41]
            .iter()
            .find(|record| i32::from(record[0]) == i32::from(id))
            .unwrap_or(&self.missions[41])[2]
    }
    fn weapon(&self, row: usize, id: i32) -> Result<WeaponRecord, Error> {
        let table: &[WeaponRecord] = if row == 0 {
            &self.primary_weapons
        } else {
            &self.secondary_weapons
        };
        table
            .get(usize::try_from(id).unwrap_or(usize::MAX))
            .copied()
            .ok_or(Error::InvalidWeaponId { row, id })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AircraftUpdate {
    pub state: AircraftSelectionState,
    /// State at 0x800D8CB4: after aircraft interpolation, before resource-slide
    /// and weapon interpolation. Use this at the matching draw effect only.
    pub draw_state: Option<AircraftSelectionState>,
    pub context: AircraftContext,
    pub callback_result: u32,
    pub effects: Vec<Effect>,
}
fn offset(effects: &mut Vec<Effect>, slot: i32, x: i32, y: i32) {
    effects.push(Effect::SetVisualOffset { slot, x, y });
}
fn style(effects: &mut Vec<Effect>, slot: i32, value: u32) {
    effects.push(Effect::SetVisualStyle { slot, style: value });
}
fn sound(effects: &mut Vec<Effect>, id: u32) {
    effects.push(Effect::PlaySound { id });
}
fn aircraft_id(id: i32) -> Result<usize, Error> {
    usize::try_from(id)
        .ok()
        .filter(|&id| id < 24)
        .ok_or(Error::InvalidAircraftId(id))
}
fn weapon_id(row: usize, id: i32) -> Result<usize, Error> {
    usize::try_from(id)
        .ok()
        .filter(|&id| id < 8)
        .ok_or(Error::InvalidWeaponId { row, id })
}

/// 0x800DA5F4 initializes only the fields actually stored by the original.
pub fn initialize_aircraft(
    mut state: AircraftSelectionState,
    context: AircraftContext,
) -> AircraftUpdate {
    state.phase = 0;
    state.substate = 0;
    state.flags = 0x40000;
    state.load_timer = 0;
    state.weapon_flags = [0; 2];
    state.weapon_slide_timer = [0; 2];
    AircraftUpdate {
        state,
        draw_state: None,
        context,
        callback_result: 1,
        effects: vec![
            Effect::StartStream { resource: 0x81d },
            Effect::ClearTextVisibility,
            Effect::PendingOriginalCallWithArgs {
                address: 0x800e7a18,
                arguments: vec![30, 10],
            },
        ],
    }
}

/// Original whole wrapper 0x800DA65C, with the drawing call represented as an
/// effect. All writes to the modeled 0x138-byte selection block are translated.
pub fn tick_aircraft(
    mut state: AircraftSelectionState,
    mut context: AircraftContext,
    tables: &AircraftTables,
    pressed: u32,
) -> Result<AircraftUpdate, Error> {
    let callback = select_aircraft_callback(AircraftState {
        phase: state.phase,
        substate: state.substate,
    })?;
    let mut effects = vec![];
    let result = aircraft_leaf(
        &mut state,
        &mut context,
        tables,
        pressed,
        callback,
        &mut effects,
    )?;
    let callback_result;
    if result & 1 != 0 {
        state.substate = state.substate.wrapping_add(1);
        callback_result = 0;
    } else if result & 2 != 0 {
        state.phase = state.phase.wrapping_add(1);
        state.substate = 0;
        callback_result = 0;
    } else if result & 8 != 0 {
        state.phase = state.phase.wrapping_sub(1);
        state.substate = 0;
        callback_result = 0;
    } else if result & 4 != 0 {
        effects.push(Effect::StopStream);
        callback_result = 0x100;
    } else if result & 16 != 0 {
        effects.push(Effect::StopStream);
        callback_result = 0x200;
    } else {
        callback_result = 0;
    }
    let mut draw_state = None;
    if callback_result == 0 {
        interpolate_aircraft(&mut state);
        draw_state = Some(state);
        effects.push(Effect::PendingOriginalCall {
            address: 0x800d8cb4,
        });
        update_aircraft_resource_slide(&mut state, &mut effects);
        interpolate_weapons(&mut state);
        update_weapon_slide(&mut state, &mut effects);
    }
    Ok(AircraftUpdate {
        state,
        draw_state,
        context,
        callback_result,
        effects,
    })
}

/// A leaf-only API for independent original-function vectors.
pub fn tick_aircraft_leaf(
    mut state: AircraftSelectionState,
    mut context: AircraftContext,
    tables: &AircraftTables,
    pressed: u32,
) -> Result<AircraftUpdate, Error> {
    let callback = select_aircraft_callback(AircraftState {
        phase: state.phase,
        substate: state.substate,
    })?;
    let mut effects = vec![];
    let callback_result = aircraft_leaf(
        &mut state,
        &mut context,
        tables,
        pressed,
        callback,
        &mut effects,
    )?;
    Ok(AircraftUpdate {
        state,
        draw_state: None,
        context,
        callback_result,
        effects,
    })
}

fn aircraft_leaf(
    state: &mut AircraftSelectionState,
    context: &mut AircraftContext,
    tables: &AircraftTables,
    pressed: u32,
    callback: u32,
    effects: &mut Vec<Effect>,
) -> Result<u32, Error> {
    match callback {
        0x800d9038 => initialize_aircraft_phase(state, context, tables, effects),
        0x800d92b8 => Ok(aircraft_intro(state, effects)),
        0x800d9438 => aircraft_input(state, tables, pressed, effects),
        0x800d9770 => {
            let timer = state.timer;
            aircraft_arrows(state, effects)?;
            if timer >= 25 {
                return Ok(if state.flags & 0x40000 != 0 { 16 } else { 1 });
            }
            aircraft_panels(state, timer.wrapping_mul(timer), effects);
            state.timer = state.timer.wrapping_add(1);
            Ok(0)
        }
        0x800d98a0 => {
            context.aircraft_id = state.current.id();
            Ok(2)
        }
        0x800d98c4 => initialize_weapon_phase(state, context, tables, effects),
        0x800d9c78 => Ok(weapon_intro(state, effects)),
        0x800d9ddc => weapon_input(state, tables, pressed, effects),
        0x800da2cc => Ok(weapon_exit(state, effects)),
        0x800da440 => {
            if let Some(&value) = [1, 2, 3, 4, 5, 6, 0].get(state.weapon_choice[0] as usize) {
                context.weapon_codes[0] = value;
            }
            if let Some(&values) = [
                [1, 1],
                [2, 2],
                [1, 3],
                [4, 4],
                [10, 10],
                [7, 7],
                [1, 5],
                [6, 6],
            ]
            .get(state.weapon_choice[1] as usize)
            {
                context.weapon_codes[1..].copy_from_slice(&values);
            }
            Ok(4)
        }
        _ => unreachable!("checked callback table"),
    }
}

fn initialize_aircraft_phase(
    state: &mut AircraftSelectionState,
    context: &mut AircraftContext,
    tables: &AircraftTables,
    effects: &mut Vec<Effect>,
) -> Result<u32, Error> {
    state.flags &= !0x80000;
    state.interpolation_timer = 0;
    effects.push(Effect::SelectVisualPage { page: 4 });
    state.timer = 25;
    offset(effects, 6, -625, 0);
    offset(effects, 7, 625, 0);
    state.panel_offsets[0] = 625;
    state.panel_offsets[3] = 625;
    if state.flags & 0x40000 != 0 {
        offset(effects, 2, 0, -625);
        offset(effects, 3, 625, 0);
        offset(effects, 4, -625, 0);
        state.panel_offsets[1] = -625;
        state.panel_offsets[2] = -625;
    } else {
        for slot in 2..5 {
            offset(effects, slot, 0, 0);
        }
        state.panel_offsets[1] = 0;
        state.panel_offsets[2] = 0;
    }
    sound(effects, 0x5a);
    // 0x800D77E4. Availability accepts source value 1 only.
    state.availability = [0; 24];
    match tables.mission_category(context.mission_id) {
        0x0c | 0x13 => {
            state.availability[17] = 1;
            context.aircraft_id = 17;
        }
        0x10 => {
            state.availability[18] = 1;
            context.aircraft_id = 18;
        }
        _ => {
            let source = if context.options & 0x10 != 0 {
                context.saved_availability
            } else {
                context.availability
            };
            for (id, value) in source.into_iter().enumerate() {
                if id != 17 && id != 18 && value == 1 {
                    state.availability[id] = 1;
                }
            }
        }
    }
    if state.availability[aircraft_id(i32::from(context.aircraft_id))?] == 0
        && let Some(id) = state.availability.iter().position(|&value| value == 1)
    {
        context.aircraft_id = id as i16;
    }
    state.load_rectangle = [0x3c0, 0x100, 0x20, 0x90];
    if let Some(index) = tables
        .aircraft
        .iter()
        .position(|record| record.id() == context.aircraft_id)
    {
        state.selection = index as i32;
        state.current = tables.aircraft[index].scaled();
        load_aircraft(state, effects);
    }
    aircraft_arrows(state, effects)?;
    Ok(1)
}
fn load_aircraft(state: &AircraftSelectionState, effects: &mut Vec<Effect>) {
    effects.push(Effect::LoadAircraftResources {
        index: state.selection,
        rectangle: state.load_rectangle,
    });
    style(effects, 0x1f, u32::from(state.current.0[0x21]));
    style(effects, 0x20, u32::from(state.current.0[0x20]));
}
fn aircraft_panels(state: &mut AircraftSelectionState, square: i32, effects: &mut Vec<Effect>) {
    let negative = square.wrapping_neg();
    offset(effects, 6, negative, 0);
    offset(effects, 7, square, 0);
    state.panel_offsets[0] = square as i16;
    if state.flags & 0x40000 != 0 {
        offset(effects, 2, 0, negative);
        offset(effects, 3, square, 0);
        offset(effects, 4, negative, 0);
        state.panel_offsets[1] = negative as i16;
        state.panel_offsets[2] = negative as i16;
    }
}
fn aircraft_intro(state: &mut AircraftSelectionState, effects: &mut Vec<Effect>) -> u32 {
    if state.timer < 1 {
        for slot in [6, 7, 2, 3, 4] {
            offset(effects, slot, 0, 0);
        }
        state.panel_offsets[..3].fill(0);
        state.timer = 0;
        state.flags = (state.flags & !0x40000) | 0x1000000;
        1
    } else {
        aircraft_panels(state, state.timer.wrapping_mul(state.timer), effects);
        state.timer = state.timer.wrapping_sub(1);
        0
    }
}
fn aircraft_input(
    state: &mut AircraftSelectionState,
    tables: &AircraftTables,
    pressed: u32,
    effects: &mut Vec<Effect>,
) -> Result<u32, Error> {
    if pressed & 0x10 != 0 {
        state.flash_timer = 0;
        state.flags = (state.flags & !0x1c) | 0x10;
        sound(effects, 0x3c);
        sound(effects, 0x5b);
        return Ok(1);
    }
    if pressed & 0x20 != 0 {
        state.flags = (state.flags & !0x1000000) | 0x40000;
        sound(effects, 0x3b);
        sound(effects, 0x5b);
        return Ok(1);
    }
    if pressed & 12 != 0 {
        if !(0..21).contains(&state.selection) {
            return Err(Error::InvalidAircraftIndex(state.selection));
        }
        let step = if pressed & 8 != 0 { 1 } else { -1 };
        let mut candidate = state.selection + step;
        while (0..21).contains(&candidate) {
            let next = tables.aircraft[candidate as usize];
            if state.availability[aircraft_id(i32::from(next.id()))?] != 0 {
                sound(effects, 0x3d);
                state.selection = candidate;
                if state.flags & 0x10000 == 0 {
                    state.flags = (state.flags & !0x20000) | 0x10000;
                }
                state.flash_timer = 0;
                state.flags = (state.flags & !0x1c) | if step > 0 { 8 } else { 4 };
                state.previous = state.current;
                state.next = next.scaled();
                state.interpolation_timer = 0;
                state.flags |= 1;
                break;
            }
            candidate += step;
        }
    }
    aircraft_arrows(state, effects)?;
    Ok(0)
}
fn aircraft_arrows(
    state: &mut AircraftSelectionState,
    effects: &mut Vec<Effect>,
) -> Result<(), Error> {
    let id = aircraft_id(i32::from(state.current.id()))?;
    style(effects, 1, 3);
    style(effects, 2, 3);
    style(effects, 3, 2);
    for candidate in id + 1..22 {
        if state.availability[candidate] == 1 {
            style(effects, 2, 2);
        }
    }
    for candidate in (0..id).rev() {
        if state.availability[candidate] == 1 {
            style(effects, 1, 2);
        }
    }
    if state.flags & 0x1c != 0 {
        for (bit, slot) in [(4, 1), (8, 2), (16, 3)] {
            if state.flags & bit != 0 {
                style(effects, slot, 1);
            }
        }
        if state.flash_timer < 25 {
            state.flash_timer = state.flash_timer.wrapping_add(1);
        } else {
            state.flash_timer = 0;
            state.flags &= !0x1c;
        }
    }
    Ok(())
}

/// 0x800D7950. The first interpolation tick changes the ID/metadata immediately,
/// while numeric values still use interpolation time zero.
pub fn interpolate_aircraft(state: &mut AircraftSelectionState) {
    if state.flags & 1 == 0 {
        return;
    }
    state.current.0[..2].copy_from_slice(&state.next.0[..2]);
    for offset in (2..12).step_by(2) {
        let previous = i32::from(read_u16(&state.previous.0, offset) as i16);
        let next = i32::from(read_u16(&state.next.0, offset) as i16);
        let value = previous.wrapping_add(
            next.wrapping_sub(previous)
                .wrapping_mul(state.interpolation_timer)
                / 50,
        ) as i16;
        state.current.0[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    for offset in (12..32).step_by(4) {
        let previous = read_u32(&state.previous.0, offset) as i32;
        let next = read_u32(&state.next.0, offset) as i32;
        let value = previous.wrapping_add(
            next.wrapping_sub(previous)
                .wrapping_mul(state.interpolation_timer)
                / 50,
        );
        state.current.0[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    state.current.0[32..35].copy_from_slice(&state.next.0[32..35]);
    if state.interpolation_timer < 50 {
        state.interpolation_timer = state.interpolation_timer.wrapping_add(1);
    } else {
        state.interpolation_timer = 0;
        state.flags &= !1;
    }
}
fn update_aircraft_resource_slide(state: &mut AircraftSelectionState, effects: &mut Vec<Effect>) {
    if state.flags & 0x10000 != 0 {
        if state.load_timer < 25 {
            state.load_timer = state.load_timer.wrapping_add(1);
        } else {
            load_aircraft(state, effects);
            state.flags = (state.flags & !0x10000) | 0x20000;
        }
    } else if state.flags & 0x20000 != 0 {
        if state.load_timer < 1 {
            state.flags &= !0x20000;
        } else {
            state.load_timer = state.load_timer.wrapping_sub(1);
        }
    }
    offset(
        effects,
        5,
        state
            .load_timer
            .wrapping_mul(state.load_timer)
            .wrapping_neg()
            .wrapping_add(i32::from(state.panel_offsets[2])),
        0,
    );
}

fn initialize_weapon_phase(
    state: &mut AircraftSelectionState,
    context: &AircraftContext,
    tables: &AircraftTables,
    effects: &mut Vec<Effect>,
) -> Result<u32, Error> {
    let category = tables.mission_category(context.mission_id);
    state.flags |= 0x80000;
    effects.push(Effect::SelectVisualPage { page: 5 });
    state.timer = 25;
    for slot in 9..24 {
        offset(effects, slot, 625, 0);
    }
    state.weapon_row = 0;
    state.weapon_availability = [[0; 8]; 2];
    if category == 0x12 {
        state.weapon_availability[0][6] = 1;
        state.weapon_availability[1][7] = 1;
        state.weapon_choice = [6, 7];
    } else {
        let record = tables
            .loadouts
            .iter()
            .find(|record| read_u32(*record, 0) as i32 == i32::from(context.aircraft_id))
            .ok_or(Error::MissingAircraftLoadout(context.aircraft_id))?;
        for row in 0..2 {
            let count = record[4 + row];
            if count > 4 {
                return Err(Error::InvalidLoadoutCount { row, count });
            }
            for &id in &record[8 + row * 4..8 + row * 4 + usize::from(count)] {
                state.weapon_availability[row][weapon_id(row, i32::from(id))?] = 1;
            }
            state.weapon_choice[row] = i16::from(record[6 + row]);
        }
    }
    for row in 0..2 {
        state.weapon_current[row] = tables
            .weapon(row, i32::from(state.weapon_choice[row]))?
            .scaled();
        state.weapon_interpolation_timer[row] = 0;
        state.weapon_flags[row] &= !1;
    }
    if category != 0x10 && category != 0x12 && context.options & 0x10 != 0 {
        if category.wrapping_sub(0x1f) > 2 && context.special_unlocks[0] != 0 {
            state.weapon_availability[1][5] = 1;
        }
        if context.special_unlocks[1] != 0 {
            state.weapon_availability[1][6] = 1;
        }
    }
    weapon_arrows(state, effects)?;
    effects.push(Effect::ClearTextVisibility);
    for row in 0..2 {
        effects.push(Effect::SetTextVisibility {
            entry: state.weapon_current[row]
                .id()
                .wrapping_add(if row == 0 { 0xaa } else { 0xb1 }),
            value: 1,
        });
    }
    sound(effects, 0x5a);
    Ok(1)
}
fn weapon_intro(state: &mut AircraftSelectionState, effects: &mut Vec<Effect>) -> u32 {
    let square;
    let result;
    if state.timer < 1 {
        state.timer = 0;
        square = 0;
        result = 1;
    } else {
        square = state.timer.wrapping_mul(state.timer);
        state.timer = state.timer.wrapping_sub(1);
        result = 0;
    }
    state.panel_offsets[3] = square as i16;
    offset(effects, 6, square.wrapping_neg(), 0);
    offset(effects, 7, square, 0);
    for slot in 9..24 {
        offset(effects, slot, square, 0);
    }
    result
}
fn weapon_exit(state: &mut AircraftSelectionState, effects: &mut Vec<Effect>) -> u32 {
    let timer = state.timer;
    let square = timer.wrapping_mul(timer);
    state.panel_offsets[3] = square as i16;
    if timer >= 25 {
        state.timer = 0;
        return if state.flags & 0x40000 != 0 { 1 } else { 8 };
    }
    offset(effects, 6, square.wrapping_neg(), 0);
    offset(effects, 7, square, 0);
    if state.flags & 0x40000 != 0 {
        let negative = square.wrapping_neg();
        offset(effects, 4, negative, 0);
        offset(effects, 3, negative, 0);
        state.panel_offsets[1] = negative as i16;
        state.panel_offsets[2] = negative as i16;
    }
    for slot in 9..24 {
        offset(effects, slot, square, 0);
    }
    state.timer = state.timer.wrapping_add(1);
    0
}
fn weapon_input(
    state: &mut AircraftSelectionState,
    tables: &AircraftTables,
    pressed: u32,
    effects: &mut Vec<Effect>,
) -> Result<u32, Error> {
    let row = usize::try_from(state.weapon_row)
        .ok()
        .filter(|&row| row < 2)
        .ok_or(Error::InvalidWeaponRow(state.weapon_row))?;
    // Unlike aircraft choice, cancel wins over confirm here.
    if pressed & 0x20 != 0 {
        sound(effects, 0x3b);
        sound(effects, 0x5b);
        return Ok(1);
    }
    if pressed & 0x10 != 0 {
        state.flash_timer = 0;
        state.flags = (state.flags & 0xfeff0fef) | 0x40010;
        sound(effects, 0x3c);
        sound(effects, 0x5b);
        return Ok(1);
    }
    if pressed & 3 != 0 {
        state.weapon_row ^= 1;
        sound(effects, 0x3d);
    } else if pressed & 12 != 0 {
        let step = if pressed & 8 != 0 { 1 } else { -1 };
        let direction = if step > 0 { 4 } else { 2 };
        let reverse = direction ^ 6;
        let mut candidate = i32::from(state.weapon_choice[row]).wrapping_add(step);
        while (0..8).contains(&candidate) {
            if state.weapon_availability[row][candidate as usize] == 1 {
                // The original sounds before checking the movement lock and keeps
                // scanning after selection: a second eligible item sounds again.
                sound(effects, 0x3d);
                if state.weapon_flags[row] & direction != 0 {
                    break;
                }
                if state.weapon_flags[row] & reverse == 0 {
                    state.weapon_flags[row] |= direction;
                    state.weapon_slide_timer[row] = 0;
                } else {
                    state.weapon_flags[row] = (state.weapon_flags[row] & !reverse) | direction;
                    state.weapon_slide_timer[row] =
                        10_i32.wrapping_sub(state.weapon_slide_timer[row]);
                }
                state.weapon_choice[row] = candidate as i16;
                state.flags = (state.flags & 0xffff0fef)
                    | match (row, step) {
                        (0, 1) => 0x2000,
                        (0, _) => 0x1000,
                        (1, 1) => 0x8000,
                        _ => 0x4000,
                    };
                state.flash_timer = 0;
                state.weapon_previous[row] = state.weapon_current[row];
                state.weapon_next[row] = tables.weapon(row, candidate)?.scaled();
                state.weapon_interpolation_timer[row] = 0;
                state.weapon_flags[row] |= 1;
            }
            candidate += step;
        }
    }
    weapon_arrows(state, effects)?;
    Ok(0)
}
fn weapon_arrows(
    state: &mut AircraftSelectionState,
    effects: &mut Vec<Effect>,
) -> Result<(), Error> {
    state.flags &= !0xf00;
    for row in 0..2 {
        let id = weapon_id(row, state.weapon_current[row].id())?;
        for candidate in id + 1..8 {
            if state.weapon_availability[row][candidate] == 1 {
                state.flags |= if row == 0 { 0x200 } else { 0x800 };
            }
        }
        for candidate in (0..id).rev() {
            if state.weapon_availability[row][candidate] == 1 {
                state.flags |= if row == 0 { 0x100 } else { 0x400 };
            }
        }
    }
    for index in 0..4 {
        style(
            effects,
            index + 1,
            if state.flags & (0x100 << index) != 0 {
                2
            } else {
                0
            },
        );
    }
    style(effects, 5, state.weapon_row.wrapping_add(1) as u32);
    style(effects, 6, 2);
    if state.flags & 0xf010 != 0 {
        for (bit, slot) in [
            (0x1000, 1),
            (0x2000, 2),
            (0x4000, 3),
            (0x8000, 4),
            (0x10, 6),
        ] {
            if state.flags & bit != 0 {
                style(effects, slot, 1);
            }
        }
        if state.flash_timer < 10 {
            state.flash_timer = state.flash_timer.wrapping_add(1);
        } else {
            state.flash_timer = 0;
            state.flags &= !0xf010;
        }
    }
    Ok(())
}
/// 0x800D7C54: two independent ten-tick byte interpolation records.
pub fn interpolate_weapons(state: &mut AircraftSelectionState) {
    for row in 0..2 {
        if state.weapon_flags[row] & 1 == 0 {
            continue;
        }
        state.weapon_current[row].0[..4].copy_from_slice(&state.weapon_next[row].0[..4]);
        for offset in 4..7 {
            let previous = i32::from(state.weapon_previous[row].0[offset]);
            let next = i32::from(state.weapon_next[row].0[offset]);
            state.weapon_current[row].0[offset] = previous.wrapping_add(
                next.wrapping_sub(previous)
                    .wrapping_mul(state.weapon_interpolation_timer[row])
                    / 10,
            ) as u8;
        }
        state.weapon_current[row].0[7] = state.weapon_next[row].0[7];
        if state.weapon_interpolation_timer[row] < 10 {
            state.weapon_interpolation_timer[row] =
                state.weapon_interpolation_timer[row].wrapping_add(1);
        } else {
            state.weapon_interpolation_timer[row] = 0;
            state.weapon_flags[row] &= !1;
        }
    }
}
fn update_weapon_slide(state: &mut AircraftSelectionState, effects: &mut Vec<Effect>) {
    for slot in 9..24 {
        style(effects, slot, 0);
    }
    for row in 0..2 {
        if state.weapon_flags[row] & 6 != 0 {
            if state.weapon_slide_timer[row] < 10 {
                state.weapon_slide_timer[row] = state.weapon_slide_timer[row].wrapping_add(1);
            } else {
                state.weapon_slide_timer[row] = 0;
                state.weapon_flags[row] &= !6;
            }
        }
        let base = if row == 0 { 9 } else { 16 };
        if state.weapon_flags[row] & 6 == 0 {
            style(
                effects,
                state.weapon_current[row].id().wrapping_add(base),
                1,
            );
        } else {
            let previous = state.weapon_previous[row].id().wrapping_add(base);
            let next = state.weapon_next[row].id().wrapping_add(base);
            style(effects, previous, 1);
            style(effects, next, 1);
            let distance =
                state.weapon_slide_timer[row].wrapping_mul(if state.weapon_flags[row] & 2 != 0 {
                    5
                } else {
                    -5
                });
            offset(effects, previous, distance, 0);
            offset(
                effects,
                next,
                distance.wrapping_add(if state.weapon_flags[row] & 2 != 0 {
                    -50
                } else {
                    50
                }),
                0,
            );
        }
    }
    if state.flags & 0x80000 != 0 {
        effects.push(Effect::ClearTextVisibility);
        if let Ok(row) = usize::try_from(state.weapon_row)
            && row < 2
        {
            effects.push(Effect::SetTextVisibility {
                entry: state.weapon_current[row].id().wrapping_add(if row == 0 {
                    0xaa
                } else {
                    0xb1
                }),
                value: 1,
            });
            style(effects, 7, u32::from(state.weapon_current[row].0[7]));
        }
    }
}
