//! Native progression through resident 80032510, composed from verified slices.
//!
//! Order: allocation prefix (32548..327E0), camera 76210, six zero words,
//! geometry offset/screen and depth cue, the 36-byte depth allocation
//! (5D390), then lighting (32878..32990). It stops at the explicit call to
//! 8004AF9C. Typed references stay relative; nothing here is a guest address.
use crate::{
    Result,
    flight_assets::{FlightAircraftAssets, FlightMissionAssets},
    flight_camera::{
        self, CameraParameters, DEPTH_ALLOCATION_BYTES, GAMEPLAY_CAMERA_OFFSET,
        GAMEPLAY_DEPTH_OFFSET, GeometrySetup, POST_CAMERA_ZERO_WORDS,
    },
    flight_initialization::{
        self as init, Allocation, InitializationPrefix, InitializedFields, PrefixInputs, Reference,
        ReferenceWrite,
    },
    flight_lighting::{self, GAMEPLAY_LIGHTING_OFFSET, LightingParameters, LightingSetup},
};
use ac3_graphics::transform::SinCosTable;

pub const PENDING_WORLD_SETUP: u32 = flight_lighting::PENDING_SUBSYSTEM_FUNCTION;

/// GTE controls written by this span. Translation and light/color matrices
/// are not written by it and remain whatever the GTE previously held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GteControls {
    pub rotation: [[i16; 3]; 3],
    pub geometry: GeometrySetup,
}

#[derive(Clone, Debug)]
pub struct GameplayProgress {
    /// Gameplay block now also holds camera, zero words, depth and lighting.
    pub prefix: InitializationPrefix,
    pub camera: CameraParameters,
    pub depth_allocation: Allocation,
    pub depth: InitializedFields,
    pub lighting: LightingSetup,
    pub gte: GteControls,
    /// Bytes allocated from the arena base after this span.
    pub allocated_bytes: usize,
    pub pending_call: u32,
}

/// `camera_mode_byte` is context +0x3A3. Entity 0's flag word comes from the
/// prefix's own zero writes, as in the original read after allocation.
pub fn initialize_through_lighting(
    mission: &FlightMissionAssets,
    aircraft: &FlightAircraftAssets,
    inputs: PrefixInputs,
    camera_mode_byte: u8,
    trig: &SinCosTable,
) -> Result<GameplayProgress> {
    if inputs.available_arena_bytes < init::PREFIX_ALLOCATION_BYTES + DEPTH_ALLOCATION_BYTES {
        return Err("insufficient arena capacity for gameplay initialization".into());
    }
    let mut prefix = init::initialize_after_loading_poll(mission, aircraft, inputs)?;
    let entity_flags = u32::from_le_bytes(prefix.entities[0].fields.scalar(0, 4)?.try_into()?);
    let camera = CameraParameters::from_assets(mission, aircraft, camera_mode_byte, entity_flags)?;
    if (
        prefix.pending_camera.mission_parameter_0c,
        prefix.pending_camera.mission_parameter_0e,
    ) != (camera.near, camera.far)
    {
        return Err("camera arguments disagree with the allocation prefix".into());
    }
    let fields = flight_camera::initialize_camera(camera);
    let target = fields.target;
    prefix.gameplay.merge(
        GAMEPLAY_CAMERA_OFFSET,
        fields.scalar_bytes(),
        fields.scalar_mask(),
        &[ReferenceWrite {
            offset: target.field_offset,
            target: Reference::Entity {
                index: target.entity_index,
                offset: target.position_offset,
            },
        }],
    )?;
    for offset in POST_CAMERA_ZERO_WORDS {
        prefix.gameplay.merge(offset, &[0; 4], &[true; 4], &[])?;
    }
    let geometry = flight_camera::geometry_setup(camera)?;

    // 80026F70 rounds to words; 36 bytes follow the prefix allocation.
    let depth_allocation = Allocation {
        offset: prefix.allocated_bytes,
        length: DEPTH_ALLOCATION_BYTES.div_ceil(4) * 4,
    };
    prefix.gameplay.merge(
        GAMEPLAY_DEPTH_OFFSET,
        &[0; 4],
        &[false; 4],
        &[ReferenceWrite {
            offset: 0,
            target: Reference::Arena {
                offset: depth_allocation.offset,
            },
        }],
    )?;
    let buckets = flight_camera::depth_buckets(i32::from(camera.near), i32::from(camera.far));
    // Seventeen halfwords; the allocation's final two bytes stay unknown.
    let mut bytes = [0; DEPTH_ALLOCATION_BYTES];
    let mut mask = [false; DEPTH_ALLOCATION_BYTES];
    for (index, value) in buckets.into_iter().enumerate() {
        bytes[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
        mask[index * 2..index * 2 + 2].fill(true);
    }
    let mut depth = InitializedFields::new(DEPTH_ALLOCATION_BYTES);
    depth.merge(0, &bytes, &mask, &[])?;

    let lighting =
        flight_lighting::initialize_lighting(LightingParameters::from_assets(mission)?, trig);
    prefix.gameplay.merge(
        GAMEPLAY_LIGHTING_OFFSET,
        lighting.fields.scalar_bytes(),
        lighting.fields.scalar_mask(),
        &[],
    )?;
    let gte = GteControls {
        rotation: lighting.calls[1].rotation,
        geometry,
    };
    Ok(GameplayProgress {
        allocated_bytes: depth_allocation.offset + depth_allocation.length,
        prefix,
        camera,
        depth_allocation,
        depth,
        lighting,
        gte,
        pending_call: PENDING_WORLD_SETUP,
    })
}
