//! Deterministic prefix of resident 80032510, ending before 80076210.
//!
//! Native assets provide relative references. Only bytes written by the original
//! prefix are initialized; allocator contents in the gaps are explicitly unknown.
//! This is not a completed gameplay initializer and returns no callback result.

use crate::flight_assets::{FlightAircraftAssets, FlightMissionAssets, RecordRef};
use std::fmt;

pub const GAMEPLAY_BYTES: usize = 700;
pub const ENTITY_COUNT: usize = 64;
pub const ENTITY_TABLE_BYTES: usize = ENTITY_COUNT * 4;
pub const PREFIX_ALLOCATION_BYTES: usize = 8628;
pub const PREFIX_START: u32 = 0x80032548;
pub const PENDING_CALL_PC: u32 = 0x800327e0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    Uninitialized { offset: usize, length: usize },
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid flight initialization: {reason}"),
            Self::Uninitialized { offset, length } => write!(
                f,
                "flight field {offset:#x}+{length:#x} is not initialized scalar data"
            ),
        }
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// Host references have identities and offsets, never guest/host pointer values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reference {
    MissionSection {
        section: usize,
        offset: usize,
    },
    AircraftSection {
        section: usize,
        offset: usize,
    },
    Gameplay {
        offset: usize,
    },
    Entity {
        index: usize,
        offset: usize,
    },
    /// Relative to the gameplay arena base, the loaded aircraft end.
    Arena {
        offset: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceWrite {
    pub offset: usize,
    pub target: Reference,
}

/// Exact known scalar writes and separately typed pointer fields. The zero bytes
/// under a false mask are storage placeholders, not a claim about original RAM.
#[derive(Debug, Clone)]
pub struct InitializedFields {
    bytes: Vec<u8>,
    mask: Vec<bool>,
    references: Vec<ReferenceWrite>,
}
impl InitializedFields {
    /// Storage with no known bytes; use `merge` for verified sparse writes.
    pub fn new(size: usize) -> Self {
        Self {
            bytes: vec![0; size],
            mask: vec![false; size],
            references: Vec::new(),
        }
    }
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    pub fn scalar_bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn scalar_mask(&self) -> &[bool] {
        &self.mask
    }
    pub fn references(&self) -> &[ReferenceWrite] {
        &self.references
    }
    pub fn scalar(&self, offset: usize, length: usize) -> Result<&[u8]> {
        let end = offset
            .checked_add(length)
            .ok_or(Error::Invalid("scalar range overflow"))?;
        let mask = self
            .mask
            .get(offset..end)
            .ok_or(Error::Invalid("scalar range outside allocation"))?;
        if mask.iter().any(|known| !known) {
            return Err(Error::Uninitialized { offset, length });
        }
        Ok(&self.bytes[offset..end])
    }
    /// Merge a verified callee's sparse writes. Reference offsets are relative
    /// to `bytes`; unknown input bytes preserve the destination. Validate the
    /// entire update before mutation, including overlap with typed pointers.
    pub fn merge(
        &mut self,
        offset: usize,
        bytes: &[u8],
        mask: &[bool],
        references: &[ReferenceWrite],
    ) -> Result<()> {
        let end = offset
            .checked_add(bytes.len())
            .ok_or(Error::Invalid("merge range overflow"))?;
        if bytes.len() != mask.len() || end > self.len() || references.len() > bytes.len() / 4 {
            return Err(Error::Invalid("merge lengths or destination bounds"));
        }
        let mut incoming = Vec::with_capacity(references.len());
        for reference in references {
            let pointer_end = reference
                .offset
                .checked_add(4)
                .ok_or(Error::Invalid("reference range overflow"))?;
            if pointer_end > bytes.len()
                || (offset + reference.offset) & 3 != 0
                || mask[reference.offset..pointer_end]
                    .iter()
                    .any(|&known| known)
            {
                return Err(Error::Invalid(
                    "merge pointer bounds, alignment or scalar overlap",
                ));
            }
            if incoming
                .iter()
                .any(|r: &ReferenceWrite| r.offset == offset + reference.offset)
            {
                return Err(Error::Invalid("duplicate merge reference"));
            }
            incoming.push(ReferenceWrite {
                offset: offset + reference.offset,
                target: reference.target,
            });
        }
        let mut combined = Vec::new();
        for existing in &self.references {
            let written: Vec<bool> = (existing.offset..existing.offset + 4)
                .map(|at| at >= offset && at < end && mask[at - offset])
                .collect();
            if written.iter().any(|&value| value) && !written.iter().all(|&value| value) {
                return Err(Error::Invalid(
                    "partial scalar overwrite of typed reference",
                ));
            }
            if !written.iter().all(|&value| value)
                && !incoming.iter().any(|r| r.offset == existing.offset)
            {
                combined.push(*existing);
            }
        }
        combined.extend(incoming.iter().copied());
        combined.sort_by_key(|r| r.offset);
        for (index, (&value, &known)) in bytes.iter().zip(mask).enumerate() {
            if known {
                self.bytes[offset + index] = value;
                self.mask[offset + index] = true;
            }
        }
        for reference in incoming {
            self.bytes[reference.offset..reference.offset + 4].fill(0);
            self.mask[reference.offset..reference.offset + 4].fill(false);
        }
        self.references = combined;
        Ok(())
    }
    fn write(&mut self, offset: usize, bytes: &[u8]) {
        // All callers are the fixed, verified field map below, not external data.
        self.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
        self.mask[offset..offset + bytes.len()].fill(true);
    }
    fn zero(&mut self, offset: usize, length: usize) {
        self.bytes[offset..offset + length].fill(0);
        self.mask[offset..offset + length].fill(true);
    }
    fn reference(&mut self, offset: usize, target: Reference) {
        self.references.push(ReferenceWrite { offset, target });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityLayout {
    Bytes88,
    Bytes92,
    Bytes148,
}
impl EntityLayout {
    pub fn bytes(self) -> usize {
        match self {
            Self::Bytes88 => 88,
            Self::Bytes92 => 92,
            Self::Bytes148 => 148,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EntityRecord {
    pub index: usize,
    pub layout: EntityLayout,
    /// Relative to the new allocation base, the loaded aircraft arena end.
    pub allocation_offset: usize,
    pub fields: InitializedFields,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allocation {
    pub offset: usize,
    pub length: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayState {
    pub flags: u32,
    /// Original display word+4.
    pub field_04: u32,
    /// Original display word+14.
    pub field_14: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectSlot {
    MissionRecord(RecordRef),
    /// This slot points at the array after the aircraft count, not one record.
    AircraftDirectory {
        section: usize,
        offset: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraSetup {
    pub function: u32,
    pub destination: Reference,
    pub projection_distance: i16,
    pub mission_parameter_0c: i16,
    pub mission_parameter_0e: i16,
    pub aircraft_parameters: Reference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredCall {
    /// Complete 354B8 before sampling PrefixInputs (only when context+3B0 is zero).
    LoadingPoll,
    DrawSync,
    /// Native allocation begins at the loaded aircraft end recorded at resource+5C.
    ResetArenaToLoadedEnd,
}

#[derive(Debug, Clone, Copy)]
pub struct PrefixInputs {
    /// Sample these after the conditional 354B8 prelude has completed.
    pub display_flags: u32,
    pub context_flags: u32,
    /// Context+3B4/+3B8 copied into resource state+70/+74.
    pub transition_words: [u32; 2],
    /// Resource state+44..+58 is copied unchanged. Its meaning is unresolved.
    pub trailing_resource_words: [u32; 6],
    /// Capacity from the loaded aircraft end to the host's checked arena limit.
    pub available_arena_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct InitializationPrefix {
    pub display: DisplayState,
    pub context_flags: u32,
    pub resource_transition_words: [u32; 2],
    pub gameplay_allocation: Allocation,
    pub gameplay: InitializedFields,
    pub entity_table_allocation: Allocation,
    pub entity_table: InitializedFields,
    /// The original BACBC pointer aliases entity-table slot32, not a new table.
    pub second_entity_table_first_slot: usize,
    pub entities: Vec<EntityRecord>,
    pub object_slots: Vec<ObjectSlot>,
    pub allocated_bytes: usize,
    /// External operations required before allocating this native prefix.
    pub required_calls: [RequiredCall; 2],
    /// 80032510 cannot continue or return a callback result until this is ported.
    pub pending_camera: CameraSetup,
}

pub fn prelude(context_field_3b0: i16) -> Option<RequiredCall> {
    (context_field_3b0 == 0).then_some(RequiredCall::LoadingPoll)
}

/// Translate 80032548..800327E0 using owned asset metadata and explicit context.
/// Original allocator failure would lead to invalid guest accesses; the host
/// rejects insufficient capacity before constructing any initialization state.
pub fn initialize_after_loading_poll(
    mission: &FlightMissionAssets,
    aircraft: &FlightAircraftAssets,
    input: PrefixInputs,
) -> Result<InitializationPrefix> {
    if input.available_arena_bytes < PREFIX_ALLOCATION_BYTES {
        return Err(Error::Invalid(
            "insufficient arena capacity for initialization prefix",
        ));
    }
    let mission_sections = mission.sections();
    let aircraft_sections = aircraft.sections();
    if mission_sections.len() != 11 || aircraft_sections.len() != 2 {
        return Err(Error::Invalid("asset section layout"));
    }
    if aircraft_sections[0].directory.is_none() || aircraft_sections[1].bytes().len() < 10 {
        return Err(Error::Invalid("aircraft directory or camera parameters"));
    }
    let mission_parameters = mission_sections[0].bytes();
    let parameter_0c = halfword(mission_parameters, 0x0c)?;
    let parameter_0e = halfword(mission_parameters, 0x0e)?;
    let mut object_slots: Vec<_> = mission
        .object_slots()
        .iter()
        .copied()
        .map(ObjectSlot::MissionRecord)
        .collect();
    let slot = object_slots
        .get_mut(3)
        .ok_or(Error::Invalid("missing mission object slot3"))?;
    *slot = ObjectSlot::AircraftDirectory {
        section: 0,
        offset: 4,
    };

    let mut gameplay = InitializedFields::new(GAMEPLAY_BYTES);
    gameplay.zero(0, 16);
    gameplay.zero(0x29c, 4);
    for axis in 0..3 {
        gameplay.write(0x2a0 + axis * 8, &0x80000001_u32.to_le_bytes());
        gameplay.write(0x2a4 + axis * 8, &0x7fffffff_u32.to_le_bytes());
    }
    // Resource words+18..+58 -> gameplay+244..+284. Section2 includes its
    // header; the other relocated directories point past their count words.
    for (section, offset) in [0, 0, 0, 4, 4, 4, 0, 0, 0, 4, 4].into_iter().enumerate() {
        gameplay.reference(
            0x244 + section * 4,
            Reference::MissionSection { section, offset },
        );
    }
    for (index, value) in input.trailing_resource_words.into_iter().enumerate() {
        gameplay.write(0x270 + index * 4, &value.to_le_bytes());
    }
    gameplay.reference(0x28, Reference::Gameplay { offset: 0xb8 });
    let mut table = InitializedFields::new(ENTITY_TABLE_BYTES);
    let mut offset = GAMEPLAY_BYTES + ENTITY_TABLE_BYTES;
    let entities: Vec<_> = (0..ENTITY_COUNT)
        .map(|index| {
            let layout = match index {
                0..=1 => EntityLayout::Bytes88,
                2..=31 => EntityLayout::Bytes92,
                _ => EntityLayout::Bytes148,
            };
            table.reference(index * 4, Reference::Entity { index, offset: 0 });
            let entity = initialize_entity(index, layout, offset);
            offset += layout.bytes();
            entity
        })
        .collect();
    debug_assert_eq!(offset, PREFIX_ALLOCATION_BYTES);
    Ok(InitializationPrefix {
        display: DisplayState {
            flags: input.display_flags | 5,
            field_04: 0,
            field_14: 2,
        },
        context_flags: input.context_flags | 0x10000000,
        resource_transition_words: input.transition_words,
        gameplay_allocation: Allocation {
            offset: 0,
            length: GAMEPLAY_BYTES,
        },
        gameplay,
        entity_table_allocation: Allocation {
            offset: GAMEPLAY_BYTES,
            length: ENTITY_TABLE_BYTES,
        },
        entity_table: table,
        second_entity_table_first_slot: 32,
        entities,
        object_slots,
        allocated_bytes: offset,
        required_calls: [RequiredCall::DrawSync, RequiredCall::ResetArenaToLoadedEnd],
        pending_camera: CameraSetup {
            function: 0x80076210,
            destination: Reference::Gameplay { offset: 0xb8 },
            projection_distance: 0x128,
            mission_parameter_0c: parameter_0c,
            mission_parameter_0e: parameter_0e,
            aircraft_parameters: Reference::AircraftSection {
                section: 1,
                offset: 4,
            },
        },
    })
}

fn initialize_entity(index: usize, layout: EntityLayout, allocation_offset: usize) -> EntityRecord {
    let mut fields = InitializedFields::new(layout.bytes());
    // The original's two64-iteration loops write these exact ranges. In
    // particular +14..+17, +1E..+1F, +2C..+2F, +36..+37 and +44..+47 survive.
    for (offset, length) in [
        (0, 20),
        (0x18, 6),
        (0x20, 12),
        (0x30, 6),
        (0x38, 12),
        (0x48, 16),
    ] {
        fields.zero(offset, length);
    }
    if index == 0 {
        fields.write(5, &[1]);
    }
    EntityRecord {
        index,
        layout,
        allocation_offset,
        fields,
    }
}

fn halfword(bytes: &[u8], offset: usize) -> Result<i16> {
    let data = bytes
        .get(offset..offset + 2)
        .ok_or(Error::Invalid("truncated mission parameter"))?;
    Ok(i16::from_le_bytes([data[0], data[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_initialization_preserves_unknown_ranges_for_all_three_sizes() {
        for (index, layout) in [
            (0, EntityLayout::Bytes88),
            (1, EntityLayout::Bytes88),
            (2, EntityLayout::Bytes92),
            (63, EntityLayout::Bytes148),
        ] {
            let entity = initialize_entity(index, layout, 956);
            assert_eq!(entity.fields.len(), layout.bytes());
            assert_eq!(entity.fields.scalar(5, 1).unwrap(), [u8::from(index == 0)]);
            for offset in [0x14, 0x1e, 0x2c, 0x36, 0x44] {
                assert!(matches!(
                    entity.fields.scalar(offset, 1),
                    Err(Error::Uninitialized { .. })
                ));
            }
            assert_eq!(
                entity
                    .fields
                    .scalar_mask()
                    .iter()
                    .filter(|&&known| known)
                    .count(),
                72
            );
            if layout.bytes() > 88 {
                assert!(entity.fields.scalar(88, 1).is_err());
            }
        }
    }

    #[test]
    fn pointer_fields_are_not_read_as_zero_scalars() {
        let mut fields = InitializedFields::new(8);
        fields.reference(0, Reference::Gameplay { offset: 0xb8 });
        fields.write(4, &123_u32.to_le_bytes());
        assert!(matches!(
            fields.scalar(0, 4),
            Err(Error::Uninitialized { .. })
        ));
        assert_eq!(fields.scalar(4, 4).unwrap(), 123_u32.to_le_bytes());
        assert!(fields.scalar(usize::MAX, 2).is_err());
        assert!(fields.scalar(8, 1).is_err());
    }

    #[test]
    fn conditional_loading_poll_uses_signed_halfword_equality() {
        assert_eq!(prelude(0), Some(RequiredCall::LoadingPoll));
        for value in [1, -1, i16::MIN, i16::MAX] {
            assert_eq!(prelude(value), None);
        }
    }

    #[test]
    fn sparse_callee_merge_preserves_unknowns_and_is_atomic_on_pointer_errors() {
        let mut fields = InitializedFields::new(20);
        fields.write(0, &[1, 2, 3, 4]);
        fields.reference(4, Reference::Gameplay { offset: 8 });
        let camera_target = ReferenceWrite {
            offset: 4,
            target: Reference::Entity {
                index: 0,
                offset: 8,
            },
        };
        fields
            .merge(
                8,
                &[9; 8],
                &[true, true, false, false, false, false, false, false],
                &[camera_target],
            )
            .unwrap();
        assert_eq!(fields.scalar(8, 2).unwrap(), [9, 9]);
        assert!(fields.scalar(10, 2).is_err());
        assert_eq!(fields.references()[1].offset, 12);
        let before = fields.scalar_bytes().to_vec();
        assert!(
            fields
                .merge(4, &[8; 4], &[true, false, false, false], &[])
                .is_err()
        );
        assert_eq!(fields.scalar_bytes(), before);
        assert!(
            fields
                .merge(
                    12,
                    &[8; 4],
                    &[true; 4],
                    &[ReferenceWrite {
                        offset: 0,
                        target: camera_target.target
                    }]
                )
                .is_err()
        );
        assert_eq!(fields.scalar_bytes(), before);
        fields.merge(4, &[0; 4], &[true; 4], &[]).unwrap();
        assert_eq!(fields.references().len(), 1);
        assert_eq!(fields.scalar(4, 4).unwrap(), [0; 4]);
    }
}
