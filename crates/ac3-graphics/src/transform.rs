//! Fixed point translations of resident RotMatrix (800A5958) and
//! RotTrans (800A58B8). These are native math functions, not a CPU interpreter.
//! The trigonometry table is loaded from the user's original executable.
use crate::{Error, Result};

#[derive(Clone)]
pub struct SinCosTable {
    values: Vec<[i16; 2]>,
}

impl SinCosTable {
    pub fn from_le_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 4096 * 4 {
            return Err(Error::Invalid("sine/cosine table length"));
        }
        Ok(Self {
            values: bytes
                .chunks_exact(4)
                .map(|v| {
                    [
                        i16::from_le_bytes([v[0], v[1]]),
                        i16::from_le_bytes([v[2], v[3]]),
                    ]
                })
                .collect(),
        })
    }

    /// 4096 angle units per turn. Sign handling precedes table wrapping,
    /// including the signed16 minimum, as in the original MIPS branches.
    pub fn sin_cos(&self, angle: i16) -> [i32; 2] {
        let [s, c] = self.values[(i32::from(angle).unsigned_abs() & 4095) as usize];
        [
            if angle < 0 {
                -i32::from(s)
            } else {
                i32::from(s)
            },
            i32::from(c),
        ]
    }

    pub fn rotation(&self, angles: [i16; 3]) -> [[i16; 3]; 3] {
        let [sx, cx] = self.sin_cos(angles[0]);
        let [sy, cy] = self.sin_cos(angles[1]);
        let [sz, cz] = self.sin_cos(angles[2]);
        // Negation before the arithmetic shift is observably different from
        // negating a rounded product. Keep the original operation order.
        let mul = |a: i32, b: i32| a.wrapping_mul(b) >> 12;
        let y_z = mul(cz, -sy);
        let y_s = mul(sz, -sy);
        [
            [mul(cz, cy) as i16, mul(-sz, cy) as i16, sy as i16],
            [
                (mul(sz, cx) - mul(y_z, sx)) as i16,
                (mul(cz, cx) + mul(y_s, sx)) as i16,
                mul(-cy, sx) as i16,
            ],
            [
                (mul(sz, sx) + mul(y_z, cx)) as i16,
                (mul(cz, sx) - mul(y_s, cx)) as i16,
                mul(cy, cx) as i16,
            ],
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transformed {
    /// RotTrans stores the signed32 MAC results, not the saturated IR values.
    pub vector: [i32; 3],
    pub saturated: [i16; 3],
    pub flags: u32,
}

/// The original routine uses MVMVA with rotation, translation, shift12 and
/// signed IR saturation. Overflow is tracked after every 44-bit addition.
pub fn rotate_translate(
    matrix: [[i16; 3]; 3],
    translation: [i32; 3],
    vector: [i16; 3],
) -> Transformed {
    let mut result = Transformed {
        vector: [0; 3],
        saturated: [0; 3],
        flags: 0,
    };
    for row in 0..3 {
        let mut accumulator = i64::from(translation[row]) << 12;
        for (&m, &v) in matrix[row].iter().zip(&vector) {
            let sum = accumulator + i64::from(m) * i64::from(v);
            if sum > (1_i64 << 43) - 1 {
                result.flags |= 1 << (30 - row);
            }
            if sum < -(1_i64 << 43) {
                result.flags |= 1 << (27 - row);
            }
            accumulator = (sum << 20) >> 20;
        }
        let value = (accumulator >> 12) as i32;
        result.vector[row] = value;
        if !(-32768..=32767).contains(&value) {
            result.flags |= 1 << (24 - row);
        }
        result.saturated[row] = value.clamp(-32768, 32767) as i16;
    }
    if result.flags & 0x7f87e000 != 0 {
        result.flags |= 0x80000000;
    }
    result
}
