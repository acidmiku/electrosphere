//! Original inner-state 14 loader, 0x800D3254/0x800D328C and five leaf tables.
//! Resource I/O, texture processing, scene allocation and drawing are host effects.
use crate::{AircraftTables, Effect, read_u16, read_u32};

pub const LOADER_CALLBACKS: [&[u32]; 5] = [
    &[0x800ced30, 0x800ced5c],
    &[0x800ceda8],
    &[0x800cee3c, 0x800ceef0, 0x800cef34, 0x800cf014],
    &[0x800cf064, 0x800cf100, 0x800cf144, 0x800cf25c, 0x800cf2f4],
    &[0x800cf350],
];
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoaderError {
    BufferTooShort { required: usize, actual: usize },
    InvalidPhase { phase: i16, substate: i16 },
    DivisionOverflow,
    InvalidLoadoutCount { row: usize, count: u8 },
    InvalidWeaponId { row: usize, id: u8 },
}
impl std::fmt::Display for LoaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for LoaderError {}
fn require(bytes: &[u8], required: usize) -> Result<(), LoaderError> {
    if bytes.len() < required {
        Err(LoaderError::BufferTooShort {
            required,
            actual: bytes.len(),
        })
    } else {
        Ok(())
    }
}

/// Control halfwords at [0x800F5E74], and scalar fields in [0x800F1E4C].
/// Original pointers at loader +0/+4/+0x18 are host-owned and preserved by writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MissionLoaderState {
    pub phase: i16,
    pub substate: i16,
    pub page: i32,
    pub fade_timer: i32,
    pub total_bytes: i32,
    pub loaded_bytes: i32,
    pub mission_bytes: i32,
}
impl MissionLoaderState {
    pub fn from_bytes(control: &[u8], loader: &[u8]) -> Result<Self, LoaderError> {
        require(control, 4)?;
        require(loader, 0x20)?;
        Ok(Self {
            phase: read_u16(control, 0) as i16,
            substate: read_u16(control, 2) as i16,
            page: read_u32(loader, 8) as i32,
            fade_timer: read_u32(loader, 0xc) as i32,
            total_bytes: read_u32(loader, 0x10) as i32,
            loaded_bytes: read_u32(loader, 0x14) as i32,
            mission_bytes: read_u32(loader, 0x1c) as i32,
        })
    }
    pub fn write_into(&self, control: &mut [u8], loader: &mut [u8]) -> Result<(), LoaderError> {
        require(control, 4)?;
        require(loader, 0x20)?;
        control[..2].copy_from_slice(&self.phase.to_le_bytes());
        control[2..4].copy_from_slice(&self.substate.to_le_bytes());
        for (offset, value) in [
            (8, self.page),
            (0xc, self.fade_timer),
            (0x10, self.total_bytes),
            (0x14, self.loaded_bytes),
            (0x1c, self.mission_bytes),
        ] {
            loader[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoaderContext {
    pub mission_id: i16,
    pub options: u32,
    pub first_saved_aircraft_available: u8,
    pub first_aircraft_available: u8,
    /// Original primary base +0x9F8 and secondary base +0x9FE overlap at one byte.
    pub weapon_availability: [u8; 14],
}
impl LoaderContext {
    pub const BYTE_LEN: usize = 0xa06;
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, LoaderError> {
        require(bytes, Self::BYTE_LEN)?;
        Ok(Self {
            mission_id: read_u16(bytes, 0x390) as i16,
            options: read_u32(bytes, 0x970),
            first_saved_aircraft_available: bytes[0x979],
            first_aircraft_available: bytes[0x9b9],
            weapon_availability: bytes[0x9f8..0xa06].try_into().unwrap(),
        })
    }
    pub fn write_into(&self, bytes: &mut [u8]) -> Result<(), LoaderError> {
        require(bytes, Self::BYTE_LEN)?;
        bytes[0x390..0x392].copy_from_slice(&self.mission_id.to_le_bytes());
        bytes[0x970..0x974].copy_from_slice(&self.options.to_le_bytes());
        bytes[0x979] = self.first_saved_aircraft_available;
        bytes[0x9b9] = self.first_aircraft_available;
        bytes[0x9f8..0xa06].copy_from_slice(&self.weapon_availability);
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoaderInputs {
    /// Exact original resource sizes, not decoded TIM count or payload sum.
    pub mission_resource_bytes: i32,
    pub common_resource_bytes: i32,
    /// Cumulative loaded bytes including the first resource during common reads.
    pub transferred_total_bytes: i32,
    pub read_complete: bool,
    /// Return from one texture queue poll (0x800CE844); only exactly 1 advances.
    pub texture_poll_result: i32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoaderRequirement {
    None,
    ResourceSizes,
    MissionResourceSize,
    TransferProgress,
    TextureQueuePoll,
}
pub fn select_loader_callback(state: MissionLoaderState) -> Result<u32, LoaderError> {
    LOADER_CALLBACKS
        .get(usize::try_from(state.phase).unwrap_or(usize::MAX))
        .and_then(|row| row.get(usize::try_from(state.substate).unwrap_or(usize::MAX)))
        .copied()
        .ok_or(LoaderError::InvalidPhase {
            phase: state.phase,
            substate: state.substate,
        })
}
pub fn loader_requirement(state: MissionLoaderState) -> Result<LoaderRequirement, LoaderError> {
    select_loader_callback(state)?;
    Ok(match (state.phase, state.substate) {
        (1, 0) => LoaderRequirement::ResourceSizes,
        (2, 0) => LoaderRequirement::MissionResourceSize,
        (2, 1) | (3, 1) => LoaderRequirement::TransferProgress,
        (2, 3) | (3, 3) => LoaderRequirement::TextureQueuePoll,
        _ => LoaderRequirement::None,
    })
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoaderEffect {
    Common(Effect),
    /// Three-word TILE packet after its DMA tag: RGB E5/E5/D5, (192,420), (width,6).
    DrawProgressBar {
        width: u16,
    },
    /// 0x800CED30 -> 0x800F1A9C: original 256-entry table inside overlay 2.
    BindBuiltinScripts {
        address: u32,
    },
    /// Clear pending resource list, allocate/read this resource; asynchronous readiness is input.
    BeginResourceRead {
        resource: u32,
    },
    /// 0x800CEF34: bind children 0/1/2, relocate child3 when nonempty, queue child4 TIMs.
    BindMissionResource {
        resource: u32,
        texture_child: u8,
        batch_size: i32,
    },
    /// 0x800CF144: bind/relocate common children, initialize text, queue child10 TIMs.
    BindCommonResource {
        resource: u32,
        texture_child: u8,
        batch_size: i32,
    },
    ReleaseResourceChild {
        resource: u32,
        child: u8,
    },
    LoadSoundBank {
        resource: u32,
        child: u8,
    },
    /// 0x800E8008 starts sounds 0x5D..0x61, saves their voice handles at
    /// background +0x110..0x120, then sets each gain to 0 over 0 ticks.
    /// It does not reset map geometry, frame clock, or animation state.
    InitializeBackgroundVoices,
    /// 0x800E7E20 copies the initial palette before dynamic rendering changes it.
    SnapshotMapPalette,
    /// 0x800CEB90 restores common child3's script table before selecting page1.
    BindCommonScripts {
        resource: u32,
        child: u8,
    },
}
impl From<Effect> for LoaderEffect {
    fn from(effect: Effect) -> Self {
        Self::Common(effect)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoaderUpdate {
    pub state: MissionLoaderState,
    pub context: LoaderContext,
    pub callback_result: u32,
    pub effects: Vec<LoaderEffect>,
}
/// 0x800D1890 has a different not-found rule from mission_category: first record.
pub fn loader_mission_resource(context: LoaderContext, tables: &AircraftTables) -> u32 {
    if context.options & 0x50 == 0x50 {
        return 0x6e;
    }
    let record = tables.missions[..41]
        .iter()
        .find(|r| i32::from(r[0]) == i32::from(context.mission_id))
        .unwrap_or(&tables.missions[0]);
    read_u32(record, 4)
}
pub fn initialize_mission_loader(
    mut state: MissionLoaderState,
    context: LoaderContext,
) -> LoaderUpdate {
    state.phase = 0;
    state.substate = 0;
    state.page = 3;
    LoaderUpdate {
        state,
        context,
        callback_result: 1,
        effects: vec![Effect::PlaySound { id: 0x56 }.into()],
    }
}
fn divide(n: i32, d: i32) -> Result<i32, LoaderError> {
    n.checked_div(d).ok_or(LoaderError::DivisionOverflow)
}
/// 0x800CEBCC clamps only above total, then 0x800D328C emits pre-leaf progress.
pub fn loader_progress(state: MissionLoaderState) -> Result<(u16, i32), LoaderError> {
    let loaded = state.loaded_bytes.min(state.total_bytes);
    if state.total_bytes == 0 {
        Ok((0, 0))
    } else {
        Ok((
            divide(loaded.wrapping_mul(115), state.total_bytes)? as u16,
            divide(loaded.wrapping_sub(1).wrapping_mul(7), state.total_bytes)?,
        ))
    }
}
fn unlock_first_aircraft(
    context: &mut LoaderContext,
    tables: &AircraftTables,
) -> Result<(), LoaderError> {
    let record = tables
        .loadouts
        .iter()
        .find(|r| read_u32(*r, 0) == 1)
        .unwrap_or(&tables.loadouts[0]);
    for row in 0..2 {
        let count = record[4 + row];
        if count > 4 {
            return Err(LoaderError::InvalidLoadoutCount { row, count });
        }
        for &id in &record[8 + row * 4..8 + row * 4 + usize::from(count)] {
            if usize::from(id) >= if row == 0 { 7 } else { 8 } {
                return Err(LoaderError::InvalidWeaponId { row, id });
            }
            context.weapon_availability[row * 6 + usize::from(id)] = 1;
        }
    }
    context.first_saved_aircraft_available = 1;
    context.first_aircraft_available = 1;
    Ok(())
}
/// Full scalar loader callback. I/O performs work in the host; supplied results
/// are consumed only by the matching original leaf and never forced to ready.
pub fn tick_mission_loader(
    mut state: MissionLoaderState,
    mut context: LoaderContext,
    input: LoaderInputs,
    tables: &AircraftTables,
) -> Result<LoaderUpdate, LoaderError> {
    select_loader_callback(state)?;
    let (width, variant) = loader_progress(state)?;
    let mut effects = vec![LoaderEffect::DrawProgressBar { width }];
    for i in 0..6 {
        effects.push(
            Effect::SetVisualStyle {
                slot: i + 1,
                style: u32::from(i < variant),
            }
            .into(),
        );
    }
    effects.push(
        Effect::SetVisualStyle {
            slot: 7,
            style: variant as u32,
        }
        .into(),
    );
    let resource = loader_mission_resource(context, tables);
    let result = match (state.phase, state.substate) {
        (0, 0) => {
            effects.push(LoaderEffect::BindBuiltinScripts {
                address: 0x800f1e50,
            });
            effects.push(Effect::SelectVisualPage { page: 1 }.into());
            1
        }
        (0, 1) => {
            effects.push(
                Effect::SelectVisualPage {
                    page: state.page as u32,
                }
                .into(),
            );
            effects.push(Effect::SetVisualParameter { slot: 1, value: 0 }.into());
            state.fade_timer = 0;
            2
        }
        (1, 0) => {
            state.loaded_bytes = 0;
            state.total_bytes = input
                .mission_resource_bytes
                .wrapping_add(input.common_resource_bytes);
            2
        }
        (2, 0) => {
            state.mission_bytes = input.mission_resource_bytes;
            effects.push(LoaderEffect::BeginResourceRead { resource });
            1
        }
        (2, 1) | (3, 1) => {
            state.loaded_bytes = input.transferred_total_bytes;
            u32::from(input.read_complete)
        }
        (2, 2) => {
            effects.push(LoaderEffect::BindMissionResource {
                resource,
                texture_child: 4,
                batch_size: 10,
            });
            1
        }
        (2, 3) => {
            if input.texture_poll_result == 1 {
                effects.push(LoaderEffect::ReleaseResourceChild { resource, child: 4 });
                2
            } else {
                0
            }
        }
        (3, 0) => {
            effects.push(LoaderEffect::BeginResourceRead { resource: 72 });
            1
        }
        (3, 2) => {
            effects.push(LoaderEffect::BindCommonResource {
                resource: 72,
                texture_child: 10,
                batch_size: 10,
            });
            1
        }
        (3, 3) => {
            if input.texture_poll_result == 1 {
                effects.extend([
                    LoaderEffect::ReleaseResourceChild {
                        resource: 72,
                        child: 10,
                    },
                    LoaderEffect::LoadSoundBank {
                        resource: 72,
                        child: 9,
                    },
                    LoaderEffect::ReleaseResourceChild {
                        resource: 72,
                        child: 9,
                    },
                    LoaderEffect::InitializeBackgroundVoices,
                    LoaderEffect::SnapshotMapPalette,
                ]);
                1
            } else {
                0
            }
        }
        (3, 4) => {
            effects.push(
                Effect::SetVisualParameter {
                    slot: 1,
                    value: state.fade_timer,
                }
                .into(),
            );
            if state.fade_timer < 10 {
                state.fade_timer = state.fade_timer.wrapping_add(1);
                0
            } else {
                2
            }
        }
        (4, 0) => 4,
        _ => unreachable!("checked loader table"),
    };
    let mut callback_result = 0;
    if result & 1 != 0 {
        state.substate = state.substate.wrapping_add(1);
    } else if result & 2 != 0 {
        state.phase = state.phase.wrapping_add(1);
        state.substate = 0;
    } else if result & 4 != 0 {
        if context.mission_id == 1 {
            unlock_first_aircraft(&mut context, tables)?;
        }
        effects.push(LoaderEffect::BindCommonScripts {
            resource: 72,
            child: 3,
        });
        effects.push(Effect::SelectVisualPage { page: 1 }.into());
        callback_result = 0x100;
    }
    Ok(LoaderUpdate {
        state,
        context,
        callback_result,
        effects,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tables() -> AircraftTables {
        AircraftTables {
            aircraft: [crate::AircraftRecord::default(); 21],
            loadouts: [[0; 16]; 21],
            primary_weapons: [crate::WeaponRecord::default(); 7],
            secondary_weapons: [crate::WeaponRecord::default(); 8],
            missions: [[0; 12]; 42],
        }
    }
    #[test]
    fn progress_uses_pre_leaf_bytes_signed_clamp_and_original_truncation() {
        let mut state = MissionLoaderState {
            total_bytes: 100,
            loaded_bytes: 100,
            ..Default::default()
        };
        assert_eq!(loader_progress(state), Ok((115, 6)));
        state.loaded_bytes = 200;
        assert_eq!(loader_progress(state), Ok((115, 6)));
        state.loaded_bytes = 1;
        assert_eq!(loader_progress(state), Ok((1, 0)));
        state.loaded_bytes = -100;
        assert_eq!(loader_progress(state), Ok(((-115_i16) as u16, -7)));
        state.total_bytes = 0;
        assert_eq!(loader_progress(state), Ok((0, 0)));
    }
    #[test]
    fn reads_wait_and_progress_is_drawn_before_new_transfer_result() {
        let state = MissionLoaderState {
            phase: 2,
            substate: 1,
            total_bytes: 100,
            loaded_bytes: 20,
            ..Default::default()
        };
        let update = tick_mission_loader(
            state,
            LoaderContext::default(),
            LoaderInputs {
                transferred_total_bytes: 80,
                ..Default::default()
            },
            &tables(),
        )
        .unwrap();
        assert_eq!(update.state.substate, 1);
        assert_eq!(update.state.loaded_bytes, 80);
        assert_eq!(
            update.effects[0],
            LoaderEffect::DrawProgressBar { width: 23 }
        );
        let update = tick_mission_loader(
            update.state,
            update.context,
            LoaderInputs {
                transferred_total_bytes: 100,
                read_complete: true,
                ..Default::default()
            },
            &tables(),
        )
        .unwrap();
        assert_eq!(update.state.substate, 2);
    }
    #[test]
    fn only_exactly_one_completes_texture_queue_and_voices_follow_bank() {
        let state = MissionLoaderState {
            phase: 3,
            substate: 3,
            ..Default::default()
        };
        for result in [-1, 0, 2] {
            let update = tick_mission_loader(
                state,
                LoaderContext::default(),
                LoaderInputs {
                    texture_poll_result: result,
                    ..Default::default()
                },
                &tables(),
            )
            .unwrap();
            assert_eq!(update.state, state);
            assert_eq!(update.effects.len(), 8);
        }
        let update = tick_mission_loader(
            state,
            LoaderContext::default(),
            LoaderInputs {
                texture_poll_result: 1,
                ..Default::default()
            },
            &tables(),
        )
        .unwrap();
        assert_eq!(update.state.substate, 4);
        assert_eq!(
            &update.effects[8..],
            &[
                LoaderEffect::ReleaseResourceChild {
                    resource: 72,
                    child: 10
                },
                LoaderEffect::LoadSoundBank {
                    resource: 72,
                    child: 9
                },
                LoaderEffect::ReleaseResourceChild {
                    resource: 72,
                    child: 9
                },
                LoaderEffect::InitializeBackgroundVoices,
                LoaderEffect::SnapshotMapPalette
            ]
        );
    }
    #[test]
    fn fade_emits_zero_through_ten_before_advancing() {
        let mut state = MissionLoaderState {
            phase: 3,
            substate: 4,
            ..Default::default()
        };
        for value in 0..=10 {
            let update = tick_mission_loader(
                state,
                LoaderContext::default(),
                LoaderInputs::default(),
                &tables(),
            )
            .unwrap();
            assert_eq!(
                update.effects.last(),
                Some(&LoaderEffect::Common(Effect::SetVisualParameter {
                    slot: 1,
                    value
                }))
            );
            assert_eq!(update.state.phase, if value == 10 { 4 } else { 3 });
            state = update.state;
        }
    }
    #[test]
    fn final_first_mission_unlock_uses_original_shared_availability_bases() {
        let mut tables = tables();
        tables.loadouts[0][..4].copy_from_slice(&1_u32.to_le_bytes());
        tables.loadouts[0][4..6].copy_from_slice(&[2, 2]);
        tables.loadouts[0][8..10].copy_from_slice(&[0, 6]);
        tables.loadouts[0][12..14].copy_from_slice(&[0, 7]);
        let context = LoaderContext {
            mission_id: 1,
            ..Default::default()
        };
        let update = tick_mission_loader(
            MissionLoaderState {
                phase: 4,
                ..Default::default()
            },
            context,
            LoaderInputs::default(),
            &tables,
        )
        .unwrap();
        assert_eq!(update.callback_result, 0x100);
        assert_eq!(
            update.context.weapon_availability,
            [1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1]
        );
        assert_eq!(update.context.first_saved_aircraft_available, 1);
        assert_eq!(update.context.first_aircraft_available, 1);
        let other = LoaderContext {
            mission_id: 2,
            ..context
        };
        assert_eq!(
            tick_mission_loader(
                MissionLoaderState {
                    phase: 4,
                    ..Default::default()
                },
                other,
                LoaderInputs::default(),
                &tables
            )
            .unwrap()
            .context,
            other
        );
    }
    #[test]
    fn missing_mission_falls_back_to_first_and_special_options_override() {
        let mut tables = tables();
        tables.missions[0][4..8].copy_from_slice(&74_u32.to_le_bytes());
        tables.missions[41][4..8].copy_from_slice(&123_u32.to_le_bytes());
        assert_eq!(
            loader_mission_resource(
                LoaderContext {
                    mission_id: -1,
                    ..Default::default()
                },
                &tables
            ),
            74
        );
        assert_eq!(
            loader_mission_resource(
                LoaderContext {
                    options: 0x50,
                    ..Default::default()
                },
                &tables
            ),
            110
        );
    }
    #[test]
    fn sparse_bridge_preserves_pointers_and_short_write_is_atomic() {
        let mut control = [0xaa; 4];
        let mut loader = [0xaa; 0x20];
        let state = MissionLoaderState {
            phase: 3,
            substate: 4,
            page: 3,
            ..Default::default()
        };
        assert!(state.write_into(&mut control, &mut loader[..10]).is_err());
        assert_eq!(control, [0xaa; 4]);
        state.write_into(&mut control, &mut loader).unwrap();
        assert_eq!(&loader[..8], &[0xaa; 8]);
        assert_eq!(&loader[0x18..0x1c], &[0xaa; 4]);
        assert_eq!(
            MissionLoaderState::from_bytes(&control, &loader).unwrap(),
            state
        );
        assert!(
            select_loader_callback(MissionLoaderState {
                phase: -1,
                ..Default::default()
            })
            .is_err()
        );
        assert!(
            select_loader_callback(MissionLoaderState {
                phase: 3,
                substate: 5,
                ..Default::default()
            })
            .is_err()
        );
    }
}
