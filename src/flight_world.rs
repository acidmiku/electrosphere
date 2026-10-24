//! Bounded resident first-flight world preparation and packet generation.
//!
//! Terrain visibility is supplied explicitly: 8005FE48's camera/frustum work and
//! 8005FF9C's terrain geometry renderer are not implemented here. Sky bands are
//! projected inputs to 80093920/80093AF4, not a replacement sky projection.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// Mission section 1, consumed by 8005CDA4. Values are signed on the original CPU.
pub struct TerrainGrid<'a> {
    bytes: &'a [u8],
}
impl<'a> TerrainGrid<'a> {
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() != 256 * 256 * 2 {
            return Err(Error("terrain grid must contain 256 by 256 halfwords"));
        }
        Ok(Self { bytes })
    }

    /// Outside the map, repeat the first/last eight-cell border. The original
    /// coordinate comparisons narrow to signed 16 bits before testing bounds.
    pub fn cell(&self, x: i32, z: i32) -> i16 {
        fn border(value: i32) -> usize {
            let value = value as i16;
            if value < 0 {
                (value & 7) as usize
            } else if value > 255 {
                ((value & 7) | 0xf8) as usize
            } else {
                value as usize
            }
        }
        let offset = (border(z) * 256 + border(x)) * 2;
        i16::from_le_bytes(self.bytes[offset..offset + 2].try_into().unwrap())
    }

    /// 8005CDA4 emits 12-byte work records and one sentinel. Padding/unused
    /// sentinel coordinates are intentionally absent from the native result.
    pub fn select(&self, view: &TerrainView) -> Result<Vec<TerrainCell>> {
        if view.width > 32 || view.row_masks.len() > 46 {
            return Err(Error("terrain view exceeds original descriptor capacity"));
        }
        let mut result = Vec::new();
        let mut previous_row = 0_u32;
        for (row, &mask) in view.row_masks.iter().enumerate() {
            let z = view.scan_origin[1].wrapping_add(row as i32);
            let mut occupied = 0;
            let mut first = true;
            for column in 0..view.width {
                let bit = 1_u32 << column;
                if mask & bit == 0 {
                    continue;
                }
                let x = view.scan_origin[0].wrapping_add(i32::from(column));
                let cell = self.cell(
                    view.tile_origin[0].wrapping_add(x),
                    view.tile_origin[1].wrapping_add(z),
                );
                if cell & 0x1ff == 0 {
                    continue;
                }
                occupied |= bit;
                let stitch = if first {
                    first = false;
                    0x0040_0000
                } else if previous_row.wrapping_shr(u32::from(column).wrapping_sub(1)) & 3 == 3 {
                    0x0e00_0000
                } else {
                    0x0800_0000
                };
                result.push(TerrainCell {
                    position: [
                        (x as i16)
                            .wrapping_mul(0x800)
                            .wrapping_sub((view.fraction[0] as i16).wrapping_sub(0x400)),
                        (view.height as i16).wrapping_neg(),
                        (z as i16)
                            .wrapping_mul(0x800)
                            .wrapping_sub((view.fraction[1] as i16).wrapping_sub(0x400)),
                    ],
                    descriptor: cell as i32 as u32 | (u32::from(column) << 16) | stitch,
                });
            }
            previous_row = occupied;
        }
        Ok(result)
    }
}

/// Native counterpart of the visibility descriptor produced by 8005FE48.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerrainView {
    pub height: i32,
    pub tile_origin: [i32; 2],
    pub fraction: [i32; 2],
    pub scan_origin: [i32; 2],
    pub width: u8,
    pub row_masks: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerrainCell {
    pub position: [i16; 3],
    /// Signed source cell, source column, first-cell and previous-row stitch bits.
    pub descriptor: u32,
}
pub const TERRAIN_LIST_END: u32 = 0x0020_0000;

