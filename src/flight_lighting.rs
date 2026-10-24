//! Resident 80032510 lighting block, 80032878..80032990.
//! Asset-derived fields and fixed-point math only; no subsequent subsystem call.
use crate::flight_assets::FlightMissionAssets;
use ac3_graphics::transform::{SinCosTable, Transformed, rotate_translate};
use ac3_graphics::{Error, Result};

pub const GAMEPLAY_LIGHTING_OFFSET: usize = 0x6c;
pub const LIGHTING_FIELD_BYTES: usize = 0x4c;
pub const PENDING_SUBSYSTEM_CALL_PC: u32 = 0x80032990;
pub const PENDING_SUBSYSTEM_FUNCTION: u32 = 0x8004af9c;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LightingParameters {
    pub angles: [[i16; 3]; 2],
    /// Two RGB columns; the third matrix column is initialized to zero.
    pub colors: [[i16; 3]; 2],
    pub background: [i16; 3],
}
impl LightingParameters {
    pub fn from_assets(mission: &FlightMissionAssets) -> Result<Self> {
        let section = mission
            .sections()
            .iter()
            .find(|section| section.index == 0)
            .ok_or(Error::Invalid("mission lighting section 0"))?;
        Self::from_section_zero(section.bytes())
    }
    pub fn from_section_zero(bytes: &[u8]) -> Result<Self> {
        let bytes = bytes
            .get(..0x56)
            .ok_or(Error::Truncated("mission lighting parameters"))?;
        let half = |offset| i16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        Ok(Self {
            angles: [[half(0x3c), half(0x3e), 0], [half(0x4c), half(0x4e), 0]],
            colors: [
                [half(0x40), half(0x42), half(0x44)],
                [half(0x50), half(0x52), half(0x54)],
            ],
            background: [half(0x46), half(0x48), half(0x4a)],
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightingFields {
    bytes: [u8; LIGHTING_FIELD_BYTES],
    mask: [bool; LIGHTING_FIELD_BYTES],
}
impl LightingFields {
    pub fn scalar_bytes(&self) -> &[u8; LIGHTING_FIELD_BYTES] {
        &self.bytes
    }
    pub fn scalar_mask(&self) -> &[bool; LIGHTING_FIELD_BYTES] {
        &self.mask
    }
    fn write(&mut self, offset: usize, bytes: &[u8]) {
        self.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
        self.mask[offset..offset + bytes.len()].fill(true);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectionSetup {
    /// Matrix loaded into GTE rotation controls by ApplyMatrixSV.
    pub rotation: [[i16; 3]; 3],
    /// ApplyMatrixSV stores saturated IR values; MAC and flags remain observable.
    pub transformed: Transformed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightingSetup {
    pub fields: LightingFields,
    pub directions: [[i16; 3]; 3],
    pub colors: [[i16; 3]; 3],
    pub background: [i32; 3],
    /// Original call order. Last entry supplies the final GTE rotation/IR/MAC.
    pub calls: [DirectionSetup; 2],
}

/// Complete RotMatrixYXZ at 800A5BE8, including negative-angle branch tails.
/// Products shift before addition/subtraction; each final entry is a halfword.
pub fn rotation_yxz(trig: &SinCosTable, angles: [i16; 3]) -> [[i16; 3]; 3] {
    let [sx, cx] = trig.sin_cos(angles[0]);
    let [sy, cy] = trig.sin_cos(angles[1]);
    let [sz, cz] = trig.sin_cos(angles[2]);
    let mul = |a: i32, b: i32| a.wrapping_mul(b) >> 12;
    let sysx = mul(sy, sx);
    let cysx = mul(cy, sx);
    [
        [
            mul(cy, cz).wrapping_add(mul(sysx, sz)),
            mul(sysx, cz).wrapping_sub(mul(cy, sz)),
            mul(sy, cx),
        ],
        [mul(sz, cx), mul(cz, cx), sx.wrapping_neg()],
        [
            mul(cysx, sz).wrapping_sub(mul(sy, cz)),
            mul(sy, sz).wrapping_add(mul(cysx, cz)),
            mul(cy, cx),
        ],
    ]
    .map(|row| row.map(|value| value as i16))
}

/// 800A5348 RTV0: rotation-only MVMVA with shift12 and signed IR saturation.
/// Matrix translation and vector/matrix padding are not inputs to the command.
pub fn apply_matrix_sv(matrix: [[i16; 3]; 3], vector: [i16; 3]) -> Transformed {
    rotate_translate(matrix, [0; 3], vector)
}

pub fn initialize_lighting(parameters: LightingParameters, trig: &SinCosTable) -> LightingSetup {
    let calls = parameters.angles.map(|angles| {
        let rotation = rotation_yxz(trig, angles);
        DirectionSetup {
            rotation,
            transformed: apply_matrix_sv(rotation, [0, 0, 4096]),
        }
    });
    let directions = [
        calls[0].transformed.saturated,
        calls[1].transformed.saturated,
        [0; 3],
    ];
    let colors =
        std::array::from_fn(|row| [parameters.colors[0][row], parameters.colors[1][row], 0]);
    let background = parameters.background.map(i32::from);
    let mut fields = LightingFields {
        bytes: [0; LIGHTING_FIELD_BYTES],
        mask: [false; LIGHTING_FIELD_BYTES],
    };
    for row in 0..3 {
        for column in 0..3 {
            fields.write(
                (row * 3 + column) * 2,
                &directions[row][column].to_le_bytes(),
            );
            fields.write(
                0x20 + (row * 3 + column) * 2,
                &colors[row][column].to_le_bytes(),
            );
        }
        fields.write(0x40 + row * 4, &background[row].to_le_bytes());
    }
    LightingSetup {
        fields,
        directions,
        colors,
        background,
        calls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn axis_trig() -> SinCosTable {
        // Exact integer axis cases only; no host trigonometry approximations.
        let mut bytes = vec![0; 4096 * 4];
        for (index, pair) in [
            (0, [0_i16, 4096]),
            (1024, [4096, 0]),
            (2048, [0, -4096]),
            (3072, [-4096, 0]),
        ] {
            bytes[index * 4..index * 4 + 2].copy_from_slice(&pair[0].to_le_bytes());
            bytes[index * 4 + 2..index * 4 + 4].copy_from_slice(&pair[1].to_le_bytes());
        }
        SinCosTable::from_le_bytes(&bytes).unwrap()
    }
    #[test]
    fn yxz_order_and_signed_angles() {
        let trig = axis_trig();
        assert_eq!(
            rotation_yxz(&trig, [0; 3]),
            [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]]
        );
        assert_eq!(
            apply_matrix_sv(rotation_yxz(&trig, [1024, 1024, 0]), [0, 0, 4096]).saturated,
            [0, -4096, 0]
        );
        assert_eq!(
            apply_matrix_sv(rotation_yxz(&trig, [-1024, 1024, 0]), [0, 0, 4096]).saturated,
            [0, 4096, 0]
        );
        assert_eq!(
            rotation_yxz(&trig, [i16::MIN, 0, 0]),
            rotation_yxz(&trig, [0; 3])
        );
    }
    #[test]
    fn apply_matrix_saturates_ir_but_retains_mac_and_flags() {
        let positive = apply_matrix_sv([[32767; 3]; 3], [32767; 3]);
        assert_eq!(positive.saturated, [32767; 3]);
        assert_eq!(positive.vector, [786384; 3]);
        assert_eq!(positive.flags, 0x81c00000);
        let negative = apply_matrix_sv([[32767; 3]; 3], [-32768; 3]);
        assert_eq!(negative.saturated, [-32768; 3]);
        assert_eq!(negative.vector, [-786408; 3]);
        assert_eq!(negative.flags, 0x81c00000);
    }
    #[test]
    fn fields_keep_matrix_padding_and_background_signs() {
        let setup = initialize_lighting(
            LightingParameters {
                angles: [[0; 3]; 2],
                colors: [[1, 2, 3], [-1, -2, -3]],
                background: [-32768, 0, 32767],
            },
            &axis_trig(),
        );
        assert_eq!(setup.colors, [[1, -1, 0], [2, -2, 0], [3, -3, 0]]);
        assert_eq!(setup.background, [-32768, 0, 32767]);
        assert_eq!(setup.fields.mask.iter().filter(|&&value| value).count(), 48);
        assert!(setup.fields.mask[0x12..0x20].iter().all(|&value| !value));
        assert!(setup.fields.mask[0x32..0x40].iter().all(|&value| !value));
    }
    #[test]
    fn truncated_asset_parameters_are_rejected() {
        assert!(LightingParameters::from_section_zero(&[0; 0x55]).is_err());
    }
}
