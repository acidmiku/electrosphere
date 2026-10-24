//! Original briefing DrawScene (ED834): camera, terrain, marker and object packets.
use crate::mission_map::{Matrix, compose};
use ac3_graphics::projection::{ProjectionControls, project, project_three};
use ac3_graphics::scalar::ScalarMath;
use ac3_graphics::transform::{SinCosTable, rotate_translate};
use ac3_graphics::{Error, Result};
use ac3_mission::{BriefingObjectDrawOutcome, FullBriefingState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BriefingCamera {
    pub matrix: Matrix,
    pub projection: ProjectionControls,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BriefingObjects {
    pub outcomes: Vec<BriefingObjectDrawOutcome>,
    /// FT4 payloads in original emission order; addPrim prepends to outer bucket0.
    pub packets: Vec<[u32; 9]>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BriefingDraw {
    pub camera: Option<BriefingCamera>,
    /// Emission order in outer bucket1; reverse at composition time.
    pub terrain_packets: Vec<Vec<u32>>,
    /// Emit through the shared original font renderer into outer bucket0 first.
    pub marker: Option<BriefingMarkerText>,
    /// Append after marker glyphs in outer bucket0, then reverse that bucket.
    pub objects: BriefingObjects,
}

pub struct BriefingScene {
    trig: SinCosTable,
    math: ScalarMath,
}

/// Original mission resource child0 and overlay palette/clip constants.
pub struct BriefingTerrain<'a> {
    heights: &'a [u8],
    colors: [u32; 16],
    full_clip: [i16; 4],
}
impl<'a> BriefingTerrain<'a> {
    pub fn new(heights: &'a [u8], overlay: &[u8]) -> Result<Self> {
        if heights.len() < 33153 {
            return Err(Error::Truncated("briefing height field"));
        }
        let table = overlay
            .get(0x2ca60..0x2caa0)
            .ok_or(Error::Truncated("briefing terrain palette"))?;
        let clip = overlay
            .get(0x1594..0x159c)
            .ok_or(Error::Truncated("briefing clip rectangle"))?;
        Ok(Self {
            heights,
            colors: std::array::from_fn(|i| {
                u32::from_le_bytes(table[i * 4..i * 4 + 4].try_into().unwrap())
            }),
            full_clip: std::array::from_fn(|i| {
                i16::from_le_bytes(clip[i * 2..i * 2 + 2].try_into().unwrap())
            }),
        })
    }
    fn height(&self, x: i16, z: i16) -> Result<i16> {
        let index = (i32::from(x) + (i32::from(z) + 128) * 258 + 128) / 2;
        let byte = *usize::try_from(index)
            .ok()
            .and_then(|i| self.heights.get(i))
            .ok_or(Error::Invalid(
                "briefing terrain coordinate outside resource",
            ))?;
        Ok(i16::from((byte >> ((x & 1) * 4)) & 15) * 128)
    }
    /// EBF68 emits 225 G4 quads bracketed by draw-area commands, into OT bucket1.
    pub fn draw(&self, state: &FullBriefingState, camera: BriefingCamera) -> Result<Vec<Vec<u32>>> {
        let mut packets = vec![draw_area(self.full_clip).to_vec()];
        let grid = |value: i32| {
            let quotient = value / 128;
            let remainder = value % 128;
            // Original subtracts one for every negative value, including exact multiples.
            if value < 0 {
                (quotient - 1, remainder + 128)
            } else {
                (quotient, remainder)
            }
        };
        let (base_x, fraction_x) = grid(state.script.camera[0]);
        let (base_z, fraction_z) = grid(state.script.camera[2]);
        for row in -7..=7 {
            let z = (base_z + row) as i16;
            let mut previous_right = [0i16; 3];
            let mut previous_height = 0;
            let mut previous_screen = [0i16; 2];
            for column in -7..=7 {
                let x = (base_x + column) as i16;
                let mut h = [
                    if column == -7 {
                        self.height(x, z)?
                    } else {
                        previous_height
                    },
                    self.height(x.wrapping_add(1), z)?,
                    self.height(x, z.wrapping_add(1))?,
                    self.height(x.wrapping_add(1), z.wrapping_add(1))?,
                ];
                let mut v = [
                    if column == -7 {
                        [x.wrapping_mul(128), -h[0], z.wrapping_mul(128)]
                    } else {
                        previous_right
                    },
                    [
                        x.wrapping_add(1).wrapping_mul(128),
                        -h[1],
                        z.wrapping_mul(128),
                    ],
                    [
                        x.wrapping_mul(128),
                        -h[2],
                        z.wrapping_add(1).wrapping_mul(128),
                    ],
                    [
                        x.wrapping_add(1).wrapping_mul(128),
                        -h[3],
                        z.wrapping_add(1).wrapping_mul(128),
                    ],
                ];
                if column == -7 {
                    v[0][0] = v[0][0].wrapping_add(fraction_x as i16);
                    v[2][0] = v[2][0].wrapping_add(fraction_x as i16);
                    h[0] = lerp_height(h[0], h[1], fraction_x);
                    h[2] = lerp_height(h[2], h[3], fraction_x);
                    v[0][1] = -h[0];
                    v[2][1] = -h[2];
                }
                if column == 7 {
                    v[1][0] = v[1][0].wrapping_add((fraction_x - 128) as i16);
                    v[3][0] = v[3][0].wrapping_add((fraction_x - 128) as i16);
                    h[1] = lerp_height(h[1], h[0], 128 - fraction_x);
                    h[3] = lerp_height(h[3], h[2], 128 - fraction_x);
                    v[1][1] = -h[1];
                    v[3][1] = -h[3];
                }
                if row == -7 {
                    v[0][2] = v[0][2].wrapping_add(fraction_z as i16);
                    v[1][2] = v[1][2].wrapping_add(fraction_z as i16);
                    h[0] = lerp_height(h[0], h[2], fraction_z);
                    h[1] = lerp_height(h[1], h[3], fraction_z);
                    v[0][1] = -h[0];
                    v[1][1] = -h[1];
                }
                if row == 7 {
                    v[2][2] = v[2][2].wrapping_add((fraction_z - 128) as i16);
                    v[3][2] = v[3][2].wrapping_add((fraction_z - 128) as i16);
                    h[2] = lerp_height(h[2], h[0], 128 - fraction_z);
                    h[3] = lerp_height(h[3], h[1], 128 - fraction_z);
                    v[2][1] = -h[2];
                    v[3][1] = -h[3];
                }
                let first = if column == -7 {
                    project(
                        camera.matrix.rotation,
                        camera.matrix.translation,
                        camera.projection,
                        v[0],
                    )
                    .screen[0]
                } else {
                    previous_screen
                };
                let projected = project_three(
                    camera.matrix.rotation,
                    camera.matrix.translation,
                    camera.projection,
                    [v[1], v[2], v[3]],
                );
                let mut colors = [0; 4];
                for i in 0..4 {
                    colors[i] = *self
                        .colors
                        .get(
                            usize::try_from(h[i] / 128)
                                .map_err(|_| Error::Invalid("negative terrain palette index"))?,
                        )
                        .ok_or(Error::Invalid("terrain palette index"))?;
                }
                if column == -7 {
                    colors[0] = lerp_color(colors[0], colors[1], fraction_x);
                    colors[2] = lerp_color(colors[2], colors[3], fraction_x);
                }
                if row == -7 {
                    colors[0] = lerp_color(colors[0], colors[2], fraction_z);
                    colors[1] = lerp_color(colors[1], colors[3], fraction_z);
                }
                if column == 7 {
                    colors[1] = lerp_color(colors[1], colors[0], fraction_x - 128);
                    colors[3] = lerp_color(colors[3], colors[2], fraction_x - 128);
                }
                if row == 7 {
                    colors[2] = lerp_color(colors[2], colors[0], fraction_z - 128);
                    colors[3] = lerp_color(colors[3], colors[1], fraction_z - 128);
                }
                packets.push(vec![
                    colors[0] | 0x38000000,
                    xy(first),
                    colors[1],
                    xy(projected.screen[0]),
                    colors[2],
                    xy(projected.screen[1]),
                    colors[3],
                    xy(projected.screen[2]),
                ]);
                previous_right = v[1];
                previous_height = h[1];
                previous_screen = projected.screen[0];
            }
        }
        packets.push(
            draw_area([
                84,
                state.view_offsets[1]
                    .wrapping_add(state.panel_offsets[1])
                    .wrapping_sub(66),
                150,
                320,
            ])
            .to_vec(),
        );
        Ok(packets)
    }
}
fn lerp_height(start: i16, end: i16, factor: i32) -> i16 {
    start.wrapping_add(((i32::from(end) - i32::from(start)) * factor / 128) as i16)
}
fn lerp_color(start: u32, end: u32, factor: i32) -> u32 {
    let mut result = start & 0xff000000;
    for shift in [0, 8, 16] {
        let a = ((start >> shift) & 255) as i32;
        let b = ((end >> shift) & 255) as i32;
        result |= ((a + (b - a) * factor / 128) as u32) << shift;
    }
    result
}
fn draw_area(rect: [i16; 4]) -> [u32; 2] {
    let coord = |x: i16, y: i16| {
        (i32::from(x).clamp(0, 1023) as u32) | ((i32::from(y).clamp(0, 511) as u32) << 10)
    };
    [
        0xe3000000 | coord(rect[0], rect[1]),
        0xe4000000
            | coord(
                rect[0].wrapping_add(rect[2]).wrapping_sub(1),
                rect[1].wrapping_add(rect[3]).wrapping_sub(1),
            ),
    ]
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BriefingMarkerText {
    pub text: String,
    pub font: u8,
    pub position: [i32; 2],
    pub rgb: [u8; 3],
}
impl BriefingScene {
    /// Complete ED834 geometry and logical draw outcomes. Font rasterization is
    /// delegated through the typed marker request to the shared menu font engine.
    pub fn draw(
        &self,
        state: &FullBriefingState,
        terrain: &BriefingTerrain<'_>,
    ) -> Result<BriefingDraw> {
        if state.script.flags & 1 == 0 {
            return Ok(BriefingDraw::default());
        }
        let camera = self.camera(state)?;
        Ok(BriefingDraw {
            camera: Some(camera),
            terrain_packets: terrain.draw(state, camera)?,
            marker: self.marker_text(state, camera),
            objects: self.draw_objects_with_camera(state, camera)?,
        })
    }
    /// ED63C's font11 request, emitted into bucket0 before object quads.
    pub fn marker_text(
        &self,
        state: &FullBriefingState,
        camera: BriefingCamera,
    ) -> Option<BriefingMarkerText> {
        let script = &state.script;
        if script.marker_id < 0
            || i32::from(script.marker[0])
                .wrapping_sub(script.camera[0])
                .wrapping_abs()
                >= 897
            || i32::from(script.marker[2])
                .wrapping_sub(script.camera[2])
                .wrapping_abs()
                >= 897
        {
            return None;
        }
        let position = project(
            camera.matrix.rotation,
            camera.matrix.translation,
            camera.projection,
            script.marker,
        )
        .screen[0];
        Some(BriefingMarkerText {
            text: format!("{:02}:{:02}", script.marker_id / 60, script.marker_id % 60),
            font: 11,
            position: [i32::from(position[0]) - 10, i32::from(position[1])],
            rgb: [128; 3],
        })
    }
    pub fn new(resident: &[u8]) -> Result<Self> {
        Ok(Self {
            trig: SinCosTable::from_le_bytes(
                resident
                    .get(0xa4f84..0xa8f84)
                    .ok_or(Error::Truncated("resident trig table"))?,
            )?,
            math: ScalarMath::from_resident(resident, 0x80010000)?,
        })
    }
    /// EBCD8: orbit camera, look-at angles, transpose and doubled vertical row.
    pub fn camera(&self, state: &FullBriefingState) -> Result<BriefingCamera> {
        let orbit = rotation_yxz(&self.trig, [state.view_angles[0], state.view_angles[1], 0]);
        let eye = rotate_translate(orbit, state.script.camera, [0, 0, state.zoom as i16]).vector;
        let delta: [i32; 3] = std::array::from_fn(|i| state.script.camera[i].wrapping_sub(eye[i]));
        let length = self.math.square_root(
            delta[0]
                .wrapping_mul(delta[0])
                .wrapping_add(delta[2].wrapping_mul(delta[2])) as u32,
        )?;
        let angles = [
            self.math.atan2(delta[1], length)?.wrapping_neg() as i16,
            self.math.atan2(delta[0], delta[2])? as i16,
            0,
        ];
        let look = rotation_yxz(&self.trig, angles);
        let mut rotation = std::array::from_fn(|r| std::array::from_fn(|c| look[c][r]));
        for value in &mut rotation[1] {
            *value = value.wrapping_mul(2);
        }
        let translation = apply_long(rotation, eye.map(i32::wrapping_neg));
        Ok(BriefingCamera {
            matrix: Matrix {
                rotation,
                translation,
            },
            projection: ProjectionControls {
                offset: [
                    (i32::from(state.view_offsets[0]) + 154).wrapping_shl(16),
                    (i32::from(state.panel_offsets[1]) + i32::from(state.view_offsets[1]) + 174)
                        .wrapping_shl(16),
                ],
                distance: 250,
                depth_scale: -11184,
                depth_offset: 0x1555000,
            },
        })
    }
    /// ED834 gating followed by EBCD8/ECE7C. Terrain and marker drawing do not
    /// mutate these logical fields; use `draw` for their packet generation too.
    pub fn draw_objects(&self, state: &FullBriefingState) -> Result<BriefingObjects> {
        if state.script.flags & 1 == 0 {
            return Ok(BriefingObjects::default());
        }
        self.draw_objects_with_camera(state, self.camera(state)?)
    }
    pub fn draw_objects_with_camera(
        &self,
        state: &FullBriefingState,
        camera: BriefingCamera,
    ) -> Result<BriefingObjects> {
        let mut output = BriefingObjects::default();
        for (index, object) in state.script.objects.iter().copied().enumerate() {
            if object.flags() & 1 == 0 {
                continue;
            }
            let half = |offset| {
                object
                    .halfword(offset)
                    .expect("fixed original object field")
            };
            let position = [
                i32::from(half(0xe)),
                i32::from(half(0x10)),
                i32::from(half(0x12)),
            ];
            let mut outcome = BriefingObjectDrawOutcome {
                index: index as u8,
                flags: object.flags(),
                appearance_remaining: half(8),
                screen: [-1, -1],
            };
            if position[0]
                .wrapping_sub(state.script.camera[0])
                .wrapping_abs()
                >= 897
                || position[2]
                    .wrapping_sub(state.script.camera[2])
                    .wrapping_abs()
                    >= 897
            {
                outcome.flags |= 2;
                output.outcomes.push(outcome);
                continue;
            }
            outcome.flags &= !2;
            let extent = if half(6) == 1 && half(8) > 0 {
                let duration = i32::from(half(10));
                if duration == 0 {
                    return Err(Error::Invalid("briefing appearance division by zero"));
                }
                outcome.appearance_remaining = half(8).wrapping_sub(1);
                (duration - i32::from(half(8))) * 128 / duration
            } else {
                128
            };
            // Original computes each signed numerator before division, then narrows.
            let negative = (-extent) as i16;
            let positive = extent as i16;
            let vertices = [
                [negative, 0, positive],
                [positive, 0, positive],
                [negative, 0, negative],
                [positive, 0, negative],
            ];
            let object_matrix = Matrix {
                rotation: self.trig.rotation([half(0x16), half(0x18), half(0x1a)]),
                translation: position,
            };
            let matrix = compose(camera.matrix, object_matrix);
            let first = project_three(
                matrix.rotation,
                matrix.translation,
                camera.projection,
                [vertices[0], vertices[1], vertices[2]],
            );
            let last = project(
                matrix.rotation,
                matrix.translation,
                camera.projection,
                vertices[3],
            );
            let screen = [
                first.screen[0],
                first.screen[1],
                first.screen[2],
                last.screen[0],
            ];
            outcome.screen = std::array::from_fn(|i| {
                ((i32::from(screen[0][i]) + i32::from(screen[3][i])) / 2) as i16
            });
            let u = (i32::from(half(4)) * 16) as u8;
            // Only GPU-used halves of UV2/UV3 are defined by the original stores.
            output.packets.push([
                0x2c808080,
                xy(screen[0]),
                u32::from(u) | (0xf5b << 16),
                xy(screen[1]),
                u32::from(u.wrapping_add(15)) | (9 << 16),
                xy(screen[2]),
                u32::from(u) | (15 << 8),
                xy(screen[3]),
                u32::from(u.wrapping_add(15)) | (15 << 8),
            ]);
            output.outcomes.push(outcome);
        }
        Ok(output)
    }
}

fn xy(p: [i16; 2]) -> u32 {
    u32::from(p[0] as u16) | (u32::from(p[1] as u16) << 16)
}

/// Resident A5BE8, including the signed-angle branches sharing its tail.
fn rotation_yxz(trig: &SinCosTable, angles: [i16; 3]) -> [[i16; 3]; 3] {
    let [sx, cx] = trig.sin_cos(angles[0]);
    let [sy, cy] = trig.sin_cos(angles[1]);
    let [sz, cz] = trig.sin_cos(angles[2]);
    let mul = |a: i32, b: i32| a.wrapping_mul(b) >> 12;
    let sysx = mul(sy, sx);
    let cysx = mul(cy, sx);
    [
        [
            mul(cy, cz) + mul(sysx, sz),
            mul(sysx, cz) - mul(cy, sz),
            mul(sy, cx),
        ],
        [mul(sz, cx), mul(cz, cx), -sx],
        [
            mul(cysx, sz) - mul(sy, cz),
            mul(sy, sz) + mul(cysx, cz),
            mul(cy, cx),
        ],
    ]
    .map(|r| r.map(|v| v as i16))
}
fn apply_long(rotation: [[i16; 3]; 3], vector: [i32; 3]) -> [i32; 3] {
    let high = vector.map(|v| (v / 32768) as i16);
    let low = vector.map(|v| (v % 32768) as i16);
    let lower = rotate_translate(rotation, [0; 3], low).vector;
    std::array::from_fn(|row| {
        let upper: i64 = (0..3)
            .map(|i| i64::from(rotation[row][i]) * i64::from(high[i]))
            .sum();
        (upper as i32).wrapping_mul(8).wrapping_add(lower[row])
    })
}

/// Asset-backed UI leaves around DrawScene. No relocated pointers are retained.
pub struct BriefingUi<'a> {
    overlay: &'a [u8],
    mission: ac3_formats::dot1::Container<'a>,
    common: ac3_formats::dot1::Container<'a>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BriefingText {
    pub text: Vec<u8>,
    pub position: [i16; 2],
}
impl BriefingText {
    /// Briefing UI uses font16 and neutral modulation (800D04C8).
    pub fn packets(
        &self,
        fonts: &crate::menu_scene::LanguageScene<'_>,
    ) -> crate::Result<Vec<Vec<u32>>> {
        let mut packets = Vec::new();
        fonts.font_text(
            16,
            &self.text,
            self.position[0],
            self.position[1],
            &mut packets,
        )?;
        Ok(packets)
    }
}
impl<'a> BriefingUi<'a> {
    pub fn new(data: &'a crate::assets::GameData, mission_resource: usize) -> crate::Result<Self> {
        Ok(Self {
            overlay: data.overlay(2)?,
            mission: ac3_formats::dot1::parse(data.resource(mission_resource)?)?,
            common: ac3_formats::dot1::parse(data.resource(72)?)?,
        })
    }
    fn overlay_bytes(&self, address: u32, length: usize) -> crate::Result<&'a [u8]> {
        address
            .checked_sub(crate::assets::OVERLAY_BASE)
            .and_then(|at| self.overlay.get(at as usize..at as usize + length))
            .ok_or_else(|| "briefing overlay pointer outside asset".into())
    }
    fn full_clip(&self) -> crate::Result<[i16; 4]> {
        let b = self.overlay_bytes(0x800cb354, 8)?;
        Ok(std::array::from_fn(|i| {
            i16::from_le_bytes(b[i * 2..i * 2 + 2].try_into().unwrap())
        }))
    }
    /// EB754 emits two flat lines into outer bucket0 before object projection.
    /// It deliberately reads the object's existing screen coordinates.
    pub fn connector(
        &self,
        state: &FullBriefingState,
        index: usize,
        visual_offset: [i16; 2],
    ) -> crate::Result<Vec<Vec<u32>>> {
        let label = state
            .script
            .labels
            .get(index)
            .ok_or("briefing label index outside state")?;
        if label.flags & 0x64 != 0
            || (label.flags & 8 == 0 && (label.flags & 0x10 != 0 || label.flags & 2 == 0))
        {
            return Ok(Vec::new());
        }
        let object = state
            .script
            .objects
            .get(usize::from(label.object))
            .ok_or("briefing label object outside state")?;
        if object.flags() & 2 != 0 {
            return Ok(Vec::new());
        }
        if label.style > 4 {
            return Err("briefing connector style outside palette".into());
        }
        let color = u32::from_le_bytes(
            self.overlay_bytes(0x800cb340 + u32::from(label.style) * 4, 4)?
                .try_into()?,
        );
        let start = [
            label.x.wrapping_add(visual_offset[0]).wrapping_add(78),
            label.y.wrapping_add(visual_offset[1]).wrapping_add(90),
        ];
        let end = [
            object.halfword(0x1e).unwrap(),
            object.halfword(0x20).unwrap(),
        ];
        Ok(vec![
            vec![0x40000000 | color, xy(start), xy(end)],
            vec![
                0x40000000 | color,
                xy([start[0], start[1].wrapping_add(1)]),
                xy([end[0], end[1].wrapping_add(1)]),
            ],
        ])
    }
    pub fn connectors(
        &self,
        state: &FullBriefingState,
        visual_offset: [i16; 2],
    ) -> crate::Result<Vec<Vec<u32>>> {
        let mut packets = Vec::new();
        for i in 0..8 {
            packets.extend(self.connector(state, i, visual_offset)?);
        }
        Ok(packets)
    }
    /// ED8B0 emits into menu bucket5 even when the 3D scene's enable bit is clear.
    pub fn minimap(&self, state: &FullBriefingState) -> crate::Result<Vec<Vec<u32>>> {
        let mut packets = vec![draw_area(self.full_clip()?).to_vec()];
        let cx = (state.script.camera[0] / 128).wrapping_add(128) as i16;
        let cz = (state.script.camera[2].wrapping_neg() / 128) as i16;
        let panel = state.panel_offsets[0];
        for object in state.script.objects {
            let kind = object.halfword(4).unwrap();
            if object.flags() & 1 == 0 || kind == 9 || kind >= 11 {
                continue;
            }
            let rgb = match kind {
                0 | 1 | 10 => 0xc86038,
                2 | 3 => 0xffffff,
                4 | 5 => 0x68d0e8,
                6 | 7 => 0x6850f0,
                8 => 0xd0d090,
                _ => return Err("briefing minimap object color undefined for negative type".into()),
            };
            let x = i32::from(object.halfword(0xe).unwrap()) / 128;
            let z = -i32::from(object.halfword(0x12).unwrap()) / 128;
            let position = [
                ((x - (i32::from(cx) - 128)) / 2).wrapping_add(275) as i16,
                (z - i32::from(cz) + i32::from(panel) + 164) as i16,
            ];
            packets.push(vec![0x60000000 | rgb, xy(position), 0x00060003]);
        }
        let left = 275i16.wrapping_sub(cx / 2);
        let right = 403i16.wrapping_sub(cx / 2);
        let top = panel.wrapping_add(164).wrapping_sub(cz.wrapping_add(128));
        let bottom = top.wrapping_add(256);
        packets.push(vec![
            0x2c808080,
            xy([left, top]),
            0x57140000,
            xy([right, top]),
            0x00050080,
            xy([left, bottom]),
            0x00008000,
            xy([right, bottom]),
            0x00008080,
        ]);
        packets.push(draw_area([243, panel.wrapping_add(100), 64, 128]).to_vec());
        Ok(packets)
    }
    pub fn text_line_count(&self, language: usize) -> crate::Result<usize> {
        let languages = ac3_formats::dot1::parse(self.mission.child(2)?)?;
        let sections = ac3_formats::dot1::parse(languages.child(language)?)?;
        Ok(ac3_formats::dot1::parse(sections.child(2)?)?.len())
    }
    /// D0B74 resolves mission child2 -> language -> section2 -> line.
    pub fn text_line(
        &self,
        language: usize,
        index: i32,
        position: [i32; 2],
    ) -> crate::Result<BriefingText> {
        let languages = ac3_formats::dot1::parse(self.mission.child(2)?)?;
        let sections = ac3_formats::dot1::parse(languages.child(language)?)?;
        let lines = ac3_formats::dot1::parse(sections.child(2)?)?;
        Ok(BriefingText {
            text: c_string(lines.child(usize::try_from(index)?)?)?,
            position: position.map(|x| x as i16),
        })
    }
    /// EEA20's four texts, evaluated before navigation changes label/camera state.
    pub fn map_text(
        &self,
        language: usize,
        mission_id: i16,
        selected_label: i32,
        grid: [i16; 2],
    ) -> crate::Result<[BriefingText; 4]> {
        let languages = ac3_formats::dot1::parse(self.common.child(7)?)?;
        let strings = ac3_formats::dot1::parse(languages.child(language)?)?;
        let labels = ac3_formats::dot1::parse(self.mission.child(3)?)?;
        let at = 0x800f5960u32
            .checked_add(
                u32::try_from(mission_id)?
                    .checked_mul(12)
                    .ok_or("mission table index overflow")?,
            )
            .ok_or("mission table address overflow")?;
        let pointer = u32::from_le_bytes(self.overlay_bytes(at, 4)?.try_into()?);
        let offset = pointer
            .checked_sub(crate::assets::OVERLAY_BASE)
            .ok_or("mission name below overlay")? as usize;
        let name = c_string(
            self.overlay
                .get(offset..)
                .ok_or("mission name outside overlay")?,
        )?;
        let mut title = b": ".to_vec();
        title.extend(name);
        let mut coordinate = format!("({:4},{:4})", grid[0], grid[1]).into_bytes();
        for b in &mut coordinate {
            if *b == b' ' {
                *b = 7;
            }
        }
        coordinate.push(0);
        Ok([
            BriefingText {
                text: c_string(strings.child(9)?)?,
                position: [25, 320],
            },
            BriefingText {
                text: title,
                position: [115, 320],
            },
            BriefingText {
                text: c_string(labels.child(usize::try_from(selected_label)?)?)?,
                position: [25, 340],
            },
            BriefingText {
                text: coordinate,
                position: [25, 360],
            },
        ])
    }
}
fn c_string(bytes: &[u8]) -> crate::Result<Vec<u8>> {
    let end = bytes
        .iter()
        .position(|&b| b == 0)
        .ok_or("unterminated briefing string")?;
    Ok(bytes[..=end].to_vec())
}

/// The callback's drawing leaves and shared UI in their original OT buckets.
/// The host still owns the background map (outer15), display environment/audio,
/// and consumption of the returned object outcomes by `finish_briefing_render`.
pub struct BriefingRenderer<'a> {
    pub geometry: BriefingScene,
    pub ui: BriefingUi<'a>,
    terrain: BriefingTerrain<'a>,
}
pub struct BriefingCommands {
    /// Insertion order; executed descending bins with each bin reversed.
    pub menu_bins: [Vec<Vec<u32>>; 16],
    pub outer_one: Vec<Vec<u32>>,
    pub outer_zero: Vec<Vec<u32>>,
    pub outcomes: Vec<BriefingObjectDrawOutcome>,
    pub remaining_effects: Vec<ac3_mission::BriefingEffect>,
}
impl BriefingCommands {
    pub fn execution_order(&self) -> Vec<Vec<u32>> {
        self.menu_bins
            .iter()
            .rev()
            .flat_map(|b| b.iter().rev())
            .chain(self.outer_one.iter().rev())
            .chain(self.outer_zero.iter().rev())
            .cloned()
            .collect()
    }
}
impl<'a> BriefingRenderer<'a> {
    pub fn new(data: &'a crate::assets::GameData, mission_resource: usize) -> crate::Result<Self> {
        let ui = BriefingUi::new(data, mission_resource)?;
        let terrain = BriefingTerrain::new(ui.mission.child(0)?, data.overlay(2)?)?;
        Ok(Self {
            geometry: BriefingScene::new(&data.executable.payload)?,
            ui,
            terrain,
        })
    }
    /// Process effects in callback order, then execute the shared visual page.
    /// `update.state` is the controller's state before `finish_briefing_render`.
    /// Text effects carry their earlier phase values, avoiding post-input reads.
    pub fn commands(
        &self,
        scene: &crate::menu_scene::LanguageScene<'_>,
        visual: &mut crate::menu_scene::MenuVisualState,
        update: &ac3_mission::BriefingUpdate,
        language: usize,
        mission_id: i16,
    ) -> crate::Result<BriefingCommands> {
        let mut output = self.callback_commands(scene, visual, update, language, mission_id)?;
        let mut shared = scene.mission_commands(visual, language)?;
        for (callback, script) in output.menu_bins.iter_mut().zip(&mut shared.bins) {
            callback.append(script);
        }
        output.outer_zero.extend(shared.fonts);
        Ok(output)
    }
    /// Callback-only boundary, before the outer loop renders the shared UI page.
    /// The host must execute that page separately or use `commands` for composition.
    pub fn callback_commands(
        &self,
        scene: &crate::menu_scene::LanguageScene<'_>,
        visual: &mut crate::menu_scene::MenuVisualState,
        update: &ac3_mission::BriefingUpdate,
        language: usize,
        mission_id: i16,
    ) -> crate::Result<BriefingCommands> {
        use ac3_mission::{BriefingEffect as B, Effect as E};
        let mut outer_zero = Vec::new();
        let mut outer_one = Vec::new();
        let mut minimap = Vec::new();
        let mut outcomes = Vec::new();
        let mut remaining_effects = Vec::new();
        for effect in &update.effects {
            match effect {
                B::Common(
                    e @ (E::SelectVisualPage { .. }
                    | E::ClearTextVisibility
                    | E::SetTextVisibility { .. }
                    | E::SetVisualStyle { .. }
                    | E::SetVisualOffset { .. }
                    | E::SetVisualParameter { .. }),
                ) => visual.apply_mission_visuals(std::slice::from_ref(e))?,
                B::SetVisualColor { slot, rgb } => {
                    visual.set_color(usize::try_from(*slot)?, *rgb)?
                }
                B::DrawTextLine { index, position } => outer_zero.extend(
                    self.ui
                        .text_line(language, *index, *position)?
                        .packets(scene)?,
                ),
                B::DrawMapText {
                    selected_label,
                    grid,
                } => {
                    for text in self
                        .ui
                        .map_text(language, mission_id, *selected_label, *grid)?
                    {
                        outer_zero.extend(text.packets(scene)?);
                    }
                }
                B::DrawLabelConnectors { visual_offset } => {
                    outer_zero.extend(self.ui.connectors(&update.state, *visual_offset)?)
                }
                B::DrawScene => {
                    let draw = self.geometry.draw(&update.state, &self.terrain)?;
                    outer_one.extend(draw.terrain_packets);
                    if let Some(marker) = draw.marker {
                        let mut text = marker.text.into_bytes();
                        text.push(0);
                        scene.font_text(
                            u16::from(marker.font),
                            &text,
                            marker.position[0] as i16,
                            marker.position[1] as i16,
                            &mut outer_zero,
                        )?;
                    }
                    outer_zero.extend(draw.objects.packets.into_iter().map(Vec::from));
                    outcomes.extend(draw.objects.outcomes);
                }
                B::DrawMinimap => minimap.extend(self.ui.minimap(&update.state)?),
                _ => remaining_effects.push(effect.clone()),
            }
        }
        let mut menu_bins = std::array::from_fn(|_| Vec::new());
        menu_bins[5] = minimap;
        Ok(BriefingCommands {
            menu_bins,
            outer_one,
            outer_zero,
            outcomes,
            remaining_effects,
        })
    }
}