/// One forward linked list, prepended as a unit to its depth bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldPacketChain {
    pub bucket: u16,
    pub packets: Vec<Vec<u32>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FogMaterial {
    pub rgb: [u8; 3],
}
impl FogMaterial {
    pub fn from_mission_parameters(bytes: &[u8]) -> Result<Self> {
        let rgb = bytes
            .get(8..11)
            .ok_or(Error("truncated mission fog color"))?;
        Ok(Self {
            rgb: rgb.try_into().unwrap(),
        })
    }

    /// 80092BCC: sixteen alternating framebuffer-copy/blend strips and a far
    /// fog-color tile. Draw environment changes are part of each chain.
    /// `depth_buckets` comes from the native 8005D390 helper, not guessed depth.
    pub fn packets(
        &self,
        depth_buckets: [i16; 17],
        draw_rect: [i16; 4],
        draw_offset: [i16; 2],
        mask_flags: u32,
    ) -> Result<Vec<WorldPacketChain>> {
        if depth_buckets.iter().any(|&v| !(0..4096).contains(&v)) {
            return Err(Error("fog depth bucket outside original 4096-entry OT"));
        }
        let mask = 0xe600_0000 | u32::from(mask_flags & 4 != 0);
        let [x, y, width, height] = draw_rect.map(i32::from);
        let restore = vec![
            0xe300_0000 | (x as u32 & 0x3ff) | ((y as u32 & 0x3ff) << 10),
            0xe400_0000
                | (x.wrapping_add(width).wrapping_sub(1) as u32 & 0x3ff)
                | ((y.wrapping_add(height).wrapping_sub(1) as u32 & 0x3ff) << 10),
            0xe500_0000 | (draw_offset[0] as u32 & 0x7ff) | ((draw_offset[1] as u32 & 0x7ff) << 11),
            0,
        ];
        let mut chains = Vec::with_capacity(17);
        for (index, bucket) in depth_buckets.into_iter().enumerate() {
            let u = ((index as u32) & 6) * 8;
            let v = ((index as u32) & 8) * 8;
            let operation = if index == 16 {
                vec![
                    0xe600_0001,
                    0x6000_0000 | rgb_word(self.rgb),
                    0,
                    0x0040_0010,
                    mask,
                ]
            } else if index & 1 == 0 {
                vec![
                    0xe600_0000,
                    0x0100_0000,
                    0x8000_0000,
                    (u + 0x140) | ((v + 0x180) << 16),
                    0x0100_0140,
                    0x0040_0010,
                    mask,
                ]
            } else {
                vec![
                    0xe600_0000,
                    0xe100_0315,
                    0x6683_8383,
                    0,
                    u | ((v + 0x80) << 8),
                    0x0040_0010,
                    0xe100_0335,
                    mask,
                ]
            };
            chains.push(WorldPacketChain {
                bucket: bucket as u16,
                packets: vec![
                    vec![0xe304_0140, 0xe404_fd4f, 0xe508_0140, 0],
                    operation,
                    restore.clone(),
                ],
            });
        }
        Ok(chains)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenPoint(pub [i16; 2]);
impl ScreenPoint {
    fn packed(self) -> u32 {
        u32::from(self.0[0] as u16) | (u32::from(self.0[1] as u16) << 16)
    }
}

/// Clipped horizon edges and viewport-corner classifications from 8009309C.
/// Each corner consumes two classification bits; duplicated high bits are used
/// when testing the following corner. These inputs do not hide camera work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkyBand {
    pub upper_codes: u32,
    pub lower_codes: u32,
    pub upper_edge: [ScreenPoint; 2],
    pub lower_edge: [ScreenPoint; 2],
    pub corners: [ScreenPoint; 4],
}

impl SkyBand {
    fn vertices(&self) -> Vec<(ScreenPoint, usize)> {
        let mut vertices = Vec::new();
        let (mut upper, mut lower) = (self.upper_codes, self.lower_codes);
        for (corner, point) in self.corners.into_iter().enumerate() {
            if (upper | lower) & 3 == 3 {
                vertices.push((point, corner));
            }
            if upper & 15 == 9 {
                vertices.push((self.upper_edge[1], 4));
            }
            if lower & 15 == 9 {
                vertices.push((self.lower_edge[1], 5));
            }
            if lower & 15 == 6 {
                vertices.push((self.lower_edge[0], 5));
            }
            if upper & 15 == 6 {
                vertices.push((self.upper_edge[0], 4));
            }
            upper = (upper as i32 >> 2) as u32;
            lower = (lower as i32 >> 2) as u32;
        }
        vertices
    }

