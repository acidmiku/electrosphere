//! Original PsyQ scalar math from the resident PAL executable.
//!
//! Angles use 4096 units per turn. SquareRoot0 deliberately preserves the
//! original lookup approximation rather than substituting an integer square root.
use crate::{Error, Result};

pub const ATAN_TABLE_ADDRESS: u32 = 0x800b8f84;
pub const SQRT_TABLE_ADDRESS: u32 = 0x800b4c54;
pub const SINE_TABLE_ADDRESS: u32 = 0x800b4444;

#[derive(Clone)]
pub struct ScalarMath {
    atan: Vec<i16>,
    sqrt: Vec<i16>,
    sine: Vec<i16>,
}

impl ScalarMath {
    /// Supply the original table bytes; this crate does not embed game data.
    pub fn from_le_bytes(atan: &[u8], sqrt: &[u8], sine: &[u8]) -> Result<Self> {
        fn table(bytes: &[u8], entries: usize, name: &'static str) -> Result<Vec<i16>> {
            if bytes.len() != entries * 2 {
                return Err(Error::Invalid(name));
            }
            Ok(bytes
                .chunks_exact(2)
                .map(|v| i16::from_le_bytes([v[0], v[1]]))
                .collect())
        }
        Ok(Self {
            atan: table(atan, 1025, "atan table length")?,
            sqrt: table(sqrt, 192, "square-root table length")?,
            sine: table(sine, 1025, "quarter-wave sine table length")?,
        })
    }

    /// Read tables from an executable payload or RAM image with its load address.
    pub fn from_resident(bytes: &[u8], load_address: u32) -> Result<Self> {
        fn range(bytes: &[u8], base: u32, address: u32, size: usize) -> Result<&[u8]> {
            let offset = address
                .checked_sub(base)
                .ok_or(Error::Invalid("scalar table before resident load address"))?
                as usize;
            bytes
                .get(offset..offset + size)
                .ok_or(Error::Truncated("resident scalar table"))
        }
        Self::from_le_bytes(
            range(bytes, load_address, ATAN_TABLE_ADDRESS, 1025 * 2)?,
            range(bytes, load_address, SQRT_TABLE_ADDRESS, 192 * 2)?,
            range(bytes, load_address, SINE_TABLE_ADDRESS, 1025 * 2)?,
        )
    }

    /// Translation of ratan2 at 0x800A6088, including its two integer-ratio paths.
    /// INT_MIN is rejected: its original wrapping absolute value can lead to a
    /// BREAK instruction or an out-of-domain lookup, depending on the other axis.
    pub fn atan2(&self, y: i32, x: i32) -> Result<i32> {
        let ax = x
            .checked_abs()
            .ok_or(Error::Invalid("atan2 INT_MIN axis"))?;
        let ay = y
            .checked_abs()
            .ok_or(Error::Invalid("atan2 INT_MIN axis"))?;
        if ax == 0 && ay == 0 {
            return Ok(0);
        }
        let (small, large, complement) = if ay < ax {
            (ay, ax, false)
        } else {
            (ax, ay, true)
        };
        let index = if small & 0x7fe00000 == 0 {
            small.wrapping_shl(10) / large
        } else {
            small / (large >> 10)
        } as usize;
        let mut angle = i32::from(
            *self
                .atan
                .get(index)
                .ok_or(Error::Invalid("atan2 table index"))?,
        );
        if complement {
            angle = 1024 - angle;
        }
        if x < 0 {
            angle = 2048 - angle;
        }
        if y < 0 {
            angle = -angle;
        }
        Ok(angle)
    }

    /// Translation of SquareRoot0 at 0x800A47C8 for nonnegative signed32 inputs.
    /// High-bit values correspond to negative inputs to the original routine.
    pub fn square_root(&self, value: u32) -> Result<i32> {
        if value > i32::MAX as u32 {
            return Err(Error::Invalid("SquareRoot0 negative signed32 input"));
        }
        if value == 0 {
            return Ok(0);
        }
        let leading = value.leading_zeros() & !1;
        let normalized = if leading >= 24 {
            value << (leading - 24)
        } else {
            value >> (24 - leading)
        };
        let index = (normalized - 64) as usize;
        let value = i32::from(self.sqrt[index]);
        let shift = (31 - leading) >> 1;
        Ok(((value.wrapping_shl(shift) as u32) >> 12) as i32)
    }

    /// rsin at 0x800A45C8. Sign handling occurs before modulo-4096 wrapping.
    pub fn sin(&self, angle: i32) -> i32 {
        let result = self.quarter_sine(angle.unsigned_abs() & 4095);
        if angle < 0 { -result } else { result }
    }

    /// rcos at 0x800A4698. All signed32 angles, including INT_MIN, are defined.
    pub fn cos(&self, angle: i32) -> i32 {
        self.quarter_sine((angle.unsigned_abs() + 1024) & 4095)
    }

    fn quarter_sine(&self, phase: u32) -> i32 {
        let (index, negative) = match phase {
            0..=1024 => (phase, false),
            1025..=2048 => (2048 - phase, false),
            2049..=3072 => (phase - 2048, true),
            _ => (4096 - phase, true),
        };
        let value = i32::from(self.sine[index as usize]);
        if negative { -value } else { value }
    }
}
