//! Resident 80076210 camera initialization and immediate projection/depth setup.
//!
//! Scalar masks describe original stores, not allocator contents. The one camera
//! pointer is a typed entity reference. This stops at 80032878, before lighting.
use crate::flight_assets::{FlightAircraftAssets, FlightMissionAssets};
use std::fmt;

pub const CAMERA_BYTES: usize = 0x160;
pub const GAMEPLAY_CAMERA_OFFSET: usize = 0xb8;
pub const DEPTH_ALLOCATION_BYTES: usize = 36;
pub const GAMEPLAY_DEPTH_OFFSET: usize = 0x288;
pub const PENDING_LIGHTING_PC: u32 = 0x80032878;
/// Original ordered stores between camera return and projection setup.
pub const POST_CAMERA_ZERO_WORDS: [usize; 6] = [0x48, 0x44, 0x40, 0x68, 0x64, 0x60];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    MissingAssetField(&'static str),
    Division { numerator: i64, denominator: i64 },
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAssetField(name) => write!(f, "missing flight camera asset field: {name}"),
            Self::Division {
                numerator,
                denominator,
            } => {
                write!(f, "invalid camera depth division {numerator}/{denominator}")
            }
        }
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CameraParameters {
    pub projection_distance: i16,
    pub near: i16,
    pub far: i16,
    pub aircraft_offsets: [i16; 3],
    /// Context byte +3A3: zero selects mode 1, any other value selects mode 2.
    pub mode_byte: u8,
    /// The first entity's original flags; camera indices use its low six bits.
    pub entity_flags: u32,
}
impl CameraParameters {
    /// Arguments supplied by 80032510, resolved from native decoded sections.
    pub fn from_assets(
        mission: &FlightMissionAssets,
        aircraft: &FlightAircraftAssets,
        mode_byte: u8,
        entity_flags: u32,
    ) -> Result<Self> {
        let m = mission
            .sections()
            .iter()
            .find(|s| s.index == 0)
            .ok_or(Error::MissingAssetField("mission section 0"))?
            .bytes();
        let a = aircraft
            .sections()
            .iter()
            .find(|s| s.index == 1)
            .ok_or(Error::MissingAssetField("aircraft section 1"))?
            .bytes();
        let half = |bytes: &[u8], offset, name| -> Result<i16> {
            Ok(i16::from_le_bytes(
                bytes
                    .get(offset..offset + 2)
                    .ok_or(Error::MissingAssetField(name))?
                    .try_into()
                    .unwrap(),
            ))
        };
        Ok(Self {
            projection_distance: 0x128,
            near: half(m, 0xc, "mission near")?,
            far: half(m, 0xe, "mission far")?,
            aircraft_offsets: [
                half(a, 4, "aircraft x")?,
                half(a, 6, "aircraft y")?,
                half(a, 8, "aircraft z")?,
            ],
            mode_byte,
            entity_flags,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CameraTarget {
    pub field_offset: usize,
    pub entity_index: usize,
    pub position_offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraFields {
    bytes: [u8; CAMERA_BYTES],
    mask: [bool; CAMERA_BYTES],
    pub target: CameraTarget,
}
impl CameraFields {
    pub fn scalar_bytes(&self) -> &[u8; CAMERA_BYTES] {
        &self.bytes
    }
    pub fn scalar_mask(&self) -> &[bool; CAMERA_BYTES] {
        &self.mask
    }
    fn write(&mut self, offset: usize, bytes: &[u8]) {
        self.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
        self.mask[offset..offset + bytes.len()].fill(true);
    }
    fn word(&mut self, offset: usize, value: u32) {
        self.write(offset, &value.to_le_bytes());
    }
    fn half(&mut self, offset: usize, value: i16) {
        self.write(offset, &value.to_le_bytes());
    }
}

/// Complete 80076210, including the mode-1/mode-2 paths through 80077D54 and
/// 800761FC. This does not generalize those callees to unrelated camera modes.
pub fn initialize_camera(parameters: CameraParameters) -> CameraFields {
    let mut fields = CameraFields {
        bytes: [0; CAMERA_BYTES],
        mask: [false; CAMERA_BYTES],
        target: CameraTarget {
            field_offset: 0x34,
            entity_index: 0,
            position_offset: 8,
        },
    };
    let mode = if parameters.mode_byte == 0 { 1 } else { 2 };
    let entity = parameters.entity_flags & 0x3f;
    fields.word(0, 0x500 | entity);
    fields.word(8, 0xffb50000);
    for offset in [
        4, 0xc, 0x38, 0x3c, 0x40, 0xcc, 0xd0, 0xdc, 0xe0, 0xe4, 0xec, 0xf0, 0x10c, 0x110, 0x114,
        0x124, 0x128, 0x12c, 0x134, 0x138, 0x13c, 0x14c, 0x150, 0x154,
    ] {
        fields.word(offset, 0);
    }
    for offset in [
        0x14, 0x16, 0x18, 0x48, 0x4a, 0x4c, 0xb4, 0xb6, 0xb8, 0xd4, 0xd6, 0xd8, 0x11c, 0x11e,
        0x120, 0x144, 0x146, 0x148, 0x15c, 0x15e,
    ] {
        fields.half(offset, 0);
    }
    for (offset, value) in [
        (0x1c, -160),
        (0x1e, -128),
        (0x20, 320),
        (0x22, 256),
        (0x24, parameters.projection_distance),
        (0x28, parameters.near),
        (0x2a, parameters.far),
    ] {
        fields.half(offset, value);
    }
    for (index, value) in parameters.aircraft_offsets.into_iter().enumerate() {
        fields.half(0x2c + index * 2, value.wrapping_shl(6));
    }
    fields.word(0x50, mode);
    fields.word(0xbc, u32::MAX);
    fields.word(0xc0, u32::MAX);
    fields.word(0xc4, if mode == 1 { entity } else { u32::MAX });
    fields.word(0xc8, entity);
    fields
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepthCue {
    pub dqa: i16,
    pub dqb: i32,
}

fn divide(numerator: i64, denominator: i64) -> Result<i64> {
    numerator.checked_div(denominator).ok_or(Error::Division {
        numerator,
        denominator,
    })
}

/// 8005D0C4: signed 64-bit fixed-point divisions, low-word DQA range test and
/// asymmetric DQB saturation. Zero/overflow divisions are rejected explicitly.
pub fn depth_cue(near: i32, far: i32, projection: i32) -> Result<DepthCue> {
    let first = divide(
        i64::from(near.wrapping_sub(far)) * 0x1000000,
        i64::from(near),
    )?;
    let slope = divide(first.wrapping_mul(i64::from(projection)), i64::from(far))?;
    let raw = divide(0x100000000, slope)? as i32;
    let clamped = (raw as u32).wrapping_add(0x8000) > 0xffff;
    let dqa = if clamped {
        if raw > 0 { i16::MAX } else { i16::MIN }
    } else {
        raw as i16
    };
    let near_term = divide(
        ((-i64::from(dqa)) << 16).wrapping_mul(i64::from(projection)),
        i64::from(near),
    )?;
    let dqb = if clamped {
        0x1000000_i64.wrapping_sub(divide(
            (i64::from(dqa) << 16).wrapping_mul(i64::from(projection)),
            i64::from(far),
        )?)
    } else {
        near_term
    };
    Ok(DepthCue {
        dqa,
        dqb: dqb.clamp(-0x7fffffff, 0x7fffffff) as i32,
    })
}

/// 8005D390 writes 17 halfwords. The 36-byte allocation's last two bytes are
/// untouched. Signed multiply/add wrap before shifts and range saturation.
pub fn depth_buckets(near: i32, far: i32) -> [i16; 17] {
    let mut table = [0; 17];
    for (index, value) in table[..16].iter_mut().enumerate() {
        let product = (index as i32 + 1).wrapping_mul(far.wrapping_sub(near));
        let rounded = if product < 0 {
            product.wrapping_add(15)
        } else {
            product
        };
        *value = (((rounded >> 4).wrapping_add(near)) >> 3).clamp(0, 4093) as i16;
    }
    table[16] = 4094;
    table
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometrySetup {
    /// GTE OFX/OFY signed 16.16 register values.
    pub offset: [i32; 2],
    pub screen: u16,
    pub dqa: i16,
    pub dqb: i32,
}
pub fn geometry_setup(parameters: CameraParameters) -> Result<GeometrySetup> {
    let cue = depth_cue(
        i32::from(parameters.near),
        i32::from(parameters.far),
        i32::from(parameters.projection_distance),
    )?;
    Ok(GeometrySetup {
        offset: [0, 0],
        screen: parameters.projection_distance as u16,
        dqa: cue.dqa,
        dqb: cue.dqb,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parameters() -> CameraParameters {
        CameraParameters {
            projection_distance: 296,
            near: 16395,
            far: 28000,
            aircraft_offsets: [0, -20, -88],
            mode_byte: 0,
            entity_flags: 0,
        }
    }
    fn word(fields: &CameraFields, offset: usize) -> u32 {
        u32::from_le_bytes(fields.bytes[offset..offset + 4].try_into().unwrap())
    }
    #[test]
    fn camera_modes_preserve_holes_and_use_low_entity_bits() {
        let mut p = parameters();
        p.entity_flags = 0x123456ab;
        let first = initialize_camera(p);
        assert_eq!(word(&first, 0), 0x52b);
        assert_eq!(word(&first, 0xc4), 0x2b);
        assert!(!first.mask[0x10]);
        assert!(!first.mask[0x34]);
        assert!(!first.mask[0x54]);
        assert_eq!(
            first.target,
            CameraTarget {
                field_offset: 0x34,
                entity_index: 0,
                position_offset: 8
            }
        );
        p.mode_byte = 255;
        let second = initialize_camera(p);
        assert_eq!(word(&second, 0x50), 2);
        assert_eq!(word(&second, 0xc4), u32::MAX);
        assert_eq!(word(&second, 0xc8), 0x2b);
    }
    #[test]
    fn aircraft_shift_wraps_at_halfword_width() {
        let mut p = parameters();
        p.aircraft_offsets = [i16::MAX, i16::MIN, 1025];
        let f = initialize_camera(p);
        assert_eq!(&f.bytes[0x2c..0x32], &[0xc0, 0xff, 0, 0, 0x40, 0]);
    }
    #[test]
    fn depth_buckets_round_toward_zero_then_shift_arithmetically() {
        assert_eq!(
            depth_buckets(8, -8),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4094]
        );
        assert_eq!(
            depth_buckets(32760, 32767),
            [
                4093, 4093, 4093, 4093, 4093, 4093, 4093, 4093, 4093, 4093, 4093, 4093, 4093, 4093,
                4093, 4093, 4094
            ]
        );
        assert_eq!(depth_buckets(100, 84)[0], 12);
        assert_eq!(depth_buckets(100, 84)[15], 10);
        assert_eq!(depth_buckets(i32::MAX, i32::MIN)[0], 4093);
        assert_eq!(depth_buckets(i32::MAX, i32::MIN)[15], 0);
    }
    #[test]
    fn invalid_depth_divisors_are_explicit() {
        for (near, far, screen) in [(0, 100, 296), (100, 0, 296), (100, 100, 296), (100, 200, 0)] {
            assert!(matches!(
                depth_cue(near, far, screen),
                Err(Error::Division { .. })
            ));
        }
    }
    #[test]
    fn depth_saturation_retains_original_asymmetric_minimum() {
        assert_eq!(depth_cue(16395, 28000, 296).unwrap().dqa, -32768);
        assert_eq!(depth_cue(1000, 999, 1_000_000).unwrap().dqb, -2147483647);
        assert_eq!(depth_cue(1000, 1001, 1_000_000).unwrap().dqb, 2147483647);
    }
}