    /// 80093920 emits flat quads then an optional triangle, in forward order.
    pub fn flat_packets(&self, rgb: [u8; 3]) -> Vec<Vec<u32>> {
        let vertices = self.vertices();
        polygon_indices(vertices.len())
            .into_iter()
            .map(|indices| {
                let opcode = if indices.len() == 4 {
                    0x2800_0000
                } else {
                    0x2000_0000
                };
                let mut packet = vec![opcode | rgb_word(rgb)];
                packet.extend(indices.into_iter().map(|i| vertices[i].0.packed()));
                packet
            })
            .collect()
    }

    /// 80093AF4's DPCS colors and Gouraud fan. The unclipped triangle and signed
    /// corner areas are original projection/clip outputs, supplied explicitly.
    pub fn gradient_packets(
        &self,
        upper_rgb: [u8; 3],
        lower_rgb: [u8; 3],
        unclipped_triangle: [ScreenPoint; 3],
        corner_areas: [i32; 4],
    ) -> Result<Vec<Vec<u32>>> {
        let divisor = nclip(unclipped_triangle) >> 4;
        let vertices = self.vertices();
        let colors = vertices
            .iter()
            .map(|&(_, kind)| {
                if kind == 5 {
                    return Ok(rgb_word(lower_rgb));
                }
                if kind == 4 || divisor == 0 || corner_areas[kind] <= 0 {
                    return Ok(rgb_word(upper_rgb));
                }
                let numerator = corner_areas[kind].wrapping_shl(8);
                let fraction = numerator
                    .checked_div(divisor)
                    .ok_or(Error("sky interpolation division trap"))?;
                Ok(rgb_word(depth_color(upper_rgb, lower_rgb, fraction as i16)))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(polygon_indices(vertices.len())
            .into_iter()
            .map(|indices| {
                let opcode = if indices.len() == 4 {
                    0x3800_0000
                } else {
                    0x3000_0000
                };
                let mut packet = Vec::with_capacity(indices.len() * 2);
                for (n, i) in indices.into_iter().enumerate() {
                    packet.push(colors[i] | if n == 0 { opcode } else { 0 });
                    packet.push(vertices[i].0.packed());
                }
                packet
            })
            .collect())
    }
}

fn polygon_indices(mut count: usize) -> Vec<Vec<usize>> {
    let mut result = Vec::new();
    while count > 3 {
        result.push(vec![0, count - 3, count - 1, count - 2]);
        count -= 2;
    }
    if count == 3 {
        result.push(vec![0, 1, 2]);
    }
    result
}
fn rgb_word(rgb: [u8; 3]) -> u32 {
    u32::from_le_bytes([rgb[0], rgb[1], rgb[2], 0])
}
fn nclip(points: [ScreenPoint; 3]) -> i32 {
    let [[x0, y0], [x1, y1], [x2, y2]] = points.map(|p| p.0.map(i64::from));
    (x0 * y1 + x1 * y2 + x2 * y0 - x0 * y2 - x1 * y0 - x2 * y1) as i32
}
// DPCS with RGB8 material endpoints. Difference is -4080..4080, so its
// intermediate signed IR clamp cannot engage. IR0 is a signed halfword.
fn depth_color(source: [u8; 3], far: [u8; 3], ir0: i16) -> [u8; 3] {
    std::array::from_fn(|i| {
        let source = i64::from(source[i]) << 16;
        let difference = (i64::from(far[i]) << 4) - (source >> 12);
        ((source + i64::from(ir0) * difference) >> 16).clamp(0, 255) as u8
    })
}
