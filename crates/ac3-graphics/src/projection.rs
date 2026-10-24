//! Native fixed point perspective operations used by the resident SDK leaves
//! RotTransPers (800A5828) and RotTransPers3 (800A5858): shift12, signed limits.
//! No instruction dispatch, timing, register bank or floating point is involved.
use crate::transform::rotate_translate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectionControls {
    /// Original OFX/OFY, in signed16.16 screen coordinates.
    pub offset: [i32; 2],
    pub distance: u16,
    pub depth_scale: i16,
    pub depth_offset: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Projection<const N: usize> {
    pub screen: [[i16; 2]; N],
    pub depth: [u16; N],
    pub last_vector: [i32; 3],
    pub last_ir: [i16; 3],
    pub ir0: u16,
    pub mac0: i32,
    pub flags: u32,
}

pub fn project(
    matrix: [[i16; 3]; 3],
    translation: [i32; 3],
    controls: ProjectionControls,
    vector: [i16; 3],
) -> Projection<1> {
    project_inner(matrix, translation, controls, [vector])
}

pub fn project_three(
    matrix: [[i16; 3]; 3],
    translation: [i32; 3],
    controls: ProjectionControls,
    vectors: [[i16; 3]; 3],
) -> Projection<3> {
    project_inner(matrix, translation, controls, vectors)
}

fn project_inner<const N: usize>(
    matrix: [[i16; 3]; 3],
    translation: [i32; 3],
    controls: ProjectionControls,
    vectors: [[i16; 3]; N],
) -> Projection<N> {
    let mut output = Projection {
        screen: [[0; 2]; N],
        depth: [0; N],
        last_vector: [0; 3],
        last_ir: [0; 3],
        ir0: 0,
        mac0: 0,
        flags: 0,
    };
    for (i, vector) in vectors.into_iter().enumerate() {
        let transformed = rotate_translate(matrix, translation, vector);
        output.flags |= transformed.flags;
        output.last_vector = transformed.vector;
        output.last_ir = transformed.saturated;
        let z = transformed.vector[2];
        if !(0..=65535).contains(&z) {
            output.flags |= 1 << 18;
        }
        output.depth[i] = z.clamp(0, 65535) as u16;
        let ratio = reciprocal_ratio(controls.distance, output.depth[i], &mut output.flags);
        for axis in 0..2 {
            let value = i64::from(controls.offset[axis])
                + i64::from(transformed.saturated[axis]) * i64::from(ratio);
            mac0_flags(value, &mut output.flags);
            let pixel = value >> 16;
            if !(-1024..=1023).contains(&pixel) {
                output.flags |= 1 << (14 - axis);
            }
            output.screen[i][axis] = pixel.clamp(-1024, 1023) as i16;
        }
        // RTPT accumulates flags from all three vertices but computes the depth
        // cue only for the last. Earlier depth-cue saturation must not leak in.
        if i + 1 == N {
            let value = i64::from(controls.depth_offset)
                + i64::from(controls.depth_scale) * i64::from(ratio);
            mac0_flags(value, &mut output.flags);
            output.mac0 = value as i32;
            let depth_cue = value >> 12;
            if !(0..=4096).contains(&depth_cue) {
                output.flags |= 1 << 12;
            }
            output.ir0 = depth_cue.clamp(0, 4096) as u16;
        }
    }
    if output.flags & 0x7f87e000 != 0 {
        output.flags |= 0x80000000;
    }
    output
}

fn mac0_flags(value: i64, flags: &mut u32) {
    if value > i64::from(i32::MAX) {
        *flags |= 1 << 16;
    }
    if value < i64::from(i32::MIN) {
        *flags |= 1 << 15;
    }
}

/// GTE's normalized reciprocal approximation, including its overflow boundary.
/// The 257-entry numerical UNR table is evaluated at the selected index.
fn reciprocal_ratio(numerator: u16, denominator: u16, flags: &mut u32) -> u32 {
    if u32::from(numerator) >= u32::from(denominator) * 2 {
        *flags |= 1 << 17;
        return 0x1ffff;
    }
    let shift = denominator.leading_zeros();
    let normalized = u32::from(denominator) << shift;
    let index = (normalized - 0x8000 + 0x40) >> 7;
    let table_value = ((0x40000 / (index + 0x100)).div_ceil(2) as i32 - 0x101).max(0) as u32;
    let estimate = table_value + 0x101;
    let correction = ((0x80 - i64::from(estimate) * i64::from(normalized)) >> 8) & 0x1ffff;
    let reciprocal = (i64::from(estimate) * correction + 0x80) >> 8;
    (((reciprocal * i64::from(u32::from(numerator) << shift) + 0x8000) >> 16) as u32).min(0x1ffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    const IDENTITY: [[i16; 3]; 3] = [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]];
    const CONTROLS: ProjectionControls = ProjectionControls {
        offset: [160 << 16, 240 << 16],
        distance: 250,
        depth_scale: 0,
        depth_offset: 0,
    };

    #[test]
    fn projected_coordinates_use_fixed_point_camera_and_saturate() {
        let result = project(IDENTITY, [0; 3], CONTROLS, [100, -200, 1000]);
        assert_eq!(result.screen, [[185, 190]]);
        assert_eq!(result.depth, [1000]);
        assert_eq!(result.flags, 0);
        let behind = project(IDENTITY, [0; 3], CONTROLS, [32767, -32768, -1]);
        assert_eq!(behind.screen, [[1023, -1024]]);
        assert_eq!(behind.depth, [0]);
        assert_eq!(behind.flags, 0x8007e000);
    }

    #[test]
    fn triplet_depth_cue_uses_only_last_vertex() {
        let controls = ProjectionControls {
            distance: 1,
            depth_scale: 32767,
            ..CONTROLS
        };
        let result = project_three(
            IDENTITY,
            [0; 3],
            controls,
            [[0, 0, 1], [0, 0, 1], [0, 0, 32767]],
        );
        assert_eq!(result.ir0, 15);
        assert_eq!(result.flags, 0);
        assert!(project(IDENTITY, [0; 3], controls, [0, 0, 1]).flags & (1 << 12) != 0);
    }
}
